// SPDX-License-Identifier: GPL-3.0-only

pub async fn new_orbit(token: Option<String>, startup: Option<String>) -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    // Atomic installs replace the executable while older shelves may still be open.
    let mut command = std::process::Command::new(executable.with_file_name("orbit"));
    command
        .arg("--new")
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
