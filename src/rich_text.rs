// SPDX-License-Identifier: GPL-3.0-only

mod view;

use cosmic::iced::{self, font};
use std::{borrow::Cow, io::Read, ops::Range, path::Path, sync::Arc};

pub const HTML: &str = "text/html";
pub const HTML_UTF8: &str = "text/html;charset=utf-8";
pub const MAX_HTML: usize = 8 * 1024 * 1024;

pub fn html_text(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    quick_xml::escape::escape(text).replace('\n', "<br>")
}

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

#[derive(Clone, Copy, Debug, Default)]
enum Whitespace {
    #[default]
    Lines,
    Normal,
    Pre,
}

struct HtmlFrame {
    name: String,
    style: Style,
    hidden: bool,
    whitespace: Whitespace,
    block: bool,
    start: usize,
    table: Option<usize>,
    cell: Option<(usize, usize, usize)>,
    flow: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct Run {
    range: Range<usize>,
    style: Style,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Alignment {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
struct Cell {
    range: Range<usize>,
    header: bool,
    columns: usize,
    rows: usize,
    alignment: Alignment,
    background: Option<[f32; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
struct Table {
    range: Range<usize>,
    rows: Vec<Vec<Cell>>,
}

#[derive(Clone, Debug, PartialEq)]
struct Block {
    range: Range<usize>,
    content_start: usize,
    content_end: usize,
    tag: String,
    number: Option<i32>,
    style: Style,
    block: bool,
    leading_break: bool,
    end_index: usize,
}

impl Table {
    fn shift(&mut self, offset: usize) {
        self.range = self.range.start + offset..self.range.end + offset;
        for cell in self.rows.iter_mut().flatten() {
            cell.range = cell.range.start + offset..cell.range.end + offset;
        }
    }

    fn slice(&self, range: &Range<usize>) -> Option<Self> {
        let start = self.range.start.max(range.start);
        let end = self.range.end.min(range.end);
        if start >= end {
            return None;
        }
        let rows = self
            .rows
            .iter()
            .filter_map(|row| {
                let cells: Vec<_> = row
                    .iter()
                    .filter_map(|cell| {
                        let start = cell.range.start.max(range.start);
                        let end = cell.range.end.min(range.end);
                        (start <= end).then(|| Cell {
                            range: start - range.start..end - range.start,
                            ..cell.clone()
                        })
                    })
                    .collect();
                (!cells.is_empty()).then_some(cells)
            })
            .collect();
        Some(Self {
            range: start - range.start..end - range.start,
            rows,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub text: String,
    runs: Vec<Run>,
    tables: Vec<Table>,
    blocks: Vec<Block>,
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
    pub fn plain(text: &str) -> Self {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let runs = if text.is_empty() {
            Vec::new()
        } else {
            vec![Run {
                range: 0..text.len(),
                style: Style::default(),
            }]
        };
        Self {
            text,
            runs,
            tables: Vec::new(),
            blocks: Vec::new(),
        }
    }

    pub fn join(documents: Vec<Self>) -> Self {
        let mut joined = Self::default();
        for (index, document) in documents.into_iter().enumerate() {
            if index > 0 {
                joined.push("\n\n", &Style::default());
            }
            let offset = joined.text.len();
            for run in document.runs {
                joined.push(&document.text[run.range], &run.style);
            }
            joined
                .tables
                .extend(document.tables.into_iter().map(|mut table| {
                    table.shift(offset);
                    table
                }));
            let block_offset = joined.blocks.len();
            joined
                .blocks
                .extend(document.blocks.into_iter().map(|mut block| {
                    block.range = block.range.start + offset..block.range.end + offset;
                    block.content_start += offset;
                    block.content_end += offset;
                    block.end_index += block_offset;
                    block
                }));
        }
        joined
    }

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
        let retained: Vec<_> = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.range.start >= range.start && block.range.end <= range.end)
            .collect();
        let blocks = retained
            .iter()
            .map(|(_, block)| Block {
                range: block.range.start - range.start..block.range.end - range.start,
                content_start: block.content_start - range.start,
                content_end: block.content_end - range.start,
                end_index: retained.partition_point(|(index, _)| *index < block.end_index),
                ..(*block).clone()
            })
            .collect();
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
            tables: self
                .tables
                .iter()
                .filter_map(|table| table.slice(&range))
                .collect(),
            blocks,
        }
    }

    pub fn html(&self) -> String {
        self.serialize(false)
    }

    pub fn clipboard_html(&self) -> String {
        self.serialize(true)
    }

    fn serialize(&self, clipboard: bool) -> String {
        let mut html = String::new();
        self.write_flow(
            &mut html,
            0..self.text.len(),
            0..self.blocks.len(),
            clipboard,
            true,
            true,
        );
        html
    }

    fn write_flow(
        &self,
        html: &mut String,
        range: Range<usize>,
        blocks: Range<usize>,
        clipboard: bool,
        paragraphs: bool,
        tables: bool,
    ) {
        let mut offset = range.start;
        let mut index = blocks.start;
        while offset < range.end {
            while index < blocks.end
                && (self.blocks[index].range.start < offset
                    || self.blocks[index].range.end > range.end)
            {
                index += 1;
            }
            let block = (index < blocks.end).then(|| &self.blocks[index]);
            let table = self.tables.iter().find(|table| {
                tables
                    && !table.range.is_empty()
                    && table.range.start >= offset
                    && table.range.end <= range.end
                    && !table.rows.is_empty()
            });
            if let Some(block) = block.filter(|block| {
                table.is_none_or(|table| {
                    block.range.start <= table.range.start && block.range.end >= table.range.end
                        || block.range.start < table.range.start
                })
            }) {
                let end = block.range.start
                    - usize::from(block.leading_break && block.range.start > offset);
                self.write_gap(html, offset..end, clipboard, paragraphs);
                let number = block
                    .number
                    .map(|number| {
                        format!(
                            " {}=\"{number}\"",
                            if block.tag == "ol" { "start" } else { "value" }
                        )
                    })
                    .unwrap_or_default();
                html.push_str(&format!(
                    "<{}{number} style=\"white-space:pre-wrap;{}{}\">",
                    block.tag,
                    if block.block { "" } else { "display:inline;" },
                    quick_xml::escape::escape(block.style.css())
                ));
                self.write_flow(
                    html,
                    block.content_start..block.content_end,
                    index + 1..block.end_index,
                    clipboard,
                    false,
                    tables,
                );
                if block.content_start == block.content_end && block.content_end < block.range.end {
                    self.write_html(html, block.content_end..block.range.end);
                }
                html.push_str(&format!("</{}>", block.tag));
                offset = block.range.end;
                index = block.end_index;
                continue;
            }
            let Some(table) = table else {
                self.write_gap(html, offset..range.end, clipboard, paragraphs);
                break;
            };
            self.write_gap(html, offset..table.range.start, clipboard, paragraphs);
            html.push_str("<table style=\"display:inline-table;border-collapse:collapse\">");
            for row in &table.rows {
                html.push_str("<tr>");
                for cell in row {
                    let tag = if cell.header { "th" } else { "td" };
                    let alignment = match cell.alignment {
                        Alignment::Left => "left",
                        Alignment::Center => "center",
                        Alignment::Right => "right",
                    };
                    let background = cell
                        .background
                        .map(|[r, g, b, a]| {
                            format!(
                                "background-color:rgba({},{},{},{a});",
                                r * 255.0,
                                g * 255.0,
                                b * 255.0
                            )
                        })
                        .unwrap_or_default();
                    html.push_str(&format!("<{tag} colspan=\"{}\" rowspan=\"{}\" style=\"border:1px solid #888;padding:6px;text-align:{alignment};white-space:pre-wrap;{background}\">", cell.columns, cell.rows));
                    self.write_flow(
                        html,
                        cell.range.clone(),
                        index..blocks.end,
                        clipboard,
                        false,
                        false,
                    );
                    html.push_str(&format!("</{tag}>"));
                }
                html.push_str("</tr>");
            }
            html.push_str("</table>");
            offset = table.range.end;
        }
    }

    fn write_gap(&self, html: &mut String, range: Range<usize>, clipboard: bool, paragraphs: bool) {
        if paragraphs {
            self.write_text(html, range, clipboard);
        } else {
            self.write_html(html, range);
        }
    }

    fn write_text(&self, html: &mut String, range: Range<usize>, clipboard: bool) {
        if range.is_empty() {
            return;
        }
        if !clipboard {
            html.push_str("<span style=\"white-space:pre-wrap\">");
            self.write_html(html, range);
            html.push_str("</span>");
            return;
        }
        let mut offset = range.start;
        let mut lines = self.text[range].split('\n').peekable();
        while let Some(line) = lines.next() {
            // Editors can discard trailing breaks inside inline spans; use block boundaries.
            let tag = if lines.peek().is_some() { "p" } else { "span" };
            html.push_str(&format!("<{tag} style=\"margin:0;white-space:pre-wrap\">"));
            if line.is_empty() && tag == "p" {
                html.push_str("<br>");
            } else {
                self.write_html(html, offset..offset + line.len());
            }
            html.push_str(&format!("</{tag}>"));
            offset += line.len() + 1;
        }
    }

    fn write_html(&self, html: &mut String, range: Range<usize>) {
        let first = self
            .runs
            .partition_point(|run| run.range.end <= range.start);
        for run in self.runs[first..]
            .iter()
            .take_while(|run| run.range.start < range.end)
        {
            let start = run.range.start.max(range.start);
            let end = run.range.end.min(range.end);
            if start >= end {
                continue;
            }
            html.push_str(&format!(
                "<span style=\"{}\">{}</span>",
                quick_xml::escape::escape(run.style.css()),
                html_text(&self.text[start..end])
            ));
        }
    }

    pub fn from_html(html: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        use quick_xml::events::Event;
        let mut reader = quick_xml::Reader::from_str(html.trim_start_matches('\u{feff}'));
        reader.config_mut().check_end_names = false;
        let mut document = Self::default();
        let mut stack: Vec<HtmlFrame> = Vec::new();
        let mut lists: Vec<(bool, i32)> = Vec::new();
        let mut block_break = None;
        let mut table_elements = 0;
        loop {
            match reader.read_event()? {
                Event::Start(tag) | Event::Empty(tag) => {
                    if stack.len() >= 256 {
                        return Err("HTML nesting exceeds 256 levels".into());
                    }
                    let name = String::from_utf8_lossy(tag.name().as_ref()).to_ascii_lowercase();
                    let (mut style, mut hidden, mut whitespace) = stack
                        .last()
                        .map(|frame| (frame.style.clone(), frame.hidden, frame.whitespace))
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
                    if name == "pre" {
                        whitespace = Whitespace::Pre;
                    }
                    let mut is_block = block(&name);
                    if name == "tr"
                        && stack
                            .iter()
                            .rev()
                            .find(|frame| frame.name == "table")
                            .is_some_and(|frame| frame.table.is_some())
                    {
                        is_block = false;
                    }
                    match name.as_str() {
                        "b" | "strong" | "th" => style.bold = true,
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
                                                whitespace = match value {
                                                    "pre" | "pre-wrap" | "break-spaces" => {
                                                        Whitespace::Pre
                                                    }
                                                    "pre-line" => Whitespace::Lines,
                                                    "normal" | "nowrap" => Whitespace::Normal,
                                                    _ => whitespace,
                                                }
                                            }
                                            "display" => match value {
                                                "none" => hidden = true,
                                                "block" | "list-item" | "flex" | "grid"
                                                | "table" | "flow-root" => is_block = true,
                                                "inline" | "inline-block" | "inline-table"
                                                | "inline-flex" | "inline-grid" | "contents" => {
                                                    is_block = false
                                                }
                                                _ => (),
                                            },
                                            "visibility" if value == "hidden" => hidden = true,
                                            _ => (),
                                        }
                                    }
                                }
                            }
                            _ => (),
                        }
                    }
                    let mut table = None;
                    let mut cell = None;
                    let mut flow = None;
                    if !hidden {
                        table_elements +=
                            usize::from(matches!(name.as_str(), "table" | "tr" | "td" | "th"));
                        let before_block = document.text.len();
                        if is_block
                            && !(matches!(name.as_str(), "p" | "div")
                                && stack.last().is_some_and(|frame| frame.name == "li")
                                && document.marker_only())
                            && !stack
                                .iter()
                                .rev()
                                .find(|frame| frame.cell.is_some())
                                .is_some_and(|frame| frame.start == document.text.len())
                        {
                            document.break_line(&style);
                        }
                        let leading_break = document.text.len() > before_block;
                        if name == "table" && !stack.iter().any(|frame| frame.name == "table") {
                            table = Some(document.tables.len());
                            document.tables.push(Table {
                                range: document.text.len()..document.text.len(),
                                rows: Vec::new(),
                            });
                        }
                        let current_table = stack
                            .iter()
                            .rev()
                            .find(|frame| frame.name == "table")
                            .and_then(|frame| frame.table);
                        if let Some(index) = current_table {
                            if name == "tr" {
                                if !document.tables[index].rows.is_empty() {
                                    document.push("\n", &Style::default());
                                } else if document.text.len() > document.tables[index].range.start {
                                    document.break_line(&Style::default());
                                    document.tables[index].range.start = document.text.len();
                                }
                                document.tables[index].rows.push(Vec::new());
                            } else if matches!(name.as_str(), "td" | "th")
                                && !document.tables[index].rows.is_empty()
                            {
                                let row = document.tables[index].rows.len() - 1;
                                if !document.tables[index].rows[row].is_empty() {
                                    document.push("\t", &Style::default());
                                }
                                let span = |key: &str| {
                                    attributes
                                        .iter()
                                        .find(|(name, _)| name == key)
                                        .and_then(|(_, value)| value.parse::<usize>().ok())
                                        .unwrap_or(1)
                                        .clamp(1, 64)
                                };
                                let mut alignment = Alignment::Left;
                                for (key, value) in &attributes {
                                    let value = if key == "align" {
                                        Some(value.as_str())
                                    } else if key == "style" {
                                        value
                                            .split(';')
                                            .filter_map(|property| property.split_once(':'))
                                            .find(|(name, _)| {
                                                name.trim().eq_ignore_ascii_case("text-align")
                                            })
                                            .map(|(_, value)| value.trim())
                                    } else {
                                        None
                                    };
                                    if let Some(value) = value {
                                        alignment = match value {
                                            "center" => Alignment::Center,
                                            "right" | "end" => Alignment::Right,
                                            _ => Alignment::Left,
                                        };
                                    }
                                }
                                let column = document.tables[index].rows[row].len();
                                document.tables[index].rows[row].push(Cell {
                                    range: document.text.len()..document.text.len(),
                                    header: name == "th",
                                    columns: span("colspan"),
                                    rows: span("rowspan"),
                                    alignment,
                                    background: style.background,
                                });
                                cell = Some((index, row, column));
                            }
                        }
                        if flow_tag(&name) {
                            flow = Some(document.blocks.len());
                            document.blocks.push(Block {
                                range: document.text.len()..document.text.len(),
                                content_start: document.text.len(),
                                content_end: document.text.len(),
                                tag: name.clone(),
                                number: attributes
                                    .iter()
                                    .find(|(key, _)| {
                                        key == if name == "ol" { "start" } else { "value" }
                                    })
                                    .and_then(|(_, value)| value.parse().ok()),
                                style: style.clone(),
                                block: is_block,
                                leading_break,
                                end_index: document.blocks.len() + 1,
                            });
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
                        if let Some(index) = flow {
                            document.blocks[index].content_start = document.text.len();
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
                        stack.push(HtmlFrame {
                            name,
                            style,
                            hidden,
                            whitespace,
                            block: is_block,
                            start: document.text.len(),
                            table,
                            cell,
                            flow,
                        });
                    }
                }
                Event::End(tag) => {
                    let name = String::from_utf8_lossy(tag.name().as_ref()).to_ascii_lowercase();
                    if let Some(index) = stack.iter().rposition(|frame| frame.name == name) {
                        for frame in stack.drain(index..).rev() {
                            document.close_frame(&frame, &mut block_break);
                            if !frame.hidden && matches!(frame.name.as_str(), "ul" | "ol") {
                                lists.pop();
                            }
                        }
                    }
                }
                Event::Text(text) => {
                    if !stack.last().is_some_and(|frame| frame.hidden) {
                        let style = stack
                            .last()
                            .map(|frame| frame.style.clone())
                            .unwrap_or_default();
                        let value = text.html_content()?;
                        // Browser indentation between list items and table cells is not content.
                        if !(value.trim().is_empty()
                            && stack.last().is_some_and(|frame| {
                                matches!(
                                    frame.name.as_str(),
                                    "ul" | "ol" | "table" | "thead" | "tbody" | "tfoot" | "tr"
                                )
                            }))
                        {
                            document.push_text(
                                &value,
                                &style,
                                stack
                                    .last()
                                    .map(|frame| frame.whitespace)
                                    .unwrap_or_default(),
                            );
                        }
                    }
                }
                Event::GeneralRef(reference) => {
                    if !stack.last().is_some_and(|frame| frame.hidden) {
                        let style = stack
                            .last()
                            .map(|frame| frame.style.clone())
                            .unwrap_or_default();
                        document.push_text(
                            &quick_xml::escape::unescape(&format!("&{};", reference.decode()?))?,
                            &style,
                            stack
                                .last()
                                .map(|frame| frame.whitespace)
                                .unwrap_or_default(),
                        );
                    }
                }
                Event::CData(text) => {
                    if !stack.last().is_some_and(|frame| frame.hidden) {
                        let style = stack
                            .last()
                            .map(|frame| frame.style.clone())
                            .unwrap_or_default();
                        document.push_text(
                            &text.html_content()?,
                            &style,
                            stack
                                .last()
                                .map(|frame| frame.whitespace)
                                .unwrap_or_default(),
                        );
                    }
                }
                Event::Eof => {
                    for frame in stack.iter().rev() {
                        document.close_frame(frame, &mut block_break);
                    }
                    break;
                }
                _ => (),
            }
            if document.text.len() > 1024 * 1024
                || document.runs.len() > 8192
                || document.blocks.len() > 8192
                || table_elements > 8192
            {
                return Err("Formatted text is too large".into());
            }
        }
        document.tables.retain(|table| {
            let mut offset = table.range.start;
            for (row_index, row) in table.rows.iter().enumerate() {
                if row_index > 0 {
                    if document.text.as_bytes().get(offset) != Some(&b'\n') {
                        return false;
                    }
                    offset += 1;
                }
                for (column, cell) in row.iter().enumerate() {
                    if column > 0 {
                        if document.text.as_bytes().get(offset) != Some(&b'\t') {
                            return false;
                        }
                        offset += 1;
                    }
                    if cell.range.start != offset {
                        return false;
                    }
                    offset = cell.range.end;
                }
            }
            offset == table.range.end && !table.range.is_empty()
        });
        Ok(document)
    }

    fn close_frame(&mut self, frame: &HtmlFrame, block_break: &mut Option<usize>) {
        if frame.hidden {
            return;
        }
        if let Some((table, row, column)) = frame.cell {
            if *block_break == Some(self.text.len()) && self.text.len() > frame.start {
                self.text.pop();
                while self
                    .runs
                    .last()
                    .is_some_and(|run| run.range.start >= self.text.len())
                {
                    self.runs.pop();
                }
                if let Some(run) = self.runs.last_mut() {
                    run.range.end = self.text.len();
                }
                for block in self
                    .blocks
                    .iter_mut()
                    .rev()
                    .take_while(|block| block.range.start >= frame.start)
                {
                    if block.range.end > self.text.len() {
                        block.block = false;
                        block.leading_break = false;
                    }
                    block.range.end = block.range.end.min(self.text.len());
                    block.content_end = block.content_end.min(self.text.len());
                }
            }
            self.tables[table].rows[row][column].range.end = self.text.len();
        }
        if let Some(index) = frame.table {
            self.tables[index].range.end = self.text.len();
        }
        let mut content_end = self.text.len();
        if frame.block {
            if frame
                .flow
                .is_some_and(|index| self.blocks.len() == index + 1)
                && self.text.get(frame.start..) == Some("\n")
            {
                content_end = frame.start;
            }
            let before = self.text.len();
            if before == frame.start {
                self.push("\n", &frame.style);
            } else {
                self.break_line(&frame.style);
            }
            if self.text.len() > before {
                *block_break = Some(self.text.len());
            }
        }
        if let Some(index) = frame.flow {
            self.blocks[index].range.end = self.text.len();
            self.blocks[index].content_end = content_end;
            self.blocks[index].end_index = self.blocks.len();
        }
    }

    fn marker_only(&self) -> bool {
        let line = self.text.rsplit('\n').next().unwrap_or_default().trim();
        line == "•"
            || line
                .strip_suffix('.')
                .is_some_and(|number| number.parse::<i32>().is_ok())
    }

    fn push_text(&mut self, text: &str, style: &Style, mode: Whitespace) {
        if matches!(mode, Whitespace::Pre) {
            self.push(text, style);
            return;
        }
        let mut normalized = String::new();
        let mut whitespace = self.text.is_empty() || self.text.ends_with([' ', '\n', '\t']);
        // Copied HTML can omit the source's white-space CSS; keep its literal line breaks.
        for ch in text.chars() {
            if ch == '\n' && matches!(mode, Whitespace::Lines) {
                normalized.push('\n');
                whitespace = true;
            } else if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
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
            HTML | HTML_UTF8 => Some(Cow::Owned(self.clipboard_html().into_bytes())),
            crate::transfer::TEXT | crate::transfer::UTF8_TEXT => {
                Some(Cow::Owned(self.text.clone().into_bytes()))
            }
            _ => None,
        }
    }
}

impl Style {
    fn css(&self) -> String {
        let mut css = String::new();
        css.push_str(if self.bold {
            "font-weight:bold;"
        } else {
            "font-weight:normal;"
        });
        css.push_str(if self.italic {
            "font-style:italic;"
        } else {
            "font-style:normal;"
        });
        if self.underline || self.strike {
            css.push_str("text-decoration:");
            if self.underline {
                css.push_str("underline ");
            }
            if self.strike {
                css.push_str("line-through");
            }
            css.push(';');
        }
        if let Some(family) = &self.family {
            css.push_str(&format!(
                "font-family:'{}';",
                family.replace(['\'', '"', ';', '\\'], "")
            ));
        }
        if let Some(size) = self.size {
            css.push_str(&format!("font-size:{size}px;"));
        }
        if let Some(height) = self.line_height {
            css.push_str(&format!("line-height:{height}px;"));
        }
        for (name, color) in [("color", self.color), ("background-color", self.background)] {
            if let Some([r, g, b, a]) = color {
                css.push_str(&format!(
                    "{name}:rgba({},{},{},{a});",
                    r * 255.0,
                    g * 255.0,
                    b * 255.0
                ));
            }
        }
        css
    }

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

fn flow_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol" | "li"
    )
}

fn block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "table"
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
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "aside"
            | "nav"
            | "figure"
            | "figcaption"
            | "dl"
            | "dt"
            | "dd"
            | "address"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_html_keeps_line_breaks_without_whitespace_styles() {
        use cosmic::iced::clipboard::mime::AsMimeTypes;

        let document = Document::from_html("<b>First</b><br><br>Second &amp; last").unwrap();
        let html = String::from_utf8(document.as_bytes(HTML).unwrap().into_owned()).unwrap();
        assert_eq!(html.matches("<p ").count(), 2);
        let normal = html.replace("white-space:pre-wrap", "white-space:normal");
        let received = Document::from_html(&normal).unwrap();
        assert_eq!(received.text, document.text);
        assert_eq!(received.runs, document.runs);
        assert_eq!(
            document.as_bytes(crate::transfer::TEXT).unwrap().as_ref(),
            document.text.as_bytes()
        );
        assert_eq!(
            html_text("First\r\n\r\nLast <line>"),
            "First<br><br>Last &lt;line&gt;"
        );
    }

    #[test]
    fn clipboard_line_breaks_survive_import_and_reopening() {
        for (html, expected) in [
            (
                "<p>First\nSecond\n\n<strong>Third\nFourth</strong></p>",
                "First\nSecond\n\nThird\nFourth\n",
            ),
            ("<p>First</p><p></p><p>Last</p>", "First\n\nLast\n"),
            (
                "<span style='white-space:pre-line'>First   part\n\nSecond</span>",
                "First part\n\nSecond",
            ),
            (
                "<span style='white-space:normal'>First\nSecond</span>",
                "First Second",
            ),
            (
                "<span style='display:block'><b>First</b></span><span style='display:block'>Second</span>",
                "First\nSecond\n",
            ),
            (
                "<section>First</section><article>Second</article>",
                "First\nSecond\n",
            ),
        ] {
            let document = Document::from_html(html).unwrap();
            assert_eq!(document.text, expected);
            assert_eq!(Document::from_html(&document.html()).unwrap(), document);
        }
    }

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

    #[test]
    fn exports_real_lists_and_paragraphs_without_literal_bullets() {
        let document = Document::from_html("<p>Before</p><ul><li>First <b>item</b></li><li>Second<ol start='3'><li>Nested</li></ol></li></ul><p>After</p>").unwrap();
        let html = document.clipboard_html();
        assert_eq!(html.matches("<ul ").count(), 1);
        assert_eq!(html.matches("<li ").count(), 3);
        assert_eq!(html.matches("<p ").count(), 2);
        assert!(html.contains("<ol start=\"3\""));
        assert!(!html.contains('•'));
        assert_eq!(Document::from_html(&html).unwrap(), document);
        assert_eq!(Document::from_html(&document.html()).unwrap(), document);
        assert_eq!(document.slice(0..document.text.len()), document);
        assert!(!html.contains("<br>"));
    }

    #[test]
    fn tables_keep_cells_headers_spans_and_selection_through_export() {
        let document = Document::from_html("<p>Before</p><table>\n<thead><tr><th>Name</th><th style='text-align:right'>Value</th></tr></thead>\n<tbody><tr><td rowspan='2'><p><i>سلام</i></p></td><td style='background-color:#ff0000'>One<br>Two</td></tr><tr><td>Three</td></tr><tr><td colspan='2'>End</td></tr></tbody></table><p>After</p>").unwrap();
        assert_eq!(
            document.text,
            "Before\nName\tValue\nسلام\tOne\nTwo\nThree\nEnd\nAfter\n"
        );
        let table = &document.tables[0];
        assert_eq!(table.rows.len(), 4);
        assert!(table.rows[0][0].header);
        assert_eq!(table.rows[0][1].alignment, Alignment::Right);
        assert_eq!(table.rows[1][0].rows, 2);
        assert_eq!(table.rows[3][0].columns, 2);
        assert_eq!(table.rows[1][1].background, Some([1.0, 0.0, 0.0, 1.0]));
        for html in [document.html(), document.clipboard_html()] {
            let received = Document::from_html(&html).unwrap();
            assert_eq!(received.text, document.text);
            assert_eq!(received.tables, document.tables);
            assert_eq!(received.runs, document.runs);
        }
        let selected = document.slice(table.range.clone());
        assert_eq!(selected.tables[0].range, 0..selected.text.len());
        assert_eq!(
            Document::from_html(&selected.clipboard_html())
                .unwrap()
                .text,
            selected.text
        );
        let partial =
            document.slice(table.rows[0][0].range.start + 1..table.rows[0][1].range.end - 1);
        assert_eq!(partial.text, "ame\tValu");
        assert_eq!(
            Document::from_html(&partial.html()).unwrap().text,
            partial.text
        );
        let joined = Document::join(vec![Document::plain("Prefix"), document.clone()]);
        let received = Document::from_html(&joined.clipboard_html()).unwrap();
        assert_eq!(received.text, joined.text);
        assert_eq!(received.tables, joined.tables);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("table.html");
        std::fs::write(&path, document.html()).unwrap();
        assert_eq!(
            Document::read(&path, &document.text).unwrap().tables,
            document.tables
        );
        let caption = Document::from_html(
            "<table><caption>Heading</caption><tr><td><p>Cell</p></td></tr></table>",
        )
        .unwrap();
        assert_eq!(caption.text, "Heading\nCell\n");
        assert_eq!(
            Document::from_html(&caption.html()).unwrap().text,
            caption.text
        );
        assert_eq!(caption.tables.len(), 1);
    }
}
