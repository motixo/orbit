// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::clipboard::{
    dnd::DndAction,
    mime::{AllowedMimeTypes, AsMimeTypes},
};
use std::{
    borrow::Cow,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use url::Url;

pub const URI_LIST: &str = "text/uri-list";
pub const GNOME_FILES: &str = "x-special/gnome-copied-files";
pub const TEXT: &str = "text/plain";
pub const UTF8_TEXT: &str = "text/plain;charset=utf-8";
const UTF8_STRING: &str = "UTF8_STRING";
pub const ORBIT_TEXT: &str = "application/x-orbit-text";
pub const MOZ_URL: &str = "text/x-moz-url";
// shortcut: text-file drags are capped at 1 MiB; stream text if larger snippets are needed.
const MAX_TEXT_FILE_SIZE: u64 = 1024 * 1024;

pub fn decode_utf16(bytes: &[u8], big_endian: bool) -> Option<String> {
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let units = pairs
        .iter()
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes(*pair)
            } else {
                u16::from_le_bytes(*pair)
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).ok()
}

pub fn is_text_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
}

#[derive(Clone, Debug)]
pub enum Text {
    Inline(String),
    File(PathBuf),
}

impl Text {
    pub(crate) fn contents(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Inline(text) => Some(Cow::Borrowed(text)),
            Self::File(path) => {
                let mut bytes = Vec::new();
                std::fs::File::open(path)
                    .ok()?
                    .take(MAX_TEXT_FILE_SIZE + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                if bytes.len() as u64 > MAX_TEXT_FILE_SIZE {
                    return None;
                }
                let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
                    decode_utf16(&bytes[2..], bytes[0] == 0xfe)?
                } else {
                    String::from_utf8(bytes)
                        .ok()?
                        .trim_start_matches('\u{feff}')
                        .to_owned()
                };
                (!text.contains('\0')).then_some(Cow::Owned(text))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClipboardData(pub Vec<u8>, pub String);

impl AllowedMimeTypes for ClipboardData {
    fn allowed() -> Cow<'static, [String]> {
        let mut mimes = Vec::new();
        mimes.extend(
            crate::import::MIME_TYPES
                .iter()
                .filter(|mime| mime.starts_with("image/"))
                .map(|mime| (*mime).into()),
        );
        mimes.extend([
            crate::rich_text::HTML_UTF8.into(),
            crate::rich_text::HTML.into(),
            ORBIT_TEXT.into(),
            URI_LIST.into(),
            GNOME_FILES.into(),
            UTF8_TEXT.into(),
            TEXT.into(),
        ]);
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
        let mut mimes = Self::allowed().into_owned();
        if self.path_text().is_some() {
            mimes.extend([TEXT.into(), UTF8_TEXT.into(), UTF8_STRING.into()]);
        }
        Cow::Owned(mimes)
    }

    fn as_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        if matches!(mime, TEXT | UTF8_TEXT | UTF8_STRING) {
            return self.path_text().map(|text| Cow::Owned(text.into_bytes()));
        }
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

impl FileList {
    fn path_text(&self) -> Option<String> {
        self.0
            .iter()
            .map(|path| path.to_str().filter(|path| !path.contains(['\r', '\n'])))
            .collect::<Option<Vec<_>>>()
            .map(|paths| paths.join("\r\n"))
    }
}

#[derive(Clone, Debug)]
pub struct Transfer {
    pub files: FileList,
    pub text: Option<Vec<Text>>,
    pub text_only: bool,
    pub has_local_files: bool,
    pub links: bool,
    pub clipboard: bool,
    pub cached: Vec<Arc<crate::cache::Directory>>,
}

impl Transfer {
    pub fn drag_actions(&self, copy_mode: bool) -> DndAction {
        if copy_mode && self.has_local_files {
            DndAction::Copy
        } else {
            // Attachment targets accept Copy; file managers can still select Move.
            DndAction::Copy | DndAction::Move
        }
    }

    fn plain_text(&self) -> Option<String> {
        self.text
            .as_ref()?
            .iter()
            .map(Text::contents)
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join("\n\n"))
    }

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
        self.available_for(false)
    }

    fn as_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        self.bytes_for(mime, false)
    }
}

pub struct Drag {
    pub transfer: Arc<Transfer>,
    pub content_only: bool,
}

impl AsMimeTypes for Drag {
    fn available(&self) -> Cow<'static, [String]> {
        self.transfer.available_for(self.content_only)
    }

    fn as_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        self.transfer.bytes_for(mime, self.content_only)
    }
}

impl Transfer {
    fn available_for(&self, content_only: bool) -> Cow<'static, [String]> {
        let mut mimes = Vec::new();
        if self.text_only {
            if !self.links && (content_only || self.clipboard) {
                mimes.extend([
                    crate::rich_text::HTML_UTF8.into(),
                    crate::rich_text::HTML.into(),
                ]);
            }
            mimes.extend([ORBIT_TEXT.into(), UTF8_TEXT.into(), TEXT.into()]);
        }
        if self.links && self.files.0.len() == 1 {
            mimes.push(MOZ_URL.into());
        }
        if !content_only {
            mimes.push(URI_LIST.into());
            if self.text.is_none() && self.files.path_text().is_some() {
                mimes.extend([TEXT.into(), UTF8_TEXT.into(), UTF8_STRING.into()]);
            }
        }
        // File managers get local files without making browser inputs prefer file URIs.
        if content_only || self.clipboard && !self.links {
            mimes.push(GNOME_FILES.into());
        }
        if self.text.is_some() && !self.text_only {
            mimes.extend([UTF8_TEXT.into(), TEXT.into()]);
        }
        Cow::Owned(mimes)
    }

    fn bytes_for(&self, mime: &str, content_only: bool) -> Option<Cow<'static, [u8]>> {
        match mime {
            crate::rich_text::HTML | crate::rich_text::HTML_UTF8
                if self.text_only && (content_only || self.clipboard) =>
            {
                let text = self.text.as_ref()?;
                let documents = self
                    .files
                    .0
                    .iter()
                    .zip(text)
                    .map(|(path, text)| {
                        let text = text.contents()?;
                        Some(
                            crate::rich_text::Document::read(path, &text)
                                .unwrap_or_else(|| crate::rich_text::Document::plain(&text)),
                        )
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(Cow::Owned(
                    crate::rich_text::Document::join(documents)
                        .clipboard_html()
                        .into_bytes(),
                ))
            }
            ORBIT_TEXT if self.text_only => {
                self.plain_text().map(|text| Cow::Owned(text.into_bytes()))
            }
            TEXT | UTF8_TEXT | UTF8_STRING => self
                .plain_text()
                .map(|text| Cow::Owned(text.into_bytes()))
                .or_else(|| (!content_only).then(|| self.file_bytes(mime)).flatten()),
            MOZ_URL if self.links && self.files.0.len() == 1 => self.plain_text().map(|text| {
                let url = text.trim();
                Cow::Owned(
                    format!("{url}\n{url}")
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect(),
                )
            }),
            URI_LIST if self.links && self.clipboard => self.plain_text().map(|text| {
                Cow::Owned(
                    text.lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>()
                        .join("\r\n")
                        .into_bytes(),
                )
            }),
            URI_LIST if !content_only => self.file_bytes(mime),
            GNOME_FILES if content_only || self.clipboard && !self.links => self.file_bytes(mime),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_drags_allow_copy_only_targets_and_keep_copy_mode() {
        let data = Transfer {
            files: FileList(vec![PathBuf::from("/tmp/Photo.png")]),
            text: None,
            text_only: false,
            has_local_files: true,
            links: false,
            clipboard: false,
            cached: Vec::new(),
        };
        let actions = data.drag_actions(false);
        assert!(actions.contains(DndAction::Copy));
        assert!(actions.contains(DndAction::Move));
        assert_eq!(data.drag_actions(true), DndAction::Copy);
        assert!(data.available().iter().any(|mime| mime == URI_LIST));
        let cached = Transfer {
            has_local_files: false,
            ..data
        };
        assert_eq!(cached.drag_actions(true), DndAction::Copy | DndAction::Move);
    }

    #[test]
    fn file_drags_offer_native_path_text_alongside_file_uris() {
        let data = Arc::new(Transfer {
            files: FileList(vec![
                "/tmp/عکس من.png".into(),
                "/tmp/second image.png".into(),
            ]),
            text: None,
            text_only: false,
            has_local_files: true,
            links: false,
            clipboard: false,
            cached: Vec::new(),
        });
        let drag = Drag {
            transfer: data.clone(),
            content_only: false,
        };
        for mime in [TEXT, UTF8_TEXT, UTF8_STRING] {
            assert!(drag.available().iter().any(|offered| offered == mime));
            assert_eq!(
                drag.as_bytes(mime).unwrap().as_ref(),
                "/tmp/عکس من.png\r\n/tmp/second image.png".as_bytes()
            );
        }
        let files = FileList::try_from((
            drag.as_bytes(URI_LIST).unwrap().into_owned(),
            URI_LIST.into(),
        ))
        .unwrap();
        assert_eq!(files.0, data.files.0);
    }

    #[test]
    fn content_drags_keep_paragraphs_styles_and_item_boundaries() {
        use crate::rich_text::{Document, HTML};

        let directory = tempfile::tempdir().unwrap();
        let styled_path = directory.path().join("styled.html");
        let text_path = directory.path().join("plain.txt");
        let styled = Document::from_html("<b>First</b><br><br><i>Second</i><br>").unwrap();
        std::fs::write(&styled_path, styled.html()).unwrap();
        std::fs::write(&text_path, "Third\nFourth").unwrap();
        let data = Arc::new(Transfer {
            files: FileList(vec![styled_path, text_path.clone()]),
            text: Some(vec![Text::Inline(styled.text), Text::File(text_path)]),
            text_only: true,
            has_local_files: true,
            links: false,
            clipboard: false,
            cached: Vec::new(),
        });
        let file_drag = Drag {
            transfer: data.clone(),
            content_only: false,
        };
        assert!(!file_drag.available().iter().any(|mime| mime == HTML));
        let drag = Drag {
            transfer: data.clone(),
            content_only: true,
        };
        assert!(drag.available().iter().any(|mime| mime == HTML));
        let html = String::from_utf8(drag.as_bytes(HTML).unwrap().into_owned()).unwrap();
        let normalized = html.replace("white-space:pre-wrap", "white-space:normal");
        let received = Document::from_html(&normalized).unwrap();
        assert_eq!(received.text, data.plain_text().unwrap());
        assert!(received.html().contains("font-weight:bold"));
        assert!(received.html().contains("font-style:italic"));

        let plain = Transfer {
            files: FileList(vec![directory.path().join("only.txt")]),
            text: Some(vec![Text::Inline("One\nTwo".into())]),
            ..(*data).clone()
        };
        let drag = Drag {
            transfer: Arc::new(plain),
            content_only: true,
        };
        assert!(drag.available().iter().any(|mime| mime == HTML));
        let html = String::from_utf8(drag.as_bytes(HTML).unwrap().into_owned()).unwrap();
        assert_eq!(Document::from_html(&html).unwrap().text, "One\nTwo");
    }

    #[test]
    fn text_drags_keep_utf8_and_offer_a_file_fallback() {
        let data = Transfer {
            files: FileList(vec![PathBuf::from("/tmp/Text.txt")]),
            text: Some(vec![Text::Inline("First line\nسلام\nLast line".into())]),
            text_only: true,
            has_local_files: false,
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
        let drag = Drag {
            transfer: Arc::new(data),
            content_only: true,
        };
        assert!(!drag.available().iter().any(|mime| mime == URI_LIST));
        assert!(drag.as_bytes(URI_LIST).is_none());
        assert!(drag.available().iter().any(|mime| mime == GNOME_FILES));
        assert_eq!(
            FileList::try_from((
                drag.as_bytes(GNOME_FILES).unwrap().into_owned(),
                GNOME_FILES.into()
            ))
            .unwrap()
            .0,
            drag.transfer.files.0
        );
        assert_eq!(
            drag.as_bytes(TEXT).unwrap().as_ref(),
            "First line\nسلام\nLast line".as_bytes()
        );
    }

    #[test]
    fn link_drags_offer_local_files_and_a_native_browser_url() {
        let data = Transfer {
            files: FileList(vec![PathBuf::from("/tmp/Link.html")]),
            text: Some(vec![Text::Inline("https://example.com/article".into())]),
            text_only: true,
            has_local_files: false,
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
        let drag = Drag {
            transfer: Arc::new(data),
            content_only: true,
        };
        assert!(!drag.available().iter().any(|mime| mime == URI_LIST));
        assert!(drag.as_bytes(URI_LIST).is_none());
        assert!(drag.available().iter().any(|mime| mime == GNOME_FILES));
        assert_eq!(
            FileList::try_from((
                drag.as_bytes(GNOME_FILES).unwrap().into_owned(),
                GNOME_FILES.into()
            ))
            .unwrap()
            .0,
            drag.transfer.files.0
        );
        assert_eq!(
            drag.as_bytes(TEXT).unwrap().as_ref(),
            b"https://example.com/article"
        );
    }

    #[test]
    fn text_file_contents_support_unicode_and_reject_invalid_or_oversized_data() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("notes.TXT");
        let source = Text::File(path.clone());
        let text = "Hello\nسلام";
        assert!(is_text_file(&path));
        std::fs::write(&path, text).unwrap();
        assert_eq!(source.contents().unwrap(), text);
        for little_endian in [true, false] {
            let mut bytes = if little_endian {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in text.encode_utf16() {
                bytes.extend(if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(source.contents().unwrap(), text);
        }
        for invalid in [&[0xff][..], &[0xff, 0xfe, 0], &[0xff, 0xfe, 0, 0xd8]] {
            std::fs::write(&path, invalid).unwrap();
            assert!(source.contents().is_none());
        }
        std::fs::write(&path, vec![b'a'; MAX_TEXT_FILE_SIZE as usize + 1]).unwrap();
        assert!(source.contents().is_none());
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
        assert!(files.as_bytes(TEXT).is_none());
        assert!(!files.available().iter().any(|mime| mime == TEXT));
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
        assert!(files.as_bytes(TEXT).is_none());
        assert!(!files.available().iter().any(|mime| mime == TEXT));
        assert_eq!(
            FileList::try_from((encoded, URI_LIST.into())).unwrap().0,
            files.0
        );
    }
}
