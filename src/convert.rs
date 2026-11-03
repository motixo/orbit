// SPDX-License-Identifier: GPL-3.0-only

use crate::{cache::Directory, import::ImportedFiles, transfer::FileList};
use image::{
    ImageEncoder, ImageFormat, ImageReader, codecs::jpeg::JpegEncoder, metadata::Orientation,
};
use image_webp::{DecodingError, WebPDecoder};
use std::{
    error::Error,
    fs::File,
    io::{self, BufWriter, Write},
    path::Path,
};

const MEMORY_LIMIT: usize = 512 * 1024 * 1024;

pub fn candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("webp"))
        && path.is_file()
}

pub fn webp_to_jpeg(
    source: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<ImportedFiles, Box<dyn Error + Send + Sync>> {
    let reader = ImageReader::open(source)?.with_guessed_format()?;
    if reader.format() != Some(ImageFormat::WebP) || cancelled() {
        return Err(io::Error::other("Not a WebP image or conversion cancelled").into());
    }
    let mut decoder = WebPDecoder::new(reader.into_inner())?;
    let size = decoder
        .output_buffer_size()
        .ok_or(DecodingError::ImageTooLarge)?;
    let remaining = MEMORY_LIMIT
        .checked_sub(size)
        .ok_or(DecodingError::MemoryLimitExceeded)?;
    decoder.set_memory_limit(remaining);
    let orientation = decoder
        .exif_metadata()?
        .as_deref()
        .and_then(Orientation::from_exif_chunk)
        .unwrap_or(Orientation::NoTransforms);
    let profile = decoder.icc_profile()?;
    let (width, height) = decoder.dimensions();
    let mut image = if decoder.has_alpha() {
        let mut pixels = image::RgbaImage::new(width, height);
        decoder.read_image(pixels.as_mut())?;
        image::DynamicImage::ImageRgba8(pixels)
    } else {
        let mut pixels = image::RgbImage::new(width, height);
        decoder.read_image(pixels.as_mut())?;
        image::DynamicImage::ImageRgb8(pixels)
    };
    image.apply_orientation(orientation);
    if image.color().has_alpha() {
        let mut pixels = image.into_rgba8();
        for pixel in pixels.pixels_mut() {
            let alpha = u16::from(pixel[3]);
            for channel in &mut pixel.0[..3] {
                *channel = ((u16::from(*channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
            }
            pixel[3] = 255;
        }
        image = image::DynamicImage::ImageRgba8(pixels);
    }
    if cancelled() {
        return Err(io::Error::other("Conversion cancelled").into());
    }
    let directory = Directory::new("orbit-image-")?;
    let path = directory
        .path()
        .join(source.file_name().unwrap_or_else(|| "image.webp".as_ref()))
        .with_extension("jpeg");
    let mut writer = BufWriter::new(File::create_new(&path)?);
    let mut encoder = JpegEncoder::new_with_quality(&mut writer, 90);
    if let Some(profile) = profile {
        encoder.set_icc_profile(profile)?;
    }
    encoder.encode_image(&image)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    if cancelled() {
        return Err(io::Error::other("Conversion cancelled").into());
    }
    Ok(ImportedFiles {
        files: FileList(vec![path]),
        directory: Some(directory),
        text: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_metadata_before_reading_it() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("metadata.webp");
        let mut image = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut image)
            .write_image(&[255, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        for chunk in [b"EXIF", b"ICCP"] {
            let mut bytes = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
            bytes.extend_from_slice(&10_u32.to_le_bytes());
            bytes.extend_from_slice(&[0; 10]);
            bytes.extend_from_slice(&image[12..]);
            bytes.extend_from_slice(chunk);
            bytes.extend_from_slice(&(MEMORY_LIMIT as u32).to_le_bytes());
            let length = (bytes.len() - 8) as u32;
            bytes[4..8].copy_from_slice(&length.to_le_bytes());
            std::fs::write(&source, bytes).unwrap();
            let error = webp_to_jpeg(&source, || false).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<DecodingError>(),
                Some(DecodingError::MemoryLimitExceeded)
            ));
        }
    }

    #[test]
    fn conversion_keeps_originals_and_rejects_other_formats() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("photo.WEBP");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 0]))
            .save_with_format(&source, ImageFormat::WebP)
            .unwrap();
        let original = std::fs::read(&source).unwrap();
        assert!(candidate(&source));
        let converted = webp_to_jpeg(&source, || false).unwrap();
        let output = &converted.files.0[0];
        assert_eq!(output.file_name().unwrap(), "photo.jpeg");
        assert_ne!(output.parent(), source.parent());
        assert_eq!(std::fs::read(&source).unwrap(), original);
        let jpeg = std::fs::read(output).unwrap();
        assert_eq!(image::guess_format(&jpeg).unwrap(), ImageFormat::Jpeg);
        let image = image::load_from_memory(&jpeg).unwrap().into_rgb8();
        assert_eq!(image.dimensions(), (2, 2));
        assert!(
            image
                .pixels()
                .all(|pixel| pixel.0.iter().all(|channel| *channel >= 245))
        );

        let fake = fixture.path().join("other.webp");
        std::fs::write(&fake, &jpeg).unwrap();
        assert!(webp_to_jpeg(&fake, || false).is_err());
        assert_eq!(std::fs::read(&fake).unwrap(), jpeg);
        assert!(webp_to_jpeg(&source, || true).is_err());
        assert_eq!(std::fs::read(source).unwrap(), original);
    }
}
