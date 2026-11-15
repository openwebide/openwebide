//! A real command settles before project deletion, but its native ACK is lost.
//! Native host and Store are real; the LSP peer, reply loss and home are fixtures.
use super::{command_case, progress, send, send_json};
use openwebide_core::{UserId, WorkspaceMode};
use serde_json::{Value, json};
use std::{path::Path, sync::atomic::Ordering, time::Duration};

fn administer(home: &Path, args: &[&str]) -> Result<Value, String> {
    let text = openwebide_bridge::recovery::execute_in_test_home(
        home,
        &args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>(),
    )?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}
async fn writer(port: u16, root: &Path, allowed: bool) {
    let request = openwebide_core::vfs::FileMutationRequest {
        root: root.canonicalize().unwrap().to_string_lossy().into_owned(),
        mutation: openwebide_core::vfs::FileMutation::Write {
            path: "ordinary-file.bin".into(),
            contents: vec![0, 255, 13, 10],
        },
    };
    let response = send_json(port, false, "/workspace/write", &request).await;
    assert_eq!(response.status, 200, "{}", response.body);
    let result: Result<(), openwebide_core::VfsError> =
        serde_json::from_str(&response.body).unwrap();
    if allowed {
        result.unwrap();
        assert_eq!(
            std::fs::read(root.join("ordinary-file.bin")).unwrap(),
            [0, 255, 13, 10]
        );
    } else {
        assert!(
            matches!(result, Err(openwebide_core::VfsError::Conflict(_))),
            "{result:?}"
        );
        assert!(!root.join("ordinary-file.bin").exists());
    }
    let reply = send_json(
        port,
        false,
        "/exec",
        &json!({"command":"printf verified > after-retirement.txt","cwd":root,"timeout_seconds":5}),
    )
    .await;
    assert_eq!(
        reply.status,
        if allowed { 200 } else { 400 },
        "{}",
        reply.body
    );
    if allowed {
        assert_eq!(
            serde_json::from_str::<Value>(&reply.body).unwrap()["exit_code"],
            0
        );
        assert_eq!(
            std::fs::read(root.join("after-retirement.txt")).unwrap(),
            b"verified"
        );
    } else {
        assert!(
            !root.join("after-retirement.txt").exists(),
            "A denied command wrote to the folder"
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_project_keeps_native_claim_until_explicit_review_of_the_real_command() {
    for paired in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut case = command_case::Case::completing(paired, directory.path()).await;
        case.backend
            .drop_settlement_ack
            .store(true, Ordering::SeqCst);
        assert_eq!(
            progress(send(case.port, paired, case.request.clone()).await),
            super::CommandProgress::Running
        );
        tokio::time::timeout(Duration::from_secs(15), async {
            while case.backend.lost_settlement_acks.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let user = UserId::new(1);
        let id = case.request.command.invocation.id.clone();
        let action = case
            .backend
            .store
            .language_action(user, 1, Some(&id))
            .await
            .unwrap()
            .unwrap();
        let outcome = action.outcome.unwrap();
        assert!(matches!(outcome, super::CommandOutcome::Completed { .. }));
        let recovery = case
            .backend
            .store
            .workspace_command_recovery(user, 1, &id)
            .await
            .unwrap()
            .unwrap();
        assert!(recovery.activity.ensure_idle().is_ok());
        assert!(!recovery.command.native_settlement_pending);
        assert!(recovery.command.command_cleanup_confirmed);
        let source = std::fs::read(case.root.join("main.notes")).unwrap();
        let executions = std::fs::read(case.root.join("executions")).unwrap();
        assert_eq!(
            executions
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            1
        );
        writer(case.port, &case.root, false).await;

        // This is the production owner-scoped deletion, including FK cascades,
        // after a genuinely committed settlement; no terminal row is fabricated.
        case.backend.store.delete_project(1, user).await.unwrap();
        assert!(matches!(
            case.backend.store.get_project(1, user).await,
            Err(openwebide_storage::StorageError::NotFound(_))
        ));
        assert!(matches!(
            case.backend.store.language_action(user, 1, Some(&id)).await,
            Err(openwebide_storage::StorageError::NotFound(_))
        ));
        assert!(matches!(
            case.backend
                .store
                .workspace_command_recovery(user, 1, &id)
                .await,
            Err(openwebide_storage::StorageError::NotFound(_))
        ));
        case.stop_native().await;
        case.backend
            .original_unavailable
            .store(false, Ordering::SeqCst);
        let previous_calls = case.backend.original_calls.load(Ordering::SeqCst);
        let peer = case.spawn_native_peer().await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while case.backend.original_calls.load(Ordering::SeqCst) == previous_calls {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        writer(peer.port, &case.root, false).await;
        let original = administer(case.recovery_home(), &["inspect"]).unwrap();
        assert_eq!(original["owners"].as_array().unwrap().len(), 1);
        assert_eq!(original["settlements"], json!([]));
        assert_eq!(original["retired"], json!([]));
        let entry = &original["owners"][0];
        assert_eq!(entry["outcome"], serde_json::to_value(&outcome).unwrap());
        assert_eq!(
            entry["outcome_digest"],
            recovery.command.outcome_digest.unwrap()
        );
        assert_eq!(entry["user"], 1);
        assert_eq!(entry["project"], 1);
        assert_eq!(entry["transaction"], id);
        assert_eq!(
            entry["mode"],
            serde_json::to_value(if paired {
                WorkspaceMode::Local
            } else {
                WorkspaceMode::Remote
            })
            .unwrap()
        );
        assert!(entry["retirement_blocked"].is_null());
        let args = [
            "retire",
            entry["id"].as_str().unwrap(),
            "--expected",
            entry["expected"].as_str().unwrap(),
            "--note",
            "Reviewed the original command and files after deleting the original project",
        ];
        // A failed callback can release its in-memory lease while the durable
        // owner continues fencing ordinary writers. Stop that retry process
        // before the explicit review; liveness alone is not a physical claim.
        peer.stop().await;
        let audit = administer(case.recovery_home(), &args).unwrap();
        assert_eq!(audit["original"], *entry);
        assert_eq!(administer(case.recovery_home(), &args).unwrap(), audit);
        let retired = json!({"owners":[],"settlements":[],"retired":[audit]});
        assert_eq!(
            administer(case.recovery_home(), &["inspect"]).unwrap(),
            retired
        );

        // Fresh native startup cannot turn a missing backend project into replay
        // authority, and the permanent audit survives ordinary writes/restarts.
        let successor = case.spawn_native_peer().await;
        writer(successor.port, &case.root, true).await;
        assert_eq!(std::fs::read(case.root.join("main.notes")).unwrap(), source);
        assert_eq!(
            std::fs::read(case.root.join("executions")).unwrap(),
            executions
        );
        assert_eq!(
            administer(case.recovery_home(), &["inspect"]).unwrap(),
            retired
        );
        successor.stop().await;
        eprintln!(
            "deleted project recovered paired={paired}; real command executed once, original outcome retained, explicit review required"
        );
    }
}
