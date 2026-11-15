use super::*;
use openwebide_core::plugins::languages::intelligence::actions::OriginalCommandRequest;
use openwebide_core::workspace_transaction::RecoveryAuthority;

async fn completed(case: &Scenario, channel: &Channel) -> WorkspaceCommandRecovery {
    let before = channel.read(case, 4).await.unwrap();
    let claimed = channel
        .send(case, channel.claim(&before, case), 4)
        .await
        .unwrap();
    let finished = channel
        .send(
            case,
            CommandWorkerOperation::Finish {
                expected: Box::new(claimed),
                worker: channel.worker.clone(),
                outcome: CommandOutcome::Completed {
                    result: json!({"original": "do not retain this payload"}),
                },
                cleaned: true,
            },
            5,
        )
        .await
        .unwrap();
    channel
        .send(
            case,
            CommandWorkerOperation::Settle {
                expected: Box::new(finished),
                worker: channel.worker.clone(),
            },
            6,
        )
        .await
        .unwrap()
}

fn original(
    case: &Scenario,
    channel: &Channel,
    settled: &WorkspaceCommandRecovery,
) -> OriginalCommandRequest {
    OriginalCommandRequest {
        authority: RecoveryAuthority::for_scope(
            case.user.get(),
            case.project,
            &settled.scope,
            "A".repeat(43),
        )
        .unwrap(),
        worker: channel.worker.clone(),
        operation: CommandJournalOperation::Scope,
    }
}

#[test]
fn schema70_settlement_backfill_survives_frozen_payloads_and_does_not_resurrect_after_ack() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = ready(mode).await;
            let channel = Channel::new(&case, mode, 'a', 4).await;
            let settled = completed(&case, &channel).await;
            // Restore the actual previous schema shape and its terminal row:
            // schema 70 discarded the worker hash on successful settlement.
            case.store
                .db
                .execute("DROP TABLE language_command_settlements", &[])
                .await
                .unwrap();
            case.store
                .db
                .execute(
                    "ALTER TABLE language_actions DROP COLUMN command_settlement_acknowledged",
                    &[],
                )
                .await
                .unwrap();
            case.store
                .db
                .execute("PRAGMA user_version=70", &[])
                .await
                .unwrap();
            case.store.db.execute("UPDATE language_actions SET approval='frozen provider payload',outcome='frozen result payload'", &[]).await.unwrap();
            case.store.db.execute("UPDATE workspace_transactions SET plan='frozen files',editor='frozen editor',language_review='frozen review'", &[]).await.unwrap();
            case.store.migrate().await.unwrap();
            let mut request = original(&case, &channel, &settled);
            for _ in 0..2 {
                let reply = case
                    .store
                    .original_language_command(&request, 7)
                    .await
                    .unwrap();
                let WorkspaceJournalResponse::CommandScope {
                    recovery: Some(recovered),
                } = reply
                else {
                    panic!("Missing migrated original settlement");
                };
                assert_eq!(*recovered, settled);
                crate::migrations::replay_step(&case.store.db, 71)
                    .await
                    .unwrap();
            }
            let rows = case
                .store
                .db
                .execute(
                    "SELECT recovery,worker_digest,legacy FROM language_command_settlements",
                    &[],
                )
                .await
                .unwrap();
            assert_eq!(rows.rows.len(), 1);
            assert_eq!(rows.rows[0].get_text(1).unwrap(), "");
            assert_eq!(rows.rows[0].get_int(2).unwrap(), 1);
            assert!(!rows.rows[0].get_text(0).unwrap().contains("frozen"));
            let mut forged = request.clone();
            forged.worker.id = "f".repeat(32);
            assert!(
                case.store
                    .original_language_command(&forged, 7)
                    .await
                    .is_err()
            );
            request.operation = CommandJournalOperation::AcknowledgeSettlement {
                expected: Box::new(settled.clone()),
            };
            case.store
                .original_language_command(&request, 8)
                .await
                .unwrap()
                .command_settlement_acknowledged(&settled)
                .unwrap();
            crate::migrations::replay_step(&case.store.db, 71)
                .await
                .unwrap();
            let rows = case
                .store
                .db
                .execute("SELECT count(*) FROM language_command_settlements", &[])
                .await
                .unwrap();
            assert_eq!(rows.rows[0].get_int(0).unwrap(), 0);
            case.store
                .original_language_command(&request, 9)
                .await
                .unwrap()
                .command_settlement_acknowledged(&settled)
                .unwrap();
        }
    });
}

#[test]
fn pending_settlement_capacity_blocks_native_admission_and_ack_releases_its_slot() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let seed = ready(mode).await;
            let seed_channel = Channel::new(&seed, mode, 'a', 4).await;
            let terminal = completed(&seed, &seed_channel).await;
            let case = ready(mode).await;
            let channel = Channel::new(&case, mode, 'b', 4).await;
            let before = channel.read(&case, 4).await.unwrap();
            let mut first = None;
            // The fixture supplies previously settled private metadata, not
            // active workers. Admission must reserve space before attaching one.
            for index in 0..256u32 {
                let mut scope = terminal.clone();
                scope.scope.id = format!("{index:032x}");
                scope.command.invocation.id.clone_from(&scope.scope.id);
                let row = serde_json::to_string(&scope).unwrap();
                case.store.db.execute("INSERT INTO language_command_settlements(user_id,project_id,id,recovery,worker_digest,legacy) VALUES(?,?,?,?,?,0)", &[
                    DbValue::Int(case.user.get()), DbValue::Int(case.project), DbValue::Text(scope.scope.id.clone()), DbValue::Text(row), DbValue::Text(seed_channel.worker.secret_digest()),
                ]).await.unwrap();
                first.get_or_insert(scope);
            }
            assert!(
                channel
                    .send(&case, channel.claim(&before, &case), 5)
                    .await
                    .is_err()
            );
            assert_eq!(channel.read(&case, 5).await.unwrap(), before);
            let scope = first.unwrap();
            let mut acknowledgement = original(&case, &seed_channel, &scope);
            acknowledgement.operation = CommandJournalOperation::AcknowledgeSettlement {
                expected: Box::new(scope.clone()),
            };
            case.store
                .original_language_command(&acknowledgement, 6)
                .await
                .unwrap()
                .command_settlement_acknowledged(&scope)
                .unwrap();
            let admitted = channel
                .send(&case, channel.claim(&before, &case), 7)
                .await
                .unwrap();
            assert!(admitted.command.native_settlement_pending);
            assert_eq!(admitted.command.worker, Some(channel.worker.state()));
        }
    });
}
