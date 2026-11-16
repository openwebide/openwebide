//! Private descriptor-bound storage; upgrade policy stays in the transaction.
use super::*;
use openwebide_core::vfs::{NATIVE_RECOVERY_UPGRADE, VfsEntryKind};

pub(super) const CURRENT: &str = "current.json";
pub(super) const PLAN: &str = "plan.json";
pub(super) const POINTER: &str = "pointer.json";
pub(super) const ORIGINAL_ANCHOR: &str = "original-anchor";
pub(super) const ORIGINAL_DIRECTORY: &str = "original-directory";
pub(super) const COMPLETE: &str = "completed.json";
pub(super) const CANCELLED: &str = "cancelled.json";
pub(super) const MAX_PLAN: u64 = 64 * 1024;

pub(super) struct Area {
    pub dir: Dir,
    _lock: File,
}

pub(super) fn directory(parent: &Dir, name: &str, uid: u32) -> Result<Dir, String> {
    if !io(parent.symlink_metadata(name))?.is_dir() {
        return Err(UPGRADE_INVALID.into());
    }
    let dir = io(parent.open_dir(name))?;
    private(&io(dir.try_clone())?.into_std_file(), uid, true)?;
    Ok(dir)
}

pub(super) fn exists(parent: &Dir, name: &str) -> Result<bool, String> {
    match parent.symlink_metadata(name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(UPGRADE_INVALID.into()),
    }
}

pub(super) fn create_directory(parent: &Dir, name: &str, uid: u32) -> Result<Dir, String> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    io(parent.create_dir_with(name, &builder))?;
    io(crate::native_directory::sync(parent))?;
    directory(parent, name, uid)
}

pub(super) fn create(parent: &Dir, name: &str, uid: u32) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    let file = io(parent.open_with(name, &options))?.into_std();
    private(&file, uid, false)?;
    Ok(file)
}

pub(super) fn write(file: &mut File, value: &impl Serialize) -> Result<(), String> {
    let data = serde_json::to_vec(value).map_err(|_| UPGRADE_INVALID)?;
    if data.len() as u64 > MAX_PLAN {
        return Err(UPGRADE_INVALID.into());
    }
    io(file.write_all(&data))?;
    io(file.sync_all())
}

pub(super) fn read<T: serde::de::DeserializeOwned>(file: &File) -> Result<T, String> {
    serde_json::from_slice(&bytes(file, MAX_PLAN)?).map_err(|_| UPGRADE_INVALID.into())
}

pub(super) fn anchor(parent: &Dir, name: &str, uid: u32) -> Result<File, String> {
    linked_file(parent, name, uid)
}
pub(super) fn linked_file(parent: &Dir, name: &str, uid: u32) -> Result<File, String> {
    // Upgrade markers and the original anchor may have one known backup link.
    // Callers validate both exact names and the complete observed link count.
    if !io(parent.symlink_metadata(name))?.is_file() {
        return Err(UPGRADE_INVALID.into());
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    let file = io(parent.open_with(name, &options))?.into_std();
    let metadata = io(file.metadata())?;
    if metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
        || !metadata.is_file()
        || !matches!(metadata.nlink(), 1 | 2)
    {
        return Err(UPGRADE_INVALID.into());
    }
    Ok(file)
}

pub(super) fn matches(parent: &Dir, name: &str, identity: &Identity) -> Result<bool, String> {
    if !exists(parent, name)? {
        return Ok(false);
    }
    if io(parent.symlink_metadata(name))?.is_symlink() {
        return Err(UPGRADE_INVALID.into());
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    let file = io(parent.open_with(name, &options))?.into_std();
    identity.matches(&file).map_err(|_| UPGRADE_INVALID.into())
}

pub(super) fn move_entry(
    source: &Dir,
    from: &str,
    destination: &Dir,
    to: &str,
    directory: bool,
) -> Result<(), String> {
    crate::runs::native_vfs::transfer_names(
        &io(source.try_clone())?.into_std_file(),
        from,
        &io(destination.try_clone())?.into_std_file(),
        to,
        if directory {
            VfsEntryKind::Directory
        } else {
            VfsEntryKind::File
        },
    )
    .map_err(|error| error.to_string())
}

impl Area {
    pub fn open(
        parent: &Dir,
        uid: u32,
        create: bool,
        exclusive: bool,
    ) -> Result<Option<Self>, String> {
        Self::open_flushed(parent, uid, create, exclusive, |parent| {
            crate::native_directory::sync(parent)
        })
    }
    pub fn open_flushed(
        parent: &Dir,
        uid: u32,
        create: bool,
        exclusive: bool,
        flush: impl FnOnce(&Dir) -> std::io::Result<()>,
    ) -> Result<Option<Self>, String> {
        if create && !exclusive {
            return Err(UPGRADE_INVALID.into());
        }
        if !exists(parent, NATIVE_RECOVERY_UPGRADE)? {
            if !create {
                return Ok(None);
            }
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            match parent.create_dir_with(NATIVE_RECOVERY_UPGRADE, &builder) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let dir = directory(parent, NATIVE_RECOVERY_UPGRADE, uid)?;
        let file = io(dir.try_clone())?.into_std_file();
        lock(&file, exclusive)?;
        // Existence does not establish durability after an interrupted mkdir.
        // Every writer confirms the parent barrier before publication.
        if exclusive {
            io(flush(parent))?;
        }
        Ok(Some(Self { dir, _lock: file }))
    }
    pub fn generation(&self, id: &str, uid: u32) -> Result<Dir, String> {
        if !openwebide_core::workspace_transaction::valid_id(id) {
            return Err(UPGRADE_INVALID.into());
        }
        directory(&self.dir, id, uid)
    }
    pub fn generations(&self) -> Result<Vec<String>, String> {
        let mut result = Vec::new();
        for entry in io(self.dir.entries())? {
            let entry = io(entry)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| UPGRADE_INVALID)?;
            if name == CURRENT {
                continue;
            }
            if result.len() >= 16
                || !openwebide_core::workspace_transaction::valid_id(&name)
                || !io(entry.file_type())?.is_dir()
            {
                return Err(UPGRADE_INVALID.into());
            }
            result.push(name);
        }
        Ok(result)
    }
}

pub(crate) fn ensure_idle(parent: &Dir) -> Result<(), String> {
    use openwebide_core::vfs::NATIVE_RECOVERY_UPGRADE;
    if !exists(parent, NATIVE_RECOVERY_UPGRADE)? {
        return Ok(());
    }
    // SAFETY: geteuid has no preconditions.
    let dir = directory(parent, NATIVE_RECOVERY_UPGRADE, unsafe { libc::geteuid() })?;
    if exists(&dir, CURRENT)? {
        return Err("Finish the native recovery upgrade using recovery upgrade inspect before changing workspace files".into());
    }
    Ok(())
}
