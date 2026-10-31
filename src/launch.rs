// SPDX-License-Identifier: GPL-3.0-only

use std::{
    ffi::OsString,
    io::{self, Read},
    path::Path,
};

pub async fn new_orbit(
    token: Option<String>,
    startup: Option<String>,
    arguments: Vec<OsString>,
) -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    // Atomic installs replace the executable while older shelves may still be open.
    let mut command = std::process::Command::new(executable.with_file_name("orbit"));
    command
        .arg("--new")
        .args(arguments)
        .env("COSMIC_SINGLE_INSTANCE", "0")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("ORBIT_START_PINNED")
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env_remove("DESKTOP_STARTUP_ID");
    if let Some(token) = token {
        command.env("XDG_ACTIVATION_TOKEN", token);
    }
    if let Some(startup) = startup {
        command.env("ORBIT_START_PINNED", startup);
    }
    cosmic::process::spawn(command).await.is_some()
}

pub async fn from_drop(token: Option<String>, bytes: Vec<u8>, mime: String) -> bool {
    let Ok(directory) = crate::cache::Directory::new("orbit-drop-") else {
        return false;
    };
    let path = directory.path().join("payload");
    if std::fs::write(&path, bytes).is_err() {
        return false;
    }
    let launched = new_orbit(
        token,
        None,
        vec!["--drop".into(), mime.into(), path.into_os_string()],
    )
    .await;
    if launched {
        directory.hand_off();
    }
    launched
}

pub fn read_drop(path: &Path) -> io::Result<Vec<u8>> {
    let owned = path.file_name().is_some_and(|name| name == "payload")
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name.to_string_lossy().starts_with("orbit-drop-"));
    let _lease = owned
        .then(|| crate::cache::Directory::for_file(path))
        .flatten()
        .ok_or_else(|| io::Error::other("Invalid Orbit drop payload"))?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(crate::import::MAX_IMAGE_SIZE as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > crate::import::MAX_IMAGE_SIZE {
        return Err(io::Error::other("Drop exceeds 50 MiB"));
    }
    std::fs::remove_file(path)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn drop_payloads_cannot_read_or_remove_unowned_files() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("payload");
        std::fs::write(&path, "original").unwrap();
        assert!(super::read_drop(&path).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "original");
    }
}
