// SPDX-License-Identifier: GPL-3.0-only

use crate::{config::Corner, shelf::Shelf};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub id: String,
    pub items: Vec<SavedItem>,
    pub pinned: bool,
    pub corner: Corner,
    pub copy_mode: bool,
    pub collapsed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SavedItem {
    pub uri: String,
    pub text: Option<String>,
    pub selected: bool,
}

impl Entry {
    pub fn label(&self) -> String {
        let name = self
            .items
            .last()
            .and_then(|item| {
                item.text
                    .as_deref()
                    .map(|text| text.lines().next().unwrap_or("Text").to_owned())
                    .or_else(|| {
                        item.path()?
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
            })
            .unwrap_or_else(|| "Orbit".into());
        format!(
            "{} · {}",
            name.chars().take(60).collect::<String>(),
            self.items.len()
        )
    }
}

impl SavedItem {
    pub fn path(&self) -> Option<PathBuf> {
        url::Url::parse(&self.uri).ok()?.to_file_path().ok()
    }
}

fn read(path: &Path) -> io::Result<Vec<Entry>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub fn list() -> io::Result<Vec<Entry>> {
    read(&crate::cache::root()?.join("recent.json"))
}

pub fn save(
    shelf: &Shelf,
    pinned: bool,
    corner: Corner,
    copy_mode: bool,
    collapsed: bool,
    restored_id: Option<&str>,
) -> io::Result<()> {
    if shelf.items.is_empty() {
        return Ok(());
    }
    let root = crate::cache::root()?;
    fs::create_dir_all(&root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("recent.lock"))?;
    lock.lock()?;
    let mut ordered = shelf.items.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|item| item.order);
    let entry = Entry {
        id: restored_id
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        items: ordered
            .into_iter()
            .map(|item| {
                Ok(SavedItem {
                    uri: url::Url::from_file_path(&item.path)
                        .map_err(|_| io::Error::other("Invalid history path"))?
                        .into(),
                    text: item.text.clone(),
                    selected: item.selected,
                })
            })
            .collect::<io::Result<_>>()?,
        pinned,
        corner,
        copy_mode,
        collapsed,
    };
    let path = root.join("recent.json");
    let mut entries = read(&path)?;
    if !store(&mut entries, entry, restored_id.is_some()) {
        return Ok(());
    }
    let mut file = tempfile::NamedTempFile::new_in(&root)?;
    serde_json::to_writer(&mut file, &entries)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    for directory in shelf
        .transfer(
            shelf.items.iter().map(|item| item.path.clone()).collect(),
            false,
        )
        .cached
    {
        directory.hand_off();
    }
    Ok(())
}

fn store(entries: &mut Vec<Entry>, entry: Entry, restored: bool) -> bool {
    if restored {
        let Some(saved) = entries.iter_mut().find(|saved| saved.id == entry.id) else {
            return false;
        };
        *saved = entry;
    } else {
        entries.insert(0, entry);
        entries.truncate(5);
    }
    true
}

pub fn protected(root: &Path) -> HashSet<PathBuf> {
    read(&root.join("recent.json"))
        .unwrap_or_default()
        .into_iter()
        .flat_map(|entry| entry.items)
        .filter_map(|item| item.path()?.parent().map(Path::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_keeps_five_shelves_and_updates_restored_entries_in_place() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from("/tmp").join(std::ffi::OsString::from_vec(vec![0xff]));
        let item = SavedItem {
            uri: url::Url::from_file_path(&path).unwrap().into(),
            text: None,
            selected: false,
        };
        assert_eq!(item.path(), Some(path));
        let mut entries = Vec::new();
        for index in 0..7 {
            store(
                &mut entries,
                Entry {
                    id: index.to_string(),
                    items: vec![item.clone()],
                    pinned: false,
                    corner: Corner::TopRight,
                    copy_mode: false,
                    collapsed: false,
                },
                false,
            );
        }
        let restored: Vec<Entry> =
            serde_json::from_slice(&serde_json::to_vec(&entries).unwrap()).unwrap();
        assert_eq!(restored.len(), 5);
        assert_eq!(restored[0].id, "6");
        assert_eq!(restored[4].id, "2");
        assert_eq!(restored[0].items[0].path(), entries[0].items[0].path());

        let ids = entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        let mut reopened = entries[2].clone();
        reopened.collapsed = true;
        assert!(store(&mut entries, reopened.clone(), true));
        assert!(entries[2].collapsed);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
            ids
        );

        reopened.id = "evicted".into();
        assert!(!store(&mut entries, reopened, true));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
    }
}
