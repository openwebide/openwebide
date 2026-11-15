use super::*;
use crate::{api::workspace_transactions, state::now};
use bytes::Bytes;
use http_body_util::BodyExt;
use openwebide_core::{
    NewProject, UserId, UserRole, WorkspaceMode,
    editor::EditorRecoveryRoot,
    workspace_transaction::{
        BeginWorkspaceTransaction, DelegateWorkspaceTransaction,
        DelegatedWorkspaceJournalOperation, DelegatedWorkspaceJournalRequest, EditorRecoveryPatch,
        EntryState, FileOperation, WorkspaceJournalOperation, WorkspaceJournalRequest,
        WorkspacePlan, WorkspaceTransactionWorker,
    },
};
use spin_sdk::http::{FullBody, Request};
mod original_command;

async fn fixture(mode: WorkspaceMode) -> (AppState, UserId, i64, WorkspaceTransactionScope) {
    let state = AppState::new().await.unwrap();
    fixture_in(state, mode).await
}

async fn fixture_in(
    state: AppState,
    mode: WorkspaceMode,
) -> (AppState, UserId, i64, WorkspaceTransactionScope) {
    let user = state
        .store
        .insert_user("owner", "hash", UserRole::User, 1)
        .await
        .unwrap();
    let project = state
        .store
        .create_project(
            &NewProject {
                name: "recovery".into(),
                mode,
                path: Some("original".into()),
            },
            user.id,
            1,
        )
        .await
        .unwrap();
    let input = BeginWorkspaceTransaction {
        id: "a".repeat(32),
        language_review: None,
        plan: WorkspacePlan {
            version: 1,
            root: "b".repeat(64),
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
    };
    state
        .store
        .begin_workspace_transaction(user.id, project.id, &input, now())
        .await
        .unwrap();
    let scope = state
        .store
        .workspace_transaction_scope(user.id, project.id, &input.id)
        .await
        .unwrap()
        .unwrap();
    (state, user.id, project.id, scope)
}

fn request(input: &impl serde::Serialize) -> Request<FullBody<Bytes>> {
    Request::builder()
        .method("POST")
        .uri("http://localhost/api")
        .body(FullBody::new(Bytes::from(
            serde_json::to_vec(input).unwrap(),
        )))
        .unwrap()
}

async fn fetch(
    state: &AppState,
    user: UserId,
    project: i64,
    scope: &WorkspaceTransactionScope,
) -> RecoveryAuthority {
    let response = if scope.root.mode == WorkspaceMode::Remote {
        workspace_transactions::service(
            request(&WorkspaceJournalRequest {
                user: user.get(),
                project,
                id: scope.id.clone(),
                operation: WorkspaceJournalOperation::RecoveryScope,
            }),
            state,
        )
        .await
        .unwrap()
    } else {
        let worker = WorkspaceTransactionWorker {
            id: "c".repeat(32),
            secret: "d".repeat(32),
        };
        let delegation = state
            .store
            .delegate_workspace_transaction(
                user,
                project,
                &scope.id,
                &DelegateWorkspaceTransaction {
                    generation: scope.generation.clone(),
                    revision: scope.revision,
                    worker: worker.delegation_key(),
                },
                now(),
            )
            .await
            .unwrap();
        workspace_transactions::delegated(
            request(&DelegatedWorkspaceJournalRequest {
                delegation,
                worker,
                operation: DelegatedWorkspaceJournalOperation::RecoveryScope,
            }),
            state,
        )
        .await
        .unwrap()
    };
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body.len() < openwebide_core::workspace_transaction::RECOVERY_SCOPE_BODY_LIMIT);
    let WorkspaceJournalResponse::RecoveryScope {
        scope: current,
        authority,
        activity,
    } = serde_json::from_slice(&body).unwrap()
    else {
        panic!("Missing private recovery authority")
    };
    assert_eq!(&current, scope);
    assert_eq!(
        activity,
        state
            .store
            .workspace_transaction_activity(user, project, &scope.id)
            .await
            .unwrap()
            .unwrap()
            .1
    );
    assert!(authority.matches_scope(user.get(), project, scope));
    *authority
}

#[test]
fn private_issuance_survives_worker_release_and_transport_rotation_in_both_modes() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (state, user, project, scope) = fixture(mode).await;
            let original = fetch(&state, user, project, &scope).await;
            assert_eq!(
                state
                    .store
                    .workspace_transaction_scope(user, project, &scope.id)
                    .await
                    .unwrap(),
                Some(scope.clone())
            );
            let worker = WorkspaceTransactionWorker {
                id: "e".repeat(32),
                secret: "f".repeat(32),
            };
            let admitted = state
                .store
                .claim_workspace_transaction_worker_reviewed(
                    user,
                    project,
                    &scope,
                    &worker,
                    None,
                    now(),
                )
                .await
                .unwrap();
            assert_eq!(original, fetch(&state, user, project, &admitted).await);
            state
                .store
                .release_workspace_transaction_worker(
                    user,
                    project,
                    &scope.id,
                    &scope.generation,
                    &worker,
                    now(),
                )
                .await
                .unwrap();
            for key in ["auth_secret", "bridge_secret_cache"] {
                state
                    .store
                    .set_setting(key, "rotated transport secret")
                    .await
                    .unwrap();
            }
            let released = state
                .store
                .workspace_transaction_scope(user, project, &scope.id)
                .await
                .unwrap()
                .unwrap();
            assert!(!released.worker.as_ref().unwrap().active);
            assert_eq!(original, fetch(&state, user, project, &released).await);
            let settings = state.store.all_user_settings(user).await.unwrap();
            assert!(!settings.contains_key(SECRET_KEY));
            assert!(
                !serde_json::to_string(&settings)
                    .unwrap()
                    .contains(&original.secret)
            );
            let public = serde_json::to_string(
                &state
                    .store
                    .workspace_transaction(user, project, Some(&scope.id))
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert!(!public.contains(&original.secret));
            let master = state.store.get_setting(SECRET_KEY).await.unwrap().unwrap();
            assert!(!public.contains(&master));
            assert_ne!(original.secret, master);
            // A fresh request state uses only the persistent store, with no
            // in-memory issuer cache carried between calls.
            let next_request = AppState { store: state.store };
            assert_eq!(
                original,
                fetch(&next_request, user, project, &released).await
            );
        }
    });
}

#[test]
fn identical_public_claims_in_another_backend_cannot_reproduce_the_owner() {
    futures::executor::block_on(async {
        let (first, user, project, scope) = fixture(WorkspaceMode::Remote).await;
        let second = AppState::new().await.unwrap();
        let WorkspaceJournalResponse::RecoveryScope {
            authority: original,
            ..
        } = issue(&first, user.get(), project, Some(scope.clone()))
            .await
            .unwrap()
        else {
            unreachable!()
        };
        // A different backend without this owned record cannot issue from
        // supplied public claims. Even its independently created issuer key
        // cannot reproduce the original private credential for identical claims.
        assert!(
            issue(&second, user.get(), project, Some(scope.clone()))
                .await
                .is_err()
        );
        let key = original_secret(&second).await.unwrap();
        let other = credential(user.get(), project, &scope, &key).unwrap();
        assert!(original.matches_scope(user.get(), project, &scope));
        assert!(other.matches_scope(user.get(), project, &scope));
        assert_ne!(
            original.secret_digest().unwrap(),
            other.secret_digest().unwrap()
        );
    });
}

#[test]
fn concurrent_issuer_initialization_keeps_one_namespace_and_corruption_never_rotates_it() {
    futures::executor::block_on(async {
        let state = AppState::new().await.unwrap();
        let barrier = std::sync::Barrier::new(8);
        let values = std::thread::scope(|scope| {
            let tasks = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        futures::executor::block_on(original_secret(&state)).unwrap()
                    })
                })
                .collect::<Vec<_>>();
            tasks
                .into_iter()
                .map(|task| task.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(values.iter().all(|value| value == &values[0]));
        for invalid in [
            "secret-must-not-appear-in-an-error".into(),
            URL_SAFE_NO_PAD.encode([0u8; 31]),
            "?".repeat(43),
        ] {
            state.store.set_setting(SECRET_KEY, &invalid).await.unwrap();
            let error = original_secret(&state).await.unwrap_err();
            assert!(!format!("{error:?}").contains(&invalid));
            assert_eq!(error.into_response().status(), 500);
            assert_eq!(
                state
                    .store
                    .get_setting(SECRET_KEY)
                    .await
                    .unwrap()
                    .as_deref(),
                Some(invalid.as_str())
            );
        }
    });
}

#[test]
fn unauthorized_or_missing_remote_journals_do_not_initialize_an_issuer() {
    futures::executor::block_on(async {
        let (state, user, project, scope) = fixture(WorkspaceMode::Remote).await;
        let other = state
            .store
            .insert_user("other", "hash", UserRole::User, 1)
            .await
            .unwrap();
        for (actor, id, expected) in [
            (other.id.get(), scope.id, 404),
            (user.get(), "0".repeat(32), 404),
        ] {
            let result = workspace_transactions::service(
                request(&WorkspaceJournalRequest {
                    user: actor,
                    project,
                    id,
                    operation: WorkspaceJournalOperation::RecoveryScope,
                }),
                &state,
            )
            .await
            .unwrap_err();
            assert_eq!(result.into_response().status(), expected);
            assert!(state.store.get_setting(SECRET_KEY).await.unwrap().is_none());
        }
    });
}

#[test]
fn paired_issuance_requires_the_private_worker_and_rejects_browser_transport() {
    futures::executor::block_on(async {
        let (state, user, project, scope) = fixture(WorkspaceMode::Local).await;
        let worker = WorkspaceTransactionWorker {
            id: "c".repeat(32),
            secret: "d".repeat(32),
        };
        let delegation = state
            .store
            .delegate_workspace_transaction(
                user,
                project,
                &scope.id,
                &DelegateWorkspaceTransaction {
                    generation: scope.generation.clone(),
                    revision: scope.revision,
                    worker: worker.delegation_key(),
                },
                now(),
            )
            .await
            .unwrap();
        let input = DelegatedWorkspaceJournalRequest {
            delegation,
            worker,
            operation: DelegatedWorkspaceJournalOperation::RecoveryScope,
        };
        let mut forged = input.clone();
        forged.worker.secret = "e".repeat(32);
        assert_eq!(
            workspace_transactions::delegated(request(&forged), &state)
                .await
                .unwrap_err()
                .into_response()
                .status(),
            409
        );
        for header in ["origin", "sec-fetch-site", "cookie"] {
            let mut browser = request(&input);
            browser
                .headers_mut()
                .insert(header, "present".parse().unwrap());
            assert_eq!(
                workspace_transactions::delegated(browser, &state)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                403
            );
        }
        assert!(state.store.get_setting(SECRET_KEY).await.unwrap().is_none());
        // The deployment service transport cannot issue Local credentials.
        assert_eq!(
            workspace_transactions::service(
                request(&WorkspaceJournalRequest {
                    user: user.get(),
                    project,
                    id: scope.id,
                    operation: WorkspaceJournalOperation::RecoveryScope,
                }),
                &state
            )
            .await
            .unwrap_err()
            .into_response()
            .status(),
            400
        );
        assert!(state.store.get_setting(SECRET_KEY).await.unwrap().is_none());
    });
}

#[test]
fn original_namespace_and_credential_survive_a_closed_and_reopened_database() {
    futures::executor::block_on(async {
        async fn open(path: &std::path::Path) -> AppState {
            let store = openwebide_storage::Store::new(crate::state::AppDb::open(path).unwrap());
            store.migrate_with(&crate::files::dir_exists).await.unwrap();
            AppState { store }
        }
        // Retain this private fixture as evidence rather than unlinking its
        // database. The override only selects test evidence output, not any
        // production authority or application state directory.
        let parent = std::env::var_os("OPENWEBIDE_VERIFICATION_EVIDENCE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let directory = parent.join(format!(
            "recovery-issuer-reopen-{}-{:032x}",
            std::process::id(),
            rand::random::<u128>(),
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
        let database = directory.join("backend.sqlite");
        let (state, user, project, scope) =
            fixture_in(open(&database).await, WorkspaceMode::Remote).await;
        let original = fetch(&state, user, project, &scope).await;
        let master = state.store.get_setting(SECRET_KEY).await.unwrap();
        for key in ["auth_secret", "bridge_secret_cache"] {
            state
                .store
                .set_setting(key, "rotated before database close")
                .await
                .unwrap();
        }
        drop(state);

        let reopened = open(&database).await;
        assert_eq!(
            master,
            reopened.store.get_setting(SECRET_KEY).await.unwrap()
        );
        assert_eq!(
            Some(scope.clone()),
            reopened
                .store
                .workspace_transaction_scope(user, project, &scope.id)
                .await
                .unwrap()
        );
        assert_eq!(original, fetch(&reopened, user, project, &scope).await);
        std::fs::write(
            directory.join("reopen-proof.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "database_closed_and_reopened": true,
                "same_private_namespace_and_owner": true,
                "authority_digest": original.secret_digest().unwrap(),
                "transport_secrets_rotated": true,
                "database": database,
                "native_filesystem_registry_or_phase_authority_proven": false,
            }))
            .unwrap(),
        )
        .unwrap();
    });
}

#[test]
fn issuance_rechecks_the_authorized_scope_before_observing_compound_recovery() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (state, user, project, scope) = fixture(mode).await;
            let original = fetch(&state, user, project, &scope).await;
            let worker = WorkspaceTransactionWorker {
                id: "1".repeat(32),
                secret: "2".repeat(32),
            };
            let current = state
                .store
                .claim_workspace_transaction_worker_reviewed(
                    user,
                    project,
                    &scope,
                    &worker,
                    None,
                    now(),
                )
                .await
                .unwrap();
            assert!(
                issue(&state, user.get(), project, Some(scope.clone()))
                    .await
                    .is_err(),
                "A previously authorized public scope cannot supply a fresh phase observation"
            );
            let WorkspaceJournalResponse::RecoveryScope {
                scope: observed,
                authority,
                activity,
            } = issue(&state, user.get(), project, Some(current.clone()))
                .await
                .unwrap()
            else {
                unreachable!()
            };
            assert_eq!(observed, current);
            assert_eq!(*authority, original);
            assert!(activity.file_recovery);
            assert!(!activity.language_command);
            assert!(
                serde_json::to_vec(&WorkspaceJournalResponse::RecoveryScope {
                    scope: observed,
                    authority,
                    activity
                })
                .unwrap()
                .len()
                    < openwebide_core::workspace_transaction::RECOVERY_SCOPE_BODY_LIMIT
            );
        }
    });
}

#[test]
fn private_transports_retain_command_activity_after_the_file_phase_finishes() {
    futures::executor::block_on(async {
        use openwebide_core::workspace_transaction::{
            AdvanceWorkspaceTransaction, TransactionProgress, WorkspaceActivity,
        };
        use openwebide_storage::db::{Db, DbValue};
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let directory = std::env::temp_dir().join(format!(
                "compound-recovery-issuer-{:032x}",
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
            let current = state
                .store
                .workspace_transaction_scope(user, project, &scope.id)
                .await
                .unwrap()
                .unwrap();
            let primitive = crate::state::AppDb::open(&path).unwrap();
            // Arrange each durable command column without requiring a live
            // provider or deserializable old command/file payloads. This tests
            // recovery observation, not command dispatch authorization.
            primitive
                .execute(
                    "UPDATE workspace_transactions SET plan='invalid',editor='invalid'",
                    &[],
                )
                .await
                .unwrap();
            primitive.execute("INSERT INTO language_actions (user_id,project_id,id,revision,server,command_id,approval,phase,bytes,created_at,updated_at) VALUES (?,?,?,1,'original','command','invalid','approved',0,0,0)", &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(scope.id.clone())]).await.unwrap();
            for (phase, pending) in [
                ("approved", true),
                ("running", true),
                ("rejected", true),
                ("uncertain", true),
                ("completed", false),
                ("acknowledged", false),
                ("cancelled", false),
            ] {
                primitive
                    .execute(
                        "UPDATE language_actions SET phase=?",
                        &[DbValue::Text(phase.into())],
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    state
                        .store
                        .workspace_transaction_activity(user, project, &scope.id)
                        .await
                        .unwrap()
                        .unwrap()
                        .1,
                    WorkspaceActivity {
                        file_recovery: false,
                        language_command: pending
                    }
                );
                // fetch checks the actual private HTTP reply against this exact
                // observation, including true, through both native transports.
                assert_eq!(fetch(&state, user, project, &current).await, original);
            }
            println!("retained_compound_issuer_fixture={}", directory.display());
        }
    });
}

#[test]
fn private_command_identity_uses_original_issuer_without_reading_frozen_payloads() {
    futures::executor::block_on(async {
        use openwebide_core::plugins::languages::intelligence::actions::{
            ActionPhase, CommandInvocation,
        };
        use openwebide_core::workspace_transaction::{
            AdvanceWorkspaceTransaction, TransactionProgress,
        };
        use openwebide_storage::db::{Db, DbValue};
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let directory = std::env::temp_dir().join(format!(
                "command-recovery-issuer-{:032x}",
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
            let current = state
                .store
                .workspace_transaction_scope(user, project, &scope.id)
                .await
                .unwrap()
                .unwrap();
            let primitive = crate::state::AppDb::open(&path).unwrap();
            let invocation = CommandInvocation {
                id: scope.id.clone(),
                client: "window".into(),
                command: "e".repeat(32),
                server: "original-service".into(),
                origin: "a".repeat(64),
                review: "b".repeat(64),
                approval: "c".repeat(64),
            };
            primitive.execute("INSERT INTO language_actions (user_id,project_id,id,revision,server,command_id,approval,phase,bytes,created_at,updated_at,invocation) VALUES (?,?,?,1,'original-service',?,'unreadable','approved',0,0,0,?)",&[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(scope.id.clone()),DbValue::Text(invocation.command.clone()),DbValue::Text(serde_json::to_string(&invocation).unwrap())]).await.unwrap();
            primitive.execute("UPDATE workspace_transactions SET plan='unreadable',editor='unreadable',language_review='unreadable'",&[]).await.unwrap();
            for phase in [
                ActionPhase::Approved,
                ActionPhase::Running,
                ActionPhase::Completed,
                ActionPhase::Rejected,
                ActionPhase::Uncertain,
                ActionPhase::Acknowledged,
                ActionPhase::Cancelled,
            ] {
                let digest = if matches!(
                    phase,
                    ActionPhase::Approved | ActionPhase::Running | ActionPhase::Cancelled
                ) {
                    None
                } else {
                    Some("d".repeat(64))
                };
                primitive.execute("UPDATE language_actions SET phase=?,revision=revision+1,outcome='unreadable',outcome_digest=?",&[DbValue::Text(serde_json::to_value(phase).unwrap().as_str().unwrap().into()),digest.clone().map_or(DbValue::Null,DbValue::Text)]).await.unwrap();
                let response = if mode == WorkspaceMode::Remote {
                    workspace_transactions::service(
                        request(&WorkspaceJournalRequest {
                            user: user.get(),
                            project,
                            id: scope.id.clone(),
                            operation: WorkspaceJournalOperation::CommandRecoveryScope,
                        }),
                        &state,
                    )
                    .await
                    .unwrap()
                } else {
                    let worker = WorkspaceTransactionWorker {
                        id: format!("{:032x}", rand::random::<u128>()),
                        secret: format!("{:032x}", rand::random::<u128>()),
                    };
                    let delegation = state
                        .store
                        .delegate_workspace_transaction(
                            user,
                            project,
                            &scope.id,
                            &DelegateWorkspaceTransaction {
                                generation: current.generation.clone(),
                                revision: current.revision,
                                worker: worker.delegation_key(),
                            },
                            now(),
                        )
                        .await
                        .unwrap();
                    let input = DelegatedWorkspaceJournalRequest {
                        delegation,
                        worker,
                        operation: DelegatedWorkspaceJournalOperation::CommandRecoveryScope,
                    };
                    let mut browser = request(&input);
                    browser
                        .headers_mut()
                        .insert("origin", "https://unrelated.example".parse().unwrap());
                    assert!(
                        workspace_transactions::delegated(browser, &state)
                            .await
                            .is_err()
                    );
                    workspace_transactions::delegated(request(&input), &state)
                        .await
                        .unwrap()
                };
                assert_eq!(response.headers()["cache-control"], "no-store");
                let body = response.into_body().collect().await.unwrap().to_bytes();
                assert!(
                    body.len() < openwebide_core::workspace_transaction::RECOVERY_SCOPE_BODY_LIMIT
                );
                let reply: WorkspaceJournalResponse = serde_json::from_slice(&body).unwrap();
                let (recovery, authority) = reply
                    .command_recovery(user.get(), project, &scope.id)
                    .unwrap();
                assert_eq!(*authority, original);
                assert_eq!(recovery.scope, current);
                assert_eq!(recovery.command.invocation, invocation);
                assert_eq!(recovery.command.phase, phase);
                assert_eq!(recovery.command.outcome_digest, digest);
                assert_eq!(recovery.activity.language_command, !phase.terminal());
                assert!(!recovery.activity.file_recovery);
                assert!(!format!("{reply:?}").contains(&authority.secret));
                assert!(
                    reply
                        .command_recovery(user.get() + 1, project, &scope.id)
                        .is_err()
                );
                assert!(
                    reply
                        .command_recovery(user.get(), project + 1, &scope.id)
                        .is_err()
                );
            }
            let earlier = state
                .store
                .workspace_command_recovery(user, project, &scope.id)
                .await
                .unwrap()
                .unwrap();
            primitive.execute("UPDATE language_actions SET phase='running',revision=revision+1,outcome_digest=NULL",&[]).await.unwrap();
            assert!(
                issue_command(&state, user.get(), project, Some(earlier))
                    .await
                    .is_err()
            );
            if mode == WorkspaceMode::Local {
                let worker = WorkspaceTransactionWorker {
                    id: "7".repeat(32),
                    secret: "8".repeat(32),
                };
                let delegation = state
                    .store
                    .delegate_workspace_transaction(
                        user,
                        project,
                        &scope.id,
                        &DelegateWorkspaceTransaction {
                            generation: current.generation.clone(),
                            revision: current.revision,
                            worker: worker.delegation_key(),
                        },
                        now(),
                    )
                    .await
                    .unwrap();
                let mut input = DelegatedWorkspaceJournalRequest {
                    delegation,
                    worker,
                    operation: DelegatedWorkspaceJournalOperation::Admit {
                        command: None,
                        review_digest: None,
                        language_grant: None,
                    },
                };
                workspace_transactions::delegated(request(&input), &state)
                    .await
                    .unwrap();
                input.operation = DelegatedWorkspaceJournalOperation::Release;
                workspace_transactions::delegated(request(&input), &state)
                    .await
                    .unwrap();
                input.operation = DelegatedWorkspaceJournalOperation::CommandRecoveryScope;
                assert!(
                    workspace_transactions::delegated(request(&input), &state)
                        .await
                        .is_err()
                );
            }
            println!(
                "retained_command_recovery_issuer_fixture={}",
                directory.display()
            );
        }
    });
}

mod command_workers;
