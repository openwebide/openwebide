//! Flush a captured directory without resolving its ambient pathname again.
use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
};

pub(crate) fn sync(directory: &impl AsRawFd) -> io::Result<()> {
    // Capability directories may hold Linux O_PATH descriptors. Reopen the same
    // directory for reading: fsync cannot operate on an O_PATH descriptor.
    // SAFETY: The borrowed descriptor and static NUL-terminated name are live.
    // Successful openat returns one owned descriptor, consumed once below.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor which File now exclusively owns.
    let directory = unsafe { File::from_raw_fd(descriptor) };
    directory.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flushing_a_captured_directory_survives_path_replacement() {
        let parent = tempfile::Builder::new()
            .prefix("openwebide-directory-durability-")
            .tempdir()
            .unwrap()
            .keep();
        let selected = parent.join("selected");
        let original = parent.join("original");
        std::fs::create_dir(&selected).unwrap();
        let directory =
            cap_std::fs::Dir::open_ambient_dir(&selected, cap_std::ambient_authority()).unwrap();
        std::fs::rename(&selected, &original).unwrap();
        // A regular replacement makes any ambient pathname-based reopen fail.
        std::fs::write(&selected, b"replacement").unwrap();
        sync(&directory).unwrap();
        assert!(sync(&File::open(&selected).unwrap()).is_err());
        assert_eq!(std::fs::read(selected).unwrap(), b"replacement");
        assert_eq!(
            same_file::Handle::from_file(directory.into_std_file()).unwrap(),
            same_file::Handle::from_path(original).unwrap()
        );
        println!("retained_directory_durability_fixture={}", parent.display());
    }
}
