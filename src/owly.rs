// SPDX-License-Identifier: GPL-3.0-only

use crate::{app::Message, fl, transfer::Transfer};
use cosmic::{
    iced::{self, Alignment, Length, window},
    prelude::*,
    widget,
};
use iced::futures::{SinkExt, StreamExt, channel::mpsc};
use serde::Deserialize;
use std::{
    collections::HashMap,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};

static LOGO: LazyLock<widget::icon::Handle> = LazyLock::new(|| {
    widget::icon::from_svg_bytes(
        include_bytes!("../resources/icons/airdrop-symbolic.svg").as_slice(),
    )
    .symbolic(true)
});

#[derive(Clone, Debug)]
pub struct Request {
    pub id: uuid::Uuid,
    pub sender: String,
    pub items: Vec<String>,
    expires: tokio::time::Instant,
}

#[derive(Clone, Debug)]
pub enum ReceiveEvent {
    Ready(Arc<Session>),
    Request(Request),
    RequestClosed(uuid::Uuid),
    Progress {
        id: u64,
        progress: Option<Progress>,
        file: Option<u64>,
    },
    Files(Vec<PathBuf>),
    Link(String),
    TransferFailed(String),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
}

impl Progress {
    fn percent(self) -> Option<u8> {
        self.total.filter(|total| *total > 0).map(|total| {
            // Completion is shown only after the receiver confirms the transfer.
            (u128::from(self.done) * 100 / u128::from(total)).min(99) as u8
        })
    }

    fn view(self, receiving: bool, file: Option<u64>) -> Element<'static, Message> {
        let label = if let Some(index) = file {
            fl!("owly-downloading-file", index = index)
        } else if receiving {
            fl!("share-downloading")
        } else {
            fl!("share-uploading")
        };
        let percent = self.percent();
        let style = if receiving {
            cosmic::theme::Text::Custom(|theme| iced::widget::text::Style {
                color: Some(theme.cosmic().palette.accent_green.into()),
                ..Default::default()
            })
        } else {
            cosmic::theme::Text::Custom(|theme| iced::widget::text::Style {
                color: Some(theme.cosmic().palette.accent_indigo.into()),
                ..Default::default()
            })
        };
        let mut content = widget::column::with_children(vec![
            widget::text::caption(match percent {
                Some(percent) => format!("{label} · {percent}%"),
                None => label,
            })
            .class(style)
            .into(),
        ])
        .width(Length::Fill)
        .spacing(cosmic::theme::spacing().space_xxs)
        .align_x(Alignment::Center);
        if let Some(percent) = percent {
            content =
                content.push(widget::determinate_circular(f32::from(percent) / 100.0).size(24.0));
        } else {
            content = content.push(widget::indeterminate_circular().size(24.0));
        }
        content.into()
    }
}

#[derive(Clone, Debug)]
pub enum SendEvent {
    Progress(Progress),
    Finished(Result<(), String>),
}

#[derive(Clone, Debug, Deserialize)]
pub struct Capabilities {
    hardware: bool,
    mode: Option<String>,
    missing: Vec<Missing>,
}

#[derive(Clone, Debug, Deserialize)]
struct Missing {
    id: String,
    say: String,
}

impl Capabilities {
    fn supported(&self) -> bool {
        self.hardware
            && matches!(
                self.mode.as_deref(),
                Some("shared" | "exclusive" | "active-monitor")
            )
            && !self.missing.iter().any(|missing| missing.id == "hardware")
    }
}

pub async fn probe() -> Option<Capabilities> {
    let output = tokio::time::timeout(
        Duration::from_secs(8),
        tokio::process::Command::new("owly")
            .args(["doctor", "--json"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let report: Capabilities = serde_json::from_slice(&output.stdout).ok()?;
    report.supported().then_some(report)
}

pub fn capabilities() -> iced::Subscription<Option<Capabilities>> {
    iced::Subscription::run(|| {
        iced::stream::channel(
            1,
            |mut output: mpsc::Sender<Option<Capabilities>>| async move {
                let mut previous = None;
                loop {
                    let state = probe_state();
                    if previous.as_ref() != Some(&state) {
                        previous = Some(state);
                        if output.send(probe().await).await.is_err() {
                            return;
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            },
        )
    })
}

fn probe_state() -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut paths: Vec<_> = std::fs::read_dir("/sys/class/net")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path).map(|directory| directory.join("owly")));
    }
    if crate::sandbox::active() {
        paths.extend([
            PathBuf::from("/run/host/usr/bin/owly"),
            PathBuf::from("/run/host/usr/local/bin/owly"),
        ]);
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let metadata = std::fs::metadata(&path).ok()?;
            Some((path, metadata.ino(), metadata.modified().ok()?))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub struct Session {
    connection: zbus::Connection,
    lease: AtomicU64,
    interface: String,
    wifi: Vec<(String, String)>,
}

impl Session {
    pub async fn start() -> Result<Arc<Self>, String> {
        let wifi = active_wifi().await;
        let connection = zbus::connection::Builder::system()
            .map_err(|error| error.to_string())?
            .method_timeout(Duration::from_secs(90))
            .build()
            .await
            .map_err(|error| error.to_string())?;
        let radio = radio(&connection)
            .await
            .map_err(|error| error.to_string())?;
        let (lease, link): (
            u64,
            std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
        ) = radio
            .call("Acquire", &(0u32,))
            .await
            .map_err(|error| error.to_string())?;
        let mut session = Self {
            connection: connection.clone(),
            lease: AtomicU64::new(lease),
            interface: String::new(),
            wifi,
        };
        session.interface = link
            .get("interface")
            .and_then(|value| value.downcast_ref::<String>().ok())
            .ok_or_else(|| fl!("owly-start-error"))?;
        Ok(Arc::new(session))
    }

    pub async fn close(&self) {
        let lease = self.lease.swap(0, Ordering::AcqRel);
        if lease != 0 {
            release(&self.connection, lease, &self.wifi).await;
        }
    }

    pub async fn browse(&self) -> Result<Vec<Device>, String> {
        let output = tokio::time::timeout(
            Duration::from_secs(25),
            tokio::process::Command::new("owly")
                .args(["browse", "--iface", &self.interface, "--seconds", "5"])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| fl!("owly-search-error"))?
        .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let mut devices = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_device)
            .collect::<Vec<_>>();
        devices.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        devices.dedup_by(|a, b| a.id == b.id);
        devices.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(devices)
    }

    pub async fn send(
        &self,
        target: &str,
        transfer: &Transfer,
        output: &mut mpsc::Sender<SendEvent>,
    ) -> Result<(), String> {
        let mut command = tokio::process::Command::new("owly");
        command.args(["send", "--iface", &self.interface, "--to", target]);
        if transfer.links {
            for text in transfer.text.iter().flatten() {
                let text = text.contents().ok_or_else(|| fl!("owly-send-error"))?;
                command.arg("--url").arg(text.trim());
            }
        } else {
            command.arg("--").args(&transfer.files.0);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut updated = std::time::Instant::now() - Duration::from_millis(100);
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            tail(stdout, |_| {}),
            tail(stderr, |record| {
                if let Some(progress) = parse_progress(record)
                    && updated.elapsed() >= Duration::from_millis(100)
                {
                    let _ = output.try_send(SendEvent::Progress(progress));
                    updated = std::time::Instant::now();
                }
            })
        );
        let status = status.map_err(|error| error.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(if stderr.is_empty() { stdout } else { stderr })
        }
    }

    pub async fn receive(
        self: &Arc<Self>,
        directory: PathBuf,
        mut replies: mpsc::UnboundedReceiver<(uuid::Uuid, bool)>,
        output: &mut mpsc::Sender<ReceiveEvent>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let directory = tempfile::Builder::new()
            .prefix("AirDrop-")
            .tempdir_in(directory)
            .map_err(|error| error.to_string())?
            .keep();
        let mut process = ReceiverProcess {
            child: None,
            directory,
        };
        let mut child = tokio::process::Command::new("owly")
            .args([
                "receive",
                "--iface",
                &self.interface,
                "--name",
                &crate::app::COMPUTER_NAME,
            ])
            .arg("--dir")
            .arg(&process.directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut stderr = child.stderr.take().unwrap();
        process.child = Some(child);
        let mut parser = ReceiverOutput::default();
        let mut buffer = [0u8; 4096];
        let mut seen = HashMap::new();
        let mut timer = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                biased;
                read = stderr.read(&mut buffer) => {
                    let count = read.map_err(|error| error.to_string())?;
                    if count == 0 { return Err(parser.error.take().unwrap_or_else(|| fl!("owly-receive-error"))); }
                    for event in parser.push(&buffer[..count])? {
                        output.send(event).await.map_err(|error| error.to_string())?;
                    }
                }
                _ = timer.tick() => {
                    if let Some(request) = &parser.pending
                        && tokio::time::Instant::now() >= request.expires
                    {
                        let id = request.id;
                        stdin.write_all(b"n\n").await.map_err(|error| error.to_string())?;
                        parser.pending = None;
                        output.send(ReceiveEvent::RequestClosed(id)).await.map_err(|error| error.to_string())?;
                    }
                }
                reply = replies.next() => {
                    let Some((id, accept)) = reply else { return Ok(()) };
                    if parser.pending.as_ref().is_some_and(|request| request.id == id && tokio::time::Instant::now() < request.expires) {
                        stdin.write_all(if accept { b"y\n" } else { b"n\n" }).await.map_err(|error| error.to_string())?;
                        parser.pending = None;
                        output.send(ReceiveEvent::RequestClosed(id)).await.map_err(|error| error.to_string())?;
                    }
                }
                read = stdout.next_line() => {
                    let Some(line) = read.map_err(|error| error.to_string())? else { return Err(fl!("owly-receive-error")); };
                    if let Ok(url) = ron::from_str::<String>(line.trim())
                        && crate::transfer::is_link(&url)
                    {
                        output.send(ReceiveEvent::Link(url)).await.map_err(|error| error.to_string())?;
                    }
                    let files = completed_files(&process.directory, &mut seen).map_err(|error| error.to_string())?;
                    if !files.is_empty() {
                        output.send(ReceiveEvent::Files(files)).await.map_err(|error| error.to_string())?;
                    }
                }
            }
        }
    }
}

struct ReceiverProcess {
    child: Option<tokio::process::Child>,
    directory: PathBuf,
}

impl Drop for ReceiverProcess {
    fn drop(&mut self) {
        let directory = self.directory.clone();
        if let Some(mut child) = self.child.take() {
            if let Some(id) = child.id() {
                // Owly handles SIGINT and removes unfinished uploads before exiting.
                let _ = std::process::Command::new("kill")
                    .args(["-INT", "--", &id.to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if tokio::time::timeout(Duration::from_secs(2), child.wait())
                        .await
                        .is_err()
                    {
                        let _ = child.kill().await;
                    }
                    let _ = std::fs::remove_dir(directory);
                });
            }
        } else {
            let _ = std::fs::remove_dir(directory);
        }
    }
}

#[derive(Default)]
struct ReceiverOutput {
    buffer: Vec<u8>,
    offer: Option<Request>,
    pending: Option<Request>,
    error: Option<String>,
}

impl ReceiverOutput {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<ReceiveEvent>, String> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > 64 * 1024 {
            return Err(fl!("owly-receive-error"));
        }
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
            self.buffer.drain(..=end);
            if let Some((id, value)) = line
                .strip_prefix("receive ")
                .and_then(|line| line.split_once(": "))
                && let Ok(id) = id.parse::<u64>()
            {
                if value == "done" {
                    events.push(ReceiveEvent::Progress {
                        id,
                        progress: None,
                        file: None,
                    });
                } else {
                    let (file, value) = if let Some(value) = value.strip_prefix("file ") {
                        let Some((file, value)) = value.split_once(": ") else {
                            continue;
                        };
                        let Ok(file) = file.parse::<u64>() else {
                            continue;
                        };
                        if file == 0 {
                            continue;
                        }
                        (Some(file), value)
                    } else {
                        (None, value)
                    };
                    if let Some(progress) = parse_progress(value.as_bytes()) {
                        events.push(ReceiveEvent::Progress {
                            id,
                            progress: Some(progress),
                            file,
                        });
                    }
                }
            } else if let Some(header) = line.strip_suffix(" wants to send:") {
                if let Some(request) = self.pending.take() {
                    events.push(ReceiveEvent::RequestClosed(request.id));
                }
                let mut decoder =
                    ron::Deserializer::from_str(header).map_err(|error| error.to_string())?;
                let name = String::deserialize(&mut decoder).map_err(|error| error.to_string())?;
                self.offer = Some(Request {
                    id: uuid::Uuid::new_v4(),
                    sender: name.chars().filter(|c| !c.is_control()).take(160).collect(),
                    items: Vec::new(),
                    expires: tokio::time::Instant::now(),
                });
            } else if line.starts_with("  ")
                && let Some(offer) = &mut self.offer
            {
                let mut decoder = ron::Deserializer::from_str(line.trim_start())
                    .map_err(|error| error.to_string())?;
                let item = String::deserialize(&mut decoder).map_err(|error| error.to_string())?;
                offer
                    .items
                    .push(item.chars().filter(|c| !c.is_control()).take(160).collect());
            } else if line.contains("timed out") || line == "the request was withdrawn" {
                if let Some(request) = self.pending.take() {
                    events.push(ReceiveEvent::RequestClosed(request.id));
                }
                self.offer = None;
            } else if let Some(reason) = line.strip_prefix("transfer failed: ") {
                events.push(ReceiveEvent::TransferFailed(reason.to_owned()));
            } else if let Some(error) = line.strip_prefix("owly: ") {
                self.error = Some(error.to_owned());
            }
        }
        if self.buffer == b"accept? [y/N] " {
            self.buffer.clear();
            let mut request = self.offer.take().ok_or_else(|| fl!("owly-receive-error"))?;
            // Decline before Owly's 15-second timeout so late answers cannot spill into a new prompt.
            request.expires = tokio::time::Instant::now() + Duration::from_secs(10);
            self.pending = Some(request.clone());
            events.push(ReceiveEvent::Request(request));
        }
        Ok(events)
    }
}

fn completed_files(
    directory: &Path,
    seen: &mut HashMap<PathBuf, (u64, i64, i64)>,
) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    // The private receive directory contains only Owly's atomically completed files and staging folders.
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".owly-staging-")
            && (kind.is_file() || kind.is_dir())
        {
            let metadata = entry.metadata()?;
            let stamp = (metadata.ino(), metadata.ctime(), metadata.ctime_nsec());
            if seen.insert(entry.path(), stamp) != Some(stamp) {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

impl Drop for Session {
    fn drop(&mut self) {
        let lease = self.lease.swap(0, Ordering::AcqRel);
        if lease != 0
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let connection = self.connection.clone();
            let wifi = std::mem::take(&mut self.wifi);
            runtime.spawn(async move {
                release(&connection, lease, &wifi).await;
            });
        }
    }
}

async fn radio(connection: &zbus::Connection) -> zbus::Result<zbus::Proxy<'_>> {
    zbus::Proxy::new(
        connection,
        "io.github.t4t5.Owly",
        "/io/github/t4t5/Owly",
        "io.github.t4t5.Owly.Radio1",
    )
    .await
}

async fn active_wifi() -> Vec<(String, String)> {
    let Ok(output) = tokio::process::Command::new("nmcli")
        .args([
            "-t",
            "-f",
            "UUID,TYPE,DEVICE",
            "connection",
            "show",
            "--active",
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let uuid = fields.next()?;
            let kind = fields.next()?;
            let device = fields.next()?;
            (kind == "802-11-wireless" && !device.is_empty() && uuid::Uuid::parse_str(uuid).is_ok())
                .then(|| (uuid.to_owned(), device.to_owned()))
        })
        .collect()
}

async fn release(connection: &zbus::Connection, lease: u64, wifi: &[(String, String)]) {
    let result: zbus::Result<()> =
        async { radio(connection).await?.call("Release", &(lease,)).await }.await;
    if let Err(error) = result {
        eprintln!("Could not release Owly radio: {error}");
    }
    // User-owned Wi-Fi passwords may be unavailable to the system service.
    let active = active_wifi().await;
    for (uuid, device) in wifi.iter().filter(|network| !active.contains(network)) {
        let _ = tokio::process::Command::new("nmcli")
            .args([
                "--wait",
                "10",
                "connection",
                "up",
                "uuid",
                uuid,
                "ifname",
                device,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await;
    }
}

async fn tail(mut reader: impl AsyncRead + Unpin, mut on_record: impl FnMut(&[u8])) -> String {
    let mut bytes = Vec::new();
    let mut record = Vec::new();
    let mut buffer = [0u8; 4096];
    while let Ok(count) = reader.read(&mut buffer).await {
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.len() > 8192 {
            bytes.drain(..bytes.len() - 8192);
        }
        for byte in &buffer[..count] {
            if matches!(byte, b'\r' | b'\n') {
                on_record(&record);
                record.clear();
            } else {
                record.push(*byte);
                if record.len() > 8192 {
                    record.drain(..4096);
                }
            }
        }
    }
    on_record(&record);
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn parse_progress(record: &[u8]) -> Option<Progress> {
    let record = std::str::from_utf8(record)
        .ok()?
        .trim()
        .strip_suffix(" bytes")?;
    let (done, total) = record.split_once(" / ")?;
    Some(Progress {
        done: done.parse().ok()?,
        total: if total == "?" {
            None
        } else {
            Some(total.parse().ok()?)
        },
    })
}

fn parse_device(line: &str) -> Option<Device> {
    let (id, detail) = line.split_once(char::is_whitespace)?;
    if id.len() != 12 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let (name, address) = detail.trim().rsplit_once(" (")?;
    if !address.ends_with(')') {
        return None;
    }
    // Owly prints names as Rust string literals, including Unicode escapes.
    let name: String = ron::from_str(name).ok()?;
    let name: String = name.chars().filter(|c| !c.is_control()).take(160).collect();
    Some(Device {
        id: id.to_owned(),
        name: if name.is_empty() { id.to_owned() } else { name },
    })
}

pub struct Dialog {
    pub id: window::Id,
    pub task: Option<iced::task::Handle>,
    pub session: Option<Arc<Session>>,
    pub devices: Vec<Device>,
    pub sending: bool,
    pub busy: bool,
    pub progress: Option<Progress>,
    pub downloads: HashMap<u64, (Progress, Option<u64>)>,
    pub receiving: bool,
    pub generation: u64,
    pub replies: Option<mpsc::UnboundedSender<(uuid::Uuid, bool)>>,
    pub request: Option<Request>,
    pub status: String,
    capabilities: Capabilities,
}

impl Dialog {
    pub fn open(capabilities: Capabilities) -> (Self, Task<cosmic::Action<Message>>) {
        let (id, open) = cosmic::surface::action::app_window::<crate::app::Orbit>(
            |_| cosmic::surface::action::LiveSettings::default(),
            |_| window::Settings {
                size: iced::Size::new(380.0, 520.0),
                min_size: Some(iced::Size::new(340.0, 360.0)),
                decorations: false,
                transparent: true,
                platform_specific: window::settings::PlatformSpecific {
                    application_id: "io.github.motixo.Orbit.AirDrop".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        (
            Self {
                id,
                task: None,
                session: None,
                devices: Vec::new(),
                sending: false,
                busy: false,
                progress: None,
                downloads: HashMap::new(),
                receiving: false,
                generation: 0,
                replies: None,
                request: None,
                status: String::new(),
                capabilities,
            },
            cosmic::task::message(cosmic::Action::Surface(open)),
        )
    }

    pub fn view(&self, selected: usize) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
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
        if !self.receiving {
            content = content.push(
                widget::container(widget::text::caption(fl!(
                    "shelf-count",
                    count = (selected as i64)
                )))
                .center_x(Length::Fill),
            );
        }
        if let Some(request) = &self.request {
            let mut offer = widget::column::with_children(vec![
                widget::text::heading(&request.sender)
                    .width(Length::Fill)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .into(),
            ])
            .spacing(spacing.space_xxs);
            for item in &request.items {
                offer = offer.push(
                    widget::text::body(item)
                        .width(Length::Fill)
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                );
            }
            content = content.push(
                widget::container(offer)
                    .padding(spacing.space_s)
                    .width(Length::Fill)
                    .class(cosmic::theme::Container::Card),
            );
        } else if self.receiving && self.session.is_some() && self.downloads.is_empty() {
            content = content.push(
                widget::container(widget::text::body(fl!(
                    "owly-listening",
                    name = crate::app::COMPUTER_NAME.as_str()
                )))
                .center_x(Length::Fill),
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
                        (!self.busy && selected > 0).then(|| Message::OwlySend(device.id.clone())),
                    ),
            );
        }
        if let Some(progress) = self.progress {
            content = content.push(progress.view(false, None));
        }
        for (progress, file) in self.downloads.values() {
            content = content.push(progress.view(true, *file));
        }
        if self.busy && self.progress.is_none() {
            content = content.push(
                widget::container(widget::indeterminate_circular().size(24.0))
                    .center_x(Length::Fill),
            );
        } else if self.sending && self.devices.is_empty() && self.status.is_empty() {
            content = content.push(widget::text::caption(fl!("owly-no-devices")));
        }
        if !self.status.is_empty() {
            content = content.push(widget::text::caption(&self.status));
        }
        if self.session.is_none() && !self.busy {
            for missing in &self.capabilities.missing {
                content = content.push(widget::text::caption(&missing.say));
            }
        }
        if !self.receiving && !self.sending {
            content = content.push(widget::text::caption(fl!("owly-receiver-hint")));
        }
        if self.capabilities.mode.as_deref() != Some("shared") {
            content = content.push(widget::text::caption(fl!("owly-wifi-hint")));
        }
        let mut header = widget::header_bar()
            .title(fl!("owly"))
            .on_drag(Message::DragOwly)
            .on_close(Message::CloseOwly);
        if self.receiving || self.sending {
            header = header.start(
                widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                    .name(fl!("back"))
                    .on_press(Message::OwlyBack),
            );
        }
        let mut page = widget::column::with_children(vec![
            header.into(),
            widget::scrollable(content.padding([0, spacing.space_s]))
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        ])
        .height(Length::Fill);
        let mut actions = widget::row::with_capacity(2)
            .width(Length::Fill)
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center);
        if let Some(request) = &self.request {
            actions = actions
                .push(
                    widget::button::standard(fl!("owly-decline"))
                        .width(Length::Fill)
                        .on_press(Message::OwlyAnswer(request.id, false)),
                )
                .push(
                    widget::button::suggested(fl!("owly-accept"))
                        .width(Length::Fill)
                        .on_press(Message::OwlyAnswer(request.id, true)),
                );
        } else if !self.receiving && !self.sending {
            actions = actions.push(
                widget::button::standard(fl!("owly-receive"))
                    .width(Length::Fill)
                    .on_press_maybe(
                        (!self.busy && self.capabilities.missing.is_empty())
                            .then_some(Message::OwlyReceive),
                    ),
            );
            actions = actions.push(
                widget::button::suggested(fl!("owly-start"))
                    .width(Length::Fill)
                    .on_press_maybe(
                        (!self.busy && self.capabilities.missing.is_empty() && selected > 0)
                            .then_some(Message::OwlyRefresh),
                    ),
            );
        }
        if self.request.is_some() || !self.receiving && !self.sending {
            page = page.push(widget::container(actions).padding(spacing.space_s));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_progress_handles_fragmented_carriage_return_records() {
        let (mut writer, reader) = tokio::io::duplex(32);
        let writing = async move {
            for chunk in [
                b"accepted\n\r12 / ".as_slice(),
                b"24 bytes\r24 / 24 bytes\n",
            ] {
                writer.write_all(chunk).await.unwrap();
            }
        };
        let mut updates = Vec::new();
        let (_, captured) = tokio::join!(
            writing,
            tail(reader, |record| {
                if let Some(progress) = parse_progress(record) {
                    updates.push(progress);
                }
            })
        );
        assert_eq!(
            updates,
            [
                Progress {
                    done: 12,
                    total: Some(24)
                },
                Progress {
                    done: 24,
                    total: Some(24)
                }
            ]
        );
        assert!(captured.contains("accepted"));
        assert_eq!(updates[0].percent(), Some(50));
        assert_eq!(updates[1].percent(), Some(99));
        assert_eq!(
            Progress {
                done: u64::MAX,
                total: Some(1)
            }
            .percent(),
            Some(99)
        );
        assert_eq!(
            Progress {
                done: 0,
                total: Some(0)
            }
            .percent(),
            None
        );
        assert!(parse_progress(b"12 / broken bytes").is_none());
    }

    #[test]
    fn receive_progress_keeps_transfers_separate_and_clears_finished_items() {
        let mut parser = ReceiverOutput::default();
        assert!(parser.push(b"receive 7: 12 / ").unwrap().is_empty());
        let events = parser
            .push(b"24 bytes\nreceive 8: 8 / ? bytes\nreceive 8: file 2: 5 / 10 bytes\nreceive 7: done\n")
            .unwrap();
        assert!(matches!(
            &events[..],
            [
                ReceiveEvent::Progress {
                    id: 7,
                    progress: Some(Progress {
                        done: 12,
                        total: Some(24)
                    }),
                    file: None,
                },
                ReceiveEvent::Progress {
                    id: 8,
                    progress: Some(Progress {
                        done: 8,
                        total: None
                    }),
                    file: None,
                },
                ReceiveEvent::Progress {
                    id: 8,
                    progress: Some(Progress {
                        done: 5,
                        total: Some(10)
                    }),
                    file: Some(2),
                },
                ReceiveEvent::Progress {
                    id: 7,
                    progress: None,
                    file: None,
                },
            ]
        ));
    }

    #[tokio::test]
    async fn receive_prompts_handle_fragmented_output_and_expire_old_requests() {
        let mut parser = ReceiverOutput::default();
        assert!(parser.push(b"\"Jane's iPhone\" (\"iPhone\") wants to send:\n  \"photo.png\" image 42 bytes\naccept? [y").unwrap().is_empty());
        let events = parser.push(b"/N] ").unwrap();
        let [ReceiveEvent::Request(request)] = &events[..] else {
            panic!("missing request")
        };
        assert_eq!(request.sender, "Jane's iPhone");
        assert_eq!(request.items, ["photo.png"]);
        let first_id = request.id;
        let events = parser.push(b"\ntimed out\n").unwrap();
        assert!(matches!(&events[..], [ReceiveEvent::RequestClosed(id)] if *id == first_id));
        assert!(parser.pending.is_none());
        let events = parser.push(b"\"Other device\" (\"Mac\") wants to send:\n  \"https://example.com/\"\naccept? [y/N] ").unwrap();
        let [ReceiveEvent::Request(request)] = &events[..] else {
            panic!("missing next request")
        };
        assert_ne!(request.id, first_id);
        assert!(parser.push(b"\nthe request was withdrawn\n").is_ok());
        assert!(parser.pending.is_none());
    }

    #[test]
    fn only_completed_receive_entries_are_added_including_newline_names() {
        let fixture = tempfile::tempdir().unwrap();
        let partial = fixture.path().join(".owly-staging-test");
        std::fs::create_dir(&partial).unwrap();
        std::fs::write(partial.join("partial.png"), b"partial").unwrap();
        let complete = fixture.path().join("photo\nname.png");
        std::fs::write(&complete, b"complete").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", fixture.path().join("link")).unwrap();
        let mut seen = HashMap::new();
        assert_eq!(
            completed_files(fixture.path(), &mut seen).unwrap(),
            std::slice::from_ref(&complete)
        );
        assert!(
            completed_files(fixture.path(), &mut seen)
                .unwrap()
                .is_empty()
        );
        let replacement = tempfile::NamedTempFile::new_in(fixture.path()).unwrap();
        std::fs::write(replacement.path(), b"new transfer").unwrap();
        std::fs::rename(replacement.path(), &complete).unwrap();
        assert_eq!(
            completed_files(fixture.path(), &mut seen).unwrap(),
            [complete]
        );
    }

    #[test]
    fn unsupported_or_unknown_hardware_is_hidden() {
        let mut report: Capabilities = serde_json::from_value(serde_json::json!({
            "hardware": true, "mode": "exclusive", "missing": []
        }))
        .unwrap();
        assert!(report.supported());
        report.missing.push(Missing {
            id: "hardware".into(),
            say: String::new(),
        });
        assert!(!report.supported());
        report.missing.clear();
        report.mode = Some("future-mode".into());
        assert!(!report.supported());
        report.mode = Some("shared".into());
        report.hardware = false;
        assert!(!report.supported());
        assert!(serde_json::from_str::<Capabilities>("{}").is_err());
    }

    #[test]
    fn receiver_output_keeps_names_and_rejects_invalid_identifiers() {
        let name = "Jane’s \"iPhone\"\u{200e}";
        let line = format!("abc123def456   {name:?} ([fe80::1%5]:8770)");
        assert_eq!(
            parse_device(&line),
            Some(Device {
                id: "abc123def456".into(),
                name: name.into()
            })
        );
        for line in [
            "not a receiver",
            "--option       \"Name\" ([fe80::1]:8770)",
            "abc123def456 invalid ([fe80::1]:8770)",
        ] {
            assert!(parse_device(line).is_none());
        }
    }
}
