//! A native object move, never copy/remove or a check-then-overwrite rename.
//! Parent handles are opened without following links and retained through the
//! syscall. A failed acknowledgement can mean the move committed; the shared
//! journal must reconcile it before attempting any subsequent primitive.
use super::VfsError;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::io_error;
use openwebide_core::vfs::VfsEntryKind;
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn transfer(
    root: &Path,
    from: &str,
    to: &str,
    kind: VfsEntryKind,
) -> Result<(), VfsError> {
    let root = std::fs::File::open(root).map_err(|error| io_error(error, "workspace root"))?;
    transfer_between(&root, from, &root, to, kind)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn transfer_between(
    source_root: &std::fs::File,
    from: &str,
    destination_root: &std::fs::File,
    to: &str,
    kind: VfsEntryKind,
) -> Result<(), VfsError> {
    let source = endpoint(source_root, from)?;
    let destination = endpoint(destination_root, to)?;
    publish(source, destination, from, to, kind)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn transfer_names(
    source_root: &std::fs::File,
    from: &str,
    destination_root: &std::fs::File,
    to: &str,
    kind: VfsEntryKind,
) -> Result<(), VfsError> {
    fn leaf(parent: &std::fs::File, name: &str) -> Result<Endpoint, VfsError> {
        if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\']) {
            return Err(VfsError::PathEscape(name.into()));
        }
        Ok(Endpoint {
            parent: parent.try_clone().map_err(|error| io_error(error, name))?,
            name: std::ffi::CString::new(name).map_err(|error| VfsError::Io(error.to_string()))?,
        })
    }
    publish(
        leaf(source_root, from)?,
        leaf(destination_root, to)?,
        from,
        to,
        kind,
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn publish(
    source: Endpoint,
    destination: Endpoint,
    from: &str,
    to: &str,
    kind: VfsEntryKind,
) -> Result<(), VfsError> {
    use std::os::fd::AsRawFd;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: Both the live directory descriptor and NUL-terminated leaf name
    // remain valid, and fstatat initializes the entire stat on success.
    let status = unsafe {
        libc::fstatat(
            source.parent.as_raw_fd(),
            source.name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(io_error(std::io::Error::last_os_error(), from));
    }
    // SAFETY: fstatat succeeded and initialized metadata above.
    let metadata = unsafe { metadata.assume_init() };
    let expected = if kind.is_dir() {
        libc::S_IFDIR
    } else {
        libc::S_IFREG
    };
    if metadata.st_mode & libc::S_IFMT != expected {
        return Err(VfsError::Io("Workspace entry kind changed".into()));
    }
    // Darwin may return success for a same-object no-op even with RENAME_EXCL.
    // Reject an existing destination up front, while still retaining the native
    // exclusive flag to reject a destination created after this observation.
    if exists(&destination, to)? {
        return Err(VfsError::AlreadyExists(to.into()));
    }
    // SAFETY: The live parent descriptors and NUL-terminated names remain valid
    // throughout this call. The kernel tests destination absence atomically.
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::renameat2(
            source.parent.as_raw_fd(),
            source.name.as_ptr(),
            destination.parent.as_raw_fd(),
            destination.name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    // SAFETY: Same descriptor/name lifetimes and exclusive destination contract
    // as above; RENAME_EXCL is the native Darwin no-replacement flag.
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            source.parent.as_raw_fd(),
            source.name.as_ptr(),
            destination.parent.as_raw_fd(),
            destination.name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if status != 0 {
        return Err(io_error(std::io::Error::last_os_error(), to));
    }
    // A hard link to the source could also arrive after the absence check.
    // Never acknowledge a kernel no-op, or a source concurrently recreated by
    // another writer, as a confirmed move. The journal reconciles this error.
    if exists(&source, from)? {
        return Err(VfsError::Io(
            "Object move could not confirm source removal".into(),
        ));
    }
    source
        .parent
        .sync_all()
        .map_err(|error| io_error(error, from))?;
    destination
        .parent
        .sync_all()
        .map_err(|error| io_error(error, to))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Endpoint {
    parent: std::fs::File,
    name: std::ffi::CString,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exists(endpoint: &Endpoint, path: &str) -> Result<bool, VfsError> {
    use std::os::fd::AsRawFd;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The descriptor and NUL-terminated name remain valid; fstatat can
    // write a full stat to the supplied storage, whose value we do not inspect.
    let status = unsafe {
        libc::fstatat(
            endpoint.parent.as_raw_fd(),
            endpoint.name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(io_error(error, path))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn endpoint(root: &std::fs::File, path: &str) -> Result<Endpoint, VfsError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let normalized = openwebide_core::vfs::workspace_path(path)?;
    if normalized.is_empty() {
        return Err(VfsError::PathEscape(path.into()));
    }
    let mut components = normalized.split('/').collect::<Vec<_>>();
    let name = std::ffi::CString::new(components.pop().unwrap())
        .map_err(|error| VfsError::Io(error.to_string()))?;
    let mut parent = root.try_clone().map_err(|error| io_error(error, path))?;
    for component in components {
        let component =
            std::ffi::CString::new(component).map_err(|error| VfsError::Io(error.to_string()))?;
        // SAFETY: The parent descriptor is live and component is a valid C
        // string. Successful ownership of the returned descriptor is transferred
        // once to File below; failed openat calls do not create a descriptor.
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(io_error(std::io::Error::last_os_error(), path));
        }
        // SAFETY: openat returned a new owned descriptor, consumed exactly once.
        parent = unsafe { std::fs::File::from_raw_fd(descriptor) };
    }
    Ok(Endpoint { parent, name })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn transfer(
    _root: &Path,
    _from: &str,
    _to: &str,
    _kind: VfsEntryKind,
) -> Result<(), VfsError> {
    Err(VfsError::Io(
        "Atomic object moves are unavailable on this platform".into(),
    ))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
