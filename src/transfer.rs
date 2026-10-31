// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::clipboard::mime::{AllowedMimeTypes, AsMimeTypes};
use std::{borrow::Cow, path::PathBuf, sync::Arc};
use url::Url;

pub const URI_LIST: &str = "text/uri-list";
pub const GNOME_FILES: &str = "x-special/gnome-copied-files";
pub const TEXT: &str = "text/plain";
pub const UTF8_TEXT: &str = "text/plain;charset=utf-8";
pub const ORBIT_TEXT: &str = "application/x-orbit-text";
pub const MOZ_URL: &str = "text/x-moz-url";

#[derive(Clone, Debug)]
pub struct ClipboardData(pub Vec<u8>, pub String);

impl AllowedMimeTypes for ClipboardData {
    fn allowed() -> Cow<'static, [String]> {
        let mut mimes = vec![ORBIT_TEXT.into(), URI_LIST.into(), GNOME_FILES.into()];
        mimes.extend(
            crate::import::MIME_TYPES
                .iter()
                .filter(|mime| mime.starts_with("image/"))
                .map(|mime| (*mime).into()),
        );
        mimes.extend([UTF8_TEXT.into(), TEXT.into()]);
        for mime in crate::import::MIME_TYPES {
            if !mimes.iter().any(|allowed| allowed == mime) {
                mimes.push((*mime).into());
            }
        }
        Cow::Owned(mimes)
    }
}

impl From<(Vec<u8>, String)> for ClipboardData {
    fn from((bytes, mime): (Vec<u8>, String)) -> Self {
        Self(bytes, mime)
    }
}

pub fn is_link(text: &str) -> bool {
    !text.trim().contains(['\n', '\r'])
        && Url::parse(text.trim())
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "mailto" | "tel" | "ftp"))
}

#[derive(Clone, Debug)]
pub struct FileList(pub Vec<PathBuf>);

impl AllowedMimeTypes for FileList {
    fn allowed() -> Cow<'static, [String]> {
        Cow::Owned(vec![URI_LIST.into(), GNOME_FILES.into()])
    }
}

impl TryFrom<(Vec<u8>, String)> for FileList {
    type Error = &'static str;

    fn try_from((bytes, mime): (Vec<u8>, String)) -> Result<Self, Self::Error> {
        if bytes.len() > 1024 * 1024 {
            return Err("File list exceeds 1 MiB");
        }
        let data = std::str::from_utf8(&bytes).map_err(|_| "Invalid UTF-8 file list")?;
        let mut lines = data.lines();
        match mime.as_str() {
            URI_LIST => (),
            GNOME_FILES => {
                // Receiving a cut list only collects references; never remove the originals.
                if !matches!(lines.next(), Some("copy" | "cut")) {
                    return Err("Invalid clipboard operation");
                }
            }
            _ => return Err("Unsupported file list type"),
        }
        let mut paths = Vec::new();
        for line in lines
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let url = Url::parse(line).map_err(|_| "Invalid file URI")?;
            if url.query().is_some() || url.fragment().is_some() {
                return Err("Unexpected file URI query or fragment");
            }
            paths.push(
                url.to_file_path()
                    .map_err(|_| "Only local file URIs are supported")?,
            );
        }
        if paths.is_empty() {
            return Err("Empty file list");
        }
        Ok(Self(paths))
    }
}

impl AsMimeTypes for FileList {
    fn available(&self) -> Cow<'static, [String]> {
        Self::allowed()
    }

    fn as_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        let mut data = match mime {
            URI_LIST => String::new(),
            GNOME_FILES => String::from("copy\n"),
            _ => return None,
        };
        for path in &self.0 {
            data.push_str(Url::from_file_path(path).ok()?.as_str());
            data.push_str("\r\n");
        }
        Some(Cow::Owned(data.into_bytes()))
    }
}

#[derive(Clone, Debug)]
pub struct Transfer {
    pub files: FileList,
    pub text: Option<String>,
    pub text_only: bool,
    pub links: bool,
    pub clipboard: bool,
    pub cached: Vec<Arc<crate::cache::Directory>>,
}

impl Transfer {
    fn file_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        let bytes = self.files.as_bytes(mime)?;
        for directory in &self.cached {
            directory.hand_off();
        }
        Some(bytes)
    }
}

impl AsMimeTypes for Transfer {
    fn available(&self) -> Cow<'static, [String]> {
        let mut mimes = Vec::new();
        if self.text_only {
            mimes.extend([ORBIT_TEXT.into(), UTF8_TEXT.into(), TEXT.into()]);
        }
        if self.links && self.files.0.len() == 1 {
            mimes.push(MOZ_URL.into());
        }
        mimes.push(URI_LIST.into());
        // Keep the clipboard's copy operation out of native Move drags.
        if self.clipboard && !self.links {
            mimes.push(GNOME_FILES.into());
        }
        if self.text.is_some() && !self.text_only {
            mimes.extend([UTF8_TEXT.into(), TEXT.into()]);
        }
        Cow::Owned(mimes)
    }

    fn as_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        match mime {
            ORBIT_TEXT if self.text_only => self
                .text
                .as_ref()
                .map(|text| Cow::Owned(text.as_bytes().to_vec())),
            TEXT | UTF8_TEXT => self
                .text
                .as_ref()
                .map(|text| Cow::Owned(text.as_bytes().to_vec())),
            MOZ_URL if self.links && self.files.0.len() == 1 => self.text.as_ref().map(|text| {
                let url = text.trim();
                Cow::Owned(
                    format!("{url}\n{url}")
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect(),
                )
            }),
            URI_LIST if self.links && self.clipboard => self.text.as_ref().map(|text| {
                Cow::Owned(
                    text.lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>()
                        .join("\r\n")
                        .into_bytes(),
                )
            }),
            URI_LIST => self.file_bytes(mime),
            GNOME_FILES if self.clipboard && !self.links => self.file_bytes(mime),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_drags_keep_utf8_and_offer_a_file_fallback() {
        let data = Transfer {
            files: FileList(vec![PathBuf::from("/tmp/Text.txt")]),
            text: Some("First line\nسلام\nLast line".into()),
            text_only: true,
            links: false,
            clipboard: false,
            cached: Vec::new(),
        };
        assert_eq!(
            data.as_bytes(TEXT).unwrap().as_ref(),
            "First line\nسلام\nLast line".as_bytes()
        );
        assert!(!data.available().iter().any(|mime| mime == GNOME_FILES));
        assert_eq!(
            FileList::try_from((
                data.as_bytes(URI_LIST).unwrap().into_owned(),
                URI_LIST.into()
            ))
            .unwrap()
            .0,
            data.files.0
        );
    }

    #[test]
    fn link_drags_offer_local_files_and_a_native_browser_url() {
        let data = Transfer {
            files: FileList(vec![PathBuf::from("/tmp/Link.html")]),
            text: Some("https://example.com/article".into()),
            text_only: true,
            links: true,
            clipboard: false,
            cached: Vec::new(),
        };
        assert_eq!(
            FileList::try_from((
                data.as_bytes(URI_LIST).unwrap().into_owned(),
                URI_LIST.into()
            ))
            .unwrap()
            .0,
            data.files.0
        );
        let bytes = data.as_bytes(MOZ_URL).unwrap();
        let units = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            "https://example.com/article\nhttps://example.com/article"
        );
    }

    #[test]
    fn uri_roundtrip_preserves_spaces_unicode_and_newlines() {
        let files = FileList(vec![
            PathBuf::from("/tmp/a b #1.txt"),
            PathBuf::from("/tmp/عکس\n.png"),
        ]);
        for mime in [URI_LIST, GNOME_FILES] {
            let encoded = files.as_bytes(mime).unwrap().into_owned();
            assert_eq!(
                FileList::try_from((encoded, mime.into())).unwrap().0,
                files.0
            );
        }
    }

    #[test]
    fn supports_comments_crlf_and_localhost() {
        let data = b"# a comment\r\n\r\nfile://localhost/tmp/test%20file\r\n".to_vec();
        assert_eq!(
            FileList::try_from((data, URI_LIST.into())).unwrap().0,
            [PathBuf::from("/tmp/test file")]
        );
    }

    #[test]
    fn rejects_remote_urls_malformed_lists_and_oversized_payloads() {
        for data in [
            "https://example.com/file",
            "file://remote/tmp/file",
            "not a URI",
            "# comment",
            "file:///tmp/a?query",
            "file:///tmp/a#fragment",
        ] {
            assert!(
                FileList::try_from((data.as_bytes().into(), URI_LIST.into())).is_err(),
                "{data}"
            );
        }
        assert!(FileList::try_from((vec![0xff], URI_LIST.into())).is_err());
        assert!(FileList::try_from((vec![b'a'; 1024 * 1024 + 1], URI_LIST.into())).is_err());
        assert!(FileList::try_from((b"file:///tmp/a".to_vec(), "text/plain".into())).is_err());
    }

    #[test]
    fn cut_clipboard_is_always_reexported_as_copy() {
        let files =
            FileList::try_from((b"cut\nfile:///tmp/a\n".to_vec(), GNOME_FILES.into())).unwrap();
        assert!(files.as_bytes(GNOME_FILES).unwrap().starts_with(b"copy\n"));
    }

    #[test]
    fn uri_roundtrip_preserves_non_utf8_unix_paths() {
        use std::os::unix::ffi::OsStringExt;
        let files = FileList(vec![PathBuf::from(std::ffi::OsString::from_vec(
            b"/tmp/file\xff".to_vec(),
        ))]);
        let encoded = files.as_bytes(URI_LIST).unwrap().into_owned();
        assert_eq!(
            FileList::try_from((encoded, URI_LIST.into())).unwrap().0,
            files.0
        );
    }
}
