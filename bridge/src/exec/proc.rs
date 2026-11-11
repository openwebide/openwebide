//! Process-group signal delivery and forceful termination.
//!
//! Every spawned command runs as its own process group leader (`process_group(0)`, or a PTY
//! session leader via `setsid`), so its pgid equals its pid. Signalling the negated pgid reaches
//! the whole group, including grandchildren that outlive the direct child (e.g. backgrounded
//! jobs), which a plain `child.kill()` never does.

#[cfg(any(unix, windows))]
use std::time::Duration;

/// A signal requested by a client `Kill` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Int,
    Term,
    Kill,
    Hup,
}

/// Parse a client-supplied signal name. `None` (no `signal` field) defaults to `Kill`; an
/// unrecognized name is an error the caller should report back to the client.
pub fn parse_signal(name: Option<&str>) -> Result<Signal, String> {
    let Some(name) = name else {
        return Ok(Signal::Kill);
    };
    match name.to_ascii_uppercase().as_str() {
        "SIGINT" | "INT" => Ok(Signal::Int),
        "SIGTERM" | "TERM" => Ok(Signal::Term),
        "SIGKILL" | "KILL" => Ok(Signal::Kill),
        "SIGHUP" | "HUP" => Ok(Signal::Hup),
        _ => Err("unsupported signal".to_string()),
    }
}

/// Map a raw signal number to its conventional name, e.g. for reporting how a process exited.
pub fn signal_name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".to_string(),
        2 => "SIGINT".to_string(),
        3 => "SIGQUIT".to_string(),
        9 => "SIGKILL".to_string(),
        15 => "SIGTERM".to_string(),
        other => format!("SIG{other}"),
    }
}

#[cfg(unix)]
fn to_libc_signal(s: Signal) -> libc::c_int {
    match s {
        Signal::Int => libc::SIGINT,
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
        Signal::Hup => libc::SIGHUP,
    }
}

/// Send `s` to every process in group `pgid`.
#[cfg(unix)]
pub fn signal_group(pgid: i32, s: Signal) {
    // SAFETY: killpg with a valid pgid and signal number is always sound; a stale/exited pgid
    // just yields ESRCH, which we don't need to distinguish here.
    unsafe {
        libc::killpg(pgid, to_libc_signal(s));
    }
}

/// Whether process group `pgid` still has any live member, probed with the null signal.
#[cfg(unix)]
fn group_alive(pgid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; it only probes whether the target exists and is
    // signalable, which is always a sound call to make.
    let ret = unsafe { libc::kill(-pgid, 0) };
    if ret == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Terminate process group `pgid`: send SIGTERM, poll for exit until `grace` elapses, then
/// SIGKILL. Returns once the group is confirmed gone or the SIGKILL has been sent.
#[cfg(unix)]
pub async fn terminate_group(pgid: i32, grace: Duration) {
    terminate_group_with(pgid, grace, Signal::Term).await;
}

#[cfg(unix)]
pub async fn terminate_group_with(pgid: i32, grace: Duration, signal: Signal) {
    let _ = terminate_group_result(pgid, grace, signal).await;
}

#[cfg(unix)]
async fn terminate_group_result(pgid: i32, grace: Duration, signal: Signal) -> Result<(), String> {
    // An exiting, not-yet-reaped group can temporarily reject signals (macOS
    // reports EPERM). Its concurrent child reaper can still prove it gone.
    let _ = deliver(pgid, signal);

    let deadline = tokio::time::Instant::now() + grace;
    loop {
        if !group_alive(pgid) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    deliver(pgid, Signal::Kill)
}

#[cfg(unix)]
fn deliver(pgid: i32, signal: Signal) -> Result<(), String> {
    // SAFETY: The process-group ID belongs to a child launched by this runtime.
    if unsafe { libc::killpg(pgid, to_libc_signal(signal)) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error.to_string())
    }
}

/// Lightweight completion information; retaining this never retains a child,
/// document buffers or a service session.
#[derive(Clone)]
pub(crate) struct Cleanup(tokio::sync::watch::Receiver<Option<Result<(), String>>>);
pub(crate) fn cleanup_channel() -> (
    tokio::sync::watch::Sender<Option<Result<(), String>>>,
    Cleanup,
) {
    let (sender, receiver) = tokio::sync::watch::channel(None);
    (sender, Cleanup(receiver))
}
impl Cleanup {
    pub(crate) fn same_process(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }

    pub async fn wait(mut self) -> Result<(), String> {
        loop {
            if let Some(result) = self.0.borrow().clone() {
                return result;
            }
            self.0
                .changed()
                .await
                .map_err(|_| "Native process cleanup ended without an outcome".to_string())?;
        }
    }
}

/// Kills process group `pgid` on drop: SIGTERM immediately, then a SIGKILL follow-up after a
/// grace period. Used to clean up a command's process group when the future driving it is
/// dropped mid-flight (e.g. a disconnected HTTP client) rather than run to completion, where an
/// `.await`-based `terminate_group` call would never get a chance to run.
#[cfg(unix)]
pub struct GroupGuard(i32);

#[cfg(unix)]
impl GroupGuard {
    pub fn new(pgid: i32) -> Self {
        Self(pgid)
    }

    /// Defuse the guard: its `Drop` performs no cleanup. For use once the future it was guarding
    /// has reached a controlled exit (the command finished, or was explicitly terminated already)
    /// instead of being dropped mid-flight, so a successful command's intentionally
    /// backgrounded/detached jobs aren't killed along with it.
    pub fn disarm(self) {
        std::mem::forget(self);
    }
    pub(crate) fn terminate(self) -> Cleanup {
        let pgid = self.0;
        self.disarm();
        signal_group(pgid, Signal::Term);
        schedule_cleanup_ticket(&tokio::runtime::Handle::current(), pgid)
    }
}

#[cfg(unix)]
impl Drop for GroupGuard {
    fn drop(&mut self) {
        let pgid = self.0;
        signal_group(pgid, Signal::Term);
        schedule_cleanup(&tokio::runtime::Handle::current(), pgid);
    }
}

#[cfg(unix)]
pub fn schedule_cleanup(runtime: &tokio::runtime::Handle, pgid: i32) {
    schedule_cleanup_ticket(runtime, pgid);
}
#[cfg(unix)]
fn schedule_cleanup_ticket(runtime: &tokio::runtime::Handle, pgid: i32) -> Cleanup {
    let (done, completion) = cleanup_channel();
    let cleanup = runtime.spawn(async move {
        let signalled = terminate_group_result(pgid, Duration::from_secs(2), Signal::Term).await;
        // Signal delivery is not process completion. Do not release a workspace
        // lease until the kernel confirms that this process group is gone.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let result = loop {
            if !group_alive(pgid) {
                break Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                break Err(signalled.err().unwrap_or_else(|| {
                    "Native process group did not exit after termination".into()
                }));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        done.send_replace(Some(result));
    });
    track_cleanup(cleanup);
    completion
}
pub(crate) fn track_cleanup(cleanup: tokio::task::JoinHandle<()>) {
    let mut tasks = cleanup_tasks().lock().unwrap();
    tasks.retain(|task| !task.is_finished());
    tasks.push(cleanup);
}

fn cleanup_tasks() -> &'static std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>> {
    static TASKS: std::sync::OnceLock<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        std::sync::OnceLock::new();
    TASKS.get_or_init(Default::default)
}
pub async fn wait_for_cleanup() {
    let tasks = std::mem::take(&mut *cleanup_tasks().lock().unwrap());
    futures::future::join_all(tasks).await;
}

#[cfg(windows)]
pub(crate) async fn terminate_job(job: std::os::windows::io::OwnedHandle) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::{
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
        QueryInformationJobObject, TerminateJobObject,
    };
    // Keep the job open while observing its members; closing its kill-on-close
    // handle alone would lose the ability to confirm descendant termination.
    // SAFETY: job owns the live Windows job handle throughout this function.
    let terminated = unsafe { TerminateJobObject(job.as_raw_handle(), 1) };
    let error = (terminated == 0).then(std::io::Error::last_os_error);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let length = u32::try_from(std::mem::size_of_val(&info)).unwrap();
        // SAFETY: info is writable and its size matches the requested class;
        // the unique job owner is retained across all polls.
        if unsafe {
            QueryInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut info).cast(),
                length,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if info.ActiveProcesses == 0 {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(error.map_or_else(
                || "Native process job did not exit after termination".into(),
                |error| error.to_string(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(windows)]
pub(crate) fn windows_job(
    child: &tokio::process::Child,
) -> Result<std::os::windows::io::OwnedHandle, String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
    };
    // SAFETY: null arguments request an unnamed job with default security. Wrap
    // its unique handle immediately so every subsequent error closes it.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let job = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) };
    // SAFETY: this Windows structure is valid with all-zero fields; the only
    // configured limit kills descendants when this owning job handle closes.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let length = u32::try_from(std::mem::size_of_val(&info)).map_err(|error| error.to_string())?;
    let process = child
        .raw_handle()
        .ok_or("Child process has no process handle")?;
    // SAFETY: job/process handles are alive and the pointer/size describe info.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&info).cast(),
            length,
        )
    } == 0
        || unsafe { AssignProcessToJobObject(job.as_raw_handle() as HANDLE, process as HANDLE) }
            == 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_signal_accepts_short_and_long_names_case_insensitively() {
        assert_eq!(parse_signal(Some("SIGINT")), Ok(Signal::Int));
        assert_eq!(parse_signal(Some("int")), Ok(Signal::Int));
        assert_eq!(parse_signal(Some("SigTerm")), Ok(Signal::Term));
        assert_eq!(parse_signal(Some("KILL")), Ok(Signal::Kill));
        assert_eq!(parse_signal(Some("sighup")), Ok(Signal::Hup));
    }

    #[test]
    fn parse_signal_none_defaults_to_kill() {
        assert_eq!(parse_signal(None), Ok(Signal::Kill));
    }

    #[test]
    fn parse_signal_rejects_unknown_names() {
        assert_eq!(
            parse_signal(Some("SIGWHATEVER")),
            Err("unsupported signal".to_string())
        );
    }

    #[test]
    fn signal_name_maps_known_numbers() {
        assert_eq!(signal_name(1), "SIGHUP");
        assert_eq!(signal_name(2), "SIGINT");
        assert_eq!(signal_name(3), "SIGQUIT");
        assert_eq!(signal_name(9), "SIGKILL");
        assert_eq!(signal_name(15), "SIGTERM");
        assert_eq!(signal_name(42), "SIG42");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_group_kills_a_real_process_group() {
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");
        let pgid = i32::try_from(child.id().expect("child has a pid")).expect("pid fits i32");

        terminate_group(pgid, Duration::from_millis(300)).await;

        let status = tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .expect("child should have been killed")
            .expect("wait should succeed");
        assert!(!status.success());
    }
}
