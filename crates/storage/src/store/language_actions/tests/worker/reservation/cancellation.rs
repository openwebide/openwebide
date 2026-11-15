use super::*;
use openwebide_core::plugins::languages::intelligence::actions::{
    CommandDispatch, CommandOperation, CommandProgress,
};

async fn recovery(case: &Scenario) -> WorkspaceCommandRecovery {
    case.store
        .workspace_command_recovery(case.user, case.project, &case.approval.id)
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn original_cancellation_before_any_file_admission_cannot_create_dispatch_permission() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            case.approve().await;
            let before = recovery(&case).await;
            let cancel_control = CommandDispatch {
                id: case.approval.id.clone(),
                operation: CommandOperation::Cancel,
                call: None,
            };
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &cancel_control, 2)
                    .await
                    .is_err()
            );
            let worker = WorkspaceTransactionWorker {
                id: "c".repeat(32),
                secret: "d".repeat(32),
            };
            let mut request = OriginalCommandRequest {
                authority: RecoveryAuthority::for_scope(
                    case.user.get(),
                    case.project,
                    &before.scope,
                    "A".repeat(43),
                )
                .unwrap(),
                worker: worker.clone(),
                operation: CommandJournalOperation::Scope,
            };
            assert!(
                case.store
                    .original_language_command(&request, 2)
                    .await
                    .is_ok()
            );
            for admission in [
                CommandAdmission::Recover,
                CommandAdmission::Start {
                    authority: CommandAuthority {
                        grant: case.call.grant.clone(),
                        session_id: None,
                    },
                },
            ] {
                request.operation = CommandJournalOperation::Worker {
                    operation: Box::new(CommandWorkerOperation::Claim {
                        expected: Box::new(before.clone()),
                        worker: worker.clone(),
                        admission,
                    }),
                };
                assert!(
                    case.store
                        .original_language_command(&request, 2)
                        .await
                        .is_err()
                );
            }
            request.operation = CommandJournalOperation::Worker {
                operation: Box::new(CommandWorkerOperation::ConfirmCancellation {
                    expected: Box::new(before.clone()),
                    worker: worker.clone(),
                }),
            };
            assert!(
                case.store
                    .original_language_command(&request, 2)
                    .await
                    .is_err()
            );
            case.store
                .advance_workspace_transaction(
                    case.user,
                    case.project,
                    &AdvanceWorkspaceTransaction {
                        id: case.approval.id.clone(),
                        generation: case.generation.clone(),
                        revision: before.scope.revision,
                        progress: TransactionProgress::prepared_cancellation(),
                    },
                    3,
                )
                .await
                .unwrap();
            case.store
                .cancel_language_action(case.user, case.project, &case.approval.id, 1, 4)
                .await
                .unwrap();
            let cancelled = recovery(&case).await;
            assert!(cancelled.command.worker.is_none());
            let authorized = case
                .store
                .authorize_language_command(case.user, case.project, &cancel_control, 4)
                .await
                .unwrap();
            assert_eq!(authorized.progress, Some(CommandProgress::Cancelled));
            assert!(authorized.selection.is_none());
            let operation = CommandWorkerOperation::ConfirmCancellation {
                expected: Box::new(cancelled),
                worker: worker.clone(),
            };
            request.operation = CommandJournalOperation::Worker {
                operation: Box::new(operation.clone()),
            };
            let confirmed = case
                .store
                .original_language_command(&request, 1_000_000)
                .await
                .unwrap()
                .command_worker(&operation)
                .unwrap()
                .clone();
            assert!(
                confirmed.command.native_settlement_pending
                    && confirmed.command.command_cleanup_confirmed
            );
            assert!(confirmed.command.outcome_digest.is_none());
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &cancel_control, 1_000_000)
                    .await
                    .unwrap()
                    .progress
                    .is_none()
            );
            assert_eq!(
                case.store
                    .original_language_command(&request, 1_000_001)
                    .await
                    .unwrap()
                    .command_worker(&operation)
                    .unwrap(),
                &confirmed
            );
            let mut replacement = request.clone();
            replacement.worker.secret = "e".repeat(32);
            replacement.operation = CommandJournalOperation::Scope;
            assert!(
                case.store
                    .original_language_command(&replacement, 1_000_001)
                    .await
                    .is_err()
            );
            let operation = CommandWorkerOperation::Settle {
                expected: Box::new(confirmed),
                worker: worker.clone(),
            };
            request.operation = CommandJournalOperation::Worker {
                operation: Box::new(operation.clone()),
            };
            let settled = case
                .store
                .original_language_command(&request, 1_000_002)
                .await
                .unwrap()
                .command_worker(&operation)
                .unwrap()
                .clone();
            assert!(!settled.command.native_settlement_pending);
            assert!(settled.command.outcome_digest.is_none());
            assert_eq!(
                case.store
                    .original_language_command(&request, 1_000_003)
                    .await
                    .unwrap()
                    .command_worker(&operation)
                    .unwrap(),
                &settled
            );
            assert!(case.claim().await.is_err());
        }
    });
}

#[test]
fn cancelled_reservations_settle_without_a_command_outcome_or_dispatch_permission() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let mut case = Scenario::new(mode).await;
            case.approve().await;
            let files = Files::new(&case, mode).await;
            let channel = Channel::new(&case, mode, 'a', 2).await;
            let before = recovery(&case).await;
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
            let mut request = OriginalCommandRequest {
                authority,
                worker: channel.worker.clone(),
                operation: CommandJournalOperation::Scope,
            };
            let confirm = |expected| CommandWorkerOperation::ConfirmCancellation {
                expected: Box::new(expected),
                worker: channel.worker.clone(),
            };
            let reserved = recovery(&case).await;
            request.operation = CommandJournalOperation::Worker {
                operation: Box::new(confirm(reserved.clone())),
            };
            assert!(
                case.store
                    .original_language_command(&request, 3)
                    .await
                    .is_err()
            );
            assert_eq!(recovery(&case).await, reserved);
            assert!(
                case.store
                    .cancel_language_action(case.user, case.project, &case.approval.id, 1, 3)
                    .await
                    .is_err()
            );
            files
                .finish_and_release(&case, &[TransactionProgress::prepared_cancellation()])
                .await;
            let action = case
                .store
                .cancel_language_action(case.user, case.project, &case.approval.id, 1, 4)
                .await
                .unwrap();
            assert_eq!(action.phase, ActionPhase::Cancelled);
            assert!(action.outcome.is_none());
            assert!(action.acknowledgement.is_none());
            let cancelled = recovery(&case).await;
            assert!(cancelled.command.native_settlement_pending);
            assert!(!cancelled.command.command_cleanup_confirmed);
            assert!(
                case.store
                    .workspace_activity(case.user, case.project)
                    .await
                    .unwrap()
                    .ensure_idle()
                    .is_err()
            );
            let settle = CommandWorkerOperation::Settle {
                expected: Box::new(cancelled.clone()),
                worker: channel.worker.clone(),
            };
            request.operation = CommandJournalOperation::Worker {
                operation: Box::new(settle),
            };
            assert!(
                case.store
                    .original_language_command(&request, 5)
                    .await
                    .is_err()
            );
            for outcome in [
                CommandOutcome::Rejected {
                    message: "Invented rejection".into(),
                },
                CommandOutcome::Uncertain {
                    message: "Invented uncertainty".into(),
                },
                CommandOutcome::Completed {
                    result: json!("Invented result"),
                },
            ] {
                request.operation = CommandJournalOperation::Worker {
                    operation: Box::new(CommandWorkerOperation::Finish {
                        expected: Box::new(cancelled.clone()),
                        worker: channel.worker.clone(),
                        outcome,
                        cleaned: true,
                    }),
                };
                assert!(
                    case.store
                        .original_language_command(&request, 5)
                        .await
                        .is_err()
                );
                assert_eq!(recovery(&case).await, cancelled);
            }
            // Recovery has no dependency on the provider payload, manual grant,
            // original file worker or process that accepted cancellation.
            case.store
                .set_user_setting(case.user, "language_settings", "{revoked provider")
                .await
                .unwrap();
            case.store = Store::new(RusqliteDb::open(&case.database).unwrap());
            let operation = confirm(cancelled.clone());
            request.operation = CommandJournalOperation::Worker {
                operation: Box::new(operation.clone()),
            };
            // A contradictory persisted dispatch permit must fail closed even
            // when the public cancellation and worker otherwise match.
            case.store.db.execute("UPDATE language_actions SET command_worker_may_dispatch=1 WHERE user_id=? AND project_id=? AND id=?", &[
                DbValue::Int(case.user.get()), DbValue::Int(case.project), DbValue::Text(case.approval.id.clone()),
            ]).await.unwrap();
            assert!(
                case.store
                    .original_language_command(&request, 1_000_000)
                    .await
                    .is_err()
            );
            assert_eq!(recovery(&case).await, cancelled);
            case.store.db.execute("UPDATE language_actions SET command_worker_may_dispatch=0 WHERE user_id=? AND project_id=? AND id=?", &[
                DbValue::Int(case.user.get()), DbValue::Int(case.project), DbValue::Text(case.approval.id.clone()),
            ]).await.unwrap();
            for forgery in 0..5 {
                let mut forged = request.clone();
                let CommandJournalOperation::Worker { operation } = &mut forged.operation else {
                    unreachable!()
                };
                let CommandWorkerOperation::ConfirmCancellation { expected, worker } =
                    operation.as_mut()
                else {
                    unreachable!()
                };
                match forgery {
                    0 => {
                        worker.secret = "f".repeat(32);
                        forged.worker = worker.clone();
                    }
                    1 => {
                        worker.id = "f".repeat(32);
                        forged.worker = worker.clone();
                    }
                    2 => expected.scope.generation = "f".repeat(32),
                    3 => expected.command.revision += 1,
                    4 => expected.command.invocation.server = "replacement".into(),
                    _ => unreachable!(),
                }
                assert!(
                    case.store
                        .original_language_command(&forged, 1_000_000)
                        .await
                        .is_err()
                );
                assert_eq!(recovery(&case).await, cancelled);
            }
            let response = case
                .store
                .original_language_command(&request, 1_000_000)
                .await
                .unwrap();
            let confirmed = response.command_worker(&operation).unwrap().clone();
            assert_eq!(confirmed.command.phase, ActionPhase::Cancelled);
            assert_eq!(confirmed.command.revision, action.revision);
            assert!(confirmed.command.outcome_digest.is_none());
            assert!(confirmed.command.native_settlement_pending);
            assert!(confirmed.command.command_cleanup_confirmed);
            assert_eq!(
                case.store
                    .original_language_command(&request, 1_000_001)
                    .await
                    .unwrap()
                    .command_worker(&operation)
                    .unwrap(),
                &confirmed
            );
            let mut stale_reply = response;
            let WorkspaceJournalResponse::CommandScope {
                recovery: Some(value),
            } = &mut stale_reply
            else {
                unreachable!()
            };
            value.scope.generation = "f".repeat(32);
            assert!(stale_reply.command_worker(&operation).is_err());
            let mut forged_reply = WorkspaceJournalResponse::CommandScope {
                recovery: Some(Box::new(confirmed.clone())),
            };
            let WorkspaceJournalResponse::CommandScope {
                recovery: Some(value),
            } = &mut forged_reply
            else {
                unreachable!()
            };
            value.command.command_cleanup_confirmed = false;
            assert!(forged_reply.command_worker(&operation).is_err());
            let operation = CommandWorkerOperation::Settle {
                expected: Box::new(confirmed),
                worker: channel.worker.clone(),
            };
            request.operation = CommandJournalOperation::Worker {
                operation: Box::new(operation.clone()),
            };
            let settled = case
                .store
                .original_language_command(&request, 1_000_002)
                .await
                .unwrap()
                .command_worker(&operation)
                .unwrap()
                .clone();
            assert!(!settled.command.worker.as_ref().unwrap().active);
            assert!(!settled.command.native_settlement_pending);
            assert_eq!(settled.command.phase, ActionPhase::Cancelled);
            assert!(settled.command.outcome_digest.is_none());
            case.store = Store::new(RusqliteDb::open(&case.database).unwrap());
            assert_eq!(
                case.store
                    .original_language_command(&request, 1_000_003)
                    .await
                    .unwrap()
                    .command_worker(&operation)
                    .unwrap(),
                &settled
            );
            let public = case.store.language_action_recovery(case.user, case.project, &openwebide_core::plugins::languages::intelligence::actions::ActionRecoveryRequest::Load { id: Some(case.approval.id.clone()) }, 1_000_003).await.unwrap().unwrap();
            assert_eq!(public.command, settled.command);
            assert!(!public.pending());
            assert!(public.outcome.is_none());
            assert!(public.acknowledgement.is_none());
            case.store
                .workspace_activity(case.user, case.project)
                .await
                .unwrap()
                .ensure_idle()
                .unwrap();
            assert!(case.claim().await.is_err());
            assert!(
                case.store
                    .native_language_command(
                        case.user,
                        case.project,
                        &case.approval.id,
                        &channel.claim(&cancelled, &case),
                        1_000_004
                    )
                    .await
                    .is_err()
            );
        }
    });
}
