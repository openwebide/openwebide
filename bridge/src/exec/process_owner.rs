//! An independent parent owns an original language-server or command group. It
//! retains passed folder descriptors until that group is confirmed stopped,
//! including after the observing bridge disappears. Detached groups and other
//! workspace writers still need the broader durable authority protocol.
pub(crate) mod commands;
mod groups;
mod wire;

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    net::Shutdown,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const VERSION: u32 = 2;
const MODE: &str = "--internal-process-owner";
static EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn request_stop(_signal: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::Relaxed);
}
fn install_signals() -> Result<(), String> {
    for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
        // SAFETY: sigaction is initialized with a signal-safe, lock-free flag
        // handler. The private watcher has no Tokio runtime or other handlers.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = request_stop as *const () as libc::sighandler_t;
        unsafe {
            libc::sigemptyset(&raw mut action.sa_mask);
        }
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Start {
        version: u32,
        program: PathBuf,
        args: Vec<String>,
        root: PathBuf,
        environment: BTreeMap<String, String>,
    },
    StartCommand {
        version: u32,
        program: PathBuf,
        args: Vec<String>,
        root: PathBuf,
        environment: BTreeMap<String, String>,
    },
    CompleteCommand,
    Retain {
        id: String,
    },
    Release {
        id: String,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Started { version: u32, pid: u32 },
    CommandExited { status: i32 },
    CommandCompleted,
    CommandRejected { reason: String },
    Retained { id: String },
    Released { id: String },
}

/// Call before starting the ordinary daemon. The internal helper uses no Tokio
/// runtime or listener, and is shipped in the same bridge executable.
pub fn run_if_requested() -> Option<Result<(), String>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(MODE)) {
        return None;
    }
    Some((|| {
        let fd = args
            .next()
            .and_then(|v| {
                v.to_str()
                    .and_then(|v| v.parse::<i32>().ok())
                    .filter(|fd| *fd >= 3)
            })
            .ok_or("Invalid process-owner control descriptor")?;
        if args.next().is_some() {
            return Err("Unexpected process-owner arguments".into());
        }
        let mut kind: libc::c_int = 0;
        let mut length = u32::try_from(std::mem::size_of_val(&kind)).unwrap();
        // SAFETY: validate the foreign descriptor before constructing an owned
        // socket. The private entry runs before any threads can close/reuse it.
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut length,
            )
        } < 0
            || kind != libc::SOCK_STREAM
        {
            return Err("Invalid process-owner control socket".into());
        }
        let mut address = std::mem::MaybeUninit::<libc::sockaddr_storage>::zeroed();
        let mut length = u32::try_from(std::mem::size_of::<libc::sockaddr_storage>()).unwrap();
        // SAFETY: getsockname initializes this appropriately sized output.
        if unsafe { libc::getsockname(fd, address.as_mut_ptr().cast(), &raw mut length) } < 0 {
            return Err("Process-owner control socket is unavailable".into());
        }
        if i32::from(unsafe { address.assume_init() }.ss_family) != libc::AF_UNIX {
            return Err("Process-owner control requires a Unix socket".into());
        }
        // SAFETY: this validated live Unix stream is transferred exactly once.
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        run(stream)
    })())
}
pub fn register_executable() -> Result<(), String> {
    let path = std::env::current_exe().map_err(|e| e.to_string())?;
    let _ = EXECUTABLE.set(path);
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Owner {
    stream: Arc<Mutex<UnixStream>>,
    stop: Arc<UnixStream>,
}
pub(crate) struct Claim {
    owner: Owner,
    id: String,
    finished: bool,
}
impl Drop for Claim {
    fn drop(&mut self) {
        if !self.finished {
            self.owner.stop();
        }
    }
}
impl Claim {
    pub async fn finish(mut self) -> Result<(), String> {
        let id = self.id.clone();
        let reply = self
            .owner
            .exchange(Request::Release { id: id.clone() }, vec![])
            .await?;
        if !matches!(reply,Reply::Released{id:received} if received==id) {
            return Err("The process owner returned a different folder release".into());
        }
        self.finished = true;
        Ok(())
    }
}
impl Owner {
    pub fn spawn(
        program: &Path,
        args: &[String],
        root: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<(Self, tokio::process::Child, u32), String> {
        Self::start(
            Request::Start {
                version: VERSION,
                program: program.to_owned(),
                args: args.to_vec(),
                root: root.to_owned(),
                environment: environment.clone(),
            },
            vec![],
            Stdio::piped(),
            &mut false,
        )
    }
    fn start(
        request: Request,
        descriptors: Vec<OwnedFd>,
        stdin: Stdio,
        may_have_dispatched: &mut bool,
    ) -> Result<(Self, tokio::process::Child, u32), String> {
        let mut frame = wire::prepare(&request, &descriptors).map_err(|e| e.to_string())?;
        let (mut stream, child_stream) = UnixStream::pair().map_err(|e| e.to_string())?;
        configure(&stream)?;
        configure(&child_stream)?;
        let child_fd = duplicate(child_stream.as_raw_fd())?;
        let mut command = helper_command(child_fd.as_raw_fd())?;
        command
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.process_group(0);
        let fd = child_fd.as_raw_fd();
        // SAFETY: fcntl is async-signal-safe; this owned duplicate stays alive
        // through spawn, and only the child's descriptor flags are changed.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let child = command
            .spawn()
            .map_err(|e| format!("Cannot start native process owner: {e}"))?;
        drop(child_fd);
        drop(child_stream);
        let stop = stream.try_clone().map_err(|e| e.to_string())?;
        let handshake = (|| {
            *may_have_dispatched = true;
            wire::send_prepared(&mut stream, &mut frame, &descriptors)
                .map_err(|e| e.to_string())?;
            let (reply, fds) = wire::receive::<Reply>(&mut stream).map_err(|e| e.to_string())?;
            match reply {
                Reply::Started {
                    version: VERSION,
                    pid,
                } if pid > 1 && fds.is_empty() => Ok(pid),
                Reply::CommandRejected { reason } if fds.is_empty() => {
                    *may_have_dispatched = false;
                    Err(reason)
                }
                _ => Err("The native process owner has an incompatible protocol".into()),
            }
        })();
        match handshake {
            Ok(pid) => Ok((
                Self {
                    stream: Arc::new(Mutex::new(stream)),
                    stop: Arc::new(stop),
                },
                child,
                pid,
            )),
            Err(error) => {
                let _ = stop.shutdown(Shutdown::Both);
                Err(error)
            }
        }
    }
    pub fn stop(&self) {
        let _ = self.stop.shutdown(Shutdown::Both);
    }
    pub async fn retain(&self, descriptors: Vec<OwnedFd>) -> Result<Claim, String> {
        // Own cleanup before the first await: cancellation may happen after
        // descriptors were delivered but before the watcher acknowledges them.
        let claim = Claim {
            owner: self.clone(),
            id: format!("{:032x}", rand::random::<u128>()),
            finished: false,
        };
        let reply = self
            .exchange(
                Request::Retain {
                    id: claim.id.clone(),
                },
                descriptors,
            )
            .await?;
        if !matches!(reply,Reply::Retained{id:received} if received==claim.id) {
            return Err("The process owner returned a different folder claim".into());
        }
        Ok(claim)
    }
    async fn exchange(&self, request: Request, descriptors: Vec<OwnedFd>) -> Result<Reply, String> {
        let stream = self.stream.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut stream = stream
                .lock()
                .map_err(|_| "Native process control failed".to_string())?;
            wire::send(&mut stream, &request, &descriptors).map_err(|e| e.to_string())?;
            let (reply, fds) = wire::receive(&mut stream).map_err(|e| e.to_string())?;
            if !fds.is_empty() {
                return Err("Unexpected process-owner descriptors".into());
            }
            Ok(reply)
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(std::convert::identity);
        if result.is_err() {
            self.stop();
        }
        result
    }
}

pub(crate) fn duplicate(fd: i32) -> Result<OwnedFd, String> {
    // SAFETY: duplicate a borrowed live fd into a fresh close-on-exec descriptor
    // above stdio. The returned owner is the sole owner of that new descriptor.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if copy < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}
fn configure(stream: &UnixStream) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    {
        let enabled: libc::c_int = 1;
        // SAFETY: setsockopt borrows this valid integer for the syscall only.
        if unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&raw const enabled).cast(),
                u32::try_from(std::mem::size_of_val(&enabled)).unwrap(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error().to_string());
        }
    }
    Ok(())
}
fn helper_command(fd: i32) -> Result<tokio::process::Command, String> {
    let executable = if let Some(path) = EXECUTABLE.get() {
        path.clone()
    } else {
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        let parent = current
            .parent()
            .ok_or("Native executable directory is unavailable")?;
        [
            parent.join("openwebide-bridge"),
            parent.parent().unwrap_or(parent).join("openwebide-bridge"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or("Build or install the matching bridge executable beside this native host")?
    };
    let mut command = tokio::process::Command::new(executable);
    command.arg(MODE).arg(fd.to_string());
    Ok(command)
}

fn run(mut stream: UnixStream) -> Result<(), String> {
    install_signals()?;
    configure(&stream)?;
    // Do not let the actual server inherit its owner's private control socket.
    // SAFETY: only descriptor flags on this exclusively owned socket change.
    if unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let (request, fds) = wire::receive::<Request>(&mut stream).map_err(|e| e.to_string())?;
    let request = match request {
        Request::StartCommand {
            version: VERSION,
            program,
            args,
            root,
            environment,
        } => {
            return commands::run(stream, program, args, root, environment, fds);
        }
        request => request,
    };
    let Request::Start {
        version: VERSION,
        program,
        args,
        root,
        environment,
    } = request
    else {
        return Err("Invalid process-owner start".into());
    };
    if !fds.is_empty() {
        return Err("Unexpected initial process-owner descriptors".into());
    }
    use std::os::unix::process::CommandExt;
    let mut child = std::process::Command::new(program)
        .args(args)
        .envs(environment)
        .current_dir(root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let pid = i32::try_from(child.id()).map_err(|e| e.to_string())?;
    let mut stopping = wire::send(
        &mut stream,
        &Reply::Started {
            version: VERSION,
            pid: child.id(),
        },
        &[],
    )
    .is_err();
    let mut retained: Option<(String, Vec<OwnedFd>)> = None;
    while !stopping && !STOP_REQUESTED.load(Ordering::Relaxed) {
        match groups::exited(pid) {
            Ok(false) => {}
            _ => {
                break;
            }
        }
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll borrows this one initialized pollfd for at most 50ms.
        let ready = unsafe { libc::poll(&raw mut poll, 1, 50) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if ready == 0 {
            continue;
        }
        let result = (|| {
            let (request, fds) =
                wire::receive::<Request>(&mut stream).map_err(|e| e.to_string())?;
            let reply = match request {
                Request::Retain { id }
                    if retained.is_none() && valid_id(&id) && !fds.is_empty() =>
                {
                    for fd in &fds {
                        // SAFETY: a temporary duplicate provides metadata without
                        // taking ownership of a descriptor already in the claim.
                        let file = std::fs::File::from(duplicate(fd.as_raw_fd())?);
                        if !file.metadata().map_err(|e| e.to_string())?.is_dir() {
                            return Err("A folder claim requires directory descriptors".into());
                        }
                    }
                    retained = Some((id.clone(), fds));
                    Reply::Retained { id }
                }
                Request::Release { id }
                    if fds.is_empty() && retained.as_ref().is_some_and(|(held, _)| held == &id) =>
                {
                    drop(retained.take());
                    Reply::Released { id }
                }
                _ => return Err("Invalid process-owner transition".into()),
            };
            wire::send(&mut stream, &reply, &[]).map_err(|e| e.to_string())
        })();
        stopping = result.is_err();
    }
    // The child is deliberately not reaped yet: its PID reserves the original
    // group ID, so cleanup cannot signal a newly reused group. SIGKILL precedes
    // the quiescence scan, preventing remaining managed members from forking.
    groups::signal(pid, libc::SIGTERM);
    std::thread::sleep(Duration::from_millis(100));
    loop {
        groups::signal(pid, libc::SIGKILL);
        if groups::quiet(pid).is_ok_and(|quiet| quiet) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(retained);
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}
fn valid_id(id: &str) -> bool {
    openwebide_core::workspace_transaction::valid_id(id)
}

#[cfg(test)]
mod tests;
