// SPDX-License-Identifier: GPL-3.0-only

use crate::{cache::Directory, import::ImportedFiles, transfer::FileList};
use image::{
    DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
    metadata::Orientation,
};
use image_webp::{DecodingError, WebPDecoder};
use std::{
    error::Error,
    fs::File,
    io::{self, BufWriter, Cursor, Read, Write},
    path::Path,
};

const MEMORY_LIMIT: usize = 512 * 1024 * 1024;
const INPUT_LIMIT: u64 = 128 * 1024 * 1024;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Clone, Copy, Debug)]
pub enum Conversion {
    WebpToJpeg,
    HeicToJpeg,
    AvifToJpeg,
    AvifToPng,
    PngToJpeg,
    JpegToPng,
}

impl Conversion {
    pub const ALL: [Self; 6] = [
        Self::WebpToJpeg,
        Self::HeicToJpeg,
        Self::AvifToJpeg,
        Self::AvifToPng,
        Self::PngToJpeg,
        Self::JpegToPng,
    ];

    pub fn candidate(self, path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| match self {
                Self::WebpToJpeg => extension.eq_ignore_ascii_case("webp"),
                Self::HeicToJpeg => ["heic", "heif"]
                    .iter()
                    .any(|ext| extension.eq_ignore_ascii_case(ext)),
                Self::AvifToJpeg | Self::AvifToPng => extension.eq_ignore_ascii_case("avif"),
                Self::PngToJpeg => extension.eq_ignore_ascii_case("png"),
                Self::JpegToPng => ["jpeg", "jpg", "jpe", "jfif"]
                    .iter()
                    .any(|ext| extension.eq_ignore_ascii_case(ext)),
            })
            && path.is_file()
    }

    fn target(self) -> ImageFormat {
        match self {
            Self::AvifToPng | Self::JpegToPng => ImageFormat::Png,
            _ => ImageFormat::Jpeg,
        }
    }
}

pub fn convert(
    source: &Path,
    conversion: Conversion,
    cancelled: impl Fn() -> bool,
) -> Result<ImportedFiles> {
    if cancelled() || !conversion.candidate(source) {
        return Err(io::Error::other("Unsupported image or conversion cancelled").into());
    }
    let (mut image, profile) = match conversion {
        Conversion::WebpToJpeg => decode_webp(source)?,
        Conversion::HeicToJpeg => decode_heic(source)?,
        Conversion::AvifToJpeg | Conversion::AvifToPng => decode_avif(source)?,
        Conversion::PngToJpeg => decode_image(source, ImageFormat::Png)?,
        Conversion::JpegToPng => decode_image(source, ImageFormat::Jpeg)?,
    };
    let target = conversion.target();
    if target == ImageFormat::Jpeg && image.color().has_alpha() {
        let mut pixels = image.into_rgba8();
        for pixel in pixels.pixels_mut() {
            let alpha = u16::from(pixel[3]);
            for channel in &mut pixel.0[..3] {
                *channel = ((u16::from(*channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
            }
            pixel[3] = 255;
        }
        image = DynamicImage::ImageRgba8(pixels);
    }
    if cancelled() {
        return Err(io::Error::other("Conversion cancelled").into());
    }
    let directory = Directory::new("orbit-image-")?;
    let path = directory
        .path()
        .join(source.file_name().unwrap_or_else(|| "image".as_ref()))
        .with_extension(if target == ImageFormat::Jpeg {
            "jpeg"
        } else {
            "png"
        });
    let mut writer = BufWriter::new(File::create_new(&path)?);
    if target == ImageFormat::Jpeg {
        let mut encoder = JpegEncoder::new_with_quality(&mut writer, 90);
        if let Some(profile) = profile {
            encoder.set_icc_profile(profile)?;
        }
        encoder.encode_image(&image)?;
    } else {
        let mut encoder = PngEncoder::new(&mut writer);
        if let Some(profile) = profile {
            encoder.set_icc_profile(profile)?;
        }
        image.write_with_encoder(encoder)?;
    }
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

fn decode_webp(source: &Path) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let reader = ImageReader::open(source)?.with_guessed_format()?;
    if reader.format() != Some(ImageFormat::WebP) {
        return Err(io::Error::other("Not a WebP image").into());
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
    Ok((image, profile))
}

fn decode_image(source: &Path, expected: ImageFormat) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let reader = ImageReader::open(source)?.with_guessed_format()?;
    if reader.format() != Some(expected) {
        return Err(io::Error::other("Unexpected image format").into());
    }
    let mut decoder = reader.into_decoder()?;
    let mut limits = image::Limits::default();
    limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(limits)?;
    let orientation = decoder.orientation()?;
    let profile = decoder.icc_profile()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok((image, profile))
}

fn read_input(source: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(source)?
        .take(INPUT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > INPUT_LIMIT {
        return Err(io::Error::other("Image input is too large").into());
    }
    Ok(bytes)
}

fn decode_heic(source: &Path) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    use zune_heic::zune_core::{
        bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions,
    };
    let bytes = read_input(source)?;
    let options = DecoderOptions::default()
        .set_hevc_max_mdat_size(INPUT_LIMIT as usize)
        .set_num_threads(2);
    let mut decoder = zune_heic::HeifDecoder::new_with_options(ZCursor::new(&bytes), options);
    decoder.decode_headers()?;
    let size = decoder
        .output_buffer_size()
        .ok_or_else(|| io::Error::other("Invalid image size"))?;
    let mut limits = image::Limits::default();
    limits.reserve_usize(size)?;
    if let Some(profile) = decoder.icc_data() {
        limits.reserve_usize(profile.len())?;
    }
    let profile = decoder.icc_data().cloned();
    let width = decoder
        .width()
        .ok_or_else(|| io::Error::other("Missing image width"))?
        .try_into()?;
    let height = decoder
        .height()
        .ok_or_else(|| io::Error::other("Missing image height"))?
        .try_into()?;
    let colorspace = decoder.colorspace();
    let premultiplied = decoder.is_alpha_premultiplied();
    let pixels = decoder.decode()?;
    let invalid = || io::Error::other("Invalid decoded image");
    let mut image = match colorspace {
        Some(ColorSpace::RGB) => DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(width, height, pixels).ok_or_else(invalid)?,
        ),
        Some(ColorSpace::RGBA) => DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(width, height, pixels).ok_or_else(invalid)?,
        ),
        Some(ColorSpace::Luma) => DynamicImage::ImageLuma8(
            image::GrayImage::from_raw(width, height, pixels).ok_or_else(invalid)?,
        ),
        Some(ColorSpace::LumaA) => DynamicImage::ImageLumaA8(
            image::GrayAlphaImage::from_raw(width, height, pixels).ok_or_else(invalid)?,
        ),
        _ => return Err(io::Error::other("Unsupported image colorspace").into()),
    };
    if premultiplied {
        let mut pixels = image.into_rgba8();
        for pixel in pixels.pixels_mut() {
            let alpha = u16::from(pixel[3]);
            for channel in &mut pixel.0[..3] {
                *channel = (u16::from(*channel) * 255 + alpha / 2)
                    .checked_div(alpha)
                    .unwrap_or(0)
                    .min(255) as u8;
            }
        }
        image = DynamicImage::ImageRgba8(pixels);
    }
    Ok((image, profile))
}

fn decode_avif(source: &Path) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let bytes = read_input(source)?;
    let avif = avif_parse::read_avif(&mut Cursor::new(&bytes))?;
    let mut decoder = avif_decode::Decoder::new(2, (MEMORY_LIMIT / 8) as u32)?;
    let image = decoder.decode(
        &avif.primary_item,
        avif.alpha_item.as_deref(),
        avif.premultiplied_alpha,
    )?;
    macro_rules! image {
        ($pixels:ident, $variant:ident) => {
            DynamicImage::$variant(
                image::ImageBuffer::from_raw(
                    $pixels.width().try_into()?,
                    $pixels.height().try_into()?,
                    bytemuck::allocation::cast_vec($pixels.into_buf()),
                )
                .ok_or_else(|| io::Error::other("Invalid decoded image"))?,
            )
        };
    }
    let image = match image {
        avif_decode::Image::Rgb8(pixels) => image!(pixels, ImageRgb8),
        avif_decode::Image::Rgb16(pixels) => image!(pixels, ImageRgb16),
        avif_decode::Image::Rgba8(pixels) => image!(pixels, ImageRgba8),
        avif_decode::Image::Rgba16(pixels) => image!(pixels, ImageRgba16),
        avif_decode::Image::Gray8(pixels) => image!(pixels, ImageLuma8),
        avif_decode::Image::Gray16(pixels) => image!(pixels, ImageLuma16),
    };
    Ok((image, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_and_jpeg_conversion_keeps_sources_and_validates_formats() {
        let fixture = tempfile::tempdir().unwrap();
        for (conversion, input, extension, output) in [
            (
                Conversion::PngToJpeg,
                ImageFormat::Png,
                "PNG",
                ImageFormat::Jpeg,
            ),
            (
                Conversion::JpegToPng,
                ImageFormat::Jpeg,
                "JPG",
                ImageFormat::Png,
            ),
        ] {
            let source = fixture.path().join(format!("photo.{extension}"));
            image::RgbImage::from_pixel(2, 3, image::Rgb([80, 120, 160]))
                .save_with_format(&source, input)
                .unwrap();
            let original = std::fs::read(&source).unwrap();
            let converted = convert(&source, conversion, || false).unwrap();
            let path = &converted.files.0[0];
            assert_ne!(path.parent(), source.parent());
            let bytes = std::fs::read(path).unwrap();
            assert_eq!(image::guess_format(&bytes).unwrap(), output);
            let image = image::load_from_memory(&bytes).unwrap();
            assert_eq!((image.width(), image.height()), (2, 3));
            assert_eq!(std::fs::read(&source).unwrap(), original);
        }
        for (extension, conversion) in [
            ("heic", Conversion::HeicToJpeg),
            ("avif", Conversion::AvifToJpeg),
            ("avif", Conversion::AvifToPng),
            ("png", Conversion::PngToJpeg),
            ("jpeg", Conversion::JpegToPng),
        ] {
            let source = fixture.path().join(format!("invalid.{extension}"));
            std::fs::write(&source, b"not an image").unwrap();
            assert!(conversion.candidate(&source));
            assert!(convert(&source, conversion, || false).is_err());
            assert_eq!(std::fs::read(source).unwrap(), b"not an image");
        }
    }

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
            let error = convert(&source, Conversion::WebpToJpeg, || false).unwrap_err();
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
        assert!(Conversion::WebpToJpeg.candidate(&source));
        let converted = convert(&source, Conversion::WebpToJpeg, || false).unwrap();
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
        assert!(convert(&fake, Conversion::WebpToJpeg, || false).is_err());
        assert_eq!(std::fs::read(&fake).unwrap(), jpeg);
        assert!(convert(&source, Conversion::WebpToJpeg, || true).is_err());
        assert_eq!(std::fs::read(source).unwrap(), original);
    }
}
