// SPDX-License-Identifier: GPL-3.0-only

use crate::{import::ImportedFiles, share::Bundle};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use cosmic::iced::futures::{StreamExt, channel::mpsc::UnboundedSender};
use serde::Deserialize;
use std::{net::Ipv4Addr, sync::Arc, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    sync::{Semaphore, watch},
};

const MAX_UPLOAD: usize = 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Address {
    ip: Ipv4Addr,
    label: String,
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
    stop: watch::Sender<bool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

#[derive(Clone)]
struct ShareState {
    bundle: Arc<Bundle>,
    origin: String,
    incoming: UnboundedSender<ImportedFiles>,
    stop: watch::Receiver<bool>,
    uploads: Arc<Semaphore>,
}

pub async fn start(
    address: &Address,
    bundle: Arc<Bundle>,
    incoming: UnboundedSender<ImportedFiles>,
) -> Result<Arc<Session>, String> {
    let listener = tokio::net::TcpListener::bind((address.ip, 0))
        .await
        .map_err(|e| e.to_string())?;
    let origin = format!(
        "http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let token = uuid::Uuid::new_v4().simple().to_string();
    let url = format!("{origin}/{token}/");
    let (stop, stopped) = watch::channel(false);
    let state = ShareState {
        bundle,
        origin,
        incoming,
        stop: stopped.clone(),
        uploads: Arc::new(Semaphore::new(2)),
    };
    let routes = Router::new()
        .route("/", get(index))
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../resources/share/style.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../resources/share/app.js"),
                )
            }),
        )
        .route("/files/{index}", get(download))
        .route(
            "/upload",
            post(upload).layer(DefaultBodyLimit::max(MAX_UPLOAD + 1024 * 1024)),
        )
        .route(
            "/text",
            post(receive_text).layer(DefaultBodyLimit::max(1024 * 1024 + 1024)),
        )
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state);
    let base = format!("/{token}/");
    let redirect = base.clone();
    let app = Router::new()
        .route(
            &format!("/{token}"),
            get(move || async move { Redirect::temporary(&redirect) }),
        )
        .nest(&base, routes);
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(stopping(stopped))
            .await
        {
            eprintln!("Phone sharing stopped: {error}");
        }
    });
    Ok(Arc::new(Session { url, stop }))
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
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
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

async fn index(State(state): State<ShareState>) -> Html<String> {
    let mut items = String::new();
    for (index, file) in state.bundle.files.iter().enumerate() {
        use std::fmt::Write;
        let name = quick_xml::escape::escape(&file.name);
        let _ = write!(
            items,
            "<article><div class=filename>{name}</div><a class=button href=\"files/{index}\" download>Download</a>"
        );
        if let Some(text) = &file.text {
            let escaped = quick_xml::escape::escape(text);
            let _ = write!(
                items,
                "<textarea readonly id=\"content-{index}\" aria-label=\"Content\">{escaped}</textarea><button type=button data-copy=\"content-{index}\">Copy</button>"
            );
            if crate::transfer::is_link(text) {
                let _ = write!(
                    items,
                    "<a class=button href=\"{}\" target=_blank rel=noreferrer>Open</a>",
                    quick_xml::escape::escape(text.trim())
                );
            }
        }
        items.push_str("</article>");
    }
    Html(include_str!("../resources/share/index.html").replace("<!-- items -->", &items))
}

async fn download(
    State(state): State<ShareState>,
    Path(index): Path<usize>,
) -> Result<Response, StatusCode> {
    let file = state.bundle.files.get(index).ok_or(StatusCode::NOT_FOUND)?;
    let input = tokio::fs::File::open(&file.path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let metadata = input.metadata().await.map_err(|_| StatusCode::NOT_FOUND)?;
    if !metadata.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }
    let stream = tokio_util::io::ReaderStream::new(input).take_until(stopping(state.stop.clone()));
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(metadata.len()));
    let disposition = format!(
        "attachment; filename=\"file\"; filename*=UTF-8''{}",
        percent_encoding::utf8_percent_encode(&file.name, percent_encoding::NON_ALPHANUMERIC)
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    Ok(response)
}

fn valid_filename(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | ".." | ".lease" | ".released")
        && !name.contains(['/', '\\', '\0', '\r', '\n'])
}

async fn upload(
    State(state): State<ShareState>,
    mut multipart: Multipart,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    let _slot = state
        .uploads
        .try_acquire()
        .map_err(|_| (StatusCode::TOO_MANY_REQUESTS, "Try again"))?;
    let mut count = 0;
    while let Some(mut field) =
        tokio::time::timeout(Duration::from_secs(60), multipart.next_field())
            .await
            .map_err(|_| (StatusCode::REQUEST_TIMEOUT, "Upload timed out"))?
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upload"))?
    {
        if field.name() != Some("file") || count >= 32 {
            return Err((StatusCode::BAD_REQUEST, "Invalid upload"));
        }
        let name = field
            .file_name()
            .filter(|name| valid_filename(name))
            .ok_or((StatusCode::BAD_REQUEST, "Invalid file name"))?
            .to_owned();
        let directory = crate::cache::Directory::new("orbit-drop-")
            .map_err(|_| (StatusCode::INSUFFICIENT_STORAGE, "Could not save file"))?;
        let path = directory.path().join(name);
        let mut output = tokio::fs::File::create_new(&path)
            .await
            .map_err(|_| (StatusCode::INSUFFICIENT_STORAGE, "Could not save file"))?;
        let mut received = 0usize;
        loop {
            let chunk = tokio::select! {
                _ = stopping(state.stop.clone()) => return Err((StatusCode::GONE, "Sharing stopped")),
                chunk = tokio::time::timeout(Duration::from_secs(60), field.chunk()) => chunk.map_err(|_| (StatusCode::REQUEST_TIMEOUT, "Upload timed out"))?.map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "Invalid or oversized upload"))?,
            };
            let Some(chunk) = chunk else {
                break;
            };
            received += chunk.len();
            if received > MAX_UPLOAD {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "File exceeds 1 GiB"));
            }
            output
                .write_all(&chunk)
                .await
                .map_err(|_| (StatusCode::INSUFFICIENT_STORAGE, "Could not save file"))?;
        }
        output
            .flush()
            .await
            .map_err(|_| (StatusCode::INSUFFICIENT_STORAGE, "Could not save file"))?;
        drop(output);
        state
            .incoming
            .unbounded_send(ImportedFiles {
                files: crate::transfer::FileList(vec![path]),
                directory: Some(directory),
                text: None,
            })
            .map_err(|_| (StatusCode::GONE, "Sharing stopped"))?;
        count += 1;
    }
    if count == 0 {
        return Err((StatusCode::BAD_REQUEST, "No file"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct TextInput {
    text: String,
}

async fn receive_text(
    State(state): State<ShareState>,
    Json(input): Json<TextInput>,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    if input.text.trim().is_empty() || input.text.len() > 1024 * 1024 {
        return Err((StatusCode::BAD_REQUEST, "Invalid text"));
    }
    let imported = crate::import::receive(
        input.text.into_bytes(),
        crate::transfer::TEXT.into(),
        crate::import::Source::Clipboard,
        |_| {},
    )
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Could not save text"))?;
    state
        .incoming
        .unbounded_send(imported)
        .map_err(|_| (StatusCode::GONE, "Sharing stopped"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn network_picker_ignores_loopback_bridges_and_public_addresses() {
        let addresses = parse_addresses(br#"[{"ifname":"lo","addr_info":[{"local":"127.0.0.1"}]},{"ifname":"docker0","addr_info":[{"local":"172.17.0.1"}]},{"ifname":"wlan0","addr_info":[{"local":"192.168.1.2"},{"local":"8.8.8.8"}]}]"#).unwrap();
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].ip, Ipv4Addr::new(192, 168, 1, 2));
    }

    #[tokio::test]
    async fn qr_sessions_only_share_selected_files_and_accept_complete_uploads() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("note.txt");
        std::fs::write(&path, "<private>").unwrap();
        let mut shelf = crate::shelf::Shelf::default();
        shelf.add([path.clone()]);
        let bundle = Bundle::prepare(
            shelf.items.clone(),
            Arc::new(shelf.transfer(shelf.export_paths(), false)),
        )
        .unwrap();
        let (output, mut input) = cosmic::iced::futures::channel::mpsc::unbounded();
        let address = Address {
            ip: Ipv4Addr::LOCALHOST,
            label: "test".into(),
        };
        let session = start(&address, bundle, output).await.unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let page = client.get(&session.url).send().await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let html = page.text().await.unwrap();
        assert!(html.contains("&lt;private&gt;"));
        assert!(html.contains("Send to Orbit"));
        let redirected = client
            .get(session.url.trim_end_matches('/'))
            .send()
            .await
            .unwrap();
        assert_eq!(redirected.status(), StatusCode::OK);
        assert_eq!(redirected.url().as_str(), session.url);
        for (asset, content_type) in [
            ("style.css", "text/css; charset=utf-8"),
            ("app.js", "text/javascript; charset=utf-8"),
        ] {
            let response = client
                .get(format!("{}{asset}", session.url))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
            assert!(!response.text().await.unwrap().is_empty());
        }
        assert_eq!(
            client
                .get(format!("{}files/0", session.url))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "<private>"
        );
        assert_eq!(
            client
                .get(format!("{}files/1", session.url))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .get(&session.url)
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
        for (name, expected) in [
            ("../escape", StatusCode::BAD_REQUEST),
            ("phone.txt", StatusCode::NO_CONTENT),
        ] {
            let body = format!(
                "--orbit-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: text/plain\r\n\r\nfrom phone\r\n--orbit-boundary--\r\n"
            );
            assert_eq!(
                client
                    .post(format!("{}upload", session.url))
                    .header(
                        header::CONTENT_TYPE,
                        "multipart/form-data; boundary=orbit-boundary"
                    )
                    .body(body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                expected
            );
        }
        let received = tokio::time::timeout(Duration::from_secs(3), input.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&received.files.0[0]).unwrap(),
            "from phone"
        );
        assert_eq!(
            client
                .post(format!("{}text", session.url))
                .header(header::CONTENT_TYPE, "application/json")
                .body(r#"{"text":"https://example.com"}"#)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        let link = tokio::time::timeout(Duration::from_secs(3), input.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(link.text.as_deref(), Some("https://example.com"));
        let url = session.url.clone();
        drop(session);
        if let Ok(response) = client.get(url).send().await {
            assert_ne!(response.status(), StatusCode::OK);
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), "<private>");
    }
}
