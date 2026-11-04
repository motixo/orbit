// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cctk::sctk::shell::wlr_layer::Anchor;
use cosmic::cosmic_config::{
    self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry,
};
use serde::{Deserialize, Serialize};

pub const ID: &str = "io.github.motixo.Orbit";
pub const REPOSITORY: &str = "https://github.com/motixo/orbit";
pub const UPDATES_URL: &str = "https://github.com/motixo/orbit/tags";
pub const SHARE_PWA_URL: &str = "https://orbit.motixo-dev.workers.dev/";
pub const SHARE_DEPLOY_URL: &str = "https://deploy.workers.cloudflare.com/?url=https%3A%2F%2Fgithub.com%2Fmotixo%2Forbit-share-pwa";

pub fn pwa_url(value: &str) -> Option<url::Url> {
    let value = value.trim();
    let url = url::Url::parse(if value.is_empty() {
        SHARE_PWA_URL
    } else {
        value
    })
    .ok()?;
    (url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum Corner {
    TopLeft,
    #[default]
    TopRight,
    BottomRight,
    BottomLeft,
}

impl Corner {
    pub const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomRight,
        Self::BottomLeft,
    ];

    pub fn anchor(self) -> Anchor {
        match self {
            Self::TopLeft => Anchor::TOP | Anchor::LEFT,
            Self::TopRight => Anchor::TOP | Anchor::RIGHT,
            Self::BottomRight => Anchor::BOTTOM | Anchor::RIGHT,
            Self::BottomLeft => Anchor::BOTTOM | Anchor::LEFT,
        }
    }

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|corner| *corner == self)
            .unwrap_or(1)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum UpdateReminder {
    Off,
    Daily,
    #[default]
    Weekly,
}

impl UpdateReminder {
    pub const ALL: [Self; 3] = [Self::Off, Self::Daily, Self::Weekly];

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|reminder| *reminder == self)
            .unwrap_or(2)
    }

    pub fn seconds(self) -> u64 {
        match self {
            Self::Off => 0,
            Self::Daily => 86_400,
            Self::Weekly => 604_800,
        }
    }
}

#[derive(Clone, Debug, PartialEq, CosmicConfigEntry)]
#[version = 1]
pub struct Preferences {
    pub tray_enabled: bool,
    pub pinned: bool,
    pub corner: Corner,
    pub pinned_hint: bool,
    pub update_reminder: UpdateReminder,
    pub watch_folders: Vec<std::path::PathBuf>,
    pub share_pwa_url: String,
    pub share_download_directory: Option<std::path::PathBuf>,
    pub shortcuts: crate::shortcuts::Bindings,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            tray_enabled: true,
            pinned: false,
            corner: Corner::default(),
            pinned_hint: true,
            update_reminder: UpdateReminder::default(),
            watch_folders: Vec::new(),
            share_pwa_url: String::new(),
            share_download_directory: None,
            shortcuts: crate::shortcuts::Bindings::default(),
        }
    }
}

impl Preferences {
    pub fn download_directory(&self) -> std::io::Result<std::path::PathBuf> {
        let directory = self
            .share_download_directory
            .clone()
            .or_else(|| {
                dirs::download_dir()
                    .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
                    .map(|directory| directory.join("Orbit-Share"))
            })
            .filter(|directory| directory.is_absolute());
        directory.ok_or_else(|| std::io::Error::other("Missing download directory"))
    }

    pub fn load() -> Self {
        let mut preferences = Config::new(ID, Self::VERSION)
            .map(|config| Self::get_entry(&config).unwrap_or_else(|(_, config)| config))
            .unwrap_or_default();
        preferences.shortcuts.complete();
        preferences
    }

    pub fn save(&self) -> Result<(), cosmic_config::Error> {
        self.write_entry(&Config::new(ID, Self::VERSION)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pwa_addresses_require_https_without_credentials_or_session_data() {
        assert_eq!(pwa_url("").unwrap().as_str(), SHARE_PWA_URL);
        assert_eq!(
            pwa_url("  https://share.example.com/mobile/  ")
                .unwrap()
                .as_str(),
            "https://share.example.com/mobile/"
        );
        for value in [
            "http://share.example.com/",
            "javascript:alert(1)",
            "not a URL",
            "https://user:password@share.example.com/",
            "https://share.example.com/?token=secret",
            "https://share.example.com/#session",
        ] {
            assert!(pwa_url(value).is_none(), "{value}");
        }
    }
}
