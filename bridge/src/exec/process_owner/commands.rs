//! A command's original folder claim reaches the independent owner before spawn.
//! Its exit status is separate from the lifetime of successful background work.
use super::*;
use std::os::unix::process::ExitStatusExt;

pub(crate) struct Command {
    owner: Owner,
    completed: bool,
}
impl Drop for Command {
    fn drop(&mut self) {
        if !self.completed {
            self.owner.stop();
        }
    }
}
impl Command {
    pub(crate) fn spawn(
        spec: &crate::exec::SpawnSpec,
        folder: &crate::folder_coordination::Lease,
    ) -> Result<(Self, tokio::process::Child), String> {
        let descriptors = folder.command_descriptors(&spec.cwd)?;
        let mut may_have_dispatched = false;
        let started = Owner::start(
            Request::StartCommand {
                version: VERSION,
                program: spec.command.clone().into(),
                args: spec.args.clone(),
                root: spec.cwd.clone(),
                environment: spec
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            },
            descriptors,
            Stdio::null(),
            &mut may_have_dispatched,
        );
        let (owner, child, _) = started.map_err(|reason| {
            if may_have_dispatched {
                folder.mark_uncertain();
                format!("{reason}; command dispatch is unconfirmed and its original folder remains blocked")
            } else {
                reason
            }
        })?;
        Ok((
            Self {
                owner,
                completed: false,
            },
            child,
        ))
    }
    pub(crate) async fn status(&self) -> Result<std::process::ExitStatus, String> {
        let stream = self.owner.stream.clone();
        // This read can outlast the control-frame timeout. The command guard
        // shuts down the socket on cancellation, unblocking the owned reader.
        let result = tokio::task::spawn_blocking(move || {
            let mut stream = stream
                .lock()
                .map_err(|_| "Native command control failed".to_string())?;
            stream.set_read_timeout(None).map_err(|e| e.to_string())?;
            let result = wire::receive::<Reply>(&mut stream).map_err(|e| e.to_string());
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .map_err(|e| e.to_string())?;
            match result? {
                (Reply::CommandExited { status }, fds) if fds.is_empty() => {
                    Ok(std::process::ExitStatus::from_raw(status))
                }
                _ => Err("The process owner returned an invalid command outcome".into()),
            }
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(std::convert::identity);
        if result.is_err() {
            self.owner.stop();
        }
        result
    }
    pub(crate) fn stop(&self) {
        self.owner.stop();
    }
    pub(crate) async fn complete(&mut self) -> Result<(), String> {
        if !matches!(
            self.owner
                .exchange(Request::CompleteCommand, vec![])
                .await?,
            Reply::CommandCompleted
        ) {
            return Err("The process owner did not acknowledge command completion".into());
        }
        self.completed = true;
        Ok(())
    }
}

pub(super) fn run(
    mut stream: UnixStream,
    program: PathBuf,
    args: Vec<String>,
    root: PathBuf,
    environment: BTreeMap<String, String>,
    descriptors: Vec<OwnedFd>,
) -> Result<(), String> {
    let mut child = match spawn(program, args, &root, environment, &descriptors) {
        Ok(child) => child,
        Err(reason) => {
            wire::send(&mut stream, &Reply::CommandRejected { reason }, &[])
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
    };
    let pid = i32::try_from(child.id()).expect("Native child IDs fit the signed process ID type");
    // Only the actual command owns its output pipes. The watcher must not hold
    // them open while waiting for the parent's completion acknowledgement.
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null");
    let detached_stdio = null.is_ok_and(|null| {
        (0..=2).all(|fd| {
            // SAFETY: null is live and dup2 replaces only this helper's stdio.
            (unsafe { libc::dup2(null.as_raw_fd(), fd) }) >= 0
        })
    });
    let mut stopped = !detached_stdio
        || wire::send(
            &mut stream,
            &Reply::Started {
                version: VERSION,
                pid: child.id(),
            },
            &[],
        )
        .is_err();
    let mut sent_status = false;
    let mut completed = false;
    while !stopped && !STOP_REQUESTED.load(Ordering::Relaxed) {
        if !sent_status && groups::exited(pid).is_ok_and(|exited| exited) {
            let Ok(status) = status(pid) else {
                break;
            };
            stopped = wire::send(&mut stream, &Reply::CommandExited { status }, &[]).is_err();
            sent_status = true;
        }
        if completed {
            // An acknowledged ordinary exit preserves intentional background
            // work. Keep the original locks and PID until its group is quiet.
            if groups::quiet(pid).is_ok_and(|quiet| quiet) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll borrows one initialized descriptor for at most 50ms.
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
        match wire::receive::<Request>(&mut stream) {
            Ok((Request::CompleteCommand, fds)) if sent_status && fds.is_empty() => {
                completed = wire::send(&mut stream, &Reply::CommandCompleted, &[]).is_ok();
                stopped = !completed;
            }
            _ => stopped = true,
        }
    }
    if !completed || STOP_REQUESTED.load(Ordering::Relaxed) {
        groups::signal(pid, libc::SIGTERM);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !groups::quiet(pid).is_ok_and(|quiet| quiet) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        loop {
            groups::signal(pid, libc::SIGKILL);
            if groups::quiet(pid).is_ok_and(|quiet| quiet) {
                break;
            }
            // Unconfirmed cleanup retains the original folder descriptors.
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let waited = child.wait().map_err(|e| e.to_string());
    // No more signals after reaping the PID. A dead descendant can remain in
    // the kernel briefly; keep exclusion until the original group is gone.
    loop {
        // SAFETY: signal zero observes only; it cannot affect a reused group.
        if unsafe { libc::kill(-pid, 0) } < 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(descriptors);
    waited.map(|_| ())
}

fn spawn(
    program: PathBuf,
    args: Vec<String>,
    root: &Path,
    environment: BTreeMap<String, String>,
    descriptors: &[OwnedFd],
) -> Result<std::process::Child, String> {
    if descriptors.is_empty() {
        return Err("A command requires its original folder claim before dispatch".into());
    }
    use std::os::unix::{fs::MetadataExt, process::CommandExt};
    let mut ancestors = root.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    if ancestors.len() != descriptors.len() {
        return Err("The command folder claim does not cover its complete path".into());
    }
    for (descriptor, path) in descriptors.iter().zip(ancestors) {
        let file = std::fs::File::from(duplicate(descriptor.as_raw_fd())?);
        let actual = file.metadata().map_err(|e| e.to_string())?;
        let current = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if !actual.is_dir() || (actual.dev(), actual.ino()) != (current.dev(), current.ino()) {
            return Err("The original command folder changed before dispatch".into());
        }
    }
    let root_fd = descriptors
        .last()
        .expect("the original folder is present")
        .as_raw_fd();
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .envs(environment)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0);
    // SAFETY: fchdir is async-signal-safe and root_fd is held across spawn. The
    // actual child enters the pinned original directory, never a replacement.
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(root_fd) < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command.spawn().map_err(|e| e.to_string())
}

fn status(pid: i32) -> Result<i32, String> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: WNOWAIT preserves our original child and process-group identity.
    if unsafe {
        libc::waitid(
            libc::P_PID,
            u32::try_from(pid).unwrap(),
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    } < 0
    {
        return Err(io::Error::last_os_error().to_string());
    }
    let info = unsafe { info.assume_init() };
    #[cfg(target_os = "linux")]
    let (code, status) = (info.si_code, unsafe { info.si_status() });
    #[cfg(target_os = "macos")]
    let (code, status) = (info.si_code, info.si_status);
    match code {
        libc::CLD_EXITED => Ok(status << 8),
        libc::CLD_KILLED => Ok(status),
        libc::CLD_DUMPED => Ok(status | 0x80),
        _ => Err("The original command has no complete exit status".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder_coordination::{Coordinator, Folder};

    fn request(root: &Path) -> Request {
        Request::StartCommand {
            version: VERSION,
            program: "sh".into(),
            args: vec!["-c".into(), "printf forbidden > dispatched".into()],
            root: root.into(),
            environment: BTreeMap::new(),
        }
    }
    #[tokio::test]
    async fn commands_cannot_dispatch_without_the_initial_original_folder_descriptors() {
        let root = tempfile::tempdir().unwrap();
        assert!(Owner::start(request(root.path()), vec![], Stdio::null(), &mut false).is_err());
        assert!(!root.path().join("dispatched").exists());
    }
    #[tokio::test]
    async fn descriptors_for_a_different_folder_cannot_start_the_command() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        let lease = coordinator
            .acquire(Folder::capture(root.path()).unwrap())
            .await
            .unwrap();
        let descriptors = lease.command_descriptors(root.path()).unwrap();
        assert!(
            Owner::start(
                request(&other.path().canonicalize().unwrap()),
                descriptors,
                Stdio::null(),
                &mut false
            )
            .is_err()
        );
        assert!(!root.path().join("dispatched").exists());
        assert!(!other.path().join("dispatched").exists());
    }
}
