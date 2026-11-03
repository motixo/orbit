// SPDX-License-Identifier: GPL-3.0-only

use image::{ImageDecoder, ImageEncoder, ImageFormat, ImageReader};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(conversion: &str, source: &Path, destination: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit"))
        .arg("--convert-image")
        .arg(serde_json::to_string(conversion).unwrap())
        .arg(source)
        .arg(destination)
        .env("RAYON_NUM_THREADS", "2")
        .output()
        .unwrap()
}

#[test]
fn worker_converts_images_and_keeps_sources_and_existing_outputs() {
    let fixture = tempfile::tempdir().unwrap();
    for (conversion, input, extension, output, target) in [
        (
            "WebpToJpeg",
            ImageFormat::WebP,
            "WEBP",
            ImageFormat::Jpeg,
            "jpeg",
        ),
        (
            "PngToJpeg",
            ImageFormat::Png,
            "PNG",
            ImageFormat::Jpeg,
            "jpeg",
        ),
        (
            "JpegToPng",
            ImageFormat::Jpeg,
            "JPG",
            ImageFormat::Png,
            "png",
        ),
    ] {
        let source = fixture.path().join(format!("عکس {extension}.{extension}"));
        if input == ImageFormat::Jpeg {
            image::RgbImage::from_pixel(2, 3, image::Rgb([255, 0, 0]))
                .save_with_format(&source, input)
                .unwrap();
        } else {
            image::RgbaImage::from_pixel(2, 3, image::Rgba([255, 0, 0, 0]))
                .save_with_format(&source, input)
                .unwrap();
        }
        let original = fs::read(&source).unwrap();
        let destination = fixture.path().join(format!("{conversion}.{target}"));
        let result = run(conversion, &source, &destination);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = fs::read(&destination).unwrap();
        assert_eq!(image::guess_format(&bytes).unwrap(), output);
        let image = image::load_from_memory(&bytes).unwrap().into_rgb8();
        assert_eq!(image.dimensions(), (2, 3));
        if input != ImageFormat::Jpeg {
            assert!(
                image
                    .pixels()
                    .all(|pixel| pixel.0.iter().all(|&channel| channel >= 245))
            );
        }
        assert!(!run(conversion, &source, &destination).status.success());
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert_eq!(fs::read(&source).unwrap(), original);
    }
}

#[test]
fn worker_keeps_grayscale_jpeg_channels_and_profile() {
    let fixture = tempfile::tempdir().unwrap();
    let profile = moxcms::ColorProfile::new_gray_with_gamma(2.2)
        .encode()
        .unwrap();
    for (index, image) in [
        image::DynamicImage::ImageLuma8(image::ImageBuffer::from_pixel(2, 2, image::Luma([180]))),
        image::DynamicImage::ImageLumaA8(image::ImageBuffer::from_pixel(
            2,
            2,
            image::LumaA([180, 128]),
        )),
        image::DynamicImage::ImageLuma16(image::ImageBuffer::from_pixel(
            2,
            2,
            image::Luma([32768_u16]),
        )),
        image::DynamicImage::ImageLumaA16(image::ImageBuffer::from_pixel(
            2,
            2,
            image::LumaA([32768_u16, 32768]),
        )),
    ]
    .into_iter()
    .enumerate()
    {
        let source = fixture.path().join(format!("gray-{index}.png"));
        let mut encoder =
            image::codecs::png::PngEncoder::new(fs::File::create_new(&source).unwrap());
        encoder.set_icc_profile(profile.clone()).unwrap();
        image.write_with_encoder(encoder).unwrap();
        let original = fs::read(&source).unwrap();
        let destination = source.with_extension("jpeg");
        let result = run("PngToJpeg", &source, &destination);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mut decoder = ImageReader::open(destination)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(decoder.color_type(), image::ColorType::L8);
        assert_eq!(decoder.icc_profile().unwrap().unwrap(), profile);
        assert_eq!(fs::read(source).unwrap(), original);
    }
}

#[test]
fn worker_converts_ten_bit_and_tiled_heic() {
    // Fixtures: Multipad-cyber/heic-decoder-rust, tests/fixtures (see HEIC-LICENSE).
    let fixture = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        (
            "ten-bit.heic",
            include_bytes!("../src/fixtures/heic-10bit.heic").as_slice(),
        ),
        (
            "grid.heic",
            include_bytes!("../src/fixtures/heic-grid.heic").as_slice(),
        ),
    ] {
        let source = fixture.path().join(name);
        fs::write(&source, bytes).unwrap();
        let info = heic_decoder::probe(bytes).unwrap();
        let destination = source.with_extension("jpeg");
        let result = run("HeicToJpeg", &source, &destination);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let image = image::open(destination).unwrap();
        assert_eq!(
            (image.width() as usize, image.height() as usize),
            (info.width, info.height)
        );
        assert_eq!(fs::read(source).unwrap(), bytes);
    }
    assert_eq!(
        heic_decoder::probe(include_bytes!("../src/fixtures/heic-10bit.heic"))
            .unwrap()
            .bit_depth_luma,
        10
    );
}

#[test]
fn worker_preserves_avif_transforms_icc_and_nclx() {
    use oxideav_avif::meta::{Colr, Property};
    // Fixtures: AOMediaCodec/libavif, tests/data (see AVIF-LICENSE).
    // The P3 variant changes only the transform fixture's NCLX primaries and transfer.
    let fixture = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        (
            "transforms.avif",
            include_bytes!("../src/fixtures/avif-transforms.avif").as_slice(),
        ),
        (
            "profile.avif",
            include_bytes!("../src/fixtures/avif-icc.avif").as_slice(),
        ),
        (
            "p3.avif",
            include_bytes!("../src/fixtures/avif-p3.avif").as_slice(),
        ),
    ] {
        let source = fixture.path().join(name);
        fs::write(&source, bytes).unwrap();
        let avif = oxideav_avif::parse(bytes).unwrap();
        let properties = avif.meta.properties_for(avif.primary_item_id);
        for (conversion, extension) in [("AvifToJpeg", "jpeg"), ("AvifToPng", "png")] {
            let destination = source.with_extension(extension);
            let result = run(conversion, &source, &destination);
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let mut decoder = ImageReader::open(&destination)
                .unwrap()
                .with_guessed_format()
                .unwrap()
                .into_decoder()
                .unwrap();
            let output_icc = decoder.icc_profile().unwrap();
            if name == "profile.avif" {
                let original = properties
                    .iter()
                    .find_map(|property| {
                        if let Property::Colr(Colr::Icc(icc)) = property {
                            Some(icc)
                        } else {
                            None
                        }
                    })
                    .unwrap();
                assert_eq!(&output_icc.unwrap(), original);
            } else if name == "p3.avif" {
                assert_eq!(decoder.dimensions(), (10, 8));
                let (primaries, transfer) = properties
                    .iter()
                    .find_map(|property| {
                        if let Property::Colr(Colr::Nclx {
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
                    .unwrap();
                let cicp = moxcms::ColorProfile::new_from_slice(&output_icc.unwrap())
                    .unwrap()
                    .cicp
                    .unwrap();
                assert_eq!(
                    (
                        cicp.color_primaries as u16,
                        cicp.transfer_characteristics as u16
                    ),
                    (primaries, transfer)
                );
            } else {
                assert_eq!(decoder.dimensions(), (10, 8));
                assert!(output_icc.is_none());
            }
            assert_eq!(fs::read(&source).unwrap(), bytes);
        }
    }
}

#[test]
fn worker_rejects_invalid_arguments_formats_and_oversized_input() {
    let fixture = tempfile::tempdir().unwrap();
    let destination = fixture.path().join("output.jpeg");
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_orbit"))
            .arg("--convert-image")
            .output()
            .unwrap()
            .status
            .success()
    );
    for (extension, conversion) in [
        ("heic", "HeicToJpeg"),
        ("avif", "AvifToJpeg"),
        ("avif", "AvifToPng"),
        ("png", "PngToJpeg"),
        ("jpeg", "JpegToPng"),
        ("webp", "WebpToJpeg"),
    ] {
        let source = fixture.path().join(format!("invalid.{extension}"));
        fs::write(&source, b"not an image").unwrap();
        assert!(!run(conversion, &source, &destination).status.success());
        assert!(!destination.exists());
        assert_eq!(fs::read(source).unwrap(), b"not an image");
    }
    let source = fixture.path().join("wrong.webp");
    image::RgbImage::from_pixel(1, 1, image::Rgb([60, 110, 160]))
        .save_with_format(&source, ImageFormat::Png)
        .unwrap();
    let original = fs::read(&source).unwrap();
    assert!(!run("WebpToJpeg", &source, &destination).status.success());
    assert!(!run("Unknown", &source, &destination).status.success());
    assert_eq!(fs::read(&source).unwrap(), original);
    for extension in ["heic", "avif"] {
        let source = fixture.path().join(format!("oversized.{extension}"));
        let size = 128 * 1024 * 1024 + 1;
        fs::File::create(&source).unwrap().set_len(size).unwrap();
        let conversion = if extension == "heic" {
            "HeicToJpeg"
        } else {
            "AvifToPng"
        };
        let result = run(conversion, &source, &destination);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("Image input is too large"));
        assert!(!destination.exists());
        assert_eq!(fs::metadata(source).unwrap().len(), size);
    }
}
