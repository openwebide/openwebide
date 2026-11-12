use super::SpawnSpec;
use crate::BridgeError;
use openwebide_core::CommandOutcome;
use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Head bytes kept verbatim per captured stream before the omission marker.
const CAPTURE_HEAD_BYTES: usize = 256 * 1024;
/// Tail bytes kept verbatim per captured stream after the omission marker.
const CAPTURE_TAIL_BYTES: usize = 768 * 1024;

/// Accumulates a byte stream, keeping the first `head_cap` bytes and the last `tail_cap` bytes,
/// with everything in between dropped and reported via an omission marker. Bounds memory use for
/// commands that produce far more output than anyone will read.
struct CappedCapture {
    head: Vec<u8>,
    head_cap: usize,
    tail: VecDeque<u8>,
    tail_cap: usize,
    total: usize,
}

impl CappedCapture {
    fn new(head_cap: usize, tail_cap: usize) -> Self {
        Self {
            head: Vec::new(),
            head_cap,
            tail: VecDeque::new(),
            tail_cap,
            total: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        let remaining_head = self.head_cap.saturating_sub(self.head.len());
        let (to_head, to_tail) = if chunk.len() <= remaining_head {
            (chunk, &[][..])
        } else {
            chunk.split_at(remaining_head)
        };
        self.head.extend_from_slice(to_head);
        self.tail.extend(to_tail);
        while self.tail.len() > self.tail_cap {
            self.tail.pop_front();
        }
    }

    fn finish(self) -> String {
        let kept = self.head.len() + self.tail.len();
        let tail_bytes: Vec<u8> = self.tail.into_iter().collect();
        if self.total <= kept {
            let mut bytes = self.head;
            bytes.extend(tail_bytes);
            return String::from_utf8_lossy(&bytes).into_owned();
        }
        let omitted = self.total - kept;
        let mut out = String::from_utf8_lossy(&self.head).into_owned();
        out.push_str(&format!("\n[… {omitted} bytes omitted …]\n"));
        out.push_str(&String::from_utf8_lossy(&tail_bytes));
        out
    }
}

/// Read `reader` to EOF, capping the captured content, and return the resulting text.
async fn capture_stream<R: AsyncRead + Unpin>(mut reader: R) -> String {
    let mut capture = CappedCapture::new(CAPTURE_HEAD_BYTES, CAPTURE_TAIL_BYTES);
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => capture.push(&buf[..n]),
        }
    }
    capture.finish()
}

/// Execute a command with a timeout, capturing stdout and stderr into a
/// [`CommandOutcome`]. `spec.cancel` resolves when the caller no longer needs the result (e.g.
/// the run driving this call was cancelled); the command's process group is killed either way.
/// Direct callers may also drop this future; its `GroupGuard` kills the group.
/// Coordinated HTTP/run execution instead owns this future in a worker, turns
/// observer loss into `spec.cancel`, and retains folder exclusion through cleanup.
pub(super) async fn execute_command_direct(spec: SpawnSpec) -> Result<CommandOutcome, BridgeError> {
    let display_cmd = spec.display_cmd();
    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args);
    cmd.envs(&spec.env);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.current_dir(&spec.cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let (mut child, mut owner) = if let Some(folder) = &spec.folder {
        let (owner, child) = super::process_owner::commands::Command::spawn(&spec, folder)
            .map_err(BridgeError::Execution)?;
        (child, Some(owner))
    } else {
        (
            cmd.spawn().map_err(|e| {
                BridgeError::Execution(format!("failed to spawn command '{display_cmd}': {e}"))
            })?,
            None,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let mut child = {
        let _ = &spec.folder;
        cmd.spawn().map_err(|e| {
            BridgeError::Execution(format!("failed to spawn command '{display_cmd}': {e}"))
        })?
    };

    let pid = child.id().and_then(|id| i32::try_from(id).ok());
    // Armed for as long as this future can still be dropped mid-flight (e.g. hyper dropping the
    // `/exec` handler on a disconnected client) instead of running to a controlled exit —
    // including while the timeout/cancel branches below are themselves awaiting
    // `terminate_group`'s grace period. Disarmed once a controlled exit is reached, so a
    // successful command doesn't have its intentionally backgrounded/detached jobs killed.
    #[cfg(unix)]
    let group_guard = {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let pid = pid.filter(|_| owner.is_none());
        pid.map(crate::exec::proc::GroupGuard::new)
    };

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let out_task = tokio::spawn(capture_stream(stdout));
    let err_task = tokio::spawn(capture_stream(stderr));

    let timeout_duration = spec
        .timeout
        .clamp(Duration::from_secs(1), Duration::from_secs(3600));

    // Waits for the leader AND drains both pipes as one future, so the timeout/cancel branches
    // below actually race it: a backgrounded job that outlives the leader but still holds a pipe
    // open (e.g. `sleep 20 &`) would otherwise hang `out_task`/`err_task` forever after `child`
    // exits, with neither the timer nor cancellation polled anymore.
    let run = async {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let status = if let Some(owner) = &owner {
            owner.status().await.map_err(std::io::Error::other)
        } else {
            child.wait().await
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let status = child.wait().await;
        if status.is_err() {
            // A lost owner can leave a child holding its pipes open. Enter
            // cleanup/quarantine now instead of waiting for unrelated EOF.
            return (status, String::new(), String::new());
        }
        let stdout = out_task.await.unwrap_or_default();
        let stderr = err_task.await.unwrap_or_default();
        (status, stdout, stderr)
    };

    let result = tokio::select! {
        (status, stdout, stderr) = run => {
            status.map(|status| CommandOutcome {
                exit_code: status.code(),
                stdout,
                stderr,
            }).map_err(|error| BridgeError::Execution(format!("execution error: {error}")))
        }
        () = tokio::time::sleep(timeout_duration) => {
            Err(BridgeError::Execution(format!("command timed out after {}s", spec.timeout.as_secs())))
        }
        () = spec.cancel => {
            Err(BridgeError::Execution("command cancelled".to_string()))
        }
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let result = match (result, owner.as_mut()) {
        (Ok(outcome), Some(owner)) => owner
            .complete()
            .await
            .map(|()| outcome)
            .map_err(BridgeError::Execution),
        (result, _) => result,
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut cleanup_unconfirmed = false;
    if result.is_err() {
        #[cfg(windows)]
        let _ = child.start_kill();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(owner) = &owner {
            owner.stop();
            // Signal delivery is not completion. A stuck helper retains its
            // descriptors independently; an observation timeout cannot free them.
            let confirmed = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            if !matches!(confirmed, Ok(Ok(status)) if status.success())
                && let Some(folder) = &spec.folder
            {
                folder.mark_uncertain();
                cleanup_unconfirmed = true;
            }
        } else {
            // Reap the direct child concurrently with group termination.
            let _ = tokio::join!(terminate_spawned_group(pid), child.wait());
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = tokio::join!(terminate_spawned_group(pid), child.wait());
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let result = if cleanup_unconfirmed {
        result.map_err(|error| BridgeError::Execution(format!(
            "{error}; native process cleanup is unconfirmed and the original folder remains blocked"
        )))
    } else {
        result
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if result.is_ok() && owner.is_some() {
        // Keep the live coordinator claim and its descriptor reservation too;
        // successful background commands cannot free the host's capacity early.
        let folder = spec.folder.clone();
        tokio::spawn(async move {
            if !child.wait().await.is_ok_and(|status| status.success())
                && let Some(folder) = &folder
            {
                folder.mark_uncertain();
            }
            drop(folder);
        });
    }

    #[cfg(unix)]
    if let Some(guard) = group_guard {
        guard.disarm();
    }

    result
}

#[cfg(unix)]
async fn terminate_spawned_group(pid: Option<i32>) {
    if let Some(pgid) = pid {
        crate::exec::proc::terminate_group(pgid, Duration::from_secs(2)).await;
    }
}

#[cfg(windows)]
async fn terminate_spawned_group(_pid: Option<i32>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    async fn execute_command_direct(
        command: &str,
        cwd: &Path,
        timeout_seconds: u64,
        cancel: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<CommandOutcome, String> {
        use crate::exec::ToolExecution;
        crate::exec::HostExecution
            .run_command(crate::exec::SpawnSpec::shell(
                command.into(),
                cwd.into(),
                timeout_seconds,
                cancel,
            ))
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn test_execute_command_direct_success() {
        let temp_dir = std::env::temp_dir();
        let res =
            execute_command_direct("echo 'openwebide'", &temp_dir, 5, std::future::pending()).await;
        assert!(res.is_ok());
        let outcome = res.unwrap();
        assert_eq!(outcome.exit_code, Some(0));
        assert!(outcome.stdout.contains("openwebide"));
        assert!(outcome.is_success());
    }

    #[tokio::test]
    async fn test_execute_command_direct_failure() {
        let temp_dir = std::env::temp_dir();
        let res = execute_command_direct("exit 42", &temp_dir, 5, std::future::pending()).await;
        assert!(res.is_ok());
        let outcome = res.unwrap();
        assert_eq!(outcome.exit_code, Some(42));
        assert!(!outcome.is_success());
    }

    #[tokio::test]
    async fn test_execute_command_direct_timeout() {
        let temp_dir = std::env::temp_dir();
        let started = std::time::Instant::now();
        let res = execute_command_direct("sleep 3", &temp_dir, 1, std::future::pending()).await;
        let elapsed = started.elapsed();
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("timed out"));
        // Reaping the leader concurrently with `terminate_group`'s liveness poll means it need
        // not pay the full 2s grace period on top of the 1s timeout.
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn exec_stdin_is_null() {
        let temp_dir = std::env::temp_dir();
        let started = std::time::Instant::now();
        let res = execute_command_direct("cat", &temp_dir, 5, std::future::pending()).await;
        assert!(res.is_ok(), "{res:?}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cat should see immediate EOF on stdin, not hang"
        );
    }

    #[tokio::test]
    async fn exec_output_is_capped() {
        let temp_dir = std::env::temp_dir();
        let res = execute_command_direct(
            "yes x | head -c 5000000",
            &temp_dir,
            10,
            std::future::pending(),
        )
        .await
        .unwrap();
        assert!(
            res.stdout.len() <= CAPTURE_HEAD_BYTES + CAPTURE_TAIL_BYTES + 200,
            "capped output grew to {} bytes",
            res.stdout.len()
        );
        assert!(res.stdout.contains("bytes omitted"));
    }

    /// A backgrounded job that outlives the shell but still holds stdout open must not defeat
    /// the timeout: the leader (`sh`) exits well before the 2s timeout, but the pipe stays open
    /// until the backgrounded `sleep 20` exits, so a naive "wait for EOF" drain would hang the
    /// whole call regardless of the timeout.
    #[cfg(unix)]
    #[tokio::test]
    async fn exec_timeout_fires_despite_backgrounded_stdout_holder() {
        let temp_dir = std::env::temp_dir();
        let started = std::time::Instant::now();
        let res = execute_command_direct(
            "sleep 20 & echo started",
            &temp_dir,
            2,
            std::future::pending(),
        )
        .await;
        let elapsed = started.elapsed();
        assert!(
            res.unwrap_err().contains("timed out"),
            "elapsed {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(4), "took {elapsed:?}");
    }

    /// Same as above, but for the `disconnected` cancellation path rather than the timeout.
    #[cfg(unix)]
    #[tokio::test]
    async fn exec_cancel_fires_despite_backgrounded_stdout_holder() {
        let temp_dir = std::env::temp_dir();
        let started = std::time::Instant::now();
        let res = execute_command_direct(
            "sleep 20 &",
            &temp_dir,
            300,
            tokio::time::sleep(Duration::from_secs(1)),
        )
        .await;
        let elapsed = started.elapsed();
        assert!(
            res.unwrap_err().contains("cancelled"),
            "elapsed {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
    }

    /// A successful command must not have its intentionally detached/backgrounded jobs killed:
    /// `GroupGuard` exists for the case where this future is dropped mid-flight (a disconnected
    /// client), not for a command that ran to completion.
    #[cfg(unix)]
    #[tokio::test]
    async fn exec_success_does_not_kill_detached_job() {
        let dir = crate::paths::canonical_root(&std::env::temp_dir()).unwrap();
        let dir = dir.join(format!("owide-detached-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        let cmd = format!(
            "nohup sleep 5 >/dev/null 2>&1 & echo $! > {}",
            pidfile.display()
        );

        let res = execute_command_direct(&cmd, &dir, 5, std::future::pending())
            .await
            .unwrap();
        assert!(res.is_success());

        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        // SAFETY: signal 0 sends nothing; it only probes whether `pid` is still alive.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        // SAFETY: pid is our own freshly-spawned detached child; terminating it in test cleanup
        // is always sound.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            alive,
            "a detached background job must survive a successful /exec"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_timeout_kills_process_group() {
        let dir = crate::paths::canonical_root(&std::env::temp_dir()).unwrap();
        let dir = dir.join(format!("owide-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m1 = dir.join("direct");
        let m2 = dir.join("grandchild");
        let cmd = format!(
            "(sleep 2; touch {}) & sleep 2; touch {}",
            m2.display(),
            m1.display()
        );
        let res = execute_command_direct(&cmd, &dir, 1, std::future::pending()).await;
        assert!(res.unwrap_err().contains("timed out"));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            !m1.exists() && !m2.exists(),
            "timed-out process group must be dead"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
