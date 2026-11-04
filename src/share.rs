// SPDX-License-Identifier: GPL-3.0-only

use crate::{app::Message, fl, shelf::Item, transfer::Transfer};
use cosmic::{
    iced::{self, Length, window},
    prelude::*,
    widget,
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

const WINDOW_HEIGHT: f32 = 520.0;

#[derive(Clone, Debug)]
pub struct SharedFile {
    pub id: usize,
    pub path: PathBuf,
    pub name: String,
    pub title: String,
    pub text: Option<String>,
    pub origin: Option<crate::shelf::Sender>,
    pub(crate) source: PathBuf,
    pub(crate) order: u64,
    pub(crate) thumbnail: Arc<OnceLock<Option<Vec<u8>>>>,
    _archive: Option<Arc<crate::cache::Directory>>,
}

#[derive(Debug)]
pub struct Bundle {
    pub files: Vec<SharedFile>,
    next_id: usize,
    _transfer: Arc<Transfer>,
}

impl Bundle {
    pub fn prepare(
        items: Vec<Item>,
        transfer: Arc<Transfer>,
        previous: Option<&Self>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Arc<Self>, String> {
        let mut files = Vec::new();
        let mut next_id = previous.map_or(0, |bundle| bundle.next_id);
        for item in items {
            if cancelled() {
                return Err("Sharing stopped".into());
            }
            if let Some(file) = previous.and_then(|bundle| {
                bundle
                    .files
                    .iter()
                    .find(|file| file.source == item.path && file.order == item.order)
            }) {
                let mut file = file.clone();
                file.title = item.name;
                file.origin = item.origin;
                files.push(file);
                continue;
            }
            let source = item.path;
            let mut path = source.clone();
            let mut cached_archive = None;
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                match crate::archive::create(
                    &[path.clone()],
                    &name,
                    crate::archive::Format::Zip,
                    &cancelled,
                ) {
                    Ok(archive) => {
                        path = archive.files.0[0].clone();
                        cached_archive = archive.directory;
                    }
                    Err(error) if cancelled() => return Err(error.to_string()),
                    Err(error) => eprintln!("Could not share folder {name}: {error}"),
                }
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            files.push(SharedFile {
                id: next_id,
                path,
                name,
                title: item.name,
                text: item.text,
                origin: item.origin,
                thumbnail: Arc::new(OnceLock::new()),
                source,
                order: item.order,
                _archive: cached_archive,
            });
            next_id = next_id.checked_add(1).ok_or("Too many shared files")?;
        }
        Ok(Arc::new(Self {
            files,
            next_id,
            _transfer: transfer,
        }))
    }

    pub fn ordered(&self) -> Vec<&SharedFile> {
        let mut files: Vec<_> = self.files.iter().collect();
        files.sort_by_key(|file| std::cmp::Reverse(file.order));
        files
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Popup {
    Info,
    Devices,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Devices,
    Pair,
    Connected,
}

pub struct DeviceEdit {
    pub id: String,
    pub name: String,
    pub input: widget::Id,
}

#[derive(Clone, Debug)]
pub struct ReceivedFile {
    pub sender: String,
    pub order: u64,
    pub dragged: bool,
    pub cache: Arc<crate::cache::Directory>,
    metadata: std::fs::Metadata,
}

impl ReceivedFile {
    pub fn new(path: &std::path::Path, sender: String, order: u64) -> std::io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(std::io::Error::other("Not a received file"));
        }
        let cache = crate::cache::Directory::new("orbit-drop-")?;
        let snapshot = cache.path().join(path.file_name().unwrap());
        if std::fs::hard_link(path, &snapshot).is_err() {
            std::fs::copy(path, &snapshot)?;
        }
        Ok(Self {
            sender,
            order,
            dragged: false,
            cache,
            metadata: std::fs::symlink_metadata(path)?,
        })
    }

    fn matches(&self, current: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt;
        current.is_file()
            && current.dev() == self.metadata.dev()
            && current.ino() == self.metadata.ino()
            && current.ctime() == self.metadata.ctime()
            && current.ctime_nsec() == self.metadata.ctime_nsec()
    }

    pub fn export_path(&self, path: &std::path::Path) -> Option<PathBuf> {
        self.matches(&std::fs::symlink_metadata(path).ok()?)
            .then(|| self.cache.path().join(path.file_name().unwrap()))
    }

    pub fn remove_copy(&self, path: &std::path::Path) -> std::io::Result<()> {
        let current = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if self.matches(&current) {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

pub struct Share {
    pub id: window::Id,
    pub activation: Option<crate::tray::Session>,
    pub session_id: uuid::Uuid,
    pub cancelled: Arc<AtomicBool>,
    pub bundle: Option<Arc<Bundle>>,
    pub updating: bool,
    pub sources: Option<Vec<(PathBuf, u64, Option<crate::shelf::Sender>)>>,
    pub status: String,
    pub addresses: Vec<crate::device_share::Address>,
    pub address: usize,
    pub qr_busy: bool,
    pub qr_task: Option<iced::task::Handle>,
    pub session: Option<Arc<crate::device_share::Session>>,
    pub qr: Option<widget::qr_code::Data>,
    pub generation: u64,
    pub connected: iced::Animation<bool>,
    pub qr_visibility: iced::Animation<bool>,
    pub connected_devices: Vec<crate::device_share::ConnectedDevice>,
    pub received: HashMap<PathBuf, ReceivedFile>,
    pub received_sources: crate::device_share::ReceivedSources,
    pulse_started: std::time::Instant,
    pub pairings: Vec<crate::device_share::PairedDevice>,
    pub nearby: Vec<crate::device_share::NearbyDevice>,
    pub pair_requests: Vec<crate::device_share::PairRequest>,
    pub connecting: HashSet<String>,
    pub page: Page,
    pub forgotten: HashSet<String>,
    pub downloads: HashMap<uuid::Uuid, crate::device_share::TransferProgress>,
    pub uploads: HashMap<uuid::Uuid, crate::device_share::TransferProgress>,
    pub popup: Option<Popup>,
    pub device_edit: Option<DeviceEdit>,
    pub device_error: Option<String>,
    pub transfer_error: Option<String>,
}

impl Drop for Share {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Share {
    pub fn open() -> (Self, Task<cosmic::Action<Message>>) {
        let (id, open) = cosmic::surface::action::app_window::<crate::app::Orbit>(
            |_| cosmic::surface::action::LiveSettings::default(),
            |_| window::Settings {
                size: iced::Size::new(380.0, WINDOW_HEIGHT),
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
                activation: None,
                session_id: uuid::Uuid::new_v4(),
                cancelled: Arc::new(AtomicBool::new(false)),
                bundle: None,
                updating: false,
                sources: None,
                status: String::new(),
                addresses: Vec::new(),
                address: 0,
                qr_busy: false,
                qr_task: None,
                session: None,
                qr: None,
                generation: 0,
                connected: iced::Animation::new(false)
                    .duration(crate::orbit_icon::TRANSITION_DURATION)
                    .easing(iced::animation::Easing::EaseInOut),
                qr_visibility: iced::Animation::new(false)
                    .duration(crate::orbit_icon::TRANSITION_DURATION)
                    .easing(iced::animation::Easing::EaseInOut),
                connected_devices: Vec::new(),
                received: HashMap::new(),
                received_sources: Default::default(),
                pulse_started: std::time::Instant::now(),
                pairings: Vec::new(),
                nearby: Vec::new(),
                pair_requests: Vec::new(),
                connecting: HashSet::new(),
                page: Page::Devices,
                forgotten: HashSet::new(),
                downloads: HashMap::new(),
                uploads: HashMap::new(),
                popup: None,
                device_edit: None,
                device_error: None,
                transfer_error: None,
            },
            cosmic::task::message(cosmic::Action::Surface(open)),
        )
    }

    pub fn show_page(&mut self, page: Page, now: std::time::Instant) {
        self.page = page;
        self.popup = None;
        if page == Page::Connected {
            self.qr_visibility.go_mut(false, now);
        } else {
            self.qr_visibility = iced::Animation::new(page == Page::Pair)
                .duration(crate::orbit_icon::TRANSITION_DURATION)
                .easing(iced::animation::Easing::EaseInOut);
        }
    }

    pub fn set_connected(&mut self, connected: bool, now: std::time::Instant) {
        let was_connected = self.connected.value();
        self.connected.go_mut(connected, now);
        if !connected && (was_connected || self.page == Page::Connected) {
            self.show_page(Page::Devices, now);
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        iced::widget::responsive(|size| self.content((size.height / WINDOW_HEIGHT).min(1.0))).into()
    }

    fn content(&self, scale: f32) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let ready = self.bundle.is_some() && !self.qr_busy;
        let speed = self
            .downloads
            .values()
            .chain(self.uploads.values())
            .map(|progress| progress.speed)
            .reduce(|a, b| a + b);
        let heading = || {
            widget::container(widget::text::heading(if self.connected.value() {
                fl!("share-connected", count = self.connected_devices.len())
            } else {
                fl!("share-waiting")
            }))
            .center_x(Length::Fill)
            .align_top(Length::Fill)
            .padding(iced::Padding {
                top: f32::from(spacing.space_xl) * scale,
                ..iced::Padding::ZERO
            })
        };
        let mut controls = Vec::new();
        if self.page == Page::Devices {
            let height = 334.4 * scale;
            let devices = widget::container(self.devices())
                .width(Length::Fill)
                .max_width(300)
                .padding(spacing.space_s)
                .clip(true)
                .class(cosmic::theme::Container::custom(|theme| {
                    let cosmic = theme.cosmic();
                    widget::container::Style {
                        background: Some(
                            iced::Color::from(cosmic.background(false).base)
                                .scale_alpha(0.75)
                                .into(),
                        ),
                        border: iced::Border {
                            radius: cosmic.radius_s().into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                }));
            controls.push(
                iced::widget::Stack::new()
                    .push(
                        widget::container(crate::orbit_icon::binary(
                            self.connected.value(),
                            speed,
                            scale,
                        ))
                        .height(height * 1.3),
                    )
                    .push(
                        widget::container(devices)
                            .center_x(Length::Fill)
                            .center_y(Length::Fill)
                            .padding(iced::Padding {
                                top: f32::from(spacing.space_m),
                                ..iced::Padding::from([0, spacing.space_s])
                            }),
                    )
                    .push(heading())
                    .width(Length::Fill)
                    .into(),
            );
        } else if let Some(qr) = &self.qr {
            let downloading = transfer_progress(&self.downloads);
            let uploading = transfer_progress(&self.uploads);
            let visibility: f32 =
                self.qr_visibility
                    .interpolate(0.0, 1.0, std::time::Instant::now());
            let qr_offset = 334.4 * scale * visibility * 0.05;
            let mut animation = iced::widget::Stack::new()
                .push(
                    widget::container(crate::orbit_icon::binary(
                        self.connected.value(),
                        speed,
                        scale,
                    ))
                    .height((272.0 + 62.4 * visibility) * scale + qr_offset),
                )
                .width(Length::Fill);
            if visibility > 0.0 {
                animation = animation.push(iced::widget::responsive(move |size| {
                    let qr_size = (size.width - 64.0)
                        .min(size.height - qr_offset - 40.0)
                        .clamp(128.0, 232.0)
                        * visibility;
                    let code = widget::container(iced::widget::themer(
                        Some(iced::Theme::Light),
                        widget::qr_code::QRCode::<iced::Theme>::new(qr)
                            .total_size(qr_size)
                            .style(move |_| widget::qr_code::Style {
                                cell: iced::Color::BLACK.scale_alpha(visibility),
                                background: iced::Color::TRANSPARENT,
                            }),
                    ))
                    .padding(12.0 * visibility)
                    .class(cosmic::theme::Container::custom(move |theme| {
                        widget::container::Style {
                            background: Some(iced::Color::WHITE.scale_alpha(visibility).into()),
                            border: iced::Border {
                                radius: theme
                                    .cosmic()
                                    .radius_s()
                                    .map(|radius| radius * visibility)
                                    .into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    }));
                    widget::container(code)
                        .center_x(Length::Fill)
                        .align_bottom(Length::Fill)
                        .padding([0.0, 8.0 * visibility])
                        .into()
                }));
            }
            controls.push(animation.push(heading()).into());
            if let Some(progress) = downloading {
                controls.push(transfer_status(
                    progress,
                    crate::device_share::Direction::Download,
                ));
            }
            if let Some(progress) = uploading {
                controls.push(transfer_status(
                    progress,
                    crate::device_share::Direction::Upload,
                ));
            }
            if downloading.is_none()
                && uploading.is_none()
                && self.page == Page::Pair
                && self.session.is_some()
            {
                controls.push(
                    widget::container(
                        widget::button::standard(fl!("copy-link")).on_press(Message::CopyShareLink),
                    )
                    .center_x(Length::Fill)
                    .into(),
                );
            }
            if self.page == Page::Connected {
                for remote in &self.connected_devices {
                    let name = self
                        .pairings
                        .iter()
                        .find(|device| device.id == remote.pair_id)
                        .map_or(remote.name.as_str(), |device| device.name.as_str());
                    let detail = if name.is_empty() {
                        remote.ip.to_string()
                    } else {
                        format!("{} · {}", name, remote.ip)
                    };
                    let opacity = if remote.reconnecting {
                        0.35 + 0.2
                            * (self.pulse_started.elapsed().as_secs_f32() * std::f32::consts::TAU
                                / 1.6)
                                .cos()
                    } else {
                        0.55
                    };
                    controls.push(
                        widget::text::caption(detail)
                            .class(
                                iced::Color::from(
                                    cosmic::theme::active().cosmic().background(false).on,
                                )
                                .scale_alpha(opacity),
                            )
                            .width(Length::Fill)
                            .align_x(iced::alignment::Horizontal::Center)
                            .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                            .into(),
                    );
                }
            }
        } else {
            controls.push(
                widget::button::suggested(fl!("share-qr"))
                    .width(Length::Fill)
                    .on_press_maybe(ready.then_some(Message::StartQr))
                    .into(),
            );
        }
        if self.bundle.is_none() && self.status.is_empty() || self.qr_busy {
            controls.push(widget::indeterminate_circular().size(24.0).into());
        }
        if !self.status.is_empty() && self.session.is_none() {
            controls.push(widget::text::body(&self.status).into());
        }
        if self.page != Page::Devices
            && let Some(error) = &self.transfer_error
        {
            controls.push(
                widget::text::caption(error)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .into(),
            );
        }
        let mut header = widget::header_bar()
            .title(fl!("share"))
            .on_drag(Message::DragShare)
            .on_close(Message::CloseShare);
        if self.page != Page::Devices {
            header = header.start(
                widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                    .name(fl!("share-back"))
                    .on_press(Message::SharePage(Page::Devices)),
            );
        }
        if self.session.is_some() {
            header = header.start(self.info()).start(self.linked_devices());
        }
        let content = widget::column::with_children(vec![
            header.into(),
            widget::scrollable(
                widget::column::with_children(controls)
                    .width(Length::Fill)
                    .spacing(spacing.space_s)
                    .padding([
                        spacing.space_s,
                        if self.page != Page::Devices && self.qr.is_none() {
                            spacing.space_s
                        } else {
                            0
                        },
                    ]),
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

    fn devices(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut content =
            widget::column::with_capacity(self.nearby.len() + self.pairings.len() + 4)
                .width(Length::Fill)
                .spacing(spacing.space_s);
        for request in &self.pair_requests {
            let mut pair = widget::column::with_children(vec![
                widget::text::body(&request.name).into(),
                widget::text(&request.code).size(28).into(),
                widget::text::caption(fl!("share-verify")).into(),
            ])
            .spacing(spacing.space_s);
            if request.approved {
                pair = pair.push(widget::text::caption(fl!("share-confirming")));
            }
            pair = pair.push(
                widget::row::with_children(vec![
                    widget::button::standard(fl!("cancel"))
                        .on_press(Message::PairDecision(request.id.clone(), false))
                        .into(),
                    widget::button::suggested(fl!("share-pair"))
                        .on_press_maybe(
                            (!request.approved)
                                .then(|| Message::PairDecision(request.id.clone(), true)),
                        )
                        .into(),
                ])
                .spacing(spacing.space_xxs),
            );
            content = content.push(
                widget::container(pair)
                    .padding(spacing.space_s)
                    .width(Length::Fill)
                    .class(cosmic::theme::Container::Card),
            );
        }
        if let Some(progress) = transfer_progress(&self.downloads) {
            content = content.push(transfer_status(
                progress,
                crate::device_share::Direction::Download,
            ));
        }
        if let Some(progress) = transfer_progress(&self.uploads) {
            content = content.push(transfer_status(
                progress,
                crate::device_share::Direction::Upload,
            ));
        }
        if !self.pairings.is_empty() {
            content = content.push(widget::text::heading(fl!("linked-devices")));
            for device in &self.pairings {
                let connected = self
                    .connected_devices
                    .iter()
                    .any(|remote| remote.pair_id == device.id && !remote.reconnecting);
                let online = connected || self.nearby.iter().any(|remote| remote.id == device.id);
                let mut row = widget::row::with_children(vec![
                    status_dot(online),
                    widget::text::body(&device.name)
                        .width(Length::Fill)
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                        .into(),
                ])
                .spacing(spacing.space_xxs)
                .align_y(iced::Alignment::Center);
                if device.desktop {
                    let connecting = self.connecting.contains(&device.id);
                    row = row.push(
                        widget::button::standard(if connected {
                            fl!("share-disconnect")
                        } else if connecting {
                            fl!("cancel")
                        } else {
                            fl!("share-connect")
                        })
                        .on_press_maybe((online || connecting).then(
                            || {
                                if connected || connecting {
                                    Message::DisconnectDevice(device.id.clone())
                                } else {
                                    Message::ConnectDevice(device.id.clone())
                                }
                            },
                        )),
                    );
                    row = row.push(
                        widget::button::standard(fl!("share-open-orbit")).on_press_maybe(
                            (online && !connecting)
                                .then(|| Message::OpenRemoteOrbit(device.id.clone())),
                        ),
                    );
                } else if connected {
                    row = row.push(
                        widget::button::standard(fl!("share-disconnect"))
                            .on_press(Message::DisconnectDevice(device.id.clone())),
                    );
                }
                content = content.push(row);
            }
        }
        content = content.push(widget::text::heading(fl!("share-nearby")));
        let mut found = false;
        for device in &self.nearby {
            if self.pairings.iter().any(|paired| paired.id == device.id) {
                continue;
            }
            found = true;
            content = content.push(
                widget::row::with_children(vec![
                    status_dot(true),
                    widget::column::with_children(vec![
                        widget::text::body(&device.name).into(),
                        widget::text::caption(device.ip.to_string()).into(),
                    ])
                    .width(Length::Fill)
                    .into(),
                    widget::button::standard(if self.connecting.contains(&device.id) {
                        fl!("cancel")
                    } else {
                        fl!("share-pair")
                    })
                    .on_press_maybe(self.pair_requests.is_empty().then(|| {
                        if self.connecting.contains(&device.id) {
                            Message::DisconnectDevice(device.id.clone())
                        } else {
                            Message::ConnectDevice(device.id.clone())
                        }
                    }))
                    .into(),
                ])
                .spacing(spacing.space_xxs)
                .align_y(iced::Alignment::Center),
            );
        }
        if !found {
            content = content.push(widget::text::caption(fl!("share-no-nearby")));
        }
        if let Some(error) = &self.device_error {
            content = content.push(widget::text::caption(error));
        }
        if let Some(error) = &self.transfer_error {
            content = content.push(
                widget::text::caption(error).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            );
        }
        let mut actions = widget::row::with_children(vec![
            widget::button::standard(fl!("share-pair-qr"))
                .width(Length::Fill)
                .on_press(Message::SharePage(Page::Pair))
                .into(),
        ])
        .spacing(spacing.space_xxs);
        if self.connected.value() {
            actions = actions.push(
                widget::button::standard(fl!("share-hide"))
                    .width(Length::Fill)
                    .on_press(Message::SharePage(Page::Connected)),
            );
        }
        content
            .push(
                widget::container(actions)
                    .width(Length::Fill)
                    .padding(iced::Padding {
                        top: f32::from(spacing.space_s),
                        ..iced::Padding::ZERO
                    }),
            )
            .into()
    }

    fn info(&self) -> Element<'_, Message> {
        let button = widget::button::icon(widget::icon::from_name("dialog-information-symbolic"))
            .name(fl!("share-info"))
            .on_press(Message::SharePopup(
                (self.popup != Some(Popup::Info)).then_some(Popup::Info),
            ));
        let mut popover = widget::popover(button)
            .position(widget::popover::Position::Bottom)
            .on_close(Message::SharePopup(None));
        if self.popup == Some(Popup::Info) && self.session.is_some() {
            let mut fields = vec![
                widget::text::caption(fl!("share-network-hint"))
                    .width(Length::Fill)
                    .into(),
            ];
            if self.addresses.len() > 1 {
                fields.push(
                    widget::dropdown(&self.addresses, Some(self.address), Message::QrAddress)
                        .width(Length::Fill)
                        .into(),
                );
            }
            if !self.status.is_empty() {
                fields.push(widget::text::caption(&self.status).into());
            }
            popover = popover.popup(iced::widget::opaque(
                widget::container(
                    widget::column::with_children(fields)
                        .width(Length::Fill)
                        .spacing(cosmic::theme::spacing().space_s),
                )
                .width(300)
                .padding(cosmic::theme::spacing().space_s)
                .class(cosmic::theme::Container::Dialog(true)),
            ));
        }
        popover.into()
    }

    fn linked_devices(&self) -> Element<'_, Message> {
        let button = widget::button::icon(
            widget::icon::from_svg_bytes(
                include_bytes!("../resources/icons/link-symbolic.svg").as_slice(),
            )
            .symbolic(true),
        )
        .name(fl!("linked-devices"))
        .on_press(Message::SharePopup(
            (self.popup != Some(Popup::Devices)).then_some(Popup::Devices),
        ));
        let mut popover = widget::popover(button)
            .position(widget::popover::Position::Bottom)
            .on_close(Message::SharePopup(None));
        if self.popup == Some(Popup::Devices) {
            let spacing = cosmic::theme::spacing();
            let mut devices = widget::column::with_capacity(self.pairings.len());
            if self.pairings.is_empty() {
                devices = devices.push(widget::text::caption(fl!("no-linked-devices")));
            }
            for device in &self.pairings {
                if let Some(edit) = &self.device_edit
                    && edit.id == device.id
                {
                    devices = devices.push(
                        widget::column::with_children(vec![
                            widget::text_input(fl!("device-name"), &edit.name)
                                .id(edit.input.clone())
                                .on_input(Message::DeviceName)
                                .on_submit(|_| Message::SaveDeviceName)
                                .into(),
                            widget::row::with_children(vec![
                                widget::button::standard(fl!("cancel"))
                                    .on_press(Message::EditDevice(None))
                                    .into(),
                                widget::button::suggested(fl!("save"))
                                    .on_press_maybe(
                                        (!edit.name.trim().is_empty())
                                            .then_some(Message::SaveDeviceName),
                                    )
                                    .into(),
                            ])
                            .spacing(spacing.space_xxs)
                            .into(),
                        ])
                        .spacing(spacing.space_xxs),
                    );
                    continue;
                }
                devices = devices.push(
                    widget::row::with_children(vec![
                        status_dot(
                            self.nearby.iter().any(|remote| remote.id == device.id)
                                || self.connected_devices.iter().any(|remote| {
                                    remote.pair_id == device.id && !remote.reconnecting
                                }),
                        ),
                        widget::text::body(&device.name)
                            .width(Length::Fill)
                            .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                            .into(),
                        widget::button::icon(widget::icon::from_name("edit-symbolic"))
                            .name(fl!("rename"))
                            .on_press(Message::EditDevice(Some(device.id.clone())))
                            .into(),
                        widget::button::text(fl!("forget"))
                            .on_press(Message::ForgetDevice(device.id.clone()))
                            .into(),
                    ])
                    .spacing(spacing.space_xxxs)
                    .align_y(iced::Alignment::Center),
                );
                devices = devices.push(
                    widget::toggler(device.allow_open)
                        .label(Some(fl!("share-allow-open")))
                        .width(Length::Fill)
                        .text_size(12)
                        .text_line_height(iced::widget::text::LineHeight::Absolute(17.0.into()))
                        .spacing(spacing.space_xxs)
                        .on_toggle({
                            let id = device.id.clone();
                            move |allowed| Message::AllowRemoteOpen(id.clone(), allowed)
                        }),
                );
            }
            let mut content = widget::column::with_children(vec![
                widget::text::heading(fl!("linked-devices")).into(),
                widget::scrollable(devices.spacing(spacing.space_s))
                    .height(Length::Shrink)
                    .into(),
            ])
            .spacing(spacing.space_s);
            if let Some(error) = &self.device_error {
                content = content.push(widget::text::caption(error));
            }
            popover = popover.popup(iced::widget::opaque(
                widget::container(content)
                    .width(300)
                    .max_height(280)
                    .padding(spacing.space_s)
                    .class(cosmic::theme::Container::Dialog(true)),
            ));
        }
        popover.into()
    }
}

fn status_dot(online: bool) -> Element<'static, Message> {
    let theme = cosmic::theme::active();
    let color = if online {
        iced::Color::from(theme.cosmic().palette.accent_green)
    } else {
        iced::Color::from(theme.cosmic().background(false).on).scale_alpha(0.25)
    };
    widget::text("●").size(8).class(color).into()
}

fn transfer_progress(
    transfers: &HashMap<uuid::Uuid, crate::device_share::TransferProgress>,
) -> Option<crate::device_share::TransferProgress> {
    transfers.values().copied().reduce(|mut total, next| {
        total.transferred += next.transferred;
        total.total = total.total.zip(next.total).map(|(a, b)| a + b);
        total.speed += next.speed;
        total
    })
}

fn transfer_status(
    progress: crate::device_share::TransferProgress,
    direction: crate::device_share::Direction,
) -> Element<'static, Message> {
    let percent = progress.total.map_or_else(String::new, |total| {
        let percent = (u128::from(progress.transferred) * 100)
            .checked_div(u128::from(total))
            .unwrap_or(100)
            .min(100);
        format!(" · {percent}%")
    });
    let (label, style) = match direction {
        crate::device_share::Direction::Download => (
            fl!("share-downloading"),
            cosmic::theme::Text::Custom(|theme| iced::widget::text::Style {
                color: Some(theme.cosmic().palette.accent_green.into()),
                ..Default::default()
            }),
        ),
        crate::device_share::Direction::Upload => (
            fl!("share-uploading"),
            cosmic::theme::Text::Custom(|theme| iced::widget::text::Style {
                color: Some(theme.cosmic().palette.accent_indigo.into()),
                ..Default::default()
            }),
        ),
    };
    widget::container(
        widget::text::caption(format!(
            "{label}{percent} · {}",
            format_speed(progress.speed)
        ))
        .class(style),
    )
    .center_x(Length::Fill)
    .into()
}

fn format_speed(mut speed: f64) -> String {
    let mut unit = "B/s";
    for next in ["kB/s", "MB/s", "GB/s"] {
        if speed < 1000.0 {
            break;
        }
        speed /= 1000.0;
        unit = next;
    }
    format!("{speed:.1} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn received_cleanup_does_not_unlink_replaced_files_or_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("received.txt");
        std::fs::write(&path, "received").unwrap();
        let received = ReceivedFile::new(&path, "device".into(), 1).unwrap();
        let replacement = directory.path().join("replacement.txt");
        std::fs::write(&replacement, "keep").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        received.remove_copy(&path).unwrap();
        assert!(received.export_path(&path).is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
        let received = ReceivedFile::new(&path, "device".into(), 1).unwrap();
        std::fs::rename(&path, &replacement).unwrap();
        std::os::unix::fs::symlink(&replacement, &path).unwrap();
        received.remove_copy(&path).unwrap();
        assert!(received.export_path(&path).is_none());
        assert!(path.is_symlink());
        assert_eq!(std::fs::read_to_string(&replacement).unwrap(), "keep");
        let copy = directory.path().join("copy.txt");
        std::fs::copy(&replacement, &copy).unwrap();
        ReceivedFile::new(&copy, "device".into(), 2)
            .unwrap()
            .remove_copy(&copy)
            .unwrap();
        assert!(!copy.exists());
        assert!(replacement.is_file());
    }

    #[test]
    fn transfer_speeds_use_readable_units() {
        assert_eq!(format_speed(0.0), "0.0 B/s");
        assert_eq!(format_speed(1234.0), "1.2 kB/s");
        assert_eq!(format_speed(2_500_000.0), "2.5 MB/s");
        assert_eq!(format_speed(1_000_000_000.0), "1.0 GB/s");
    }

    #[test]
    fn closing_share_cancels_preparation_without_changing_sources() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("folder");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("note.txt"), "keep").unwrap();
        let mut shelf = crate::shelf::Shelf::default();
        shelf.add([source.clone()]);
        let transfer = Arc::new(shelf.transfer(shelf.export_paths(), false));
        let (share, _) = Share::open();
        let cancelled = share.cancelled.clone();
        drop(share);
        assert!(
            Bundle::prepare(shelf.items.clone(), transfer, None, || cancelled
                .load(Ordering::Relaxed))
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(source.join("note.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn a_failed_folder_does_not_block_other_shared_files() {
        use std::os::unix::ffi::OsStringExt;
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(
            folder.join(std::ffi::OsString::from_vec(vec![0xff])),
            "keep",
        )
        .unwrap();
        let text = directory.path().join("note.txt");
        std::fs::write(&text, "hello").unwrap();
        let mut shelf = crate::shelf::Shelf::default();
        shelf.add([folder.clone(), text.clone()]);
        let bundle = Bundle::prepare(
            shelf.items.clone(),
            Arc::new(shelf.transfer(shelf.export_paths(), false)),
            None,
            || false,
        )
        .unwrap();
        assert_eq!(bundle.files.len(), 2);
        assert_eq!(bundle.files[0].path, folder);
        assert_eq!(
            std::fs::read_to_string(&bundle.files[1].path).unwrap(),
            "hello"
        );
        assert!(text.is_file());
    }

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
        let bundle = Bundle::prepare(shelf.items.clone(), transfer, None, || false).unwrap();
        assert_eq!(bundle.ordered()[0].title, shelf.items[1].name);
        assert_eq!(bundle.files[0].path.file_name().unwrap(), "folder.zip");
        assert_eq!(
            std::fs::read_to_string(&bundle.files[1].path).unwrap(),
            "hello"
        );
        assert!(folder.is_dir() && text.is_file());

        shelf.remove(&folder);
        let photo = directory.path().join("photo.png");
        std::fs::write(&photo, "image").unwrap();
        shelf.add([photo.clone()]);
        shelf.items.last_mut().unwrap().origin = Some(crate::shelf::Sender {
            id: "remote".into(),
            name: "Device".into(),
        });
        let updated = Bundle::prepare(
            shelf.items.clone(),
            Arc::new(shelf.transfer(shelf.export_paths(), false)),
            Some(&bundle),
            || false,
        )
        .unwrap();
        assert_eq!(updated.files[0].id, bundle.files[1].id);
        assert!(updated.files[1].origin.is_some());
        assert!(updated.files[1].id > bundle.files[1].id);
        assert!(bundle.files[0].path.is_file());

        shelf.add([folder]);
        let restored = Bundle::prepare(
            shelf.items.clone(),
            Arc::new(shelf.transfer(shelf.export_paths(), false)),
            Some(&updated),
            || false,
        )
        .unwrap();
        assert!(restored.files[2].id > updated.files[1].id);
        assert_ne!(restored.files[2].id, bundle.files[0].id);
        let mut items = shelf.items.clone();
        items[0].order = u64::MAX;
        items[0].name = "Text preview".into();
        std::fs::remove_file(&items[0].path).unwrap();
        let missing = Bundle::prepare(
            items,
            Arc::new(shelf.transfer(shelf.export_paths(), false)),
            Some(&restored),
            || false,
        )
        .unwrap();
        assert_eq!(missing.ordered()[0].title, "Text preview");
        assert!(!missing.ordered()[0].path.exists());
    }
}
