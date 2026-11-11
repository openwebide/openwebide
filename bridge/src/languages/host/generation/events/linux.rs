//! Nonrecursive inotify primitives bound to owned object descriptors. Multiple
//! hard-link paths share a kernel watch, so registrations retain reference counts.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
};

pub(super) type Id = i32;
pub(super) struct Kernel {
    descriptor: OwnedFd,
    references: BTreeMap<Id, usize>,
    removed: BTreeSet<Id>,
}

pub(super) fn open(path: &Path) -> Result<File, String> {
    // O_PATH cannot block on a substituted FIFO and retains the exact object
    // for metadata comparison and /proc/self/fd registration.
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| error.to_string())
}

impl Kernel {
    pub fn new(_available: std::sync::Arc<tokio::sync::Semaphore>) -> Result<Self, String> {
        // The successful syscall transfers one nonblocking close-on-exec fd.
        let descriptor = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // This is a unique live descriptor returned by the syscall above.
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        Ok(Self {
            descriptor,
            references: BTreeMap::new(),
            removed: BTreeSet::new(),
        })
    }

    pub fn register(&mut self, file: &File, directory: bool, _path: &Path) -> Result<Id, String> {
        let path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
        let mask = libc::IN_ATTRIB
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF
            | libc::IN_MASK_ADD
            | if directory {
                libc::IN_CREATE
                    | libc::IN_DELETE
                    | libc::IN_MOVED_FROM
                    | libc::IN_MOVED_TO
                    | libc::IN_ONLYDIR
            } else {
                libc::IN_MODIFY | libc::IN_CLOSE_WRITE
            };
        // The owned file remains live through registration. Following its proc
        // descriptor link selects that exact object, without a pathname race.
        let id =
            unsafe { libc::inotify_add_watch(self.descriptor.as_raw_fd(), path.as_ptr(), mask) };
        if id < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if self.removed.contains(&id) {
            return Err(
                "Project-source event identity was reused before its removal was observed".into(),
            );
        }
        let count = self.references.entry(id).or_default();
        *count = count
            .checked_add(1)
            .ok_or("Project-source event reference count exhausted")?;
        Ok(id)
    }

    pub fn remove(&mut self, id: Id) -> Result<(), String> {
        let count = self
            .references
            .get_mut(&id)
            .ok_or("Unknown project-source event registration")?;
        if *count > 1 {
            *count -= 1;
            return Ok(());
        }
        // Removing the final path produces IN_IGNORED. Retain that expected
        // receipt until it is drained; unexpected removals invalidate coverage.
        if unsafe { libc::inotify_rm_watch(self.descriptor.as_raw_fd(), id) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        self.references.remove(&id);
        if !self.removed.insert(id) {
            return Err("Duplicate project-source event removal".into());
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        let mut buffer = [0_u8; 8192];
        let mut changed = false;
        for _ in 0..32 {
            // The writable buffer is live and bounded. This fd was created
            // nonblocking; no read can wait for a future event.
            let count = unsafe {
                libc::read(
                    self.descriptor.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                match error.kind() {
                    std::io::ErrorKind::WouldBlock => return Ok(changed),
                    std::io::ErrorKind::Interrupted => continue,
                    _ => return Err(error.to_string()),
                }
            }
            let count = usize::try_from(count).unwrap();
            if count == 0 || count > buffer.len() {
                return Err("Invalid project-source event read".into());
            }
            changed |= self.records(&buffer[..count])?;
        }
        Err("Project-source event collection exceeds its work budget".into())
    }

    fn records(&mut self, mut bytes: &[u8]) -> Result<bool, String> {
        let mut changed = false;
        let header = std::mem::size_of::<libc::inotify_event>();
        while !bytes.is_empty() {
            if bytes.len() < header {
                return Err("Truncated project-source event header".into());
            }
            // The checked bytes contain one complete header. read_unaligned
            // handles any alignment of this byte buffer; names are never decoded.
            let event = unsafe {
                bytes
                    .as_ptr()
                    .cast::<libc::inotify_event>()
                    .read_unaligned()
            };
            let length = header
                .checked_add(usize::try_from(event.len).unwrap())
                .filter(|length| *length <= bytes.len())
                .ok_or("Truncated project-source event name")?;
            changed |= self.event(event.wd, event.mask)?;
            bytes = &bytes[length..];
        }
        Ok(changed)
    }

    fn event(&mut self, id: Id, mask: u32) -> Result<bool, String> {
        if mask & (libc::IN_Q_OVERFLOW | libc::IN_UNMOUNT) != 0 {
            return Err("Project-source event coverage requires a new observation".into());
        }
        if mask & libc::IN_IGNORED != 0 {
            if self.removed.remove(&id) && mask & !(libc::IN_IGNORED | libc::IN_ISDIR) == 0 {
                return Ok(false);
            }
            return Err("A project-source event watch was lost".into());
        }
        if !self.references.contains_key(&id) && !self.removed.contains(&id) {
            return Err("Unknown project-source event registration".into());
        }
        let changes = libc::IN_ATTRIB
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE;
        if mask & changes == 0 {
            return Err("Unknown project-source event kind".into());
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
