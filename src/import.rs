// SPDX-License-Identifier: GPL-3.0-only

use crate::transfer::{
    FileList, GNOME_FILES, ORBIT_TEXT, TEXT, URI_LIST, UTF8_TEXT, decode_utf16, is_link,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    error::Error,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use url::Url;

pub const MAX_IMAGE_SIZE: usize = 50 * 1024 * 1024;
const MAX_TEXT_SIZE: usize = 1024 * 1024;

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
    "text/html;charset=utf-8",
    "text/html",
    ORBIT_TEXT,
    URI_LIST,
    GNOME_FILES,
    "application/x-moz-file-promise-url",
    "text/x-moz-url-data",
    "text/x-moz-url",
    UTF8_TEXT,
    TEXT,
];

#[derive(Clone, Copy)]
pub enum Source {
    Drop,
    Clipboard,
}

#[derive(Clone, Debug)]
pub struct ImportedFiles {
    pub files: FileList,
    pub directory: Option<Arc<crate::cache::Directory>>,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub name: Option<String>,
    pub status: Option<String>,
    pub received: u64,
    pub total: Option<u64>,
}

pub async fn receive(
    bytes: Vec<u8>,
    mime: String,
    source: Source,
    progress: impl FnMut(Progress),
) -> Result<ImportedFiles, Box<dyn Error + Send + Sync>> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_SIZE {
        return Err("Invalid item size".into());
    }
    if mime.starts_with("image/") {
        return save_image(&bytes, None);
    }
    if matches!(mime.as_str(), URI_LIST | GNOME_FILES)
        && let Ok(files) = FileList::try_from((bytes.clone(), mime.clone()))
    {
        return Ok(ImportedFiles {
            files,
            directory: None,
            text: None,
        });
    }
    if mime == GNOME_FILES {
        return Err("Invalid file list".into());
    }
    let text_limit = if mime.starts_with("text/html") {
        crate::rich_text::MAX_HTML
    } else {
        MAX_TEXT_SIZE
    };
    if bytes.len() > text_limit {
        return Err("Text is too large".into());
    }

    let text = if matches!(
        mime.as_str(),
        "text/x-moz-url" | "text/x-moz-url-data" | "application/x-moz-file-promise-url"
    ) && (bytes.contains(&0) || bytes.starts_with(&[0xfe, 0xff]))
    {
        decode_utf16(&bytes, bytes.starts_with(&[0xfe, 0xff])).ok_or("Invalid browser URL")?
    } else {
        String::from_utf8(bytes)?
    };
    if matches!(mime.as_str(), TEXT | UTF8_TEXT | ORBIT_TEXT) {
        return save_text(text);
    }
    let (text, image) = if mime.starts_with("text/html") {
        let fragment = browser_fragment(&text)?;
        if fragment.text.trim().is_empty()
            && let Some(image) = fragment.image
        {
            (image, true)
        } else if !fragment.outside_link
            && !fragment.structured
            && let Some(link) = fragment.link
        {
            return save_text(link);
        } else {
            let document = crate::rich_text::Document::from_html(&text)?;
            if !fragment.structured && is_link(&document.text) {
                return save_text(document.text);
            }
            let html = format!("<!DOCTYPE html><meta charset=\"utf-8\">{}", document.html());
            return save_snippet(document.text, "html", html);
        }
    } else {
        (text, false)
    };
    let mut lines = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let address = lines.next().ok_or("Missing URL")?;
    let address = if mime == "downloadurl" {
        address
            .splitn(3, ':')
            .nth(2)
            .ok_or("Invalid browser download URL")?
    } else {
        address
    };
    if mime == URI_LIST && lines.next().is_some() {
        return save_text(text.trim().to_owned());
    }
    let mut url = Url::parse(address)?;
    let mut image = image
        || (mime == "downloadurl" && text.starts_with("image/"))
        || mime == "application/x-moz-file-promise-url";
    if url.path() == "/imgres"
        && let Some((_, address)) = url.query_pairs().find(|(key, _)| key == "imgurl")
    {
        url = Url::parse(&address)?;
        image |= matches!(source, Source::Drop);
    }
    if matches!(source, Source::Drop) {
        image |= url.as_str().starts_with("data:image/");
        image |= url.path().rsplit('.').next().is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "avif" | "svg" | "bmp" | "tiff"
            )
        });
    }
    if !image {
        return save_text(address.to_owned());
    }
    match url.scheme() {
        "http" | "https" => download(url, progress).await,
        "data" => {
            let (metadata, encoded) = url
                .as_str()
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

#[derive(Default)]
struct Fragment {
    image: Option<String>,
    link: Option<String>,
    text: String,
    outside_link: bool,
    structured: bool,
}

fn browser_fragment(html: &str) -> Result<Fragment, Box<dyn Error + Send + Sync>> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(html.trim_start_matches('\u{feff}'));
    // Browser fragments contain HTML void elements without XML closing tags.
    reader.config_mut().check_end_names = false;
    let mut fragment = Fragment::default();
    let mut in_link = false;
    let mut links = 0;
    let mut hidden = false;
    loop {
        match reader.read_event()? {
            Event::Start(tag) | Event::Empty(tag) => {
                let name = String::from_utf8_lossy(tag.name().as_ref()).to_ascii_lowercase();
                if matches!(name.as_str(), "script" | "style") {
                    hidden = true;
                }
                if hidden {
                    continue;
                }
                fragment.structured |= matches!(name.as_str(), "ul" | "ol" | "table");
                if matches!(name.as_str(), "br" | "p" | "div" | "li")
                    && !fragment.text.ends_with('\n')
                    && !fragment.text.is_empty()
                {
                    fragment.text.push('\n');
                }
                if name == "a" {
                    in_link = true;
                    links += 1;
                }
                for attribute in tag.html_attributes() {
                    let attribute = attribute?;
                    if (name == "img"
                        && attribute.key.as_ref().eq_ignore_ascii_case(b"src")
                        && fragment.image.is_none())
                        || (name == "a" && attribute.key.as_ref().eq_ignore_ascii_case(b"href"))
                    {
                        let value = std::str::from_utf8(attribute.value.as_ref())?;
                        let value = quick_xml::escape::unescape(value)?.into_owned();
                        if name == "img" {
                            fragment.image = Some(value);
                        } else {
                            fragment.link = Some(value);
                        }
                    }
                }
            }
            Event::End(tag) => {
                let name = String::from_utf8_lossy(tag.name().as_ref()).to_ascii_lowercase();
                if matches!(name.as_str(), "script" | "style") {
                    hidden = false;
                }
                if name == "a" {
                    in_link = false;
                }
                if !hidden
                    && matches!(name.as_str(), "p" | "div" | "li")
                    && !fragment.text.ends_with('\n')
                {
                    fragment.text.push('\n');
                }
            }
            Event::Text(text) if !hidden => {
                let text = text.html_content()?;
                fragment.outside_link |= !in_link && !text.trim().is_empty();
                fragment.text.push_str(&text);
            }
            Event::CData(text) if !hidden => {
                let text = text.html_content()?;
                fragment.outside_link |= !in_link && !text.trim().is_empty();
                fragment.text.push_str(&text);
            }
            Event::GeneralRef(reference) if !hidden => {
                let value = format!("&{};", reference.decode()?);
                let value = quick_xml::escape::unescape(&value)?;
                fragment.outside_link |= !in_link && !value.trim().is_empty();
                fragment.text.push_str(&value);
            }
            Event::Eof => {
                fragment.outside_link |= links != 1;
                return Ok(fragment);
            }
            _ => (),
        }
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
    eprintln!("Orbit import failed:\n{detail}");
    if let Ok(cache) = crate::cache::root() {
        let _ = std::fs::create_dir_all(&cache);
        let _ = std::fs::write(cache.join("last-error.log"), detail);
    }
}

fn save_text(text: String) -> Result<ImportedFiles, Box<dyn Error + Send + Sync>> {
    let (extension, contents) = if is_link(&text) {
        let url = Url::parse(text.trim())?;
        let url = quick_xml::escape::escape(url.as_str());
        (
            "html",
            format!(
                "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"/><meta http-equiv=\"refresh\" content=\"0;url={url}\"/></head></html>\n"
            ),
        )
    } else {
        ("txt", text.clone())
    };
    save_snippet(text, extension, contents)
}

fn save_snippet(
    text: String,
    extension: &str,
    contents: String,
) -> Result<ImportedFiles, Box<dyn Error + Send + Sync>> {
    if text.len() > MAX_TEXT_SIZE
        || contents.len() > crate::rich_text::MAX_HTML
        || text.trim().is_empty()
        || text.contains('\0')
    {
        return Err("Invalid text".into());
    }
    let directory = crate::cache::Directory::new("orbit-text-")?;
    // File managers need a local file fallback for collected text.
    let path = directory
        .path()
        .join(format!("{}.{extension}", uuid::Uuid::new_v4()));
    std::fs::write(&path, contents.as_bytes())?;
    Ok(ImportedFiles {
        files: FileList(vec![path]),
        directory: Some(directory),
        text: Some(text),
    })
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
    let directory = crate::cache::Directory::new("orbit-image-")?;
    let path = directory.path().join(format!("{stem}.{extension}"));
    std::fs::write(&path, bytes)?;
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
    fn saved_link_redirects_to_the_original_url_without_injecting_markup() {
        let text = "https://example.com/?q=\"'><img src=x>&next=1#section";
        let imported = save_text(text.into()).unwrap();
        let path = &imported.files.0[0];
        let html = std::fs::read_to_string(path).unwrap();

        assert_eq!(path.extension().unwrap(), "html");
        assert_eq!(imported.text.as_deref(), Some(text));
        let document =
            roxmltree::Document::parse(html.strip_prefix("<!DOCTYPE html>\n").unwrap()).unwrap();
        let refresh = document
            .descendants()
            .find(|node| node.attribute("http-equiv") == Some("refresh"))
            .unwrap();
        assert_eq!(
            refresh.attribute("content").unwrap(),
            format!("0;url={}", Url::parse(text).unwrap())
        );
        assert!(
            document
                .descendants()
                .filter(|node| node.is_element())
                .all(|node| matches!(node.tag_name().name(), "html" | "head" | "meta"))
        );
    }

    #[test]
    fn browser_fragments_preserve_selected_text_and_linked_images() {
        let fragment =
            browser_fragment("<p>Hello <b>world</b> &amp; سلام</p><p>Next<br>line</p>").unwrap();
        assert_eq!(fragment.text.trim(), "Hello world & سلام\nNext\nline");
        assert!(fragment.outside_link);

        let fragment = browser_fragment(r#"<a href="https://example.com/article"><img src="https://example.com/photo.png?a=1&amp;b=2"></a>"#).unwrap();
        assert_eq!(
            fragment.image.as_deref(),
            Some("https://example.com/photo.png?a=1&b=2")
        );
        assert!(fragment.text.is_empty());

        let fragment =
            browser_fragment(r#"<a href="https://example.com/article">Article</a>"#).unwrap();
        assert_eq!(
            fragment.link.as_deref(),
            Some("https://example.com/article")
        );
        assert!(!fragment.outside_link);

        let fragment = browser_fragment("<table><tr><td><a href='https://example.com'>https://example.com</a></td></tr></table>").unwrap();
        assert!(fragment.structured);
    }
}
