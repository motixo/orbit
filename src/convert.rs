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
    io::{self, BufWriter, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const MEMORY_LIMIT: usize = 512 * 1024 * 1024;
const INPUT_LIMIT: u64 = 128 * 1024 * 1024;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
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
    let directory = Directory::new("orbit-image-")?;
    let path = directory
        .path()
        .join(source.file_name().unwrap_or_else(|| "image".as_ref()))
        .with_extension(if conversion.target() == ImageFormat::Jpeg {
            "jpeg"
        } else {
            "png"
        });
    // Decode in a bounded process so a codec panic or allocation failure cannot strand the batch.
    let child = Command::new("/proc/self/exe")
        .arg("--convert-image")
        .arg(serde_json::to_string(&conversion)?)
        .arg(source)
        .arg(&path)
        .env("RAYON_NUM_THREADS", "2")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()?;
    wait_for_conversion(child, cancelled)?;
    Ok(ImportedFiles {
        files: FileList(vec![path]),
        directory: Some(directory),
        text: None,
    })
}

fn wait_for_conversion(mut child: Child, cancelled: impl Fn() -> bool) -> Result<()> {
    let started = Instant::now();
    let result = loop {
        if cancelled() || started.elapsed() >= Duration::from_secs(120) {
            break Err(io::Error::other("Conversion cancelled or timed out"));
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(status)) => {
                break Err(io::Error::other(format!("Image decoder failed: {status}")));
            }
            Err(error) => break Err(error),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(result?)
}

pub fn run_worker(args: &[std::ffi::OsString]) -> io::Result<()> {
    use rustix::process::{Resource, Rlimit, Signal, set_parent_process_death_signal, setrlimit};
    set_parent_process_death_signal(Some(Signal::KILL))?;
    for (resource, limit) in [
        (Resource::As, (MEMORY_LIMIT * 2) as u64),
        (Resource::Cpu, 120),
        (Resource::Core, 0),
    ] {
        setrlimit(
            resource,
            Rlimit {
                current: Some(limit),
                maximum: Some(limit),
            },
        )?;
    }
    let [conversion, source, destination] = args else {
        return Err(io::Error::other("Invalid conversion arguments"));
    };
    let conversion = serde_json::from_str(
        conversion
            .to_str()
            .ok_or_else(|| io::Error::other("Invalid conversion"))?,
    )?;
    write_converted_image(
        Path::new(source),
        conversion,
        Path::new(destination),
        || false,
    )
    .map_err(io::Error::other)
}

fn write_converted_image(
    source: &Path,
    conversion: Conversion,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<()> {
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
    let mut writer = BufWriter::new(File::create_new(destination)?);
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
    Ok(())
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
    let bytes = read_input(source)?;
    let info = heic_decoder::probe(&bytes)?;
    info.coded_width
        .checked_mul(info.coded_height)
        .and_then(|size| size.checked_mul(8))
        .filter(|&size| size <= MEMORY_LIMIT)
        .ok_or_else(|| io::Error::other("Image is too large"))?;
    let decoded = heic_decoder::decode(&bytes)?;
    let rgba = decoded.to_rgba8()?;
    let premultiplied = rgba.premultiplied;
    let mut image = DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(rgba.width.try_into()?, rgba.height.try_into()?, rgba.data)
            .ok_or_else(|| io::Error::other("Invalid decoded image"))?,
    );
    let profile = decoded.metadata.icc_profile;
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
    with_cicp_profile(
        image,
        profile,
        decoded
            .color_info
            .map(|info| (info.color_primaries, info.transfer_characteristics)),
    )
}

fn decode_avif(source: &Path) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let bytes = read_input(source)?;
    let avif = oxideav_avif::parse(&bytes)?;
    let alpha_id = oxideav_avif::find_alpha_item_id(&avif.meta, avif.primary_item_id);
    let alpha = alpha_id
        .map(|id| oxideav_avif::inspect::item_payload_bytes(&bytes, id))
        .transpose()?;
    let premultiplied = alpha_id.is_some_and(|alpha_id| {
        avif.meta.irefs.iter().any(|reference| {
            reference.reference_type == *b"prem"
                && ((reference.from_id == avif.primary_item_id
                    && reference.to_ids.contains(&alpha_id))
                    || (reference.from_id == alpha_id
                        && reference.to_ids.contains(&avif.primary_item_id)))
        })
    });
    let mut decoder = avif_decode::Decoder::new(2, (MEMORY_LIMIT / 8) as u32)?;
    let image = decoder.decode(&avif.primary_item_data, alpha.as_deref(), premultiplied)?;
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
    let properties = avif.meta.properties_for(avif.primary_item_id);
    let description = properties
        .iter()
        .find_map(|property| {
            if let oxideav_avif::meta::Property::Colr(oxideav_avif::meta::Colr::Nclx {
                colour_primaries,
                transfer_characteristics,
                ..
            }) = property
            {
                Some((*colour_primaries, *transfer_characteristics))
            } else {
                None
            }
        })
        .or_else(|| decoder.color_description());
    let (image, profile) = apply_avif_properties(image, &properties)?;
    with_cicp_profile(image, profile, description)
}

fn with_cicp_profile(
    image: DynamicImage,
    profile: Option<Vec<u8>>,
    description: Option<(u16, u16)>,
) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    if profile.is_some() {
        return Ok((image, profile));
    }
    let Some((primaries, transfer)) =
        description.filter(|&(primaries, transfer)| primaries != 2 && transfer != 2)
    else {
        return Ok((image, None));
    };
    // Matrix and range have already been expanded by the codec into full-range RGB.
    let profile = moxcms::ColorProfile::new_from_cicp(moxcms::CicpProfile {
        color_primaries: u8::try_from(primaries)?.try_into()?,
        transfer_characteristics: u8::try_from(transfer)?.try_into()?,
        matrix_coefficients: moxcms::MatrixCoefficients::Identity,
        full_range: true,
    });
    if profile.cicp.is_none()
        || profile.red_trc.is_none()
        || profile.green_trc.is_none()
        || profile.blue_trc.is_none()
    {
        return Err(io::Error::other("Unsupported image color description").into());
    }
    let image = match image.color() {
        image::ColorType::L8 => DynamicImage::ImageRgb8(image.into_rgb8()),
        image::ColorType::L16 => DynamicImage::ImageRgb16(image.into_rgb16()),
        image::ColorType::La8 => DynamicImage::ImageRgba8(image.into_rgba8()),
        image::ColorType::La16 => DynamicImage::ImageRgba16(image.into_rgba16()),
        _ => image,
    };
    Ok((image, Some(profile.encode()?)))
}

fn apply_avif_properties(
    mut image: DynamicImage,
    properties: &[&oxideav_avif::meta::Property],
) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    use oxideav_avif::meta::{Colr, Property};
    let mut profile = None;
    for property in properties {
        match property {
            Property::Colr(Colr::Icc(icc)) => profile = Some(icc.clone()),
            Property::Clap(crop) => {
                let (left, width) = crop_axis(
                    image.width(),
                    crop.clean_aperture_width_n,
                    crop.clean_aperture_width_d,
                    crop.horiz_off_n,
                    crop.horiz_off_d,
                )?;
                let (top, height) = crop_axis(
                    image.height(),
                    crop.clean_aperture_height_n,
                    crop.clean_aperture_height_d,
                    crop.vert_off_n,
                    crop.vert_off_d,
                )?;
                image = image.crop_imm(left, top, width, height);
            }
            Property::Irot(rotation) => image.apply_orientation(match rotation.angle {
                0 => Orientation::NoTransforms,
                1 => Orientation::Rotate270,
                2 => Orientation::Rotate180,
                3 => Orientation::Rotate90,
                _ => return Err(io::Error::other("Invalid image rotation").into()),
            }),
            Property::Imir(mirror) => image.apply_orientation(match mirror.axis {
                0 => Orientation::FlipVertical,
                1 => Orientation::FlipHorizontal,
                _ => return Err(io::Error::other("Invalid image mirror").into()),
            }),
            _ => (),
        }
    }
    Ok((image, profile))
}

fn crop_axis(
    full: u32,
    extent_n: i32,
    extent_d: i32,
    offset_n: i32,
    offset_d: i32,
) -> Result<(u32, u32)> {
    let invalid = || io::Error::other("Invalid or fractional image crop");
    if extent_n <= 0 || extent_d <= 0 || offset_d <= 0 || extent_n % extent_d != 0 {
        return Err(invalid().into());
    }
    let size = (extent_n / extent_d) as u32;
    let remaining = full.checked_sub(size).ok_or_else(invalid)?;
    let denominator = 2 * i128::from(offset_d);
    let numerator = i128::from(remaining) * i128::from(offset_d) + 2 * i128::from(offset_n);
    if numerator < 0
        || numerator > i128::from(remaining) * denominator
        || numerator % denominator != 0
    {
        return Err(invalid().into());
    }
    Ok(((numerator / denominator) as u32, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cicp_profiles_keep_primaries_transfer_and_existing_icc() {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            1,
            image::Rgba([60, 110, 160, 128]),
        ));
        for description in [(12, 13), (9, 16), (9, 18)] {
            let (output, profile) =
                with_cicp_profile(image.clone(), None, Some(description)).unwrap();
            assert_eq!(output.as_bytes(), image.as_bytes());
            let profile = moxcms::ColorProfile::new_from_slice(&profile.unwrap()).unwrap();
            let cicp = profile.cicp.unwrap();
            assert_eq!(cicp.color_primaries as u16, description.0);
            assert_eq!(cicp.transfer_characteristics as u16, description.1);
            assert_eq!(
                cicp.matrix_coefficients,
                moxcms::MatrixCoefficients::Identity
            );
            assert!(cicp.full_range);
        }
        let (_, profile) =
            with_cicp_profile(image.clone(), Some(vec![1, 2, 3]), Some((12, 13))).unwrap();
        assert_eq!(profile.unwrap(), [1, 2, 3]);
        let (_, profile) = with_cicp_profile(image.clone(), None, Some((2, 2))).unwrap();
        assert!(profile.is_none());
        assert!(with_cicp_profile(image, None, Some((255, 255))).is_err());
        let (output, _) = with_cicp_profile(
            DynamicImage::ImageLuma16(image::ImageBuffer::from_pixel(
                1,
                1,
                image::Luma([32768_u16]),
            )),
            None,
            Some((9, 16)),
        )
        .unwrap();
        assert_eq!(output.color(), image::ColorType::Rgb16);
        assert_eq!(output.into_rgb16().as_raw(), &[32768; 3]);
    }

    fn convert(
        source: &Path,
        conversion: Conversion,
        cancelled: impl Fn() -> bool,
    ) -> Result<ImportedFiles> {
        let directory = Directory::new("orbit-image-")?;
        let path = directory
            .path()
            .join(source.file_name().unwrap())
            .with_extension(if conversion.target() == ImageFormat::Jpeg {
                "jpeg"
            } else {
                "png"
            });
        write_converted_image(source, conversion, &path, cancelled)?;
        Ok(ImportedFiles {
            files: FileList(vec![path]),
            directory: Some(directory),
            text: None,
        })
    }

    #[test]
    fn failed_or_cancelled_decoder_returns_an_error_and_is_reaped() {
        let child = Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
        assert!(wait_for_conversion(child, || false).is_err());
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
        assert!(wait_for_conversion(child, || true).is_err());
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }

    #[test]
    fn converts_ten_bit_and_tiled_heic_without_changing_the_source() {
        // Fixtures: Multipad-cyber/heic-decoder-rust, tests/fixtures (see HEIC-LICENSE).
        let fixture = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            (
                "ten-bit.heic",
                include_bytes!("fixtures/heic-10bit.heic").as_slice(),
            ),
            (
                "grid.heic",
                include_bytes!("fixtures/heic-grid.heic").as_slice(),
            ),
        ] {
            let source = fixture.path().join(name);
            std::fs::write(&source, bytes).unwrap();
            let info = heic_decoder::probe(bytes).unwrap();
            let converted = convert(&source, Conversion::HeicToJpeg, || false).unwrap();
            let image = image::open(&converted.files.0[0]).unwrap();
            assert_eq!(
                (image.width() as usize, image.height() as usize),
                (info.width, info.height)
            );
            assert_eq!(std::fs::read(source).unwrap(), bytes);
        }
        assert_eq!(
            heic_decoder::probe(include_bytes!("fixtures/heic-10bit.heic"))
                .unwrap()
                .bit_depth_luma,
            10
        );
    }

    #[test]
    fn avif_properties_preserve_crop_transform_order_and_icc() {
        use oxideav_avif::meta::{Clap, Colr, Imir, Irot, Property};
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_fn(6, 4, |x, y| {
            image::Rgb([(x + y * 6) as u8, 0, 0])
        }));
        let crop = Property::Clap(Clap::new(4, 1, 2, 1, 0, 1, 0, 1));
        let rotation = Property::Irot(Irot::new(1));
        let mirror = Property::Imir(Imir::new(0));
        let icc = Property::Colr(Colr::Icc(vec![1, 2, 3]));
        let (converted, profile) =
            apply_avif_properties(image.clone(), &[&crop, &rotation, &mirror, &icc]).unwrap();
        assert_eq!((converted.width(), converted.height()), (2, 4));
        assert_eq!(
            converted
                .to_rgb8()
                .pixels()
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            [7, 13, 8, 14, 9, 15, 10, 16]
        );
        assert_eq!(profile.unwrap(), [1, 2, 3]);
        let mirror = Property::Imir(Imir::new(1));
        let (converted, _) = apply_avif_properties(image, &[&crop, &mirror]).unwrap();
        assert_eq!(
            converted
                .to_rgb8()
                .pixels()
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            [10, 9, 8, 7, 16, 15, 14, 13]
        );
        for crop in [
            (4, 0, 0, 1),
            (4, 1, 0, 0),
            (7, 1, 0, 1),
            (3, 2, 0, 1),
            (4, 1, 5, 1),
            (3, 1, 0, 1),
        ] {
            assert!(crop_axis(6, crop.0, crop.1, crop.2, crop.3).is_err());
        }
        assert_eq!(crop_axis(6, 4, 1, -1, 1).unwrap(), (0, 4));
    }

    #[test]
    fn converts_avif_with_container_transforms_and_icc() {
        // Fixtures: AOMediaCodec/libavif, tests/data (see AVIF-LICENSE).
        let fixture = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            (
                "transforms.avif",
                include_bytes!("fixtures/avif-transforms.avif").as_slice(),
            ),
            (
                "profile.avif",
                include_bytes!("fixtures/avif-icc.avif").as_slice(),
            ),
        ] {
            let source = fixture.path().join(name);
            std::fs::write(&source, bytes).unwrap();
            for conversion in [Conversion::AvifToJpeg, Conversion::AvifToPng] {
                let converted = convert(&source, conversion, || false).unwrap();
                let mut decoder = ImageReader::open(&converted.files.0[0])
                    .unwrap()
                    .with_guessed_format()
                    .unwrap()
                    .into_decoder()
                    .unwrap();
                if name == "profile.avif" {
                    let avif = oxideav_avif::parse(bytes).unwrap();
                    let original_icc = avif
                        .meta
                        .properties_for(avif.primary_item_id)
                        .into_iter()
                        .find_map(|property| {
                            if let oxideav_avif::meta::Property::Colr(
                                oxideav_avif::meta::Colr::Icc(profile),
                            ) = property
                            {
                                Some(profile)
                            } else {
                                None
                            }
                        })
                        .unwrap();
                    assert_eq!(&decoder.icc_profile().unwrap().unwrap(), original_icc);
                } else {
                    assert_eq!(decoder.dimensions(), (10, 8));
                }
                assert_eq!(std::fs::read(&source).unwrap(), bytes);
            }
        }
    }

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
