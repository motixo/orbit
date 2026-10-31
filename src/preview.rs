// SPDX-License-Identifier: GPL-3.0-only

use crate::{app::Message, fl, transfer::Transfer};
use cosmic::{
    iced::{self, Length, window},
    prelude::*,
    widget,
};
use std::{path::Path, sync::Arc};

#[derive(Clone, Debug)]
pub enum Content {
    Image(widget::image::Handle),
    Text(String),
    Unavailable,
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
                    application_id: <crate::app::Orbit as cosmic::Application>::APP_ID.into(),
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
                widget::scrollable(widget::text::body(text).width(Length::Fill))
                    .height(Length::Fill)
                    .into()
            }
            Some(Content::Unavailable) => widget::text::body(fl!("preview-unavailable")).into(),
            None => widget::indeterminate_circular()
                .cycle_duration(std::time::Duration::ZERO)
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
        return Content::Text(text);
    }
    if crate::thumbnail::kind(path).is_some() {
        return crate::thumbnail::generate(path, 1024)
            .map(Content::Image)
            .unwrap_or(Content::Unavailable);
    }
    crate::transfer::Text::File(path.to_owned())
        .contents()
        .map(|text| Content::Text(text.into_owned()))
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
        assert!(matches!(load(&path, None), Content::Text(text) if text == "hello"));
        assert!(
            matches!(load(&path, Some("https://example.com".into())), Content::Text(text) if text == "https://example.com")
        );
        std::fs::write(&path, [0, 1, 2]).unwrap();
        assert!(matches!(load(&path, None), Content::Unavailable));
    }
}
