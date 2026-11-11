//! Nonrecursive directory notifications. Pending reads own their handles and
//! kernel-visible memory through confirmed completion, including retirement.
use super::super::windows as identity;
use std::{
    cell::UnsafeCell,
    collections::BTreeMap,
    fs::{File, OpenOptions},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, mpsc},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use windows_sys::Win32::{
    Foundation::{ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, GetLastError},
    Globalization::{CSTR_EQUAL, CompareStringOrdinal},
    Storage::FileSystem::{
        FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
        FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES,
        FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
        FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SECURITY, FILE_NOTIFY_CHANGE_SIZE,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        ReadDirectoryChangesW,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Threading::CreateEventW,
    },
};

const WORDS: usize = 4096;
// Directory timestamps can change when an unrelated child is written. Keep
// object kinds so parent watches use directory names only for namespace events;
// their own reads cover child names, while metadata scans cover attributes.
type Names = BTreeMap<(Vec<u16>, bool), usize>;
pub(super) type Id = u64;

pub(super) fn open(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OVERLAPPED,
        )
        .open(path)
        .map_err(|error| error.to_string())
}

struct Reading {
    // Owned descriptors and memory are freed before their permits.
    file: File,
    _descriptor: OwnedSemaphorePermit,
    event: OwnedHandle,
    _event_permit: OwnedSemaphorePermit,
    data: Box<UnsafeCell<[u32; WORDS]>>,
    overlapped: Box<UnsafeCell<OVERLAPPED>>,
    _bytes: OwnedSemaphorePermit,
    pending: bool,
    cancelled: bool,
}
// The exclusive owner moves only boxes, preserving the addresses passed to the
// kernel. UnsafeCell permits kernel writes; result memory is read only after
// GetOverlappedResult confirms completion. No other Rust thread accesses it.
unsafe impl Send for Reading {}

impl Reading {
    fn issue(&mut self) -> Result<(), String> {
        if self.pending {
            return Err("A directory notification is already pending".into());
        }
        // The previous operation is complete. These stable allocations remain
        // owned until its successor also completes or is retired for cleanup.
        unsafe {
            self.overlapped.get().write(OVERLAPPED::default());
            (*self.overlapped.get()).hEvent = self.event.as_raw_handle();
        }
        let ok = unsafe {
            ReadDirectoryChangesW(
                self.file.as_raw_handle(),
                self.data.get().cast(),
                u32::try_from(WORDS * 4).unwrap(),
                0,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_ATTRIBUTES
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_CREATION
                    | FILE_NOTIFY_CHANGE_SECURITY,
                std::ptr::null_mut(),
                self.overlapped.get(),
                None,
            )
        };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                return Err(std::io::Error::from_raw_os_error(i32::from_ne_bytes(
                    error.to_ne_bytes(),
                ))
                .to_string());
            }
        }
        self.pending = true;
        self.cancelled = false;
        Ok(())
    }

    fn completed(&mut self) -> Result<Option<usize>, String> {
        if !self.pending {
            return Err("A directory notification is not pending".into());
        }
        let mut bytes = 0;
        // All arguments remain owned and valid. A nonblocking incomplete result
        // retains the allocations; any other I/O status completes this request.
        let ok = unsafe {
            GetOverlappedResult(
                self.file.as_raw_handle(),
                self.overlapped.get(),
                &raw mut bytes,
                0,
            )
        };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_IO_INCOMPLETE {
                return Ok(None);
            }
            self.pending = false;
            return Err(
                std::io::Error::from_raw_os_error(i32::from_ne_bytes(error.to_ne_bytes()))
                    .to_string(),
            );
        }
        self.pending = false;
        let bytes = usize::try_from(bytes).unwrap();
        if bytes == 0 || bytes > WORDS * 4 {
            return Err("Directory notification coverage requires a new observation".into());
        }
        Ok(Some(bytes))
    }

    fn bytes(&self, length: usize) -> Result<&[u8], String> {
        if self.pending || length > WORDS * 4 {
            return Err("Directory notification data is not ready".into());
        }
        // Completion synchronizes kernel writes before this bounded read. No
        // new operation is issued until this borrow has ended.
        Ok(unsafe { std::slice::from_raw_parts(self.data.get().cast(), length) })
    }

    fn cancel(&mut self) {
        if self.pending && !self.cancelled {
            self.cancelled = true;
            // Cancellation is advisory. The reaper keeps every allocation and
            // permit until the original request's completion is observed.
            unsafe {
                CancelIoEx(self.file.as_raw_handle(), self.overlapped.get());
            }
        }
    }
}

static CLEANUP: OnceLock<Result<mpsc::Sender<Box<Reading>>, String>> = OnceLock::new();
#[allow(
    clippy::vec_box,
    reason = "Kernel-visible allocations must retain their addresses through cancellation completion"
)]
fn cleanup() -> Result<&'static mpsc::Sender<Box<Reading>>, String> {
    CLEANUP
        .get_or_init(|| {
            let (send, receive) = mpsc::channel::<Box<Reading>>();
            std::thread::Builder::new()
                .name("language-event-cleanup".into())
                .spawn(move || {
                    let mut waiting = Vec::<Box<Reading>>::new();
                    loop {
                        match receive.recv_timeout(Duration::from_millis(20)) {
                            Ok(read) => waiting.push(read),
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) if waiting.is_empty() => {
                                break;
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => {}
                        }
                        for _ in 0..64 {
                            let Ok(read) = receive.try_recv() else {
                                break;
                            };
                            waiting.push(read);
                        }
                        waiting.retain_mut(|read| {
                            read.cancel();
                            let _ = read.completed();
                            read.pending
                        });
                    }
                })
                .map_err(|error| error.to_string())?;
            Ok(send)
        })
        .as_ref()
        .map_err(Clone::clone)
}

struct Read(Option<Box<Reading>>);
impl Read {
    fn new(
        file: File,
        descriptor: OwnedSemaphorePermit,
        available: &Arc<Semaphore>,
    ) -> Result<Self, String> {
        let event_permit = super::descriptor(available)?;
        // Each read has its own manual-reset event, retained with its handle
        // and OVERLAPPED until confirmed completion, including cancellation.
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // CreateEventW transferred ownership of this non-null handle.
        let event = unsafe { OwnedHandle::from_raw_handle(handle) };
        let bytes = super::buffer(
            WORDS * 4 + std::mem::size_of::<OVERLAPPED>() + std::mem::size_of::<Reading>(),
        )?;
        let mut read = Self(Some(Box::new(Reading {
            file,
            _descriptor: descriptor,
            event,
            _event_permit: event_permit,
            data: Box::new(UnsafeCell::new([0; WORDS])),
            overlapped: Box::new(UnsafeCell::new(OVERLAPPED::default())),
            _bytes: bytes,
            pending: false,
            cancelled: false,
        })));
        read.get().issue()?;
        Ok(read)
    }
    fn get(&mut self) -> &mut Reading {
        self.0.as_deref_mut().expect("owned notification read")
    }
}
impl Drop for Read {
    fn drop(&mut self) {
        let Some(read) = self.0.take() else {
            return;
        };
        if read.pending {
            // The queue is bounded by the descriptor and buffer permits owned
            // by every queued item. One process-wide reaper owns retirement.
            match cleanup() {
                Ok(sender) => {
                    if let Err(error) = sender.send(read) {
                        // Retain kernel-visible memory if the cleanup service has
                        // failed. Its permits stay charged; no unsafe early reuse.
                        std::mem::forget(error.0);
                    }
                }
                Err(_) => std::mem::forget(read),
            }
        }
    }
}

struct Directory {
    read: Read,
    owners: usize,
    names: Names,
}
struct Registration {
    parent: Option<(PathBuf, Vec<u16>)>,
    directory: Option<PathBuf>,
}
pub(super) struct Kernel {
    directories: BTreeMap<PathBuf, Directory>,
    registrations: BTreeMap<Id, Registration>,
    next: Id,
    available: Arc<Semaphore>,
}
impl Kernel {
    pub fn new(available: Arc<Semaphore>) -> Result<Self, String> {
        cleanup()?;
        Ok(Self {
            directories: BTreeMap::new(),
            registrations: BTreeMap::new(),
            next: 0,
            available,
        })
    }

    fn directory(&mut self, path: &Path, file: Option<&File>) -> Result<&mut Directory, String> {
        if !self.directories.contains_key(path) {
            let permit = super::descriptor(&self.available)?;
            let file = file.map_or_else(
                || open(path),
                |file| file.try_clone().map_err(|error| error.to_string()),
            )?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("A notification directory is no longer available".into());
            }
            let directory = Directory {
                read: Read::new(file, permit, &self.available)?,
                owners: 0,
                names: BTreeMap::new(),
            };
            self.directories.insert(path.to_path_buf(), directory);
        }
        let directory = self.directories.get_mut(path).expect("inserted directory");
        if let Some(file) = file
            && !identity::same_object(file, &directory.read.get().file)?
        {
            return Err("A notification directory changed identity".into());
        }
        Ok(directory)
    }

    pub fn register(&mut self, file: &File, is_directory: bool, path: &Path) -> Result<Id, String> {
        let id = self
            .next
            .checked_add(1)
            .ok_or("Project-source event identity exhausted")?;
        let parent = if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            let name = name.encode_wide().collect::<Vec<_>>();
            let count = self
                .directory(parent, None)?
                .names
                .entry((name.clone(), is_directory))
                .or_default();
            *count = count
                .checked_add(1)
                .ok_or("Notification reference count exhausted")?;
            Some((parent.to_path_buf(), name))
        } else {
            None
        };
        let directory = if is_directory {
            let directory = self.directory(path, Some(file))?;
            directory.owners = directory
                .owners
                .checked_add(1)
                .ok_or("Notification reference count exhausted")?;
            Some(path.to_path_buf())
        } else {
            None
        };
        self.next = id;
        self.registrations
            .insert(id, Registration { parent, directory });
        Ok(id)
    }

    pub fn remove(&mut self, id: Id) -> Result<(), String> {
        let registration = self
            .registrations
            .remove(&id)
            .ok_or("Unknown notification registration")?;
        let is_directory = registration.directory.is_some();
        if let Some((parent, name)) = registration.parent {
            let directory = self
                .directories
                .get_mut(&parent)
                .ok_or("Missing notification directory")?;
            let count = directory
                .names
                .get_mut(&(name.clone(), is_directory))
                .ok_or("Missing notification name")?;
            *count = count
                .checked_sub(1)
                .ok_or("Notification reference count underflow")?;
            if *count == 0 {
                directory.names.remove(&(name, is_directory));
            }
        }
        if let Some(path) = registration.directory {
            let directory = self
                .directories
                .get_mut(&path)
                .ok_or("Missing notification directory")?;
            directory.owners = directory
                .owners
                .checked_sub(1)
                .ok_or("Notification reference count underflow")?;
        }
        self.directories
            .retain(|_, directory| directory.owners != 0 || !directory.names.is_empty());
        Ok(())
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        let mut changed = false;
        let mut comparisons = 65_536_usize;
        let mut calls = 4096_usize;
        for (path, directory) in &mut self.directories {
            // Parent-only directories may change due to unrelated projects.
            // Verify namespace identity without requiring stable timestamps.
            if !identity::same_object(&identity::open(path)?, &directory.read.get().file)? {
                return Err("A notification directory changed identity".into());
            }
            loop {
                calls = calls
                    .checked_sub(1)
                    .ok_or("Directory notification collection exceeds its work budget")?;
                let Some(length) = directory.read.get().completed()? else {
                    break;
                };
                changed |= records(
                    directory.read.get().bytes(length)?,
                    directory.owners != 0,
                    &directory.names,
                    &mut comparisons,
                )?;
                directory.read.get().issue()?;
            }
        }
        Ok(changed)
    }
}

fn records(
    mut bytes: &[u8],
    owner: bool,
    names: &Names,
    comparisons: &mut usize,
) -> Result<bool, String> {
    let mut changed = false;
    loop {
        if bytes.len() < 12 {
            return Err("Truncated directory notification".into());
        }
        let integer = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let next = usize::try_from(integer(0)).unwrap();
        let action = integer(4);
        let length = usize::try_from(integer(8)).unwrap();
        if length == 0 || length % 2 != 0 || length > bytes.len() - 12 {
            return Err("Invalid directory notification name".into());
        }
        let name = bytes[12..12 + length]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|word| u16::from_le_bytes([word[0], word[1]]))
            .collect::<Vec<_>>();
        if name.iter().any(|word| matches!(*word, 0 | 47 | 92)) {
            return Err("Invalid directory notification path".into());
        }
        let namespace = match action {
            FILE_ACTION_ADDED
            | FILE_ACTION_REMOVED
            | FILE_ACTION_RENAMED_OLD_NAME
            | FILE_ACTION_RENAMED_NEW_NAME => true,
            FILE_ACTION_MODIFIED => false,
            _ => return Err("Unknown directory notification action".into()),
        };
        let mut relevant = owner && namespace;
        if !relevant {
            for (expected, is_directory) in names.keys() {
                if *is_directory && !namespace {
                    continue;
                }
                *comparisons = comparisons
                    .checked_sub(1)
                    .ok_or("Directory notification name budget exceeded")?;
                // Both bounded UTF-16 slices stay live for this comparison. A
                // broader case match conservatively invalidates extra results.
                let equal = unsafe {
                    CompareStringOrdinal(
                        name.as_ptr(),
                        i32::try_from(name.len()).unwrap(),
                        expected.as_ptr(),
                        i32::try_from(expected.len()).unwrap(),
                        1,
                    )
                };
                if equal == 0 {
                    return Err("Cannot compare notification names".into());
                }
                if equal == CSTR_EQUAL {
                    relevant = true;
                    break;
                }
            }
        }
        #[cfg(test)]
        if relevant {
            eprintln!(
                "Relevant Windows source event: action={action}, name={:?}, directory_owner={owner}",
                String::from_utf16_lossy(&name)
            );
        }
        changed |= relevant;
        if next == 0 {
            return Ok(changed);
        }
        if next % 4 != 0 || next < 12 + length || next >= bytes.len() {
            return Err("Invalid directory notification offset".into());
        }
        bytes = &bytes[next..];
    }
}

#[cfg(test)]
mod tests;
