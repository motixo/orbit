// SPDX-License-Identifier: GPL-3.0-only

mod view;

use cosmic::iced::{self, font};
use std::{borrow::Cow, io::Read, ops::Range, path::Path, sync::Arc};

pub const HTML: &str = "text/html";
pub const HTML_UTF8: &str = "text/html;charset=utf-8";
pub const MAX_HTML: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    family: Option<String>,
    size: Option<f32>,
    line_height: Option<f32>,
    color: Option<[f32; 4]>,
    background: Option<[f32; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
struct Run {
    range: Range<usize>,
    style: Style,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub text: String,
    runs: Vec<Run>,
}

#[derive(Clone, Debug)]
pub struct Preview {
    document: Arc<Document>,
    selection: Range<usize>,
}

impl Preview {
    pub fn new(document: Document) -> Self {
        Self {
            document: Arc::new(document),
            selection: 0..0,
        }
    }

    pub fn select(&mut self, selection: Range<usize>) {
        if self.document.text.is_char_boundary(selection.start)
            && self.document.text.is_char_boundary(selection.end)
        {
            self.selection = selection;
        }
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.document.text.len();
    }

    pub fn selected(&self) -> Option<Document> {
        let range = self.selection.start.min(self.selection.end)
            ..self.selection.start.max(self.selection.end);
        (!range.is_empty()).then(|| self.document.slice(range))
    }

    pub fn has_selection(&self) -> bool {
        self.selection.start != self.selection.end
    }

    pub fn view(&self, id: iced::window::Id) -> cosmic::Element<'_, crate::app::Message> {
        view::view(self, id)
    }
}

impl Document {
    pub fn read(path: &Path, text: &str) -> Option<Self> {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("html"))
        {
            return None;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(MAX_HTML as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_HTML {
            return None;
        }
        let document = Self::from_html(std::str::from_utf8(&bytes).ok()?).ok()?;
        (document.text == text).then_some(document)
    }

    fn slice(&self, range: Range<usize>) -> Self {
        Self {
            text: self.text[range.clone()].to_owned(),
            runs: self
                .runs
                .iter()
                .filter_map(|run| {
                    let start = run.range.start.max(range.start);
                    let end = run.range.end.min(range.end);
                    (start < end).then(|| Run {
                        range: start - range.start..end - range.start,
                        style: run.style.clone(),
                    })
                })
                .collect(),
        }
    }

    pub fn html(&self) -> String {
        let mut html = String::from("<span style=\"white-space:pre-wrap\">");
        for run in &self.runs {
            let style = &run.style;
            let mut css = String::new();
            if style.bold {
                css.push_str("font-weight:bold;");
            }
            if style.italic {
                css.push_str("font-style:italic;");
            }
            if style.underline || style.strike {
                css.push_str("text-decoration:");
                if style.underline {
                    css.push_str("underline ");
                }
                if style.strike {
                    css.push_str("line-through");
                }
                css.push(';');
            }
            if let Some(family) = &style.family {
                css.push_str(&format!(
                    "font-family:'{}';",
                    family.replace(['\'', '"', ';', '\\'], "")
                ));
            }
            if let Some(size) = style.size {
                css.push_str(&format!("font-size:{size}px;"));
            }
            if let Some(height) = style.line_height {
                css.push_str(&format!("line-height:{height}px;"));
            }
            for (name, color) in [
                ("color", style.color),
                ("background-color", style.background),
            ] {
                if let Some([r, g, b, a]) = color {
                    css.push_str(&format!(
                        "{name}:rgba({},{},{},{a});",
                        r * 255.0,
                        g * 255.0,
                        b * 255.0
                    ));
                }
            }
            html.push_str(&format!(
                "<span style=\"{}\">{}</span>",
                quick_xml::escape::escape(&css),
                quick_xml::escape::escape(&self.text[run.range.clone()])
            ));
        }
        html.push_str("</span>");
        html
    }

    pub fn from_html(html: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        use quick_xml::events::Event;
        let mut reader = quick_xml::Reader::from_str(html.trim_start_matches('\u{feff}'));
        reader.config_mut().check_end_names = false;
        let mut document = Self::default();
        let mut stack: Vec<(String, Style, bool, bool)> = Vec::new();
        let mut lists: Vec<(bool, i32)> = Vec::new();
        loop {
            match reader.read_event()? {
                Event::Start(tag) | Event::Empty(tag) => {
                    if stack.len() >= 256 {
                        return Err("HTML nesting exceeds 256 levels".into());
                    }
                    let name = String::from_utf8_lossy(tag.name().as_ref()).to_ascii_lowercase();
                    let (mut style, mut hidden, mut pre) = stack
                        .last()
                        .map(|(_, style, hidden, pre)| (style.clone(), *hidden, *pre))
                        .unwrap_or_default();
                    let mut attributes = Vec::new();
                    for attribute in tag.html_attributes() {
                        let attribute = attribute?;
                        attributes.push((
                            String::from_utf8_lossy(attribute.key.as_ref()).to_ascii_lowercase(),
                            quick_xml::escape::unescape(std::str::from_utf8(
                                attribute.value.as_ref(),
                            )?)?
                            .into_owned(),
                        ));
                    }
                    hidden |= matches!(
                        name.as_str(),
                        "script" | "style" | "head" | "iframe" | "object" | "template"
                    );
                    hidden |= attributes.iter().any(|(key, _)| key == "hidden");
                    pre |= name == "pre";
                    match name.as_str() {
                        "b" | "strong" => style.bold = true,
                        "i" | "em" => style.italic = true,
                        "u" | "ins" => style.underline = true,
                        "s" | "strike" | "del" => style.strike = true,
                        "code" | "pre" | "kbd" | "samp" => style.family = Some("monospace".into()),
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                            style.bold = true;
                            style.size = Some(match name.as_str() {
                                "h1" => 28.0,
                                "h2" => 24.0,
                                "h3" => 20.0,
                                "h4" => 18.0,
                                _ => 16.0,
                            });
                        }
                        _ => (),
                    }
                    for (key, value) in &attributes {
                        match key.as_str() {
                            "color" if name == "font" => style.color = parse_color(value),
                            "face" if name == "font" => style.family = family(value),
                            "style" => {
                                for property in value.split(';') {
                                    if let Some((key, value)) = property.split_once(':') {
                                        let value =
                                            value.trim().trim_end_matches("!important").trim();
                                        match key.trim().to_ascii_lowercase().as_str() {
                                            "font-weight" => {
                                                style.bold = matches!(value, "bold" | "bolder")
                                                    || value
                                                        .parse::<u16>()
                                                        .is_ok_and(|weight| weight >= 600)
                                            }
                                            "font-style" => {
                                                style.italic = matches!(value, "italic" | "oblique")
                                            }
                                            "font-family" => style.family = family(value),
                                            "font-size" => {
                                                style.size =
                                                    css_size(value, style.size.unwrap_or(14.0))
                                                        .map(|size| size.clamp(6.0, 128.0))
                                            }
                                            "line-height" => {
                                                style.line_height =
                                                    css_size(value, style.size.unwrap_or(14.0))
                                                        .or_else(|| {
                                                            value
                                                                .parse::<f32>()
                                                                .ok()
                                                                .filter(|v| v.is_finite())
                                                                .map(|v| {
                                                                    v * style.size.unwrap_or(14.0)
                                                                })
                                                        })
                                                        .map(|height| height.clamp(6.0, 256.0))
                                            }
                                            "color" => {
                                                if let Some(color) = parse_color(value) {
                                                    style.color = Some(color);
                                                }
                                            }
                                            "background" | "background-color" => {
                                                if let Some(color) = parse_color(value) {
                                                    style.background = Some(color);
                                                }
                                            }
                                            "text-decoration" | "text-decoration-line" => {
                                                style.underline = value.contains("underline");
                                                style.strike = value.contains("line-through");
                                            }
                                            "white-space" => {
                                                pre = matches!(
                                                    value,
                                                    "pre" | "pre-wrap" | "break-spaces"
                                                )
                                            }
                                            "display" if value == "none" => hidden = true,
                                            "visibility" if value == "hidden" => hidden = true,
                                            _ => (),
                                        }
                                    }
                                }
                            }
                            _ => (),
                        }
                    }
                    if !hidden {
                        if block(&name)
                            && !(matches!(name.as_str(), "p" | "div")
                                && stack.last().is_some_and(|(tag, _, _, _)| tag == "li")
                                && document.marker_only())
                        {
                            document.break_line(&style);
                        }
                        if matches!(name.as_str(), "ul" | "ol") {
                            let start = attributes
                                .iter()
                                .find(|(key, _)| key == "start")
                                .and_then(|(_, value)| value.parse::<i32>().ok())
                                .unwrap_or(1);
                            lists.push((name == "ol", start));
                        }
                        if name == "li" {
                            document.push(&"  ".repeat(lists.len().saturating_sub(1)), &style);
                            let marker = if let Some((ordered, counter)) = lists.last_mut() {
                                if *ordered {
                                    if let Some(value) = attributes
                                        .iter()
                                        .find(|(key, _)| key == "value")
                                        .and_then(|(_, value)| value.parse::<i32>().ok())
                                    {
                                        *counter = value;
                                    }
                                    let marker = format!("{counter}. ");
                                    *counter = counter.saturating_add(1);
                                    marker
                                } else {
                                    "• ".into()
                                }
                            } else {
                                "• ".into()
                            };
                            document.push(&marker, &style);
                        }
                        if name == "br" {
                            document.push("\n", &style);
                        }
                    }
                    if !matches!(
                        name.as_str(),
                        "br" | "img"
                            | "hr"
                            | "input"
                            | "meta"
                            | "link"
                            | "wbr"
                            | "source"
                            | "area"
                            | "base"
                            | "embed"
                            | "param"
                            | "track"
                            | "col"
                    ) {
                        stack.push((name, style, hidden, pre));
                    }
                }
                Event::End(tag) => {
                    let name = String::from_utf8_lossy(tag.name().as_ref()).to_ascii_lowercase();
                    if let Some(index) = stack.iter().rposition(|(tag, _, _, _)| *tag == name) {
                        let (_, style, hidden, _) = &stack[index];
                        if !hidden && block(&name) {
                            document.break_line(style);
                        }
                        if !hidden && matches!(name.as_str(), "ul" | "ol") {
                            lists.pop();
                        }
                        stack.truncate(index);
                    }
                }
                Event::Text(text) => {
                    if !stack.last().is_some_and(|(_, _, hidden, _)| *hidden) {
                        let style = stack
                            .last()
                            .map(|(_, style, _, _)| style.clone())
                            .unwrap_or_default();
                        let value = text.html_content()?;
                        // Browser HTML indentation is not a visible blank list item.
                        if !(value.trim().is_empty()
                            && stack.last().is_some_and(|(name, _, _, _)| {
                                matches!(name.as_str(), "ul" | "ol")
                            }))
                        {
                            document.push_text(
                                &value,
                                &style,
                                stack.last().is_some_and(|(_, _, _, pre)| *pre),
                            );
                        }
                    }
                }
                Event::GeneralRef(reference) => {
                    if !stack.last().is_some_and(|(_, _, hidden, _)| *hidden) {
                        let style = stack
                            .last()
                            .map(|(_, style, _, _)| style.clone())
                            .unwrap_or_default();
                        document.push_text(
                            &quick_xml::escape::unescape(&format!("&{};", reference.decode()?))?,
                            &style,
                            stack.last().is_some_and(|(_, _, _, pre)| *pre),
                        );
                    }
                }
                Event::CData(text) => {
                    if !stack.last().is_some_and(|(_, _, hidden, _)| *hidden) {
                        let style = stack
                            .last()
                            .map(|(_, style, _, _)| style.clone())
                            .unwrap_or_default();
                        document.push(&text.decode()?, &style);
                    }
                }
                Event::Eof => break,
                _ => (),
            }
            if document.text.len() > 1024 * 1024 || document.runs.len() > 8192 {
                return Err("Formatted text is too large".into());
            }
        }
        Ok(document)
    }

    fn marker_only(&self) -> bool {
        let line = self.text.rsplit('\n').next().unwrap_or_default().trim();
        line == "•"
            || line
                .strip_suffix('.')
                .is_some_and(|number| number.parse::<i32>().is_ok())
    }

    fn push_text(&mut self, text: &str, style: &Style, pre: bool) {
        if pre {
            self.push(text, style);
            return;
        }
        let mut normalized = String::new();
        let mut whitespace = self.text.is_empty() || self.text.ends_with([' ', '\n', '\t']);
        for ch in text.chars() {
            if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                if !whitespace {
                    normalized.push(' ');
                }
                whitespace = true;
            } else {
                normalized.push(ch);
                whitespace = false;
            }
        }
        self.push(&normalized, style);
    }

    fn push(&mut self, text: &str, style: &Style) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if let Some(last) = self.runs.last_mut()
            && last.style == *style
        {
            last.range.end = self.text.len();
        } else {
            self.runs.push(Run {
                range: start..self.text.len(),
                style: style.clone(),
            });
        }
    }

    fn break_line(&mut self, style: &Style) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.push("\n", style);
        }
    }
}

impl cosmic::iced::clipboard::mime::AsMimeTypes for Document {
    fn available(&self) -> Cow<'static, [String]> {
        Cow::Owned(vec![
            HTML_UTF8.into(),
            HTML.into(),
            crate::transfer::UTF8_TEXT.into(),
            crate::transfer::TEXT.into(),
        ])
    }
    fn as_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        match mime {
            HTML | HTML_UTF8 => Some(Cow::Owned(self.html().into_bytes())),
            crate::transfer::TEXT | crate::transfer::UTF8_TEXT => {
                Some(Cow::Owned(self.text.clone().into_bytes()))
            }
            _ => None,
        }
    }
}

impl Style {
    fn font(&self) -> iced::Font {
        use std::{
            collections::HashMap,
            sync::{LazyLock, Mutex},
        };
        static FAMILIES: LazyLock<Mutex<HashMap<String, &'static str>>> =
            LazyLock::new(Mutex::default);
        let mut font = cosmic::font::default();
        if let Some(name) = &self.family {
            font.family = match name.to_ascii_lowercase().as_str() {
                "monospace" => font::Family::Monospace,
                "serif" => font::Family::Serif,
                "sans-serif" | "system-ui" => font::Family::SansSerif,
                "cursive" => font::Family::Cursive,
                "fantasy" => font::Family::Fantasy,
                _ => {
                    let mut families = FAMILIES.lock().unwrap();
                    let family = families.get(name).copied().or_else(|| {
                        if families.len() >= 128 {
                            return None;
                        }
                        let family = Box::leak(name.clone().into_boxed_str());
                        families.insert(name.clone(), family);
                        Some(&*family)
                    });
                    family.map(font::Family::Name).unwrap_or(font.family)
                }
            };
        }
        font.weight = if self.bold {
            font::Weight::Bold
        } else {
            font::Weight::Normal
        };
        font.style = if self.italic {
            font::Style::Italic
        } else {
            font::Style::Normal
        };
        font
    }
}

fn family(value: &str) -> Option<String> {
    let name = value.split(',').next()?.trim().trim_matches(['\'', '"']);
    (!name.is_empty() && name.len() <= 128).then(|| name.to_owned())
}

fn parse_color(value: &str) -> Option<[f32; 4]> {
    value
        .parse::<csscolorparser::Color>()
        .ok()
        .map(|color| color.to_array().map(|v| v.clamp(0.0, 1.0)))
}

fn css_size(value: &str, base: f32) -> Option<f32> {
    for (suffix, scale) in [
        ("rem", 14.0),
        ("px", 1.0),
        ("pt", 4.0 / 3.0),
        ("em", base),
        ("%", base / 100.0),
    ] {
        if let Some(value) = value.strip_suffix(suffix) {
            return value
                .trim()
                .parse::<f32>()
                .ok()
                .map(|v| v * scale)
                .filter(|v| v.is_finite());
        }
    }
    None
}

fn block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "li"
            | "ul"
            | "ol"
            | "pre"
            | "blockquote"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "tr"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_inline_formatting_lists_and_safe_clipboard_html() {
        let document = Document::from_html("<h2>Title</h2><ul><li><p><b>Bold</b></p><ul><li><i>Nested</i></li></ul></li></ul><ol start='3'><li style='color:rgb(255,0,0);font-size:20px'>Number</li></ol><script>hidden</script>").unwrap();
        assert_eq!(document.text, "Title\n• Bold\n  • Nested\n3. Number\n");
        assert!(
            document
                .runs
                .iter()
                .any(|run| run.style.bold && document.text[run.range.clone()].contains("Bold"))
        );
        assert!(document.runs.iter().any(|run| run.style.italic));
        assert!(document.runs.iter().any(
            |run| run.style.size == Some(20.0) && run.style.color == Some([1.0, 0.0, 0.0, 1.0])
        ));
        let roundtrip = Document::from_html(&document.html()).unwrap();
        assert_eq!(roundtrip, document);
        assert!(!document.html().contains("script"));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snippet.html");
        std::fs::write(&path, document.html()).unwrap();
        assert_eq!(
            Document::read(&path, &document.text),
            Some(document.clone())
        );
        assert!(Document::read(&path, "changed").is_none());
        let inline = Document::from_html("<b>Inline</b> text").unwrap();
        assert_eq!(Document::from_html(&inline.html()).unwrap(), inline);
    }
}
