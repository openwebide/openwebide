//! A published, still-empty creation is not permission to discard later metadata.
use super::metadata::Attribute;
use super::*;
use std::{
    fs::{File, FileTimes, Permissions},
    os::unix::fs::{MetadataExt, PermissionsExt},
    time::Duration,
};

async fn created(mode: WorkspaceMode, directory: bool) -> Accepted {
    created_at(mode, directory, true).await
}
async fn created_at(mode: WorkspaceMode, directory: bool, receipt: bool) -> Accepted {
    let host = Transactions::new();
    let owner = if mode == WorkspaceMode::Local {
        "paired/user:1"
    } else {
        "user:1"
    };
    let mut fixture = Fixture::with_mode(owner, if directory { "create-tree" } else { "create" });
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
    for active in [false, true] {
        advance(
            &backend,
            &command,
            &worker,
            &mut state,
            TransactionProgress {
                phase: TransactionPhase::Applying,
                active,
                ..Default::default()
            },
        )
        .await;
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
    assert_eq!(script.steps.len(), if directory { 2 } else { 1 });
    let ResourceStep::Create { path, kind } = &script.steps[0] else {
        panic!("Expected native creation")
    };
    let path = if receipt {
        files.create_resource(&scope, 0, path, *kind).await.unwrap();
        fixture.root.path().join(path)
    } else {
        let mut reached = 0;
        let result = files.create_resource_observed_for_test(&scope, 0, path, *kind, || {
            reached += 1;
            if reached == 2 {
                Err(std::io::Error::other(
                    "Interrupted before creation metadata receipt",
                ))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(reached, 2);
        fixture
            .root
            .path()
            .join(format!(".openwebide-create-{}-0", scope.transaction))
    };
    let original = same_file::Handle::from_path(&path).unwrap();
    assert_eq!(kind.is_dir(), directory);
    if directory {
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
    } else {
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }
    // There is an actual native object and durable intent, but no primitive
    // acknowledgement. Recovery cannot refresh the old language authority.
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

#[derive(Clone, Copy, Debug)]
enum Change {
    None,
    Attribute(Attribute),
    Permissions,
    Modified,
    Link,
}
async fn changed(mode: WorkspaceMode, directory: bool) {
    for change in [
        Change::None,
        Change::Attribute(Attribute::Extended),
        Change::Attribute(Attribute::Acl),
        Change::Attribute(Attribute::Flags),
        Change::Permissions,
        Change::Modified,
        Change::Link,
    ] {
        if directory && matches!(change, Change::Link) {
            continue;
        }
        let accepted = created(mode, directory).await;
        let editor = accepted
            .backend
            .store
            .editor_recovery(UserId::new(accepted.command.user), accepted.command.project)
            .await
            .unwrap();
        let queries = query_count(&accepted.fixture);
        let before = std::fs::metadata(&accepted.path).unwrap();
        let outside = tempfile::tempdir().unwrap();
        match change {
            Change::None => {}
            Change::Attribute(attribute) => attribute.set(&accepted.path),
            Change::Permissions => std::fs::set_permissions(
                &accepted.path,
                Permissions::from_mode(before.mode() ^ 0o100),
            )
            .unwrap(),
            Change::Modified => File::open(&accepted.path)
                .unwrap()
                .set_times(
                    FileTimes::new()
                        .set_modified(before.modified().unwrap() + Duration::from_secs(5)),
                )
                .unwrap(),
            Change::Link => {
                std::fs::hard_link(&accepted.path, outside.path().join("link")).unwrap();
            }
        }
        let expected_attribute = if let Change::Attribute(attribute) = change {
            Some(attribute.read(&accepted.path))
        } else {
            None
        };
        let expected_metadata = std::fs::metadata(&accepted.path).unwrap();
        if !matches!(change, Change::None) {
            assert_eq!(
                same_file::Handle::from_path(&accepted.path).unwrap(),
                accepted.original
            );
            if directory {
                assert_eq!(std::fs::read_dir(&accepted.path).unwrap().count(), 0);
            } else {
                assert_eq!(std::fs::read(&accepted.path).unwrap(), b"");
            }
        }
        let result = accepted.execute(WorkspaceTransactionAction::Recover).await;
        if !matches!(change, Change::None) {
            assert!(
                result
                    .as_ref()
                    .map_or(true, |outcome| outcome.error.is_some()),
                "Accepted rollback deleted a created object with later metadata: mode={mode:?}, directory={directory}, change={change:?}, result={result:?}"
            );
            assert_eq!(
                same_file::Handle::from_path(&accepted.path).unwrap(),
                accepted.original
            );
            if let Change::Attribute(attribute) = change {
                assert_eq!(attribute.read(&accepted.path), expected_attribute.unwrap());
            }
            let after = std::fs::metadata(&accepted.path).unwrap();
            assert_eq!(
                (after.mode(), after.nlink(), after.modified().unwrap()),
                (
                    expected_metadata.mode(),
                    expected_metadata.nlink(),
                    expected_metadata.modified().unwrap()
                ),
            );
        } else {
            let outcome = result.unwrap();
            assert!(outcome.error.is_none(), "{:?}", outcome.error);
            assert_eq!(outcome.phase, Some(TransactionPhase::Restored));
            assert!(!accepted.path.exists());
        }
        assert_eq!(
            accepted
                .backend
                .store
                .editor_recovery(UserId::new(accepted.command.user), accepted.command.project)
                .await
                .unwrap(),
            editor
        );
        assert_eq!(query_count(&accepted.fixture), queries);
        assert!(!accepted.fixture.root.path().join("executions").exists());
    }
}
#[tokio::test]
async fn remote_accepted_created_metadata_is_preserved_after_service_expiry() {
    changed(WorkspaceMode::Remote, false).await;
}
#[tokio::test]
async fn paired_accepted_created_metadata_is_preserved_after_service_expiry() {
    changed(WorkspaceMode::Local, false).await;
}
#[tokio::test]
async fn remote_accepted_created_directory_metadata_is_preserved_after_service_expiry() {
    changed(WorkspaceMode::Remote, true).await;
}
#[tokio::test]
async fn paired_accepted_created_directory_metadata_is_preserved_after_service_expiry() {
    changed(WorkspaceMode::Local, true).await;
}

async fn before_receipt(mode: WorkspaceMode) {
    for directory in [false, true] {
        // Exercise the externally changed object first to establish the causal
        // failure, then the unchanged but unwitnessed control.
        for external in [true, false] {
            let accepted = created_at(mode, directory, false).await;
            let user = UserId::new(accepted.command.user);
            let editor = accepted
                .backend
                .store
                .editor_recovery(user, accepted.command.project)
                .await
                .unwrap();
            let queries = query_count(&accepted.fixture);
            if external {
                Attribute::Extended.set(&accepted.path);
            }
            let result = accepted.execute(WorkspaceTransactionAction::Recover).await;
            assert!(
                result
                    .as_ref()
                    .map_or(true, |outcome| outcome.error.is_some()),
                "Unwitnessed creation was removed: mode={mode:?}, directory={directory}, external={external}, result={result:?}",
            );
            assert_eq!(
                same_file::Handle::from_path(&accepted.path).unwrap(),
                accepted.original
            );
            if external {
                assert_eq!(
                    Attribute::Extended.read(&accepted.path),
                    b"external update after the original move"
                );
            }
            assert_eq!(
                accepted
                    .backend
                    .store
                    .editor_recovery(user, accepted.command.project)
                    .await
                    .unwrap(),
                editor
            );
            assert_eq!(query_count(&accepted.fixture), queries);
            assert!(!accepted.fixture.root.path().join("executions").exists());

            // Model an operator explicitly inspecting and removing the artifact;
            // the application must never treat a fresh read as the lost baseline.
            if directory {
                std::fs::remove_dir(&accepted.path).unwrap();
            } else {
                std::fs::remove_file(&accepted.path).unwrap();
            }
            let outcome = accepted
                .execute(WorkspaceTransactionAction::Recover)
                .await
                .unwrap();
            assert!(outcome.error.is_none(), "{:?}", outcome.error);
            assert_eq!(outcome.phase, Some(TransactionPhase::Restored));
            assert_eq!(
                accepted
                    .backend
                    .store
                    .editor_recovery(user, accepted.command.project)
                    .await
                    .unwrap(),
                editor
            );
            assert_eq!(query_count(&accepted.fixture), queries);
            assert!(!accepted.fixture.root.path().join("executions").exists());
        }
    }
}
#[tokio::test]
async fn remote_accepted_unwitnessed_creation_is_preserved_after_service_expiry() {
    before_receipt(WorkspaceMode::Remote).await;
}
#[tokio::test]
async fn paired_accepted_unwitnessed_creation_is_preserved_after_service_expiry() {
    before_receipt(WorkspaceMode::Local).await;
}
