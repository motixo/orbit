// SPDX-License-Identifier: GPL-3.0-only

use crate::transfer::{FileList, URI_LIST};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    error::Error,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use url::Url;

const MAX_IMAGE_SIZE: usize = 50 * 1024 * 1024;

// Prefer bytes from the browser: they also work for images that require its session.
pub const MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/avif",
    "image/svg+xml",
    "image/bmp",
    "image/tiff",
    "downloadurl",
    URI_LIST,
    "application/x-moz-file-promise-url",
    "text/x-moz-url-data",
    "text/x-moz-url",
    "text/plain;charset=utf-8",
    "text/plain",
];

#[derive(Clone, Debug)]
pub struct ImportedFiles {
    pub files: FileList,
    pub directory: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub name: Option<String>,
    pub received: u64,
    pub total: Option<u64>,
}

pub async fn receive(
    bytes: Vec<u8>,
    mime: String,
    progress: impl FnMut(Progress),
) -> Result<ImportedFiles, Box<dyn Error + Send + Sync>> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_SIZE {
        return Err("Invalid image size".into());
    }
    if mime.starts_with("image/") {
        return save_image(&bytes, None);
    }
    if mime == URI_LIST
        && let Ok(files) = FileList::try_from((bytes.clone(), mime.clone()))
    {
        return Ok(ImportedFiles {
            files,
            directory: None,
        });
    }

    let text = if matches!(
        mime.as_str(),
        "text/x-moz-url" | "text/x-moz-url-data" | "application/x-moz-file-promise-url"
    ) && (bytes.contains(&0) || bytes.starts_with(&[0xfe, 0xff]))
    {
        if bytes.len() % 2 != 0 {
            return Err("Invalid browser URL".into());
        }
        let big_endian = bytes.starts_with(&[0xfe, 0xff]);
        let units = bytes
            .chunks_exact(2)
            .map(|pair| {
                if big_endian {
                    u16::from_be_bytes([pair[0], pair[1]])
                } else {
                    u16::from_le_bytes([pair[0], pair[1]])
                }
            })
            .collect::<Vec<_>>();
        String::from_utf16(&units)?
    } else {
        String::from_utf8(bytes)?
    };
    let mut lines = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let address = lines.next().ok_or("Missing image URL")?;
    let address = if mime == "downloadurl" {
        address
            .splitn(3, ':')
            .nth(2)
            .ok_or("Invalid browser download URL")?
    } else {
        address
    };
    if mime == URI_LIST && lines.next().is_some() {
        return Err("Expected one image URL".into());
    }
    let url = Url::parse(address)?;
    match url.scheme() {
        "http" | "https" => download(url, progress).await,
        "data" => {
            let (metadata, encoded) = address
                .strip_prefix("data:")
                .and_then(|data| data.split_once(','))
                .ok_or("Invalid image URL")?;
            if !metadata.starts_with("image/") {
                return Err("Unsupported image URL".into());
            }
            let bytes = percent_encoding::percent_decode_str(encoded).collect::<Vec<_>>();
            let bytes = if metadata.ends_with(";base64") {
                STANDARD.decode(bytes)?
            } else {
                bytes
            };
            save_image(&bytes, None)
        }
        _ => Err("Unsupported image URL".into()),
    }
}

async fn download(
    url: Url,
    mut progress: impl FnMut(Progress),
) -> Result<ImportedFiles, Box<dyn Error + Send + Sync>> {
    let mut state = Progress {
        name: image_name(&url),
        ..Default::default()
    };
    progress(state.clone());
    let client = reqwest::Client::builder()
        .user_agent(concat!("Orbit/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_IMAGE_SIZE as u64)
    {
        return Err("Image exceeds 50 MiB".into());
    }
    state.name = image_name(response.url()).or(state.name);
    state.total = response.content_length().filter(|length| *length > 0);
    progress(state.clone());
    let mut bytes = Vec::new();
    let mut last_update = Instant::now();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_IMAGE_SIZE - bytes.len() {
            return Err("Image exceeds 50 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
        state.received = bytes.len() as u64;
        if last_update.elapsed() >= Duration::from_millis(80) {
            progress(state.clone());
            last_update = Instant::now();
        }
    }
    progress(state.clone());
    save_image(&bytes, state.name.as_deref())
}

fn image_name(url: &Url) -> Option<String> {
    url.path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| !name.is_empty())
        .map(|name| {
            percent_encoding::percent_decode_str(name)
                .decode_utf8_lossy()
                .into_owned()
        })
}

pub fn record_error(mime: &str, size: usize, error: Box<dyn Error + Send + Sync>) {
    // Keep diagnostics local and omit URLs, query tokens and dropped image data.
    let error: Box<dyn Error + Send + Sync> = match error.downcast::<reqwest::Error>() {
        Ok(error) => Box::new(error.without_url()),
        Err(error) => error,
    };
    let mut detail = format!("MIME: {mime:?}\nBytes: {size}\n");
    let mut cause: Option<&dyn Error> = Some(error.as_ref());
    while let Some(error) = cause {
        detail.push_str(&format!("{error}\n"));
        cause = error.source();
    }
    eprintln!("Orbit image import failed:\n{detail}");
    if let Ok(cache) = image_cache() {
        let _ = std::fs::create_dir_all(&cache);
        let _ = std::fs::write(cache.join("last-error.log"), detail);
    }
}

fn image_cache() -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    Ok(std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or("Missing cache directory")?
        .join("io.github.motixo.Orbit/images"))
}

fn save_image(
    bytes: &[u8],
    name: Option<&str>,
) -> Result<ImportedFiles, Box<dyn Error + Send + Sync>> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_SIZE {
        return Err("Invalid image size".into());
    }
    let extension = if let Ok(format) = image::guess_format(bytes) {
        format.extensions_str()[0]
    } else {
        let svg = roxmltree::Document::parse_with_options(
            std::str::from_utf8(bytes)?,
            roxmltree::ParsingOptions {
                nodes_limit: 100_000,
                ..Default::default()
            },
        )?;
        if svg.root_element().tag_name().name() != "svg" {
            return Err("Not an image".into());
        }
        "svg"
    };
    let stem = name
        .and_then(|name| Path::new(name).file_stem())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 120)
        .unwrap_or("Image");
    let cache = image_cache()?;
    std::fs::create_dir_all(&cache)?;
    let directory = tempfile::Builder::new()
        .prefix("orbit-image-")
        .tempdir_in(cache)?;
    let path = directory.path().join(format!("{stem}.{extension}"));
    std::fs::write(&path, bytes)?;
    Ok(ImportedFiles {
        files: FileList(vec![path]),
        directory: Some(directory.keep()),
    })
}
