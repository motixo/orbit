// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::clipboard::mime::{AllowedMimeTypes, AsMimeTypes};
use std::{borrow::Cow, path::PathBuf};
use url::Url;

pub const URI_LIST: &str = "text/uri-list";
const GNOME_FILES: &str = "x-special/gnome-copied-files";

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

pub struct DragFiles(pub FileList);

impl AsMimeTypes for DragFiles {
    fn available(&self) -> Cow<'static, [String]> {
        // The drag action carries Move; keep the clipboard's copy format out of the offer.
        Cow::Owned(vec![URI_LIST.into()])
    }

    fn as_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        if mime == URI_LIST {
            self.0.as_bytes(mime)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
