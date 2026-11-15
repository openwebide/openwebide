//! Kill the real native owner on either side of backend Claim, before its ACK
//! can permit native admission. Neither callback loss nor restart may send RPC.
use super::{command_case, command_journal, progress, send, send_json};
use openwebide_core::{
    UserId,
    plugins::languages::intelligence::actions::{
        ActionPhase, CommandOperation, CommandOutcome, CommandProgress, CommandWorkerOperation,
        HostCommandRequest,
    },
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

const CHILD: &str = "OPENWEBIDE_PREPARED_COMMAND_FIXTURE";
const TEST: &str =
    "prepared::prepared_commands_recover_without_dispatch_on_either_side_of_backend_claim";
#[derive(Serialize, Deserialize)]
struct Original {
    root: PathBuf,
    recovery_home: PathBuf,
    callback: String,
    request: HostCommandRequest,
    claim: CommandWorkerOperation,
}

async fn original(directory: &std::path::Path) {
    let (paired, committed): (bool, bool) =
        serde_json::from_slice(&std::fs::read(directory.join("case.json")).unwrap()).unwrap();
    let case = command_case::Case::dispatched(paired, directory).await;
    case.backend
        .block_claim_after_commit
        .store(committed, Ordering::SeqCst);
    case.backend
        .block_command_claim
        .store(true, Ordering::SeqCst);
    assert_eq!(
        progress(send(case.port, paired, case.request.clone()).await),
        CommandProgress::Running
    );
    let claim = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(claim) = case.backend.held_command_claim.lock().unwrap().clone() {
                break claim;
            }
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
        claim,
    };
    assert!(!case.root.join("executions").exists());
    std::fs::write(
        directory.join("ready.tmp"),
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
    std::fs::rename(directory.join("ready.tmp"), directory.join("ready.json")).unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
    panic!("The original Prepared owner was not killed by its supervisor");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_commands_recover_without_dispatch_on_either_side_of_backend_claim() {
    if let Some(directory) = std::env::var_os(CHILD) {
        original(&PathBuf::from(directory)).await;
        return;
    }
    let mut recovered = Vec::new();
    for paired in [false, true] {
        for committed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(
                directory.path().join("case.json"),
                serde_json::to_vec(&(paired, committed)).unwrap(),
            )
            .unwrap();
            let log = std::fs::File::create(directory.path().join("original.log")).unwrap();
            let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
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
            .expect("Original command did not reach its backend Claim boundary");
            let original: Original = serde_json::from_slice(
                &std::fs::read(directory.path().join("ready.json")).unwrap(),
            )
            .unwrap();
            child.start_kill().unwrap();
            let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap();
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGKILL));
            let backend =
                command_journal::Backend::persisted(&directory.path().join("journal.sqlite")).await;
            backend.now.store(103, Ordering::SeqCst);
            let id = &original.request.command.invocation.id;
            let before = backend
                .store
                .workspace_command_recovery(UserId::new(1), 1, id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(before.command.phase, ActionPhase::Running);
            assert!(
                before.command.native_settlement_pending
                    && !before.command.command_cleanup_confirmed
            );
            let editors = backend
                .store
                .editor_recovery(UserId::new(1), 1)
                .await
                .unwrap();
            let source = std::fs::read(original.root.join("main.notes")).unwrap();
            // Cleanup cannot depend on the old provider remaining configured.
            backend
                .store
                .set_user_setting(UserId::new(1), "language_settings", "{revoked provider")
                .await
                .unwrap();
            let url = url::Url::parse(&original.callback).unwrap();
            assert_eq!(url.host_str(), Some("127.0.0.1"));
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", url.port().unwrap()))
                .await
                .unwrap();
            let callbacks = command_journal::serve_on(backend.clone(), listener);
            let peer = command_case::spawn_native_peer(
                original.root.parent().unwrap(),
                &original.recovery_home,
                &callbacks.url,
            )
            .await;
            assert_ne!(peer.pid, original_pid);
            let blocked = send_json(
                peer.port,
                false,
                "/exec",
                &serde_json::json!({"command":"true","cwd":original.root}),
            )
            .await;
            assert_eq!(blocked.status, 400, "{}", blocked.body);
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
                        .workspace_command_recovery(UserId::new(1), 1, id)
                        .await
                        .unwrap()
                        .unwrap();
                    if reply.status == 200
                        && let CommandProgress::Finished {
                            outcome: outcome @ CommandOutcome::Rejected { .. },
                        } = progress(reply)
                        && current.command.phase == ActionPhase::Rejected
                        && current.command.command_cleanup_confirmed
                    {
                        break outcome;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .ok();
            if let Some(outcome) = &outcome {
                let current = backend
                    .store
                    .workspace_command_recovery(UserId::new(1), 1, id)
                    .await
                    .unwrap()
                    .unwrap();
                let record = backend
                    .store
                    .language_action(UserId::new(1), 1, Some(id))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(record.outcome.as_ref(), Some(outcome));
                assert!(current.command.native_settlement_pending);
                // A request delayed before the old owner died cannot acquire
                // dispatch authority after its exact no-dispatch result exists.
                assert!(
                    backend
                        .store
                        .native_language_command(UserId::new(1), 1, id, &original.claim, 104)
                        .await
                        .is_err()
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
                        id,
                        current.command.revision,
                        "Reviewed the original command that was never dispatched",
                        104,
                    )
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(8), async {
                    loop {
                        let reply = send_json(
                            peer.port,
                            false,
                            "/exec",
                            &serde_json::json!({"command":"true","cwd":original.root}),
                        )
                        .await;
                        if reply.status == 200 {
                            let writer: openwebide_core::CommandOutcome =
                                serde_json::from_str(&reply.body).unwrap();
                            assert!(writer.is_success(), "{writer:?}");
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                })
                .await
                .unwrap();
                assert!(
                    backend
                        .store
                        .native_language_command(UserId::new(1), 1, id, &original.claim, 105)
                        .await
                        .is_err()
                );
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
                editors
            );
            assert!(!original.root.join("executions").exists());
            assert!(!original.root.join("child-ready").exists());
            eprintln!(
                "prepared recovery paired={paired} committed={committed} original_pid={original_pid} successor_pid={} outcome={outcome:?}",
                peer.pid
            );
            peer.stop().await;
            recovered.push((paired, committed, outcome));
        }
    }
    assert!(
        recovered.iter().all(|(_, _, result)| result.is_some()),
        "Original Prepared commands did not recover: {recovered:?}"
    );
}
