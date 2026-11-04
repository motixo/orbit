// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    fl,
    import::ImportedFiles,
    share::{Bundle, SharedFile},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use cosmic::iced::futures::{channel::mpsc::UnboundedSender, stream};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::Hasher,
    io::Write,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{mpsc, watch},
};

const PEER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/orbit-peer"));

pub fn executable() -> std::io::Result<std::path::PathBuf> {
    dirs::data_dir()
        .filter(|path| path.is_absolute())
        .map(|path| path.join(crate::config::ID).join("bin/orbit-peer"))
        .ok_or_else(|| std::io::Error::other("Missing data directory"))
}

fn peer_version() -> String {
    let mut hash = DefaultHasher::new();
    hash.write(PEER);
    format!("{:016x}", hash.finish())
}

fn peer_runtime() -> std::io::Result<std::path::PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("orbit-share").join(peer_version()))
        .ok_or_else(|| std::io::Error::other("Missing runtime directory"))
}

pub async fn set_pwa(url: &url::Url) -> std::io::Result<()> {
    if !installed() {
        return Ok(());
    }
    let socket = peer_runtime()?.join("control.sock");
    listen();
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut connection = loop {
            match tokio::net::UnixStream::connect(&socket).await {
                Ok(connection) => break connection,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                Err(error) => return Err(error),
            }
        };
        connection
            .write_all(format!("{}\n", serde_json::json!({"UpdatePWA": url.as_str()})).as_bytes())
            .await?;
        let line = BufReader::new(connection)
            .lines()
            .next_line()
            .await?
            .ok_or_else(|| std::io::Error::other("Sharing service closed"))?;
        let reply: serde_json::Value = serde_json::from_str(&line)?;
        if reply["ok"] == true {
            Ok(())
        } else {
            Err(std::io::Error::other("Could not update sharing address"))
        }
    })
    .await
    .map_err(|_| std::io::Error::other("Sharing service timed out"))?
}

pub fn listen() {
    let _ = std::thread::Builder::new()
        .name("orbit-share-listener".into())
        .spawn(|| {
            if !installed() {
                return;
            }
            let Ok(executable) = executable() else { return };
            let Ok(orbit) = std::env::current_exe() else {
                return;
            };
            let Ok(runtime) = peer_runtime() else { return };
            let version = peer_version();
            let socket = runtime.join("control.sock");
            if std::os::unix::net::UnixStream::connect(socket).is_ok() {
                return;
            }
            let Some(pwa) =
                crate::config::pwa_url(&crate::config::Preferences::load().share_pwa_url)
            else {
                return;
            };
            let bind = addresses()
                .ok()
                .and_then(|addresses| addresses.first().map(|address| address.ip.to_string()))
                .unwrap_or_default();
            if let Ok(mut child) = std::process::Command::new(executable)
                .env("ORBIT_PEER_VERSION", version)
                .env("ORBIT_EXECUTABLE", orbit.with_file_name("orbit"))
                .env("ORBIT_PEER_BIND", bind)
                .env("ORBIT_SHARE_PWA", pwa.as_str())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .spawn()
            {
                let _ = child.wait();
            }
        });
}

pub fn installed() -> bool {
    executable().is_ok_and(|path| installed_at(&path))
}

fn installed_at(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.permissions().mode() & 0o777 == 0o700
            && metadata.len() == PEER.len() as u64
    }) && std::fs::read(path).is_ok_and(|bytes| bytes == PEER)
}

pub fn install() -> std::io::Result<()> {
    install_at(&executable()?)
}

fn install_at(path: &std::path::Path) -> std::io::Result<()> {
    if installed_at(path) {
        return Ok(());
    }
    let directory = path.parent().unwrap();
    std::fs::create_dir_all(directory)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(PEER)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o700))?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub ip: Ipv4Addr,
    pub label: String,
}

impl AsRef<str> for Address {
    fn as_ref(&self) -> &str {
        &self.label
    }
}

pub fn addresses() -> Result<Vec<Address>, String> {
    let output = std::process::Command::new("ip")
        .args(["-j", "-4", "address", "show", "up"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("Could not list network addresses".into());
    }
    parse_addresses(&output.stdout)
}

fn parse_addresses(bytes: &[u8]) -> Result<Vec<Address>, String> {
    let interfaces: Vec<serde_json::Value> =
        serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let mut addresses = Vec::new();
    for interface in interfaces {
        let name = interface["ifname"].as_str().unwrap_or_default();
        if ["lo", "docker", "br-", "veth", "virbr"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        if let Some(entries) = interface["addr_info"].as_array() {
            for entry in entries {
                if let Some(ip) = entry["local"]
                    .as_str()
                    .and_then(|ip| ip.parse::<Ipv4Addr>().ok())
                    && !ip.is_loopback()
                    && !ip.is_unspecified()
                    && !ip.is_multicast()
                    && (ip.is_private() || ip.is_link_local())
                {
                    addresses.push(Address {
                        ip,
                        label: format!("{name} · {ip}"),
                    });
                }
            }
        }
    }
    addresses.sort_by_key(|address| (!address.label.starts_with("wl"), address.label.clone()));
    addresses.dedup_by_key(|address| address.ip);
    if addresses.is_empty() {
        return Err("No local network".into());
    }
    Ok(addresses)
}

#[derive(Debug)]
pub struct Session {
    pub url: String,
    bundle: watch::Sender<Arc<Bundle>>,
    stop: watch::Sender<bool>,
    commands: Option<mpsc::UnboundedSender<String>>,
    directory: watch::Sender<std::path::PathBuf>,
}

impl Session {
    pub fn command(&self, value: serde_json::Value) -> bool {
        self.commands
            .as_ref()
            .is_some_and(|commands| commands.send(format!("{value}\n")).is_ok())
    }
    pub async fn disconnect(&self) {
        if self
            .commands
            .as_ref()
            .is_some_and(|commands| commands.send("{\"disconnect\":true}\n".into()).is_ok())
            && tokio::time::timeout(Duration::from_secs(1), stopping(self.stop.subscribe()))
                .await
                .is_err()
        {
            eprintln!("Sharing disconnect timed out");
        }
        let _ = self.stop.send(true);
    }

    pub fn set_directory(&self, directory: std::path::PathBuf) {
        self.directory.send_replace(directory);
    }

    pub fn update(&self, bundle: Arc<Bundle>) {
        self.bundle.send_replace(bundle);
        if let Some(commands) = &self.commands {
            let _ = commands.send("{\"items\":true}\n".into());
        }
    }

    pub fn rename(&self, id: &str, name: &str) -> bool {
        self.commands.as_ref().is_some_and(|commands| {
            commands
                .send(format!(
                    "{}\n",
                    serde_json::json!({"rename": {"id": id, "name": name}})
                ))
                .is_ok()
        })
    }

    pub fn forget(&self, id: &str) -> bool {
        self.commands.as_ref().is_some_and(|commands| {
            commands
                .send(format!("{}\n", serde_json::json!({"forget": id})))
                .is_ok()
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

#[derive(Clone)]
struct ShareState {
    bundle: watch::Receiver<Arc<Bundle>>,
    origin: String,
    incoming: UnboundedSender<Event>,
    stop: watch::Receiver<bool>,
    staging: Arc<tokio::sync::Mutex<Staging>>,
    directory: watch::Receiver<std::path::PathBuf>,
    received: ReceivedSources,
}

pub type ReceivedSources = Arc<tokio::sync::Mutex<HashSet<(String, String)>>>;
type Staging = HashMap<uuid::Uuid, (String, Weak<tempfile::TempDir>)>;

#[derive(Clone, Debug)]
pub enum Event {
    Received(ImportedFiles, Option<String>),
    Remove(std::path::PathBuf, u64),
    Stopped,
    Devices(Vec<ConnectedDevice>, bool),
    Pairings(Vec<PairedDevice>),
    PairingError(String),
    TransferError(String, String),
    Nearby(Vec<NearbyDevice>),
    PairRequest(PairRequest),
    Connecting(String, bool),
    Transfer(uuid::Uuid, Direction, Option<TransferProgress>),
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConnectedDevice {
    pub ip: Ipv4Addr,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pair_id: String,
    #[serde(default)]
    pub reconnecting: bool,
    #[serde(skip)]
    unresponsive_since: Option<Instant>,
}

impl ConnectedDevice {
    pub fn same_device(&self, other: &Self) -> bool {
        self.pair_id == other.pair_id && (!self.pair_id.is_empty() || self.ip == other.ip)
    }

    pub fn paired_from(&self, previous: &Self) -> bool {
        !self.pair_id.is_empty() && previous.pair_id.is_empty() && self.ip == previous.ip
    }

    pub fn is_sender(&self, key: &str) -> bool {
        key.is_empty()
            || self.pair_id == key
            || key
                .strip_prefix("ip:")
                .and_then(|ip| ip.parse::<Ipv4Addr>().ok())
                == Some(self.ip)
    }

    pub fn mark_reconnecting(&mut self, now: Instant) {
        self.reconnecting = true;
        self.unresponsive_since.get_or_insert(now);
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.unresponsive_since
            .is_some_and(|since| now.saturating_duration_since(since) >= Duration::from_secs(1))
    }
}

pub fn update_devices(
    known: &mut Vec<ConnectedDevice>,
    devices: Vec<ConnectedDevice>,
    now: Instant,
) {
    for remote in known.iter_mut() {
        remote.mark_reconnecting(now);
    }
    for mut remote in devices {
        let previous = known
            .iter()
            .position(|previous| previous.same_device(&remote))
            .or_else(|| {
                known
                    .iter()
                    .position(|previous| remote.paired_from(previous))
            });
        if let Some(index) = previous {
            let previous = &mut known[index];
            if remote.name.is_empty() {
                remote.name.clone_from(&previous.name);
            }
            remote.unresponsive_since = if remote.reconnecting {
                previous.unresponsive_since
            } else {
                None
            };
            *previous = remote;
        } else if !remote.reconnecting {
            known.push(remote);
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub desktop: bool,
    #[serde(default)]
    pub allow_open: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NearbyDevice {
    pub id: String,
    pub name: String,
    pub ip: Ipv4Addr,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PairRequest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub approved: bool,
}

pub fn sender_name<'a>(
    key: &str,
    pairings: &'a [PairedDevice],
    connected_devices: &'a [ConnectedDevice],
) -> Option<&'a str> {
    let ip = key
        .strip_prefix("ip:")
        .and_then(|ip| ip.parse::<Ipv4Addr>().ok());
    let remote = connected_devices
        .iter()
        .find(|remote| remote.pair_id == key || ip == Some(remote.ip));
    pairings
        .iter()
        .find(|device| device.id == key || remote.is_some_and(|remote| remote.pair_id == device.id))
        .map(|device| device.name.as_str())
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            remote
                .map(|remote| remote.name.as_str())
                .filter(|name| !name.trim().is_empty())
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TransferProgress {
    pub transferred: u64,
    pub total: Option<u64>,
    pub speed: f64,
}

struct Transfer {
    id: uuid::Uuid,
    direction: Direction,
    progress: TransferProgress,
    incoming: UnboundedSender<Event>,
    sampled: Instant,
    sampled_bytes: u64,
}

impl Transfer {
    fn new(incoming: UnboundedSender<Event>, total: Option<u64>, direction: Direction) -> Self {
        let transfer = Self {
            id: uuid::Uuid::new_v4(),
            direction,
            progress: TransferProgress {
                total,
                ..Default::default()
            },
            incoming,
            sampled: Instant::now(),
            sampled_bytes: 0,
        };
        let _ = transfer.incoming.unbounded_send(Event::Transfer(
            transfer.id,
            direction,
            Some(transfer.progress),
        ));
        transfer
    }

    fn report(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.sampled).as_secs_f64();
        if elapsed > 0.0 {
            self.progress.speed = (self.progress.transferred - self.sampled_bytes) as f64 / elapsed;
        }
        self.sampled = now;
        self.sampled_bytes = self.progress.transferred;
        let _ = self.incoming.unbounded_send(Event::Transfer(
            self.id,
            self.direction,
            Some(self.progress),
        ));
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        let _ = self
            .incoming
            .unbounded_send(Event::Transfer(self.id, self.direction, None));
    }
}

pub async fn start(
    address: &Address,
    session_id: uuid::Uuid,
    remote_open: Option<uuid::Uuid>,
    bundle: Arc<Bundle>,
    preferences: &crate::config::Preferences,
    received: ReceivedSources,
    incoming: UnboundedSender<Event>,
) -> Result<Arc<Session>, String> {
    use std::process::Stdio;

    let pwa_url =
        crate::config::pwa_url(&preferences.share_pwa_url).ok_or_else(|| fl!("share-pwa-error"))?;
    let directory = preferences
        .download_directory()
        .map_err(|_| fl!("share-folder-error"))?;
    let executable = executable().map_err(|error| error.to_string())?;
    if !installed_at(&executable) {
        return Err("Run File sharing setup to install Croc".into());
    }
    let mut session = start_http(bundle, incoming.clone(), directory, received).await?;
    let token = session
        .url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let peer_version = peer_version();
    let runtime = peer_runtime().map_err(|error| error.to_string())?;
    tokio::fs::create_dir_all(&runtime)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|e| e.to_string())?;
    let socket = runtime.join("control.sock");
    let connection = match tokio::net::UnixStream::connect(&socket).await {
        Ok(connection) => connection,
        Err(_) => {
            let mut child = tokio::process::Command::new(executable)
                .env("ORBIT_PEER_VERSION", &peer_version)
                .env(
                    "ORBIT_EXECUTABLE",
                    std::env::current_exe()
                        .map_err(|error| error.to_string())?
                        .with_file_name("orbit"),
                )
                .env("ORBIT_PEER_BIND", address.ip.to_string())
                .env("ORBIT_SHARE_PWA", pwa_url.as_str())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| e.to_string())?;
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if let Ok(connection) = tokio::net::UnixStream::connect(&socket).await {
                        break connection;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .map_err(|_| "Could not start sharing")?
        }
    };
    let (read, mut input) = connection.into_split();
    let config = serde_json::json!({"Bind":address.ip.to_string(), "URL":session.url, "Token":token, "Share":session_id.to_string(), "PWA":pwa_url.as_str(), "Open": remote_open.map(|id| id.to_string()).unwrap_or_default()});
    input
        .write_all(format!("{config}\n").as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    let mut output = BufReader::new(read).lines();
    let line = tokio::time::timeout(Duration::from_secs(15), output.next_line())
        .await
        .map_err(|_| "Sharing timed out")?
        .map_err(|error| error.to_string())?
        .ok_or("Could not start sharing")?;
    #[derive(Deserialize)]
    struct Ready {
        port: u16,
        fingerprint: String,
    }
    let ready: Ready = serde_json::from_str(&line).map_err(|error| error.to_string())?;
    session.url = invitation_url(
        pwa_url,
        address.ip,
        ready.port,
        &ready.fingerprint,
        &token,
        session_id,
    )?;
    let (commands, mut pending) = mpsc::unbounded_channel::<String>();
    session.commands = Some(commands);
    let stop = session.stop.subscribe();
    let stopped = session.stop.clone();
    tokio::spawn(async move {
        let mut croc_transfers: HashMap<uuid::Uuid, Transfer> = HashMap::new();
        loop {
            tokio::select! {
                _ = stopping(stop.clone()) => break,
                command = pending.recv() => {
                    let Some(command) = command else { break };
                    if input.write_all(command.as_bytes()).await.is_err() {
                        break;
                    }
                }
                line = output.next_line() => match line {
                    Ok(Some(line)) => {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                            let progress = &value["crocProgress"];
                            if let Some(id) = progress["id"].as_str().and_then(|id| id.parse::<uuid::Uuid>().ok()) {
                                if progress["finished"] == true {
                                    croc_transfers.remove(&id);
                                } else if let (Some(done), Some(total)) = (progress["done"].as_u64(), progress["total"].as_u64()) {
                                    let direction = if progress["direction"] == "upload" { Direction::Upload } else { Direction::Download };
                                    let transfer = croc_transfers.entry(id).or_insert_with(|| Transfer::new(incoming.clone(), Some(total), direction));
                                    if done >= transfer.progress.transferred && done <= total {
                                        transfer.progress.transferred = done;
                                        transfer.report(Instant::now());
                                    }
                                }
                            }
                            if let Ok(devices) = serde_json::from_value(value["devices"].clone()) {
                                let _ = incoming.unbounded_send(Event::Devices(devices, value["intentional"] == true));
                            }
                            if let Ok(pairings) = serde_json::from_value(value["pairings"].clone()) {
                                let _ = incoming.unbounded_send(Event::Pairings(pairings));
                            }
                            if let (Some(name), Some(error)) = (value["transferError"]["name"].as_str(), value["transferError"]["message"].as_str()) {
                                let _ = incoming.unbounded_send(Event::TransferError(name.into(), error.into()));
                            }
                            if let Some(error) = value["pairingError"].as_str() {
                                let _ = incoming.unbounded_send(Event::PairingError(error.into()));
                            }
                            if let Some(error) = value["discoveryError"].as_str() {
                                let _ = incoming.unbounded_send(Event::PairingError(error.into()));
                            }
                            if let Ok(devices) = serde_json::from_value(value["nearby"].clone()) {
                                let _ = incoming.unbounded_send(Event::Nearby(devices));
                            }
                            if let Ok(request) = serde_json::from_value(value["pairRequest"].clone()) {
                                let _ = incoming.unbounded_send(Event::PairRequest(request));
                            }
                            if let (Some(id),Some(active)) = (value["connecting"]["id"].as_str(),value["connecting"]["active"].as_bool()) {
                                let _ = incoming.unbounded_send(Event::Connecting(id.into(), active));
                            }
                        }
                    }
                    _ => break,
                }
            }
        }
        let _ = incoming.unbounded_send(Event::Stopped);
        let _ = stopped.send(true);
    });
    Ok(Arc::new(session))
}

fn invitation_url(
    mut pwa_url: url::Url,
    ip: Ipv4Addr,
    port: u16,
    fingerprint: &str,
    token: &str,
    session_id: uuid::Uuid,
) -> Result<String, String> {
    use base64::Engine;

    let fingerprint = fingerprint.replace(':', "");
    if port == 0 || fingerprint.len() != 64 || token.len() != 64 {
        return Err("Invalid sharing credentials".into());
    }
    // Version, IPv4, port, fingerprint, token, and shelf identity.
    let mut payload = vec![2];
    payload.extend(ip.octets());
    payload.extend(port.to_be_bytes());
    for hex in [fingerprint.as_str(), token] {
        if !hex.is_ascii() {
            return Err("Invalid sharing credentials".into());
        }
        for index in (0..hex.len()).step_by(2) {
            payload.push(
                u8::from_str_radix(&hex[index..index + 2], 16)
                    .map_err(|_| "Invalid sharing credentials".to_owned())?,
            );
        }
    }
    payload.extend(session_id.as_bytes());
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
    pwa_url.set_fragment(Some(&encoded));
    Ok(pwa_url.to_string())
}

pub(crate) async fn start_http(
    bundle: Arc<Bundle>,
    incoming: UnboundedSender<Event>,
    directory: std::path::PathBuf,
    received: ReceivedSources,
) -> Result<Session, String> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| e.to_string())?;
    let origin = format!(
        "http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let url = format!("{origin}/{token}/");
    let (stop, stopped) = watch::channel(false);
    let (bundle, snapshot) = watch::channel(bundle);
    let (directory, destination) = watch::channel(directory);
    let state = ShareState {
        bundle: snapshot,
        origin,
        incoming,
        stop: stopped.clone(),
        staging: Default::default(),
        directory: destination,
        received,
    };
    let routes = Router::new()
        .route("/items", get(items))
        .route("/files/{index}", delete(remove))
        .route("/croc/files/{index}", get(croc_file))
        .route("/thumbnails/{index}", get(thumbnail))
        .route("/croc/staging", post(croc_staging))
        .route("/croc/import/{id}", post(croc_import))
        .route("/received/{share}/{index}", get(received_source))
        .route(
            "/text",
            post(receive_text).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state);
    let app = Router::new().nest(&format!("/{token}/"), routes);
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(stopping(stopped))
            .await
        {
            eprintln!("Device sharing stopped: {error}");
        }
    });
    Ok(Session {
        url,
        bundle,
        stop,
        commands: None,
        directory,
    })
}

async fn stopping(mut stop: watch::Receiver<bool>) {
    if !*stop.borrow() {
        let _ = stop.changed().await;
    }
}

async fn guard(State(state): State<ShareState>, request: Request, next: Next) -> Response {
    if *state.stop.borrow() || !valid_origin(&state.origin, request.headers()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        (
            "content-security-policy",
            "default-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

fn valid_origin(origin: &str, headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        == origin.strip_prefix("http://")
        && headers
            .get(header::ORIGIN)
            .is_none_or(|value| value.to_str().ok() == Some(origin))
        && headers
            .get("sec-fetch-site")
            .is_none_or(|value| value == "same-origin" || value == "none")
}

fn device_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-orbit-device")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(|id| id.to_string())
}

fn own_file(file: &SharedFile, headers: &HeaderMap) -> bool {
    file.origin
        .as_ref()
        .is_some_and(|origin| device_key(headers).as_deref() == Some(&origin.id))
}

async fn items(
    State(state): State<ShareState>,
    headers: HeaderMap,
) -> Json<Vec<serde_json::Value>> {
    let bundle = state.bundle.borrow().clone();
    let mut items = Vec::with_capacity(bundle.files.len());
    for file in bundle.ordered() {
        let size = tokio::fs::metadata(&file.path)
            .await
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.len());
        let text = if file.text.is_some() {
            file.text.clone()
        } else if crate::transfer::is_text_file(&file.path) {
            let path = file.path.clone();
            tokio::task::spawn_blocking(move || {
                crate::transfer::Text::File(path)
                    .contents()
                    .map(|text| text.into_owned())
            })
            .await
            .ok()
            .flatten()
        } else {
            None
        };
        items.push(serde_json::json!({
            "index":file.id, "name":file.title, "filename":file.name, "size":size, "text":text,
            "local":file.origin.is_none(), "content":file.text.is_some(),
            "downloadable":size.is_some() && !own_file(file, &headers),
            "sender": file.origin.as_ref().map_or(crate::app::COMPUTER_NAME.as_str(), |origin| origin.name.as_str()),
            "removable": own_file(file, &headers),
            "kind": match crate::thumbnail::kind(&file.path) {
                Some(crate::thumbnail::Kind::Image) => "image",
                Some(crate::thumbnail::Kind::Video) => "video",
                None if text.as_deref().is_some_and(crate::transfer::is_link) => "link",
                None if text.is_some() => "text",
                None => "file",
            },
        }));
    }
    Json(items)
}

async fn remove(
    State(state): State<ShareState>,
    Path(index): Path<usize>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    let mut updates = state.bundle.clone();
    let file = updates
        .borrow()
        .files
        .iter()
        .find(|file| file.id == index)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    if !own_file(&file, &headers) {
        return Err(StatusCode::FORBIDDEN);
    }
    state
        .incoming
        .unbounded_send(Event::Remove(file.source, file.order))
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !updates.borrow().files.iter().any(|file| file.id == index) {
                return Ok(StatusCode::NO_CONTENT);
            }
            updates.changed().await.map_err(|_| StatusCode::GONE)?;
        }
    })
    .await
    .map_err(|_| StatusCode::REQUEST_TIMEOUT)?
}

async fn thumbnail(
    State(state): State<ShareState>,
    Path(index): Path<usize>,
) -> Result<Response, StatusCode> {
    let bundle = state.bundle.borrow().clone();
    let bytes = tokio::task::spawn_blocking(move || {
        let file = bundle.files.iter().find(|file| file.id == index)?;
        file.thumbnail
            .get_or_init(|| crate::thumbnail::bytes(&file.path, 64))
            .clone()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(([(header::CONTENT_TYPE, "image/png")], bytes).into_response())
}

async fn croc_file(
    State(state): State<ShareState>,
    Path(index): Path<usize>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let bundle = state.bundle.borrow().clone();
    let file = bundle
        .files
        .iter()
        .find(|file| file.id == index)
        .ok_or(StatusCode::NOT_FOUND)?;
    if own_file(file, &headers) {
        return Err(StatusCode::FORBIDDEN);
    }
    let metadata = tokio::fs::metadata(&file.path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if !metadata.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }
    let mut data = serde_json::to_vec(&serde_json::json!({
        "path": file.path, "name": file.name, "size": metadata.len(),
    }))
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    data.push(b'\n');
    let (sender, receiver) = mpsc::channel(1);
    let _ = sender.send(Ok::<_, std::convert::Infallible>(data)).await;
    tokio::spawn(async move {
        // Keep generated files alive until croc releases the snapshot.
        let _snapshot = bundle;
        tokio::select! {
            _ = sender.closed() => {},
            _ = stopping(state.stop) => {},
        }
    });
    let body = stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|chunk| (chunk, receiver))
    });
    Ok(Body::from_stream(body).into_response())
}

fn valid_filename(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | ".." | ".lease" | ".released")
        && !name.contains(['/', '\\', '\0', '\r', '\n'])
}

fn incoming_file(directory: &std::path::Path) -> std::io::Result<tempfile::NamedTempFile> {
    if !directory.is_absolute() {
        return Err(std::io::Error::other("Invalid download directory"));
    }
    std::fs::create_dir_all(directory)?;
    tempfile::Builder::new()
        .prefix(".orbit-")
        .suffix(".part")
        .tempfile_in(directory)
}

fn save_received(
    mut file: tempfile::NamedTempFile,
    name: &str,
    directory: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    if !valid_filename(name) {
        return Err(std::io::Error::other("Invalid file name"));
    }
    file.as_file().sync_all()?;
    let original = std::path::Path::new(name);
    let mut suffix = 0_u64;
    loop {
        let filename = if suffix == 0 {
            name.to_owned()
        } else {
            let counter = format!(" ({suffix})");
            let extension = original
                .extension()
                .and_then(|value| value.to_str())
                .filter(|value| value.len() + counter.len() + 1 < 255);
            let stem = if extension.is_some() {
                original.file_stem().unwrap().to_str().unwrap()
            } else {
                name
            };
            let mut length = stem
                .len()
                .min(255 - counter.len() - extension.map_or(0, |value| value.len() + 1));
            while !stem.is_char_boundary(length) {
                length -= 1;
            }
            format!(
                "{}{}{}",
                &stem[..length],
                counter,
                extension
                    .map(|value| format!(".{value}"))
                    .unwrap_or_default()
            )
        };
        let path = directory.join(filename);
        match file.persist_noclobber(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                file = error.file;
                suffix = suffix
                    .checked_add(1)
                    .ok_or_else(|| std::io::Error::other("Too many files"))?;
            }
            Err(error) => return Err(error.error),
        }
    }
}

async fn croc_staging(
    State(state): State<ShareState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, &'static str)> {
    let sender = device_key(&headers).ok_or((StatusCode::BAD_REQUEST, "Invalid sender"))?;
    let destination = state.directory.borrow().clone();
    let directory = tokio::task::spawn_blocking(move || {
        if !destination.is_absolute() {
            return Err(std::io::Error::other("Invalid download directory"));
        }
        std::fs::create_dir_all(&destination)?;
        tempfile::Builder::new()
            .prefix(".orbit-croc-")
            .tempdir_in(destination)
    })
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Could not save file"))?
    .map_err(|_| (StatusCode::INSUFFICIENT_STORAGE, "Could not save file"))?;
    let directory = Arc::new(directory);
    let id = uuid::Uuid::new_v4();
    let mut data = serde_json::to_vec(&serde_json::json!({"id": id, "path": directory.path()}))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Could not save file"))?;
    state
        .staging
        .lock()
        .await
        .insert(id, (sender, Arc::downgrade(&directory)));
    data.push(b'\n');
    let (sender, receiver) = mpsc::channel(1);
    let _ = sender.send(Ok::<_, std::convert::Infallible>(data)).await;
    tokio::spawn(async move {
        // The lease owns unfinished files until croc exits or sharing stops.
        let _directory = directory;
        tokio::select! {
            _ = sender.closed() => {},
            _ = stopping(state.stop) => {},
        }
        state.staging.lock().await.remove(&id);
    });
    let body = stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|chunk| (chunk, receiver))
    });
    Ok(Body::from_stream(body).into_response())
}

async fn received_source(
    State(state): State<ShareState>,
    Path((share, index)): Path<(uuid::Uuid, u32)>,
    headers: HeaderMap,
) -> Result<Json<bool>, StatusCode> {
    let sender = device_key(&headers).ok_or(StatusCode::BAD_REQUEST)?;
    Ok(Json(
        state
            .received
            .lock()
            .await
            .contains(&(sender, format!("{share}/{index}"))),
    ))
}

#[derive(Deserialize)]
struct CrocImport {
    name: String,
    size: u64,
}

async fn croc_import(
    State(state): State<ShareState>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(file): Json<CrocImport>,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    let sender = device_key(&headers).ok_or((StatusCode::BAD_REQUEST, "Invalid sender"))?;
    let directory = state
        .staging
        .lock()
        .await
        .get(&id)
        .filter(|(owner, _)| owner == &sender)
        .and_then(|(_, directory)| directory.upgrade())
        .ok_or((StatusCode::NOT_FOUND, "Unknown transfer"))?;
    if !valid_filename(&file.name) {
        return Err((StatusCode::BAD_REQUEST, "Invalid file name"));
    }
    let source = headers
        .get("x-orbit-source")
        .map(|value| {
            (|| {
                let (share, index) = value.to_str().ok()?.split_once('/')?;
                let share = uuid::Uuid::parse_str(share).ok()?;
                let index = index.parse::<u32>().ok()?;
                Some((sender.clone(), format!("{share}/{index}")))
            })()
            .ok_or((StatusCode::BAD_REQUEST, "Invalid source"))
        })
        .transpose()?;
    let content = headers
        .get("x-orbit-content")
        .is_some_and(|value| value == "true");
    if content && (source.is_none() || file.size > 1024 * 1024) {
        return Err((StatusCode::BAD_REQUEST, "Invalid text"));
    }
    let mut received = state.received.lock().await;
    if source.as_ref().is_some_and(|key| received.contains(key)) {
        return Ok(StatusCode::NO_CONTENT);
    }
    let imported = tokio::task::spawn_blocking(move || {
        let mut entries = std::fs::read_dir(directory.path())
            .map_err(|_| (StatusCode::BAD_REQUEST, "Incomplete transfer"))?;
        let entry = entries
            .next()
            .and_then(Result::ok)
            .ok_or((StatusCode::BAD_REQUEST, "Incomplete transfer"))?;
        if entries.next().is_some()
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
            || !entry
                .metadata()
                .is_ok_and(|metadata| metadata.len() == file.size)
        {
            return Err((StatusCode::BAD_REQUEST, "Incomplete transfer"));
        }
        let text = if content {
            let html = std::path::Path::new(&file.name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("html"));
            Some(
                received_content(&entry.path(), html)
                    .ok_or((StatusCode::BAD_REQUEST, "Invalid text"))?,
            )
        } else {
            None
        };
        let pending = tempfile::NamedTempFile::from_parts(
            std::fs::File::open(entry.path())
                .map_err(|_| (StatusCode::INSUFFICIENT_STORAGE, "Could not save file"))?,
            tempfile::TempPath::try_from_path(entry.path())
                .map_err(|_| (StatusCode::INSUFFICIENT_STORAGE, "Could not save file"))?,
        );
        let path = save_received(pending, &file.name, directory.path().parent().unwrap())
            .map_err(|_| (StatusCode::INSUFFICIENT_STORAGE, "Could not save file"))?;
        Ok(ImportedFiles {
            files: crate::transfer::FileList(vec![path]),
            directory: None,
            text,
        })
    })
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Could not save file"))??;
    state
        .incoming
        .unbounded_send(Event::Received(imported, Some(sender)))
        .map_err(|_| (StatusCode::GONE, "Sharing stopped"))?;
    if let Some(key) = source {
        received.insert(key);
    }
    Ok(StatusCode::NO_CONTENT)
}

fn received_content(path: &std::path::Path, html: bool) -> Option<String> {
    let text = crate::transfer::Text::File(path.to_owned())
        .contents()?
        .into_owned();
    if !html {
        return Some(text);
    }
    if let Ok(document) =
        roxmltree::Document::parse(text.strip_prefix("<!DOCTYPE html>\n").unwrap_or(&text))
    {
        for node in document.descendants() {
            if node
                .attribute("http-equiv")
                .is_some_and(|value| value.eq_ignore_ascii_case("refresh"))
                && let Some(link) = node
                    .attribute("content")
                    .and_then(|value| value.strip_prefix("0;url="))
                && crate::transfer::is_link(link)
            {
                return Some(link.to_owned());
            }
        }
    }
    crate::rich_text::Document::from_html(&text)
        .ok()
        .map(|document| document.text)
}

async fn receive_text(
    State(state): State<ShareState>,
    headers: HeaderMap,
    text: String,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    let sender = device_key(&headers).ok_or((StatusCode::BAD_REQUEST, "Invalid sender"))?;
    if text.trim().is_empty() || text.len() > 1024 * 1024 {
        return Err((StatusCode::BAD_REQUEST, "Invalid text"));
    }
    let destination = state.directory.borrow().clone();
    let imported = crate::import::receive(
        text.into_bytes(),
        crate::transfer::TEXT.into(),
        crate::import::Source::Clipboard,
        |_| {},
    )
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Could not save text"))?;
    let imported = tokio::task::spawn_blocking(move || {
        let source = &imported.files.0[0];
        let mut pending = incoming_file(&destination)?;
        std::io::copy(&mut std::fs::File::open(source)?, pending.as_file_mut())?;
        let path = save_received(
            pending,
            &source.file_name().unwrap().to_string_lossy(),
            &destination,
        )?;
        Ok::<_, std::io::Error>(ImportedFiles {
            files: crate::transfer::FileList(vec![path]),
            directory: None,
            text: imported.text,
        })
    })
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Could not save text"))?
    .map_err(|_| (StatusCode::INSUFFICIENT_STORAGE, "Could not save text"))?;
    state
        .incoming
        .unbounded_send(Event::Received(imported, Some(sender)))
        .map_err(|_| (StatusCode::GONE, "Sharing stopped"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::futures::StreamExt;

    #[test]
    fn core_install_is_atomic_private_and_repairs_incomplete_files() {
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bin/orbit-peer");
        assert!(!installed_at(&path));
        install_at(&path).unwrap();
        assert!(installed_at(&path));
        let inode = path.metadata().unwrap().ino();
        install_at(&path).unwrap();
        assert_eq!(path.metadata().unwrap().ino(), inode);
        let old = std::fs::File::open(&path).unwrap();
        std::fs::write(&path, "incomplete").unwrap();
        assert!(!installed_at(&path));
        install_at(&path).unwrap();
        assert!(installed_at(&path));
        assert_ne!(
            path.metadata().unwrap().ino(),
            old.metadata().unwrap().ino()
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    async fn staging_lease(
        client: &reqwest::Client,
        session: &Session,
        sender: &str,
    ) -> (reqwest::Response, String, std::path::PathBuf) {
        let mut lease = client
            .post(format!("{}croc/staging", session.url))
            .header("x-orbit-device", sender)
            .send()
            .await
            .unwrap();
        assert_eq!(lease.status(), StatusCode::OK);
        let mut data = Vec::new();
        while !data.ends_with(b"\n") {
            data.extend_from_slice(&lease.chunk().await.unwrap().unwrap());
        }
        let stage: serde_json::Value = serde_json::from_slice(&data).unwrap();
        (
            lease,
            stage["id"].as_str().unwrap().to_owned(),
            std::path::PathBuf::from(stage["path"].as_str().unwrap()),
        )
    }

    #[test]
    fn native_content_preserves_links_and_rich_text() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("content.html");
        std::fs::write(&path, "<!DOCTYPE html>\n<html><head><meta http-equiv=\"refresh\" content=\"0;url=https://example.com/?a=1&amp;b=2\"/></head></html>").unwrap();
        assert_eq!(
            received_content(&path, true).as_deref(),
            Some("https://example.com/?a=1&b=2")
        );
        std::fs::write(
            &path,
            "<p><strong>First</strong></p><ul><li>Second</li></ul>",
        )
        .unwrap();
        let expected =
            crate::rich_text::Document::from_html(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .text;
        assert_eq!(received_content(&path, true), Some(expected));
    }

    #[tokio::test]
    async fn native_reconnect_deduplicates_per_sender_across_http_sessions() {
        let fixture = tempfile::tempdir().unwrap();
        let destination = fixture.path().join("received");
        let shelf = crate::shelf::Shelf::default();
        let bundle = Bundle::prepare(
            Vec::new(),
            Arc::new(shelf.transfer(Vec::new(), false)),
            None,
            || false,
        )
        .unwrap();
        let received: ReceivedSources = Default::default();
        let (output, mut events) = cosmic::iced::futures::channel::mpsc::unbounded();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        for (sender, already_received) in [
            ("aabbccdd-1234-4567-8901-123456789abc", false),
            ("aabbccdd-1234-4567-8901-123456789abc", true),
            ("bbccddee-1234-4567-8901-123456789abc", false),
        ] {
            let session = start_http(
                bundle.clone(),
                output.clone(),
                destination.clone(),
                received.clone(),
            )
            .await
            .unwrap();
            let source = "12345678-1234-4123-8123-123456789abc/0";
            let check = || {
                client
                    .get(format!("{}received/{source}", session.url))
                    .header("x-orbit-device", sender)
            };
            assert_eq!(
                check().send().await.unwrap().text().await.unwrap(),
                already_received.to_string()
            );
            let (lease, id, directory) = staging_lease(&client, &session, sender).await;
            assert_eq!(directory.parent().unwrap(), destination);
            std::fs::write(directory.join("note.txt"), "Hello").unwrap();
            let response = client
                .post(format!("{}croc/import/{id}", session.url))
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-orbit-device", sender)
                .header("x-orbit-source", source)
                .header("x-orbit-content", "true")
                .body(r#"{"name":"note.txt","size":5}"#)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert_eq!(check().send().await.unwrap().text().await.unwrap(), "true");
            drop(lease);
            tokio::time::timeout(Duration::from_secs(3), async {
                while directory.exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        let mut imported = 0;
        while let Ok(event) = events.try_recv() {
            if let Event::Received(files, _) = event {
                imported += 1;
                assert_eq!(files.text.as_deref(), Some("Hello"));
            }
        }
        assert_eq!(imported, 2);
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 2);
    }

    #[test]
    fn compact_invitation_preserves_the_wire_format_and_full_credentials() {
        let fingerprint = (0..32)
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(":");
        let token = (32..64)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let ip = Ipv4Addr::new(192, 168, 1, 2);
        let session_id = uuid::Uuid::parse_str("12345678-1234-4123-8123-123456789abc").unwrap();
        let pwa_url = crate::config::pwa_url("").unwrap();
        assert_eq!(
            invitation_url(pwa_url.clone(), ip, 49152, &fingerprint, &token, session_id).unwrap(),
            "https://orbit.motixo-dev.workers.dev/#AsCoAQLAAAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8SNFZ4EjRBI4EjEjRWeJq8"
        );
        assert!(
            invitation_url(
                pwa_url.clone(),
                ip,
                49152,
                &fingerprint,
                &token[..62],
                session_id
            )
            .is_err()
        );
        assert!(invitation_url(pwa_url, ip, 0, &fingerprint, &token, session_id).is_err());
    }

    #[test]
    fn uploads_and_origins_cannot_escape_the_share() {
        for name in [
            "../file",
            "/etc/passwd",
            "..",
            ".lease",
            "folder\\file",
            "x\r\ny",
        ] {
            assert!(!valid_filename(name));
        }
        assert!(valid_filename("photo 1.jpg"));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-orbit-ip", HeaderValue::from_static("192.168.1.2"));
        assert_eq!(device_key(&headers), None);
        headers.insert(
            "x-orbit-device",
            HeaderValue::from_static("aabbccdd-1234-4567-8901-123456789abc"),
        );
        assert_eq!(
            device_key(&headers).as_deref(),
            Some("aabbccdd-1234-4567-8901-123456789abc")
        );
        headers.insert(header::HOST, HeaderValue::from_static("192.168.1.2:1234"));
        assert!(valid_origin("http://192.168.1.2:1234", &headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://other.example"),
        );
        assert!(!valid_origin("http://192.168.1.2:1234", &headers));
        headers.remove(header::ORIGIN);
        headers.insert(header::HOST, HeaderValue::from_static("other.example"));
        assert!(!valid_origin("http://192.168.1.2:1234", &headers));
    }

    #[test]
    fn received_files_preserve_existing_files_and_fit_long_unicode_names() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("received");
        for name in ["photo.png".to_owned(), format!("{}.txt", "ی".repeat(125))] {
            let mut pending = incoming_file(&directory).unwrap();
            let original = directory.join(&name);
            std::fs::write(&original, "original").unwrap();
            std::io::Write::write_all(pending.as_file_mut(), b"received").unwrap();
            let saved = save_received(pending, &name, &directory).unwrap();
            assert_ne!(saved, original);
            assert_eq!(std::fs::read_to_string(original).unwrap(), "original");
            assert_eq!(std::fs::read_to_string(&saved).unwrap(), "received");
            assert!(saved.file_name().unwrap().len() <= 255);
            if name == "photo.png" {
                assert_eq!(saved.file_name().unwrap(), "photo (1).png");
            }
        }
    }

    #[test]
    fn network_picker_ignores_loopback_bridges_and_public_addresses() {
        let addresses = parse_addresses(br#"[{"ifname":"lo","addr_info":[{"local":"127.0.0.1"}]},{"ifname":"docker0","addr_info":[{"local":"172.17.0.1"}]},{"ifname":"wlan0","addr_info":[{"local":"192.168.1.2"},{"local":"8.8.8.8"}]}]"#).unwrap();
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].ip, Ipv4Addr::new(192, 168, 1, 2));
    }

    #[test]
    fn unresponsive_devices_keep_their_identity_and_recover_independently() {
        let now = Instant::now();
        let remote = |ip, name: &str, id: &str| ConnectedDevice {
            ip: Ipv4Addr::new(192, 168, 1, ip),
            name: name.into(),
            pair_id: id.into(),
            reconnecting: false,
            unresponsive_since: None,
        };
        let mut connected_devices =
            vec![remote(2, "iPhone", "first"), remote(3, "Pixel", "second")];
        update_devices(&mut connected_devices, vec![remote(2, "", "first")], now);
        assert!(!connected_devices[0].reconnecting);
        assert_eq!(connected_devices[0].name, "iPhone");
        assert!(connected_devices[1].reconnecting);

        update_devices(
            &mut connected_devices,
            Vec::new(),
            now + Duration::from_secs(3),
        );
        assert!(connected_devices.iter().all(|remote| remote.reconnecting));
        assert!(!connected_devices[0].expired(now + Duration::from_millis(3500)));
        assert!(connected_devices[1].expired(now + Duration::from_secs(1)));
        update_devices(
            &mut connected_devices,
            vec![remote(4, "", "first"), remote(3, "", "second")],
            now + Duration::from_secs(4),
        );
        assert_eq!(connected_devices.len(), 2);
        assert!(connected_devices.iter().all(|remote| !remote.reconnecting));
        assert_eq!(connected_devices[0].name, "iPhone");
        assert_eq!(connected_devices[0].ip, Ipv4Addr::new(192, 168, 1, 4));
        assert_eq!(connected_devices[1].name, "Pixel");
        assert!(
            connected_devices
                .iter()
                .all(|remote| !remote.expired(now + Duration::from_secs(30)))
        );

        connected_devices[1].mark_reconnecting(now + Duration::from_secs(6));
        connected_devices.retain(|remote| !remote.expired(now + Duration::from_secs(11)));
        let mut stale = remote(3, "Pixel", "second");
        stale.reconnecting = true;
        update_devices(
            &mut connected_devices,
            vec![remote(4, "", "first"), stale],
            now + Duration::from_secs(12),
        );
        assert_eq!(connected_devices.len(), 1);
        update_devices(
            &mut connected_devices,
            vec![remote(4, "", "first"), remote(3, "Pixel", "second")],
            now + Duration::from_secs(13),
        );
        assert_eq!(connected_devices.len(), 2);
        assert!(connected_devices.iter().all(|remote| !remote.reconnecting));
    }

    #[test]
    fn pairing_on_the_same_address_does_not_replace_another_sender() {
        let now = Instant::now();
        let devices = |ids: &[&str]| {
            ids.iter()
                .map(|id| {
                    serde_json::from_value(serde_json::json!({
                        "ip": "192.168.1.2", "pair_id": id
                    }))
                    .unwrap()
                })
                .collect()
        };
        let mut known = Vec::new();
        update_devices(&mut known, devices(&["first", "second"]), now);
        assert_eq!(known.len(), 2);
        update_devices(&mut known, devices(&["first", "second", ""]), now);
        assert_eq!(known.len(), 3);
        update_devices(&mut known, devices(&["first", "second", "third"]), now);
        assert_eq!(known.len(), 3);
        assert!(known.iter().all(|device| !device.reconnecting));
        assert!(known.iter().any(|device| device.is_sender("first")));
        assert!(known.iter().any(|device| device.is_sender("second")));
        assert!(known.iter().any(|device| device.is_sender("third")));
    }

    #[test]
    fn sender_names_use_device_aliases_and_keep_unpaired_device_names() {
        let connected_devices: Vec<ConnectedDevice> = serde_json::from_str(
            r#"[
            {"ip":"192.168.1.2", "name":"iPhone", "pair_id":"first"},
            {"ip":"192.168.1.3", "name":"Pixel"}
        ]"#,
        )
        .unwrap();
        let pairings = vec![PairedDevice {
            id: "first".into(),
            name: "My device".into(),
            desktop: false,
            allow_open: false,
        }];
        assert_eq!(
            sender_name("first", &pairings, &connected_devices),
            Some("My device")
        );
        assert_eq!(
            sender_name("ip:192.168.1.2", &pairings, &connected_devices),
            Some("My device")
        );
        assert_eq!(
            sender_name("ip:192.168.1.3", &pairings, &connected_devices),
            Some("Pixel")
        );
        assert_eq!(sender_name("first", &pairings, &[]), Some("My device"));
        assert_eq!(sender_name("unknown", &pairings, &connected_devices), None);
    }

    #[test]
    fn transfer_progress_measures_bytes_and_clears_when_dropped() {
        for direction in [Direction::Download, Direction::Upload] {
            let (output, mut input) = cosmic::iced::futures::channel::mpsc::unbounded();
            let mut download = Transfer::new(output, Some(4096), direction);
            let id = download.id;
            let started = download.sampled;
            assert!(matches!(
                input.try_recv(),
                Ok(Event::Transfer(
                    _,
                    _,
                    Some(TransferProgress { transferred: 0, .. })
                ))
            ));
            download.progress.transferred = 2048;
            download.report(started + Duration::from_secs(1));
            let Ok(Event::Transfer(event_id, event_direction, Some(progress))) = input.try_recv()
            else {
                panic!("Expected transfer progress");
            };
            assert_eq!(event_id, id);
            assert_eq!(event_direction, direction);
            assert_eq!(progress.transferred, 2048);
            assert_eq!(progress.total, Some(4096));
            assert_eq!(progress.speed, 2048.0);
            download.report(started + Duration::from_secs(2));
            assert!(matches!(
                input.try_recv(),
                Ok(Event::Transfer(
                    _,
                    _,
                    Some(TransferProgress { speed: 0.0, .. })
                ))
            ));
            drop(download);
            assert!(
                matches!(input.try_recv(), Ok(Event::Transfer(event_id, event_direction, None)) if event_id == id && event_direction == direction)
            );
        }
    }

    #[tokio::test]
    async fn croc_snapshots_live_until_the_transfer_releases_them() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("file.bin");
        std::fs::write(&path, vec![0; 1024 * 1024]).unwrap();
        let mut shelf = crate::shelf::Shelf::default();
        shelf.add([path]);
        let bundle = Bundle::prepare(
            shelf.items.clone(),
            Arc::new(shelf.transfer(shelf.export_paths(), false)),
            None,
            || false,
        )
        .unwrap();
        let weak = Arc::downgrade(&bundle);
        let (incoming, _) = cosmic::iced::futures::channel::mpsc::unbounded();
        let (_stop, stop) = watch::channel(false);
        let state = ShareState {
            bundle: watch::channel(bundle).1,
            directory: watch::channel(fixture.path().join("received")).1,
            origin: "http://127.0.0.1".into(),
            incoming,
            stop,
            staging: Default::default(),
            received: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
        };
        let response = croc_file(State(state), Path(0), HeaderMap::new())
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        let data = tokio::time::timeout(Duration::from_secs(3), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let snapshot: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(snapshot["size"], 1024 * 1024);
        assert!(weak.upgrade().is_some());
        drop(body);
        tokio::time::timeout(Duration::from_secs(3), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn local_api_refreshes_text_updates_files_and_accepts_complete_uploads() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("note.txt");
        std::fs::write(&path, "<private>").unwrap();
        let mut shelf = crate::shelf::Shelf::default();
        shelf.add([path.clone()]);
        let bundle = Bundle::prepare(
            shelf.items.clone(),
            Arc::new(shelf.transfer(shelf.export_paths(), false)),
            None,
            || false,
        )
        .unwrap();
        let (output, mut input) = cosmic::iced::futures::channel::mpsc::unbounded();
        let session = start_http(
            bundle,
            output,
            fixture.path().join("received"),
            Default::default(),
        )
        .await
        .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        for text in ["<private>", "<updated>"] {
            std::fs::write(&path, text).unwrap();
            let response = client
                .get(format!("{}items", session.url))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let listed: serde_json::Value =
                serde_json::from_str(&response.text().await.unwrap()).unwrap();
            assert_eq!(listed[0]["text"], text);
            assert_eq!(listed[0]["kind"], "text");
        }
        let mut snapshot = client
            .get(format!("{}croc/files/0", session.url))
            .send()
            .await
            .unwrap();
        assert_eq!(snapshot.status(), StatusCode::OK);
        let mut data = Vec::new();
        while !data.ends_with(b"\n") {
            data.extend_from_slice(&snapshot.chunk().await.unwrap().unwrap());
        }
        let metadata: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(metadata["path"], path.to_str().unwrap());
        assert_eq!(metadata["size"], 9);
        drop(snapshot);
        assert_eq!(
            client
                .get(format!("{}croc/files/1", session.url))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .get(format!("{}items", session.url))
                .header(header::ORIGIN, "http://other.example")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let mut wrong = url::Url::parse(&session.url).unwrap();
        wrong.set_path("/wrong/");
        assert_eq!(
            client.get(wrong).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        use std::os::unix::fs::{MetadataExt, symlink};
        let sender = "aabbccdd-1234-4567-8901-123456789abc";
        let (lease, id, staging) = staging_lease(&client, &session, sender).await;
        let received_path = staging.join("remote.txt");
        symlink(&path, &received_path).unwrap();
        let import = |name: &str, size: u64, device: &str| {
            client
                .post(format!("{}croc/import/{id}", session.url))
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-orbit-device", device)
                .body(serde_json::json!({"name": name, "size": size}).to_string())
        };
        assert_eq!(
            import("remote.txt", 9, sender)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        std::fs::remove_file(&received_path).unwrap();
        std::fs::write(&received_path, "from orbit").unwrap();
        let inode = received_path.metadata().unwrap().ino();
        assert_eq!(
            import("remote.txt", 10, "bbccddee-1234-4567-8901-123456789abc")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            import("remote.txt", 9, sender)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            import("../escape", 10, sender)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            import("remote.txt", 10, sender)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert!(!received_path.exists());
        assert_eq!(
            fixture
                .path()
                .join("received/remote.txt")
                .metadata()
                .unwrap()
                .ino(),
            inode
        );
        drop(lease);
        let Event::Received(received, remote) =
            tokio::time::timeout(Duration::from_secs(3), input.next())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("Expected received file");
        };
        assert_eq!(
            remote.as_deref(),
            Some("aabbccdd-1234-4567-8901-123456789abc")
        );
        assert_eq!(
            std::fs::read_to_string(&received.files.0[0]).unwrap(),
            "from orbit"
        );
        assert_eq!(
            received.files.0[0].parent().unwrap(),
            fixture.path().join("received")
        );
        assert!(received.directory.is_none());
        let custom = fixture.path().join("custom");
        session.set_directory(custom.clone());
        assert_eq!(
            client
                .post(format!("{}text", session.url))
                .header(header::CONTENT_TYPE, "text/plain")
                .header("x-orbit-device", sender)
                .body("https://example.com")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        let Event::Received(link, _) = tokio::time::timeout(Duration::from_secs(3), input.next())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("Expected a link");
        };
        assert_eq!(link.text.as_deref(), Some("https://example.com"));
        assert_eq!(link.files.0[0].parent().unwrap(), custom);
        assert!(link.directory.is_none());
        let text = "\"<>\n".repeat(256 * 1024);
        assert_eq!(text.len(), 1024 * 1024);
        assert_eq!(
            client
                .post(format!("{}text", session.url))
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .header("x-orbit-device", sender)
                .body(text.clone())
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT,
        );
        let Event::Received(large, _) = input.next().await.unwrap() else {
            panic!("Expected full-sized text");
        };
        assert_eq!(large.text.as_deref(), Some(text.as_str()));
        assert_eq!(large.files.0[0].parent().unwrap(), custom);
        let original = session.bundle.borrow().clone();
        let added = fixture.path().join("new.txt");
        std::fs::write(&added, "new").unwrap();
        shelf.add([added.clone()]);
        shelf.items.last_mut().unwrap().origin = Some(crate::shelf::Sender {
            id: "aabbccdd-1234-4567-8901-123456789abc".into(),
            name: "iPhone".into(),
        });
        session.update(
            Bundle::prepare(
                shelf.items.clone(),
                Arc::new(shelf.transfer(shelf.export_paths(), false)),
                Some(&original),
                || false,
            )
            .unwrap(),
        );
        let list = client
            .get(format!("{}items", session.url))
            .header("x-orbit-device", "aabbccdd-1234-4567-8901-123456789abc")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let list: serde_json::Value = serde_json::from_str(&list).unwrap();
        assert_eq!(list[0]["index"], 1);
        assert_eq!(list[0]["downloadable"], false);
        assert_eq!(list[0]["sender"], "iPhone");
        assert_eq!(list[0]["removable"], true);
        assert_eq!(list[0]["kind"], "text");
        assert_eq!(list[1]["downloadable"], true);
        assert_eq!(list[1]["removable"], false);
        assert_eq!(
            client
                .get(format!("{}croc/files/1", session.url))
                .header("x-orbit-device", "aabbccdd-1234-4567-8901-123456789abc")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(list[1]["index"], 0);
        let other_list = client
            .get(format!("{}items", session.url))
            .header("x-orbit-device", "aabbccdd-1234-4567-8901-123456789abd")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let other_list: serde_json::Value = serde_json::from_str(&other_list).unwrap();
        assert_eq!(other_list[0]["downloadable"], true);
        assert_eq!(other_list[0]["removable"], false);
        for (index, device) in [
            (0, "aabbccdd-1234-4567-8901-123456789abc"),
            (1, "aabbccdd-1234-4567-8901-123456789abd"),
            (1, ""),
        ] {
            assert_eq!(
                client
                    .delete(format!("{}files/{index}", session.url))
                    .header("x-orbit-device", device)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }

        let previous = session.bundle.borrow().clone();
        let removal = {
            let client = client.clone();
            let url = format!("{}files/1", session.url);
            tokio::spawn(async move {
                client
                    .delete(url)
                    .header("x-orbit-device", "aabbccdd-1234-4567-8901-123456789abc")
                    .send()
                    .await
                    .unwrap()
                    .status()
            })
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Event::Remove(source, order) = input.next().await.unwrap() {
                    assert_eq!(source, added);
                    assert_eq!(
                        order,
                        shelf
                            .items
                            .iter()
                            .find(|item| item.path == added)
                            .unwrap()
                            .order
                    );
                    break;
                }
            }
        })
        .await
        .unwrap();
        shelf.remove(&added);
        session.update(
            Bundle::prepare(
                shelf.items.clone(),
                Arc::new(shelf.transfer(shelf.export_paths(), false)),
                Some(&previous),
                || false,
            )
            .unwrap(),
        );
        assert_eq!(removal.await.unwrap(), StatusCode::NO_CONTENT);
        assert!(added.is_file());
        assert_eq!(
            client
                .get(format!("{}croc/files/1", session.url))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let mut snapshot = client
            .get(format!("{}croc/files/0", session.url))
            .send()
            .await
            .unwrap();
        assert_eq!(snapshot.status(), StatusCode::OK);
        let mut data = Vec::new();
        while !data.ends_with(b"\n") {
            data.extend_from_slice(&snapshot.chunk().await.unwrap().unwrap());
        }
        let metadata: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(metadata["path"], path.to_str().unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "<updated>");
        drop(snapshot);
        let url = format!("{}items", session.url);
        drop(session);
        if let Ok(response) = client.get(url).send().await {
            assert_ne!(response.status(), StatusCode::OK);
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), "<updated>");
        let received_path = received.files.0[0].clone();
        drop(received);
        assert_eq!(
            std::fs::read_to_string(received_path).unwrap(),
            "from orbit"
        );
    }
}
