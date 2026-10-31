// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    config::{ID, UpdateReminder},
    fl,
};
use cosmic::cosmic_config::{self, Config, ConfigGet, ConfigSet};
use semver::Version;
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub enum Outcome {
    Skipped,
    Current,
    Available(String),
}

fn timestamp(state: &Config, key: &str) -> Result<u64, String> {
    match state.get(key) {
        Ok(value) => Ok(value),
        Err(cosmic_config::Error::NotFound) => Ok(0),
        Err(cosmic_config::Error::GetKey(_, error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(0)
        }
        Err(_) => Err(fl!("update-check-error")),
    }
}

pub async fn check(reminder: UpdateReminder, manual: bool) -> Result<Outcome, String> {
    if !manual && reminder == UpdateReminder::Off {
        return Ok(Outcome::Skipped);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| fl!("update-check-error"))?;
    let directory = std::path::PathBuf::from(runtime).join(ID);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .map_err(|_| fl!("update-check-error"))?;
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join("updates.lock"))
        .map_err(|_| fl!("update-check-error"))?;
    match lease.try_lock() {
        Ok(()) => (),
        Err(fs::TryLockError::WouldBlock) => return Ok(Outcome::Skipped),
        Err(_) => return Err(fl!("update-check-error")),
    }
    let state = Config::new_state(ID, 1).map_err(|_| fl!("update-check-error"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| fl!("update-check-error"))?
        .as_secs();
    let last_success = timestamp(&state, "update_success")?;
    let last_attempt = timestamp(&state, "update_attempt")?;
    let since = |timestamp: u64| {
        if timestamp > now {
            u64::MAX
        } else {
            now - timestamp
        }
    };
    if !manual && (since(last_success) < reminder.seconds() || since(last_attempt) < 3600) {
        return Ok(Outcome::Skipped);
    }
    state
        .set("update_attempt", now)
        .map_err(|_| fl!("update-check-error"))?;
    let latest = latest().await.map_err(|_| fl!("update-check-error"))?;
    if let Some(version) = latest.as_ref()
        && !manual
    {
        notify(version)
            .await
            .map_err(|_| fl!("update-notify-error"))?;
    }
    state
        .set("update_success", now)
        .map_err(|_| fl!("update-check-error"))?;
    Ok(latest.map(Outcome::Available).unwrap_or(Outcome::Current))
}

#[derive(Deserialize)]
struct Tag {
    name: String,
}

async fn latest() -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("Orbit/", env!("CARGO_PKG_VERSION")))
        .build()?;
    // shortcut: inspect 100 tags; add pagination if the repository exceeds this window.
    let mut response = client
        .get("https://api.github.com/repos/motixo/orbit/tags?per_page=100")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .send()
        .await?
        .error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err("Tag response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let tags: Vec<Tag> = serde_json::from_slice(&bytes)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    Ok(tags
        .into_iter()
        .filter_map(|tag| Version::parse(tag.name.trim_start_matches('v')).ok())
        .filter(|version| version.pre.is_empty() && version.cmp_precedence(&current).is_gt())
        .max_by(Version::cmp_precedence)
        .map(|version| version.to_string()))
}

async fn notify(version: &str) -> Result<(), zbus::Error> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await?;
    let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
    let _: u32 = proxy
        .call(
            "Notify",
            &(
                "Orbit",
                0_u32,
                ID,
                "Orbit",
                fl!("update-available", version = version),
                Vec::<&str>::new(),
                hints,
                -1_i32,
            ),
        )
        .await?;
    Ok(())
}
