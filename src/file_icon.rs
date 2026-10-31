// SPDX-License-Identifier: GPL-3.0-only

use cosmic::widget::icon::{self, IconFallback, Named};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::LazyLock,
};

static ALIASES: LazyLock<HashMap<String, String>> = LazyLock::new(|| mime_map("aliases", ' '));
static GENERIC_ICONS: LazyLock<HashMap<String, String>> =
    LazyLock::new(|| mime_map("generic-icons", ':'));

pub fn for_path(path: &Path) -> Named {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mime_name = ALIASES
        .get(mime.essence_str())
        .map(String::as_str)
        .unwrap_or(mime.essence_str());
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Some extensions are reported as plain text or binary by mime_guess.
    let name = match extension.as_str() {
        "py" | "pyw" => "text-x-python".into(),
        "java" => "text-x-java".into(),
        "cc" | "cpp" | "cxx" => "text-x-c++".into(),
        "h" | "hh" | "hpp" | "hxx" => "text-x-cheader".into(),
        "cs" => "text-x-c#".into(),
        "sh" | "bash" | "zsh" | "fish" => "text-x-script".into(),
        "deb" => "package-x-deb".into(),
        "rpm" => "package-x-rpm".into(),
        "exe" | "appimage" => "application-x-executable".into(),
        _ if !extension.is_empty()
            && matches!(mime_name, "text/plain" | "application/octet-stream") =>
        {
            format!("text-x-{extension}")
        }
        _ => mime_name.replace('/', "-"),
    };
    let mut fallbacks = Vec::new();
    let mime_icon = mime_name.replace('/', "-");
    if mime_icon != name {
        fallbacks.push(mime_icon.into());
    }
    if let Some((category, subtype)) = mime_name.split_once('/') {
        if category == "text" || matches!(subtype, "json" | "xml") {
            fallbacks.push(format!("text-x-{subtype}").into());
        }
        if let Some(generic) = GENERIC_ICONS.get(mime_name) {
            fallbacks.push(generic.clone().into());
        }
        fallbacks.push(format!("{category}-x-generic").into());
    }
    fallbacks.push("text-x-generic".into());
    fallbacks.dedup();
    icon::from_name(name).fallback(Some(IconFallback::Names(fallbacks)))
}

fn mime_map(file: &str, separator: char) -> HashMap<String, String> {
    let mut directories = Vec::new();
    if let Some(path) = std::env::var_os("XDG_DATA_HOME").filter(|path| !path.is_empty()) {
        directories.push(PathBuf::from(path));
    } else if let Some(home) = std::env::var_os("HOME") {
        directories.push(PathBuf::from(home).join(".local/share"));
    }
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    directories.extend(std::env::split_paths(&data_dirs));
    let mut names = HashMap::new();
    for directory in directories.into_iter().filter(|path| path.is_absolute()) {
        if let Ok(contents) = std::fs::read_to_string(directory.join("mime").join(file)) {
            for (mime, name) in contents
                .lines()
                .filter_map(|line| line.split_once(separator))
            {
                names
                    .entry(mime.to_owned())
                    .or_insert_with(|| name.to_owned());
            }
        }
    }
    names
}
