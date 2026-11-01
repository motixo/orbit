// SPDX-License-Identifier: GPL-3.0-only

use crate::{app::Message, fl, transfer::Transfer};
use cosmic::{
    iced::{self, Length, window},
    prelude::*,
    widget::{self, text_editor},
};
use std::{path::Path, sync::Arc};

const LOADER_SIZE: f32 = 32.0;

#[derive(Clone, Debug)]
pub enum Content {
    Image(widget::image::Handle),
    Text(text_editor::Content),
    Unavailable,
}

impl Content {
    pub fn interact(&mut self, action: text_editor::Action) {
        if !action.is_edit()
            && let Self::Text(text) = self
        {
            text.perform(action);
        }
    }
}

pub struct Preview {
    pub id: window::Id,
    name: String,
    pub content: Option<Content>,
    _lease: Arc<Transfer>,
}

impl Preview {
    pub fn open(
        path: std::path::PathBuf,
        lease: Arc<Transfer>,
    ) -> (Self, Task<cosmic::Action<Message>>) {
        let (id, open) = cosmic::surface::action::app_window::<crate::app::Orbit>(
            |_| cosmic::surface::action::LiveSettings::default(),
            |_| window::Settings {
                size: iced::Size::new(640.0, 480.0),
                min_size: Some(iced::Size::new(320.0, 240.0)),
                decorations: false,
                transparent: true,
                platform_specific: window::settings::PlatformSpecific {
                    application_id: "io.github.motixo.Orbit.Preview".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        (
            Self {
                id,
                name,
                content: None,
                _lease: lease,
            },
            cosmic::task::message(cosmic::Action::Surface(open)),
        )
    }

    pub fn view(&self) -> Element<'_, Message> {
        let content: Element<'_, Message> = match &self.content {
            Some(Content::Image(image)) => widget::image(image.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::Contain)
                .into(),
            Some(Content::Text(text)) => {
                let id = self.id;
                let editor = text_editor::text_editor(text)
                    .height(Length::Fill)
                    .font(cosmic::font::default())
                    .size(14.0)
                    .line_height(iced::advanced::text::LineHeight::Absolute(21.0.into()))
                    .padding(0)
                    .context_menu(false)
                    .on_action(move |action| Message::PreviewText(id, action))
                    .key_binding(|key| {
                        use text_editor::Binding;
                        match Binding::from_key_press(key) {
                            binding @ Some(
                                Binding::Copy
                                | Binding::SelectAll
                                | Binding::Move(_)
                                | Binding::Select(_)
                                | Binding::SelectWord
                                | Binding::SelectLine
                                | Binding::Unfocus,
                            ) => binding,
                            _ => None,
                        }
                    })
                    .style(|theme, status| text_editor::Style {
                        background: iced::Color::TRANSPARENT.into(),
                        border: iced::Border::default(),
                        value: theme.cosmic().background(theme.transparent).on.into(),
                        ..<cosmic::Theme as text_editor::Catalog>::style(
                            theme,
                            &Default::default(),
                            status,
                        )
                    });
                let menu = [
                    (
                        fl!("copy"),
                        text.selection().map(|_| Message::CopyPreview(id)),
                    ),
                    (
                        fl!("select-all"),
                        Some(Message::PreviewText(id, text_editor::Action::SelectAll)),
                    ),
                ]
                .into_iter()
                .map(|(label, message)| {
                    let button: Element<'static, Message> =
                        widget::menu::menu_button(vec![widget::text(label).into()])
                            .on_press_maybe(message)
                            .into();
                    widget::menu::Tree::from(button)
                })
                .collect();
                widget::context_menu(editor, Some(menu))
                    .window_id(id)
                    .on_surface_action(Message::Surface)
                    .into()
            }
            Some(Content::Unavailable) => widget::text::body(fl!("preview-unavailable")).into(),
            None => iced::widget::responsive(|size| {
                iced::widget::pin(widget::indeterminate_circular().size(LOADER_SIZE))
                    .position(iced::Point::new(
                        ((size.width - LOADER_SIZE) / 2.0).floor().max(0.0),
                        ((size.height - LOADER_SIZE) / 2.0).floor().max(0.0),
                    ))
                    .into()
            })
            .into(),
        };
        let content = widget::column::with_children(vec![
            widget::header_bar()
                .title(&self.name)
                .on_drag(Message::DragPreview)
                .on_close(Message::ClosePreview)
                .into(),
            widget::container(content)
                .padding(cosmic::theme::spacing().space_s)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into(),
        ])
        .height(Length::Fill);
        widget::container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(1)
            .clip(true)
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                widget::container::Style {
                    background: Some(cosmic.background(theme.transparent).base.into()),
                    border: iced::Border {
                        color: cosmic.bg_divider().into(),
                        width: 1.0,
                        radius: cosmic
                            .radius_s()
                            .map(|radius| if radius < 4.0 { radius } else { radius + 4.0 })
                            .into(),
                    },
                    ..Default::default()
                }
            }))
            .into()
    }
}

pub fn load(path: &Path, text: Option<String>) -> Content {
    if let Some(text) = text {
        return Content::Text(text_editor::Content::with_text(&text));
    }
    if crate::thumbnail::kind(path).is_some() {
        return crate::thumbnail::generate(path, 1024)
            .map(Content::Image)
            .unwrap_or(Content::Unavailable);
    }
    crate::transfer::Text::File(path.to_owned())
        .contents()
        .map(|text| Content::Text(text_editor::Content::with_text(&text)))
        .unwrap_or(Content::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_uses_snippet_content_and_rejects_binary_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snippet.txt");
        std::fs::write(&path, "hello").unwrap();
        assert!(matches!(load(&path, None), Content::Text(text) if text.text() == "hello"));
        assert!(
            matches!(load(&path, Some("https://example.com".into())), Content::Text(text) if text.text() == "https://example.com")
        );
        std::fs::write(&path, [0, 1, 2]).unwrap();
        assert!(matches!(load(&path, None), Content::Unavailable));
    }

    #[test]
    fn preview_selects_multiple_lines_without_editing() {
        let original = "first line\nخط دوم\nlast line";
        let mut content = Content::Text(text_editor::Content::with_text(original));
        content.interact(text_editor::Action::Move(
            text_editor::Motion::DocumentStart,
        ));
        content.interact(text_editor::Action::Select(
            text_editor::Motion::DocumentEnd,
        ));
        content.interact(text_editor::Action::Edit(text_editor::Edit::Delete));
        content.interact(text_editor::Action::Edit(text_editor::Edit::Insert('x')));
        let Content::Text(text) = content else {
            panic!("Expected text preview");
        };
        assert_eq!(text.selection().as_deref(), Some(original));
        assert_eq!(text.text(), original);
    }
}
