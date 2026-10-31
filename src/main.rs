// SPDX-License-Identifier: GPL-3.0-only

mod always_on_top;
mod app;
mod applet;
mod cache;
mod config;
mod file_icon;
mod i18n;
mod import;
mod launch;
mod pin;
mod pointer;
mod shelf;
mod shortcuts;
mod thumbnail;
mod transfer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths: Vec<_> = std::env::args_os().skip(1).collect();
    let mut independent = false;
    match paths.first().and_then(|arg| arg.to_str()) {
        Some("--help" | "-h") => {
            println!(
                "Orbit — a file shelf for COSMIC\n\nUsage: orbit [FILE_OR_FOLDER ...]\n       orbit --new\n       orbit --applet\n       orbit --version"
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
            cosmic::applet::run::<applet::OrbitApplet>(())?;
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
    let size = app::SHELF_SIZE as f32;
    let settings = cosmic::app::Settings::default()
        .no_main_window(startup.is_some())
        .size(cosmic::iced::Size::new(size, size))
        .size_limits(
            cosmic::iced::Limits::NONE
                .min_width(size)
                .max_width(size)
                .min_height(size)
                .max_height(size),
        );
    cache::start_cleanup()?;
    always_on_top::start()?;
    let flags = app::Flags {
        uris,
        action: "collect".into(),
        startup,
    };
    if independent {
        cosmic::app::run::<app::Orbit>(settings, flags)?;
    } else {
        cosmic::app::run_single_instance::<app::Orbit>(settings, flags)?;
    }
    Ok(())
}
