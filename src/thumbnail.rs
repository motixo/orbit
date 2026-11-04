// SPDX-License-Identifier: GPL-3.0-only

use cosmic::widget::image::Handle;
use std::{
    path::Path,
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
}

pub fn kind(path: &Path) -> Option<Kind> {
    match mime_guess::from_path(path).first()?.type_().as_str() {
        "image" => Some(Kind::Image),
        "video" => Some(Kind::Video),
        _ => None,
    }
}

pub fn generate(path: &Path, size: u32) -> Option<Handle> {
    bytes(path, size).map(Handle::from_bytes)
}

pub fn bytes(path: &Path, size: u32) -> Option<Vec<u8>> {
    let kind = kind(path)?;
    let directory = tempfile::tempdir().ok()?;
    let output = directory.path().join("thumbnail.png");
    let uri = url::Url::from_file_path(path).ok()?;
    let mut command = Command::new("timeout");
    command.args(["--kill-after=2s", "15s"]);
    match kind {
        Kind::Image => {
            command
                .arg("glycin-thumbnailer")
                .arg("--input")
                .arg(uri.as_str())
                .arg("--output")
                .arg(&output)
                .arg("--size")
                .arg(size.to_string());
        }
        Kind::Video => {
            command
                .arg("cosmic-player")
                .arg("--thumbnail")
                .arg(&output)
                .arg("--size")
                .arg(format!("{size}x{size}"))
                .arg(uri.as_str());
        }
    }
    if !command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success()
        || output.metadata().ok()?.len() > (u64::from(size) * u64::from(size) * 8).max(1024 * 1024)
    {
        return None;
    }
    let bytes = std::fs::read(output).ok()?;
    (image::guess_format(&bytes).ok()? == image::ImageFormat::Png).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_thumbnail_candidates() {
        for (name, expected) in [
            ("IMAGE.AVIF", Some(Kind::Image)),
            ("clip.mp4", Some(Kind::Video)),
            ("report.pdf", None),
        ] {
            assert_eq!(kind(Path::new(name)), expected);
        }
    }
}
