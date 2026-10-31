// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug)]
pub struct Directory {
    path: PathBuf,
    _lease: File,
    handed_off: AtomicBool,
}

impl Directory {
    pub fn new(prefix: &str) -> io::Result<Arc<Self>> {
        let cache = root()?;
        fs::create_dir_all(&cache)?;
        let directory = tempfile::Builder::new().prefix(prefix).tempdir_in(cache)?;
        let lease = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(directory.path().join(".lease"))?;
        lease.lock_shared()?;
        Ok(Arc::new(Self {
            path: directory.keep(),
            _lease: lease,
            handed_off: AtomicBool::new(false),
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn for_file(path: &Path) -> Option<Arc<Self>> {
        let directory = path.parent()?;
        let name = directory.file_name()?.to_str()?;
        if directory.parent()? != root().ok()?
            || !(name.starts_with("orbit-image-")
                || name.starts_with("orbit-text-")
                || name.starts_with("orbit-archive-")
                || name.starts_with("orbit-drop-"))
        {
            return None;
        }
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join(".lease"))
            .ok()?;
        lease.try_lock_shared().ok()?;
        Some(Arc::new(Self {
            path: directory.to_owned(),
            _lease: lease,
            handed_off: AtomicBool::new(directory.join(".released").exists()),
        }))
    }

    pub fn hand_off(&self) {
        if !self.handed_off.swap(true, Ordering::Relaxed) {
            let _ = fs::write(self.path.join(".released"), []);
        }
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if self.handed_off.load(Ordering::Relaxed) || self.path.join(".released").exists() {
            let _ = fs::write(self.path.join(".released"), []);
        } else if self._lease.try_lock().is_ok() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub fn root() -> io::Result<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .map(|path| path.join("io.github.motixo.Orbit/items"))
        .ok_or_else(|| io::Error::other("Missing cache directory"))
}

pub fn protect_clipboard(directories: &[Arc<Directory>]) -> io::Result<()> {
    let cache = root()?;
    fs::create_dir_all(&cache)?;
    let mut hasher = DefaultHasher::new();
    std::env::var_os("XDG_RUNTIME_DIR").hash(&mut hasher);
    std::env::var_os("WAYLAND_DISPLAY").hash(&mut hasher);
    let mut file = tempfile::NamedTempFile::new_in(&cache)?;
    for directory in directories {
        if let Ok(uri) = url::Url::from_file_path(directory.path()) {
            writeln!(file, "{uri}")?;
        }
    }
    file.persist(cache.join(format!("clipboard-{}.list", hasher.finish())))?;
    Ok(())
}

pub fn start_cleanup() -> io::Result<()> {
    std::thread::Builder::new()
        .name("orbit-cache".into())
        .spawn(|| {
            loop {
                if let Ok(cache) = root() {
                    cleanup(&cache);
                }
                std::thread::sleep(Duration::from_secs(60));
            }
        })
        .map(|_| ())
}

fn cleanup(cache: &Path) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    let mut protected = HashSet::new();
    let mut directories = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("clipboard-") && name.ends_with(".list") {
            if let Ok(contents) = fs::read_to_string(entry.path()) {
                protected.extend(
                    contents
                        .lines()
                        .filter_map(|line| url::Url::parse(line).ok()?.to_file_path().ok()),
                );
            }
        } else if (name.starts_with("orbit-image-")
            || name.starts_with("orbit-text-")
            || name.starts_with("orbit-archive-")
            || name.starts_with("orbit-drop-"))
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            directories.push(entry.path());
        }
    }
    for directory in directories {
        let Ok(lease) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join(".lease"))
        else {
            if directory
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age >= Duration::from_secs(60))
            {
                // Legacy versions had no leases; only remove their empty directories.
                let _ = fs::remove_dir(&directory);
            }
            continue;
        };
        if lease
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_none_or(|age| age < Duration::from_secs(60))
            || lease.try_lock().is_err()
        {
            continue;
        }
        let empty = fs::read_dir(&directory).is_ok_and(|mut entries| {
            entries.all(|entry| {
                entry.is_ok_and(|entry| {
                    matches!(entry.file_name().to_str(), Some(".lease" | ".released"))
                })
            })
        });
        let expired = fs::metadata(directory.join(".released"))
            .or_else(|_| lease.metadata())
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= RETENTION);
        // Keep handed-off files long enough for receivers; the latest Orbit clipboard is exempt.
        if empty || (expired && !protected.contains(&directory)) {
            let _ = fs::remove_dir_all(directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_keeps_active_and_clipboard_files_but_removes_expired_files() {
        let cache = tempfile::tempdir().unwrap();
        let directory = cache.path().join("orbit-image-expired");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("image.png"), b"payload").unwrap();
        let lease = File::create(directory.join(".lease")).unwrap();
        let released = File::create(directory.join(".released")).unwrap();
        let old = std::time::SystemTime::now() - RETENTION - Duration::from_secs(60);
        lease.set_modified(old).unwrap();
        released.set_modified(old).unwrap();
        lease.lock_shared().unwrap();
        cleanup(cache.path());
        assert!(directory.exists());
        drop(lease);
        let manifest = cache.path().join("clipboard-test.list");
        fs::write(
            &manifest,
            url::Url::from_file_path(&directory).unwrap().as_str(),
        )
        .unwrap();
        cleanup(cache.path());
        assert!(directory.exists());
        fs::remove_file(&manifest).unwrap();
        cleanup(cache.path());
        assert!(!directory.exists());

        fs::create_dir(&directory).unwrap();
        let lease = File::create(directory.join(".lease")).unwrap();
        lease.set_modified(old).unwrap();
        fs::write(
            &manifest,
            url::Url::from_file_path(&directory).unwrap().as_str(),
        )
        .unwrap();
        cleanup(cache.path());
        assert!(!directory.exists());
    }

    #[test]
    fn cache_is_shared_until_last_owner_and_handoffs_survive_release() {
        let directory = Directory::new("orbit-text-").unwrap();
        let path = directory.path().to_owned();
        let second = directory.clone();
        drop(directory);
        assert!(path.exists());
        drop(second);
        assert!(!path.exists());

        let directory = Directory::new("orbit-text-").unwrap();
        let path = directory.path().to_owned();
        directory.hand_off();
        drop(directory);
        assert!(path.join(".released").exists());
        fs::remove_dir_all(path).unwrap();
    }
}
