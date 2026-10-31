// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cctk::sctk::shell::wlr_layer::Anchor;
use cosmic::cosmic_config::{
    self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry,
};
use serde::{Deserialize, Serialize};

pub const ID: &str = "io.github.motixo.Orbit";
pub const REPOSITORY: &str = "https://github.com/motixo/orbit";

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

#[derive(Clone, Debug, Default, PartialEq, CosmicConfigEntry)]
#[version = 1]
pub struct Preferences {
    pub pinned: bool,
    pub corner: Corner,
    pub shortcuts: crate::shortcuts::Bindings,
}

impl Preferences {
    pub fn load() -> Self {
        Config::new(ID, Self::VERSION)
            .map(|config| Self::get_entry(&config).unwrap_or_else(|(_, config)| config))
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), cosmic_config::Error> {
        self.write_entry(&Config::new(ID, Self::VERSION)?)
    }
}
