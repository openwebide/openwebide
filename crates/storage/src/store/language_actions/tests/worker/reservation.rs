use super::*;
mod cancellation;
mod outcome;
use openwebide_core::{
    plugins::languages::intelligence::actions::{CommandReservation, OriginalCommandRequest},
    workspace_transaction::{
        DelegateWorkspaceTransaction, DelegatedWorkspaceJournalOperation as FileOperation,
        DelegatedWorkspaceJournalRequest as FileRequest, RecoveryAuthority,
        WorkspaceTransactionScope,
    },
};

#[test]
fn original_reserved_command_cleanup_survives_file_release_and_lost_acknowledgements() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            case.approve().await;
            let files = Files::new(&case, mode).await;
            let channel = Channel::new(&case, mode, 'a', 2).await;
            let before = case
                .store
                .workspace_command_recovery(case.user, case.project, &case.approval.id)
                .await
                .unwrap()
                .unwrap();
            let authority = RecoveryAuthority::for_scope(
                case.user.get(),
                case.project,
                &before.scope,
                "A".repeat(43),
            )
            .unwrap();
            files
                .admit(
                    &case,
                    Some(&CommandReservation {
                        expected: Box::new(before),
                        worker: channel.worker.clone(),
                    }),
                )
                .await
                .unwrap();
            files.commit_and_release(&case).await;
            // The private issuer is verified by the HTTP transport. This
            // contract tests Store's original-worker/CAS policy independently.
            let mut input = OriginalCommandRequest {
                authority,
                worker: channel.worker.clone(),
                operation: CommandJournalOperation::Scope,
            };
            let after_files = case
                .store
                .workspace_command_recovery(case.user, case.project, &case.approval.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(after_files.command.phase, ActionPhase::Approved);
            assert!(!after_files.scope.worker.as_ref().unwrap().active);
            case.store
                .set_user_setting(
                    case.user,
                    "language_settings",
                    "{revoked provider configuration",
                )
                .await
                .unwrap();
            let operation = CommandWorkerOperation::RejectUnclaimed {
                expected: Box::new(after_files),
                worker: channel.worker.clone(),
                message: "Stopped before dispatch".into(),
            };
            input.operation = CommandJournalOperation::Worker {
                operation: Box::new(operation.clone()),
            };
            let mut forged = input.clone();
            forged.worker.secret = "f".repeat(32);
            assert!(
                case.store
                    .original_language_command(&forged, 1_000_000)
                    .await
                    .is_err()
            );
            let published = case
                .store
                .original_language_command(&input, 1_000_000)
                .await
                .unwrap()
                .command_worker(&operation)
                .unwrap()
                .clone();
            assert_eq!(published.command.phase, ActionPhase::Rejected);
            assert!(published.command.native_settlement_pending);
            assert_eq!(
                case.store
                    .original_language_command(&input, 1_000_001)
                    .await
                    .unwrap()
                    .command_worker(&operation)
                    .unwrap(),
                &published,
                "A lost rejection ACK must join the original result"
            );
            case.store
                .acknowledge_language_action(
                    case.user,
                    case.project,
                    &case.approval.id,
                    published.command.revision,
                    "Reviewed original applied files",
                    1_000_002,
                )
                .await
                .unwrap();
            let current = case
                .store
                .workspace_command_recovery(case.user, case.project, &case.approval.id)
                .await
                .unwrap()
                .unwrap();
            let operation = CommandWorkerOperation::Settle {
                expected: Box::new(current),
                worker: channel.worker.clone(),
            };
            input.operation = CommandJournalOperation::Worker {
                operation: Box::new(operation.clone()),
            };
            let settled = case
                .store
                .original_language_command(&input, 1_000_003)
                .await
                .unwrap()
                .command_worker(&operation)
                .unwrap()
                .clone();
            assert!(!settled.command.native_settlement_pending);
            assert!(!settled.command.worker.as_ref().unwrap().active);
            assert_eq!(
                case.store
                    .original_language_command(&input, 1_000_004)
                    .await
                    .unwrap()
                    .command_worker(&operation)
                    .unwrap(),
                &settled,
                "A lost final settlement ACK must join without replacing its worker"
            );
            input.operation = CommandJournalOperation::Scope;
            let WorkspaceJournalResponse::CommandScope {
                recovery: Some(observed),
            } = case
                .store
                .original_language_command(&input, 1_000_005)
                .await
                .unwrap()
            else {
                panic!("Missing original settlement receipt")
            };
            assert_eq!(*observed, settled);
            case.store
                .workspace_activity(case.user, case.project)
                .await
                .unwrap()
                .ensure_idle()
                .unwrap();
        }
    });
}

#[test]
fn legacy_file_workers_and_intents_recover_without_new_command_permission() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for prior_worker in [false, true] {
                let case = Scenario::new(mode).await;
                case.approve().await;
                if prior_worker {
                    // Persisted admission from before command reservations.
                    // Its language-review digest was not retained by that version.
                    let old = WorkspaceTransactionWorker {
                        id: "9".repeat(32),
                        secret: "8".repeat(32),
                    };
                    case.store.db.execute("UPDATE workspace_transactions SET worker_id=?,worker_digest=? WHERE user_id=? AND project_id=? AND id=?", &[
                        DbValue::Text(old.id.clone()),DbValue::Text(old.secret_digest()),DbValue::Int(case.user.get()),DbValue::Int(case.project),DbValue::Text(case.approval.id.clone()),
                    ]).await.unwrap();
                } else {
                    // Legacy accepted intent with no private worker columns.
                    case.store
                        .advance_workspace_transaction(
                            case.user,
                            case.project,
                            &AdvanceWorkspaceTransaction {
                                id: case.approval.id.clone(),
                                generation: case.generation.clone(),
                                revision: 1,
                                progress: TransactionProgress {
                                    phase: TransactionPhase::Applying,
                                    ..Default::default()
                                },
                            },
                            2,
                        )
                        .await
                        .unwrap();
                }
                let before = case
                    .store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap()
                    .unwrap();
                case.store
                    .set_user_setting(case.user, "language_settings", "{revoked configuration")
                    .await
                    .unwrap();
                let files = Files::new(&case, mode).await;
                let accepted = files.admit(&case, None).await.unwrap();
                assert_eq!(accepted.worker, Some(files.worker.state()));
                let after = case
                    .store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(after.command, before.command);
            }
        }
    });
}

struct Files {
    worker: WorkspaceTransactionWorker,
    grant: Option<WorkspaceJournalDelegation>,
    scope: WorkspaceTransactionScope,
}
impl Files {
    async fn new(case: &Scenario, mode: WorkspaceMode) -> Self {
        let scope = case
            .store
            .workspace_transaction_scope(case.user, case.project, &case.approval.id)
            .await
            .unwrap()
            .unwrap();
        let worker = WorkspaceTransactionWorker {
            id: "1".repeat(32),
            secret: "2".repeat(32),
        };
        let grant = if mode == WorkspaceMode::Local {
            Some(
                case.store
                    .delegate_workspace_transaction(
                        case.user,
                        case.project,
                        &case.approval.id,
                        &DelegateWorkspaceTransaction {
                            generation: scope.generation.clone(),
                            revision: scope.revision,
                            worker: worker.delegation_key(),
                        },
                        2,
                    )
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        Self {
            worker,
            grant,
            scope,
        }
    }
    async fn admit(
        &self,
        case: &Scenario,
        command: Option<&CommandReservation>,
    ) -> Result<WorkspaceTransactionScope, StorageError> {
        if let Some(grant) = &self.grant {
            let input = FileRequest {
                delegation: grant.clone(),
                worker: self.worker.clone(),
                operation: FileOperation::Admit {
                    review_digest: None,
                    language_grant: None,
                    command: command.cloned().map(Box::new),
                },
            };
            let input: FileRequest =
                serde_json::from_slice(&serde_json::to_vec(&input).unwrap()).unwrap();
            let WorkspaceJournalResponse::WorkerClaimed { scope } =
                case.store.delegated_workspace_journal(&input, 2).await?
            else {
                panic!("Missing file admission")
            };
            Ok(scope)
        } else {
            case.store
                .claim_workspace_transaction_worker_compound(
                    case.user,
                    case.project,
                    &self.scope,
                    &self.worker,
                    (None, command),
                    2,
                )
                .await
        }
    }
    async fn commit_and_release(&self, case: &Scenario) {
        self.finish_and_release(
            case,
            &[
                TransactionProgress {
                    phase: TransactionPhase::Applying,
                    ..Default::default()
                },
                TransactionProgress {
                    phase: TransactionPhase::Committed,
                    ..Default::default()
                },
            ],
        )
        .await;
    }
    async fn finish_and_release(&self, case: &Scenario, steps: &[TransactionProgress]) {
        for (index, progress) in steps.iter().enumerate() {
            let revision = u64::try_from(index).unwrap() + 1;
            let progress = progress.clone();
            if let Some(grant) = &self.grant {
                case.store
                    .delegated_workspace_journal(
                        &FileRequest {
                            delegation: grant.clone(),
                            worker: self.worker.clone(),
                            operation: FileOperation::Advance { revision, progress },
                        },
                        3,
                    )
                    .await
                    .unwrap();
            } else {
                case.store
                    .advance_workspace_transaction_for_worker(
                        case.user,
                        case.project,
                        &AdvanceWorkspaceTransaction {
                            id: case.approval.id.clone(),
                            generation: case.generation.clone(),
                            revision,
                            progress,
                        },
                        Some(&self.worker),
                        3,
                    )
                    .await
                    .unwrap();
            }
        }
        if let Some(grant) = &self.grant {
            case.store
                .delegated_workspace_journal(
                    &FileRequest {
                        delegation: grant.clone(),
                        worker: self.worker.clone(),
                        operation: FileOperation::Release,
                    },
                    3,
                )
                .await
                .unwrap();
        } else {
            case.store
                .release_workspace_transaction_worker(
                    case.user,
                    case.project,
                    &case.approval.id,
                    &case.generation,
                    &self.worker,
                    3,
                )
                .await
                .unwrap();
        }
    }
}

#[test]
fn file_admission_reserves_the_original_command_atomically_without_dispatch_permission_in_both_modes()
 {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for admit_start in [false, true] {
                let case = Scenario::new(mode).await;
                case.approve().await;
                let files = Files::new(&case, mode).await;
                let mut channel = Channel::new(&case, mode, 'a', 2).await;
                let original = case
                    .store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap()
                    .unwrap();
                let reservation = CommandReservation {
                    expected: Box::new(original.clone()),
                    worker: channel.worker.clone(),
                };
                let mut wrong = reservation.clone();
                wrong.expected.command.invocation.server = "replacement".into();
                for rejected in [None, Some(&wrong)] {
                    assert!(files.admit(&case, rejected).await.is_err());
                    assert_eq!(
                        case.store
                            .workspace_command_recovery(case.user, case.project, &case.approval.id)
                            .await
                            .unwrap()
                            .unwrap(),
                        original
                    );
                }
                let accepted = files.admit(&case, Some(&reservation)).await.unwrap();
                assert_eq!(accepted.worker, Some(files.worker.state()));
                let reserved = case
                    .store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(reserved.command.phase, ActionPhase::Approved);
                assert_eq!(reserved.command.worker, Some(channel.worker.state()));
                assert!(
                    reserved.command.native_settlement_pending && reserved.activity.file_recovery
                );
                assert!(
                    channel
                        .send(&case, channel.claim(&reserved, &case), 2)
                        .await
                        .is_err()
                );
                assert_eq!(
                    files.admit(&case, Some(&reservation)).await.unwrap(),
                    accepted
                );
                let mut substitute = reservation.clone();
                substitute.worker.secret = "f".repeat(32);
                assert!(files.admit(&case, Some(&substitute)).await.is_err());
                assert_eq!(
                    case.store
                        .workspace_command_recovery(case.user, case.project, &case.approval.id)
                        .await
                        .unwrap()
                        .unwrap(),
                    reserved
                );
                files.commit_and_release(&case).await;
                let after_files = case
                    .store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    !after_files.activity.file_recovery && after_files.activity.language_command
                );
                assert!(!after_files.scope.worker.as_ref().unwrap().active);
                assert_eq!(after_files.command.worker, Some(channel.worker.state()));
                // A fresh bootstrap may name the reserved commitment, but it does
                // not replace the private worker or grant permission to dispatch.
                if mode == WorkspaceMode::Local {
                    channel.grant = Some(
                        case.store
                            .delegate_language_command(
                                case.user,
                                case.project,
                                &CommandDelegationRequest {
                                    id: case.approval.id.clone(),
                                    worker: channel.worker.delegation_key(),
                                },
                                4,
                            )
                            .await
                            .unwrap(),
                    );
                }
                case.claim().await.unwrap();
                let running = channel.read(&case, 4).await.unwrap();
                let mut denied = channel.claim(&running, &case);
                if let CommandWorkerOperation::Claim {
                    admission: CommandAdmission::Start { authority },
                    ..
                } = &mut denied
                {
                    authority.grant = "0".repeat(32);
                }
                assert!(channel.send(&case, denied, 4).await.is_err());
                assert_eq!(channel.read(&case, 4).await.unwrap(), running);
                let admitted = if admit_start {
                    channel
                        .send(&case, channel.claim(&running, &case), 4)
                        .await
                        .unwrap()
                } else {
                    running
                };
                // A rejected Start must not expire the reserved worker's cleanup
                // callback or require another user grant to record no RPC.
                let cleanup_time = 1_000_000;
                let outcome = CommandOutcome::Rejected {
                    message: "No command was sent".into(),
                };
                let finished = channel
                    .send(
                        &case,
                        CommandWorkerOperation::Finish {
                            expected: Box::new(admitted),
                            worker: channel.worker.clone(),
                            outcome,
                            cleaned: true,
                        },
                        cleanup_time,
                    )
                    .await
                    .unwrap();
                case.store
                    .acknowledge_language_action(
                        case.user,
                        case.project,
                        &case.approval.id,
                        finished.command.revision,
                        "Reviewed the applied edit and original command outcome",
                        cleanup_time,
                    )
                    .await
                    .unwrap();
                let acknowledged = channel.read(&case, cleanup_time).await.unwrap();
                let settled = channel
                    .send(
                        &case,
                        CommandWorkerOperation::Settle {
                            expected: Box::new(acknowledged),
                            worker: channel.worker.clone(),
                        },
                        cleanup_time,
                    )
                    .await
                    .unwrap();
                assert!(!settled.command.native_settlement_pending);
                case.store
                    .workspace_activity(case.user, case.project)
                    .await
                    .unwrap()
                    .ensure_idle()
                    .unwrap();
            }
        }
    });
}
