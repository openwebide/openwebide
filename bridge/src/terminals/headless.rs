//! Headless (non-PTY) terminal sessions with streamed stdout/stderr.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::exec::proc::Signal;
use crate::terminals::session::Session;

/// Spawn a non-interactive process attached to a Session.
pub fn spawn_headless(id: String, spec: crate::exec::SpawnSpec) -> Result<Arc<Session>, String> {
    let display_cmd = spec.display_cmd();
    let crate::exec::SpawnSpec {
        command,
        args,
        cwd: effective_cwd,
        env,
        ..
    } = spec;
    let mut cmd = Command::new(&command);
    cmd.args(&args);
    cmd.current_dir(&effective_cwd);
    cmd.envs(env);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn '{command}': {e}"))?;

    let pid = child.id().and_then(|id| i32::try_from(id).ok());

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin = child.stdin.take();

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(128);
    let (resize_tx, _resize_rx) = mpsc::channel::<(u16, u16)>(1);
    let (kill_tx, mut kill_rx) = mpsc::channel::<Signal>(4);

    let session = Arc::new(Session::new(
        id,
        display_cmd,
        false,
        pid,
        stdin_tx,
        resize_tx,
        kill_tx,
    ));

    // 1. Stdout reader task
    let mut out_task = if let Some(mut stdout) = stdout {
        let sess = session.clone();
        Some(tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let mut decoder = openwebide_core::utf8::Utf8Decoder::new();
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) => {
                        let text = decoder.finish();
                        if !text.is_empty() {
                            sess.push_output("stdout", &text);
                        }
                        break;
                    }
                    Ok(n) => {
                        let text = decoder.push(&buf[..n]);
                        if !text.is_empty() {
                            sess.push_output("stdout", &text);
                        }
                    }
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::Interrupted {
                            let text = decoder.finish();
                            if !text.is_empty() {
                                sess.push_output("stdout", &text);
                            }
                            break;
                        }
                    }
                }
            }
        }))
    } else {
        None
    };

    // 2. Stderr reader task
    let mut err_task = if let Some(mut stderr) = stderr {
        let sess = session.clone();
        Some(tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let mut decoder = openwebide_core::utf8::Utf8Decoder::new();
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) => {
                        let text = decoder.finish();
                        if !text.is_empty() {
                            sess.push_output("stderr", &text);
                        }
                        break;
                    }
                    Ok(n) => {
                        let text = decoder.push(&buf[..n]);
                        if !text.is_empty() {
                            sess.push_output("stderr", &text);
                        }
                    }
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::Interrupted {
                            let text = decoder.finish();
                            if !text.is_empty() {
                                sess.push_output("stderr", &text);
                            }
                            break;
                        }
                    }
                }
            }
        }))
    } else {
        None
    };

    // 3. Stdin writer task
    if let Some(mut stdin) = stdin {
        tokio::spawn(async move {
            while let Some(data) = stdin_rx.recv().await {
                if stdin.write_all(data.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
    }

    // 4. Process supervisor task: signals the group on each `Kill` message (and keeps waiting
    // for exit afterward, so escalating from SIGTERM to SIGKILL works), and reports the exact
    // exit code/signal once the child is reaped.
    let sess_exit = session.clone();
    tokio::spawn(async move {
        let mut kill_channel_open = true;

        // `child.wait()` is cancel-safe, so a fresh one each iteration (rather than a single
        // pinned future reused across iterations) is fine, and it keeps `child` free between
        // iterations for the Windows kill branch to call `start_kill()` on.
        let status = loop {
            tokio::select! {
                sig = kill_rx.recv(), if kill_channel_open => {
                    match sig {
                        Some(sig) => {
                            #[cfg(unix)]
                            if let Some(pgid) = pid {
                                crate::exec::proc::signal_group(pgid, sig);
                            }
                            #[cfg(windows)]
                            {
                                let _ = sig;
                                let _ = child.start_kill();
                            }
                        }
                        None => kill_channel_open = false,
                    }
                }
                status = child.wait() => {
                    break status;
                }
            }
        };

        if tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                async {
                    if let Some(ref mut t) = out_task {
                        let _ = t.await;
                    }
                },
                async {
                    if let Some(ref mut t) = err_task {
                        let _ = t.await;
                    }
                }
            )
        })
        .await
        .is_err()
        {
            if let Some(t) = out_task {
                t.abort();
            }
            if let Some(t) = err_task {
                t.abort();
            }
        }

        match status {
            Ok(s) => {
                #[cfg(unix)]
                let signal = {
                    use std::os::unix::process::ExitStatusExt;
                    s.signal().map(crate::exec::proc::signal_name)
                };
                #[cfg(windows)]
                let signal = None;
                sess_exit.emit_exit(s.code(), signal);
            }
            Err(e) => {
                sess_exit.emit_error(&format!("wait failed: {e}"));
                sess_exit.emit_exit(Some(1), None);
            }
        }
    });

    Ok(session)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::terminals::session::SessionManager;

    #[cfg(unix)]
    #[tokio::test]
    async fn headless_kill_sigint_reports_signal() {
        let temp_dir = crate::paths::canonical_root(&std::env::temp_dir()).unwrap();
        let session = spawn_headless(
            "sess-int".into(),
            crate::exec::SpawnSpec::in_root(
                "sleep".into(),
                vec!["5".into()],
                None,
                Default::default(),
                &temp_dir,
            )
            .unwrap(),
        )
        .unwrap();

        let mut rx = session.ring.lock().unwrap().subscribe();
        session.try_kill(Signal::Int);

        let exited = tokio::time::timeout(Duration::from_secs(2), async {
            let mut cursor = 0;
            loop {
                let _ = rx.changed().await;
                let (batch, _) = session.ring.lock().unwrap().read_after(cursor, 100);
                for (seq, msg) in batch {
                    cursor = seq;
                    if let openwebide_core::BridgeServerMessage::Exited {
                        exit_code, signal, ..
                    } = msg
                    {
                        return (exit_code, signal);
                    }
                }
            }
        })
        .await
        .expect("session should exit");

        assert_eq!(exited, (None, Some("SIGINT".to_string())));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn headless_kill_reaches_grandchild() {
        let dir = crate::paths::canonical_root(&std::env::temp_dir()).unwrap();
        let dir = dir.join(format!("owide-killgroup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("grandchild");
        let cmd = format!("(sleep 5; touch {}) & wait", marker.display());

        let session = spawn_headless(
            "sess-kill".into(),
            crate::exec::SpawnSpec::in_root(
                "sh".into(),
                vec!["-c".into(), cmd],
                None,
                Default::default(),
                &dir,
            )
            .unwrap(),
        )
        .unwrap();

        session.try_kill(Signal::Kill);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!session.running.load(std::sync::atomic::Ordering::SeqCst));

        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(
            !marker.exists(),
            "SIGKILL to the group must reach the grandchild"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_all_terminates_running() {
        let dir = crate::paths::canonical_root(&std::env::temp_dir()).unwrap();
        let mgr = SessionManager::new();
        let session = spawn_headless(
            "sess-killall".into(),
            crate::exec::SpawnSpec::in_root(
                "sleep".into(),
                vec!["30".into()],
                None,
                Default::default(),
                &dir,
            )
            .unwrap(),
        )
        .unwrap();
        let _ = mgr.try_insert(session.clone());

        mgr.kill_all().await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(!session.running.load(std::sync::atomic::Ordering::SeqCst));
    }
}
