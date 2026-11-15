use super::*;

async fn pre_metadata_schema(case: &Scenario) {
    for sql in [
        "DROP TABLE language_command_delegations",
        "DROP INDEX language_actions_pending",
        "ALTER TABLE language_actions DROP COLUMN command_worker_id",
        "ALTER TABLE language_actions DROP COLUMN command_worker_digest",
        "ALTER TABLE language_actions DROP COLUMN command_worker_may_dispatch",
        "ALTER TABLE language_actions DROP COLUMN native_settlement_pending",
        "ALTER TABLE language_actions DROP COLUMN command_cleanup_confirmed",
        "CREATE UNIQUE INDEX language_actions_pending ON language_actions(user_id,project_id) WHERE phase NOT IN ('completed','acknowledged','cancelled')",
        "ALTER TABLE language_actions DROP COLUMN invocation",
        "ALTER TABLE language_actions DROP COLUMN outcome_digest",
        "PRAGMA user_version=68",
    ] {
        case.store.db.execute(sql, &[]).await.unwrap();
    }
}

async fn phase(case: &Scenario, phase: ActionPhase) -> ActionRecord {
    let approved = case.approve().await;
    if phase == ActionPhase::Approved {
        return approved;
    }
    if phase == ActionPhase::Cancelled {
        case.store
            .advance_workspace_transaction(
                case.user,
                case.project,
                &AdvanceWorkspaceTransaction {
                    id: case.approval.id.clone(),
                    generation: case.generation.clone(),
                    revision: 1,
                    progress: TransactionProgress::prepared_cancellation(),
                },
                3,
            )
            .await
            .unwrap();
        return case
            .store
            .cancel_language_action(
                case.user,
                case.project,
                &case.approval.id,
                approved.revision,
                4,
            )
            .await
            .unwrap();
    }
    case.commit().await;
    if phase == ActionPhase::Rejected {
        return case
            .store
            .reject_unstarted_language_action(
                case.user,
                case.project,
                &case.approval.id,
                approved.revision,
                "Not dispatched",
                4,
            )
            .await
            .unwrap();
    }
    let running = case.claim().await.unwrap();
    if phase == ActionPhase::Running {
        return running;
    }
    let outcome = if phase == ActionPhase::Completed {
        CommandOutcome::Completed {
            result: json!({"result":"文😀"}),
        }
    } else {
        CommandOutcome::Uncertain {
            message: "Response lost".into(),
        }
    };
    let finished = case
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
    if phase == ActionPhase::Acknowledged {
        case.store
            .acknowledge_language_action(
                case.user,
                case.project,
                &case.approval.id,
                finished.revision,
                "Reviewed original effects",
                6,
            )
            .await
            .unwrap()
    } else {
        finished
    }
}

#[test]
fn original_command_and_compound_phase_survive_frozen_payloads_and_database_reopen() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for expected_phase in [
                ActionPhase::Approved,
                ActionPhase::Running,
                ActionPhase::Completed,
                ActionPhase::Rejected,
                ActionPhase::Uncertain,
                ActionPhase::Acknowledged,
                ActionPhase::Cancelled,
            ] {
                let case = Scenario::new(mode).await;
                let record = phase(&case, expected_phase).await;
                let original = case
                    .store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    original.command.invocation,
                    case.approval.command_invocation().unwrap()
                );
                assert_eq!(original.command.phase, expected_phase);
                assert_eq!(original.command.revision, record.revision);
                assert_eq!(
                    original.command.outcome_digest,
                    record
                        .outcome
                        .as_ref()
                        .map(|value| openwebide_core::plugins::content_digest(
                            &serde_json::to_vec(value).unwrap()
                        ))
                );
                assert_eq!(original.scope.generation, case.generation);
                assert_eq!(
                    original.activity.language_command,
                    !expected_phase.terminal()
                );
                assert!(
                    serde_json::to_vec(&original).unwrap().len()
                        < openwebide_core::workspace_transaction::RECOVERY_SCOPE_BODY_LIMIT
                );
                let activity = case
                    .store
                    .workspace_transaction_activity(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap();
                case.store.db.execute("UPDATE language_actions SET approval='unreadable provider/source',outcome='unreadable outcome',acknowledgement='unreadable acknowledgement'",&[]).await.unwrap();
                case.store.db.execute("UPDATE workspace_transactions SET plan='unreadable file snapshot',editor='unreadable editor',language_review='unreadable provider'",&[]).await.unwrap();
                let reopened = Store::new(RusqliteDb::open(&case.database).unwrap());
                reopened.migrate().await.unwrap();
                assert!(
                    reopened
                        .language_action(case.user, case.project, Some(&case.approval.id))
                        .await
                        .is_err()
                );
                assert_eq!(
                    reopened
                        .workspace_command_recovery(case.user, case.project, &case.approval.id)
                        .await
                        .unwrap()
                        .unwrap(),
                    original
                );
                assert_eq!(
                    reopened
                        .workspace_transaction_activity(case.user, case.project, &case.approval.id)
                        .await
                        .unwrap(),
                    activity
                );
                assert!(
                    reopened
                        .workspace_command_recovery(case.other, case.project, &case.approval.id)
                        .await
                        .is_err()
                );
                assert!(
                    reopened
                        .workspace_command_recovery(case.user, case.project, &"f".repeat(32))
                        .await
                        .unwrap()
                        .is_none()
                );
            }
        }
    });
}

#[test]
fn readable_legacy_actions_get_one_original_binding_that_later_payloads_cannot_replace() {
    block_on(async {
        use openwebide_core::plugins::languages::intelligence::actions::{
            CommandDispatch, CommandOperation,
        };
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            case.approve().await;
            case.commit().await;
            case.claim().await.unwrap();
            let original = case.approval.command_invocation().unwrap();
            let workspace = case
                .store
                .workspace_transaction_scope(case.user, case.project, &case.approval.id)
                .await
                .unwrap();
            pre_metadata_schema(&case).await;
            case.store.migrate().await.unwrap();
            case.store.migrate().await.unwrap();
            assert_eq!(
                case.store
                    .workspace_transaction_scope(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap(),
                workspace
            );
            assert!(
                case.store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .is_err()
            );
            let dispatch = CommandDispatch {
                id: case.approval.id.clone(),
                operation: CommandOperation::Start,
                call: Some(Box::new(case.call.clone())),
            };
            case.store
                .authorize_language_command(case.user, case.project, &dispatch, 4)
                .await
                .unwrap();
            assert_eq!(
                case.store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .command
                    .invocation,
                original
            );
            let mut replacement = case.approval.clone();
            replacement.review.command.as_mut().unwrap().name = "a different command".into();
            case.store
                .db
                .execute(
                    "UPDATE language_actions SET approval=?",
                    &[DbValue::Text(serde_json::to_string(&replacement).unwrap())],
                )
                .await
                .unwrap();
            let authorized = case
                .store
                .authorize_language_command(case.user, case.project, &dispatch, 4)
                .await
                .unwrap();
            assert_ne!(authorized.invocation, original);
            assert_eq!(
                case.store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .command
                    .invocation,
                original
            );
            for invalid in ["not-json".into(), "x".repeat(100_000)] {
                case.store
                    .db
                    .execute(
                        "UPDATE language_actions SET invocation=?",
                        &[DbValue::Text(invalid)],
                    )
                    .await
                    .unwrap();
                assert!(
                    case.store
                        .workspace_command_recovery(case.user, case.project, &case.approval.id)
                        .await
                        .is_err()
                );
                assert!(
                    case.store
                        .workspace_transaction_scope(case.user, case.project, &case.approval.id)
                        .await
                        .unwrap()
                        .is_some()
                );
            }
        }
    });
}

#[test]
fn unreadable_legacy_commands_migrate_without_losing_their_pending_recovery() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            case.approve().await;
            let original = case
                .store
                .workspace_transaction_activity(case.user, case.project, &case.approval.id)
                .await
                .unwrap();
            pre_metadata_schema(&case).await;
            case.store.db.execute("UPDATE language_actions SET approval='unreadable original',outcome='unreadable original outcome'", &[]).await.unwrap();
            case.store.migrate().await.unwrap();
            case.store.migrate().await.unwrap();
            assert_eq!(
                case.store
                    .workspace_transaction_activity(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap(),
                original
            );
            assert!(
                case.store
                    .workspace_command_recovery(case.user, case.project, &case.approval.id)
                    .await
                    .is_err()
            );
            assert!(
                case.store
                    .language_action(case.user, case.project, Some(&case.approval.id))
                    .await
                    .is_err()
            );
            let retained = case
                .store
                .db
                .execute("SELECT approval,outcome FROM language_actions", &[])
                .await
                .unwrap();
            assert_eq!(retained.rows[0].get_text(0).unwrap(), "unreadable original");
            assert_eq!(
                retained.rows[0].get_text(1).unwrap(),
                "unreadable original outcome"
            );
            assert!(
                case.store
                    .workspace_transaction(case.user, case.project, Some(&case.approval.id))
                    .await
                    .unwrap()
                    .is_some()
            );
        }
    });
}
