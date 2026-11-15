use super::*;
use openwebide_core::{
    UserId,
    plugins::languages::intelligence::actions::{ActionPhase, ActionRecoveryRequest},
};
use std::sync::atomic::Ordering;

async fn original(
    case: &command_case::Case,
) -> openwebide_core::workspace_transaction::WorkspaceCommandRecovery {
    case.backend
        .store
        .workspace_command_recovery(UserId::new(1), 1, &case.request.command.invocation.id)
        .await
        .unwrap()
        .unwrap()
}
async fn writer(case: &command_case::Case, paired: bool) -> HttpResponse {
    let response = send_json(
        case.port,
        paired,
        "/exec",
        &json!({"command":"true","cwd":case.root.to_string_lossy(),"timeout_seconds":5}),
    )
    .await;
    if response.status == 200 {
        let outcome: openwebide_core::CommandOutcome =
            serde_json::from_str(&response.body).unwrap();
        assert!(outcome.is_success(), "{outcome:?}");
    }
    response
}
async fn wait_counter(counter: &std::sync::atomic::AtomicUsize, minimum: usize) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while counter.load(Ordering::SeqCst) < minimum {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_controls_keep_their_terminal_state_on_warm_and_retired_services() {
    for paired in [false, true] {
        let case = command_case::Case::cancelled(paired, false).await;
        case.backend
            .original_unavailable
            .store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(8), async {
            while writer(&case, paired).await.status != 200 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let mut status = case.request.clone();
        status.command.operation = CommandOperation::Status;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let observed = progress(send(case.port, paired, status.clone()).await);
                if observed == CommandProgress::Cancelled {
                    break;
                }
                assert_eq!(observed, CommandProgress::Ready);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        case.host.revoke(
            if paired { "paired/user:1" } else { "user:1" },
            Some(1),
            None,
        );
        assert_eq!(
            progress(send(case.port, paired, status).await),
            CommandProgress::Cancelled
        );
        assert!(!case.root.join("executions").exists());
        case.close().await;
    }
}

#[tokio::test]
async fn cancelled_commands_retire_without_an_outcome_after_original_context_and_process_recovery()
{
    for paired in [false, true] {
        for unadmitted in [false, true] {
            for fault in 0..5 {
                let mut case = command_case::Case::cancelled(paired, unadmitted).await;
                case.backend
                    .store
                    .set_user_setting(
                        UserId::new(1),
                        "language_settings",
                        "{revoked provider configuration",
                    )
                    .await
                    .unwrap();
                case.host.revoke(
                    if paired { "paired/user:1" } else { "user:1" },
                    Some(1),
                    None,
                );
                let before = original(&case).await;
                assert_eq!(before.command.phase, ActionPhase::Cancelled);
                assert!(before.command.outcome_digest.is_none());
                assert_eq!(before.command.worker.is_none(), unadmitted);
                assert_eq!(writer(&case, paired).await.status, 400);
                let mut forged = case.request.clone();
                forged.command.invocation.review = "f".repeat(64);
                assert_eq!(send(case.port, paired, forged).await.status, 400);
                let moved = case.root.with_extension("original");
                if fault != 0 {
                    case.stop_native().await;
                }
                if fault == 2 {
                    std::fs::rename(&case.root, &moved).unwrap();
                    std::fs::create_dir(&case.root).unwrap();
                    std::fs::write(case.root.join("sentinel"), "replacement").unwrap();
                }
                let pid = if fault != 0 {
                    Some(case.start_native_process().await)
                } else {
                    None
                };
                if fault == 3 {
                    case.backend
                        .drop_cancellation_ack
                        .store(true, Ordering::SeqCst);
                }
                if fault == 4 {
                    case.backend
                        .drop_settlement_ack
                        .store(true, Ordering::SeqCst);
                }
                if fault == 1 {
                    let next = case.backend.original_calls.load(Ordering::SeqCst) + 1;
                    wait_counter(&case.backend.original_calls, next).await;
                    assert_eq!(writer(&case, paired).await.status, 400);
                }
                case.backend
                    .original_unavailable
                    .store(false, Ordering::SeqCst);
                if fault == 0 {
                    assert_eq!(
                        progress(send(case.port, paired, case.request.clone()).await),
                        CommandProgress::Cancelled
                    );
                } else if fault == 2 {
                    let next = case.backend.original_calls.load(Ordering::SeqCst) + 1;
                    wait_counter(&case.backend.original_calls, next).await;
                    assert_eq!(writer(&case, paired).await.status, 400);
                    assert!(!original(&case).await.command.command_cleanup_confirmed);
                    assert_eq!(
                        std::fs::read_to_string(case.root.join("sentinel")).unwrap(),
                        "replacement"
                    );
                    std::fs::remove_file(case.root.join("sentinel")).unwrap();
                    std::fs::remove_dir(&case.root).unwrap();
                    std::fs::rename(&moved, &case.root).unwrap();
                } else if fault >= 3 {
                    wait_counter(
                        if fault == 3 {
                            &case.backend.lost_cancellation_acks
                        } else {
                            &case.backend.lost_settlement_acks
                        },
                        1,
                    )
                    .await;
                    let receipt = original(&case).await;
                    assert_eq!(receipt.command.phase, ActionPhase::Cancelled);
                    assert!(
                        receipt.command.outcome_digest.is_none()
                            && receipt.command.command_cleanup_confirmed
                    );
                    assert_eq!(receipt.command.native_settlement_pending, fault == 3);
                    assert_eq!(writer(&case, paired).await.status, 400);
                    let killed = pid.unwrap();
                    case.kill_native_process().await;
                    let successor = case.start_native_process().await;
                    assert_ne!(successor, killed);
                    eprintln!(
                        "cancelled original paired={paired} unadmitted={unadmitted} fault={fault} killed={killed} successor={successor}"
                    );
                    assert_eq!(writer(&case, paired).await.status, 400);
                    case.backend
                        .original_unavailable
                        .store(false, Ordering::SeqCst);
                }
                tokio::time::timeout(Duration::from_secs(8), async {
                    loop {
                        let response = writer(&case, paired).await;
                        if response.status == 200 {
                            let outcome: openwebide_core::CommandOutcome =
                                serde_json::from_str(&response.body).unwrap();
                            assert!(outcome.is_success(), "{outcome:?}");
                            break;
                        }
                        assert_eq!(response.status, 400, "{}", response.body);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
                let public = case
                    .backend
                    .store
                    .language_action_recovery(
                        UserId::new(1),
                        1,
                        &ActionRecoveryRequest::Load {
                            id: Some(case.request.command.invocation.id.clone()),
                        },
                        200,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(public.command.phase, ActionPhase::Cancelled);
                assert!(!public.pending());
                assert!(public.outcome.is_none() && public.acknowledgement.is_none());
                assert!(!case.root.join("executions").exists());
                assert_eq!(
                    std::fs::read_to_string(case.root.join("main.notes")).unwrap(),
                    SOURCE
                );
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                super::settlement::retired(case.recovery_home()).await;
                let calls = case.backend.original_calls.load(Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(2200)).await;
                assert_eq!(case.backend.original_calls.load(Ordering::SeqCst), calls);
                case.close().await;
            }
        }
    }
}
