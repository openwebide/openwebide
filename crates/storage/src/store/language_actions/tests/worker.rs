use super::*;
mod reservation;
mod settlement;
mod settlement_upgrade;
use openwebide_core::{
    plugins::languages::intelligence::actions::{
        CommandAdmission, CommandAuthority, CommandDelegationRequest, CommandJournalOperation,
        CommandWorkerOperation, DelegatedCommandRequest,
    },
    workspace_transaction::{
        WorkspaceCommandRecovery, WorkspaceJournalDelegation, WorkspaceJournalResponse,
        WorkspaceTransactionWorker,
    },
};
struct Channel {
    worker: WorkspaceTransactionWorker,
    grant: Option<WorkspaceJournalDelegation>,
}
impl Channel {
    async fn new(case: &Scenario, mode: WorkspaceMode, tag: char, now: i64) -> Self {
        let worker = WorkspaceTransactionWorker {
            id: tag.to_string().repeat(32),
            secret: format!("{:032x}", u32::from(tag) + 17),
        };
        let grant = if mode == WorkspaceMode::Local {
            Some(
                case.store
                    .delegate_language_command(
                        case.user,
                        case.project,
                        &CommandDelegationRequest {
                            id: case.approval.id.clone(),
                            worker: worker.delegation_key(),
                        },
                        now,
                    )
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        Self { worker, grant }
    }
    async fn read(
        &self,
        case: &Scenario,
        now: i64,
    ) -> Result<WorkspaceCommandRecovery, StorageError> {
        if let Some(grant) = &self.grant {
            let WorkspaceJournalResponse::CommandScope {
                recovery: Some(value),
            } = case
                .store
                .delegated_language_command(
                    &DelegatedCommandRequest {
                        delegation: grant.clone(),
                        worker: self.worker.clone(),
                        operation: CommandJournalOperation::Scope,
                    },
                    now,
                )
                .await?
            else {
                panic!("Missing original command scope")
            };
            Ok(*value)
        } else {
            case.store
                .workspace_command_recovery(case.user, case.project, &case.approval.id)
                .await?
                .ok_or_else(|| StorageError::Conflict("Missing original command".into()))
        }
    }
    async fn send(
        &self,
        case: &Scenario,
        operation: CommandWorkerOperation,
        now: i64,
    ) -> Result<WorkspaceCommandRecovery, StorageError> {
        let operation: CommandWorkerOperation =
            serde_json::from_slice(&serde_json::to_vec(&operation).unwrap()).unwrap();
        let result = if let Some(grant) = &self.grant {
            let WorkspaceJournalResponse::CommandScope {
                recovery: Some(value),
            } = case
                .store
                .delegated_language_command(
                    &DelegatedCommandRequest {
                        delegation: grant.clone(),
                        worker: self.worker.clone(),
                        operation: CommandJournalOperation::Worker {
                            operation: Box::new(operation.clone()),
                        },
                    },
                    now,
                )
                .await?
            else {
                panic!("Missing original command acknowledgement")
            };
            Ok(*value)
        } else {
            case.store
                .native_language_command(
                    case.user,
                    case.project,
                    &case.approval.id,
                    &operation,
                    now,
                )
                .await
        };
        if let Ok(recovery) = &result {
            WorkspaceJournalResponse::CommandScope {
                recovery: Some(Box::new(recovery.clone())),
            }
            .command_worker(&operation)
            .unwrap();
        }
        result
    }
    fn claim(
        &self,
        expected: &WorkspaceCommandRecovery,
        case: &Scenario,
    ) -> CommandWorkerOperation {
        CommandWorkerOperation::Claim {
            expected: Box::new(expected.clone()),
            worker: self.worker.clone(),
            admission: CommandAdmission::Start {
                authority: CommandAuthority {
                    grant: case.call.grant.clone(),
                    session_id: case.call.session_id,
                },
            },
        }
    }
}
async fn ready(mode: WorkspaceMode) -> Scenario {
    let case = Scenario::new(mode).await;
    case.approve().await;
    case.commit().await;
    case.claim().await.unwrap();
    case
}

#[test]
fn native_commands_outlive_file_workers_and_publish_only_their_original_outcome() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = ready(mode).await;
            let file = WorkspaceTransactionWorker {
                id: "1".repeat(32),
                secret: "2".repeat(32),
            };
            let scope = case
                .store
                .workspace_transaction_scope(case.user, case.project, &case.approval.id)
                .await
                .unwrap()
                .unwrap();
            case.store
                .claim_workspace_transaction_worker_reviewed(
                    case.user,
                    case.project,
                    &scope,
                    &file,
                    None,
                    4,
                )
                .await
                .unwrap();
            let channel = Channel::new(&case, mode, 'a', 4).await;
            let before = channel.read(&case, 4).await.unwrap();
            let admitted = channel
                .send(&case, channel.claim(&before, &case), 4)
                .await
                .unwrap();
            assert_eq!(admitted.command.worker, Some(channel.worker.state()));
            assert!(admitted.command.native_settlement_pending);
            assert!(!admitted.activity.file_recovery);
            // A lost claim acknowledgement joins its exact active worker, even
            // after the short-lived manual grant and bootstrap have expired.
            assert_eq!(
                channel
                    .send(&case, channel.claim(&before, &case), 1_000_000)
                    .await
                    .unwrap(),
                admitted
            );
            case.store
                .release_workspace_transaction_worker(
                    case.user,
                    case.project,
                    &case.approval.id,
                    &scope.generation,
                    &file,
                    5,
                )
                .await
                .unwrap();
            let current = channel.read(&case, 1_000_000).await.unwrap();
            assert!(!current.scope.worker.as_ref().unwrap().active);
            assert!(current.command.worker.as_ref().unwrap().active);
            if mode == WorkspaceMode::Local {
                // Issuing another control bootstrap must not prune the active
                // command grant merely because its file journal is terminal.
                let _control = Channel::new(&case, mode, 'b', 1_000_000).await;
                assert!(channel.read(&case, 1_000_000).await.is_ok());
            }
            let outcome = CommandOutcome::Completed {
                result: json!({"original":"文😀"}),
            };
            assert!(
                case.store
                    .finish_language_action(
                        case.user,
                        case.project,
                        &case.approval.id,
                        current.command.revision,
                        &outcome,
                        6
                    )
                    .await
                    .is_err()
            );
            assert!(
                channel
                    .send(
                        &case,
                        CommandWorkerOperation::Finish {
                            expected: Box::new(current.clone()),
                            worker: channel.worker.clone(),
                            outcome: outcome.clone(),
                            cleaned: false
                        },
                        6
                    )
                    .await
                    .is_err()
            );
            case.store.db.execute("UPDATE language_actions SET approval='unreadable provider/source',outcome='unreadable old outcome'",&[]).await.unwrap();
            let finished = channel
                .send(
                    &case,
                    CommandWorkerOperation::Finish {
                        expected: Box::new(current.clone()),
                        worker: channel.worker.clone(),
                        outcome: outcome.clone(),
                        cleaned: true,
                    },
                    7,
                )
                .await
                .unwrap();
            assert_eq!(finished.command.phase, ActionPhase::Completed);
            assert!(finished.command.command_cleanup_confirmed);
            assert!(finished.activity.language_command);
            assert!(
                case.store
                    .workspace_activity(case.user, case.project)
                    .await
                    .unwrap()
                    .ensure_idle()
                    .is_err()
            );
            assert!(
                case.store
                    .delete_project(case.project, case.user)
                    .await
                    .is_err()
            );
            assert_eq!(
                channel
                    .send(
                        &case,
                        CommandWorkerOperation::Finish {
                            expected: Box::new(current.clone()),
                            worker: channel.worker.clone(),
                            outcome: outcome.clone(),
                            cleaned: true
                        },
                        8
                    )
                    .await
                    .unwrap(),
                finished
            );
            assert!(
                channel
                    .send(
                        &case,
                        CommandWorkerOperation::Finish {
                            expected: Box::new(finished.clone()),
                            worker: channel.worker.clone(),
                            outcome: CommandOutcome::Completed {
                                result: json!("replacement")
                            },
                            cleaned: true
                        },
                        8
                    )
                    .await
                    .is_err()
            );
            let settled = channel
                .send(
                    &case,
                    CommandWorkerOperation::Settle {
                        expected: Box::new(finished.clone()),
                        worker: channel.worker.clone(),
                    },
                    9,
                )
                .await
                .unwrap();
            assert!(!settled.command.native_settlement_pending);
            assert!(!settled.command.worker.as_ref().unwrap().active);
            assert!(settled.activity.ensure_idle().is_ok());
            assert_eq!(
                channel
                    .send(
                        &case,
                        CommandWorkerOperation::Settle {
                            expected: Box::new(finished),
                            worker: channel.worker.clone()
                        },
                        10
                    )
                    .await
                    .unwrap(),
                settled
            );
            if mode == WorkspaceMode::Local {
                assert!(channel.read(&case, 10).await.is_err());
            }
            assert!(
                channel
                    .send(&case, channel.claim(&settled, &case), 10)
                    .await
                    .is_err()
            );
            let next = Channel::new(&case, mode, 'c', 10).await;
            assert!(
                next.send(&case, next.claim(&settled, &case), 10)
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn uncertain_native_commands_require_positive_cleanup_and_manual_review_before_settlement() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = ready(mode).await;
            let channel = Channel::new(&case, mode, 'a', 4).await;
            let initial = channel.read(&case, 4).await.unwrap();
            let claimed = channel
                .send(&case, channel.claim(&initial, &case), 4)
                .await
                .unwrap();
            let outcome = CommandOutcome::Uncertain {
                message: "Original RPC response lost".into(),
            };
            let uncertain = channel
                .send(
                    &case,
                    CommandWorkerOperation::Finish {
                        expected: Box::new(claimed.clone()),
                        worker: channel.worker.clone(),
                        outcome: outcome.clone(),
                        cleaned: false,
                    },
                    5,
                )
                .await
                .unwrap();
            assert!(
                case.store
                    .acknowledge_language_action(
                        case.user,
                        case.project,
                        &case.approval.id,
                        uncertain.command.revision,
                        "Reviewed effects",
                        6
                    )
                    .await
                    .is_err()
            );
            assert!(
                channel
                    .send(
                        &case,
                        CommandWorkerOperation::Release {
                            expected: Box::new(uncertain.clone()),
                            worker: channel.worker.clone()
                        },
                        6
                    )
                    .await
                    .is_err()
            );
            assert!(
                channel
                    .send(
                        &case,
                        CommandWorkerOperation::Settle {
                            expected: Box::new(uncertain),
                            worker: channel.worker.clone()
                        },
                        6
                    )
                    .await
                    .is_err()
            );
            let cleaned = channel
                .send(
                    &case,
                    CommandWorkerOperation::Finish {
                        expected: Box::new(claimed),
                        worker: channel.worker.clone(),
                        outcome,
                        cleaned: true,
                    },
                    7,
                )
                .await
                .unwrap();
            let released = channel
                .send(
                    &case,
                    CommandWorkerOperation::Release {
                        expected: Box::new(cleaned),
                        worker: channel.worker.clone(),
                    },
                    8,
                )
                .await
                .unwrap();
            assert!(released.command.native_settlement_pending);
            assert!(!released.command.worker.as_ref().unwrap().active);
            if mode == WorkspaceMode::Local {
                assert!(channel.read(&case, 8).await.is_err());
            }
            case.store
                .acknowledge_language_action(
                    case.user,
                    case.project,
                    &case.approval.id,
                    released.command.revision,
                    "Reviewed original effects",
                    9,
                )
                .await
                .unwrap();
            assert!(
                case.store
                    .workspace_activity(case.user, case.project)
                    .await
                    .unwrap()
                    .language_command
            );
            let recovery = Channel::new(&case, mode, 'b', 10).await;
            let original = recovery.read(&case, 10).await.unwrap();
            assert_eq!(original.command.phase, ActionPhase::Acknowledged);
            let recovered = recovery
                .send(
                    &case,
                    CommandWorkerOperation::Claim {
                        expected: Box::new(original),
                        worker: recovery.worker.clone(),
                        admission: CommandAdmission::Recover,
                    },
                    10,
                )
                .await
                .unwrap();
            let settled = recovery
                .send(
                    &case,
                    CommandWorkerOperation::Settle {
                        expected: Box::new(recovered),
                        worker: recovery.worker.clone(),
                    },
                    11,
                )
                .await
                .unwrap();
            assert!(settled.activity.ensure_idle().is_ok());
        }
    });
}

#[test]
fn command_admission_rejects_missing_manual_authority_and_recovery_without_an_original_claim() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = ready(mode).await;
            let channel = Channel::new(&case, mode, 'a', 4).await;
            let original = channel.read(&case, 4).await.unwrap();
            assert!(
                channel
                    .send(
                        &case,
                        CommandWorkerOperation::Claim {
                            expected: Box::new(original.clone()),
                            worker: channel.worker.clone(),
                            admission: CommandAdmission::Recover
                        },
                        4
                    )
                    .await
                    .is_err()
            );
            let bad = CommandWorkerOperation::Claim {
                expected: Box::new(original.clone()),
                worker: channel.worker.clone(),
                admission: CommandAdmission::Start {
                    authority: CommandAuthority {
                        grant: "f".repeat(32),
                        session_id: None,
                    },
                },
            };
            assert!(channel.send(&case, bad, 4).await.is_err());
            case.store
                .set_user_setting(
                    case.user,
                    "language_settings",
                    r#"{"defaults":{"providers":{"custom":{"mode":"disabled"}}}}"#,
                )
                .await
                .unwrap();
            assert!(
                channel
                    .send(&case, channel.claim(&original, &case), 4)
                    .await
                    .is_err()
            );
            assert_eq!(channel.read(&case, 4).await.unwrap(), original);
        }
    });
}

#[test]
fn schema69_command_upgrade_and_replay_preserve_original_claims_and_native_settlement() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = ready(mode).await;
            let original = case
                .store
                .workspace_command_recovery(case.user, case.project, &case.approval.id)
                .await
                .unwrap()
                .unwrap();
            for sql in [
                "DROP TABLE language_command_delegations",
                "DROP INDEX language_actions_pending",
                "ALTER TABLE language_actions DROP COLUMN command_worker_id",
                "ALTER TABLE language_actions DROP COLUMN command_worker_digest",
                "ALTER TABLE language_actions DROP COLUMN command_worker_may_dispatch",
                "ALTER TABLE language_actions DROP COLUMN native_settlement_pending",
                "ALTER TABLE language_actions DROP COLUMN command_cleanup_confirmed",
                "CREATE UNIQUE INDEX language_actions_pending ON language_actions(user_id,project_id) WHERE phase NOT IN ('completed','acknowledged','cancelled')",
                "PRAGMA user_version=69",
            ] {
                case.store.db.execute(sql, &[]).await.unwrap();
            }
            case.store.migrate_with(&|_| true).await.unwrap();
            assert_eq!(
                case.store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap()
                    .unwrap(),
                original
            );
            let channel = Channel::new(&case, mode, 'a', 4).await;
            let claimed = channel
                .send(&case, channel.claim(&original, &case), 4)
                .await
                .unwrap();
            let outcome = CommandOutcome::Completed {
                result: json!({"original":"文😀"}),
            };
            let operation = CommandWorkerOperation::Finish {
                expected: Box::new(claimed),
                worker: channel.worker.clone(),
                outcome,
                cleaned: true,
            };
            let finished = channel.send(&case, operation.clone(), 5).await.unwrap();
            case.store.db.execute("UPDATE language_actions SET approval='frozen original approval',outcome='frozen original result'", &[]).await.unwrap();
            // Replay must keep admitted native credentials and terminal-but-
            // unsettled protection even when old payloads cannot be decoded.
            case.store
                .db
                .execute("PRAGMA user_version=69", &[])
                .await
                .unwrap();
            case.store.migrate_with(&|_| true).await.unwrap();
            assert_eq!(channel.read(&case, 1_000_000).await.unwrap(), finished);
            assert!(
                case.store
                    .delete_project(case.project, case.user)
                    .await
                    .is_err()
            );
            assert_eq!(
                channel.send(&case, operation, 1_000_000).await.unwrap(),
                finished
            );
            let settled = channel
                .send(
                    &case,
                    CommandWorkerOperation::Settle {
                        expected: Box::new(finished),
                        worker: channel.worker.clone(),
                    },
                    1_000_001,
                )
                .await
                .unwrap();
            assert!(settled.activity.ensure_idle().is_ok());
            case.store
                .delete_project(case.project, case.user)
                .await
                .unwrap();
        }
    });
}

#[test]
fn paired_file_and_command_grants_cannot_substitute_for_each_other() {
    use openwebide_core::workspace_transaction::{
        DelegateWorkspaceTransaction, DelegatedWorkspaceJournalOperation,
        DelegatedWorkspaceJournalRequest,
    };
    block_on(async {
        let case = ready(WorkspaceMode::Local).await;
        let channel = Channel::new(&case, WorkspaceMode::Local, 'a', 4).await;
        let original = channel.read(&case, 4).await.unwrap();
        let file_worker = WorkspaceTransactionWorker {
            id: "6".repeat(32),
            secret: "7".repeat(32),
        };
        let file_grant = case
            .store
            .delegate_workspace_transaction(
                case.user,
                case.project,
                &case.approval.id,
                &DelegateWorkspaceTransaction {
                    generation: original.scope.generation.clone(),
                    revision: original.scope.revision,
                    worker: file_worker.delegation_key(),
                },
                4,
            )
            .await
            .unwrap();
        assert!(
            case.store
                .delegated_language_command(
                    &DelegatedCommandRequest {
                        delegation: file_grant,
                        worker: file_worker,
                        operation: CommandJournalOperation::Scope
                    },
                    4
                )
                .await
                .is_err()
        );
        assert!(
            case.store
                .delegated_workspace_journal(
                    &DelegatedWorkspaceJournalRequest {
                        delegation: channel.grant.clone().unwrap(),
                        worker: channel.worker.clone(),
                        operation: DelegatedWorkspaceJournalOperation::Inspect
                    },
                    4
                )
                .await
                .is_err()
        );
        assert_eq!(channel.read(&case, 4).await.unwrap(), original);
        channel
            .send(&case, channel.claim(&original, &case), 4)
            .await
            .unwrap();
    });
}

#[test]
fn native_intent_can_reject_a_definitely_unclaimed_command_without_fresh_dispatch_authority() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = ready(mode).await;
            let channel = Channel::new(&case, mode, 'a', 4).await;
            let original = channel.read(&case, 4).await.unwrap();
            let mut start = channel.claim(&original, &case);
            if let CommandWorkerOperation::Claim {
                admission: CommandAdmission::Start { authority },
                ..
            } = &mut start
            {
                authority.grant = "f".repeat(32);
            }
            assert!(channel.send(&case, start.clone(), 4).await.is_err());
            let reject = CommandWorkerOperation::RejectUnclaimed {
                expected: Box::new(original.clone()),
                worker: channel.worker.clone(),
                message: "Native admission failed before any command was sent".into(),
            };
            let rejected = channel.send(&case, reject.clone(), 5).await.unwrap();
            assert_eq!(rejected.command.phase, ActionPhase::Rejected);
            assert!(
                rejected.command.command_cleanup_confirmed
                    && rejected.command.native_settlement_pending
            );
            assert_eq!(
                channel.send(&case, reject, 1_000_000).await.unwrap(),
                rejected
            );
            // Neither the old start nor a freshly granted Start can convert this
            // no-dispatch reconciliation into permission to run the command.
            assert!(channel.send(&case, start, 6).await.is_err());
            assert!(
                channel
                    .send(&case, channel.claim(&rejected, &case), 6)
                    .await
                    .is_err()
            );
            assert!(
                case.store
                    .delete_project(case.project, case.user)
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn recorded_outcomes_remain_observable_without_reauthorizing_dispatch_in_both_modes() {
    use openwebide_core::plugins::languages::intelligence::actions::{
        CommandDispatch, CommandOperation,
    };
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for kind in 0..4 {
                let case = ready(mode).await;
                let running = case
                    .store
                    .language_action(case.user, case.project, Some(&case.approval.id))
                    .await
                    .unwrap()
                    .unwrap();
                let original = running.invocation().unwrap();
                let outcome = match kind {
                    0 => CommandOutcome::Completed {
                        result: json!({"original":true}),
                    },
                    1 => CommandOutcome::Rejected {
                        message: "Not sent".into(),
                    },
                    _ => CommandOutcome::Uncertain {
                        message: "Response lost".into(),
                    },
                };
                let mut finished = case
                    .store
                    .finish_language_action(
                        case.user,
                        case.project,
                        &case.approval.id,
                        running.revision,
                        &outcome,
                        5,
                    )
                    .await
                    .unwrap();
                if kind == 3 {
                    finished = case
                        .store
                        .acknowledge_language_action(
                            case.user,
                            case.project,
                            &case.approval.id,
                            finished.revision,
                            "Reviewed original effects",
                            6,
                        )
                        .await
                        .unwrap();
                }
                for operation in [CommandOperation::Status, CommandOperation::Stop] {
                    assert_eq!(operation.invocation(&finished).unwrap(), original);
                    let control = case
                        .store
                        .authorize_language_command(
                            case.user,
                            case.project,
                            &CommandDispatch {
                                id: case.approval.id.clone(),
                                operation,
                                call: None,
                            },
                            1_000_000,
                        )
                        .await
                        .unwrap();
                    assert_eq!(control.invocation, original);
                    assert_eq!(
                        control.progress,
                        (operation == CommandOperation::Status).then(|| {
                            openwebide_core::plugins::languages::intelligence::actions::CommandProgress::Finished {
                                outcome: outcome.clone(),
                            }
                        })
                    );
                    assert!(
                        case.store
                            .authorize_language_command(
                                case.other,
                                case.project,
                                &CommandDispatch {
                                    id: case.approval.id.clone(),
                                    operation,
                                    call: None
                                },
                                1_000_000
                            )
                            .await
                            .is_err()
                    );
                }
                assert!(CommandOperation::Start.invocation(&finished).is_err());
                assert!(
                    case.store
                        .authorize_language_command(
                            case.user,
                            case.project,
                            &CommandDispatch {
                                id: case.approval.id.clone(),
                                operation: CommandOperation::Start,
                                call: Some(Box::new(case.call.clone()))
                            },
                            6
                        )
                        .await
                        .is_err()
                );
            }
        }
    });
}

#[test]
fn stopping_a_recorded_uncertain_command_requires_positive_native_cleanup_in_both_modes() {
    use openwebide_core::plugins::languages::intelligence::actions::{
        CommandDispatch, CommandOperation, CommandProgress,
    };
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = ready(mode).await;
            let channel = Channel::new(&case, mode, 'a', 4).await;
            let before = channel.read(&case, 4).await.unwrap();
            let mut current = channel
                .send(&case, channel.claim(&before, &case), 4)
                .await
                .unwrap();
            let outcome = CommandOutcome::Uncertain {
                message: "Original response lost".into(),
            };
            for cleaned in [false, true] {
                current = channel
                    .send(
                        &case,
                        CommandWorkerOperation::Finish {
                            expected: Box::new(current),
                            worker: channel.worker.clone(),
                            outcome: outcome.clone(),
                            cleaned,
                        },
                        5,
                    )
                    .await
                    .unwrap();
                for operation in [CommandOperation::Status, CommandOperation::Stop] {
                    let control = case
                        .store
                        .authorize_language_command(
                            case.user,
                            case.project,
                            &CommandDispatch {
                                id: case.approval.id.clone(),
                                operation,
                                call: None,
                            },
                            1_000_000,
                        )
                        .await
                        .unwrap();
                    assert_eq!(control.invocation, current.command.invocation);
                    assert_eq!(
                        control.progress,
                        (cleaned || operation == CommandOperation::Status).then(|| {
                            CommandProgress::Finished {
                                outcome: outcome.clone(),
                            }
                        })
                    );
                }
                assert!(
                    case.store
                        .authorize_language_command(
                            case.user,
                            case.project,
                            &CommandDispatch {
                                id: case.approval.id.clone(),
                                operation: CommandOperation::Start,
                                call: Some(Box::new(case.call.clone()))
                            },
                            5
                        )
                        .await
                        .is_err()
                );
                assert!(current.command.native_settlement_pending);
            }
        }
    });
}

#[test]
fn public_compact_recovery_preserves_native_ownership_with_frozen_payloads_in_both_modes() {
    use openwebide_core::plugins::languages::intelligence::actions::{
        ActionRecoveryRequest, CommandDispatch, CommandOperation,
    };
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = ready(mode).await;
            let channel = Channel::new(&case, mode, 'a', 4).await;
            let before = channel.read(&case, 4).await.unwrap();
            let mut current = channel
                .send(&case, channel.claim(&before, &case), 4)
                .await
                .unwrap();
            case.store
                .db
                .execute(
                    "UPDATE language_actions SET approval='unreadable provider/source'",
                    &[],
                )
                .await
                .unwrap();
            case.store.db.execute("UPDATE workspace_transactions SET plan='unreadable',editor='unreadable',language_review='unreadable'",&[]).await.unwrap();
            let load = ActionRecoveryRequest::Load {
                id: Some(case.approval.id.clone()),
            };
            let running = case
                .store
                .language_action_recovery(case.user, case.project, &load, 1_000_000)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(running.command, current.command);
            assert!(running.pending());
            assert!(
                case.store
                    .language_action_recovery(case.other, case.project, &load, 5)
                    .await
                    .is_err()
            );
            for operation in [CommandOperation::Status, CommandOperation::Stop] {
                let control = case
                    .store
                    .authorize_language_command(
                        case.user,
                        case.project,
                        &CommandDispatch {
                            id: case.approval.id.clone(),
                            operation,
                            call: None,
                        },
                        1_000_000,
                    )
                    .await
                    .unwrap();
                assert!(control.selection.is_none());
                assert_eq!(control.invocation, running.command.invocation);
                assert!(control.progress.is_none());
            }
            assert!(
                case.store
                    .authorize_language_command(
                        case.user,
                        case.project,
                        &CommandDispatch {
                            id: case.approval.id.clone(),
                            operation: CommandOperation::Start,
                            call: Some(Box::new(case.call.clone()))
                        },
                        4
                    )
                    .await
                    .is_err()
            );
            let outcome = CommandOutcome::Uncertain {
                message: "Original response lost".into(),
            };
            current = channel
                .send(
                    &case,
                    CommandWorkerOperation::Finish {
                        expected: Box::new(current),
                        worker: channel.worker.clone(),
                        outcome: outcome.clone(),
                        cleaned: false,
                    },
                    5,
                )
                .await
                .unwrap();
            let record = case
                .store
                .language_action_recovery(case.user, case.project, &load, 6)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(record.outcome, Some(outcome.clone()));
            assert!(record.pending());
            let acknowledge = ActionRecoveryRequest::Acknowledge {
                id: case.approval.id.clone(),
                revision: record.command.revision,
                note: "Reviewed original effects".into(),
            };
            assert!(
                case.store
                    .language_action_recovery(case.user, case.project, &acknowledge, 6)
                    .await
                    .is_err()
            );
            assert!(
                case.store
                    .language_action_recovery(
                        case.user,
                        case.project,
                        &ActionRecoveryRequest::Finish {
                            id: case.approval.id.clone(),
                            revision: record.command.revision,
                            outcome: CommandOutcome::Completed {
                                result: json!({"forged":true})
                            }
                        },
                        6
                    )
                    .await
                    .is_err()
            );
            current = channel
                .send(
                    &case,
                    CommandWorkerOperation::Finish {
                        expected: Box::new(current),
                        worker: channel.worker.clone(),
                        outcome,
                        cleaned: true,
                    },
                    7,
                )
                .await
                .unwrap();
            assert!(current.command.command_cleanup_confirmed);
            let acknowledged = case
                .store
                .language_action_recovery(case.user, case.project, &acknowledge, 8)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(acknowledged.command.phase, ActionPhase::Acknowledged);
            assert!(acknowledged.pending());
            assert!(
                case.store
                    .workspace_activity(case.user, case.project)
                    .await
                    .unwrap()
                    .ensure_idle()
                    .is_err()
            );
            assert!(
                case.store
                    .delete_project(case.project, case.user)
                    .await
                    .is_err()
            );
            let expected = channel.read(&case, 9).await.unwrap();
            channel
                .send(
                    &case,
                    CommandWorkerOperation::Settle {
                        expected: Box::new(expected),
                        worker: channel.worker.clone(),
                    },
                    9,
                )
                .await
                .unwrap();
            let settled = case
                .store
                .language_action_recovery(case.user, case.project, &load, 10)
                .await
                .unwrap()
                .unwrap();
            assert!(!settled.pending());
            assert_eq!(settled.command.phase, ActionPhase::Acknowledged);
            assert!(
                case.store
                    .workspace_activity(case.user, case.project)
                    .await
                    .unwrap()
                    .ensure_idle()
                    .is_ok()
            );
        }
    });
}

#[test]
fn compact_recovery_checks_original_outcome_digest_and_bounds_before_returning_it() {
    use openwebide_core::plugins::languages::intelligence::actions::{
        ActionRecoveryRequest, MAX_COMMAND_BYTES,
    };
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = ready(mode).await;
            let outcome = CommandOutcome::Uncertain {
                message: "Lost original response".into(),
            };
            let load = ActionRecoveryRequest::Load {
                id: Some(case.approval.id.clone()),
            };
            let running = case
                .store
                .language_action_recovery(case.user, case.project, &load, 4)
                .await
                .unwrap()
                .unwrap();
            let finished = case
                .store
                .language_action_recovery(
                    case.user,
                    case.project,
                    &ActionRecoveryRequest::Finish {
                        id: case.approval.id.clone(),
                        revision: running.command.revision,
                        outcome: outcome.clone(),
                    },
                    5,
                )
                .await
                .unwrap()
                .unwrap();
            case.store
                .db
                .execute("UPDATE language_actions SET approval='unreadable'", &[])
                .await
                .unwrap();
            let recovered = case
                .store
                .language_action_recovery(case.user, case.project, &load, 6)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(recovered, finished);
            let original = serde_json::to_string(&outcome).unwrap();
            for changed in [
                serde_json::to_string(&CommandOutcome::Uncertain {
                    message: "Different result".into(),
                })
                .unwrap(),
                "x".repeat(MAX_COMMAND_BYTES + 1),
            ] {
                case.store
                    .db
                    .execute(
                        "UPDATE language_actions SET outcome=?",
                        &[DbValue::Text(changed)],
                    )
                    .await
                    .unwrap();
                assert!(
                    case.store
                        .language_action_recovery(case.user, case.project, &load, 6)
                        .await
                        .is_err()
                );
            }
            case.store
                .db
                .execute(
                    "UPDATE language_actions SET outcome=?",
                    &[DbValue::Text(original)],
                )
                .await
                .unwrap();
            let ack = ActionRecoveryRequest::Acknowledge {
                id: case.approval.id.clone(),
                revision: finished.command.revision,
                note: "Reviewed original effects".into(),
            };
            let acknowledged = case
                .store
                .language_action_recovery(case.user, case.project, &ack, 7)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(acknowledged.command.phase, ActionPhase::Acknowledged);
            assert!(
                case.store
                    .language_action_recovery(case.user, case.project, &ack, 8)
                    .await
                    .is_err()
            );
        }
    });
}
