// SPDX-License-Identifier: GPL-3.0-only

use crate::transfer::{FileList, Transfer, is_link};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

#[derive(Debug)]
pub struct Item {
    pub path: PathBuf,
    pub name: String,
    pub directory: bool,
    pub selected: bool,
    pub text: Option<String>,
    pub order: u64,
    cache: Option<Arc<crate::cache::Directory>>,
}

#[derive(Default)]
pub struct Shelf {
    pub items: Vec<Item>,
    next_order: u64,
    selection_drag: OnceLock<Arc<Transfer>>,
}

#[derive(Default, Debug, PartialEq)]
pub struct AddReport {
    pub added: usize,
    pub duplicates: usize,
    pub rejected: usize,
}

impl Shelf {
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> AddReport {
        self.selection_drag.take();
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
            let order = self.reserve_order();
            let cache = crate::cache::Directory::for_file(&path);
            self.items.push(Item {
                path,
                name,
                directory: metadata.is_dir(),
                selected: false,
                text: None,
                order,
                cache,
            });
            report.added += 1;
        }
        report
    }

    pub fn reserve_order(&mut self) -> u64 {
        let order = self.next_order;
        self.next_order += 1;
        order
    }

    pub fn set_import(
        &mut self,
        path: &Path,
        text: Option<String>,
        cache: Option<Arc<crate::cache::Directory>>,
    ) {
        self.selection_drag.take();
        if let Some(item) = self.items.iter_mut().find(|item| item.path == path) {
            item.cache = cache;
            if let Some(text) = text {
                item.name = text
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or("Text")
                    .chars()
                    .take(160)
                    .collect();
                item.text = Some(text);
            }
        }
    }

    pub fn contains_link(&self, text: &str) -> bool {
        if !is_link(text) {
            return false;
        }
        let Ok(url) = url::Url::parse(text.trim()) else {
            return false;
        };
        self.items
            .iter()
            .filter_map(|item| item.text.as_deref())
            .filter(|text| is_link(text))
            .any(|text| url::Url::parse(text.trim()).is_ok_and(|existing| existing == url))
    }

    pub fn transfer(&self, paths: Vec<PathBuf>, clipboard: bool) -> Transfer {
        let items = paths
            .iter()
            .filter_map(|path| self.items.iter().find(|item| &item.path == path))
            .collect::<Vec<_>>();
        let text_only = !items.is_empty() && items.iter().all(|item| item.text.is_some());
        let links = text_only
            && items
                .iter()
                .all(|item| item.text.as_deref().is_some_and(is_link));
        let text = items.iter().any(|item| item.text.is_some()).then(|| {
            items
                .iter()
                .map(|item| {
                    item.text.clone().unwrap_or_else(|| {
                        url::Url::from_file_path(&item.path)
                            .map(|url| url.to_string())
                            .unwrap_or_else(|_| item.path.to_string_lossy().into_owned())
                    })
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        });
        Transfer {
            cached: items.iter().filter_map(|item| item.cache.clone()).collect(),
            files: FileList(paths),
            text,
            text_only,
            links,
            clipboard,
        }
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

    pub fn drag_transfer(&self, path: &Path) -> Arc<Transfer> {
        if self
            .items
            .iter()
            .any(|item| item.path == path && item.selected)
        {
            self.selection_drag
                .get_or_init(|| Arc::new(self.transfer(self.export_paths(), false)))
                .clone()
        } else {
            Arc::new(self.transfer(vec![path.to_owned()], false))
        }
    }

    pub fn toggle(&mut self, path: &Path) {
        self.selection_drag.take();
        if let Some(item) = self.items.iter_mut().find(|item| item.path == path) {
            item.selected = !item.selected;
        }
    }

    pub fn select_all(&mut self) {
        self.selection_drag.take();
        let selected = self.selected_count() != self.items.len();
        for item in &mut self.items {
            item.selected = selected;
        }
    }

    pub fn remove(&mut self, path: &Path) {
        self.selection_drag.take();
        self.items.retain(|item| item.path != path);
    }

    pub fn clear(&mut self) {
        self.selection_drag.take();
        let has_selection = self.selected_count() > 0;
        self.items.retain(|item| has_selection && !item.selected);
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
        let group = shelf.drag_transfer(&a);
        assert_eq!(group.files.0, [a, b.clone()]);
        assert!(Arc::ptr_eq(&group, &shelf.drag_transfer(&b)));
        assert_eq!(shelf.drag_transfer(&c).files.0, [c]);
        shelf.select_all();
        assert_eq!(shelf.selected_count(), 3);
        assert!(!Arc::ptr_eq(&group, &shelf.drag_transfer(&b)));
        shelf.select_all();
        assert_eq!(shelf.selected_count(), 0);
    }

    #[test]
    fn link_duplicates_use_normalized_urls_and_keep_distinct_targets() {
        let fixture = Fixture::new();
        let path = fixture.file("link.txt");
        let mut shelf = Shelf::default();
        shelf.add([path.clone()]);
        shelf.set_import(
            &path,
            Some("https://example.com/article?a=1#section".into()),
            None,
        );
        assert!(shelf.contains_link(" https://EXAMPLE.com:443/article?a=1#section "));
        assert!(!shelf.contains_link("https://example.com/article?a=2#section"));
        assert!(!shelf.contains_link("https://example.com/article?a=1#other"));
        assert!(!shelf.contains_link("https://example.com/article?a=1#section\nAnother line"));
    }

    #[test]
    fn clear_removes_only_selected_items_or_everything_without_selection() {
        let fixture = Fixture::new();
        let a = fixture.file("a");
        let b = fixture.file("b");
        let mut shelf = Shelf::default();
        shelf.add([a.clone(), b.clone()]);
        shelf.toggle(&a);
        shelf.clear();
        assert_eq!(shelf.export_paths(), [b]);
        shelf.clear();
        assert!(shelf.items.is_empty());
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
