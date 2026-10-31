// SPDX-License-Identifier: GPL-3.0-only

use crate::{cache::Directory, import::ImportedFiles, transfer::FileList};
use flate2::{Compression, write::GzEncoder};
use std::{
    collections::HashSet,
    error::Error,
    fs::{self, File},
    io::{self, BufWriter, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use zip::{ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Copy, Debug, Default)]
pub enum Format {
    #[default]
    Zip,
    Tgz,
}

impl Format {
    pub const ALL: &[Self] = &[Self::Zip, Self::Tgz];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Zip => ".zip",
            Self::Tgz => ".tgz",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Zip => 0,
            Self::Tgz => 1,
        }
    }
}

impl AsRef<str> for Format {
    fn as_ref(&self) -> &str {
        self.extension()
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && !matches!(name, "." | "..") && !name.contains(['/', '\0'])
}

pub fn create(
    paths: &[PathBuf],
    name: &str,
    format: Format,
    cancelled: impl Fn() -> bool,
) -> Result<ImportedFiles, Box<dyn Error + Send + Sync>> {
    if !valid_name(name) || paths.is_empty() {
        return Err(io::Error::other("Invalid archive name or empty selection").into());
    }
    let directory = Directory::new("orbit-archive-")?;
    let excluded = directory.path().canonicalize()?;
    let path = directory
        .path()
        .join(format!("{name}{}", format.extension()));
    let writer = Cancellable {
        inner: BufWriter::new(File::create_new(&path)?),
        cancelled,
    };
    let mut names = HashSet::new();
    let mut entries = Vec::new();
    for source in paths {
        let original = source.file_name().unwrap_or_else(|| "root".as_ref());
        let mut name = PathBuf::from(original);
        let mut suffix = 2;
        while !names.insert(name.clone()) {
            let mut unique = source.file_stem().unwrap_or(original).to_os_string();
            unique.push(format!(" ({suffix})"));
            if let Some(extension) = source.extension() {
                unique.push(".");
                unique.push(extension);
            }
            name = PathBuf::from(unique);
            suffix += 1;
        }
        entries.push((source, name));
    }
    let mut writer = match format {
        Format::Zip => {
            let mut archive = ZipWriter::new(writer);
            for (source, name) in entries {
                zip_entry(&mut archive, source, &name, &excluded)?;
            }
            archive.finish()?
        }
        Format::Tgz => {
            let mut archive = tar::Builder::new(GzEncoder::new(writer, Compression::default()));
            archive.follow_symlinks(false);
            for (source, name) in entries {
                tar_entry(&mut archive, source, &name, &excluded)?;
            }
            archive.into_inner()?.finish()?
        }
    };
    writer.flush()?;
    writer.inner.get_ref().sync_all()?;
    Ok(ImportedFiles {
        files: FileList(vec![path]),
        directory: Some(directory),
        text: None,
    })
}

fn zip_entry<W: Write + Seek>(
    archive: &mut ZipWriter<W>,
    source: &Path,
    name: &Path,
    excluded: &Path,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let metadata = fs::symlink_metadata(source)?;
    // A selected parent folder must not include the archive being written.
    if metadata.is_dir() && source.canonicalize()? == excluded {
        return Ok(());
    }
    let name_str = name.to_str().ok_or_else(|| {
        io::Error::other("ZIP requires UTF-8 file names; use TGZ for this selection")
    })?;
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(metadata.permissions().mode())
        .large_file(metadata.len() >= u32::MAX.into());
    if metadata.is_symlink() {
        let target = fs::read_link(source)?;
        let target = target
            .to_str()
            .ok_or_else(|| io::Error::other("ZIP requires UTF-8 symlinks; use TGZ"))?;
        archive.add_symlink(name_str, target, options)?;
    } else if metadata.is_dir() {
        archive.add_directory(name_str, options)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            zip_entry(
                archive,
                &entry.path(),
                &name.join(entry.file_name()),
                excluded,
            )?;
        }
    } else if metadata.is_file() {
        archive.start_file(name_str, options)?;
        io::copy(&mut File::open(source)?, archive)?;
    } else {
        return Err(io::Error::other("Cannot compress a special file").into());
    }
    Ok(())
}

fn tar_entry<W: Write>(
    archive: &mut tar::Builder<W>,
    source: &Path,
    name: &Path,
    excluded: &Path,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() && source.canonicalize()? == excluded {
        return Ok(());
    }
    archive.append_path_with_name(source, name)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            tar_entry(
                archive,
                &entry.path(),
                &name.join(entry.file_name()),
                excluded,
            )?;
        }
    }
    Ok(())
}

struct Cancellable<W, F> {
    inner: W,
    cancelled: F,
}

impl<W: Write, F: Fn() -> bool> Write for Cancellable<W, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            return Err(io::Error::other("Compression cancelled"));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek, F> Seek for Cancellable<W, F> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn archives_preserve_duplicate_names_and_originals() {
        let fixture = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for (folder, contents) in [("a", "first"), ("b", "second")] {
            let folder = fixture.path().join(folder);
            fs::create_dir(&folder).unwrap();
            let path = folder.join("item.txt");
            fs::write(&path, contents).unwrap();
            paths.push(path);
        }
        for format in Format::ALL {
            let result = create(&paths, "Archive", *format, || false).unwrap();
            let file = File::open(&result.files.0[0]).unwrap();
            let mut contents = Vec::new();
            match format {
                Format::Zip => {
                    let mut archive = zip::ZipArchive::new(file).unwrap();
                    for index in 0..archive.len() {
                        let mut entry = archive.by_index(index).unwrap();
                        let mut text = String::new();
                        entry.read_to_string(&mut text).unwrap();
                        contents.push((entry.name().to_owned(), text));
                    }
                }
                Format::Tgz => {
                    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
                    for entry in archive.entries().unwrap() {
                        let mut entry = entry.unwrap();
                        let name = entry.path().unwrap().to_str().unwrap().to_owned();
                        let mut text = String::new();
                        entry.read_to_string(&mut text).unwrap();
                        contents.push((name, text));
                    }
                }
            }
            assert_eq!(
                contents,
                [
                    ("item.txt".into(), "first".into()),
                    ("item (2).txt".into(), "second".into())
                ]
            );
        }
        assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "first");
        assert_eq!(fs::read_to_string(&paths[1]).unwrap(), "second");
        assert!(create(&paths, "../invalid", Format::Zip, || false).is_err());
        assert!(create(&paths, "Cancelled", Format::Tgz, || true).is_err());
    }
}
