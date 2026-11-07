// SPDX-License-Identifier: GPL-3.0-only

use crate::{app::Message, fl, share::Bundle};
use cosmic::{
    iced::{self, Length, window},
    prelude::*,
    widget,
};
use iced::futures::{SinkExt, StreamExt, channel::mpsc};
use serde::Deserialize;
use std::{
    collections::HashMap,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const CORE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/orbit-localsend"));

static LOGO: LazyLock<widget::icon::Handle> = LazyLock::new(|| {
    widget::icon::from_svg_bytes(
        include_bytes!("../resources/icons/localsend-symbolic.svg").as_slice(),
    )
    .symbolic(true)
});

fn executable() -> std::io::Result<PathBuf> {
    dirs::data_dir()
        .filter(|path| path.is_absolute())
        .map(|path| path.join(crate::config::ID).join("bin/orbit-localsend"))
        .ok_or_else(|| std::io::Error::other("Missing data directory"))
}

pub fn installed() -> bool {
    executable().is_ok_and(|path| {
        std::fs::symlink_metadata(&path).is_ok_and(|metadata| {
            metadata.is_file()
                && metadata.permissions().mode() & 0o777 == 0o700
                && metadata.len() == CORE.len() as u64
        }) && std::fs::read(path).is_ok_and(|bytes| bytes == CORE)
    })
}

pub fn install() -> std::io::Result<()> {
    if installed() {
        return Ok(());
    }
    let path = executable()?;
    let directory = path.parent().unwrap();
    std::fs::create_dir_all(directory)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(CORE)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o700))?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Controller(tokio::sync::mpsc::UnboundedSender<serde_json::Value>);

impl Controller {
    pub fn command(&self, command: serde_json::Value) -> bool {
        self.0.send(command).is_ok()
    }
}

pub fn send_request(
    device: &str,
    files: &[&Path],
    text: Option<&str>,
) -> serde_json::Result<serde_json::Value> {
    let files = serde_json::to_value(files)?;
    Ok(serde_json::json!({"action":"send", "device":device, "files":files, "text":text}))
}

#[derive(Clone, Debug, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Request {
    pub id: String,
    pub sender: String,
    pub names: Vec<String>,
    pub count: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Progress {
    pub name: String,
    pub done: u64,
    pub total: u64,
    pub receiving: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Update {
    Ready,
    Receiving,
    Devices {
        devices: Vec<Device>,
    },
    Request(Request),
    RequestClosed,
    Progress {
        id: String,
        #[serde(flatten)]
        progress: Progress,
    },
    ProgressEnd {
        id: String,
    },
    Received {
        paths: Vec<PathBuf>,
    },
    Text {
        text: String,
    },
    Done {
        receiving: bool,
        error: Option<String>,
    },
    Error {
        error: String,
    },
}

#[derive(Clone, Debug)]
pub enum Event {
    Started(Controller),
    Update(Update),
}

#[derive(Clone, Debug)]
pub enum Action {
    Refresh,
    Send(String),
    Answer(bool),
    Cancel,
}

pub async fn run(directory: PathBuf, mut output: mpsc::Sender<Event>) {
    let result = async {
        let mut child = tokio::process::Command::new(executable().map_err(|e| e.to_string())?)
            .arg(directory).arg(crate::app::COMPUTER_NAME.as_str())
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .kill_on_drop(true).spawn().map_err(|e| e.to_string())?;
        let mut input = child.stdin.take().ok_or("Could not open LocalSend input")?;
        let mut lines = BufReader::new(child.stdout.take().ok_or("Could not open LocalSend output")?).lines();
        let (sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let mut failed = false;
        output.send(Event::Started(Controller(sender))).await.map_err(|_| "LocalSend closed")?;
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line.map_err(|e| e.to_string())? else { break; };
                    if line.len() > 2 * 1024 * 1024 { return Err("Invalid LocalSend response".to_owned()); }
                    let event = serde_json::from_str(&line).map_err(|_| "Invalid LocalSend response")?;
                    failed |= matches!(event, Update::Error { .. });
                    output.send(Event::Update(event)).await.map_err(|_| "LocalSend closed")?;
                }
                command = commands.recv() => {
                    let Some(command) = command else { break; };
                    input.write_all(format!("{command}\n").as_bytes()).await.map_err(|e| e.to_string())?;
                }
            }
        }
        drop(input);
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await
            .map_err(|_| "LocalSend did not stop")?.map_err(|e| e.to_string())?;
        if !status.success() && !failed { return Err("LocalSend stopped".to_owned()); }
        Ok::<_, String>(())
    }.await;
    if let Err(error) = result {
        let _ = output.send(Event::Update(Update::Error { error })).await;
    }
}

pub struct Dialog {
    pub id: window::Id,
    _task: iced::task::Handle,
    pub controller: Option<Controller>,
    pub devices: Vec<Device>,
    pub request: Option<Request>,
    pub progress: HashMap<String, Progress>,
    pub busy: bool,
    pub receiving: bool,
    pub generation: u64,
    pub ready: bool,
    pub status: String,
    pub bundle: Option<Arc<Bundle>>,
    pub cancelled: Arc<AtomicBool>,
}

impl Drop for Dialog {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.command(serde_json::json!({"action":"stop"}));
    }
}

impl Dialog {
    pub fn open(directory: PathBuf) -> (Self, Task<cosmic::Action<Message>>) {
        let (id, surface) = cosmic::surface::action::app_window::<crate::app::Orbit>(
            |_| cosmic::surface::action::LiveSettings::default(),
            |_| window::Settings {
                size: iced::Size::new(380.0, 520.0),
                min_size: Some(iced::Size::new(340.0, 360.0)),
                decorations: false,
                transparent: true,
                platform_specific: window::settings::PlatformSpecific {
                    application_id: "io.github.motixo.Orbit.LocalSend".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        let (task, handle) = cosmic::task::stream(
            iced::stream::channel(32, move |output| run(directory, output))
                .map(move |event| Message::LocalSendEvent(id, event)),
        )
        .abortable();
        let dialog = Self {
            id,
            _task: handle,
            controller: None,
            devices: Vec::new(),
            request: None,
            progress: HashMap::new(),
            busy: false,
            receiving: false,
            generation: 0,
            ready: false,
            status: String::new(),
            bundle: None,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        (
            dialog,
            Task::batch([
                cosmic::task::message(cosmic::Action::Surface(surface)),
                task,
            ]),
        )
    }

    pub fn command(&self, value: serde_json::Value) -> bool {
        self.controller
            .as_ref()
            .is_some_and(|controller| controller.command(value))
    }

    pub fn view(&self, count: usize) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let logo = LOGO
            .clone()
            .icon()
            .size(112)
            .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(|theme| {
                widget::svg::Style {
                    color: Some(theme.cosmic().accent_text_color().into()),
                }
            })));
        let mut content = widget::column::with_capacity(10)
            .spacing(spacing.space_s)
            .width(Length::Fill)
            .push(
                widget::container(logo)
                    .padding([spacing.space_s, 0])
                    .center_x(Length::Fill),
            );
        content = content.push(widget::text::caption(fl!(
            "shelf-count",
            count = (count as i64)
        )));
        if let Some(request) = &self.request {
            let mut offer = widget::column::with_capacity(request.names.len() + 2)
                .spacing(spacing.space_xxs)
                .push(
                    widget::text::heading(&request.sender)
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                )
                .push(widget::text::caption(fl!(
                    "shelf-count",
                    count = (request.count as i64)
                )));
            for name in &request.names {
                offer = offer.push(
                    widget::text::body(name).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                );
            }
            content = content.push(
                widget::container(offer)
                    .padding(spacing.space_s)
                    .width(Length::Fill)
                    .class(cosmic::theme::Container::Card),
            );
            content = content.push(
                widget::row::with_children(vec![
                    widget::button::standard(fl!("owly-decline"))
                        .on_press(Message::LocalSendAction(Action::Answer(false)))
                        .into(),
                    widget::button::suggested(fl!("owly-accept"))
                        .on_press(Message::LocalSendAction(Action::Answer(true)))
                        .into(),
                ])
                .spacing(spacing.space_xs),
            );
        }
        if !self.devices.is_empty() {
            content = content.push(widget::text::heading(fl!("share-nearby")));
        }
        for device in &self.devices {
            content = content.push(
                widget::button::standard(&device.name)
                    .width(Length::Fill)
                    .on_press_maybe(
                        (self.ready
                            && !self.busy
                            && !self.receiving
                            && self.request.is_none()
                            && self.progress.is_empty()
                            && count > 0)
                            .then(|| Message::LocalSendAction(Action::Send(device.id.clone()))),
                    ),
            );
        }
        for progress in self.progress.values() {
            let percent = ((u128::from(progress.done) * 100) / u128::from(progress.total.max(1)))
                .min(99) as u8;
            let label = if progress.receiving {
                fl!("share-downloading")
            } else {
                fl!("share-uploading")
            };
            content = content.push(
                widget::row::with_children(vec![
                    widget::determinate_circular(f32::from(percent) / 100.0)
                        .size(24.0)
                        .into(),
                    widget::column::with_children(vec![
                        widget::text::body(&progress.name)
                            .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                            .into(),
                        widget::text::caption(format!("{label} · {percent}%")).into(),
                    ])
                    .into(),
                ])
                .spacing(spacing.space_s),
            );
        }
        if (!self.ready || self.busy || self.receiving)
            && self.progress.is_empty()
            && self.status.is_empty()
        {
            content = content.push(
                widget::container(widget::indeterminate_circular().size(24.0))
                    .center_x(Length::Fill),
            );
        } else if self.ready && self.devices.is_empty() && self.status.is_empty() {
            content = content.push(widget::text::caption(fl!("localsend-no-devices")));
        }
        if !self.status.is_empty() {
            content = content.push(
                widget::text::caption(&self.status)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            );
        }
        content = content.push(widget::text::caption(fl!("localsend-hint")));
        let header = widget::header_bar()
            .title(fl!("localsend"))
            .on_drag(Message::DragLocalSend)
            .on_close(Message::CloseLocalSend);
        let mut actions = widget::row::with_capacity(2).spacing(spacing.space_xs);
        actions = actions.push(
            widget::button::standard(fl!("localsend-refresh")).on_press_maybe(
                (self.ready && !self.busy).then_some(Message::LocalSendAction(Action::Refresh)),
            ),
        );
        if self.busy || self.receiving {
            actions = actions.push(
                widget::button::standard(fl!("cancel"))
                    .on_press(Message::LocalSendAction(Action::Cancel)),
            );
        }
        widget::container(
            widget::column::with_children(vec![
                header.into(),
                widget::scrollable(content.padding(spacing.space_s))
                    .height(Length::Fill)
                    .into(),
                widget::container(actions).padding(spacing.space_s).into(),
            ])
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(1)
        .clip(true)
        .class(cosmic::theme::Container::custom(|theme| {
            widget::container::Style {
                background: Some(theme.cosmic().background(theme.transparent).base.into()),
                border: iced::Border {
                    color: theme.cosmic().bg_divider().into(),
                    width: 1.0,
                    radius: theme.cosmic().corner_radii.radius_m.into(),
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
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    #[test]
    fn send_request_reports_unsupported_paths_without_panicking() {
        let path = Path::new("/tmp/photo.png");
        let request = send_request("device", &[path], None).unwrap();
        assert_eq!(request["files"], serde_json::json!(["/tmp/photo.png"]));
        let invalid = PathBuf::from(OsString::from_vec(b"/tmp/invalid-\xff.png".to_vec()));
        assert!(send_request("device", &[&invalid], None).is_err());
    }
}
