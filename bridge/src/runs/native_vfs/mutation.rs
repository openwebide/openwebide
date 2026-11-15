//! Native mutation policy shared by raw and original-directory adapters.
use super::{NativeFsVfs, io_error, transfer, workspace_root};
use crate::folder_coordination::Folder;
use cap_std::fs::{Dir, OpenOptions};
use openwebide_core::{VfsError, vfs::VfsEntryKind};
use std::{
    collections::VecDeque,
    ffi::OsString,
    io::Write,
    path::{Component, Path, PathBuf},
};

pub(super) async fn blocking<F>(work: F) -> Result<(), VfsError>
where
    F: FnOnce() -> Result<(), VfsError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| VfsError::Io("Native file operation stopped unexpectedly".into()))?
}

pub(super) struct NativeMutations {
    directory: Dir,
    path: PathBuf,
}
impl NativeMutations {
    fn open(files: &NativeFsVfs) -> Result<Self, VfsError> {
        let path = files
            .root
            .canonicalize()
            .map_err(|error| io_error(error, "workspace root"))?;
        workspace_root(&path)?;
        let directory = Dir::open_ambient_dir(&path, cap_std::ambient_authority())
            .map_err(|error| io_error(error, "workspace root"))?;
        Ok(Self { directory, path })
    }
    pub(super) fn original(folder: &Folder) -> Result<Self, VfsError> {
        workspace_root(&folder.path)?;
        // Reopen '.' relative to the retained object, never the replaceable
        // pathname. On Windows this also obtains cap-std's no-share-delete
        // directory handle; cloning the same-file handle does not provide it.
        let directory = Dir::reopen_dir(folder.handle.as_file())
            .map_err(|error| io_error(error, "workspace root"))?;
        Ok(Self {
            directory,
            path: folder.path.clone(),
        })
    }
    fn resolve(&self, path: &str, follow_leaf: bool) -> Result<PathBuf, VfsError> {
        let lexical = openwebide_core::vfs::workspace_path(path)?;
        if lexical.is_empty() {
            return Err(VfsError::PathEscape(path.into()));
        }
        let mut pending: VecDeque<OsString> = Path::new(&lexical)
            .components()
            .map(|part| part.as_os_str().to_owned())
            .collect();
        let mut resolved = PathBuf::new();
        let mut entry = None;
        let mut links = 0;
        // Match the capability library's platform link-expansion budget.
        let maximum_links = if cfg!(windows) { 63 } else { 40 };
        while let Some(part) = pending.pop_front() {
            match Path::new(&part).components().next().unwrap() {
                Component::CurDir => continue,
                Component::ParentDir => {
                    if !resolved.pop() {
                        return Err(VfsError::PathEscape(path.into()));
                    }
                    continue;
                }
                Component::Normal(_) => {}
                _ => return Err(VfsError::PathEscape(path.into())),
            }
            resolved.push(part);
            if pending.is_empty() && !follow_leaf && entry.is_none() {
                entry = Some(resolved.clone());
            }
            match self.directory.symlink_metadata(&resolved) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    links += 1;
                    if links > maximum_links {
                        return Err(VfsError::Io("too many symbolic links".into()));
                    }
                    let target = self
                        .directory
                        .read_link_contents(&resolved)
                        .map_err(|error| io_error(error, path))?;
                    resolved.pop();
                    let target = if target.is_absolute() {
                        // Preserve supported absolute aliases into this root.
                        // Only the projection is ambient: all subsequent file
                        // primitives are relative to the retained directory.
                        let target = self.absolute_target(&target, path)?;
                        resolved.clear();
                        target
                    } else {
                        target
                    };
                    let mut parts: VecDeque<_> = target
                        .components()
                        .map(|part| part.as_os_str().to_owned())
                        .collect();
                    parts.append(&mut pending);
                    pending = parts;
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(error, path)),
            }
        }
        openwebide_core::vfs::workspace_path(&resolved.to_string_lossy().replace('\\', "/"))?;
        Ok(entry.unwrap_or(resolved))
    }
    fn absolute_target(&self, target: &Path, path: &str) -> Result<PathBuf, VfsError> {
        if let Ok(relative) = target.strip_prefix(&self.path) {
            return Ok(relative.into());
        }
        // macOS /var and /private/var (and user directory aliases) can refer
        // to the same root. Resolve the existing prefix, including dangling
        // in-root targets, without ever using the result as a write pathname.
        let mut prefix = target.to_path_buf();
        let mut missing = Vec::new();
        let mut projected = loop {
            match prefix.canonicalize() {
                Ok(path) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let name = prefix.file_name().ok_or_else(|| io_error(error, path))?;
                    missing.push(name.to_owned());
                    prefix.pop();
                }
                Err(error) => return Err(io_error(error, path)),
            }
        };
        for part in missing.into_iter().rev() {
            projected.push(part);
        }
        projected
            .strip_prefix(&self.path)
            .map(Path::to_path_buf)
            .map_err(|_| VfsError::PathEscape(path.into()))
    }
    fn parents(&self, path: &Path) -> Result<(), VfsError> {
        if let Some(parent) = path.parent() {
            self.directory
                .create_dir_all(parent)
                .map_err(|error| io_error(error, &parent.to_string_lossy()))?;
        }
        Ok(())
    }
    pub(super) fn write_bytes_sync(&self, path: &str, contents: &[u8]) -> Result<(), VfsError> {
        let target = self.resolve(path, true)?;
        self.parents(&target)?;
        self.directory
            .write(target, contents)
            .map_err(|error| io_error(error, path))
    }
    pub(super) fn create_sync(&self, path: &str, kind: VfsEntryKind) -> Result<(), VfsError> {
        let target = self.resolve(path, kind.is_dir())?;
        self.parents(&target)?;
        if kind.is_dir() {
            self.directory
                .create_dir_all(target)
                .map_err(|error| io_error(error, path))
        } else {
            self.directory
                .open_with(target, OpenOptions::new().write(true).create_new(true))
                .map(|_| ())
                .map_err(|error| io_error(error, path))
        }
    }
    pub(super) fn delete_sync(&self, path: &str) -> Result<(), VfsError> {
        let target = self.resolve(path, false)?;
        let metadata = self
            .directory
            .symlink_metadata(&target)
            .map_err(|error| io_error(error, path))?;
        if metadata.is_dir() {
            self.directory.remove_dir_all(target)
        } else {
            self.directory.remove_file(target)
        }
        .map_err(|error| io_error(error, path))
    }
    pub(super) fn copy_sync(&self, from: &str, to: &str) -> Result<(), VfsError> {
        let from = self.resolve(from, true)?;
        let to = self.resolve(to, true)?;
        self.parents(&to)?;
        self.directory
            .copy(from, &self.directory, to)
            .map(|_| ())
            .map_err(|error| io_error(error, "copy"))
    }
    pub(super) fn remove_entry_sync(&self, path: &str, kind: VfsEntryKind) -> Result<(), VfsError> {
        let target = self.resolve(path, false)?;
        let metadata = self
            .directory
            .symlink_metadata(&target)
            .map_err(|error| io_error(error, path))?;
        if metadata.file_type().is_symlink()
            || metadata.is_dir() != kind.is_dir()
            || (!kind.is_dir() && !metadata.is_file())
        {
            return Err(VfsError::Io("Workspace entry kind changed".into()));
        }
        if kind.is_dir() {
            self.directory.remove_dir(&target)
        } else {
            self.directory.remove_file(&target)
        }
        .map_err(|error| io_error(error, path))?;
        #[cfg(unix)]
        self.sync_parent(&target, path)?;
        Ok(())
    }
    #[cfg(unix)]
    fn sync_parent(&self, target: &Path, path: &str) -> Result<(), VfsError> {
        self.directory
            .open_dir(
                target
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )
            .and_then(|parent| crate::native_directory::sync(&parent))
            .map_err(|error| io_error(error, path))?;
        Ok(())
    }
    pub(super) fn move_atomic_sync(
        &self,
        from: &str,
        to: &str,
        kind: VfsEntryKind,
    ) -> Result<(), VfsError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let root = self
                .directory
                .try_clone()
                .map_err(|error| io_error(error, "workspace root"))?
                .into_std_file();
            transfer::transfer_between(&root, from, &root, to, kind)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            transfer::transfer(&self.path, from, to, kind)
        }
    }
    pub(super) fn replace_atomic_sync(&self, path: &str, content: &[u8]) -> Result<(), VfsError> {
        let target = self.resolve(path, false)?;
        let metadata = self
            .directory
            .symlink_metadata(&target)
            .map_err(|error| io_error(error, path))?;
        if !metadata.is_file() {
            return Err(VfsError::Io(
                "Atomic replacement requires an existing regular file".into(),
            ));
        }
        let mut temporary = Temporary::create(&self.directory, &self.path)
            .map_err(|error| io_error(error, path))?;
        temporary
            .file
            .set_permissions(metadata.permissions())
            .map_err(|error| io_error(error, path))?;
        temporary
            .file
            .write_all(content)
            .map_err(|error| io_error(error, path))?;
        temporary
            .file
            .sync_all()
            .map_err(|error| io_error(error, path))?;
        temporary
            .directory
            .rename(&temporary.name, &self.directory, &target)
            .map_err(|error| io_error(error, path))?;
        temporary.committed = true;
        #[cfg(unix)]
        self.sync_parent(&target, path)?;
        #[cfg(unix)]
        crate::native_directory::sync(&self.directory).map_err(|error| io_error(error, path))?;
        Ok(())
    }
}
struct Temporary {
    directory: Dir,
    name: OsString,
    file: cap_std::fs::File,
    committed: bool,
}
impl Temporary {
    fn create(directory: &Dir, path: &Path) -> std::io::Result<Self> {
        let directory = directory.try_clone()?;
        // Retain tempfile's original name generation and bounded retries.
        // Its absolute path is only a name suggestion: creation and cleanup
        // both use the captured directory. Disable its pathname destructor.
        let temporary = tempfile::Builder::new()
            .prefix(".openwebide-replace-")
            .disable_cleanup(true)
            .make_in(path, |candidate| {
                let name = candidate
                    .file_name()
                    .ok_or(std::io::ErrorKind::InvalidInput)?
                    .to_owned();
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use cap_std::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let file = directory.open_with(&name, &options)?;
                Ok((file, name))
            })?;
        let ((file, name), suggested_path) = temporary.into_parts();
        drop(suggested_path);
        Ok(Self {
            directory,
            name,
            file,
            committed: false,
        })
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.directory.remove_file(&self.name);
        }
    }
}

impl NativeFsVfs {
    pub(super) fn write_bytes_sync(&self, path: &str, contents: &[u8]) -> Result<(), VfsError> {
        NativeMutations::open(self)?.write_bytes_sync(path, contents)
    }
    pub(super) fn create_sync(&self, path: &str, kind: VfsEntryKind) -> Result<(), VfsError> {
        NativeMutations::open(self)?.create_sync(path, kind)
    }
    pub(super) fn delete_sync(&self, path: &str) -> Result<(), VfsError> {
        NativeMutations::open(self)?.delete_sync(path)
    }
    pub(super) fn copy_sync(&self, from: &str, to: &str) -> Result<(), VfsError> {
        NativeMutations::open(self)?.copy_sync(from, to)
    }
    pub(super) fn remove_entry_sync(&self, path: &str, kind: VfsEntryKind) -> Result<(), VfsError> {
        NativeMutations::open(self)?.remove_entry_sync(path, kind)
    }
    pub(super) fn move_atomic_sync(
        &self,
        from: &str,
        to: &str,
        kind: VfsEntryKind,
    ) -> Result<(), VfsError> {
        NativeMutations::open(self)?.move_atomic_sync(from, to, kind)
    }
    pub(super) fn replace_atomic_sync(&self, path: &str, contents: &[u8]) -> Result<(), VfsError> {
        NativeMutations::open(self)?.replace_atomic_sync(path, contents)
    }
}
