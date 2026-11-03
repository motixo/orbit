// SPDX-License-Identifier: GPL-3.0-only

use super::Message;
use crate::fl;
use cosmic::iced::futures::{SinkExt, channel::mpsc::Sender};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

const SOURCE: &str = "0f2ca4087e6d938d823e76c856fcd04d0a0162b6";

#[derive(Clone, Copy, Debug)]
pub enum Step {
    Downloading(f32),
    Dependencies,
    Building,
    Installing,
    Verifying,
}

impl Step {
    pub fn label(self) -> String {
        match self {
            Self::Downloading(_) => fl!("sharing-setup-download"),
            Self::Dependencies => fl!("sharing-setup-dependencies"),
            Self::Building => fl!("sharing-setup-building"),
            Self::Installing => fl!("sharing-setup-installing"),
            Self::Verifying => fl!("sharing-setup-verifying"),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Distribution {
    Arch,
    Debian,
    Fedora,
}

fn distribution() -> Option<Distribution> {
    let release = std::fs::read_to_string("/etc/os-release").ok()?;
    for key in ["ID=", "ID_LIKE="] {
        if let Some(value) = release.lines().find_map(|line| line.strip_prefix(key)) {
            for id in value.trim_matches(['\'', '"']).split_whitespace() {
                match id {
                    "arch" => return Some(Distribution::Arch),
                    "debian" | "ubuntu" | "pop" => return Some(Distribution::Debian),
                    "fedora" => return Some(Distribution::Fedora),
                    _ => (),
                }
            }
        }
    }
    None
}

pub fn is_arch() -> bool {
    distribution() == Some(Distribution::Arch)
}

async fn command(command: &mut Command) -> Result<(), String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| {
            eprintln!("Owly installer: {error}");
            fl!("sharing-setup-install-error")
        })?;
    if output.status.success() {
        Ok(())
    } else {
        eprintln!(
            "Owly installer: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Err(fl!("sharing-setup-install-error"))
    }
}

async fn progress(output: &mut Sender<Message>, step: Step) {
    let _ = output.send(Message::Progress(step)).await;
}

async fn download(
    url: &str,
    checksum: &str,
    destination: &Path,
    output: &mut Sender<Message>,
) -> Result<(), String> {
    let result = async {
        let client = reqwest::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(300))
            .user_agent(concat!("Orbit/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let mut response = client.get(url).send().await?.error_for_status()?;
        let total = response.content_length().filter(|total| *total > 0);
        let mut file = tokio::fs::File::create(destination).await?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut previous = 0;
        while let Some(chunk) = response.chunk().await? {
            size = size.saturating_add(chunk.len() as u64);
            if size > 100 * 1024 * 1024 {
                return Err("Owly download exceeds 100 MiB".into());
            }
            hash.update(&chunk);
            file.write_all(&chunk).await?;
            if let Some(total) = total {
                let percent = (size.saturating_mul(100) / total).min(100) as u8;
                if percent != previous {
                    progress(output, Step::Downloading(f32::from(percent))).await;
                    previous = percent;
                }
            }
        }
        file.flush().await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
            hash.finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        )
    }
    .await;
    match result {
        Ok(digest) if digest == checksum => Ok(()),
        Ok(_) => Err(fl!("sharing-setup-checksum-error")),
        Err(error) => {
            eprintln!("Owly download: {error}");
            Err(fl!("sharing-setup-download-error"))
        }
    }
}

pub async fn run(output: &mut Sender<Message>) -> Result<(), String> {
    let distribution = distribution().ok_or_else(|| fl!("sharing-setup-platform-error"))?;
    let architecture = std::env::consts::ARCH;
    if !matches!(architecture, "x86_64" | "aarch64")
        || (distribution != Distribution::Arch && architecture != "x86_64")
    {
        return Err(fl!("sharing-setup-architecture-error"));
    }
    let directory = tempfile::Builder::new()
        .prefix("orbit-owly-")
        .tempdir()
        .map_err(|_| fl!("sharing-setup-install-error"))?;
    let (url, checksum, filename) = match distribution {
        Distribution::Arch => (
            format!("https://codeload.github.com/t4t5/owly/tar.gz/{SOURCE}"),
            "2df3b08f9e7c1b8e29f4511065f599b8c887c28dc0a272a059f9f995ea2ca487",
            "owly.tar.gz",
        ),
        Distribution::Debian => (
            "https://github.com/t4t5/owly/releases/download/v0.4.2/owly_0.4.2-1_amd64.deb".into(),
            "9e891f9cf23a7697d849115fbe93eec64743651c58ca7eb79957f1d4e00e9a08",
            "owly.deb",
        ),
        Distribution::Fedora => (
            "https://github.com/t4t5/owly/releases/download/v0.4.2/owly-0.4.2-1.x86_64.rpm".into(),
            "82b155cf0defda58ac85f683fe9f2888926b92dd38757c01f6083bd4f379e768",
            "owly.rpm",
        ),
    };
    let package = directory.path().join(filename);
    download(&url, checksum, &package, output).await?;
    match distribution {
        Distribution::Arch => {
            let source = package.clone();
            let destination = directory.path().to_owned();
            tokio::task::spawn_blocking(move || {
                tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(source)?))
                    .unpack(destination)
            })
            .await
            .map_err(|_| fl!("sharing-setup-install-error"))?
            .map_err(|_| fl!("sharing-setup-install-error"))?;
            progress(output, Step::Dependencies).await;
            let mut dependencies = Command::new("pkexec");
            dependencies.args([
                "/usr/bin/pacman",
                "-S",
                "--needed",
                "--noconfirm",
                "base-devel",
                "hostapd",
                "dbus",
                "systemd",
                "polkit",
                "networkmanager",
                "iproute2",
                "iputils",
                "util-linux",
            ]);
            if !Command::new("cargo")
                .arg("--version")
                .output()
                .await
                .is_ok_and(|result| result.status.success())
            {
                dependencies.arg("rust");
            }
            command(&mut dependencies).await?;
            progress(output, Step::Building).await;
            let packages = directory.path().join("packages");
            std::fs::create_dir(&packages).map_err(|_| fl!("sharing-setup-install-error"))?;
            let build = directory
                .path()
                .join(format!("owly-{SOURCE}/packaging/arch/owly"));
            command(
                Command::new("makepkg")
                    .args(["--nodeps", "--nocheck", "--noconfirm"])
                    .env("PKGDEST", &packages)
                    .current_dir(build),
            )
            .await?;
            let package = std::fs::read_dir(packages)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("owly-")
                                && name.contains(".pkg.tar.")
                                && !name.ends_with(".sig")
                        })
                })
                .ok_or_else(|| fl!("sharing-setup-install-error"))?;
            progress(output, Step::Installing).await;
            command(
                Command::new("pkexec")
                    .args(["/usr/bin/pacman", "-U", "--needed", "--noconfirm"])
                    .arg(package),
            )
            .await?;
        }
        Distribution::Debian => {
            progress(output, Step::Installing).await;
            command(
                Command::new("pkexec")
                    .args(["/usr/bin/apt-get", "install", "--yes"])
                    .arg(package),
            )
            .await?;
        }
        Distribution::Fedora => {
            progress(output, Step::Installing).await;
            command(
                Command::new("pkexec")
                    .args(["/usr/bin/dnf", "install", "--assumeyes"])
                    .arg(package)
                    .arg("hostapd"),
            )
            .await?;
        }
    }
    progress(output, Step::Verifying).await;
    if !super::installed().await {
        return Err(fl!("sharing-setup-verify-error"));
    }
    Ok(())
}
