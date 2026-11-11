//! One accepted-recovery contract through both native folder/journal adapters.
use super::*;
use openwebide_core::workspace_transaction::{
    ResourceLocation,
    companion::{FolderAction, FolderReply, FolderRequest},
};
use std::path::PathBuf;

mod created_metadata;
mod metadata;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Checkpoint {
    Prepared,
    AmbiguousTransfer,
    Cleanup,
}

struct Accepted {
    host: Transactions,
    fixture: Fixture,
    state: WorkspaceTransactionState,
    backend: Arc<Backend>,
    command: NativeWorkspaceTransaction,
    original: same_file::Handle,
    path: PathBuf,
}
impl Accepted {
    async fn execute(
        &self,
        action: WorkspaceTransactionAction,
    ) -> Result<openwebide_core::workspace_transaction::WorkspaceTransactionOutcome, String> {
        let mut command = self.command.clone();
        command.request.action = action;
        let workspace = self.fixture.scope.root.parent().unwrap().to_path_buf();
        if self.state.editor.root.mode == WorkspaceMode::Remote {
            return self
                .host
                .execute(workspace, command, self.backend.clone())
                .await;
        }
        // A successor Local proof changes the directory namespace. Original
        // identities must survive those legitimate changes and service expiry.
        let (session, worker, _) = paired::prove(
            &self.host,
            &self.fixture,
            Some(self.state.journal.plan.root.clone()),
        )
        .await;
        paired::bind(&self.host, &self.fixture, &self.state, &session).await;
        let delegation = paired::delegate(&self.backend, &self.state, worker, 100_000).await;
        let server = paired::serve(self.backend.clone()).await;
        let reply = self
            .host
            .folder_request(
                workspace,
                self.fixture.scope.owner.clone(),
                FolderRequest {
                    user: command.user,
                    project: command.project,
                    action: FolderAction::Execute {
                        session: session.clone(),
                        api_url: server.url.clone(),
                        delegation,
                        request: command.request,
                    },
                },
                None,
            )
            .await;
        paired::call(&self.host, &self.fixture, FolderAction::Close { session }).await;
        reply.map(|reply| match reply {
            FolderReply::Executed { outcome } => outcome,
            _ => panic!("Missing native recovery outcome"),
        })
    }
    async fn assert_editor_unchanged(&self) {
        assert_eq!(
            self.backend
                .store
                .editor_recovery(UserId::new(self.command.user), self.command.project)
                .await
                .unwrap()
                .revision,
            1
        );
        assert!(!self.fixture.root.path().join("executions").exists());
    }
}
async fn accepted(mode: WorkspaceMode, checkpoint: Checkpoint) -> Accepted {
    let prefix = checkpoint != Checkpoint::Prepared;
    let host = Transactions::new();
    let owner = if mode == WorkspaceMode::Local {
        "paired/user:1"
    } else {
        "user:1"
    };
    let mut fixture = Fixture::with_mode(owner, if prefix { "inputs-overwrite" } else { "inputs" });
    let from = fixture.root.path().join("settings-old.json");
    let mut path = if prefix {
        std::fs::create_dir_all(from.join("nested")).unwrap();
        std::fs::write(from.join("nested/data.json"), "source contents").unwrap();
        let target = fixture.root.path().join("settings-new.json");
        std::fs::create_dir_all(target.join("nested")).unwrap();
        std::fs::write(target.join("sibling.json"), "unchanged sibling").unwrap();
        target.join("nested/data.json")
    } else {
        from.clone()
    };
    std::fs::write(&path, "{\"original\":true}").unwrap();
    let original = same_file::Handle::from_path(&path).unwrap();
    let initial = if mode == WorkspaceMode::Local {
        Some(paired::prove(&host, &fixture, None).await.0)
    } else {
        None
    };
    let state = fixture.state().await;
    fixture.prepare(&state).await;
    let (fixture, mut state, backend, mut command) = store_review(fixture, state, true).await;
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
    let worker = WorkspaceTransactionWorker {
        id: "e".repeat(32),
        secret: "f".repeat(32),
    };
    // Real original-service verification precedes the real authority CAS.
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
    guard.ensure_current().unwrap();
    if prefix {
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
            state.journal.progress = TransactionProgress {
                phase: TransactionPhase::Applying,
                active,
                ..Default::default()
            };
        }
        let scope = ResourceScope {
            transaction: state.journal.id.clone(),
            fingerprint: state.journal.plan.fingerprint().unwrap(),
        };
        let files = crate::runs::native_vfs::resources::ResourceFiles::reviewed(
            folder.handle.as_file(),
            &state.journal.plan.root,
            scope.clone(),
            state.language_review.as_deref(),
            &state.journal.plan,
        )
        .unwrap();
        let script = ResourceScript::prepare(&state.journal.plan).unwrap();
        let ResourceStep::Transfer {
            from,
            to: to @ ResourceLocation::Retained(slot),
            kind,
        } = &script.steps[0]
        else {
            panic!("Expected the actual original directory move");
        };
        files
            .transfer_resource(&scope, from, to, *kind)
            .await
            .unwrap();
        path = fixture.root.path().join(format!(
            ".openwebide/recovery-{}/objects/{slot}/nested/data.json",
            state.journal.id
        ));
        assert_eq!(same_file::Handle::from_path(&path).unwrap(), original);
        assert!(!fixture.root.path().join("settings-new.json").exists());
        // The actual directory transfer has no durable acknowledgement.
        if checkpoint == Checkpoint::Cleanup {
            for completed in 1..=script.steps.len() {
                let progress = TransactionProgress {
                    phase: TransactionPhase::Applying,
                    completed,
                    ..Default::default()
                };
                advance(&backend, &command, &worker, &mut state, progress).await;
                if let Some(step) = script.steps.get(completed) {
                    advance(
                        &backend,
                        &command,
                        &worker,
                        &mut state,
                        TransactionProgress {
                            phase: TransactionPhase::Applying,
                            completed,
                            active: true,
                            ..Default::default()
                        },
                    )
                    .await;
                    let ResourceStep::Transfer { from, to, kind } = step else {
                        panic!("Expected the remaining original directory transfer");
                    };
                    files
                        .transfer_resource(&scope, from, to, *kind)
                        .await
                        .unwrap();
                }
            }
            advance(
                &backend,
                &command,
                &worker,
                &mut state,
                TransactionProgress {
                    phase: TransactionPhase::Finalizing,
                    completed: script.steps.len(),
                    ..Default::default()
                },
            )
            .await;
        }
    }
    drop(guard);
    drop(lease);
    if let Some(session) = initial {
        paired::call(&host, &fixture, FolderAction::Close { session }).await;
    }
    fixture.host.shutdown();
    backend.now.store(100_001, Ordering::SeqCst);
    backend
        .store
        .set_user_setting(user, "language_settings", "{expired provider")
        .await
        .unwrap();
    command.request.language_grant = None;
    Accepted {
        host,
        fixture,
        state,
        backend,
        command,
        original,
        path,
    }
}
async fn advance(
    backend: &Backend,
    command: &NativeWorkspaceTransaction,
    worker: &WorkspaceTransactionWorker,
    state: &mut WorkspaceTransactionState,
    progress: TransactionProgress,
) {
    state.journal.revision = backend
        .store
        .advance_workspace_transaction_for_worker(
            UserId::new(command.user),
            command.project,
            &AdvanceWorkspaceTransaction {
                id: state.journal.id.clone(),
                generation: state.journal.generation.clone(),
                revision: state.journal.revision,
                progress: progress.clone(),
            },
            Some(worker),
            102,
        )
        .await
        .unwrap()
        .revision;
    state.journal.progress = progress;
}
fn query_count(fixture: &Fixture) -> usize {
    fixture
        .calls()
        .lines()
        .filter(|method| {
            matches!(
                *method,
                "textDocument/codeAction" | "workspace/executeCommand"
            )
        })
        .count()
}
async fn resume(mode: WorkspaceMode, replacement: bool, prefix: bool) {
    let accepted = accepted(
        mode,
        if prefix {
            Checkpoint::AmbiguousTransfer
        } else {
            Checkpoint::Prepared
        },
    )
    .await;
    let saved = tempfile::tempdir().unwrap();
    if replacement {
        // Keep the original alive: the new object has approved bytes but a
        // different identity, including when its parent is the original tree.
        let bytes = std::fs::read(&accepted.path).unwrap();
        std::fs::rename(&accepted.path, saved.path().join("original")).unwrap();
        std::fs::write(&accepted.path, bytes).unwrap();
    }
    let current = same_file::Handle::from_path(&accepted.path).unwrap();
    assert_eq!(accepted.original == current, !replacement);
    let queries = query_count(&accepted.fixture);
    let result = accepted
        .execute(if prefix {
            WorkspaceTransactionAction::Recover
        } else {
            WorkspaceTransactionAction::Apply
        })
        .await;
    assert_eq!(query_count(&accepted.fixture), queries);
    if replacement {
        assert!(
            result
                .as_ref()
                .map_or(true, |outcome| outcome.error.is_some()),
            "Accepted recovery moved an unreviewed same-byte replacement: {result:?}"
        );
        assert_eq!(
            same_file::Handle::from_path(&accepted.path).unwrap(),
            current
        );
        assert_eq!(
            same_file::Handle::from_path(saved.path().join("original")).unwrap(),
            accepted.original
        );
        assert!(
            !accepted
                .fixture
                .root
                .path()
                .join("settings-new.json")
                .exists()
        );
        accepted.assert_editor_unchanged().await;
    } else {
        let outcome = result.unwrap();
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(
            outcome.phase,
            Some(if prefix {
                TransactionPhase::Restored
            } else {
                TransactionPhase::Committed
            })
        );
        let path = if prefix {
            accepted
                .fixture
                .root
                .path()
                .join("settings-new.json/nested/data.json")
        } else {
            accepted.fixture.root.path().join("settings-new.json")
        };
        assert_eq!(
            same_file::Handle::from_path(path).unwrap(),
            accepted.original
        );
        if prefix {
            accepted.assert_editor_unchanged().await;
        }
    }
}

#[tokio::test]
async fn remote_accepted_prepared_recovery_preserves_original_identity_after_service_expiry() {
    resume(WorkspaceMode::Remote, false, false).await;
    resume(WorkspaceMode::Remote, true, false).await;
}
#[tokio::test]
async fn paired_accepted_prepared_recovery_preserves_original_identity_after_service_expiry() {
    resume(WorkspaceMode::Local, false, false).await;
    resume(WorkspaceMode::Local, true, false).await;
}
#[tokio::test]
async fn remote_accepted_ambiguous_directory_recovery_preserves_original_children() {
    resume(WorkspaceMode::Remote, false, true).await;
    resume(WorkspaceMode::Remote, true, true).await;
}
#[tokio::test]
async fn paired_accepted_ambiguous_directory_recovery_preserves_original_children() {
    resume(WorkspaceMode::Local, false, true).await;
    resume(WorkspaceMode::Local, true, true).await;
}

async fn cleanup(mode: WorkspaceMode, replacement: bool) {
    let accepted = accepted(mode, Checkpoint::Cleanup).await;
    let editor = accepted
        .backend
        .store
        .editor_recovery(UserId::new(accepted.command.user), accepted.command.project)
        .await
        .unwrap();
    // The irreversible decision already publishes the editor successor.
    // Finishing or refusing cleanup must not publish that successor twice.
    assert_eq!(editor.revision, 2);
    let saved = tempfile::tempdir().unwrap();
    if replacement {
        let bytes = std::fs::read(&accepted.path).unwrap();
        std::fs::rename(&accepted.path, saved.path().join("original")).unwrap();
        std::fs::write(&accepted.path, bytes).unwrap();
    }
    let current = same_file::Handle::from_path(&accepted.path).unwrap();
    assert_eq!(current == accepted.original, !replacement);
    let queries = query_count(&accepted.fixture);
    let result = accepted.execute(WorkspaceTransactionAction::Recover).await;
    assert_eq!(query_count(&accepted.fixture), queries);
    if replacement {
        assert!(
            result
                .as_ref()
                .map_or(true, |outcome| outcome.error.is_some()),
            "Cleanup deleted an unreviewed same-byte replacement: {result:?}"
        );
        assert_eq!(
            same_file::Handle::from_path(&accepted.path).unwrap(),
            current
        );
        assert_eq!(
            same_file::Handle::from_path(saved.path().join("original")).unwrap(),
            accepted.original
        );
    } else {
        let outcome = result.unwrap();
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
        assert!(!accepted.path.exists());
    }
    assert_eq!(
        accepted
            .backend
            .store
            .editor_recovery(UserId::new(accepted.command.user), accepted.command.project,)
            .await
            .unwrap(),
        editor
    );
    assert_eq!(
        std::fs::read_to_string(
            accepted
                .fixture
                .root
                .path()
                .join("settings-new.json/nested/data.json")
        )
        .unwrap(),
        "source contents"
    );
    assert!(!accepted.fixture.root.path().join("executions").exists());
}

#[tokio::test]
async fn remote_accepted_cleanup_preserves_replaced_original_children() {
    cleanup(WorkspaceMode::Remote, false).await;
    cleanup(WorkspaceMode::Remote, true).await;
}
#[tokio::test]
async fn paired_accepted_cleanup_preserves_replaced_original_children() {
    cleanup(WorkspaceMode::Local, false).await;
    cleanup(WorkspaceMode::Local, true).await;
}

async fn replacement_during_intent(mode: WorkspaceMode) {
    let accepted = accepted(mode, Checkpoint::Prepared).await;
    let saved = tempfile::tempdir().unwrap();
    let backup = saved.path().join("original");
    *accepted.backend.replace_on_intent.lock().unwrap() =
        Some((accepted.path.clone(), backup.clone()));
    let queries = query_count(&accepted.fixture);
    let result = accepted.execute(WorkspaceTransactionAction::Apply).await;
    assert!(accepted.backend.replace_on_intent.lock().unwrap().is_none());
    assert!(
        result
            .as_ref()
            .map_or(true, |outcome| outcome.error.is_some()),
        "A same-byte replacement made during the durable intent was moved: {result:?}"
    );
    assert_eq!(
        same_file::Handle::from_path(backup).unwrap(),
        accepted.original
    );
    assert_ne!(
        same_file::Handle::from_path(&accepted.path).unwrap(),
        accepted.original
    );
    assert_eq!(
        std::fs::read_to_string(&accepted.path).unwrap(),
        "{\"original\":true}"
    );
    assert!(
        !accepted
            .fixture
            .root
            .path()
            .join("settings-new.json")
            .exists()
    );
    assert_eq!(query_count(&accepted.fixture), queries);
    accepted.assert_editor_unchanged().await;
}

#[tokio::test]
async fn remote_accepted_mutation_rechecks_original_identity_after_durable_intent() {
    replacement_during_intent(WorkspaceMode::Remote).await;
}
#[tokio::test]
async fn paired_accepted_mutation_rechecks_original_identity_after_durable_intent() {
    replacement_during_intent(WorkspaceMode::Local).await;
}
