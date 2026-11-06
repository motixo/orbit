// SPDX-License-Identifier: GPL-3.0-only

use crate::{app::Message, fl, share::Bundle};
use cosmic::{
    iced::{self, Alignment, Length, window},
    prelude::*,
    widget,
};
use iced::futures::{SinkExt, StreamExt, channel::mpsc};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

pub const SERVICE: &str = "io.github.hepp3n.kdeconnect";

static LOGO: LazyLock<widget::icon::Handle> = LazyLock::new(|| {
    widget::icon::from_svg_bytes(include_bytes!("../resources/icons/kde-connect.svg").as_slice())
        .symbolic(true)
});

#[derive(Clone, Debug, Deserialize, Serialize, zbus::zvariant::Type)]
pub struct Device {
    id: String,
    name: String,
    device_type: String,
    is_paired: bool,
    is_reachable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, zbus::zvariant::Type)]
pub struct TransferInfo {
    transfer_id: String,
    device_id: String,
    filename: String,
    direction: String,
    bytes_transferred: u64,
    total_bytes: u64,
    path: String,
    state: String,
    error: String,
}

impl TransferInfo {
    fn percent(&self) -> Option<u8> {
        if self.state == "completed" {
            Some(100)
        } else if self.total_bytes > 0 {
            Some(
                (u128::from(self.bytes_transferred) * 100 / u128::from(self.total_bytes)).min(99)
                    as u8,
            )
        } else {
            None
        }
    }
}

#[zbus::proxy(
    interface = "io.github.hepp3n.kdeconnect.Daemon",
    default_service = "io.github.hepp3n.kdeconnect",
    default_path = "/io/github/hepp3n/kdeconnect/Daemon"
)]
trait Daemon {
    async fn list_devices(&self) -> zbus::Result<Vec<Device>>;
    async fn pair_device(&self, device_id: &str) -> zbus::Result<()>;
    async fn unpair_device(&self, device_id: &str) -> zbus::Result<()>;
    async fn accept_pairing(&self, device_id: &str) -> zbus::Result<()>;
    async fn reject_pairing(&self, device_id: &str) -> zbus::Result<()>;
    async fn send_files(&self, device_id: &str, files: Vec<String>) -> zbus::Result<()>;
}

fn transfer_api(xml: &str) -> bool {
    ["FileTransferProgress", "FileReceived"]
        .iter()
        .all(|name| xml.contains(&format!("<signal name=\"{name}\"")))
}

pub async fn installed() -> bool {
    let check = async {
        let connection = zbus::Connection::session().await?;
        let dbus = zbus::fdo::DBusProxy::new(&connection).await?;
        if !dbus.name_has_owner(SERVICE.try_into()?).await?
            && !dbus
                .list_activatable_names()
                .await?
                .iter()
                .any(|name| name.as_str() == SERVICE)
        {
            return Ok::<_, zbus::Error>(false);
        }
        let proxy = DaemonProxy::new(&connection).await?;
        Ok(transfer_api(&proxy.inner().introspect().await?))
    };
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), check).await,
        Ok(Ok(true))
    )
}

#[derive(Clone, Debug)]
pub enum Action {
    Close,
    Drag,
    Refresh,
    Pair(String),
    Unpair(String),
    Answer(String, bool),
    Send(String),
}

#[derive(Clone, Debug)]
pub enum Event {
    Ready(Vec<Device>),
    Device(Device),
    Disconnected(String),
    Pairing(String, String),
    Progress(TransferInfo),
    Received(PathBuf),
    Sent,
    Error(String),
    Stopped(String),
}

enum Command {
    Refresh,
    Pair(String),
    Unpair(String),
    Answer(String, bool),
    Send(String, Arc<Bundle>),
}

async fn listen(
    output: &mut mpsc::Sender<Event>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> Result<(), String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|error| error.to_string())?;
    connection
        .request_name_with_flags(
            "io.github.motixo.Orbit.KdeConnect",
            zbus::fdo::RequestNameFlags::DoNotQueue.into(),
        )
        .await
        .map_err(|error| match error {
            zbus::Error::NameTaken => fl!("kde-connect-already-open"),
            error => error.to_string(),
        })?;
    let proxy = DaemonProxy::new(&connection)
        .await
        .map_err(|error| error.to_string())?;
    let dbus = zbus::fdo::DBusProxy::new(&connection)
        .await
        .map_err(|error| error.to_string())?;
    let mut owners = dbus
        .receive_name_owner_changed_with_args(&[(0, SERVICE)])
        .await
        .map_err(|error| error.to_string())?;
    let mut signals = proxy
        .inner()
        .receive_all_signals()
        .await
        .map_err(|error| error.to_string())?;
    let devices = proxy
        .list_devices()
        .await
        .map_err(|_| fl!("kde-connect-service-error"))?;
    let api = proxy
        .inner()
        .introspect()
        .await
        .map_err(|error| error.to_string())?;
    if !transfer_api(&api) {
        return Err(fl!("kde-connect-api-error"));
    }
    output
        .send(Event::Ready(devices))
        .await
        .map_err(|error| error.to_string())?;
    loop {
        tokio::select! {
            signal = signals.next() => {
                let signal = signal.ok_or_else(|| fl!("kde-connect-service-error"))?;
                let event = decode(&signal).map_err(|error| error.to_string())?;
                if let Some(event) = event
                    && output.send(event).await.is_err()
                {
                    return Ok(());
                }
            }
            owner = owners.next() => {
                let owner = owner.ok_or_else(|| fl!("kde-connect-service-error"))?;
                if owner.args().map_err(|error| error.to_string())?.old_owner().is_some() {
                    return Err(fl!("kde-connect-service-error"));
                }
            }
            command = commands.next() => {
                let Some(command) = command else { return Ok(()) };
                let result = match command {
                    Command::Refresh => async {
                        let devices = proxy.list_devices().await?;
                        Ok(Some(Event::Ready(devices)))
                    }.await,
                    Command::Pair(device) => proxy.pair_device(&device).await.map(|()| None),
                    Command::Unpair(device) => proxy.unpair_device(&device).await.map(|()| None),
                    Command::Answer(device, accept) => {
                        if accept { proxy.accept_pairing(&device).await }
                        else { proxy.reject_pairing(&device).await }.map(|()| None)
                    }
                    Command::Send(device, bundle) => {
                        let files: Result<Vec<_>, _> = bundle.files.iter().map(|file| {
                            if !file.path.is_file() { return Err(zbus::Error::Failure(fl!("kde-connect-export-error"))); }
                            let path = file.path.to_str().ok_or_else(|| zbus::Error::Failure(fl!("kde-connect-export-error")))?;
                            // The daemon opens files asynchronously, even after this window closes.
                            if let Some(cache) = crate::cache::Directory::for_file(&file.path) { cache.hand_off(); }
                            Ok(path.to_owned())
                        }).collect();
                        match files {
                            Ok(files) => proxy.send_files(&device, files).await.map(|()| Some(Event::Sent)),
                            Err(error) => Err(error),
                        }
                    }
                };
                let event = match result {
                    Ok(event) => event,
                    Err(error) => Some(Event::Error(error.to_string())),
                };
                if let Some(event) = event
                    && output.send(event).await.is_err()
                {
                    return Ok(());
                }
            }
        }
    }
}

fn decode(signal: &zbus::Message) -> zbus::Result<Option<Event>> {
    let body = signal.body();
    Ok(match signal.header().member().map(|name| name.as_str()) {
        Some("DeviceConnected" | "DevicePaired") => {
            let (_, device): (String, Device) = body.deserialize()?;
            Some(Event::Device(device))
        }
        Some("DeviceDisconnected") => Some(Event::Disconnected(body.deserialize::<String>()?)),
        Some("PairingRequested") => {
            let (id, name): (String, String) = body.deserialize()?;
            Some(Event::Pairing(id, name))
        }
        Some("FileTransferProgress") => {
            let (progress,): (TransferInfo,) = body.deserialize()?;
            Some(Event::Progress(progress))
        }
        Some("FileReceived") => {
            let (_, path): (String, String) = body.deserialize()?;
            let path = PathBuf::from(path);
            path.is_absolute().then_some(Event::Received(path))
        }
        _ => None,
    })
}

pub struct Dialog {
    pub id: window::Id,
    pub ready: bool,
    pub busy: bool,
    pub cancelled: Arc<AtomicBool>,
    pub status: String,
    devices: Vec<Device>,
    pairing: Option<(String, String)>,
    progress: Vec<TransferInfo>,
    commands: mpsc::UnboundedSender<Command>,
    _task: iced::task::Handle,
}

impl Drop for Dialog {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Dialog {
    pub fn open() -> (Self, Task<cosmic::Action<Message>>) {
        let (id, open) = cosmic::surface::action::app_window::<crate::app::Orbit>(
            |_| cosmic::surface::action::LiveSettings::default(),
            |_| window::Settings {
                size: iced::Size::new(380.0, 520.0),
                min_size: Some(iced::Size::new(340.0, 360.0)),
                decorations: false,
                transparent: true,
                platform_specific: window::settings::PlatformSpecific {
                    application_id: "io.github.motixo.Orbit.KdeConnect".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        let (commands, incoming) = mpsc::unbounded();
        let (task, handle) = cosmic::task::stream(
            iced::stream::channel(32, |mut output| async move {
                if let Err(error) = listen(&mut output, incoming).await {
                    let _ = output.send(Event::Stopped(error)).await;
                }
            })
            .map(move |event| Message::KdeConnectEvent(id, event)),
        )
        .abortable();
        (
            Self {
                id,
                ready: false,
                busy: false,
                cancelled: Arc::new(AtomicBool::new(false)),
                status: String::new(),
                devices: Vec::new(),
                pairing: None,
                progress: Vec::new(),
                commands,
                _task: handle.abort_on_drop(),
            },
            Task::batch([cosmic::task::message(cosmic::Action::Surface(open)), task]),
        )
    }

    pub fn action(&mut self, action: Action) {
        let command = match action {
            Action::Refresh => Command::Refresh,
            Action::Pair(device) => {
                self.status = fl!("share-confirming");
                Command::Pair(device)
            }
            Action::Unpair(device) => {
                self.status.clear();
                Command::Unpair(device)
            }
            Action::Answer(device, accept) => {
                self.pairing = None;
                Command::Answer(device, accept)
            }
            _ => return,
        };
        if self.commands.unbounded_send(command).is_err() {
            self.ready = false;
            self.status = fl!("kde-connect-service-error");
        }
    }

    pub fn send(&mut self, device: String, result: Result<Arc<Bundle>, String>) {
        match result {
            Ok(bundle) => {
                if self
                    .commands
                    .unbounded_send(Command::Send(device, bundle))
                    .is_err()
                {
                    self.busy = false;
                    self.status = fl!("kde-connect-service-error");
                }
            }
            Err(error) => {
                self.busy = false;
                self.status = error;
            }
        }
    }

    pub fn event(&mut self, event: Event) -> Option<PathBuf> {
        match event {
            Event::Ready(mut devices) => {
                devices.sort_by(|a, b| a.name.cmp(&b.name));
                self.devices = devices;
                self.ready = true;
            }
            Event::Device(device) => {
                if device.is_paired
                    && self
                        .pairing
                        .as_ref()
                        .is_some_and(|(id, _)| id == &device.id)
                {
                    self.pairing = None;
                }
                self.devices.retain(|old| old.id != device.id);
                self.devices.push(device);
                self.devices.sort_by(|a, b| a.name.cmp(&b.name));
                self.status.clear();
            }
            Event::Disconnected(id) => {
                if let Some(device) = self.devices.iter_mut().find(|device| device.id == id) {
                    device.is_reachable = false;
                }
                if self
                    .pairing
                    .as_ref()
                    .is_some_and(|(device, _)| device == &id)
                {
                    self.pairing = None;
                }
            }
            Event::Pairing(id, name) => self.pairing = Some((id, name)),
            Event::Progress(progress) => record_progress(&mut self.progress, progress),
            Event::Received(path) => return Some(path),
            Event::Sent => self.busy = false,
            Event::Error(error) => {
                self.busy = false;
                self.status = error;
            }
            Event::Stopped(error) => {
                self.ready = false;
                self.busy = false;
                self.pairing = None;
                for progress in &mut self.progress {
                    if progress.state == "transferring" {
                        progress.state = "failed".into();
                        progress.error = error.clone();
                    }
                }
                self.status = error;
            }
        }
        None
    }

    pub fn view(&self, selected: usize) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let action = |action| Message::KdeConnectAction(action);
        let header = widget::header_bar()
            .title(fl!("kde-connect"))
            .on_drag(action(Action::Drag))
            .on_close(action(Action::Close));
        let logo = LOGO
            .clone()
            .icon()
            .size(128)
            .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(|theme| {
                widget::svg::Style {
                    color: Some(theme.cosmic().accent_text_color().into()),
                }
            })));
        let mut content = widget::column::with_children(vec![
            widget::container(logo)
                .padding([spacing.space_s, 0])
                .center_x(Length::Fill)
                .into(),
        ])
        .width(Length::Fill)
        .spacing(spacing.space_s);
        if let Some((id, name)) = &self.pairing {
            content = content.push(widget::text::heading(name)).push(
                widget::row::with_children(vec![
                    widget::button::standard(fl!("owly-decline"))
                        .on_press(action(Action::Answer(id.clone(), false)))
                        .into(),
                    widget::button::suggested(fl!("owly-accept"))
                        .on_press(action(Action::Answer(id.clone(), true)))
                        .into(),
                ])
                .spacing(spacing.space_xxs),
            );
        }
        for device in &self.devices {
            let enabled = self.ready && device.is_reachable && !self.busy;
            let button = if device.is_paired {
                widget::button::suggested(fl!("kde-connect-send")).on_press_maybe(
                    (enabled && selected > 0).then(|| action(Action::Send(device.id.clone()))),
                )
            } else {
                widget::button::standard(fl!("share-pair"))
                    .on_press_maybe(enabled.then(|| action(Action::Pair(device.id.clone()))))
            };
            let mut actions = widget::row::with_capacity(2)
                .spacing(spacing.space_xxs)
                .push(button);
            if device.is_paired {
                actions = actions.push(
                    widget::button::standard(fl!("kde-connect-unpair")).on_press_maybe(
                        (self.ready && !self.busy)
                            .then(|| action(Action::Unpair(device.id.clone()))),
                    ),
                );
            }
            content = content.push(
                widget::row::with_children(vec![
                    widget::icon::from_name(if device.device_type == "phone" {
                        "phone-symbolic"
                    } else {
                        "computer-symbolic"
                    })
                    .size(24)
                    .icon()
                    .into(),
                    widget::column::with_children(vec![
                        widget::text::body(&device.name)
                            .width(Length::Fill)
                            .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                            .into(),
                        widget::text::caption(if device.is_reachable {
                            fl!("kde-connect-online")
                        } else {
                            fl!("kde-connect-offline")
                        })
                        .into(),
                    ])
                    .width(Length::Fill)
                    .into(),
                    actions.into(),
                ])
                .spacing(spacing.space_s)
                .align_y(Alignment::Center),
            );
        }
        if self.ready && self.devices.is_empty() {
            content = content.push(widget::text::caption(fl!("kde-connect-no-devices")));
        }
        for progress in self.progress.iter().rev() {
            let percent = progress.percent();
            let icon: Element<'_, Message> = if progress.state == "failed" {
                widget::icon::from_name("dialog-error-symbolic")
                    .size(24)
                    .icon()
                    .into()
            } else if let Some(percent) = percent {
                widget::determinate_circular(f32::from(percent) / 100.0)
                    .size(24.0)
                    .into()
            } else {
                widget::indeterminate_circular().size(24.0).into()
            };
            let state = match progress.state.as_str() {
                "completed" => fl!("kde-connect-completed"),
                "failed" => fl!("kde-connect-failed"),
                _ if progress.direction == "download" => fl!("share-downloading"),
                _ => fl!("share-uploading"),
            };
            let label = if progress.state == "failed" {
                format!("{state} · {}", progress.error)
            } else if let Some(percent) = percent {
                format!("{state} · {percent}%")
            } else {
                state
            };
            let mut detail = widget::column::with_children(vec![
                widget::text::body(&progress.filename)
                    .width(Length::Fill)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .into(),
                widget::text::caption(label)
                    .width(Length::Fill)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .into(),
            ])
            .width(Length::Fill);
            if let Some(device) = self
                .devices
                .iter()
                .find(|device| device.id == progress.device_id)
            {
                detail = detail.push(widget::text::caption(&device.name));
            }
            content = content.push(
                widget::row::with_children(vec![icon, detail.into()])
                    .spacing(spacing.space_s)
                    .align_y(Alignment::Center),
            );
        }
        if (!self.ready && self.status.is_empty()) || self.busy {
            content = content.push(
                widget::container(widget::indeterminate_circular().size(24.0))
                    .center_x(Length::Fill),
            );
        }
        if !self.status.is_empty() {
            content = content.push(
                widget::text::caption(&self.status)
                    .width(Length::Fill)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            );
        }
        let footer = widget::row::with_children(vec![
            widget::text::caption(fl!("shelf-count", count = (selected as i64))).into(),
            widget::space::horizontal().into(),
            widget::button::standard(fl!("kde-connect-refresh"))
                .on_press_maybe(self.ready.then(|| action(Action::Refresh)))
                .into(),
        ])
        .align_y(Alignment::Center);
        let page = widget::column::with_children(vec![
            header.into(),
            widget::scrollable(content.padding([0, spacing.space_s]))
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            widget::container(footer)
                .padding(spacing.space_s)
                .width(Length::Fill)
                .into(),
        ])
        .width(Length::Fill)
        .height(Length::Fill);
        widget::container(page)
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

fn record_progress(transfers: &mut Vec<TransferInfo>, progress: TransferInfo) {
    if progress.state == "completed" {
        transfers.retain(|old| old.transfer_id != progress.transfer_id);
        return;
    }
    if let Some(previous) = transfers
        .iter_mut()
        .find(|old| old.transfer_id == progress.transfer_id)
    {
        *previous = progress;
    } else {
        transfers.push(progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_pair_request_survives_unpaired_device_updates() {
        let (commands, _) = mpsc::unbounded();
        let (_, task) = Task::<Message>::none().abortable();
        let mut dialog = Dialog {
            id: window::Id::unique(),
            ready: true,
            busy: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            status: String::new(),
            devices: Vec::new(),
            pairing: None,
            progress: Vec::new(),
            commands,
            _task: task,
        };
        let mut device = Device {
            id: "phone".into(),
            name: "trinity".into(),
            device_type: "phone".into(),
            is_paired: false,
            is_reachable: true,
        };
        dialog.event(Event::Pairing(device.id.clone(), device.name.clone()));
        dialog.event(Event::Device(device.clone()));
        assert_eq!(
            dialog.pairing.as_ref().map(|(id, _)| id.as_str()),
            Some("phone")
        );
        device.is_paired = true;
        dialog.event(Event::Device(device));
        assert!(dialog.pairing.is_none());
    }

    #[test]
    fn tracks_concurrent_files_and_removes_only_completed_transfers() {
        let mut first = TransferInfo {
            transfer_id: "first".into(),
            device_id: "phone".into(),
            filename: "video.mov".into(),
            direction: "download".into(),
            bytes_transferred: u64::MAX,
            total_bytes: u64::MAX,
            path: String::new(),
            state: "transferring".into(),
            error: String::new(),
        };
        assert_eq!(first.percent(), Some(99));
        let mut transfers = Vec::new();
        record_progress(&mut transfers, first.clone());
        let mut second = first.clone();
        second.transfer_id = "second".into();
        second.device_id = "other".into();
        second.direction = "upload".into();
        second.total_bytes = 0;
        assert_eq!(second.percent(), None);
        record_progress(&mut transfers, second.clone());
        assert_eq!(transfers.len(), 2);
        first.state = "completed".into();
        assert_eq!(first.percent(), Some(100));
        record_progress(&mut transfers, first.clone());
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].transfer_id, "second");
        assert_eq!(transfers[0].state, "transferring");
        record_progress(&mut transfers, first.clone());
        assert_eq!(transfers.len(), 1);
        second.state = "completed".into();
        record_progress(&mut transfers, second);
        assert!(transfers.is_empty());
        first.state = "failed".into();
        record_progress(&mut transfers, first);
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].state, "failed");
    }
}
