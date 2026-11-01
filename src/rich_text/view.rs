// SPDX-License-Identifier: GPL-3.0-only

use super::{Preview, Style};
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
    height: f32,
    paragraph: NativeParagraph,
    styles: Vec<Style>,
}

#[derive(Default)]
struct State {
    document: Option<Arc<super::Document>>,
    width: f32,
    lines: Vec<Line>,
    focused: bool,
    dragging: bool,
    anchor: usize,
    modifiers: keyboard::Modifiers,
    click: Option<mouse::Click>,
}

impl State {
    fn hit(&self, point: Point) -> usize {
        let Some(line) = self
            .lines
            .iter()
            .find(|line| point.y < line.top + line.height)
            .or(self.lines.last())
        else {
            return 0;
        };
        let index = line
            .paragraph
            .hit_test(Point::new(point.x, (point.y - line.top).max(0.0)))
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
            let mut offset = 0;
            let mut top = 0.0;
            // Each paragraph has one logical line, so hit_test's byte offset stays unambiguous.
            for line in document.text.split('\n') {
                let range = offset..offset + line.len();
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
                    align_x: text::Alignment::Default,
                    align_y: iced::alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::WordOrGlyph,
                    ellipsize: text::Ellipsize::None,
                });
                let height = paragraph.min_height().max(21.0);
                state.lines.push(Line {
                    range,
                    top,
                    height,
                    paragraph,
                    styles,
                });
                top += height;
                offset += line.len() + 1;
            }
        }
        let height = state
            .lines
            .last()
            .map(|line| line.top + line.height)
            .unwrap_or(21.0);
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
            for line in &state.lines {
                let origin = layout.position() + Vector::new(0.0, line.top);
                if !Rectangle::new(origin, Size::new(state.width, line.height)).intersects(&clip) {
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
                        Some(state.hit(Point::new(point.x, point.y + line.top + delta)))
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
