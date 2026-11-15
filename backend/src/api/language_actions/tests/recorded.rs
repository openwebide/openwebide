use super::*;
use openwebide_core::{
    editor::EditorRecoveryRoot,
    plugins::languages::{
        intelligence::{
            self, Query,
            actions::{
                ActionApproval, ActionReview, AuthorizedCommand, CommandOperation, CommandOutcome,
                ServerCommand,
            },
            results::WorkspaceEdit,
        },
        source_digest,
    },
    workspace_transaction::{
        BeginWorkspaceTransaction, EditorRecoveryPatch, EntryState, FileOperation, WorkspacePlan,
    },
};
use openwebide_storage::db::{Db, DbValue};

#[test]
fn recorded_results_do_not_require_a_connected_host_in_both_modes() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let directory = std::env::temp_dir()
                .join(format!("recorded-command-{:032x}", rand::random::<u128>()));
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
            let path = directory.join("recorded.db");
            let primitive = crate::state::AppDb::open(&path).unwrap();
            let state = AppState {
                store: openwebide_storage::Store::new(crate::state::AppDb::open(&path).unwrap()),
            };
            state.store.migrate().await.unwrap();
            let owner = state
                .store
                .insert_user("owner", "hash", UserRole::Admin, 1)
                .await
                .unwrap();
            let other = state
                .store
                .insert_user("other", "hash", UserRole::User, 1)
                .await
                .unwrap();
            let project = state
                .store
                .create_project(
                    &NewProject {
                        name: "commands".into(),
                        mode,
                        path: Some("original".into()),
                    },
                    owner.id,
                    1,
                )
                .await
                .unwrap();
            let mut prepared = openwebide_core::plugins::testing::receipt();
            prepared.manifest.compatibility.plugin_api = 9;
            prepared.manifest.executable = None;
            prepared.manifest.contributions.skills.clear();
            prepared.manifest.contributions.tools.clear();
            prepared.manifest.contributions.languages = serde_json::from_value(json!([{
                "id":"custom", "name":"Custom", "extensions":["custom"],
                "features":{"languageServer":{"command":"custom-lsp"}}
            }]))
            .unwrap();
            let id = "a".repeat(32);
            let approval = ActionApproval {
                id: id.clone(),
                prepared,
                binding_revision: 1,
                input: intelligence::Request {
                    version: intelligence::VERSION,
                    client: "old-window".into(),
                    document: "tab".into(),
                    path: "main.custom".into(),
                    language: "custom".into(),
                    request: 1,
                    revision: 1,
                    source: "source".into(),
                    source_digest: source_digest("source"),
                    query: Query::ReviewAction { id: "b".repeat(32) },
                },
                review: ActionReview {
                    title: "Original command".into(),
                    edit: WorkspaceEdit {
                        files: vec![],
                        changes: vec![],
                    },
                    command: Some(ServerCommand {
                        id: "b".repeat(32),
                        server: "old-host".into(),
                        name: "custom.apply".into(),
                        arguments: vec![],
                    }),
                },
            };
            approval.validate().unwrap();
            state
                .store
                .begin_workspace_transaction(
                    owner.id,
                    project.id,
                    &BeginWorkspaceTransaction {
                        id: id.clone(),
                        language_review: None,
                        plan: WorkspacePlan {
                            version: 1,
                            root: "c".repeat(64),
                            operations: vec![FileOperation::Create {
                                path: "new".into(),
                                overwrite: false,
                                ignore_if_exists: false,
                            }],
                            roots: vec!["new".into()],
                            before: [("new".into(), EntryState::Missing)].into(),
                            after: [("new".into(), EntryState::file(Vec::new()))].into(),
                        },
                        editor: EditorRecoveryPatch {
                            revision: 0,
                            root: EditorRecoveryRoot::for_project(&project),
                            before: vec![],
                            after: vec![],
                            order_before: vec![],
                            order_after: vec![],
                            selected_before: None,
                            selected_after: None,
                        },
                    },
                    1,
                )
                .await
                .unwrap();
            // No provider installation, execution-host setting, native process
            // or manual grant exists in this transport fixture.
            primitive.execute("INSERT INTO language_actions(user_id,project_id,id,revision,server,command_id,approval,phase,outcome,bytes,created_at,updated_at) VALUES(?,?,?,2,'old-host',?,?,'completed',?,0,1,1)", &[
                DbValue::Int(owner.id.get()), DbValue::Int(project.id), DbValue::Text(id.clone()),
                DbValue::Text("b".repeat(32)), DbValue::Text(serde_json::to_string(&approval).unwrap()),
                DbValue::Text(serde_json::to_string(&CommandOutcome::Completed { result: json!({"original":"文😀"}) }).unwrap()),
            ]).await.unwrap();
            for outcome in [
                CommandOutcome::Completed {
                    result: json!({"original":"文😀"}),
                },
                CommandOutcome::Rejected {
                    message: "Never sent".into(),
                },
                CommandOutcome::Uncertain {
                    message: "Response lost".into(),
                },
            ] {
                for cleaned in [false, true] {
                    primitive.execute("UPDATE language_actions SET phase=?,outcome=?,command_cleanup_confirmed=?,command_worker_id=?,outcome_digest=?", &[
                        DbValue::Text(serde_json::to_value(outcome.phase()).unwrap().as_str().unwrap().into()),
                        DbValue::Text(serde_json::to_string(&outcome).unwrap()), DbValue::Int(i64::from(cleaned)),
                        DbValue::Text(if cleaned {"d".repeat(32)} else {String::new()}),
                        DbValue::Text(openwebide_core::plugins::content_digest(&serde_json::to_vec(&outcome).unwrap())),
                    ]).await.unwrap();
                    for operation in [CommandOperation::Status, CommandOperation::Stop] {
                        for authorize_only in [false, true] {
                            let path = format!(
                                "/api/projects/{}/language-commands{}",
                                project.id,
                                if authorize_only { "/authorize" } else { "" }
                            );
                            let body =
                                json!({"id":id,"operation":operation,"call":null}).to_string();
                            assert_eq!(
                                dispatch(
                                    request(&body, body.len()),
                                    &state,
                                    &path,
                                    AuthedUser {
                                        id: other.id,
                                        role: other.role
                                    },
                                    authorize_only
                                )
                                .await
                                .unwrap_err()
                                .into_response()
                                .status(),
                                404
                            );
                            let response = dispatch(
                                request(&body, body.len()),
                                &state,
                                &path,
                                AuthedUser {
                                    id: owner.id,
                                    role: owner.role,
                                },
                                authorize_only,
                            )
                            .await;
                            let known = operation == CommandOperation::Status || cleaned;
                            if authorize_only {
                                let body = response
                                    .unwrap()
                                    .into_body()
                                    .collect()
                                    .await
                                    .unwrap()
                                    .to_bytes();
                                let value: AuthorizedCommand =
                                    serde_json::from_slice(&body).unwrap();
                                assert_eq!(
                                    value.invocation,
                                    approval.command_invocation().unwrap()
                                );
                                assert!(value.selection.is_none());
                                primitive.execute("UPDATE language_actions SET approval='unreadable original provider/source'",&[]).await.unwrap();
                                let recovery_path = format!(
                                    "/api/projects/{}/language-actions/recovery",
                                    project.id
                                );
                                let body = json!({"operation":"load","id":id}).to_string();
                                let response = recovery(
                                    request(&body, body.len()),
                                    &state,
                                    &recovery_path,
                                    AuthedUser {
                                        id: owner.id,
                                        role: owner.role,
                                    },
                                )
                                .await
                                .unwrap();
                                assert_eq!(response.headers()["cache-control"], "no-store");
                                let bytes =
                                    response.into_body().collect().await.unwrap().to_bytes();
                                let recovered:Option<openwebide_core::plugins::languages::intelligence::actions::ActionRecovery>=serde_json::from_slice(&bytes).unwrap();
                                let recovered = recovered.unwrap();
                                recovered.validate().unwrap();
                                assert_eq!(recovered.command.invocation, value.invocation);
                                assert_eq!(recovered.outcome, Some(outcome.clone()));
                                assert_eq!(recovered.command.command_cleanup_confirmed, cleaned);
                                assert_eq!(
                                    recovery(
                                        request(&body, body.len()),
                                        &state,
                                        &recovery_path,
                                        AuthedUser {
                                            id: other.id,
                                            role: other.role
                                        }
                                    )
                                    .await
                                    .unwrap_err()
                                    .into_response()
                                    .status(),
                                    404
                                );

                                assert_eq!(
                                    value.progress,
                                    known.then(|| CommandProgress::Finished {
                                        outcome: outcome.clone()
                                    })
                                );
                            } else if known {
                                let body = response
                                    .unwrap()
                                    .into_body()
                                    .collect()
                                    .await
                                    .unwrap()
                                    .to_bytes();
                                assert_eq!(
                                    serde_json::from_slice::<CommandProgress>(&body).unwrap(),
                                    CommandProgress::Finished {
                                        outcome: outcome.clone()
                                    }
                                );
                            } else {
                                // An uncertain cleanup must still fence the
                                // original host. Its absence cannot imply success.
                                assert!(response.is_err());
                            }
                        }
                    }
                }
            }
            let scope = state
                .store
                .workspace_transaction_scope(owner.id, project.id, &id)
                .await
                .unwrap()
                .unwrap();
            state.store.advance_workspace_transaction(owner.id,project.id,&openwebide_core::workspace_transaction::AdvanceWorkspaceTransaction {id:id.clone(),generation:scope.generation,revision:scope.revision,progress:openwebide_core::workspace_transaction::TransactionProgress::prepared_cancellation()},2).await.unwrap();
            let recovery_path = format!("/api/projects/{}/language-actions/recovery", project.id);
            let body=json!({"operation":"acknowledge","id":id,"revision":2,"note":"Reviewed original effects"}).to_string();
            let response = recovery(
                request(&body, body.len()),
                &state,
                &recovery_path,
                AuthedUser {
                    id: owner.id,
                    role: owner.role,
                },
            )
            .await
            .unwrap();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let acknowledged: Option<
                openwebide_core::plugins::languages::intelligence::actions::ActionRecovery,
            > = serde_json::from_slice(&bytes).unwrap();
            assert!(!acknowledged.unwrap().pending());
        }
    });
}
