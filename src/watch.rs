// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::Subscription;
use notify::{
    Event, EventKind, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode, ModifyKind, RenameMode},
};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions, TryLockError},
    hash::{DefaultHasher, Hash, Hasher},
    io,
    path::{Path, PathBuf},
};

pub struct Folder {
    pub path: PathBuf,
    _lease: File,
}

impl Folder {
    pub fn claim(path: &Path) -> io::Result<Option<Self>> {
        let path = path.canonicalize()?;
        if !path.is_dir() {
            return Err(io::Error::other("Not a folder"));
        }
        let root = crate::cache::root()?;
        fs::create_dir_all(&root)?;
        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        std::env::var_os("WAYLAND_DISPLAY").hash(&mut hasher);
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(format!("watch-{}.lock", hasher.finish())))?;
        match lease.try_lock() {
            Ok(()) => Ok(Some(Self {
                path,
                _lease: lease,
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    pub fn subscription(&self) -> Subscription<crate::app::Message> {
        Subscription::run_with(self.path.clone(), |path| {
            let (output, input) = cosmic::iced::futures::channel::mpsc::unbounded();
            let path = path.clone();
            let errors = output.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("orbit-watch".into())
                .spawn(move || {
                    let send = |result| {
                        output
                            .unbounded_send(crate::app::Message::WatchFiles(result))
                            .is_ok()
                    };
                    if let Err(error) = monitor(&path, send) {
                        let _ = output.unbounded_send(crate::app::Message::WatchFiles(Err(
                            error.to_string()
                        )));
                    }
                })
            {
                let _ =
                    errors.unbounded_send(crate::app::Message::WatchFiles(Err(error.to_string())));
            }
            input
        })
    }
}

fn candidate(path: &Path, root: &Path) -> bool {
    let Some(name) = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
    else {
        return false;
    };
    path.parent() == Some(root)
        && !name.starts_with('.')
        && ![".part", ".crdownload", ".download", ".tmp", ".swp", "~"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        && path.is_file()
}

fn completed_files(
    event: Event,
    root: &Path,
    seen: &mut HashSet<PathBuf>,
) -> io::Result<Vec<PathBuf>> {
    let completed = matches!(
        event.kind,
        EventKind::Access(AccessKind::Close(AccessMode::Write))
            | EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Both))
    );
    let mut ready = Vec::new();
    for file in event.paths {
        if file == root && !root.is_dir() {
            return Err(io::Error::other("Folder is unavailable"));
        }
        if !file.exists() {
            seen.remove(&file);
        } else if completed && !seen.contains(&file) && candidate(&file, root) {
            seen.insert(file.clone());
            ready.push(file);
        }
    }
    ready.sort_by_key(|file| {
        file.metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    Ok(ready)
}

fn monitor(path: &Path, send: impl Fn(Result<Vec<PathBuf>, String>) -> bool) -> io::Result<()> {
    let (events, input) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        let _ = events.send(event);
    })
    .map_err(io::Error::other)?;
    watcher
        .watch(path, RecursiveMode::NonRecursive)
        .map_err(io::Error::other)?;
    let mut seen = fs::read_dir(path)?
        .flatten()
        .map(|entry| entry.path())
        .collect::<HashSet<_>>();
    loop {
        let event = input
            .recv()
            .map_err(|_| io::Error::other("Folder monitor stopped"))?
            .map_err(io::Error::other)?;
        let ready = completed_files(event, path, &mut seen)?;
        if !ready.is_empty() && !send(Ok(ready)) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_temporary_hidden_and_nested_paths() {
        let root = tempfile::tempdir().unwrap();
        for (name, expected) in [
            ("image.png", true),
            ("image.png.crdownload", false),
            ("IMAGE.PART", false),
            (".hidden", false),
        ] {
            let file = root.path().join(name);
            fs::write(&file, []).unwrap();
            assert_eq!(candidate(&file, root.path()), expected);
        }
        let nested = root.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let file = nested.join("file.txt");
        fs::write(&file, []).unwrap();
        assert!(!candidate(&file, root.path()));
        assert!(!candidate(&nested, root.path()));
    }

    #[test]
    fn waits_for_write_close_and_collects_final_renames_once() {
        use notify::event::{CreateKind, DataChange};
        use std::io::Write;

        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("existing.png");
        fs::write(&existing, []).unwrap();
        let mut seen = HashSet::from([existing.clone()]);
        let path = root.path().join("image.png");
        let mut writer = File::create(&path).unwrap();
        writer.write_all(b"first part").unwrap();
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
        ] {
            assert!(
                completed_files(
                    Event::new(kind).add_path(path.clone()),
                    root.path(),
                    &mut seen
                )
                .unwrap()
                .is_empty()
            );
        }
        writer.write_all(b"last part").unwrap();
        drop(writer);
        let close = EventKind::Access(AccessKind::Close(AccessMode::Write));
        assert_eq!(
            completed_files(
                Event::new(close).add_path(path.clone()),
                root.path(),
                &mut seen
            )
            .unwrap(),
            vec![path.clone()]
        );
        for path in [path, existing] {
            assert!(
                completed_files(Event::new(close).add_path(path), root.path(), &mut seen)
                    .unwrap()
                    .is_empty()
            );
        }

        let temporary = root.path().join("download.png.crdownload");
        fs::write(&temporary, b"complete").unwrap();
        assert!(
            completed_files(
                Event::new(close).add_path(temporary.clone()),
                root.path(),
                &mut seen
            )
            .unwrap()
            .is_empty()
        );
        let final_path = root.path().join("download.png");
        fs::rename(&temporary, &final_path).unwrap();
        for (mode, paths, expected) in [
            (
                RenameMode::To,
                vec![final_path.clone()],
                vec![final_path.clone()],
            ),
            (RenameMode::Both, vec![temporary, final_path], Vec::new()),
        ] {
            let mut event = Event::new(EventKind::Modify(ModifyKind::Name(mode)));
            event.paths = paths;
            assert_eq!(
                completed_files(event, root.path(), &mut seen).unwrap(),
                expected
            );
        }
    }
}
