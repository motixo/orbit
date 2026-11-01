// SPDX-License-Identifier: GPL-3.0-only

use super::{Alignment, Document, Preview, Style, Table};
use crate::app::Message;
use cosmic::iced::advanced::Renderer as _;
use cosmic::{
    Element, Renderer, Theme,
    iced::{
        self, Event, Length, Point, Rectangle, Size, Vector,
        advanced::{
            Clipboard, Layout, Shell, Widget, layout, mouse, renderer,
            text::{self, Paragraph, Renderer as _},
            widget::{Tree, tree},
        },
        keyboard, window,
    },
    widget,
};
use std::{ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;

type NativeParagraph = <Renderer as text::Renderer>::Paragraph;

pub(super) fn view(preview: &Preview, id: window::Id) -> Element<'_, Message> {
    widget::scrollable(Element::new(View { preview, id }))
        .height(Length::Fill)
        .into()
}

struct View<'a> {
    preview: &'a Preview,
    id: window::Id,
}

struct Line {
    range: Range<usize>,
    top: f32,
    left: f32,
    width: f32,
    height: f32,
    paragraph: NativeParagraph,
    styles: Vec<Style>,
}

struct CellBounds {
    bounds: Rectangle,
    lines: Range<usize>,
    images: Range<usize>,
    header: bool,
    background: Option<[f32; 4]>,
}

struct ImageBounds {
    range: Range<usize>,
    bounds: Rectangle,
    handle: widget::icon::Handle,
}

#[derive(Default)]
struct State {
    document: Option<Arc<super::Document>>,
    width: f32,
    lines: Vec<Line>,
    cells: Vec<CellBounds>,
    images: Vec<ImageBounds>,
    height: f32,
    focused: bool,
    dragging: bool,
    anchor: usize,
    modifiers: keyboard::Modifiers,
    click: Option<mouse::Click>,
}

impl State {
    fn hit(&self, point: Point) -> usize {
        if let Some(image) = self
            .images
            .iter()
            .find(|image| image.bounds.contains(point))
        {
            return if point.x < image.bounds.center_x() {
                image.range.start
            } else {
                image.range.end
            };
        }
        let lines = self
            .cells
            .iter()
            .find(|cell| cell.bounds.contains(point))
            .map(|cell| &self.lines[cell.lines.clone()])
            .unwrap_or(&self.lines);
        let distance = |line: &Line| {
            let dx = (line.left - point.x)
                .max(0.0)
                .max(point.x - line.left - line.width);
            let dy = (line.top - point.y)
                .max(0.0)
                .max(point.y - line.top - line.height);
            dx * dx + dy * dy
        };
        let Some(line) = lines
            .iter()
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        else {
            return 0;
        };
        let index = line
            .paragraph
            .hit_test(Point::new(
                (point.x - line.left).max(0.0),
                (point.y - line.top).max(0.0),
            ))
            .map(|hit| hit.cursor())
            .unwrap_or(line.range.len());
        line.range.start + index.min(line.range.len())
    }

    fn line_at(&self, index: usize) -> Option<&Line> {
        self.lines
            .iter()
            .find(|line| index <= line.range.end)
            .or(self.lines.last())
    }

    fn append(
        &mut self,
        preview: &Preview,
        range: Range<usize>,
        bounds: Rectangle,
        alignment: Option<Alignment>,
        last: bool,
    ) -> f32 {
        let mut offset = range.start;
        let mut top = bounds.y;
        for (image, content) in preview.document.images.iter().zip(&preview.images) {
            let Some((handle, size)) = content else {
                continue;
            };
            if image.range.start < range.start || image.range.end > range.end {
                continue;
            }
            top += self.append_text(
                &preview.document,
                offset..image.range.start,
                Rectangle { y: top, ..bounds },
                alignment,
                false,
            );
            let scale = (bounds.width / size.width).min(1.0);
            let size = Size::new(size.width * scale, size.height * scale);
            self.images.push(ImageBounds {
                range: image.range.clone(),
                bounds: Rectangle::new(Point::new(bounds.x, top), size),
                handle: handle.clone(),
            });
            top += size.height + 8.0;
            offset = image.range.end;
        }
        top += self.append_text(
            &preview.document,
            offset..range.end,
            Rectangle { y: top, ..bounds },
            alignment,
            last,
        );
        top - bounds.y
    }

    fn append_text(
        &mut self,
        document: &Document,
        range: Range<usize>,
        bounds: Rectangle,
        alignment: Option<Alignment>,
        last: bool,
    ) -> f32 {
        if range.is_empty() && !last {
            return 0.0;
        }
        let mut offset = range.start;
        let mut top = bounds.y;
        let mut lines = document.text[range.clone()].split('\n').peekable();
        while let Some(line) = lines.next() {
            if line.is_empty() && lines.peek().is_none() && !last {
                break;
            }
            if top > 0.0
                && document.blocks[document
                    .blocks
                    .partition_point(|block| block.range.start < offset)..]
                    .iter()
                    .take_while(|block| block.range.start == offset)
                    .any(|block| {
                        matches!(
                            block.tag.as_str(),
                            "p" | "div"
                                | "ul"
                                | "ol"
                                | "blockquote"
                                | "h1"
                                | "h2"
                                | "h3"
                                | "h4"
                                | "h5"
                                | "h6"
                        )
                    })
            {
                top += 8.0;
            }
            let range = offset..offset + line.len();
            let left = bounds.x;
            let width = bounds.width.max(1.0);
            let mut styles = Vec::new();
            let first = document
                .runs
                .partition_point(|run| run.range.end <= range.start);
            let spans = document.runs[first..]
                .iter()
                .take_while(|run| run.range.start < range.end)
                .filter_map(|run| {
                    let start = run.range.start.max(range.start);
                    let end = run.range.end.min(range.end);
                    if start >= end {
                        return None;
                    }
                    styles.push(run.style.clone());
                    let mut span: text::Span<'_, ()> =
                        text::Span::new(&document.text[start..end]).font(run.style.font());
                    span.size = run.style.size.map(iced::Pixels);
                    span.line_height = run
                        .style
                        .line_height
                        .map(|height| text::LineHeight::Absolute(height.into()));
                    span.color = run.style.color.map(color);
                    Some(span)
                })
                .collect::<Vec<_>>();
            let paragraph = NativeParagraph::with_spans(text::Text {
                content: &spans,
                bounds: Size::new(width, f32::INFINITY),
                size: 14.0.into(),
                line_height: text::LineHeight::Relative(1.5),
                font: cosmic::font::default(),
                align_x: match alignment {
                    None => text::Alignment::Default,
                    Some(Alignment::Left) => text::Alignment::Left,
                    Some(Alignment::Center) => text::Alignment::Center,
                    Some(Alignment::Right) => text::Alignment::Right,
                },
                align_y: iced::alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::WordOrGlyph,
                ellipsize: text::Ellipsize::None,
            });
            let height = paragraph.min_height().max(21.0);
            self.lines.push(Line {
                range,
                top,
                left,
                width,
                height,
                paragraph,
                styles,
            });
            top += height;
            offset += line.len() + 1;
        }
        top - bounds.y
    }

    fn table(&mut self, preview: &Preview, table: &Table, top: f32, width: f32) -> f32 {
        let mut occupied: Vec<usize> = Vec::new();
        let mut placements = Vec::new();
        for (row_index, row) in table.rows.iter().enumerate() {
            let mut column = 0;
            for cell in row {
                while occupied
                    .iter()
                    .skip(column)
                    .take(cell.columns)
                    .any(|until| *until > row_index)
                {
                    column += 1;
                }
                occupied.resize(occupied.len().max(column + cell.columns), 0);
                for until in &mut occupied[column..column + cell.columns] {
                    *until = row_index + cell.rows;
                }
                placements.push((row_index, column, cell));
                column += cell.columns;
            }
        }
        let column_width = width / occupied.len().max(1) as f32;
        let mut heights = vec![34.0_f32; table.rows.len()];
        let first_cell = self.cells.len();
        for (row, column, cell) in &placements {
            let left = *column as f32 * column_width;
            let cell_width = cell.columns as f32 * column_width;
            let padding = 6.0_f32.min(cell_width / 4.0);
            let first_line = self.lines.len();
            let first_image = self.images.len();
            let height = self.append(
                preview,
                cell.range.clone(),
                Rectangle::new(
                    Point::new(left + padding, 0.0),
                    Size::new((cell_width - padding * 2.0).max(1.0), 0.0),
                ),
                Some(cell.alignment),
                true,
            ) + 12.0;
            let end_row = (*row + cell.rows).min(heights.len());
            let available: f32 = heights[*row..end_row].iter().sum();
            if height > available {
                heights[end_row - 1] += height - available;
            }
            self.cells.push(CellBounds {
                bounds: Rectangle::new(Point::new(left, 0.0), Size::new(cell_width, 0.0)),
                lines: first_line..self.lines.len(),
                images: first_image..self.images.len(),
                header: cell.header,
                background: cell.background,
            });
        }
        let mut positions = vec![top];
        for height in &heights {
            positions.push(positions.last().unwrap() + height);
        }
        for ((row, _, cell), bounds) in placements.iter().zip(&mut self.cells[first_cell..]) {
            bounds.bounds.y = positions[*row];
            bounds.bounds.height =
                positions[(*row + cell.rows).min(heights.len())] - positions[*row];
            for line in &mut self.lines[bounds.lines.clone()] {
                line.top += bounds.bounds.y + 6.0;
            }
            for image in &mut self.images[bounds.images.clone()] {
                image.bounds.y += bounds.bounds.y + 6.0;
            }
        }
        positions.last().copied().unwrap_or(top)
    }
}

impl Widget<Message, Theme, Renderer> for View<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let width = limits.max().width.max(1.0);
        let document = &self.preview.document;
        if state.width != width
            || state
                .document
                .as_ref()
                .is_none_or(|previous| !Arc::ptr_eq(previous, document))
        {
            state.width = width;
            state.document = Some(document.clone());
            state.lines.clear();
            state.cells.clear();
            state.images.clear();
            let mut offset = 0;
            let mut top = 0.0;
            for table in &document.tables {
                top += state.append(
                    self.preview,
                    offset..table.range.start,
                    Rectangle::new(Point::new(0.0, top), Size::new(width, 0.0)),
                    None,
                    false,
                );
                top = state.table(self.preview, table, top, width);
                offset = table.range.end;
                if document.text.as_bytes().get(offset) == Some(&b'\n') {
                    offset += 1;
                }
            }
            top += state.append(
                self.preview,
                offset..document.text.len(),
                Rectangle::new(Point::new(0.0, top), Size::new(width, 0.0)),
                None,
                true,
            );
            state.height = top.max(21.0);
        }
        let height = state.height;
        layout::Node::new(limits.resolve(Length::Fill, Length::Shrink, Size::new(width, height)))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let Some(clip) = layout.bounds().intersection(viewport) else {
            return;
        };
        let selected = self.preview.selection.start.min(self.preview.selection.end)
            ..self.preview.selection.start.max(self.preview.selection.end);
        renderer.with_layer(clip, |renderer| {
            for cell in &state.cells {
                let bounds = cell.bounds + (layout.position() - Point::ORIGIN);
                if !bounds.intersects(&clip) {
                    continue;
                }
                renderer.fill_quad(
                    renderer::Quad {
                        bounds,
                        border: iced::Border {
                            width: 1.0,
                            color: theme.current_container().divider.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    cell.background.map(color).unwrap_or_else(|| {
                        if cell.header {
                            theme.current_container().component.base.into()
                        } else {
                            iced::Color::TRANSPARENT
                        }
                    }),
                );
            }
            for line in &state.lines {
                let origin = layout.position() + Vector::new(line.left, line.top);
                if !Rectangle::new(origin, Size::new(line.width, line.height)).intersects(&clip) {
                    continue;
                }
                let translation = origin - Point::ORIGIN;
                for (index, style) in line.styles.iter().enumerate() {
                    if let Some(background) = style.background {
                        for bounds in line.paragraph.span_bounds(index) {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds: bounds + translation,
                                    ..Default::default()
                                },
                                color(background),
                            );
                        }
                    }
                }
                let start = selected.start.max(line.range.start);
                let end = selected.end.min(line.range.end);
                if start < end {
                    for bounds in line.paragraph.highlight(
                        0,
                        (start - line.range.start, text::Affinity::After),
                        (end - line.range.start, text::Affinity::Before),
                    ) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: bounds + translation,
                                ..Default::default()
                            },
                            iced::Color::from(theme.cosmic().accent.base).scale_alpha(0.35),
                        );
                    }
                }
                renderer.fill_paragraph(&line.paragraph, origin, defaults.text_color, clip);
                for (index, style) in line.styles.iter().enumerate() {
                    if !style.underline && !style.strike {
                        continue;
                    }
                    let size = style.size.unwrap_or(14.0);
                    let baseline = size + (style.line_height.unwrap_or(size * 1.5) - size) / 2.0;
                    for bounds in line.paragraph.span_bounds(index) {
                        for enabled in [
                            (style.underline, baseline),
                            (style.strike, baseline - size / 2.0),
                        ] {
                            if enabled.0 {
                                renderer.fill_quad(
                                    renderer::Quad {
                                        bounds: Rectangle::new(
                                            bounds.position()
                                                + translation
                                                + Vector::new(0.0, enabled.1),
                                            Size::new(bounds.width, 1.0),
                                        ),
                                        ..Default::default()
                                    },
                                    style.color.map(color).unwrap_or(defaults.text_color),
                                );
                            }
                        }
                    }
                }
            }
            for image in &state.images {
                let bounds = image.bounds + (layout.position() - Point::ORIGIN);
                if !bounds.intersects(&clip) {
                    continue;
                }
                widget::icon::draw(renderer, &image.handle, bounds);
                if selected.start < image.range.end && selected.end > image.range.start {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds,
                            ..Default::default()
                        },
                        iced::Color::from(theme.cosmic().accent.base).scale_alpha(0.35),
                    );
                }
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let text = &self.preview.document.text;
        let selection = &self.preview.selection;
        let publish = |range, shell: &mut Shell<'_, Message>| {
            shell.publish(Message::PreviewSelection(self.id, range));
            shell.capture_event();
        };
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if cursor.is_over(layout.bounds()) && cursor.is_over(*viewport) {
                    state.focused = true;
                    state.dragging = true;
                    let position = cursor.position().unwrap();
                    let index = state.hit(Point::new(
                        position.x - layout.bounds().x,
                        position.y - layout.bounds().y,
                    ));
                    let click =
                        mouse::Click::new(position, mouse::Button::Left, state.click.take());
                    let range = match click.kind() {
                        mouse::click::Kind::Single => {
                            if state.modifiers.shift() {
                                selection.start..index
                            } else {
                                index..index
                            }
                        }
                        mouse::click::Kind::Double => text
                            .unicode_word_indices()
                            .find(|(start, word)| *start <= index && index < start + word.len())
                            .map(|(start, word)| start..start + word.len())
                            .unwrap_or(index..index),
                        mouse::click::Kind::Triple => state
                            .line_at(index)
                            .map(|line| line.range.clone())
                            .unwrap_or(index..index),
                    };
                    state.anchor = range.start;
                    state.click = Some(click);
                    publish(range, shell);
                } else {
                    state.focused = false;
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                if cursor.is_over(layout.bounds()) {
                    state.focused = true;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging = false
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
                let index = state.hit(Point::new(
                    position.x - layout.bounds().x,
                    position.y - layout.bounds().y,
                ));
                publish(state.anchor..index, shell);
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) if state.focused => {
                if modifiers.command() {
                    match key.to_latin(*physical_key) {
                        Some('c') => {
                            shell.publish(Message::CopyPreview(self.id));
                            shell.capture_event();
                            return;
                        }
                        Some('a') => {
                            publish(0..text.len(), shell);
                            return;
                        }
                        _ => (),
                    }
                }
                let index = selection.end;
                let line = state.line_at(index);
                let target = match key.as_ref() {
                    keyboard::Key::Named(keyboard::key::Named::ArrowLeft) => Some(
                        text[..index]
                            .grapheme_indices(true)
                            .next_back()
                            .map(|(start, _)| start)
                            .unwrap_or(0),
                    ),
                    keyboard::Key::Named(keyboard::key::Named::ArrowRight) => Some(
                        text[index..]
                            .graphemes(true)
                            .next()
                            .map(|grapheme| index + grapheme.len())
                            .unwrap_or(text.len()),
                    ),
                    keyboard::Key::Named(keyboard::key::Named::Home) => {
                        Some(if modifiers.command() {
                            0
                        } else {
                            line.map(|line| line.range.start).unwrap_or(0)
                        })
                    }
                    keyboard::Key::Named(keyboard::key::Named::End) => {
                        Some(if modifiers.command() {
                            text.len()
                        } else {
                            line.map(|line| line.range.end).unwrap_or(text.len())
                        })
                    }
                    keyboard::Key::Named(
                        keyboard::key::Named::ArrowUp | keyboard::key::Named::ArrowDown,
                    ) => line.and_then(|line| {
                        let point = line.paragraph.cursor_position(
                            0,
                            index - line.range.start,
                            text::Affinity::After,
                        )?;
                        let delta = if matches!(
                            key.as_ref(),
                            keyboard::Key::Named(keyboard::key::Named::ArrowUp)
                        ) {
                            -21.0
                        } else {
                            21.0
                        };
                        let mut target =
                            Point::new(point.x + line.left, point.y + line.top + delta);
                        if target.y < line.top || target.y >= line.top + line.height {
                            let adjacent = state
                                .lines
                                .iter()
                                .filter(|candidate| {
                                    target.x >= candidate.left
                                        && target.x <= candidate.left + candidate.width
                                        && if delta < 0.0 {
                                            candidate.top < line.top
                                        } else {
                                            candidate.top >= line.top + line.height
                                        }
                                })
                                .min_by(|a, b| {
                                    (a.top - line.top)
                                        .abs()
                                        .total_cmp(&(b.top - line.top).abs())
                                });
                            if let Some(adjacent) = adjacent {
                                target.y = if delta < 0.0 {
                                    adjacent.top + adjacent.height - 1.0
                                } else {
                                    adjacent.top + 1.0
                                };
                            }
                        }
                        Some(state.hit(target))
                    }),
                    _ => None,
                };
                if let Some(target) = target {
                    publish(
                        if modifiers.shift() {
                            selection.start..target
                        } else {
                            target..target
                        },
                        shell,
                    );
                }
            }
            Event::Window(window::Event::Unfocused) => state.dragging = false,
            _ => (),
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) && cursor.is_over(*viewport) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }

    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        _tree: &Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        let text: Element<'_, Message> = widget::text::body(&self.preview.document.text).into();
        text.as_widget()
            .a11y_nodes(layout, &Tree::new(text.as_widget()), cursor)
    }
}

fn color([r, g, b, a]: [f32; 4]) -> iced::Color {
    iced::Color::from_rgba(r, g, b, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_cells_share_columns_and_hit_their_own_text() {
        let document = Document::from_html(
            "<table><tr><th>A</th><th>B</th></tr><tr><td colspan='2'>Wide</td></tr></table>",
        )
        .unwrap();
        let mut state = State::default();
        let preview = Preview::new(document);
        let document = &preview.document;
        let height = state.table(&preview, &document.tables[0], 0.0, 300.0);
        assert_eq!(state.cells.len(), 3);
        assert_eq!(state.cells[0].bounds.width, 150.0);
        assert_eq!(state.cells[1].bounds.x, 150.0);
        assert_eq!(state.cells[2].bounds.width, 300.0);
        assert_eq!(state.cells[0].bounds.y, state.cells[1].bounds.y);
        assert_eq!(state.cells[2].bounds.y, state.cells[0].bounds.height);
        assert!(height >= 68.0);
        for (cell, bounds) in document.tables[0].rows.iter().flatten().zip(&state.cells) {
            let index = state.hit(bounds.bounds.position() + Vector::new(8.0, 8.0));
            assert!(index >= cell.range.start && index <= cell.range.end);
        }
    }

    #[test]
    fn image_preview_keeps_aspect_ratio_and_selection_offsets() {
        let document = Document::from_html("Before<img src='data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=' width='200' height='100'>After").unwrap();
        let preview = Preview::new(document);
        let mut state = State::default();
        let height = state.append(
            &preview,
            0..preview.document.text.len(),
            Rectangle::new(Point::ORIGIN, Size::new(100.0, 0.0)),
            None,
            true,
        );
        assert_eq!(state.images.len(), 1);
        let image = &state.images[0];
        assert_eq!(image.bounds.size(), Size::new(100.0, 50.0));
        assert_eq!(
            state.hit(image.bounds.position() + Vector::new(1.0, 1.0)),
            image.range.start
        );
        assert_eq!(
            state.hit(image.bounds.position() + Vector::new(99.0, 1.0)),
            image.range.end
        );
        assert!(height > image.bounds.height);
        assert!(state.lines.last().unwrap().top > image.bounds.y);
    }
}
