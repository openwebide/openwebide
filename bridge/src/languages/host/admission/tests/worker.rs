//! Real original-service verification followed by the durable worker boundary.
use super::*;
use crate::workspace_transactions::{Host as Transactions, JournalBackend};
use openwebide_core::{
    NewProject, UserId, UserRole,
    plugins::{PluginPackage, RecordPlugin, execution::PluginExecutionContext},
    workspace_transaction::{
        AdvanceWorkspaceTransaction, BeginWorkspaceTransaction, ExecuteWorkspaceTransaction,
        NativeWorkspaceTransaction, ResourceScope, ResourceScript, ResourceStep, TransactionFiles,
        TransactionPhase, WorkspaceJournalOperation as Operation, WorkspaceJournalRequest,
        WorkspaceJournalResponse as Reply, WorkspaceTransactionAction, WorkspaceTransactionWorker,
    },
};
use openwebide_storage::{Store, rusqlite_db::RusqliteDb};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

mod paired;
mod recovery_identity;

struct Backend {
    store: Store<RusqliteDb>,
    revoke_on_claim: AtomicBool,
    now: AtomicI64,
    intent_count: std::sync::atomic::AtomicUsize,
    replace_on_intent: std::sync::Mutex<Option<(std::path::PathBuf, std::path::PathBuf)>>,
}
impl Backend {
    fn after_progress(&self, progress: &TransactionProgress) {
        if progress.active {
            self.intent_count.fetch_add(1, Ordering::SeqCst);
        }
        if progress.active
            && let Some((path, saved)) = self.replace_on_intent.lock().unwrap().take()
        {
            let bytes = std::fs::read(&path).unwrap();
            std::fs::rename(&path, saved).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
    }
}
impl JournalBackend for Backend {
    async fn request(&self, request: &WorkspaceJournalRequest) -> Result<Reply, String> {
        // Thin SQLite test transport, with real authority and CAS. This adapter
        // supplies no successful native verification.
        futures::executor::block_on(async {
            let user = UserId::new(request.user);
            let project = request.project;
            let now = self.now.load(Ordering::SeqCst);
            match &request.operation {
                Operation::Scope => Ok(Reply::Scope {
                    scope: self
                        .store
                        .workspace_transaction_scope(user, project, &request.id)
                        .await?,
                }),
                Operation::Load => Ok(Reply::Loaded {
                    state: self
                        .store
                        .workspace_transaction(user, project, Some(&request.id))
                        .await?
                        .map(Box::new),
                }),
                Operation::ClaimWorker {
                    scope,
                    worker,
                    review_digest,
                    language_grant,
                } => {
                    if self.revoke_on_claim.swap(false, Ordering::SeqCst) {
                        self.store
                            .set_user_setting(
                                user,
                                "language_settings",
                                r#"{"defaults":{"providers":{"notes":"disabled"}}}"#,
                            )
                            .await?;
                    }
                    Ok(Reply::WorkerClaimed {
                        scope: self
                            .store
                            .claim_workspace_transaction_worker_reviewed(
                                user,
                                project,
                                scope,
                                worker,
                                review_digest.as_deref().zip(language_grant.as_deref()),
                                now,
                            )
                            .await?,
                    })
                }
                Operation::ReleaseWorker { generation, worker } => {
                    self.store
                        .release_workspace_transaction_worker(
                            user,
                            project,
                            &request.id,
                            generation,
                            worker,
                            now,
                        )
                        .await?;
                    Ok(Reply::WorkerReleased {
                        generation: generation.clone(),
                        worker: worker.id.clone(),
                    })
                }
                Operation::AdvanceOwned {
                    worker,
                    generation,
                    revision,
                    progress,
                } => {
                    let response = self
                        .store
                        .advance_workspace_transaction_for_worker(
                            user,
                            project,
                            &AdvanceWorkspaceTransaction {
                                id: request.id.clone(),
                                generation: generation.clone(),
                                revision: *revision,
                                progress: progress.clone(),
                            },
                            Some(worker),
                            now,
                        )
                        .await?;
                    self.after_progress(progress);
                    Ok(Reply::Advanced { response })
                }
                Operation::Advance { .. } => Err(openwebide_storage::StorageError::Conflict(
                    "Owned worker required".into(),
                )),
            }
        })
        .map_err(|error| error.to_string())
    }
}
async fn setup(
    approved: bool,
) -> (
    Fixture,
    WorkspaceTransactionState,
    Arc<Backend>,
    NativeWorkspaceTransaction,
) {
    let mut fixture = Fixture::with_mode("user:1", "inputs");
    std::fs::write(
        fixture.root.path().join("settings-old.json"),
        "{\"original\":true}",
    )
    .unwrap();
    let state = fixture.state().await;
    fixture.prepare(&state).await;
    store_review(fixture, state, approved).await
}
async fn store_review(
    fixture: Fixture,
    mut state: WorkspaceTransactionState,
    approved: bool,
) -> (
    Fixture,
    WorkspaceTransactionState,
    Arc<Backend>,
    NativeWorkspaceTransaction,
) {
    let store = Store::new(RusqliteDb::open_in_memory().unwrap());
    store.migrate().await.unwrap();
    let user = store
        .insert_user("owner", "hash", UserRole::Admin, 0)
        .await
        .unwrap()
        .id;
    let project = store
        .create_project(
            &NewProject {
                name: "reviewed".into(),
                mode: state.editor.root.mode,
                path: state.editor.root.path.clone(),
            },
            user,
            0,
        )
        .await
        .unwrap();
    assert_eq!(user.get(), 1);
    assert_eq!(project.id, fixture.scope.project);
    store
        .save_editor_recovery(
            user,
            project.id,
            0,
            &EditorRecovery {
                format: 1,
                root: Some(state.editor.root.clone()),
                files: state.editor.before.clone(),
                selected: state.editor.selected_before.clone(),
            },
        )
        .await
        .unwrap();
    store
        .record_plugin(
            user,
            &RecordPlugin {
                prepared: fixture.prepared.clone(),
                package: Some(Box::new(PluginPackage {
                    prepared: fixture.prepared.clone(),
                    skills: vec![],
                })),
                approved_capabilities: vec![],
                revision: None,
                update_policy: None,
            },
            100,
        )
        .await
        .unwrap();
    let binding = store
        .project_plugins(user, project.id)
        .await
        .unwrap()
        .into_iter()
        .find(|binding| binding.prepared.source == fixture.prepared.source)
        .unwrap();
    assert_eq!(
        binding.revision,
        state.language_review.as_ref().unwrap().binding_revision
    );
    let grant = "d".repeat(32);
    store
        .issue_plugin_context_grant(
            user,
            &PluginExecutionContext {
                project_id: Some(project.id),
                user_action: true,
                ..Default::default()
            },
            &fixture.prepared,
            &grant,
            100,
        )
        .await
        .unwrap();
    let prepared = store
        .begin_workspace_transaction(
            user,
            project.id,
            &BeginWorkspaceTransaction {
                id: state.journal.id.clone(),
                plan: state.journal.plan.clone(),
                editor: state.editor.clone(),
                language_review: state.language_review.clone(),
            },
            100,
        )
        .await
        .unwrap();
    state.journal.generation = prepared.journal.generation.clone();
    if approved {
        approve(&store, user, project.id, &state, &grant).await;
    }
    let command = NativeWorkspaceTransaction {
        user: user.get(),
        project: project.id,
        request: ExecuteWorkspaceTransaction {
            id: state.journal.id.clone(),
            generation: prepared.journal.generation,
            action: WorkspaceTransactionAction::Apply,
            language_grant: Some(grant),
        },
    };
    (
        fixture,
        state,
        Arc::new(Backend {
            store,
            revoke_on_claim: AtomicBool::new(false),
            now: AtomicI64::new(101),
            intent_count: Default::default(),
            replace_on_intent: Default::default(),
        }),
        command,
    )
}

async fn approve(
    store: &Store<RusqliteDb>,
    user: UserId,
    project: i64,
    state: &WorkspaceTransactionState,
    grant: &str,
) {
    let review = state.language_review.as_ref().unwrap();
    store
        .approve_language_action(
            user,
            project,
            &protocol::Call {
                prepared: review.prepared.clone(),
                input: review.input.clone(),
                grant: grant.into(),
                session_id: None,
            },
            &protocol::actions::ActionApproval {
                id: state.journal.id.clone(),
                prepared: review.prepared.clone(),
                binding_revision: review.binding_revision,
                input: review.input.clone(),
                review: serde_json::from_value(review.result.clone()).unwrap(),
            },
            100,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn current_authority_at_claim_rejects_after_real_native_verification_then_original_review_can_retry()
 {
    let (fixture, state, backend, mut command) = setup(false).await;
    backend.revoke_on_claim.store(true, Ordering::SeqCst);
    let host = Transactions::new();
    let workspace = fixture.scope.root.parent().unwrap().to_path_buf();
    let calls = fixture.calls();
    assert!(
        host.execute_reviewed(
            workspace.clone(),
            command.clone(),
            backend.clone(),
            Some(fixture.host.clone())
        )
        .await
        .is_err()
    );
    assert!(fixture.root.path().join("settings-old.json").exists());
    assert!(!fixture.root.path().join("settings-new.json").exists());
    let scope = backend
        .store
        .workspace_transaction_scope(
            UserId::new(command.user),
            command.project,
            &command.request.id,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(scope.worker.is_none() && scope.accepted_language_review_digest.is_none());
    backend
        .store
        .set_user_setting(UserId::new(command.user), "language_settings", "{}")
        .await
        .unwrap();
    // A prepared native command alone cannot substitute for approval of the
    // complete durable action, even after ordinary provider authority returns.
    assert!(
        host.execute_reviewed(
            workspace.clone(),
            command.clone(),
            backend.clone(),
            Some(fixture.host.clone())
        )
        .await
        .is_err()
    );
    assert!(fixture.root.path().join("settings-old.json").exists());
    approve(
        &backend.store,
        UserId::new(command.user),
        command.project,
        &state,
        command.request.language_grant.as_deref().unwrap(),
    )
    .await;
    let outcome = host
        .execute_reviewed(
            workspace.clone(),
            command.clone(),
            backend.clone(),
            Some(fixture.host.clone()),
        )
        .await
        .unwrap();
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
    assert!(!fixture.root.path().join("settings-old.json").exists());
    assert!(fixture.root.path().join("settings-new.json").exists());
    let accepted = backend
        .store
        .workspace_transaction_scope(
            UserId::new(command.user),
            command.project,
            &command.request.id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        accepted.accepted_language_review_digest,
        Some(
            state
                .language_review
                .as_ref()
                .unwrap()
                .fingerprint()
                .unwrap()
        )
    );
    assert_eq!(fixture.calls(), calls);
    fixture.host.shutdown();
    backend.now.store(100_000, Ordering::SeqCst);
    backend
        .store
        .set_user_setting(
            UserId::new(command.user),
            "language_settings",
            "{revoked configuration",
        )
        .await
        .unwrap();
    command.request.language_grant = None;
    command.request.action = WorkspaceTransactionAction::Recover;
    let recovery = Transactions::new()
        .execute(workspace, command, backend)
        .await
        .unwrap();
    // The existing interpreter preserves its terminal decision: Recover is
    // not an undo request. Reaching this result required no expired authority.
    assert!(
        recovery
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("operation committed")),
        "{:?}",
        recovery.error
    );
    assert_eq!(recovery.phase, Some(TransactionPhase::Committed));
    assert!(!fixture.root.path().join("executions").exists());
}

#[tokio::test]
async fn accepted_ambiguous_native_move_restores_after_original_service_and_grant_expire() {
    let (fixture, mut state, backend, mut command) = setup(true).await;
    let user = UserId::new(command.user);
    let folder = Folder::capture(&fixture.scope.root).unwrap();
    let lease = fixture
        .host
        .inner
        .coordinator
        .acquire(folder.clone())
        .await
        .unwrap();
    let guard = fixture
        .host
        .validate_workspace_review(fixture.scope.clone(), &state, &lease)
        .await
        .unwrap();
    let worker = WorkspaceTransactionWorker {
        id: "e".repeat(32),
        secret: "f".repeat(32),
    };
    let scope = backend
        .store
        .workspace_transaction_scope(user, command.project, &command.request.id)
        .await
        .unwrap()
        .unwrap();
    let digest = state
        .language_review
        .as_ref()
        .unwrap()
        .fingerprint()
        .unwrap();
    backend
        .store
        .claim_workspace_transaction_worker_reviewed(
            user,
            command.project,
            &scope,
            &worker,
            Some((&digest, command.request.language_grant.as_deref().unwrap())),
            101,
        )
        .await
        .unwrap();
    for active in [false, true] {
        state.journal.revision = backend
            .store
            .advance_workspace_transaction_for_worker(
                user,
                command.project,
                &AdvanceWorkspaceTransaction {
                    id: state.journal.id.clone(),
                    generation: state.journal.generation.clone(),
                    revision: state.journal.revision,
                    progress: TransactionProgress {
                        phase: TransactionPhase::Applying,
                        active,
                        ..Default::default()
                    },
                },
                Some(&worker),
                102,
            )
            .await
            .unwrap()
            .revision;
    }
    let scope = ResourceScope {
        transaction: state.journal.id.clone(),
        fingerprint: state.journal.plan.fingerprint().unwrap(),
    };
    let files = crate::runs::native_vfs::resources::ResourceFiles::new(
        folder.handle.as_file(),
        &state.journal.plan.root,
        scope.clone(),
    )
    .unwrap();
    let script = ResourceScript::prepare(&state.journal.plan).unwrap();
    let ResourceStep::Transfer { from, to, kind } = &script.steps[0] else {
        panic!("Expected the original native move")
    };
    files
        .transfer_resource(&scope, from, to, *kind)
        .await
        .unwrap();
    assert!(!fixture.root.path().join("settings-old.json").exists());
    // Leave an actual active native move without its acknowledgement, modeling
    // a stopped worker. The original object and accepted identity remain.
    drop(files);
    drop(guard);
    drop(lease);
    fixture.host.shutdown();
    backend.now.store(100_000, Ordering::SeqCst);
    backend
        .store
        .set_user_setting(user, "language_settings", "{revoked configuration")
        .await
        .unwrap();
    command.request.language_grant = None;
    command.request.action = WorkspaceTransactionAction::Recover;
    let outcome = Transactions::new()
        .execute(
            fixture.scope.root.parent().unwrap().to_path_buf(),
            command.clone(),
            backend.clone(),
        )
        .await
        .unwrap();
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.phase, Some(TransactionPhase::Restored));
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("settings-old.json")).unwrap(),
        "{\"original\":true}"
    );
    assert!(!fixture.root.path().join("settings-new.json").exists());
    assert!(!fixture.root.path().join("executions").exists());
    assert_eq!(
        backend
            .store
            .editor_recovery(user, command.project)
            .await
            .unwrap()
            .revision,
        1
    );
    assert!(
        backend
            .store
            .workspace_transaction_scope(user, command.project, &command.request.id)
            .await
            .unwrap()
            .unwrap()
            .accepted_language_review_digest
            .is_some()
    );
}
