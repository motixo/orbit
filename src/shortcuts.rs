// SPDX-License-Identifier: GPL-3.0-only

use crate::fl;
use cosmic::{
    cosmic_config::{self, Config, ConfigGet, ConfigSet},
    iced::keyboard,
};
use ron::value::RawValue;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub enum Action {
    New,
    Pin,
    Corner,
    Collapse,
    Files,
    Folders,
    Copy,
    Paste,
    SelectAll,
    Clear,
    Close,
    Undo,
    Preview,
}

impl Action {
    pub const ALL: [Self; 13] = [
        Self::New,
        Self::Pin,
        Self::Corner,
        Self::Collapse,
        Self::Files,
        Self::Folders,
        Self::Copy,
        Self::Paste,
        Self::SelectAll,
        Self::Clear,
        Self::Close,
        Self::Undo,
        Self::Preview,
    ];

    pub fn label(self) -> String {
        match self {
            Self::New => fl!("new-orbit"),
            Self::Pin => fl!("pin-toggle"),
            Self::Corner => fl!("next-corner"),
            Self::Collapse => fl!("collapse-toggle"),
            Self::Files => fl!("add-files"),
            Self::Folders => fl!("add-folders"),
            Self::Copy => fl!("copy"),
            Self::Paste => fl!("paste"),
            Self::SelectAll => fl!("select-all"),
            Self::Clear => fl!("clear-shelf"),
            Self::Close => fl!("close"),
            Self::Undo => fl!("undo"),
            Self::Preview => fl!("preview"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Shortcut {
    pub modifiers: u8,
    pub key: String,
}

pub fn physical_key(key: keyboard::key::Physical, layer_surface: bool) -> keyboard::key::Physical {
    // shortcut: correct libcosmic 5a8bd94's layer offset until its SCTK key conversion is fixed.
    if layer_surface {
        iced_winit::conversion::physical_to_scancode(key)
            .and_then(|code| code.checked_add(8))
            .map(iced_winit::platform_specific::wayland::keymap::scancode_to_physicalkey)
            .map(iced_winit::conversion::physical_key)
            .unwrap_or(key)
    } else {
        key
    }
}

impl Shortcut {
    pub fn from_event(event: &keyboard::Event, layer_surface: bool) -> Option<Self> {
        let keyboard::Event::KeyPressed {
            key,
            physical_key,
            modifiers,
            repeat: false,
            ..
        } = event
        else {
            return None;
        };
        let physical_key = self::physical_key(*physical_key, layer_surface);
        let key = if let Some(key) = key.to_latin(physical_key) {
            key.to_ascii_lowercase().to_string()
        } else if let keyboard::Key::Named(named) = key {
            match named {
                keyboard::key::Named::Control
                | keyboard::key::Named::Shift
                | keyboard::key::Named::Alt
                | keyboard::key::Named::AltGraph
                | keyboard::key::Named::Super
                | keyboard::key::Named::Meta => return None,
                _ => format!("{named:?}"),
            }
        } else {
            return None;
        };
        Some(Self {
            modifiers: u8::from(modifiers.control())
                | (u8::from(modifiers.shift()) << 1)
                | (u8::from(modifiers.alt()) << 2)
                | (u8::from(modifiers.logo()) << 3),
            key,
        })
    }

    pub fn label(&self) -> String {
        let mut label = String::new();
        for (bit, name) in [(8, "Super+"), (1, "Ctrl+"), (4, "Alt+"), (2, "Shift+")] {
            if self.modifiers & bit != 0 {
                label.push_str(name);
            }
        }
        label.push_str(&if self.key.len() == 1 {
            self.key.to_uppercase()
        } else {
            self.key.clone()
        });
        label
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Bindings(pub BTreeMap<Action, Option<Shortcut>>);

impl Default for Bindings {
    fn default() -> Self {
        Self(
            Action::ALL
                .into_iter()
                .zip([
                    (1, "n"),
                    (1, "p"),
                    (3, "p"),
                    (1, "m"),
                    (1, "o"),
                    (3, "o"),
                    (1, "c"),
                    (1, "v"),
                    (1, "a"),
                    (0, "Delete"),
                    (1, "q"),
                    (1, "z"),
                    (0, "Space"),
                ])
                .map(|(action, (modifiers, key))| {
                    (
                        action,
                        Some(Shortcut {
                            modifiers,
                            key: key.into(),
                        }),
                    )
                })
                .collect(),
        )
    }
}

impl Bindings {
    pub fn complete(&mut self) {
        for (action, binding) in Self::default().0 {
            if !self.0.contains_key(&action) {
                let binding = binding.filter(|shortcut| self.action(shortcut).is_none());
                self.0.insert(action, binding);
            }
        }
    }

    pub fn action(&self, shortcut: &Shortcut) -> Option<Action> {
        self.0
            .iter()
            .find(|(_, binding)| binding.as_ref() == Some(shortcut))
            .map(|(action, _)| *action)
    }
}

const GLOBAL_ID: &str = "com.system76.CosmicSettings.Shortcuts";
const COMMAND: &str = "orbit --new";
// Preserve compositor actions and binding fields verbatim when changing Orbit's shortcut.
type GlobalMap = BTreeMap<Box<RawValue>, Box<RawValue>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Super,
}

#[derive(Deserialize, Serialize)]
struct GlobalBinding {
    modifiers: Vec<Modifier>,
    #[serde(default)]
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keycode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Deserialize, Serialize)]
enum GlobalAction {
    Spawn(String),
}

fn owned(action: &RawValue) -> bool {
    matches!(action.into_rust::<GlobalAction>(), Ok(GlobalAction::Spawn(command)) if command == COMMAND)
}

fn read(config: &Config, key: &str) -> Result<GlobalMap, String> {
    match config.get(key) {
        Ok(map) => Ok(map),
        Err(cosmic_config::Error::NotFound) => Ok(GlobalMap::new()),
        Err(cosmic_config::Error::GetKey(_, error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(GlobalMap::new())
        }
        Err(_) => Err(fl!("shortcut-save-error")),
    }
}

impl GlobalBinding {
    fn shortcut(&self) -> Shortcut {
        let modifiers = self.modifiers.iter().fold(0, |bits, modifier| {
            bits | match modifier {
                Modifier::Ctrl => 1,
                Modifier::Shift => 2,
                Modifier::Alt => 4,
                Modifier::Super => 8,
            }
        });
        let key = match self.key.as_str() {
            "Return" => "Enter",
            "space" => "Space",
            "Left" => "ArrowLeft",
            "Right" => "ArrowRight",
            "Up" => "ArrowUp",
            "Down" => "ArrowDown",
            "Prior" => "PageUp",
            "Next" => "PageDown",
            key => key,
        };
        Shortcut {
            modifiers,
            key: if key.len() == 1 {
                key.to_ascii_lowercase()
            } else {
                key.into()
            },
        }
    }
}

pub fn global() -> Result<Option<Shortcut>, String> {
    let config = Config::new(GLOBAL_ID, 1).map_err(|_| fl!("shortcut-save-error"))?;
    for (binding, action) in read(&config, "custom")? {
        if owned(&action) {
            return binding
                .into_rust::<GlobalBinding>()
                .map(|binding| Some(binding.shortcut()))
                .map_err(|_| fl!("shortcut-save-error"));
        }
    }
    Ok(None)
}

pub fn set_global(shortcut: Option<&Shortcut>) -> Result<(), String> {
    let config = Config::new(GLOBAL_ID, 1).map_err(|_| fl!("shortcut-save-error"))?;
    let mut custom = read(&config, "custom")?;
    custom.retain(|_, action| !owned(action));
    if let Some(shortcut) = shortcut {
        let defaults = read(&config, "defaults")?;
        for (binding, _) in custom.iter().chain(defaults.iter()) {
            // Abort if a binding cannot be understood; never overwrite another action.
            let binding = binding
                .into_rust::<GlobalBinding>()
                .map_err(|_| fl!("shortcut-save-error"))?;
            if binding.shortcut() == *shortcut {
                return Err(fl!("shortcut-conflict"));
            }
        }
        let key = match shortcut.key.as_str() {
            "Enter" => "Return",
            "Space" => "space",
            "ArrowLeft" => "Left",
            "ArrowRight" => "Right",
            "ArrowUp" => "Up",
            "ArrowDown" => "Down",
            "PageUp" => "Prior",
            "PageDown" => "Next",
            key => key,
        };
        let binding = GlobalBinding {
            modifiers: [
                (1, Modifier::Ctrl),
                (2, Modifier::Shift),
                (4, Modifier::Alt),
                (8, Modifier::Super),
            ]
            .into_iter()
            .filter(|(bit, _)| shortcut.modifiers & bit != 0)
            .map(|(_, modifier)| modifier)
            .collect(),
            key: key.into(),
            keycode: None,
            description: Some(fl!("new-orbit")),
        };
        custom.insert(
            RawValue::from_rust(&binding).map_err(|_| fl!("shortcut-save-error"))?,
            RawValue::from_rust(&GlobalAction::Spawn(COMMAND.into()))
                .map_err(|_| fl!("shortcut-save-error"))?,
        );
    }
    config
        .set("custom", custom)
        .map_err(|_| fl!("shortcut-save-error"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_winit::{conversion, platform_specific::wayland::keymap};

    #[test]
    fn new_shortcuts_preserve_existing_assignments_and_disabled_keys() {
        let mut bindings = Bindings::default();
        bindings.0.remove(&Action::Undo);
        bindings.complete();
        assert_eq!(
            bindings.action(&Shortcut {
                modifiers: 1,
                key: "z".into()
            }),
            Some(Action::Undo)
        );
        bindings.0.remove(&Action::Undo);
        bindings.0.insert(
            Action::Copy,
            Some(Shortcut {
                modifiers: 1,
                key: "z".into(),
            }),
        );
        bindings.complete();
        assert_eq!(bindings.0[&Action::Undo], None);
        bindings.complete();
        assert_eq!(bindings.0[&Action::Undo], None);
    }

    #[test]
    fn non_latin_shortcuts_match_on_floating_and_layer_surfaces() {
        let bindings = Bindings::default();
        for (character, scancode, action) in [
            ("ز", 46, Action::Copy),
            ("ر", 47, Action::Paste),
            ("ش", 30, Action::SelectAll),
            ("с", 46, Action::Copy),
        ] {
            for layer in [false, true] {
                let physical = if layer {
                    keymap::raw_keycode_to_physicalkey(scancode)
                } else {
                    keymap::scancode_to_physicalkey(scancode)
                };
                let key = keyboard::Key::Character(character.into());
                let event = keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key,
                    physical_key: conversion::physical_key(physical),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::CTRL,
                    text: None,
                    repeat: false,
                };
                let shortcut = Shortcut::from_event(&event, layer).unwrap();
                assert_eq!(bindings.action(&shortcut), Some(action));
            }
        }
    }
}
