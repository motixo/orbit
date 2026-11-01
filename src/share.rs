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
    pub name: String,
    pub text: Option<String>,
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
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let text = item.text.or_else(|| {
                crate::transfer::is_text_file(&path)
                    .then(|| {
                        crate::transfer::Text::File(path.clone())
                            .contents()
                            .map(|text| text.into_owned())
                    })
                    .flatten()
            });
            files.push(SharedFile { path, name, text });
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
    pub receive: bool,
    pub addresses: Vec<crate::phone::Address>,
    pub address: usize,
    pub qr_busy: bool,
    pub session: Option<Arc<crate::phone::Session>>,
    pub qr: Option<widget::qr_code::Data>,
    pub generation: u64,
    pub connected: bool,
}

impl Share {
    pub fn open(receive: bool) -> (Self, Task<cosmic::Action<Message>>) {
        let (id, open) = cosmic::surface::action::app_window::<crate::app::Orbit>(
            |_| cosmic::surface::action::LiveSettings::default(),
            |_| window::Settings {
                size: iced::Size::new(380.0, 520.0),
                min_size: Some(iced::Size::new(340.0, 360.0)),
                decorations: false,
                transparent: true,
                platform_specific: window::settings::PlatformSpecific {
                    application_id: "io.github.motixo.Orbit.Share".into(),
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
                receive,
                addresses: Vec::new(),
                address: 0,
                qr_busy: false,
                session: None,
                qr: None,
                generation: 0,
                connected: false,
            },
            cosmic::task::message(cosmic::Action::Surface(open)),
        )
    }

    pub fn view(&self) -> Element<'_, Message> {
        let ready = self.bundle.is_some() && !self.busy && !self.qr_busy;
        let mut controls = Vec::new();
        if let (Some(session), Some(qr)) = (&self.session, &self.qr) {
            controls.push(
                widget::container(crate::orbit_icon::binary())
                    .center_x(Length::Fill)
                    .into(),
            );
            if self.addresses.len() > 1 {
                controls.push(
                    widget::dropdown(&self.addresses, Some(self.address), Message::QrAddress)
                        .width(Length::Fill)
                        .into(),
                );
            }
            controls.push(
                widget::container(iced::widget::themer(
                    Some(iced::Theme::Light),
                    widget::qr_code::QRCode::<iced::Theme>::new(qr)
                        .total_size(240)
                        .style(|_| widget::qr_code::Style {
                            cell: iced::Color::BLACK,
                            background: iced::Color::WHITE,
                        }),
                ))
                .center_x(Length::Fill)
                .padding(8)
                .into(),
            );
            controls.push(
                widget::text::caption(if self.connected {
                    fl!("share-connected")
                } else {
                    fl!("share-waiting")
                })
                .into(),
            );
            controls.push(widget::text::caption(fl!("share-network-hint")).into());
            controls.push(
                widget::selectable_text::caption(&session.url)
                    .width(Length::Fill)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .into(),
            );
            controls.push(
                widget::row::with_children(vec![
                    widget::button::standard(fl!("copy-link"))
                        .on_press(Message::CopyShareLink)
                        .into(),
                    widget::button::standard(fl!("stop-sharing"))
                        .on_press(Message::StopQr)
                        .into(),
                ])
                .spacing(cosmic::theme::spacing().space_xxs)
                .into(),
            );
        } else {
            controls.push(
                widget::button::suggested(fl!("share-qr"))
                    .width(Length::Fill)
                    .on_press_maybe(ready.then_some(Message::StartQr))
                    .into(),
            );
        }
        if !self.receive && self.session.is_none() {
            controls.push(widget::divider::horizontal::default().into());
            controls.push(widget::text::heading(fl!("kde-connect")).into());
            match &self.devices {
                Some(Ok(devices)) if !devices.is_empty() => {
                    for device in devices {
                        controls.push(
                            widget::button::standard(&device.name)
                                .width(Length::Fill)
                                .on_press_maybe(
                                    ready.then(|| Message::SendToDevice(device.id.clone())),
                                )
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
        }
        if self.bundle.is_none() && self.status.is_empty() || self.busy || self.qr_busy {
            controls.push(widget::indeterminate_circular().size(24.0).into());
        }
        if !self.status.is_empty() {
            controls.push(widget::text::body(&self.status).into());
        }
        let content = widget::column::with_children(vec![
            widget::header_bar()
                .title(if self.receive {
                    fl!("receive")
                } else {
                    fl!("share")
                })
                .on_drag(Message::DragShare)
                .on_close(Message::CloseShare)
                .into(),
            widget::scrollable(
                widget::column::with_children(controls)
                    .width(Length::Fill)
                    .spacing(cosmic::theme::spacing().space_s)
                    .padding(cosmic::theme::spacing().space_s),
            )
            .width(Length::Fill)
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
