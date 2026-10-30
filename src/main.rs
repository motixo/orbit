// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod i18n;
mod pin;
mod shelf;
mod transfer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths: Vec<_> = std::env::args_os().skip(1).collect();
    match paths.first().and_then(|arg| arg.to_str()) {
        Some("--help" | "-h") => {
            println!(
                "Orbit — a floating file shelf for COSMIC\n\nUsage: orbit [FILE_OR_FOLDER ...]\n       orbit --version\n\nCtrl+O: add files    Ctrl+Shift+O: add folders\nCtrl+V: collect      Ctrl+C: copy items\nCtrl+A: select all   Ctrl+P: pin shelf\nCtrl+Q: quit"
            );
            return Ok(());
        }
        Some("--version" | "-V") => {
            println!("Orbit {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
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
    let size = app::SHELF_SIZE as f32;
    let settings = cosmic::app::Settings::default()
        .size(cosmic::iced::Size::new(size, size))
        .size_limits(
            cosmic::iced::Limits::NONE
                .min_width(size)
                .max_width(size)
                .min_height(size)
                .max_height(size),
        );
    cosmic::app::run_single_instance::<app::Orbit>(
        settings,
        app::Flags {
            uris,
            action: "collect".into(),
        },
    )?;
    Ok(())
}
