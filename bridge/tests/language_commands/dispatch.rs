//! Real dispatch and original-group cleanup after the owning HTTP/backend process
//! is killed. The durable Store and private namespace survive in disposable homes.
use super::{command_case, command_journal, progress, send, send_json};
use openwebide_core::{
    UserId,
    plugins::languages::intelligence::actions::{
        ActionPhase, CommandOperation, CommandOutcome, CommandProgress, HostCommandRequest,
    },
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

const CHILD: &str = "OPENWEBIDE_COMMAND_PROCESS_FIXTURE";
#[derive(Serialize, Deserialize)]
struct Original {
    root: PathBuf,
    recovery_home: PathBuf,
    callback: String,
    request: HostCommandRequest,
}
async fn original(directory: &std::path::Path) {
    let paired: bool =
        serde_json::from_slice(&std::fs::read(directory.join("paired.json")).unwrap()).unwrap();
    let case = command_case::Case::dispatched(paired, directory).await;
    assert_eq!(
        progress(send(case.port, paired, case.request.clone()).await),
        CommandProgress::Running
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !case.root.join("executions").exists()
            || !case.root.join("child-ready").exists()
            || !case.root.join("heartbeat").exists()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let data = Original {
        root: case.root.clone(),
        recovery_home: case.recovery_home().into(),
        callback: case.backend_url().into(),
        request: case.request.clone(),
    };
    std::fs::write(
        directory.join("ready.tmp"),
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
    std::fs::rename(directory.join("ready.tmp"), directory.join("ready.json")).unwrap();
    // The supervising test kills this actual process; no Rust shutdown runs.
    tokio::time::sleep(Duration::from_secs(120)).await;
    panic!("The original command owner was not killed by its supervisor");
}
async fn quiet(root: &std::path::Path) {
    // Observation for the test only. Production recovery must obtain an original
    // durable receipt, never infer cleanup from this heartbeat or a missing PID.
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut previous = std::fs::read(root.join("heartbeat")).ok();
        let mut stable = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let current = std::fs::read(root.join("heartbeat")).ok();
            stable = if current.is_some() && current == previous {
                stable + 1
            } else {
                0
            };
            previous = current;
            if stable == 8 {
                break;
            }
        }
    })
    .await
    .expect("The original independent watcher did not stop the managed descendant");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatched_commands_recover_after_original_process_cleanup() {
    if let Some(directory) = std::env::var_os(CHILD) {
        original(&PathBuf::from(directory)).await;
        return;
    }
    let mut recovered = Vec::new();
    for paired in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("paired.json"),
            serde_json::to_vec(&paired).unwrap(),
        )
        .unwrap();
        let log = std::fs::File::create(directory.path().join("original.log")).unwrap();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dispatch::dispatched_commands_recover_after_original_process_cleanup",
                "--nocapture",
            ])
            .env(CHILD, directory.path())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let original_pid = child.id().unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            while !directory.path().join("ready.json").exists() {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!(
                        "Original process exited {status}: {}",
                        std::fs::read_to_string(directory.path().join("original.log")).unwrap()
                    );
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Original command did not dispatch");
        let original: Original =
            serde_json::from_slice(&std::fs::read(directory.path().join("ready.json")).unwrap())
                .unwrap();
        child.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        quiet(&original.root).await;
        let backend =
            command_journal::Backend::persisted(&directory.path().join("journal.sqlite")).await;
        backend.now.store(103, Ordering::SeqCst);
        let url = url::Url::parse(&original.callback).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", url.port().unwrap()))
            .await
            .unwrap();
        let callbacks = command_journal::serve_on(backend.clone(), listener);
        assert_eq!(callbacks.url, original.callback);
        let before = backend
            .store
            .editor_recovery(UserId::new(1), 1)
            .await
            .unwrap();
        let source = std::fs::read(original.root.join("main.notes")).unwrap();
        let peer = command_case::spawn_native_peer(
            original.root.parent().unwrap(),
            &original.recovery_home,
            &callbacks.url,
        )
        .await;
        assert_ne!(peer.pid, original_pid);
        let mut control = original.request.clone();
        control.root.clear();
        control.command.operation = CommandOperation::Stop;
        control.command.selection = None;
        control.authority = None;
        control.journal = None;
        let outcome = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let reply = send(peer.port, paired, control.clone()).await;
                let current = backend
                    .store
                    .workspace_command_recovery(
                        UserId::new(1),
                        1,
                        &original.request.command.invocation.id,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                if reply.status == 200
                    && let CommandProgress::Finished { outcome } = progress(reply)
                    && current.command.phase == ActionPhase::Uncertain
                    && current.command.command_cleanup_confirmed
                {
                    break outcome;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .ok();
        if let Some(outcome) = &outcome {
            assert!(
                matches!(outcome, CommandOutcome::Uncertain { .. }),
                "{outcome:?}"
            );
            let current = backend
                .store
                .workspace_command_recovery(
                    UserId::new(1),
                    1,
                    &original.request.command.invocation.id,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(current.command.phase, ActionPhase::Uncertain);
            assert!(
                current.command.command_cleanup_confirmed
                    && current.command.native_settlement_pending
            );
            assert_eq!(
                send_json(
                    peer.port,
                    false,
                    "/exec",
                    &serde_json::json!({"command":"true","cwd":original.root})
                )
                .await
                .status,
                400
            );
            backend
                .store
                .acknowledge_language_action(
                    UserId::new(1),
                    1,
                    &original.request.command.invocation.id,
                    current.command.revision,
                    "Reviewed the original command effects after process loss",
                    104,
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let current = backend
                        .store
                        .workspace_command_recovery(
                            UserId::new(1),
                            1,
                            &original.request.command.invocation.id,
                        )
                        .await
                        .unwrap()
                        .unwrap();
                    if !current.command.native_settlement_pending {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            let response = send_json(
                peer.port,
                false,
                "/exec",
                &serde_json::json!({"command":"true","cwd":original.root}),
            )
            .await;
            assert_eq!(response.status, 200, "{}", response.body);
            let writer: openwebide_core::CommandOutcome =
                serde_json::from_str(&response.body).unwrap();
            assert!(writer.is_success(), "{writer:?}");
        }
        assert_eq!(
            std::fs::read(original.root.join("main.notes")).unwrap(),
            source
        );
        assert_eq!(
            backend
                .store
                .editor_recovery(UserId::new(1), 1)
                .await
                .unwrap(),
            before
        );
        assert_eq!(
            std::fs::read_to_string(original.root.join("executions"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        eprintln!(
            "dispatched recovery paired={paired} original_pid={original_pid} successor_pid={} outcome={outcome:?}",
            peer.pid
        );
        peer.stop().await;
        recovered.push((paired, outcome));
    }
    assert!(
        recovered.iter().all(|(_, result)| result.is_some()),
        "The original dispatched command never obtained a durable cleanup result: {recovered:?}"
    );
}
