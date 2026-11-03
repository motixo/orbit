// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cctk::sctk::shell::wlr_layer::Anchor;
use cosmic::cosmic_config::{
    self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry,
};
use serde::{Deserialize, Serialize};

pub const ID: &str = "io.github.motixo.Orbit";
pub const REPOSITORY: &str = "https://github.com/motixo/orbit";
pub const UPDATES_URL: &str = "https://github.com/motixo/orbit/tags";

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
            shortcuts: crate::shortcuts::Bindings::default(),
        }
    }
}

impl Preferences {
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
