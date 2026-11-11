//! Bounded, private Unix control frames. Passed descriptors stay owned across
//! cancellation and are never selected by an HTTP or language-server payload.
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
};

pub(super) const MAX_FDS: usize = 64;
// Preserve the public server declaration's 64 x 4096-byte arguments, including
// JSON's six-byte escaping of control characters, plus launch paths and env.
const MAX_BODY: usize = 4 * 1024 * 1024;

pub(super) fn send(
    stream: &mut UnixStream,
    body: &impl Serialize,
    descriptors: &[OwnedFd],
) -> io::Result<()> {
    let body = serde_json::to_vec(body).map_err(io::Error::other)?;
    if body.len() > MAX_BODY || descriptors.len() > MAX_FDS {
        return Err(io::Error::other(
            "Native process control frame exceeds its limit",
        ));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(&body);
    let mut control = [0usize; 80];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    let mut vector = libc::iovec {
        iov_base: frame.as_mut_ptr().cast(),
        iov_len: frame.len(),
    };
    message.msg_iov = &raw mut vector;
    message.msg_iovlen = 1;
    if !descriptors.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        let bytes = u32::try_from(descriptors.len() * std::mem::size_of::<libc::c_int>()).unwrap();
        // SAFETY: the aligned control buffer holds a header and all bounded fds.
        #[cfg(target_os = "macos")]
        {
            message.msg_controllen = unsafe { libc::CMSG_SPACE(bytes) };
        }
        #[cfg(target_os = "linux")]
        {
            message.msg_controllen = usize::try_from(unsafe { libc::CMSG_SPACE(bytes) }).unwrap();
        }
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            #[cfg(target_os = "macos")]
            {
                (*header).cmsg_len = libc::CMSG_LEN(bytes);
            }
            #[cfg(target_os = "linux")]
            {
                (*header).cmsg_len = usize::try_from(libc::CMSG_LEN(bytes)).unwrap();
            }
            let data = libc::CMSG_DATA(header).cast::<libc::c_int>();
            for (index, descriptor) in descriptors.iter().enumerate() {
                data.add(index).write_unaligned(descriptor.as_raw_fd());
            }
        }
    }
    // SAFETY: message references live byte/control buffers for this syscall.
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_NOSIGNAL;
    #[cfg(target_os = "macos")]
    let flags = 0;
    let written = loop {
        // SAFETY: the same complete frame and descriptors remain borrowed until
        // sendmsg succeeds. EINTR reports no transferred bytes/rights.
        let result = unsafe { libc::sendmsg(stream.as_raw_fd(), &message, flags) };
        if result >= 0 {
            break result;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    };
    let written = usize::try_from(written).unwrap();
    if written == 0 {
        return Err(io::ErrorKind::WriteZero.into());
    }
    stream.write_all(&frame[written..])
}

pub(super) fn receive<T: DeserializeOwned>(
    stream: &mut UnixStream,
) -> io::Result<(T, Vec<OwnedFd>)> {
    let mut prefix = [0u8; 4];
    let mut offset = 0;
    let mut descriptors = Vec::new();
    while offset < prefix.len() {
        let mut control = [0usize; 80];
        // SAFETY: zero is the documented empty msghdr state; pointers below
        // refer to aligned, writable buffers which outlive recvmsg.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        let mut vector = libc::iovec {
            iov_base: prefix[offset..].as_mut_ptr().cast(),
            iov_len: prefix.len() - offset,
        };
        message.msg_iov = &raw mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        #[cfg(target_os = "macos")]
        {
            message.msg_controllen = u32::try_from(std::mem::size_of_val(&control)).unwrap();
        }
        #[cfg(target_os = "linux")]
        {
            message.msg_controllen = std::mem::size_of_val(&control);
        }
        // SAFETY: the msghdr and all its output buffers are valid for recvmsg.
        let received = unsafe { libc::recvmsg(stream.as_raw_fd(), &raw mut message, 0) };
        if received < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if received == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        // SAFETY: only headers returned in the initialized ancillary buffer are
        // inspected. Each SCM_RIGHTS descriptor is immediately given one owner.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&message);
            while !header.is_null() {
                if (*header).cmsg_level != libc::SOL_SOCKET
                    || (*header).cmsg_type != libc::SCM_RIGHTS
                {
                    return Err(io::Error::other(
                        "Unexpected native control descriptor type",
                    ));
                }
                #[cfg(target_os = "macos")]
                let length = usize::try_from((*header).cmsg_len).unwrap();
                #[cfg(target_os = "linux")]
                let length = (*header).cmsg_len;
                let base = usize::try_from(libc::CMSG_LEN(0)).unwrap();
                let count = length
                    .checked_sub(base)
                    .filter(|bytes| bytes % std::mem::size_of::<libc::c_int>() == 0)
                    .ok_or_else(|| io::Error::other("Invalid native descriptor frame"))?
                    / std::mem::size_of::<libc::c_int>();
                let data = libc::CMSG_DATA(header).cast::<libc::c_int>();
                for index in 0..count {
                    let fd = data.add(index).read_unaligned();
                    if fd < 0 {
                        return Err(io::Error::other("Invalid native descriptor"));
                    }
                    let fd = OwnedFd::from_raw_fd(fd);
                    if libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    descriptors.push(fd);
                }
                header = libc::CMSG_NXTHDR(&message, header);
            }
        }
        if message.msg_flags & libc::MSG_CTRUNC != 0 || descriptors.len() > MAX_FDS {
            return Err(io::Error::other("Truncated native descriptor frame"));
        }
        offset += usize::try_from(received).unwrap();
    }
    let length = usize::try_from(u32::from_be_bytes(prefix)).unwrap();
    if length > MAX_BODY {
        return Err(io::Error::other("Native control body exceeds its limit"));
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    Ok((
        serde_json::from_slice(&body).map_err(io::Error::other)?,
        descriptors,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_complete_public_argument_budget_survives_control_framing() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let arguments = vec!["\u{1}".repeat(4096); 64];
        let expected = arguments.clone();
        let reader = std::thread::spawn(move || {
            let (actual, fds) = receive::<Vec<String>>(&mut receiver).unwrap();
            assert!(fds.is_empty());
            assert_eq!(actual, expected);
        });
        send(&mut sender, &arguments, &[]).unwrap();
        reader.join().unwrap();
    }
    #[test]
    fn malformed_body_drops_received_directory_descriptors() {
        let directory = tempfile::tempdir().unwrap();
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let file = std::fs::File::open(directory.path()).unwrap();
        file.try_lock().unwrap();
        let descriptors = vec![super::super::duplicate(file.as_raw_fd()).unwrap()];
        send(&mut sender, &"unexpected reply type", &descriptors).unwrap();
        drop(file);
        drop(descriptors);
        assert!(receive::<u64>(&mut receiver).is_err());
        std::fs::File::open(directory.path())
            .unwrap()
            .try_lock()
            .unwrap();
    }
    #[test]
    fn a_full_descriptor_set_arrives_with_close_on_exec_on_every_copy() {
        let directory = tempfile::tempdir().unwrap();
        let file = std::fs::File::open(directory.path()).unwrap();
        let descriptors = (0..MAX_FDS)
            .map(|_| super::super::duplicate(file.as_raw_fd()).unwrap())
            .collect::<Vec<_>>();
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        send(&mut sender, &42u32, &descriptors).unwrap();
        let (body, received) = receive::<u32>(&mut receiver).unwrap();
        assert_eq!(body, 42);
        assert_eq!(received.len(), MAX_FDS);
        for fd in received {
            // SAFETY: F_GETFD only inspects this owned live descriptor's flags.
            let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        }
    }
}
