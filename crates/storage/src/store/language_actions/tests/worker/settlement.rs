use super::*;
use openwebide_core::plugins::languages::intelligence::actions::OriginalCommandRequest;
use openwebide_core::workspace_transaction::RecoveryAuthority;

#[test]
fn original_settlement_survives_public_history_pruning_without_retaining_payloads() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for kind in ["completed", "rejected", "uncertain"] {
                let case = ready(mode).await;
                let channel = Channel::new(&case, mode, 'a', 4).await;
                let before = channel.read(&case, 4).await.unwrap();
                let admitted = channel
                    .send(&case, channel.claim(&before, &case), 4)
                    .await
                    .unwrap();
                let mut finished = channel
                    .send(
                        &case,
                        CommandWorkerOperation::Finish {
                            expected: Box::new(admitted),
                            worker: channel.worker.clone(),
                            outcome: match kind {
                                "completed" => CommandOutcome::Completed {
                                    result: json!({"kept":"原本 😀"}),
                                },
                                "rejected" => CommandOutcome::Rejected {
                                    message: "Original rejection 文😀".into(),
                                },
                                _ => CommandOutcome::Uncertain {
                                    message: "Original uncertain result 文😀".into(),
                                },
                            },
                            cleaned: true,
                        },
                        5,
                    )
                    .await
                    .unwrap();
                if kind != "completed" {
                    case.store
                        .acknowledge_language_action(
                            case.user,
                            case.project,
                            &case.approval.id,
                            finished.command.revision,
                            "Reviewed the original outcome",
                            6,
                        )
                        .await
                        .unwrap();
                    finished = channel.read(&case, 6).await.unwrap();
                }
                let settle = CommandWorkerOperation::Settle {
                    expected: Box::new(finished),
                    worker: channel.worker.clone(),
                };
                let settled = channel.send(&case, settle.clone(), 6).await.unwrap();
                assert!(!settled.command.native_settlement_pending);
                let mut next = case.begin.clone();
                next.id = "d".repeat(32);
                let editor = case
                    .store
                    .editor_recovery(case.user, case.project)
                    .await
                    .unwrap();
                next.editor.revision = editor.revision;
                next.editor.before = editor.state.files.clone();
                next.editor.after = editor.state.files.clone();
                next.editor.order_before =
                    editor.state.files.iter().map(|f| f.path.clone()).collect();
                next.editor.order_after = next.editor.order_before.clone();
                next.editor
                    .selected_before
                    .clone_from(&editor.state.selected);
                next.editor
                    .selected_after
                    .clone_from(&editor.state.selected);
                case.store
                    .begin_workspace_transaction(case.user, case.project, &next, 1_000_000)
                    .await
                    .unwrap();
                assert!(
                    case.store
                        .language_action(case.user, case.project, Some(&case.approval.id))
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(
                    case.store
                        .workspace_transaction(case.user, case.project, Some(&case.approval.id))
                        .await
                        .unwrap()
                        .is_none()
                );
                let mut original = OriginalCommandRequest {
                    authority: RecoveryAuthority::for_scope(
                        case.user.get(),
                        case.project,
                        &settled.scope,
                        "A".repeat(43),
                    )
                    .unwrap(),
                    worker: channel.worker.clone(),
                    operation: CommandJournalOperation::Scope,
                };
                let reply = case
                    .store
                    .original_language_command(&original, 1_000_001)
                    .await
                    .unwrap();
                let WorkspaceJournalResponse::CommandScope {
                    recovery: Some(current),
                } = reply
                else {
                    panic!("Missing retained original settlement");
                };
                assert_eq!(*current, settled);
                original.operation = CommandJournalOperation::Worker {
                    operation: Box::new(settle),
                };
                assert_eq!(
                    *case
                        .store
                        .original_language_command(&original, 1_000_001)
                        .await
                        .unwrap()
                        .command_worker(match &original.operation {
                            CommandJournalOperation::Worker { operation } => operation,
                            _ => unreachable!(),
                        })
                        .unwrap(),
                    settled
                );
                let receipt = case.store.db.execute("SELECT recovery FROM language_command_settlements WHERE user_id=? AND project_id=? AND id=?", &[DbValue::Int(case.user.get()),DbValue::Int(case.project),DbValue::Text(case.approval.id.clone())]).await.unwrap();
                let data = receipt.rows[0].get_text(0).unwrap();
                assert!(
                    !data.contains("text 文")
                        && !data.contains("原本")
                        && !data.contains(&channel.worker.secret)
                );
                assert!(data.len() < 64 * 1024);
                for mutation in ["secret", "worker", "generation", "root", "user", "outcome"] {
                    let mut forged = original.clone();
                    forged.operation = CommandJournalOperation::Scope;
                    match mutation {
                        "secret" => forged.worker.secret = "f".repeat(32),
                        "worker" => forged.worker.id = "f".repeat(32),
                        "generation" => forged.authority.generation = "f".repeat(32),
                        "root" => forged.authority.root_identity = "f".repeat(64),
                        "user" => forged.authority.user = case.other.get(),
                        _ => {
                            forged.operation = CommandJournalOperation::Outcome {
                                expected: Box::new(settled.clone()),
                            }
                        }
                    }
                    assert!(
                        case.store
                            .original_language_command(&forged, 1_000_002)
                            .await
                            .is_err(),
                        "{mode:?}/{kind}/{mutation}"
                    );
                }
                let before_ack = case
                    .store
                    .workspace_transaction_scope(case.user, case.project, &next.id)
                    .await
                    .unwrap();
                original.operation = CommandJournalOperation::AcknowledgeSettlement {
                    expected: Box::new(settled.clone()),
                };
                for _ in 0..2 {
                    let acknowledged = case
                        .store
                        .original_language_command(&original, 1_000_003)
                        .await
                        .unwrap();
                    acknowledged
                        .command_settlement_acknowledged(&settled)
                        .unwrap();
                    assert!(
                        acknowledged
                            .command_recovery(case.user.get(), case.project, &case.approval.id)
                            .is_err()
                    );
                }
                assert_eq!(
                    case.store
                        .workspace_transaction_scope(case.user, case.project, &next.id)
                        .await
                        .unwrap(),
                    before_ack
                );
                let retained = case
                    .store
                    .db
                    .execute(
                        "SELECT count(*) FROM language_command_settlements WHERE user_id=?",
                        &[DbValue::Int(case.user.get())],
                    )
                    .await
                    .unwrap();
                assert_eq!(retained.rows[0].get_int(0).unwrap(), 0);
                original.operation = CommandJournalOperation::Scope;
                assert!(
                    case.store
                        .original_language_command(&original, 1_000_004)
                        .await
                        .is_err()
                );
            }
        }
    });
}
