// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use localsend::{
    discovery::{
        DEFAULT_DISCOVERY_TIMEOUT, DeviceChannel, DeviceIdentity, DiscoveryConfig, DiscoveryHandle,
    },
    http::{
        client::LsHttpClientV2,
        dto_v2::{PrepareUploadRequestDtoV2, RegisterDtoV2},
        server::{
            ServerConfigV2, ServerHandle, TlsConfig,
            common::save::FileUploadTarget,
            start_with_port,
            v2::{PrepareUploadDecisionV2, ServerEventV2},
        },
        state::ClientInfo,
    },
    model::{
        discovery::{DeviceType, PROTOCOL_VERSION_V2, ProtocolType},
        transfer::{FileContent, FileDto},
    },
    multicast::{
        DEFAULT_MULTICAST_GROUP, DEFAULT_MULTICAST_GROUP_V6, DEFAULT_PORT, MulticastDevice,
    },
    util::interface::{InterfaceFilter, local_interface_addresses},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, oneshot},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Command {
    Refresh,
    Send(SendRequest),
    Answer { id: String, accept: bool },
    Cancel,
    Stop,
}

#[derive(Deserialize)]
struct SendRequest {
    device: String,
    files: Vec<PathBuf>,
    text: Option<String>,
}

fn emit(value: serde_json::Value) {
    println!("{value}");
}

#[derive(serde::Serialize, Deserialize)]
struct Identity {
    cert: String,
    key: String,
    fingerprint: String,
}

impl Identity {
    fn load(directory: &Path) -> Result<Self> {
        let path = directory.join("identity.json");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let cert = localsend::crypto::cert::generate_self_signed()?;
                let identity = Self {
                    cert: cert.certificate_pem,
                    key: cert.private_key_pem,
                    fingerprint: cert.fingerprint,
                };
                let mut file = tempfile::NamedTempFile::new_in(directory)?;
                file.as_file()
                    .set_permissions(std::fs::Permissions::from_mode(0o600))?;
                file.write_all(&serde_json::to_vec(&identity)?)?;
                file.as_file().sync_all()?;
                file.persist_noclobber(path)?;
                Ok(identity)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn info(&self, name: &str, port: u16) -> RegisterDtoV2 {
        RegisterDtoV2 {
            alias: name.to_owned(),
            version: PROTOCOL_VERSION_V2.to_owned(),
            device_model: Some("Orbit".into()),
            device_type: Some(DeviceType::Desktop),
            fingerprint: self.fingerprint.clone(),
            port,
            protocol: ProtocolType::Https,
            download: false,
        }
    }

    fn client(&self, fingerprint: String) -> Result<LsHttpClientV2> {
        Ok(LsHttpClientV2::try_new(
            &self.key,
            &self.cert,
            Some(fingerprint),
            None,
        )?)
    }
}

struct Offer {
    id: String,
    files: HashMap<String, FileDto>,
    reply: oneshot::Sender<PrepareUploadDecisionV2>,
}

struct Receiving {
    id: String,
    stage: tempfile::TempDir,
    pending: usize,
    ended: bool,
    failed: bool,
    roots: HashSet<std::ffi::OsString>,
    last_activity: Instant,
}

impl Receiving {
    fn expired(&self, now: Instant) -> bool {
        !self.ended && now.duration_since(self.last_activity) >= Duration::from_secs(60)
    }
}

enum Finished {
    Activity(String),
    File {
        session: String,
        id: String,
        path: PathBuf,
        result: Result<(), String>,
    },
    SendStarted {
        id: String,
        host: String,
    },
    Send(Result<()>),
}

fn relative_path(name: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty() && name.len() <= 4096 && !name.contains(['\\', '\0', '\r', '\n']),
        "Invalid file name"
    );
    let path = Path::new(name);
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::Normal(name) if name.len() <= 255)),
        "Invalid file name"
    );
    Ok(path.to_owned())
}

fn validate_files(files: &HashMap<String, FileDto>) -> Result<()> {
    ensure!(!files.is_empty(), "No files offered");
    let paths = files
        .values()
        .map(|file| relative_path(&file.file_name))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        files.iter().all(|(id, file)| id == &file.id),
        "Invalid file identifier"
    );
    let unique: HashSet<_> = paths.iter().collect();
    ensure!(unique.len() == paths.len(), "Duplicate file names");
    for path in &paths {
        ensure!(
            !path
                .ancestors()
                .skip(1)
                .any(|parent| unique.contains(&parent.to_path_buf())),
            "Conflicting file names"
        );
    }
    Ok(())
}

fn message(files: &HashMap<String, FileDto>) -> Option<&str> {
    let file = (files.len() == 1)
        .then(|| files.values().next())
        .flatten()?;
    ((file.file_type == "text" || file.file_type.starts_with("text/"))
        && file
            .preview
            .as_ref()
            .is_some_and(|text| text.len() <= 1024 * 1024))
    .then_some(file.preview.as_deref())
    .flatten()
}

fn numbered_name(name: &str, index: usize) -> String {
    if index == 0 {
        return name.to_owned();
    }
    let path = Path::new(name);
    let extension = path
        .extension()
        .map(|part| format!(".{}", part.to_string_lossy()))
        .filter(|extension| extension.len() < 240)
        .unwrap_or_default();
    let stem = if extension.is_empty() {
        name
    } else {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(name)
    };
    let suffix = format!(" ({index}){extension}");
    let mut length = stem.len().min(255usize.saturating_sub(suffix.len()));
    while !stem.is_char_boundary(length) {
        length -= 1;
    }
    format!("{}{suffix}", &stem[..length])
}

fn unique_name(name: &str, used: &mut HashSet<String>) -> String {
    for index in 0.. {
        let candidate = numbered_name(name, index);
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!()
}

fn publish(
    stage: &Path,
    directory: &Path,
    roots: &HashSet<std::ffi::OsString>,
) -> Result<Vec<PathBuf>> {
    let mut saved = Vec::new();
    for entry in std::fs::read_dir(stage)? {
        let entry = entry?;
        let name = entry.file_name();
        if !roots.contains(&name) {
            continue;
        }
        let mut index = 0;
        loop {
            let target = directory.join(numbered_name(&name.to_string_lossy(), index));
            match rustix::fs::renameat_with(
                rustix::fs::CWD,
                entry.path(),
                rustix::fs::CWD,
                &target,
                rustix::fs::RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {
                    saved.push(target);
                    break;
                }
                Err(rustix::io::Errno::EXIST) => index += 1,
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(saved)
}

fn finish_receive(receiving: &mut Option<Receiving>, directory: &Path) -> Result<()> {
    if !receiving
        .as_ref()
        .is_some_and(|state| state.ended && state.pending == 0)
    {
        return Ok(());
    }
    let state = receiving.take().unwrap();
    let stage = state.stage.keep();
    let paths = match publish(&stage, directory, &state.roots) {
        Ok(paths) => {
            let _ = std::fs::remove_dir_all(&stage);
            paths
        }
        Err(error) => {
            emit(json!({"event":"received","paths":[stage]}));
            emit(
                json!({"event":"done","receiving":true,"error":format!("Could not finish saving files: {error}")}),
            );
            return Ok(());
        }
    };
    if !paths.is_empty() {
        emit(json!({"event":"received", "paths":paths}));
    }
    emit(
        json!({"event":"done", "receiving":true,"error":state.failed.then_some("Some files could not be received")}),
    );
    Ok(())
}

async fn file_result(
    session: String,
    id: String,
    file: FileDto,
    path: PathBuf,
    result: oneshot::Receiver<Result<(), String>>,
    mut progress: mpsc::Receiver<u64>,
    finished: mpsc::Sender<Finished>,
) {
    let mut result = result;
    let mut bytes = 0;
    let mut reported = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let outcome = loop {
        tokio::select! {
            value = &mut result => break value.unwrap_or_else(|_| Err("Transfer ended early".into())),
            Some(value) = progress.recv() => bytes = value,
            _ = tick.tick() => {
                if bytes != reported {
                    let _ = finished.try_send(Finished::Activity(session.clone()));
                    reported = bytes;
                }
                emit(json!({"event":"progress", "id":id,"name":file.file_name,"done":bytes,"total":file.size,"receiving":true}));
            },
        }
    };
    if outcome.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
    }
    let _ = finished
        .send(Finished::File {
            session,
            id,
            path,
            result: outcome,
        })
        .await;
}

async fn send(
    identity: Arc<Identity>,
    info: RegisterDtoV2,
    discovery: Arc<DiscoveryHandle>,
    request: SendRequest,
    cancel: CancellationToken,
    finished: mpsc::Sender<Finished>,
) -> Result<()> {
    let SendRequest {
        device,
        files: paths,
        text,
    } = request;
    let device = discovery
        .device_by_fingerprint(&device)
        .context("Device unavailable")?;
    let channel = device.get_best_channel().context("Device unavailable")?;
    let DeviceChannel::Http(http) = channel;
    ensure!(
        http.protocol == ProtocolType::Https,
        "Encrypted connection required"
    );
    let client = identity.client(device.device.fingerprint.clone())?;
    let mut files = HashMap::new();
    let mut sources = HashMap::new();
    if let Some(text) = text {
        ensure!(text.len() <= 1024 * 1024, "Text is too large");
        files.insert(
            "text".into(),
            FileDto {
                id: "text".into(),
                file_name: "message.txt".into(),
                size: text.len() as u64,
                file_type: "text".into(),
                sha256: None,
                preview: Some(text),
                metadata: None,
            },
        );
    } else {
        let mut names = HashSet::new();
        for path in paths {
            let metadata = tokio::fs::metadata(&path).await?;
            ensure!(path.is_absolute() && metadata.is_file(), "Not a file");
            let id = uuid::Uuid::new_v4().to_string();
            files.insert(
                id.clone(),
                FileDto {
                    id: id.clone(),
                    file_name: unique_name(
                        &path
                            .file_name()
                            .context("Invalid file name")?
                            .to_string_lossy(),
                        &mut names,
                    ),
                    size: metadata.len(),
                    file_type: mime_guess::from_path(&path)
                        .first_or_octet_stream()
                        .to_string(),
                    sha256: None,
                    preview: None,
                    metadata: None,
                },
            );
            sources.insert(id, path);
        }
    }
    ensure!(!files.is_empty(), "No files selected");
    let prepared = client
        .prepare_upload(
            http.protocol,
            &http.host,
            http.port,
            None,
            PrepareUploadRequestDtoV2 {
                info,
                files: files.clone(),
            },
            None,
            cancel.clone(),
        )
        .await?;
    let Some(prepared) = prepared.response else {
        return Ok(());
    };
    let _ = finished
        .send(Finished::SendStarted {
            id: prepared.session_id.clone(),
            host: http.host.clone(),
        })
        .await;
    for (id, token) in &prepared.files {
        let file = files.get(id).context("Unknown accepted file")?;
        let path = sources.get(id).context("Missing file")?;
        let mut done = 0;
        let mut last = std::time::Instant::now() - Duration::from_secs(1);
        let (id, name, total) = (id.clone(), file.file_name.clone(), file.size);
        let body = localsend::reqwest::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(FileContent::Path(path.clone()).into_receiver()).map(move |bytes| {
            done += bytes.len() as u64;
            if last.elapsed() >= Duration::from_millis(100) {
                emit(json!({"event":"progress","id":id,"name":name,"done":done,"total":total,"receiving":false}));
                last = std::time::Instant::now();
            }
            Ok::<_, std::io::Error>(bytes)
        }));
        let result = client
            .upload(
                http.protocol,
                &http.host,
                http.port,
                None,
                &prepared.session_id,
                &file.id,
                token,
                body,
                cancel.clone(),
            )
            .await;
        emit(json!({"event":"progress_end","id":file.id}));
        if let Err(error) = result {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                client.cancel(http.protocol, &http.host, http.port, &prepared.session_id),
            )
            .await;
            return Err(error.into());
        }
    }
    Ok(())
}

type Server = (ServerHandle, oneshot::Sender<()>);

async fn start_server(
    port: u16,
    identity: &Identity,
    info: ClientInfo,
    events: mpsc::Sender<ServerEventV2>,
) -> Result<Server> {
    let (stop_tx, stop_rx) = oneshot::channel();
    let handle = start_with_port(
        port,
        Some(TlsConfig {
            cert: identity.cert.clone(),
            private_key: identity.key.clone(),
        }),
        info,
        None,
        Some(ServerConfigV2 {
            pin: None,
            verify_checksums: true,
            event_tx: events,
        }),
        None,
        stop_rx,
    )
    .await?;
    Ok((handle, stop_tx))
}

async fn restart_server(
    server: Server,
    identity: &Identity,
    info: ClientInfo,
    events: mpsc::Sender<ServerEventV2>,
) -> Result<Server> {
    let port = server.0.port();
    let _ = server.1.send(());
    // Clearing a core session alone leaves active uploads running.
    server.0.wait_stopped().await;
    start_server(port, identity, info, events).await
}

async fn run(directory: PathBuf, name: String) -> Result<()> {
    ensure!(
        directory.is_absolute() && directory.to_str().is_some(),
        "Invalid download folder"
    );
    std::fs::create_dir_all(&directory)?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("Missing runtime directory")?;
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(Path::new(&runtime).join("orbit-localsend.lock"))?;
    lease
        .try_lock()
        .context("LocalSend is already open in another Orbit")?;
    let data = dirs::data_dir()
        .context("Missing data directory")?
        .join("io.github.motixo.Orbit/localsend");
    std::fs::create_dir_all(&data)?;
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700))?;
    let identity = Arc::new(Identity::load(&data)?);
    let (server_tx, mut server_rx) = mpsc::channel(32);
    let info = ClientInfo {
        alias: name.clone(),
        version: PROTOCOL_VERSION_V2.into(),
        device_model: Some("Orbit".into()),
        device_type: Some(DeviceType::Desktop),
        token: identity.fingerprint.clone(),
    };
    let mut server =
        match start_server(DEFAULT_PORT, &identity, info.clone(), server_tx.clone()).await {
            Ok(server) => server,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::AddrInUse) =>
            {
                start_server(0, &identity, info.clone(), server_tx.clone()).await?
            }
            Err(error) => return Err(error),
        };
    let port = server.0.port();
    let (discovery_tx, mut discovery_rx) = mpsc::channel(32);
    let (discovery_stop_tx, discovery_stop_rx) = oneshot::channel();
    let discovery = Arc::new(
        localsend::discovery::start(
            DiscoveryConfig {
                group: DEFAULT_MULTICAST_GROUP,
                group_v6: Some(DEFAULT_MULTICAST_GROUP_V6),
                port: DEFAULT_PORT,
                interface_filter: InterfaceFilter::default(),
                device: MulticastDevice {
                    alias: name.clone(),
                    version: PROTOCOL_VERSION_V2.into(),
                    device_model: Some("Orbit".into()),
                    device_type: Some(DeviceType::Desktop),
                    fingerprint: identity.fingerprint.clone(),
                    port,
                    protocol: ProtocolType::Https,
                    download: false,
                },
                identity: DeviceIdentity {
                    cert_pem: identity.cert.clone(),
                    private_key_pem: identity.key.clone(),
                },
                timeout: DEFAULT_DISCOVERY_TIMEOUT,
                event_tx: Some(discovery_tx),
            },
            discovery_stop_rx,
        )
        .await,
    );
    emit(json!({"event":"ready"}));
    refresh(discovery.clone());
    let (finished_tx, mut finished_rx) = mpsc::channel(32);
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let mut offer: Option<Offer> = None;
    let mut receiving: Option<Receiving> = None;
    let mut sending: Option<CancellationToken> = None;
    let mut sending_target: Option<(String, String)> = None;
    let mut idle = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            line = input.next_line() => {
                let Some(line) = line? else { break; };
                let command: Command = serde_json::from_str(&line)?;
                match command {
                    Command::Stop => break,
                    Command::Refresh => refresh(discovery.clone()),
                    Command::Cancel => {
                        if let Some(cancel) = &sending { cancel.cancel(); }
                        if let Some(state) = receiving.as_mut().filter(|state| !state.ended) {
                            state.ended = true;
                            state.failed = true;
                            server = restart_server(server, &identity, info.clone(), server_tx.clone()).await?;
                            finish_receive(&mut receiving, &directory)?;
                        }
                    },
                    Command::Answer { id, accept } => {
                        if !offer.as_ref().is_some_and(|offer| offer.id == id) { continue; }
                        let request = offer.take().unwrap();
                        if !accept { let _ = request.reply.send(PrepareUploadDecisionV2::Decline); continue; }
                        if let Some(text) = message(&request.files) {
                            emit(json!({"event":"text","text":text}));
                            let _ = request.reply.send(PrepareUploadDecisionV2::Accept(HashSet::new()));
                        } else {
                            let stage = tempfile::Builder::new().prefix(".orbit-localsend-").tempdir_in(&directory)?;
                            receiving = Some(Receiving { id:request.id, stage, pending:0, ended:false, failed:false, roots:HashSet::new(), last_activity:Instant::now() });
                            emit(json!({"event":"receiving"}));
                            let _ = request.reply.send(PrepareUploadDecisionV2::Accept(request.files.keys().cloned().collect()));
                        }
                    }
                    Command::Send(request) => {
                        if sending.is_some() || receiving.is_some() || offer.is_some() { emit(json!({"event":"done","receiving":false,"error":"A transfer is already in progress"})); continue; }
                        let cancel = CancellationToken::new();
                        sending = Some(cancel.clone());
                        let info = identity.info(&name, port);
                        let (identity, discovery, finished) = (identity.clone(), discovery.clone(), finished_tx.clone());
                        tokio::spawn(async move { let result = send(identity, info, discovery, request, cancel, finished.clone()).await; let _ = finished.send(Finished::Send(result)).await; });
                    }
                }
            }
            Some(event) = server_rx.recv() => match event {
                ServerEventV2::Register { ip, info } => {
                    discovery.add_device(localsend::discovery::DiscoveredDevice { alias:info.alias, version:info.version, device_model:info.device_model, device_type:info.device_type, fingerprint:info.fingerprint, download:info.download,
                        channel:DeviceChannel::Http(localsend::discovery::HttpChannel { host:ip.to_string(), port:info.port, protocol:info.protocol }) }).await;
                    devices(&discovery);
                }
                ServerEventV2::PrepareUpload { session_id, info, cert_fingerprint, files, decision_tx, .. } => {
                    if sending.is_some() || offer.is_some() || receiving.is_some() || cert_fingerprint.is_none() || validate_files(&files).is_err() {
                        let _ = decision_tx.send(PrepareUploadDecisionV2::Decline); continue;
                    }
                    emit(json!({"event":"request","id":session_id,"sender":info.alias,"names":files.values().map(|f| &f.file_name).take(20).collect::<Vec<_>>(),"count":files.len()}));
                    offer = Some(Offer { id:session_id, files, reply:decision_tx });
                }
                ServerEventV2::PrepareUploadAborted { session_id } => {
                    if offer.as_ref().is_some_and(|offer| offer.id == session_id) { offer = None; emit(json!({"event":"request_closed"})); }
                }
                ServerEventV2::FileUpload { session_id, file_id, file, target_tx } => {
                    let Some(state) = receiving.as_mut().filter(|state| state.id == session_id && !state.ended) else { continue; };
                    state.last_activity = Instant::now();
                    let path = state.stage.path().join(relative_path(&file.file_name)?);
                    std::fs::create_dir_all(path.parent().unwrap())?;
                    state.pending += 1;
                    let (result_tx, result_rx) = oneshot::channel();
                    let (progress_tx, progress_rx) = mpsc::channel(2);
                    let _ = target_tx.send(FileUploadTarget::Path { path:path.clone(), result_tx, progress_tx:Some(progress_tx) });
                    tokio::spawn(file_result(session_id, file_id, file, path, result_rx, progress_rx, finished_tx.clone()));
                }
                ServerEventV2::SessionEnd { session_id, reason } => {
                    if let Some(state) = receiving.as_mut().filter(|state| state.id == session_id) {
                        let cancelled = reason == localsend::http::server::v2::SessionEndReasonV2::Cancelled;
                        let restart = cancelled && !state.ended && state.pending > 0;
                        state.ended = true;
                        state.failed |= cancelled;
                        if restart { server = restart_server(server, &identity, info.clone(), server_tx.clone()).await?; }
                    }
                    finish_receive(&mut receiving, &directory)?;
                }
                ServerEventV2::CancelReceived { ip, session_id } => {
                    if sending_target.as_ref().is_some_and(|(id, host)| id == &session_id && host == &ip.to_string())
                        && let Some(cancel) = &sending { cancel.cancel(); }
                },
                ServerEventV2::ListenerFailed { error } => bail!(error),
            },
            Some(event) = finished_rx.recv() => match event {
                Finished::Activity(session) => {
                    if let Some(state) = receiving.as_mut().filter(|state| state.id == session) {
                        state.last_activity = Instant::now();
                    }
                }
                Finished::File { session, id, path, result } => {
                    emit(json!({"event":"progress_end","id":id}));
                    if let Some(state) = receiving.as_mut().filter(|state| state.id == session) {
                        let restart = result.is_err() && !state.ended;
                        state.last_activity = Instant::now();
                        state.pending -= 1;
                        state.failed |= result.is_err();
                        if result.is_err() { state.ended = true; }
                        if result.is_ok() && let Ok(relative) = path.strip_prefix(state.stage.path())
                            && let Some(Component::Normal(root)) = relative.components().next() { state.roots.insert(root.to_owned()); }
                        if restart { server = restart_server(server, &identity, info.clone(), server_tx.clone()).await?; }
                    }
                    finish_receive(&mut receiving, &directory)?;
                }
                Finished::SendStarted { id, host } => sending_target = Some((id, host)),
                Finished::Send(result) => { sending = None; sending_target = None; emit(json!({"event":"done","receiving":false,"error":result.err().map(|e| e.to_string())})); },
            },
            _ = idle.tick() => {
                if let Some(state) = receiving.as_mut().filter(|state| state.expired(Instant::now())) {
                    state.ended = true;
                    state.failed = true;
                    server = restart_server(server, &identity, info.clone(), server_tx.clone()).await?;
                    finish_receive(&mut receiving, &directory)?;
                }
            }
            Some(_) = discovery_rx.recv() => devices(&discovery),
        }
    }
    if let Some(cancel) = sending {
        cancel.cancel();
    }
    let _ = discovery_stop_tx.send(());
    let _ = server.1.send(());
    Ok(())
}

fn devices(discovery: &DiscoveryHandle) {
    let mut devices: Vec<_> = discovery.devices().into_iter()
        .filter(|device| matches!(device.device.channel, DeviceChannel::Http(ref http) if http.protocol == ProtocolType::Https))
        .map(|device| json!({"id":device.device.fingerprint,"name":device.device.alias})).collect();
    devices.sort_by_key(|device| device["name"].as_str().unwrap_or_default().to_lowercase());
    emit(json!({"event":"devices","devices":devices}));
}

fn refresh(discovery: Arc<DiscoveryHandle>) {
    tokio::spawn(async move {
        let interfaces = local_interface_addresses(&InterfaceFilter::default()).unwrap_or_default();
        let _ = discovery
            .discover_staged(
                Vec::new(),
                interfaces,
                DEFAULT_PORT,
                ProtocolType::Https,
                Duration::from_secs(1),
            )
            .await;
    });
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--version") {
        println!("orbit-localsend 1.18.2");
        return;
    }
    let result = (|| -> Result<()> {
        ensure!(args.len() == 2, "Expected download folder and device name");
        tokio::runtime::Runtime::new()?.block_on(run(
            PathBuf::from(&args[0]),
            args[1].to_string_lossy().into_owned(),
        ))
    })();
    if let Err(error) = result {
        emit(json!({"event":"error","error":format!("{error:#}")}));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_receives_expire_after_a_minute_without_progress() {
        let now = Instant::now();
        let mut state = Receiving {
            id: "session".into(),
            stage: tempfile::tempdir().unwrap(),
            pending: 0,
            ended: false,
            failed: false,
            roots: HashSet::new(),
            last_activity: now,
        };
        assert!(!state.expired(now + Duration::from_secs(59)));
        assert!(state.expired(now + Duration::from_secs(60)));
        state.last_activity = now + Duration::from_secs(50);
        assert!(!state.expired(now + Duration::from_secs(60)));
        state.ended = true;
        assert!(!state.expired(now + Duration::from_secs(120)));
    }

    #[test]
    fn aborted_receives_keep_completed_files_and_discard_incomplete_files() {
        let directory = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir_in(directory.path()).unwrap();
        let staging_path = stage.path().to_owned();
        std::fs::write(stage.path().join("complete.txt"), "complete").unwrap();
        std::fs::write(stage.path().join("partial.txt"), "partial").unwrap();
        let mut receiving = Some(Receiving {
            id: "session".into(),
            stage,
            pending: 1,
            ended: true,
            failed: true,
            roots: HashSet::from(["complete.txt".into()]),
            last_activity: Instant::now(),
        });
        finish_receive(&mut receiving, directory.path()).unwrap();
        assert!(receiving.is_some());
        assert!(staging_path.exists());
        receiving.as_mut().unwrap().pending = 0;
        finish_receive(&mut receiving, directory.path()).unwrap();
        assert!(receiving.is_none());
        assert_eq!(
            std::fs::read(directory.path().join("complete.txt")).unwrap(),
            b"complete"
        );
        assert!(!directory.path().join("partial.txt").exists());
        assert!(!staging_path.exists());
    }

    #[test]
    fn outgoing_names_are_unique_and_keep_their_extension() {
        let mut used = HashSet::new();
        assert_eq!(unique_name("photo.png", &mut used), "photo.png");
        assert_eq!(unique_name("photo.png", &mut used), "photo (1).png");
        assert_eq!(unique_name("photo (1).png", &mut used), "photo (1) (1).png");
        assert_eq!(unique_name("photo.png", &mut used), "photo (2).png");
        let long = format!("{}.png", "é".repeat(125));
        unique_name(&long, &mut used);
        let renamed = unique_name(&long, &mut used);
        assert!(renamed.len() <= 255 && renamed.ends_with(" (1).png"));
    }

    #[test]
    fn remote_paths_stay_inside_staging() {
        for name in ["../secret", "/etc/passwd", "a/../../b", "a\\b", "", "a\0b"] {
            assert!(relative_path(name).is_err());
        }
        assert_eq!(
            relative_path("folder/photo.png").unwrap(),
            PathBuf::from("folder/photo.png")
        );
    }
}
