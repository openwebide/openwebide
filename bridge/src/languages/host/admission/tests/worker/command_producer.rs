use super::*;
use crate::runs::{backend_client::BackendClient, http_client::ReqwestHttpClient};
use openwebide_core::workspace_transaction as transaction;
use protocol::actions::{
    AuthorizedCommand, CommandAuthority, CommandDelegationRequest, CommandDispatch,
    CommandOperation, CommandOutcome, CommandPreparation, CommandProgress, HostCommandRequest,
    PairedCommandJournalRequest,
};
use transaction::companion::{FolderAction, FolderReply};

#[tokio::test]
async fn durable_command_producer_finishes_and_settles_after_the_file_worker_releases_in_both_modes()
 {
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        for reject_before_dispatch in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let mut transactions = Transactions::new();
            transactions.set_test_recovery_home(Arc::new(home.path().into()));
            let mut fixture = Fixture::with_mode(
                if mode == WorkspaceMode::Local {
                    "paired/user:1"
                } else {
                    "user:1"
                },
                "inputs",
            );
            fixture.host = Host::with_runtime(
                crate::languages::host::MemoryLimits::default(),
                transactions.coordinator.clone(),
            )
            .unwrap();
            std::fs::write(
                fixture.root.path().join("settings-old.json"),
                "{\"original\":true}",
            )
            .unwrap();
            let paired_proof = if mode == WorkspaceMode::Local {
                Some(paired::prove(&transactions, &fixture, None).await)
            } else {
                None
            };
            let state = fixture.state().await;
            fixture.prepare(&state).await;
            let (fixture, state, backend, command) = store_review(fixture, state, true).await;
            *backend.reservation_home.lock().unwrap() = Some(home.path().into());
            let server = paired::serve(backend.clone()).await;
            if let Some((session, worker, _)) = paired_proof {
                paired::bind(&transactions, &fixture, &state, &session).await;
                let delegation = paired::delegate(&backend, &state, worker, 101).await;
                let reply = paired::call(
                    &transactions,
                    &fixture,
                    FolderAction::Execute {
                        session,
                        api_url: server.url.clone(),
                        delegation,
                        request: command.request.clone(),
                    },
                )
                .await;
                let FolderReply::Executed { outcome } = reply else {
                    panic!("Missing original file outcome")
                };
                assert!(outcome.error.is_none(), "{outcome:?}");
                assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
            } else {
                let outcome = transactions
                    .execute_reviewed(
                        fixture.scope.root.parent().unwrap().into(),
                        command.clone(),
                        backend.clone(),
                        Some(fixture.host.clone()),
                    )
                    .await
                    .unwrap();
                assert!(outcome.error.is_none(), "{outcome:?}");
                assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
            }
            let pending = backend
                .store
                .workspace_command_recovery(UserId::new(1), 1, &state.journal.id)
                .await
                .unwrap()
                .unwrap();
            assert!(!pending.activity.file_recovery && pending.activity.language_command);
            assert!(pending.scope.worker.as_ref().is_some_and(|w| !w.active));
            let folder = crate::folder_coordination::Folder::capture(&fixture.scope.root).unwrap();
            assert!(
                transactions
                    .coordinator
                    .acquire(folder.clone())
                    .await
                    .is_err()
            );
            let approval = backend
                .store
                .language_action(UserId::new(1), 1, Some(&state.journal.id))
                .await
                .unwrap()
                .unwrap()
                .approval;
            let mut synchronized = fixture.input.clone();
            synchronized.request += 1;
            synchronized.revision += 1;
            synchronized.source = state
                .editor
                .after
                .iter()
                .find(|f| f.path == "main.notes")
                .unwrap()
                .document
                .as_ref()
                .unwrap()
                .restore()
                .unwrap()
                .text()
                .into();
            synchronized.source_digest = source_digest(&synchronized.source);
            synchronized.query = Query::SynchronizeAction {
                action: ActionSynchronization::from_approval(&approval).unwrap(),
                close: false,
            };
            fixture
                .host
                .query_package(fixture.scope.clone(), &synchronized, &fixture.prepared)
                .await
                .unwrap();
            let call = protocol::Call {
                prepared: approval.prepared.clone(),
                input: approval.input.clone(),
                grant: "d".repeat(32),
                session_id: None,
            };
            backend
                .store
                .claim_language_action(UserId::new(1), 1, &approval.id, 1, &call, 102)
                .await
                .unwrap();
            backend.now.store(102, Ordering::SeqCst);
            let authorized = backend
                .store
                .authorize_language_command(
                    UserId::new(1),
                    1,
                    &CommandDispatch {
                        id: approval.id.clone(),
                        operation: CommandOperation::Start,
                        call: Some(Box::new(call.clone())),
                    },
                    102,
                )
                .await
                .unwrap();
            let invocation = authorized.invocation.clone();
            assert!(authorized.progress.is_none());
            let mut request = HostCommandRequest {
                user: Some(1),
                root: fixture.scope.root.to_string_lossy().into(),
                command: AuthorizedCommand {
                    user: 1,
                    project: 1,
                    selection: authorized.selection,
                    invocation: invocation.clone(),
                    operation: CommandOperation::Start,
                    progress: None,
                },
                authority: Some(CommandAuthority {
                    grant: call.grant,
                    session_id: None,
                }),
                journal: None,
            };
            let CommandPreparation::Worker { worker } = fixture
                .host
                .prepare_command(fixture.scope.clone(), &invocation)
                .await
                .unwrap()
            else {
                panic!("Command unexpectedly claimed")
            };
            assert_eq!(
                backend.reserved_worker.lock().unwrap().as_ref(),
                Some(&worker)
            );
            let service = if mode == WorkspaceMode::Local {
                let delegation = backend
                    .store
                    .delegate_language_command(
                        UserId::new(1),
                        1,
                        &CommandDelegationRequest {
                            id: approval.id.clone(),
                            worker,
                        },
                        102,
                    )
                    .await
                    .unwrap();
                request.journal = Some(PairedCommandJournalRequest {
                    api_url: server.url.clone(),
                    delegation,
                });
                None
            } else {
                Some(Arc::new(BackendClient::new(
                    server.url.clone(),
                    "native-command-test-service".into(),
                    ReqwestHttpClient::default(),
                )))
            };
            if reject_before_dispatch {
                request.authority.as_mut().unwrap().grant = "e".repeat(32);
            }
            // Backend response hints echoed by a client cannot complete or
            // authorize native execution. The original worker still decides.
            request.command.progress = Some(CommandProgress::Finished {
                outcome: CommandOutcome::Completed {
                    result: serde_json::json!({"forged":true}),
                },
            });
            let first = fixture
                .host
                .start_owned_command(fixture.scope.clone(), request.clone(), service.clone())
                .await
                .unwrap();
            assert_eq!(first, CommandProgress::Running);
            if reject_before_dispatch {
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if let CommandProgress::Finished { outcome } = fixture
                            .host
                            .command_progress(fixture.scope.clone(), &invocation)
                            .await
                            .unwrap()
                        {
                            assert!(
                                matches!(outcome, CommandOutcome::Rejected { .. }),
                                "{outcome:?}"
                            );
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                assert!(!fixture.root.path().join("executions").exists());
                let original = backend
                    .store
                    .workspace_command_recovery(UserId::new(1), 1, &approval.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    original.command.native_settlement_pending
                        && original.command.command_cleanup_confirmed
                );
                assert!(
                    transactions
                        .coordinator
                        .acquire(folder.clone())
                        .await
                        .is_err()
                );
                backend
                    .store
                    .acknowledge_language_action(
                        UserId::new(1),
                        1,
                        &approval.id,
                        original.command.revision,
                        "Reviewed the original applied files; no command was sent",
                        103,
                    )
                    .await
                    .unwrap();
            }
            // Observe persisted backend state rather than keep an HTTP/native
            // completion future alive. The owned producer publishes independently.
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    let current = backend
                        .store
                        .workspace_command_recovery(UserId::new(1), 1, &approval.id)
                        .await
                        .unwrap()
                        .unwrap();
                    if current.command.phase.terminal()
                        && !current.command.native_settlement_pending
                    {
                        break;
                    }
                    if !reject_before_dispatch
                        && let CommandProgress::Finished { outcome } = fixture
                            .host
                            .command_progress(fixture.scope.clone(), &invocation)
                            .await
                            .unwrap()
                    {
                        panic!(
                            "Producer ended before settlement: {outcome:?}; backend={current:?}"
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let outcome = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let CommandProgress::Finished { outcome } = fixture
                        .host
                        .command_progress(fixture.scope.clone(), &invocation)
                        .await
                        .unwrap()
                    {
                        break outcome;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                matches!(outcome, CommandOutcome::Completed { .. }),
                !reject_before_dispatch,
                "{outcome:?}"
            );
            assert!(
                backend
                    .store
                    .workspace_activity(UserId::new(1), 1)
                    .await
                    .unwrap()
                    .ensure_idle()
                    .is_ok()
            );
            drop(transactions.coordinator.acquire(folder).await.unwrap());
            for operation in [CommandOperation::Status, CommandOperation::Stop] {
                let control = backend
                    .store
                    .authorize_language_command(
                        UserId::new(1),
                        1,
                        &CommandDispatch {
                            id: approval.id.clone(),
                            operation,
                            call: None,
                        },
                        1_000_000,
                    )
                    .await
                    .unwrap();
                assert!(control.selection.is_none());
                assert_eq!(
                    fixture
                        .host
                        .control_command(&fixture.scope.owner, 1, &invocation, operation)
                        .await
                        .unwrap(),
                    CommandProgress::Finished {
                        outcome: outcome.clone()
                    }
                );
                assert_eq!(control.invocation, invocation);
                assert_eq!(
                    control.progress,
                    Some(CommandProgress::Finished {
                        outcome: outcome.clone()
                    })
                );
            }
            assert!(
                backend
                    .store
                    .authorize_language_command(
                        UserId::new(1),
                        1,
                        &CommandDispatch {
                            id: approval.id.clone(),
                            operation: CommandOperation::Start,
                            call: None
                        },
                        1_000_000
                    )
                    .await
                    .is_err()
            );
            let duplicate = fixture
                .host
                .start_owned_command(fixture.scope.clone(), request, service)
                .await
                .unwrap();
            assert_eq!(duplicate, CommandProgress::Finished { outcome });
            if reject_before_dispatch {
                assert!(!fixture.root.path().join("executions").exists());
            } else {
                assert_eq!(
                    std::fs::read_to_string(fixture.root.path().join("executions")).unwrap(),
                    "executed\n"
                );
            }
            assert!(
                fixture
                    .host
                    .inner
                    .entries
                    .lock()
                    .unwrap()
                    .values()
                    .all(|entry| !*entry.stopped.borrow())
            );
        }
    }
}
