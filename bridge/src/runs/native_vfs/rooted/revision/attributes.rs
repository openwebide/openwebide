//! Bounded attribute I/O on the same captured descriptor as native metadata.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    ffi::CStr,
    sync::{Arc, LazyLock},
};
use tokio::sync::Semaphore;

const MAX_NAMES_BYTES: usize = 64 * 1024;
const MAX_NAMES: usize = 256;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_ACL_BYTES: usize = 64 * 1024;
const WORKING_BYTES: usize = MAX_BYTES + 2 * MAX_NAMES_BYTES + MAX_ACL_BYTES;
static CAPACITY: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(16 * 1024 * 1024)));

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Native attributes exceed their bounded budget",
    )
}
fn unsupported(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| code == libc::ENOTSUP || code == libc::ENOTTY)
}
fn value(hash: &mut Sha256, bytes: &[u8]) {
    hash.update(u64::try_from(bytes.len()).unwrap().to_le_bytes());
    hash.update(bytes);
}

pub(super) fn digest(file: &File, metadata: &fs::Stat) -> io::Result<Option<String>> {
    let Ok(_working) = CAPACITY
        .clone()
        .try_acquire_many_owned(WORKING_BYTES.try_into().unwrap())
    else {
        return Ok(None);
    };
    let read = || -> io::Result<String> {
        let mut hash = Sha256::new();
        value(&mut hash, b"native-attributes-1");
        #[cfg(target_os = "linux")]
        let original_flags = flags(file)?;
        extended(file, &mut hash)?;
        #[cfg(target_os = "macos")]
        {
            value(&mut hash, b"flags");
            value(&mut hash, &metadata.st_flags.to_le_bytes());
            acl(file, &mut hash)?;
            if fs::fstat(file)?.st_flags != metadata.st_flags {
                return Err(io::Error::other(
                    "Native flags changed while reading attributes",
                ));
            }
        }
        #[cfg(target_os = "linux")]
        {
            let _ = metadata;
            let current_flags = flags(file)?;
            if current_flags != original_flags {
                return Err(io::Error::other(
                    "Native flags changed while reading attributes",
                ));
            }
            value(&mut hash, b"flags");
            match current_flags {
                Some(flags) => value(&mut hash, &flags.to_le_bytes()),
                None => value(&mut hash, b"unsupported"),
            }
        }
        Ok(openwebide_core::plugins::content_digest(&hash.finalize()))
    };
    match read() {
        Ok(digest) => Ok(Some(digest)),
        // A partial observation remains useful to ordinary text analysis, but
        // cannot authorize resource work. The native review constructor rejects
        // original objects without a complete attribute observation.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn extended(file: &File, hash: &mut Sha256) -> io::Result<()> {
    value(hash, b"extended");
    let length = match fs::flistxattr(file, &mut [0_u8; 0][..]).map_err(io::Error::from) {
        Ok(length) => length,
        Err(error) if unsupported(&error) => {
            value(hash, b"unsupported");
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if length > MAX_NAMES_BYTES {
        return Err(unavailable());
    }
    let mut list = vec![0; length];
    let actual = fs::flistxattr(file, &mut list[..])?;
    if actual > list.len() {
        return Err(io::Error::other(
            "Native attribute names changed while reading",
        ));
    }
    list.truncate(actual);
    let mut names = Vec::new();
    let mut remaining = list.as_slice();
    while !remaining.is_empty() {
        if names.len() == MAX_NAMES {
            return Err(unavailable());
        }
        let name = CStr::from_bytes_until_nul(remaining).map_err(io::Error::other)?;
        if name.is_empty() {
            return Err(io::Error::other("Invalid native attribute name"));
        }
        remaining = &remaining[name.to_bytes_with_nul().len()..];
        names.push(name);
    }
    names.sort_unstable();
    let mut total = list.len();
    for name in names {
        let length = fs::fgetxattr(file, name, &mut [0_u8; 0][..])?;
        total = total
            .checked_add(length)
            .filter(|bytes| *bytes <= MAX_BYTES)
            .ok_or_else(unavailable)?;
        let mut bytes = vec![0; length];
        let actual = fs::fgetxattr(file, name, &mut bytes[..])?;
        if actual > bytes.len() {
            return Err(io::Error::other("A native attribute changed while reading"));
        }
        value(hash, name.to_bytes());
        value(hash, &bytes[..actual]);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn flags(file: &File) -> io::Result<Option<u32>> {
    match fs::ioctl_getflags(file).map_err(io::Error::from) {
        Ok(flags) => {
            // Internal storage layout may change with ordinary namespace IO.
            // Keep every other bit, including flags newer than rustix's named
            // set (for example case folding and encryption).
            // https://github.com/torvalds/linux/blob/master/include/uapi/linux/fs.h
            const STORAGE_FLAGS: u32 = 0x0000_0100 // DIRTY
                | 0x0000_0200 // COMPRBLK
                | 0x0000_1000 // INDEX
                | 0x0004_0000 // HUGE_FILE
                | 0x0008_0000 // EXTENT
                | 0x0020_0000 // EA_INODE
                | 0x0040_0000 // EOFBLOCKS
                | 0x1000_0000; // INLINE_DATA
            Ok(Some(flags.bits() & !STORAGE_FLAGS))
        }
        Err(error) if unsupported(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn acl(file: &File, hash: &mut Sha256) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // These descriptor-based functions are declared in the native sys/acl.h.
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_size(acl: *mut libc::c_void) -> libc::ssize_t;
        fn acl_copy_ext(
            buffer: *mut libc::c_void,
            acl: *mut libc::c_void,
            size: libc::ssize_t,
        ) -> libc::ssize_t;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    struct Owned(*mut libc::c_void);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the owned allocation came from acl_get_fd_np and is freed once.
            unsafe {
                acl_free(self.0);
            }
        }
    }
    value(hash, b"acl");
    // SAFETY: file owns a live descriptor; 0x100 is ACL_TYPE_EXTENDED.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        // Darwin FILESEC_ACL reports ENOENT for an absent ACL. This call
        // uses a still-owned descriptor, not a replaceable pathname.
        // https://github.com/apple-oss-distributions/Libc/blob/main/gen/filesec.c
        if error.raw_os_error() == Some(libc::ENOENT) {
            fs::fstat(file)?;
            value(hash, b"absent");
            return Ok(());
        }
        if unsupported(&error) {
            value(hash, b"unsupported");
            return Ok(());
        }
        return Err(error);
    }
    let acl = Owned(acl);
    // SAFETY: acl remains allocated for both size and external-copy calls.
    let length = unsafe { acl_size(acl.0) };
    if length < 0 {
        return Err(io::Error::last_os_error());
    }
    let length = usize::try_from(length).map_err(io::Error::other)?;
    if length > MAX_ACL_BYTES {
        return Err(unavailable());
    }
    let mut bytes = vec![0; length];
    // SAFETY: bytes is writable for the checked size and acl stays live.
    let actual =
        unsafe { acl_copy_ext(bytes.as_mut_ptr().cast(), acl.0, length.try_into().unwrap()) };
    if actual < 0 {
        return Err(io::Error::last_os_error());
    }
    let actual = usize::try_from(actual).map_err(io::Error::other)?;
    if actual > bytes.len() {
        return Err(io::Error::other("Invalid native ACL length"));
    }
    value(hash, &bytes[..actual]);
    Ok(())
}
