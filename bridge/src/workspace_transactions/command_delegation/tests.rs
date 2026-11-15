use super::*;
use crate::runs::backend_client::BackendClient;
use openwebide_core::{
    editor::EditorRecoveryRoot,
    plugins::languages::intelligence::actions::{
        ActionPhase, CommandAdmission, CommandAuthority, CommandInvocation, CommandOutcome,
        CommandRecoveryScope, CommandWorkerOperation,
    },
    workspace_transaction::{
        RecoveryAuthority, WorkspaceActivity, WorkspaceCommandRecovery, WorkspaceTransactionScope,
    },
};
fn fixture(
    mode: WorkspaceMode,
) -> (
    WorkspaceCommandRecovery,
    WorkspaceTransactionWorker,
    WorkspaceJournalDelegation,
) {
    let scope = WorkspaceTransactionScope {
        id: "a".repeat(32),
        generation: "b".repeat(32),
        revision: 3,
        root_identity: "c".repeat(64),
        root: EditorRecoveryRoot { mode, path: None },
        worker: None,
        accepted_language_review_digest: None,
    };
    let recovery = WorkspaceCommandRecovery {
        scope: scope.clone(),
        activity: WorkspaceActivity {
            file_recovery: false,
            language_command: true,
        },
        command: CommandRecoveryScope {
            invocation: CommandInvocation {
                id: scope.id.clone(),
                client: "window".into(),
                command: "d".repeat(32),
                server: "original".into(),
                origin: "e".repeat(64),
                review: "f".repeat(64),
                approval: "0".repeat(64),
            },
            revision: 2,
            phase: ActionPhase::Running,
            outcome_digest: None,
            worker: None,
            native_settlement_pending: false,
            command_cleanup_confirmed: false,
        },
    };
    let worker = WorkspaceTransactionWorker {
        id: "1".repeat(32),
        secret: "2".repeat(32),
    };
    let delegation = WorkspaceJournalDelegation {
        id: "3".repeat(32),
        secret: "4".repeat(32),
        user: 7,
        project: 11,
        scope,
    };
    (recovery, worker, delegation)
}
async fn exchange(
    mode: WorkspaceMode,
    body: String,
    status: u16,
    operation: WorkspaceJournalOperation,
) -> (Result<WorkspaceJournalResponse, String>, String) {
    let (_, worker, delegation) = fixture(mode);
    let response = format!(
        "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, captured) =
        crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str())).await;
    let request = WorkspaceJournalRequest {
        user: 7,
        project: 11,
        id: delegation.scope.id.clone(),
        operation,
    };
    let result = match mode {
        WorkspaceMode::Local => {
            JournalBackend::request(
                &PairedCommandJournal::new(
                    format!("{url}/api"),
                    &delegation.scope.root_identity.clone(),
                    delegation,
                    worker,
                )
                .unwrap(),
                &request,
            )
            .await
        }
        WorkspaceMode::Remote => {
            BackendClient::new(
                format!("{url}/api"),
                "service-secret".into(),
                ReqwestHttpClient::default(),
            )
            .workspace_journal_service(&request)
            .await
        }
    };
    (result, captured.await.unwrap())
}
#[tokio::test]
async fn native_command_callbacks_validate_original_workers_outcomes_and_settlement_in_both_transports()
 {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (initial, worker, _) = fixture(mode);
        let mut claimed = initial.clone();
        claimed.command.worker = Some(worker.state());
        claimed.command.native_settlement_pending = true;
        let outcome = CommandOutcome::Completed {
            result: serde_json::json!("result".repeat(16384)),
        };
        outcome.validate().unwrap();
        let mut finished = claimed.clone();
        finished.command.revision += 1;
        finished.command.phase = ActionPhase::Completed;
        finished.command.outcome_digest = Some(openwebide_core::plugins::content_digest(
            &serde_json::to_vec(&outcome).unwrap(),
        ));
        finished.command.command_cleanup_confirmed = true;
        let mut settled = finished.clone();
        settled.command.worker.as_mut().unwrap().active = false;
        settled.command.native_settlement_pending = false;
        settled.activity.language_command = false;
        let mut uncertain = finished.clone();
        uncertain.command.phase = ActionPhase::Uncertain;
        let mut released = uncertain.clone();
        released.command.worker.as_mut().unwrap().active = false;
        for (operation, good) in [
            (
                CommandWorkerOperation::Claim {
                    expected: Box::new(initial),
                    worker: worker.clone(),
                    admission: CommandAdmission::Start {
                        authority: CommandAuthority {
                            grant: "5".repeat(32),
                            session_id: None,
                        },
                    },
                },
                claimed.clone(),
            ),
            (
                CommandWorkerOperation::Finish {
                    expected: Box::new(claimed),
                    worker: worker.clone(),
                    outcome,
                    cleaned: true,
                },
                finished.clone(),
            ),
            (
                CommandWorkerOperation::Release {
                    expected: Box::new(uncertain),
                    worker: worker.clone(),
                },
                released,
            ),
            (
                CommandWorkerOperation::Settle {
                    expected: Box::new(finished),
                    worker: worker.clone(),
                },
                settled,
            ),
        ] {
            for case in 0..14 {
                let mut changed = good.clone();
                match case {
                    0 | 8 | 9 | 10 => {}
                    1 => changed.scope.generation = "6".repeat(32),
                    2 => changed.scope.root_identity = "6".repeat(64),
                    3 => changed.command.invocation.approval = "6".repeat(64),
                    4 => changed.command.worker.as_mut().unwrap().id = "6".repeat(32),
                    5 => {
                        changed.command.worker.as_mut().unwrap().active ^= true;
                    }
                    6 => changed.command.native_settlement_pending ^= true,
                    7 => {
                        changed.command.outcome_digest = Some("6".repeat(64));
                        changed.command.command_cleanup_confirmed = false;
                    }
                    11 => changed.command.revision = 1,
                    12 => changed.command.worker = None,
                    13 => {
                        if changed.command.outcome_digest.is_some() {
                            changed.command.outcome_digest = Some("6".repeat(64));
                        } else {
                            changed.command.phase = ActionPhase::Approved;
                        }
                    }
                    _ => unreachable!(),
                }
                let body = if case == 9 {
                    "private-invalid-response-marker".to_string()
                } else if case == 10 {
                    "{\"error\":\"private-error-marker\"}".into()
                } else {
                    serde_json::to_string(&WorkspaceJournalResponse::CommandScope {
                        recovery: Some(Box::new(changed)),
                    })
                    .unwrap()
                        + &" ".repeat(if case == 8 {
                            RECOVERY_SCOPE_BODY_LIMIT
                        } else {
                            0
                        })
                };
                let (result, sent) = exchange(
                    mode,
                    body,
                    if case == 10 { 409 } else { 200 },
                    WorkspaceJournalOperation::Command {
                        operation: Box::new(operation.clone()),
                    },
                )
                .await;
                assert_eq!(
                    result.is_ok(),
                    case == 0,
                    "{mode:?} case={case} operation={:?} result={result:?}",
                    std::mem::discriminant(&operation)
                );
                if let Err(error) = &result {
                    assert!(!error.contains("private-error-marker"));
                    assert!(!error.contains("private-invalid-response-marker"));
                }
                if let Ok(reply) = result {
                    assert_eq!(reply.command_worker(&operation).unwrap(), &good);
                }
                if mode == WorkspaceMode::Local {
                    assert!(sent.starts_with("POST /api/language-commands/delegated HTTP/1.1"));
                    for header in ["authorization:", "cookie:", "origin:"] {
                        assert!(!sent.to_ascii_lowercase().contains(header));
                    }
                    assert!(!sent.contains("service-secret"));
                    let input: DelegatedCommandRequest =
                        serde_json::from_str(sent.split_once("\r\n\r\n").unwrap().1).unwrap();
                    input.validate().unwrap();
                } else {
                    assert!(sent.starts_with("POST /api/language-commands/service HTTP/1.1"));
                }
                if matches!(operation, CommandWorkerOperation::Finish { .. }) {
                    assert!(sent.len() > RECOVERY_SCOPE_BODY_LIMIT);
                }
            }
        }
    }
}
#[tokio::test]
async fn paired_command_scope_requires_the_original_issuer_and_revokes_missing_or_oversized_replies()
 {
    let (recovery, _, delegation) = fixture(WorkspaceMode::Local);
    let authority = RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
    for case in 0..5 {
        let mut value = serde_json::to_value(WorkspaceJournalResponse::CommandRecoveryScope {
            recovery: Box::new(recovery.clone()),
            authority: Box::new(authority.clone()),
        })
        .unwrap();
        match case {
            0 | 4 => {}
            1 => value["authority"]["user"] = serde_json::json!(8),
            2 => value["recovery"]["scope"]["generation"] = serde_json::json!("6".repeat(32)),
            3 => {
                value.as_object_mut().unwrap().remove("authority");
            }
            _ => unreachable!(),
        }
        let body = serde_json::to_string(&value).unwrap()
            + &" ".repeat(if case == 4 {
                RECOVERY_SCOPE_BODY_LIMIT
            } else {
                0
            });
        let (result, sent) = exchange(
            WorkspaceMode::Local,
            body,
            200,
            WorkspaceJournalOperation::CommandRecoveryScope,
        )
        .await;
        assert_eq!(result.is_ok(), case == 0);
        if let Ok(reply) = result {
            let (current, issuer) = reply
                .command_recovery(delegation.user, delegation.project, &delegation.scope.id)
                .unwrap();
            assert_eq!(*current, recovery);
            assert_eq!(*issuer, authority);
        }
        assert!(!sent.contains(&authority.secret));
    }
}
