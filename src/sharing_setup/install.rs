// SPDX-License-Identifier: GPL-3.0-only

use super::Message;
use crate::fl;
use cosmic::iced::futures::{SinkExt, channel::mpsc::Sender};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};

const VERSION: &str = "0.4.2";
const SOURCE: &str = "0f2ca4087e6d938d823e76c856fcd04d0a0162b6";

#[derive(Clone, Copy, Debug)]
pub enum Step {
    CrocInstalling,
    CrocVerifying,
    Downloading(f32),
    Toolchain,
    Dependencies,
    Building,
    Installing,
    Verifying,
}

impl Step {
    pub fn label(self) -> String {
        match self {
            Self::CrocInstalling => fl!("sharing-setup-croc-installing"),
            Self::CrocVerifying => fl!("sharing-setup-croc-verifying"),
            Self::Downloading(_) => fl!("sharing-setup-download"),
            Self::Toolchain => fl!("sharing-setup-toolchain"),
            Self::Dependencies => fl!("sharing-setup-dependencies"),
            Self::Building => fl!("sharing-setup-building"),
            Self::Installing => fl!("sharing-setup-installing"),
            Self::Verifying => fl!("sharing-setup-verifying"),
        }
    }
}

pub async fn croc(output: &mut Sender<Message>) -> Result<(), String> {
    tokio::task::spawn_blocking(crate::device_share::install)
        .await
        .map_err(|_| fl!("sharing-setup-croc-error"))?
        .map_err(|error| {
            eprintln!("Croc installer: {error}");
            fl!("sharing-setup-croc-error")
        })?;
    progress(output, Step::CrocVerifying).await;
    let executable =
        crate::device_share::executable().map_err(|_| fl!("sharing-setup-croc-error"))?;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(executable)
            .args(["--croc", "--version"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    if !result.is_ok_and(|result| result.is_ok_and(|output| output.status.success())) {
        return Err(fl!("sharing-setup-croc-error"));
    }
    Ok(())
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

pub fn builds_from_source() -> bool {
    distribution().is_some_and(|distribution| {
        distribution == Distribution::Arch || std::env::consts::ARCH == "aarch64"
    })
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
    stage: fn(f32) -> Step,
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
                    progress(output, stage(f32::from(percent))).await;
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

async fn prepare_rust(
    build: &mut Command,
    directory: &Path,
    output: &mut Sender<Message>,
) -> Result<(), String> {
    let rust = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("rustc")
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .filter(|result| result.status.success())
    .and_then(|result| String::from_utf8(result.stdout).ok())
    .and_then(|version| {
        version
            .split_whitespace()
            .nth(1)
            .and_then(|version| semver::Version::parse(version).ok())
    });
    let cargo = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("cargo")
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .is_ok_and(|result| result.is_ok_and(|result| result.status.success()));
    if rust.is_some_and(|version| version >= semver::Version::new(1, 88, 0)) && cargo {
        return Ok(());
    }

    progress(output, Step::Toolchain).await;
    let bootstrap = directory.join("rustup-init");
    download(
        "https://static.rust-lang.org/rustup/archive/1.29.2/aarch64-unknown-linux-gnu/rustup-init",
        "a9c168dbea81a84671c6b7a5c1795cb0eb699020110d107ddfca427bd797e697",
        &bootstrap,
        output,
        |_| Step::Toolchain,
    )
    .await
    .map_err(|error| {
        eprintln!("Rust setup download: {error}");
        fl!("sharing-setup-toolchain-error")
    })?;
    std::fs::set_permissions(&bootstrap, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| fl!("sharing-setup-toolchain-error"))?;
    let cargo_directory = directory.join("cargo");
    let rustup_directory = directory.join("rustup");
    command(
        Command::new(bootstrap)
            .args([
                "-y",
                "--no-modify-path",
                "--profile",
                "minimal",
                "--default-toolchain",
                "1.93.0",
            ])
            .env("CARGO_HOME", &cargo_directory)
            .env("RUSTUP_HOME", &rustup_directory)
            .env("RUSTUP_TOOLCHAIN", "1.93.0")
            .env("RUSTUP_INIT_SKIP_PATH_CHECK", "yes"),
    )
    .await
    .map_err(|_| fl!("sharing-setup-toolchain-error"))?;
    let search = std::env::var_os("PATH").unwrap_or_default();
    let search = std::env::join_paths(
        std::iter::once(cargo_directory.join("bin")).chain(std::env::split_paths(&search)),
    )
    .map_err(|_| fl!("sharing-setup-toolchain-error"))?;
    build
        .env("PATH", search)
        .env("CARGO_HOME", cargo_directory)
        .env("RUSTUP_HOME", rustup_directory)
        .env("RUSTUP_TOOLCHAIN", "1.93.0");
    Ok(())
}

async fn build_arm_package(
    distribution: Distribution,
    directory: &Path,
    output: &mut Sender<Message>,
) -> Result<PathBuf, String> {
    progress(output, Step::Dependencies).await;
    let (format, filename) = match distribution {
        Distribution::Debian => {
            command(Command::new("pkexec").args(["/usr/bin/apt-get", "update"])).await?;
            command(Command::new("pkexec").args([
                "/usr/bin/apt-get",
                "install",
                "--yes",
                "--no-install-recommends",
                "build-essential",
                "ca-certificates",
                "curl",
                "dpkg-dev",
            ]))
            .await?;
            ("deb", format!("owly_{VERSION}-1_arm64.deb"))
        }
        Distribution::Fedora => {
            command(Command::new("pkexec").args([
                "/usr/bin/dnf",
                "install",
                "--assumeyes",
                "gcc",
                "binutils",
                "rpm-build",
                "curl",
                "ca-certificates",
            ]))
            .await?;
            ("rpm", format!("owly-{VERSION}-1.aarch64.rpm"))
        }
        Distribution::Arch => return Err(fl!("sharing-setup-platform-error")),
    };
    let packages = directory.join("packages");
    std::fs::create_dir(&packages).map_err(|_| fl!("sharing-setup-install-error"))?;
    let mut build = Command::new("sh");
    build
        .args(["packaging/package.sh", format])
        .current_dir(directory.join(format!("owly-{SOURCE}")))
        .env("OUT", &packages)
        .env_remove("CARGO_TARGET_DIR");
    prepare_rust(&mut build, directory, output).await?;
    progress(output, Step::Building).await;
    command(&mut build).await?;
    let package = packages.join(filename);
    if !package.is_file() {
        return Err(fl!("sharing-setup-install-error"));
    }
    Ok(package)
}

pub async fn run(output: &mut Sender<Message>) -> Result<(), String> {
    let distribution = distribution().ok_or_else(|| fl!("sharing-setup-platform-error"))?;
    let architecture = std::env::consts::ARCH;
    if !matches!(architecture, "x86_64" | "aarch64") {
        return Err(fl!("sharing-setup-architecture-error"));
    }
    let directory = tempfile::Builder::new()
        .prefix("orbit-owly-")
        .tempdir()
        .map_err(|_| fl!("sharing-setup-install-error"))?;
    let source_build = distribution == Distribution::Arch || architecture == "aarch64";
    let (url, checksum, filename) = match (distribution, architecture) {
        (Distribution::Arch, _) | (_, "aarch64") => (
            format!("https://codeload.github.com/t4t5/owly/tar.gz/{SOURCE}"),
            "2df3b08f9e7c1b8e29f4511065f599b8c887c28dc0a272a059f9f995ea2ca487",
            "owly.tar.gz",
        ),
        (Distribution::Debian, _) => (
            format!(
                "https://github.com/t4t5/owly/releases/download/v{VERSION}/owly_{VERSION}-1_amd64.deb"
            ),
            "9e891f9cf23a7697d849115fbe93eec64743651c58ca7eb79957f1d4e00e9a08",
            "owly.deb",
        ),
        (Distribution::Fedora, _) => (
            format!(
                "https://github.com/t4t5/owly/releases/download/v{VERSION}/owly-{VERSION}-1.x86_64.rpm"
            ),
            "82b155cf0defda58ac85f683fe9f2888926b92dd38757c01f6083bd4f379e768",
            "owly.rpm",
        ),
    };
    let mut package = directory.path().join(filename);
    download(&url, checksum, &package, output, Step::Downloading).await?;
    if source_build {
        let source = package.clone();
        let destination = directory.path().to_owned();
        tokio::task::spawn_blocking(move || {
            tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(source)?))
                .unpack(destination)
        })
        .await
        .map_err(|_| fl!("sharing-setup-install-error"))?
        .map_err(|_| fl!("sharing-setup-install-error"))?;
        if distribution != Distribution::Arch {
            package = build_arm_package(distribution, directory.path(), output).await?;
        }
    }
    match distribution {
        Distribution::Arch => {
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
    if !super::installed_version().await.is_some_and(|version| {
        version
            .cmp_precedence(&semver::Version::parse(VERSION).unwrap())
            .is_ge()
    }) {
        return Err(fl!("sharing-setup-verify-error"));
    }
    Ok(())
}
