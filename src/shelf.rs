// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Item {
    pub path: PathBuf,
    pub name: String,
    pub directory: bool,
    pub selected: bool,
}

#[derive(Default)]
pub struct Shelf {
    pub items: Vec<Item>,
}

#[derive(Default, Debug, PartialEq)]
pub struct AddReport {
    pub added: usize,
    pub duplicates: usize,
    pub rejected: usize,
}

impl Shelf {
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> AddReport {
        let mut report = AddReport::default();
        for path in paths {
            let Ok(path) = std::path::absolute(path) else {
                report.rejected += 1;
                continue;
            };
            let Ok(metadata) = path.metadata() else {
                report.rejected += 1;
                continue;
            };
            if !metadata.is_file() && !metadata.is_dir() {
                report.rejected += 1;
                continue;
            }
            if self.items.iter().any(|item| item.path == path) {
                report.duplicates += 1;
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into();
            self.items.push(Item {
                path,
                name,
                directory: metadata.is_dir(),
                selected: false,
            });
            report.added += 1;
        }
        report
    }

    pub fn selected_count(&self) -> usize {
        self.items.iter().filter(|item| item.selected).count()
    }

    pub fn export_paths(&self) -> Vec<PathBuf> {
        let has_selection = self.selected_count() > 0;
        self.items
            .iter()
            .filter(|item| !has_selection || item.selected)
            .map(|item| item.path.clone())
            .collect()
    }

    pub fn drag_paths(&self, path: &Path) -> Vec<PathBuf> {
        if self
            .items
            .iter()
            .any(|item| item.path == path && item.selected)
        {
            self.export_paths()
        } else {
            vec![path.to_owned()]
        }
    }

    pub fn toggle(&mut self, path: &Path) {
        if let Some(item) = self.items.iter_mut().find(|item| item.path == path) {
            item.selected = !item.selected;
        }
    }

    pub fn select_all(&mut self) {
        let selected = self.selected_count() != self.items.len();
        for item in &mut self.items {
            item.selected = selected;
        }
    }

    pub fn remove(&mut self, path: &Path) {
        self.items.retain(|item| item.path != path);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "orbit-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, b"original contents").unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn collect_deduplicates_full_paths_and_rejects_missing_files() {
        let fixture = Fixture::new();
        let a = fixture.file("report.txt");
        let directory = fixture.0.join("folder");
        fs::create_dir(&directory).unwrap();
        let b = directory.join("report.txt");
        fs::write(&b, "different file").unwrap();
        let mut shelf = Shelf::default();
        assert_eq!(
            shelf.add([a.clone(), a, b, directory, fixture.0.join("missing")]),
            AddReport {
                added: 3,
                duplicates: 1,
                rejected: 1
            }
        );
        assert!(shelf.items[2].directory);
    }

    #[test]
    fn selections_control_group_drags_without_affecting_single_item_drags() {
        let fixture = Fixture::new();
        let a = fixture.file("a");
        let b = fixture.file("b");
        let c = fixture.file("c");
        let mut shelf = Shelf::default();
        shelf.add([a.clone(), b.clone(), c.clone()]);
        assert_eq!(shelf.export_paths(), [a.clone(), b.clone(), c.clone()]);
        shelf.toggle(&a);
        shelf.toggle(&b);
        assert_eq!(shelf.drag_paths(&a), [a, b]);
        assert_eq!(shelf.drag_paths(&c), [c]);
        shelf.select_all();
        assert_eq!(shelf.selected_count(), 3);
        shelf.select_all();
        assert_eq!(shelf.selected_count(), 0);
    }

    #[test]
    fn remove_and_clear_never_change_originals() {
        let fixture = Fixture::new();
        let path = fixture.file("keep me.txt");
        let mut shelf = Shelf::default();
        shelf.add([path.clone()]);
        shelf.remove(&path);
        shelf.add([path.clone()]);
        shelf.clear();
        assert!(shelf.items.is_empty());
        assert_eq!(fs::read(path).unwrap(), b"original contents");
    }

    #[test]
    fn reject_special_files_and_keep_symlink_identity() {
        let fixture = Fixture::new();
        let path = fixture.file("target");
        let link = fixture.0.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let mut shelf = Shelf::default();
        assert_eq!(
            shelf
                .add([link.clone(), PathBuf::from("/dev/null")])
                .rejected,
            1
        );
        assert_eq!(shelf.items[0].path, link);
    }
}
