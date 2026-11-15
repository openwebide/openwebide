use super::*;
use openwebide_core::{
    plugins::languages::intelligence::actions::{
        CommandAdmission, CommandDelegationRequest, CommandInvocation, CommandJournalOperation,
        CommandOutcome, CommandWorkerOperation, DelegatedCommandRequest,
    },
    workspace_transaction::{
        AdvanceWorkspaceTransaction, TransactionProgress, WorkspaceCommandRecovery,
        WorkspaceJournalDelegation,
    },
};
use openwebide_storage::db::{Db, DbValue};
async fn mutate(
    state: &AppState,
    user: UserId,
    project: i64,
    grant: Option<&WorkspaceJournalDelegation>,
    worker: &WorkspaceTransactionWorker,
    operation: CommandWorkerOperation,
) -> Result<WorkspaceCommandRecovery, ApiError> {
    let expected = operation.clone();
    let response = if let Some(grant) = grant {
        crate::api::language_actions::delegated(
            request(&DelegatedCommandRequest {
                delegation: grant.clone(),
                worker: worker.clone(),
                operation: CommandJournalOperation::Worker {
                    operation: Box::new(operation),
                },
            }),
            state,
        )
        .await?
    } else {
        workspace_transactions::command_service(
            request(&WorkspaceJournalRequest {
                user: user.get(),
                project,
                id: operation.expected().scope.id.clone(),
                operation: WorkspaceJournalOperation::Command {
                    operation: Box::new(operation),
                },
            }),
            state,
        )
        .await?
    };
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body.len() < openwebide_core::workspace_transaction::RECOVERY_SCOPE_BODY_LIMIT);
    assert!(!String::from_utf8_lossy(&body).contains(&worker.secret));
    let reply: WorkspaceJournalResponse = serde_json::from_slice(&body).unwrap();
    Ok(reply.command_worker(&expected).unwrap().clone())
}
#[test]
fn owned_command_callbacks_share_completion_and_revocation_in_both_modes() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let directory = std::env::temp_dir().join(format!(
                "owned-command-http-{:032x}",
                rand::random::<u128>()
            ));
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .unwrap();
            }
            #[cfg(not(unix))]
            std::fs::create_dir(&directory).unwrap();
            let path = directory.join("backend.sqlite");
            let store = openwebide_storage::Store::new(crate::state::AppDb::open(&path).unwrap());
            store.migrate_with(&crate::files::dir_exists).await.unwrap();
            let (state, user, project, scope) = fixture_in(AppState { store }, mode).await;
            let original = fetch(&state, user, project, &scope).await;
            state
                .store
                .advance_workspace_transaction(
                    user,
                    project,
                    &AdvanceWorkspaceTransaction {
                        id: scope.id.clone(),
                        generation: scope.generation.clone(),
                        revision: scope.revision,
                        progress: TransactionProgress::prepared_cancellation(),
                    },
                    now(),
                )
                .await
                .unwrap();
            let primitive = crate::state::AppDb::open(&path).unwrap();
            // Arrange an already-admitted original producer. Start/manual
            // approval policy is exercised by the real storage action fixtures.
            let invocation = CommandInvocation {
                id: scope.id.clone(),
                client: "window".into(),
                command: "b".repeat(32),
                server: "original-service".into(),
                origin: "c".repeat(64),
                review: "d".repeat(64),
                approval: "e".repeat(64),
            };
            primitive.execute("INSERT INTO language_actions(user_id,project_id,id,revision,server,command_id,approval,phase,bytes,created_at,updated_at,invocation,command_worker_id,command_worker_digest,native_settlement_pending) VALUES(?,?,?,2,'original-service',?,'unreadable original provider','running',0,0,0,?,?,?,1)",&[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(scope.id.clone()),DbValue::Text(invocation.command.clone()),DbValue::Text(serde_json::to_string(&invocation).unwrap()),DbValue::Text("f".repeat(32)),DbValue::Text("0".repeat(64))]).await.unwrap();
            let worker = WorkspaceTransactionWorker {
                id: "1".repeat(32),
                secret: "2".repeat(32),
            };
            let grant = if mode == WorkspaceMode::Local {
                let mut browser = request(&CommandDelegationRequest {
                    id: scope.id.clone(),
                    worker: worker.delegation_key(),
                });
                browser
                    .headers_mut()
                    .insert("cookie", "session=browser".parse().unwrap());
                let result = crate::api::language_actions::delegate(
                    browser,
                    &state,
                    &format!("/api/projects/{project}/language-commands/delegate"),
                    crate::auth::AuthedUser {
                        id: user,
                        role: UserRole::User,
                    },
                )
                .await
                .unwrap();
                assert_eq!(result.headers()["cache-control"], "no-store");
                let body = result.into_body().collect().await.unwrap().to_bytes();
                Some(serde_json::from_slice::<WorkspaceJournalDelegation>(&body).unwrap())
            } else {
                None
            };
            let before = state
                .store
                .workspace_command_recovery(user, project, &scope.id)
                .await
                .unwrap()
                .unwrap();
            let claimed = mutate(
                &state,
                user,
                project,
                grant.as_ref(),
                &worker,
                CommandWorkerOperation::Claim {
                    expected: Box::new(before),
                    worker: worker.clone(),
                    admission: CommandAdmission::Recover,
                },
            )
            .await
            .unwrap();
            assert_eq!(claimed.command.worker, Some(worker.state()));
            assert!(claimed.command.native_settlement_pending);
            if let Some(grant) = &grant {
                let input = DelegatedCommandRequest {
                    delegation: grant.clone(),
                    worker: worker.clone(),
                    operation: CommandJournalOperation::Scope,
                };
                let mut browser = request(&input);
                browser
                    .headers_mut()
                    .insert("origin", "https://unrelated.example".parse().unwrap());
                assert!(
                    crate::api::language_actions::delegated(browser, &state)
                        .await
                        .is_err()
                );
                let result = crate::api::language_actions::delegated(request(&input), &state)
                    .await
                    .unwrap();
                let body = result.into_body().collect().await.unwrap().to_bytes();
                let reply: WorkspaceJournalResponse = serde_json::from_slice(&body).unwrap();
                let (_, authority) = reply
                    .command_recovery(user.get(), project, &scope.id)
                    .unwrap();
                assert_eq!(*authority, original);
            }
            let outcome = CommandOutcome::Uncertain {
                message: "Original producer response lost".into(),
            };
            let finished = mutate(
                &state,
                user,
                project,
                grant.as_ref(),
                &worker,
                CommandWorkerOperation::Finish {
                    expected: Box::new(claimed),
                    worker: worker.clone(),
                    outcome,
                    cleaned: true,
                },
            )
            .await
            .unwrap();
            assert!(finished.command.command_cleanup_confirmed);
            assert!(finished.activity.language_command);
            // Arrange the separately tested user acknowledgement. No source or
            // provider payload is needed for original native settlement.
            primitive
                .execute(
                    "UPDATE language_actions SET phase='acknowledged',revision=revision+1",
                    &[],
                )
                .await
                .unwrap();
            let acknowledged = state
                .store
                .workspace_command_recovery(user, project, &scope.id)
                .await
                .unwrap()
                .unwrap();
            let settled = mutate(
                &state,
                user,
                project,
                grant.as_ref(),
                &worker,
                CommandWorkerOperation::Settle {
                    expected: Box::new(acknowledged.clone()),
                    worker: worker.clone(),
                },
            )
            .await
            .unwrap();
            assert!(settled.activity.ensure_idle().is_ok());
            assert_eq!(
                mutate(
                    &state,
                    user,
                    project,
                    grant.as_ref(),
                    &worker,
                    CommandWorkerOperation::Settle {
                        expected: Box::new(acknowledged),
                        worker: worker.clone()
                    }
                )
                .await
                .unwrap(),
                settled
            );
            if let Some(grant) = &grant {
                assert!(
                    crate::api::language_actions::delegated(
                        request(&DelegatedCommandRequest {
                            delegation: grant.clone(),
                            worker,
                            operation: CommandJournalOperation::Scope
                        }),
                        &state
                    )
                    .await
                    .is_err()
                );
            }
            println!(
                "retained_owned_command_http_fixture={}",
                directory.display()
            );
        }
    });
}
