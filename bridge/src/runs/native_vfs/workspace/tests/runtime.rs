//! A real saturated FIFO keeps the filesystem's blocking write alive after
//! runtime shutdown. Drain before asserting so an expected red does not leak it.
use super::*;
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    task::Poll,
    time::Instant,
};

const BYTES: usize = 4 * 1024 * 1024;
const VALUE: u8 = 0x5a;

fn available(reader: &File) -> io::Result<usize> {
    let mut count: libc::c_int = 0;
    // SAFETY: reader owns a live descriptor and FIONREAD writes one c_int.
    if unsafe { libc::ioctl(reader.as_raw_fd(), libc::FIONREAD, &mut count) } == -1 {
        return Err(io::Error::last_os_error());
    }
    usize::try_from(count).map_err(io::Error::other)
}
fn writer_open(reader: &File) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: the pointer names one initialized pollfd whose descriptor is live.
    if unsafe { libc::poll(&raw mut descriptor, 1, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(descriptor.revents & libc::POLLHUP == 0)
}

struct Drain(Option<File>);
impl Drain {
    fn finish(&mut self) -> io::Result<usize> {
        let Some(mut reader) = self.0.take() else {
            return Ok(0);
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0;
        while total < BYTES {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    return Err(io::Error::other(
                        "FIFO writer ended before the complete payload",
                    ));
                }
                Ok(count) => {
                    if buffer[..count].iter().any(|byte| *byte != VALUE) {
                        return Err(io::Error::other("FIFO payload changed"));
                    }
                    total += count;
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(total)
    }
}
impl Drop for Drain {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[test]
fn blocked_native_file_io_keeps_its_folder_gate_after_runtime_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let fifo = directory.path().join("pending");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is NUL-terminated and remains live for this call. The path
    // is a fresh entry inside this test's owned temporary directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let reader = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    let mut drain = Drain(Some(reader));
    let coordinator = Coordinator::default();
    let files = WorkspaceFiles::new(directory.path().into(), coordinator.clone());
    let first = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (observer, dispatched) = first.block_on(async {
        let observer =
            tokio::spawn(async move { files.write_bytes("pending", &vec![VALUE; BYTES]).await });
        let dispatched = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let count = available(drain.0.as_ref().unwrap())?;
                if count > 0 {
                    return Ok::<_, io::Error>(count);
                }
                if observer.is_finished() {
                    return Err(io::Error::other(
                        "Native FIFO write finished before dispatch",
                    ));
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        (observer, dispatched)
    });
    let was_pending = !observer.is_finished();
    first.shutdown_timeout(Duration::ZERO);
    let still_open = writer_open(drain.0.as_ref().unwrap());
    let still_buffered = available(drain.0.as_ref().unwrap());
    let second = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (early, drained, later) = second.block_on(async {
        let mut competing =
            Box::pin(coordinator.acquire(Folder::capture(directory.path()).unwrap()));
        let early = match futures::poll!(&mut competing) {
            Poll::Ready(result) => Some(result),
            Poll::Pending => None,
        };
        // Complete actual I/O before any assertion about the expected bug.
        let drained = drain.finish();
        let later = if early.is_none() {
            Some(tokio::time::timeout(Duration::from_secs(2), competing).await)
        } else {
            None
        };
        (early, drained, later)
    });
    println!(
        "FIFO runtime probe: payload={BYTES}, dispatched={dispatched:?}, pending_before_shutdown={was_pending}, writer_open_after_shutdown={still_open:?}, buffered_after_shutdown={still_buffered:?}, drained={drained:?}, premature_grant={}",
        early.as_ref().is_some_and(Result::is_ok)
    );
    assert_eq!(drained.unwrap(), BYTES);
    let count = dispatched.unwrap().unwrap();
    assert!(count > 0 && count < BYTES && was_pending);
    assert!(still_open.unwrap());
    assert!(still_buffered.unwrap() > 0);
    if let Some(Err(error)) = &early {
        panic!("Unexpected native folder error after runtime shutdown: {error}");
    }
    assert!(
        early.is_none(),
        "Native folder gate released while blocked file I/O survived runtime shutdown"
    );
    drop(later.unwrap().unwrap().unwrap());
    drop(observer);
}
