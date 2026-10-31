// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::Subscription;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions, TryLockError},
    hash::{DefaultHasher, Hash, Hasher},
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
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
    let mut pending = HashMap::<PathBuf, Instant>::new();
    loop {
        let event = if pending.is_empty() {
            input
                .recv()
                .map_err(|_| io::Error::other("Folder monitor stopped"))?
        } else {
            match input.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => event,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(Event::new(EventKind::Other)),
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        .map_err(io::Error::other)?;
        if matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
        ) {
            for file in event.paths {
                if file == path && !path.is_dir() {
                    return Err(io::Error::other("Folder is unavailable"));
                }
                if !file.exists() {
                    seen.remove(&file);
                    pending.remove(&file);
                } else if !seen.contains(&file) && candidate(&file, path) {
                    pending.insert(file, Instant::now());
                }
            }
        }
        let mut ready = pending
            .iter()
            .filter(|(_, changed)| changed.elapsed() >= Duration::from_millis(500))
            .map(|(file, _)| file.clone())
            .collect::<Vec<_>>();
        for file in &ready {
            pending.remove(file);
        }
        ready.retain(|file| candidate(file, path));
        ready.sort_by_key(|file| {
            file.metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        });
        seen.extend(ready.iter().cloned());
        if !ready.is_empty() && !send(Ok(ready)) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collects_completed_files_only_from_the_selected_folder() {
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
}
