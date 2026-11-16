//! Descriptor-relative filesystem primitives. Every parent is opened without
//! following links; no operation reopens the replaceable project root pathname.
use openwebide_core::{
    FileEntry,
    vfs::{MAX_READ_BYTES, VfsEntryKind, normalize_vfs_path},
};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use std::{
    fs::File,
    io::{self, Read, Write},
};

pub(crate) struct Rooted(pub File, pub bool);

pub(crate) mod revision;

pub(crate) use crate::file_identity::Identity;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
fn normalized(path: &str) -> io::Result<&str> {
    if normalize_vfs_path(path).map_err(|error| invalid(error.to_string()))? != path {
        return Err(invalid("Expected a normalized relative path"));
    }
    Ok(path)
}

impl Rooted {
    pub fn file_identity(&self, file: &File) -> io::Result<Identity> {
        if self.1 {
            Identity::legacy(file)
        } else {
            Identity::of(file)
        }
    }
    pub fn observation(&self, file: &File) -> io::Result<revision::Observation> {
        revision::Observation::of_mode(file, self.1)
    }

    pub fn directory(&self, path: &str) -> io::Result<Self> {
        normalized(path)?;
        let mut directory = self.0.try_clone()?;
        for component in path.split('/').filter(|component| !component.is_empty()) {
            directory = File::from(fs::openat(
                &directory,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
        }
        Ok(Self(directory, self.1))
    }
    fn parent<'a>(&self, path: &'a str) -> io::Result<(Self, &'a str)> {
        normalized(path)?;
        if path.is_empty() {
            return Err(invalid("A workspace entry needs a name"));
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        Ok((self.directory(parent)?, name))
    }
    pub fn kind(&self, path: &str) -> io::Result<Option<FileType>> {
        let value = (|| {
            let (parent, name) = self.parent(path)?;
            fs::statat(&parent.0, name, AtFlags::SYMLINK_NOFOLLOW)
                .map(|metadata| FileType::from_raw_mode(metadata.st_mode))
                .map_err(io::Error::from)
        })();
        match value {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    pub fn regular(&self, path: &str) -> io::Result<File> {
        let (parent, name) = self.parent(path)?;
        let file = File::from(fs::openat(
            &parent.0,
            name,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        if !file.metadata()?.is_file() {
            return Err(invalid("Expected a regular file"));
        }
        Ok(file)
    }
    pub fn object(&self, path: &str) -> io::Result<File> {
        match self.kind(path)? {
            Some(FileType::Directory) => Ok(self.directory(path)?.0),
            Some(FileType::RegularFile) => self.regular(path),
            _ => Err(invalid("Cannot identify a missing or unsupported object")),
        }
    }
    pub fn identity(&self, path: &str) -> io::Result<Identity> {
        self.file_identity(&self.object(path)?)
    }
    pub fn list(&self, path: &str, limit: usize) -> io::Result<Vec<FileEntry>> {
        let directory = self.directory(path)?;
        let mut iterator = fs::Dir::read_from(&directory.0)?;
        let mut entries = Vec::new();
        while let Some(entry) = iterator.read() {
            let entry = entry?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| invalid("Workspace resource names must be valid UTF-8"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            if entries.len() >= limit {
                return Err(invalid("Directory inventory exceeds its entry limit"));
            }
            let metadata = fs::statat(&directory.0, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)?;
            entries.push(FileEntry {
                name: name.into(),
                path: if path.is_empty() {
                    name.into()
                } else {
                    format!("{path}/{name}")
                },
                is_dir: FileType::from_raw_mode(metadata.st_mode) == FileType::Directory,
                size: u64::try_from(metadata.st_size)
                    .map_err(|_| invalid("Invalid resource size"))?,
            });
        }
        Ok(entries)
    }
    pub fn read(&self, path: &str, limit: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.regular(path)?
            .take(limit.min(MAX_READ_BYTES) + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(invalid("Resource exceeds its read budget"));
        }
        Ok(bytes)
    }
    pub fn is_empty(&self, path: &str, kind: VfsEntryKind) -> io::Result<bool> {
        if !kind.is_dir() {
            return Ok(self.regular(path)?.metadata()?.len() == 0);
        }
        let directory = self.directory(path)?;
        let mut iterator = fs::Dir::read_from(&directory.0)?;
        while let Some(entry) = iterator.read() {
            if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub fn create(&self, path: &str, kind: VfsEntryKind, private: bool) -> io::Result<()> {
        let (parent, name) = self.parent(path)?;
        if kind.is_dir() {
            fs::mkdirat(
                &parent.0,
                name,
                Mode::from_raw_mode(if private { 0o700 } else { 0o777 }),
            )?;
            self.directory(path)?.0.sync_all()?;
        } else {
            let file = File::from(fs::openat(
                &parent.0,
                name,
                OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(if private { 0o600 } else { 0o666 }),
            )?);
            file.sync_all()?;
        }
        parent.0.sync_all()
    }
    pub fn write_new(&self, path: &str, bytes: &[u8]) -> io::Result<()> {
        let (parent, name) = self.parent(path)?;
        let mut file = File::from(fs::openat(
            &parent.0,
            name,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )?);
        file.write_all(bytes)?;
        file.sync_all()?;
        parent.0.sync_all()
    }
    pub fn remove(&self, path: &str, kind: VfsEntryKind) -> io::Result<()> {
        let (parent, name) = self.parent(path)?;
        let actual = fs::statat(&parent.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let expected = if kind.is_dir() {
            FileType::Directory
        } else {
            FileType::RegularFile
        };
        if FileType::from_raw_mode(actual.st_mode) != expected {
            return Err(invalid("Resource entry kind changed"));
        }
        fs::unlinkat(
            &parent.0,
            name,
            if kind.is_dir() {
                AtFlags::REMOVEDIR
            } else {
                AtFlags::empty()
            },
        )?;
        parent.0.sync_all()
    }
    pub fn transfer(
        &self,
        from: &str,
        other: &Self,
        to: &str,
        kind: VfsEntryKind,
    ) -> io::Result<()> {
        let (source, name) = self.parent(from)?;
        let (destination, target) = other.parent(to)?;
        super::transfer::transfer_names(&source.0, name, &destination.0, target, kind).map_err(
            |error| match error {
                openwebide_core::VfsError::AlreadyExists(path) => {
                    io::Error::new(io::ErrorKind::AlreadyExists, path)
                }
                error => io::Error::other(error.to_string()),
            },
        )
    }
    pub fn link(&self, from: &str, other: &Self, to: &str) -> io::Result<()> {
        let (source, name) = self.parent(from)?;
        let (destination, target) = other.parent(to)?;
        fs::linkat(&source.0, name, &destination.0, target, AtFlags::empty())?;
        destination.0.sync_all()
    }
}
