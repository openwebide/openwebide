//! macOS persistent volume identity, obtained from the retained file descriptor.
use std::{fs::File, io, os::fd::AsRawFd};

pub(super) fn uuid(file: &File) -> io::Result<[u8; 16]> {
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    #[repr(C)]
    struct Volume {
        length: u32,
        uuid: [u8; 16],
    }
    let mut volume = Volume {
        length: 0,
        uuid: [0; 16],
    };
    // SAFETY: Both structures have the layouts required by fgetattrlist; only
    // ATTR_VOL_UUID is requested and the complete initialized buffer stays live.
    let result = unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            (&raw mut attributes).cast(),
            (&raw mut volume).cast(),
            std::mem::size_of::<Volume>(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if volume.length as usize != std::mem::size_of::<Volume>() || volume.uuid == [0; 16] {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Persistent volume identity is unavailable",
        ));
    }
    Ok(volume.uuid)
}
