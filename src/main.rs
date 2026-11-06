// SPDX-License-Identifier: GPL-3.0-only

mod always_on_top;
mod app;
mod applet;
mod archive;
mod cache;
mod config;
mod convert;
mod device_share;
mod drag_preview;
mod file_icon;
mod history;
mod i18n;
mod import;
mod launch;
mod layer_input;
mod localsend;
mod orbit_icon;
mod owly;
mod pin;
mod pointer;
mod presence;
mod preview;
mod rich_text;
mod share;
mod sharing_setup;
mod shelf;
mod shortcuts;
mod thumbnail;
mod transfer;
mod tray;
mod updates;
mod watch;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths: Vec<_> = std::env::args_os().skip(1).collect();
    let mut independent = false;
    if !matches!(
        paths.first().and_then(|arg| arg.to_str()),
        Some("--help" | "-h" | "--version" | "-V" | "--sharing-setup" | "--convert-image")
    ) {
        sharing_setup::launch(false);
        device_share::listen();
    }
    match paths.first().and_then(|arg| arg.to_str()) {
        Some("--sharing-setup") => return sharing_setup::run(),
        Some("--convert-image") => {
            convert::run_worker(&paths[1..])?;
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!(
                "Orbit — a file shelf for COSMIC\n\nUsage: orbit [FILE_OR_FOLDER ...]\n       orbit --new\n       orbit --share\n       orbit --applet\n       orbit --tray\n       orbit --menu\n       orbit --version"
            );
            return Ok(());
        }
        Some("--version" | "-V") => {
            println!("Orbit {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--applet") => {
            let languages = i18n_embed::DesktopLanguageRequester::requested_languages();
            i18n::init(&languages);
            cosmic::applet::run::<applet::OrbitApplet>(applet::Flags::Applet)?;
            return Ok(());
        }
        Some("--tray" | "--menu") => {
            let show_menu = paths[0] == "--menu";
            let Some(session) = tray::Session::open(show_menu)? else {
                return Ok(());
            };
            let languages = i18n_embed::DesktopLanguageRequester::requested_languages();
            i18n::init(&languages);
            cosmic::app::run::<applet::OrbitApplet>(
                cosmic::app::Settings::default()
                    .no_main_window(true)
                    .exit_on_close(false),
                applet::Flags::Desktop { session, show_menu },
            )?;
            return Ok(());
        }
        Some("--new") => {
            independent = true;
            paths.remove(0);
        }
        Some("--") => {
            paths.remove(0);
        }
        _ => (),
    }
    let watch = if paths.first().is_some_and(|arg| arg == "--watch") {
        if paths.len() != 2 {
            return Err("Usage: orbit --new --watch FOLDER".into());
        }
        let Some(folder) = watch::Folder::claim(std::path::Path::new(&paths[1]))? else {
            return Ok(());
        };
        paths.clear();
        independent = true;
        Some(folder)
    } else {
        None
    };
    let remote_open = if paths.first().is_some_and(|arg| arg == "--remote-share") {
        if paths.len() != 2 {
            return Err("Invalid remote Orbit request".into());
        }
        let id = uuid::Uuid::parse_str(paths[1].to_str().ok_or("Invalid remote Orbit request")?)?;
        paths.clear();
        Some(id)
    } else {
        None
    };
    let share = remote_open.is_some() || paths.first().is_some_and(|arg| arg == "--share");
    if share {
        if remote_open.is_none() && paths.len() != 1 {
            return Err("Usage: orbit --share".into());
        }
        paths.clear();
        independent = true;
    }
    let share_activation = if share {
        let token = std::env::var("XDG_ACTIVATION_TOKEN").ok();
        let Some(session) = tray::Session::open_share(token.as_deref())? else {
            return Ok(());
        };
        Some(session)
    } else {
        None
    };
    let restored = if paths.first().is_some_and(|arg| arg == "--restore") {
        if paths.len() != 2 {
            return Err("Usage: orbit --new --restore ID".into());
        }
        let id = paths[1].to_str().ok_or("Invalid history ID")?;
        let entry = history::list()?
            .into_iter()
            .find(|entry| entry.id == id)
            .ok_or("Orbit no longer available")?;
        paths.clear();
        Some(entry)
    } else {
        None
    };
    let incoming_drop = if paths.first().is_some_and(|arg| arg == "--drop") {
        if paths.len() != 3 {
            return Err("Usage: orbit --new --drop MIME PAYLOAD".into());
        }
        let mime = paths[1]
            .to_str()
            .ok_or("Invalid drop MIME type")?
            .to_owned();
        let bytes = launch::read_drop(std::path::Path::new(&paths[2]))?;
        paths.clear();
        Some((bytes, mime))
    } else {
        None
    };
    let mut uris = Vec::new();
    for path in paths {
        let path = std::path::absolute(path)?;
        uris.push(
            url::Url::from_file_path(path)
                .map_err(|_| "Invalid local path")?
                .to_string(),
        );
    }
    let languages = i18n_embed::DesktopLanguageRequester::requested_languages();
    i18n::init(&languages);
    let startup = pin::Startup::from_env()?;
    let mut preferences = config::Preferences::load();
    if let Some(entry) = &restored {
        preferences.pinned = entry.pinned;
        preferences.corner = entry.corner;
    }
    let pinned = startup.is_some() || preferences.pinned;
    let size = app::SHELF_SIZE as f32;
    let settings = cosmic::app::Settings::default()
        .no_main_window(pinned || watch.is_some())
        .exit_on_close(watch.is_none())
        .size(cosmic::iced::Size::new(size, size))
        .size_limits(
            cosmic::iced::Limits::NONE
                .min_width(size)
                .max_width(size)
                .min_height(size)
                .max_height(size),
        );
    cache::start_cleanup()?;
    let startup_area = always_on_top::start()?;
    let flags = app::Flags {
        share_activation,
        remote_open,
        uris,
        incoming_drop,
        restored,
        startup_area: ((preferences.pinned || watch.is_some()) && startup.is_none())
            .then_some(startup_area),
        watch,
        action: "collect".into(),
        preferences,
        startup,
    };
    if independent {
        cosmic::app::run::<app::Orbit>(settings, flags)?;
    } else {
        cosmic::app::run_single_instance::<app::Orbit>(settings, flags)?;
    }
    Ok(())
}
