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

pub fn generate(path: &Path) -> Option<Handle> {
    let kind = kind(path)?;
    let directory = tempfile::tempdir().ok()?;
    let output = directory.path().join("thumbnail.png");
    let uri = url::Url::from_file_path(path).ok()?;
    // Use the installed desktop thumbnailers without blocking the app or leaving children hung.
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
                .args(["--size", "64"]);
        }
        Kind::Video => {
            command
                .arg("cosmic-player")
                .arg("--thumbnail")
                .arg(&output)
                .args(["--size", "64x64"])
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
        || output.metadata().ok()?.len() > 1024 * 1024
    {
        return None;
    }
    let bytes = std::fs::read(output).ok()?;
    (image::guess_format(&bytes).ok()? == image::ImageFormat::Png)
        .then(|| Handle::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types_include_browser_images_and_common_video_formats() {
        for name in [
            "photo.png",
            "PHOTO.JPG",
            "image.webp",
            "image.avif",
            "drawing.svg",
        ] {
            assert_eq!(kind(Path::new(name)), Some(Kind::Image));
        }
        for name in ["clip.mp4", "CLIP.MOV", "clip.webm", "clip.mkv"] {
            assert_eq!(kind(Path::new(name)), Some(Kind::Video));
        }
        for name in ["Text.txt", "report.pdf", "archive.zip"] {
            assert_eq!(kind(Path::new(name)), None);
        }
    }
}
