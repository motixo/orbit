// SPDX-License-Identifier: GPL-3.0-only

use crate::{app::Message, fl, shelf::Item, transfer::Transfer};
use cosmic::{
    iced::{self, Length, window},
    prelude::*,
    widget,
};
use std::{path::PathBuf, sync::Arc};

#[derive(Debug)]
pub struct SharedFile {
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct Bundle {
    pub files: Vec<SharedFile>,
    transfer: Arc<Transfer>,
    archives: Vec<Arc<crate::cache::Directory>>,
}

impl Bundle {
    pub fn prepare(items: Vec<Item>, transfer: Arc<Transfer>) -> Result<Arc<Self>, String> {
        let mut files = Vec::new();
        let mut archives = Vec::new();
        for item in items {
            let mut path = item.path;
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                let archive =
                    crate::archive::create(&[path], &name, crate::archive::Format::Zip, || false)
                        .map_err(|e| e.to_string())?;
                path = archive.files.0[0].clone();
                archives.extend(archive.directory);
            }
            let metadata = path.metadata().map_err(|e| e.to_string())?;
            if !metadata.is_file() || path.to_str().is_none() {
                return Err("Unsupported file".into());
            }
            files.push(SharedFile { path });
        }
        if files.is_empty() {
            return Err("No files selected".into());
        }
        Ok(Arc::new(Self {
            files,
            transfer,
            archives,
        }))
    }

    pub fn hand_off(&self) {
        for directory in self.transfer.cached.iter().chain(&self.archives) {
            directory.hand_off();
        }
    }
}

pub struct Share {
    pub id: window::Id,
    pub bundle: Option<Arc<Bundle>>,
    pub devices: Option<Result<Vec<crate::kdeconnect::Device>, String>>,
    pub busy: bool,
    pub status: String,
}

impl Share {
    pub fn open() -> (Self, Task<cosmic::Action<Message>>) {
        let (id, open) = cosmic::surface::action::app_window::<crate::app::Orbit>(
            |_| cosmic::surface::action::LiveSettings::default(),
            |_| window::Settings {
                size: iced::Size::new(380.0, 420.0),
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
        (
            Self {
                id,
                bundle: None,
                devices: None,
                busy: false,
                status: String::new(),
            },
            cosmic::task::message(cosmic::Action::Surface(open)),
        )
    }

    pub fn view(&self) -> Element<'_, Message> {
        let ready = self.bundle.is_some() && !self.busy;
        let mut controls = vec![widget::text::heading(fl!("kde-connect")).into()];
        match &self.devices {
            Some(Ok(devices)) if !devices.is_empty() => {
                for device in devices {
                    controls.push(
                        widget::button::standard(&device.name)
                            .width(Length::Fill)
                            .on_press_maybe(ready.then(|| Message::SendToDevice(device.id.clone())))
                            .into(),
                    );
                }
            }
            Some(Ok(_)) => controls.push(widget::text::body(fl!("share-no-devices")).into()),
            Some(Err(_)) => controls.push(widget::text::body(fl!("share-no-service")).into()),
            None => controls.push(widget::indeterminate_circular().size(24.0).into()),
        }
        controls.push(
            widget::button::standard(fl!("refresh"))
                .on_press_maybe((!self.busy).then_some(Message::RefreshDevices))
                .into(),
        );
        if self.bundle.is_none() && self.status.is_empty() || self.busy {
            controls.push(widget::indeterminate_circular().size(24.0).into());
        }
        if !self.status.is_empty() {
            controls.push(widget::text::body(&self.status).into());
        }
        let content = widget::column::with_children(vec![
            widget::header_bar()
                .title(fl!("share"))
                .on_drag(Message::DragShare)
                .on_close(Message::CloseShare)
                .into(),
            widget::scrollable(
                widget::column::with_children(controls)
                    .spacing(cosmic::theme::spacing().space_s)
                    .padding(cosmic::theme::spacing().space_s),
            )
            .height(Length::Fill)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_folders_are_archived_and_text_keeps_its_content() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let text = directory.path().join("note.txt");
        std::fs::write(&text, "hello").unwrap();
        let mut shelf = crate::shelf::Shelf::default();
        shelf.add([folder.clone(), text.clone()]);
        let transfer = Arc::new(shelf.transfer(shelf.export_paths(), false));
        let bundle = Bundle::prepare(shelf.items.clone(), transfer).unwrap();
        assert_eq!(bundle.files[0].path.file_name().unwrap(), "folder.zip");
        assert_eq!(
            std::fs::read_to_string(&bundle.files[1].path).unwrap(),
            "hello"
        );
        assert!(folder.is_dir() && text.is_file());
    }
}
