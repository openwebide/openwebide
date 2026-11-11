//! Explicit vnode registrations; no path discovery or background threads.
use std::{
    fs::{File, OpenOptions},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
};

pub(super) type Id = i32;
pub(super) struct Kernel(OwnedFd);

pub(super) fn open(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_EVTONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| error.to_string())
}

impl Kernel {
    pub fn new(_available: std::sync::Arc<tokio::sync::Semaphore>) -> Result<Self, String> {
        // A successful syscall transfers one new descriptor to this owner.
        let descriptor = unsafe { libc::kqueue() };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // The descriptor is unique, valid and not otherwise owned.
        let kernel = unsafe { OwnedFd::from_raw_fd(descriptor) };
        // This live owned descriptor must not survive process exec.
        if unsafe { libc::fcntl(kernel.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self(kernel))
    }

    pub fn register(&mut self, file: &File, _directory: bool, _path: &Path) -> Result<Id, String> {
        let id = file.as_raw_fd();
        self.update(
            id,
            libc::EV_ADD | libc::EV_CLEAR,
            libc::NOTE_WRITE
                | libc::NOTE_EXTEND
                | libc::NOTE_ATTRIB
                | libc::NOTE_LINK
                | libc::NOTE_RENAME
                | libc::NOTE_DELETE
                | libc::NOTE_REVOKE,
        )?;
        Ok(id)
    }

    pub fn remove(&mut self, id: Id) -> Result<(), String> {
        self.update(id, libc::EV_DELETE, 0)
    }

    fn update(&self, id: Id, flags: u16, fflags: u32) -> Result<(), String> {
        let change = libc::kevent {
            ident: usize::try_from(id).map_err(|_| "Invalid project-source event descriptor")?,
            filter: libc::EVFILT_VNODE,
            flags,
            fflags,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Stack input stays live for the synchronous update. No userspace
        // pointer is retained; no output buffer is supplied.
        if unsafe {
            libc::kevent(
                self.0.as_raw_fd(),
                &raw const change,
                1,
                std::ptr::null_mut(),
                0,
                &raw const zero,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        const BATCH: usize = 64;
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let mut events: [MaybeUninit<libc::kevent>; BATCH] =
            std::array::from_fn(|_| MaybeUninit::uninit());
        let mut changed = false;
        for _ in 0..32 {
            // The zero timeout never waits. Only the returned number of
            // output entries are initialized by this synchronous syscall.
            let count = unsafe {
                libc::kevent(
                    self.0.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr().cast(),
                    i32::try_from(BATCH).unwrap(),
                    &raw const zero,
                )
            };
            if count < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let count = usize::try_from(count).unwrap();
            if count == 0 {
                return Ok(changed);
            }
            if count > events.len() {
                return Err("Invalid project-source event count".into());
            }
            for event in &events[..count] {
                // The checked syscall count guarantees initialization.
                let event = unsafe { event.assume_init_ref() };
                if event.flags & libc::EV_ERROR != 0
                    || event.filter != libc::EVFILT_VNODE
                    || event.fflags & libc::NOTE_REVOKE != 0
                    || event.fflags == 0
                {
                    return Err("Project-source event coverage requires a new observation".into());
                }
                changed = true;
            }
        }
        Err("Project-source event collection exceeds its work budget".into())
    }
}
