use super::*;

#[test]
fn original_outcome_reads_are_bounded_source_independent_and_bound_to_the_exact_worker_snapshot() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for outcome in [
                CommandOutcome::Rejected {
                    message: "Keep the original edit: 文😀 \\\" reason".repeat(200),
                },
                CommandOutcome::Completed {
                    result: json!({"retained":"x".repeat(200_000)}),
                },
            ] {
                let case = ready(mode).await;
                let channel = Channel::new(&case, mode, 'a', 4).await;
                let before = channel.read(&case, 4).await.unwrap();
                let current = channel
                    .send(&case, channel.claim(&before, &case), 4)
                    .await
                    .unwrap();
                let current = channel
                    .send(
                        &case,
                        CommandWorkerOperation::Finish {
                            expected: Box::new(current),
                            worker: channel.worker.clone(),
                            outcome: outcome.clone(),
                            cleaned: true,
                        },
                        5,
                    )
                    .await
                    .unwrap();
                let credentials = case
                    .store
                    .command_credentials(case.user, case.project, &case.approval.id)
                    .await
                    .unwrap();
                // Compact invocation and result proof remain available after
                // the archived executable/provider payload becomes unreadable.
                case.store.db.execute("UPDATE language_actions SET approval='archived provider payload' WHERE user_id=? AND project_id=? AND id=?",&[
                    DbValue::Int(case.user.get()),DbValue::Int(case.project),DbValue::Text(case.approval.id.clone()),
                ]).await.unwrap();
                let request = OriginalCommandRequest {
                    authority: RecoveryAuthority::for_scope(
                        case.user.get(),
                        case.project,
                        &current.scope,
                        "A".repeat(43),
                    )
                    .unwrap(),
                    worker: channel.worker.clone(),
                    operation: CommandJournalOperation::Outcome {
                        expected: Box::new(current.clone()),
                    },
                };
                let reply = case
                    .store
                    .original_language_command(&request, 1_000_000)
                    .await
                    .unwrap();
                assert_eq!(reply.command_outcome(&current).unwrap(), &outcome);
                assert_eq!(
                    case.store
                        .command_credentials(case.user, case.project, &case.approval.id)
                        .await
                        .unwrap(),
                    credentials
                );
                for forgery in 0..3 {
                    let mut forged = request.clone();
                    match forgery {
                        0 => forged.worker.secret = "f".repeat(32),
                        1 => {
                            let CommandJournalOperation::Outcome { expected } =
                                &mut forged.operation
                            else {
                                unreachable!()
                            };
                            expected.command.revision += 1;
                        }
                        2 => {
                            let CommandJournalOperation::Outcome { expected } =
                                &mut forged.operation
                            else {
                                unreachable!()
                            };
                            expected.scope.root_identity = "f".repeat(64);
                        }
                        _ => unreachable!(),
                    }
                    assert!(
                        case.store
                            .original_language_command(&forged, 1_000_001)
                            .await
                            .is_err()
                    );
                }
                let mut forged = reply.clone();
                let WorkspaceJournalResponse::CommandOutcome { outcome, .. } = &mut forged else {
                    unreachable!()
                };
                *outcome = CommandOutcome::Rejected {
                    message: "different original result".into(),
                };
                assert!(forged.command_outcome(&current).is_err());
                let mut stale = current.clone();
                stale.command.revision += 1;
                assert!(reply.command_outcome(&stale).is_err());
                assert_eq!(
                    case.store
                        .workspace_command_recovery(case.user, case.project, &case.approval.id)
                        .await
                        .unwrap()
                        .unwrap(),
                    current
                );
            }
        }
    });
}
