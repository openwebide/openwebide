use super::*;
use openwebide_core::{
    NewProject, UserId, UserRole,
    editor::{Document, EditorRecovery, EditorRecoveryFile, EditorRecoveryRoot, RecoveryScroll},
    workspace_transaction::{
        AdvanceWorkspaceTransaction, BeginWorkspaceTransaction, EditorRecoveryPatch,
        ExecuteWorkspaceTransaction, FileOperation, WorkspaceTransactionAction,
    },
};
use openwebide_storage::{Store, rusqlite_db::RusqliteDb};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Notify;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod paired;
mod scope;

pub(super) struct Backend {
    pub(super) store: Store<RusqliteDb>,
    pause: AtomicBool,
    pause_step: AtomicUsize,
    entered: Notify,
    release: Notify,
    lose_commit_ack: AtomicBool,
    scope_reads: AtomicUsize,
    snapshot_reads: AtomicUsize,
    last_worker: Mutex<Option<WorkspaceTransactionWorker>>,
}
impl JournalBackend for Backend {
    async fn request(
        &self,
        request: &WorkspaceJournalRequest,
    ) -> Result<WorkspaceJournalResponse, String> {
        let user = UserId::new(request.user);
        match &request.operation {
            WorkspaceJournalOperation::ClaimWorker {
                scope,
                worker,
                review_digest,
                language_grant,
            } => {
                let review = review_digest.as_deref().zip(language_grant.as_deref());
                let claimed = futures::executor::block_on(
                    self.store.claim_workspace_transaction_worker_reviewed(
                        user,
                        request.project,
                        scope,
                        worker,
                        review,
                        100,
                    ),
                )
                .map_err(|error| error.to_string())?;
                *self.last_worker.lock().unwrap() = Some(worker.clone());
                Ok(WorkspaceJournalResponse::WorkerClaimed { scope: claimed })
            }
            WorkspaceJournalOperation::ReleaseWorker { generation, worker } => {
                futures::executor::block_on(self.store.release_workspace_transaction_worker(
                    user,
                    request.project,
                    &request.id,
                    generation,
                    worker,
                    100,
                ))
                .map(|()| WorkspaceJournalResponse::WorkerReleased {
                    generation: generation.clone(),
                    worker: worker.id.clone(),
                })
                .map_err(|error| error.to_string())
            }
            WorkspaceJournalOperation::Scope => {
                self.scope_reads.fetch_add(1, Ordering::SeqCst);
                self.store
                    .workspace_transaction_scope(user, request.project, &request.id)
                    .await
                    .map(|scope| WorkspaceJournalResponse::Scope { scope })
                    .map_err(|error| error.to_string())
            }
            WorkspaceJournalOperation::Load => {
                self.snapshot_reads.fetch_add(1, Ordering::SeqCst);
                self.store
                    .workspace_transaction(user, request.project, Some(&request.id))
                    .await
                    .map(|state| WorkspaceJournalResponse::Loaded {
                        state: state.map(Box::new),
                    })
                    .map_err(|error| error.to_string())
            }
            WorkspaceJournalOperation::Advance { .. } => {
                Err("Native workspace execution requires an admitted worker".into())
            }
            WorkspaceJournalOperation::AdvanceOwned {
                worker,
                generation,
                revision,
                progress,
            } => {
                if progress.active
                    && progress.completed == self.pause_step.load(Ordering::SeqCst)
                    && self.pause.swap(false, Ordering::SeqCst)
                {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                // Rusqlite's primitives complete synchronously. Poll this test
                // transport locally rather than imposing Send on its GAT future.
                let response = futures::executor::block_on(
                    self.store.advance_workspace_transaction_for_worker(
                        user,
                        request.project,
                        &AdvanceWorkspaceTransaction {
                            id: request.id.clone(),
                            generation: generation.clone(),
                            revision: *revision,
                            progress: progress.clone(),
                        },
                        Some(worker),
                        100,
                    ),
                )
                .map_err(|e| e.to_string())?;
                if progress.phase == TransactionPhase::Committed
                    && self.lose_commit_ack.swap(false, Ordering::SeqCst)
                {
                    return Err("Commit response lost".into());
                }
                Ok(WorkspaceJournalResponse::Advanced { response })
            }
        }
    }
}
pub(super) struct Fixture {
    pub(super) directory: tempfile::TempDir,
    pub(super) backend: Arc<Backend>,
    pub(super) user: UserId,
    pub(super) project: i64,
    pub(super) command: NativeWorkspaceTransaction,
}
async fn fixture() -> Fixture {
    fixture_version(1).await
}
async fn fixture_version(version: u32) -> Fixture {
    fixture_mode(version, WorkspaceMode::Remote).await
}
pub(super) async fn fixture_mode(version: u32, mode: WorkspaceMode) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("project")).unwrap();
    std::fs::write(directory.path().join("project/before.rs"), "disk 文😀\r\n").unwrap();
    if version == 2 {
        std::fs::write(
            directory.path().join("project/after.rs"),
            "overwritten original",
        )
        .unwrap();
    }
    let store = Store::new(RusqliteDb::open_in_memory().unwrap());
    store.migrate().await.unwrap();
    let user = store
        .insert_user("owner", "hash", UserRole::Admin, 1)
        .await
        .unwrap()
        .id;
    let project = store
        .create_project(
            &NewProject {
                name: "native".into(),
                mode,
                path: Some("project".into()),
            },
            user,
            1,
        )
        .await
        .unwrap();
    let root = EditorRecoveryRoot::for_project(&project);
    let mut document = Document::new("disk 文😀\r\n");
    document.replace_selections("unsaved ", None).unwrap();
    let before = EditorRecoveryFile {
        path: "before.rs".into(),
        document: Some(document.recovery()),
        scroll: RecoveryScroll::default(),
        read_only: false,
    };
    let mut after = before.clone();
    after.path = "after.rs".into();
    let recovery = EditorRecovery {
        format: 1,
        root: Some(root.clone()),
        selected: Some(before.path.clone()),
        files: vec![before.clone()],
    };
    store
        .save_editor_recovery(user, project.id, 0, &recovery)
        .await
        .unwrap();
    let vfs = NativeFsVfs {
        root: directory.path().join("project").canonicalize().unwrap(),
    };
    let identity =
        Host::root_identity(directory.path(), "project", user.get(), project.id).unwrap();
    let files = VfsTransactionFiles {
        vfs: &vfs,
        identity: &identity,
    };
    let operations = vec![FileOperation::Rename {
        from: "before.rs".into(),
        to: "after.rs".into(),
        overwrite: version == 2,
        ignore_if_exists: false,
    }];
    let plan = if version == 2 {
        transaction::prepare_native(&files, identity.clone(), operations, || true).await
    } else {
        transaction::prepare(&files, identity.clone(), operations, || true).await
    }
    .unwrap();
    let id = "0123456789abcdef0123456789abcdef".to_string();
    let prepared = store
        .begin_workspace_transaction(
            user,
            project.id,
            &BeginWorkspaceTransaction {
                language_review: None,
                id: id.clone(),
                plan,
                editor: EditorRecoveryPatch {
                    revision: 1,
                    root,
                    before: vec![before],
                    after: vec![after],
                    order_before: vec!["before.rs".into()],
                    order_after: vec!["after.rs".into()],
                    selected_before: Some("before.rs".into()),
                    selected_after: Some("after.rs".into()),
                },
            },
            100,
        )
        .await
        .unwrap();
    Fixture {
        directory,
        backend: Arc::new(Backend {
            store,
            pause: AtomicBool::new(false),
            pause_step: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Notify::new(),
            lose_commit_ack: AtomicBool::new(false),
            scope_reads: AtomicUsize::new(0),
            snapshot_reads: AtomicUsize::new(0),
            last_worker: Mutex::new(None),
        }),
        user,
        project: project.id,
        command: NativeWorkspaceTransaction {
            user: user.get(),
            project: project.id,
            request: ExecuteWorkspaceTransaction {
                language_grant: None,
                id,
                generation: prepared.journal.generation,
                action: WorkspaceTransactionAction::Apply,
            },
        },
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn resource_worker_uses_native_objects_and_the_same_durable_editor_journal() {
    let fixture = fixture_version(2).await;
    let root = fixture.directory.path().join("project");
    let original = same_file::Handle::from_path(root.join("before.rs")).unwrap();
    let overwritten = std::fs::File::open(root.join("after.rs")).unwrap();
    fixture
        .backend
        .lose_commit_ack
        .store(true, Ordering::SeqCst);
    let outcome = Host::default()
        .execute(
            fixture.directory.path().into(),
            fixture.command.clone(),
            fixture.backend.clone(),
        )
        .await
        .unwrap();
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
    assert_eq!(
        same_file::Handle::from_path(root.join("after.rs")).unwrap(),
        original
    );
    use std::io::Read;
    let mut bytes = String::new();
    overwritten.take(8192).read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "overwritten original");
    let editor = fixture
        .backend
        .store
        .editor_recovery(fixture.user, fixture.project)
        .await
        .unwrap();
    assert_eq!(editor.revision, 2);
    assert_eq!(editor.state.files[0].path, "after.rs");
    assert!(
        std::fs::read_dir(root.join(".openwebide"))
            .unwrap()
            .next()
            .is_none()
    );
}
#[tokio::test]
async fn native_worker_commits_files_and_unsaved_recovery_once() {
    let fixture = fixture().await;
    fixture
        .backend
        .lose_commit_ack
        .store(true, Ordering::SeqCst);
    let host = Host::default();
    let outcome = host
        .execute(
            fixture.directory.path().into(),
            fixture.command.clone(),
            fixture.backend.clone(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
    assert!(outcome.error.is_none());
    assert!(!fixture.directory.path().join("project/before.rs").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.directory.path().join("project/after.rs")).unwrap(),
        "disk 文😀\r\n"
    );
    let recovery = fixture
        .backend
        .store
        .editor_recovery(fixture.user, fixture.project)
        .await
        .unwrap();
    assert_eq!(recovery.revision, 2);
    assert_eq!(recovery.state.files[0].path, "after.rs");
    assert!(
        recovery.state.files[0]
            .document
            .as_ref()
            .unwrap()
            .text
            .starts_with("unsaved ")
    );
    std::fs::write(
        fixture.directory.path().join("project/after.rs"),
        "later external edit",
    )
    .unwrap();
    let outcome = host
        .execute(
            fixture.directory.path().into(),
            fixture.command.clone(),
            fixture.backend.clone(),
        )
        .await
        .unwrap();
    assert!(outcome.error.is_none());
    assert_eq!(
        std::fs::read_to_string(fixture.directory.path().join("project/after.rs")).unwrap(),
        "later external edit"
    );
    assert_eq!(
        fixture
            .backend
            .store
            .editor_recovery(fixture.user, fixture.project)
            .await
            .unwrap()
            .revision,
        2
    );
}

#[tokio::test]
async fn disconnected_request_keeps_worker_and_queues_recovery() {
    let fixture = fixture().await;
    fixture.backend.pause.store(true, Ordering::SeqCst);
    let host = Host::default();
    let root = fixture.directory.path().to_path_buf();
    let running = tokio::spawn({
        let (host, backend, command, root) = (
            host.clone(),
            fixture.backend.clone(),
            fixture.command.clone(),
            root.clone(),
        );
        async move { host.execute(root, command, backend).await }
    });
    fixture.backend.entered.notified().await;
    let owned_scope = fixture
        .backend
        .store
        .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
        .await
        .unwrap()
        .unwrap();
    assert!(owned_scope.worker.as_ref().unwrap().active);
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    let recovery = tokio::spawn({
        let (host, backend, mut command) = (
            host.clone(),
            fixture.backend.clone(),
            fixture.command.clone(),
        );
        command.request.action = WorkspaceTransactionAction::Recover;
        async move { host.execute(root, command, backend).await }
    });
    tokio::task::yield_now().await;
    assert!(!recovery.is_finished());
    fixture.backend.release.notify_one();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), recovery)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
    let completed_scope = fixture
        .backend
        .store
        .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!completed_scope.worker.as_ref().unwrap().active);
    // Recovery waits for the owned worker; it never undoes an already committed operation.
    assert!(outcome.error.is_some());
    assert!(!fixture.directory.path().join("project/before.rs").exists());
    assert!(fixture.directory.path().join("project/after.rs").exists());
    assert_eq!(
        fixture
            .backend
            .store
            .editor_recovery(fixture.user, fixture.project)
            .await
            .unwrap()
            .revision,
        2
    );
}

#[tokio::test]
async fn restarted_host_restores_an_active_intent_and_rejects_delayed_apply() {
    let fixture = fixture().await;
    let previous = crate::ServerConfig::new(
        fixture.directory.path().into(),
        "previous-secret".into(),
        None,
    );
    let restarted = crate::ServerConfig::new(
        fixture.directory.path().into(),
        "rotated-secret".into(),
        None,
    );
    assert_ne!(previous.secret, restarted.secret);
    let original = Host::root_identity(
        fixture.directory.path(),
        "project",
        fixture.user.get(),
        fixture.project,
    )
    .unwrap();
    assert_ne!(
        original,
        Host::root_identity(
            fixture.directory.path(),
            "project",
            fixture.user.get() + 1,
            fixture.project
        )
        .unwrap()
    );
    assert_ne!(
        original,
        Host::root_identity(
            fixture.directory.path(),
            "project",
            fixture.user.get(),
            fixture.project + 1
        )
        .unwrap()
    );
    let mut state = fixture
        .backend
        .store
        .workspace_transaction(
            fixture.user,
            fixture.project,
            Some(&fixture.command.request.id),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original, state.journal.plan.root);
    for active in [false, true] {
        state.journal.revision = fixture
            .backend
            .store
            .advance_workspace_transaction(
                fixture.user,
                fixture.project,
                &AdvanceWorkspaceTransaction {
                    id: state.journal.id.clone(),
                    generation: state.journal.generation.clone(),
                    revision: state.journal.revision,
                    progress: TransactionProgress {
                        phase: TransactionPhase::Applying,
                        active,
                        ..TransactionProgress::default()
                    },
                },
                100,
            )
            .await
            .unwrap()
            .revision;
        state.journal.progress = TransactionProgress {
            phase: TransactionPhase::Applying,
            active,
            ..TransactionProgress::default()
        };
    }
    let vfs = NativeFsVfs {
        root: fixture
            .directory
            .path()
            .join("project")
            .canonicalize()
            .unwrap(),
    };
    let files = VfsTransactionFiles {
        vfs: &vfs,
        identity: &state.journal.plan.root,
    };
    state
        .journal
        .active_mutation()
        .unwrap()
        .execute(&files)
        .await
        .unwrap();
    let host = restarted.workspace_transactions;
    let mut recovery = fixture.command.clone();
    recovery.request.action = WorkspaceTransactionAction::Recover;
    let outcome = host
        .execute(
            fixture.directory.path().into(),
            recovery,
            fixture.backend.clone(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.phase, Some(TransactionPhase::Restored));
    assert!(outcome.error.is_none());
    let outcome = host
        .execute(
            fixture.directory.path().into(),
            fixture.command,
            fixture.backend.clone(),
        )
        .await
        .unwrap();
    assert!(outcome.error.is_some());
    assert!(fixture.directory.path().join("project/before.rs").exists());
    assert!(!fixture.directory.path().join("project/after.rs").exists());
    assert_eq!(
        fixture
            .backend
            .store
            .editor_recovery(fixture.user, fixture.project)
            .await
            .unwrap()
            .revision,
        1
    );
}

#[tokio::test]
async fn native_worker_uses_authenticated_http_journal_transport() {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full, Limited};
    use hyper::{Request, Response, body::Incoming, service::service_fn};
    use hyper_util::rt::TokioIo;
    let fixture = fixture().await;
    let previous = crate::ServerConfig::new(
        fixture.directory.path().into(),
        "previous-secret".into(),
        None,
    );
    let rotated = crate::ServerConfig::new(
        fixture.directory.path().into(),
        "native-test-secret".into(),
        None,
    );
    assert_ne!(previous.secret, rotated.secret);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let backend = fixture.backend.clone();
    let service = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            let backend = backend.clone();
            connections.spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |request: Request<Incoming>| {
                            let backend = backend.clone();
                            async move {
                                assert_eq!(
                                    request.uri().path(),
                                    "/api/workspace-transactions/service"
                                );
                                assert_eq!(
                                    request.headers()["authorization"],
                                    "Bearer native-test-secret"
                                );
                                let body = Limited::new(request.into_body(), 64 * 1024)
                                    .collect()
                                    .await
                                    .unwrap()
                                    .to_bytes();
                                let request = serde_json::from_slice(&body).unwrap();
                                let result = backend.request(&request).await;
                                let (status, body) = match result {
                                    Ok(response) => (200, serde_json::to_vec(&response).unwrap()),
                                    Err(error) => (
                                        409,
                                        serde_json::to_vec(&serde_json::json!({"error":error}))
                                            .unwrap(),
                                    ),
                                };
                                Ok::<_, std::convert::Infallible>(
                                    Response::builder()
                                        .status(status)
                                        .header("content-type", "application/json")
                                        .body(Full::new(Bytes::from(body)))
                                        .unwrap(),
                                )
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    let client = Arc::new(BackendClient::new(
        format!("http://{address}/api"),
        "native-test-secret".into(),
        crate::runs::http_client::ReqwestHttpClient::default(),
    ));
    let outcome = rotated
        .workspace_transactions
        .execute(fixture.directory.path().into(), fixture.command, client)
        .await
        .unwrap();
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
    assert_eq!(
        fixture
            .backend
            .store
            .editor_recovery(fixture.user, fixture.project)
            .await
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        std::fs::read_to_string(fixture.directory.path().join("project/after.rs")).unwrap(),
        "disk 文😀\r\n"
    );
    service.abort();
    let _ = service.await;
}

#[tokio::test]
async fn identical_files_in_a_replaced_root_do_not_reuse_folder_authority() {
    let fixture = fixture().await;
    let original = fixture.directory.path().join("original-project");
    std::fs::rename(fixture.directory.path().join("project"), &original).unwrap();
    std::fs::create_dir(fixture.directory.path().join("project")).unwrap();
    std::fs::copy(
        original.join("before.rs"),
        fixture.directory.path().join("project/before.rs"),
    )
    .unwrap();
    assert!(
        Host::default()
            .execute(
                fixture.directory.path().into(),
                fixture.command.clone(),
                fixture.backend.clone()
            )
            .await
            .is_err()
    );
    assert!(!fixture.directory.path().join("project/after.rs").exists());
    assert!(!original.join("after.rs").exists());
    assert_eq!(
        fixture
            .backend
            .store
            .workspace_transaction(
                fixture.user,
                fixture.project,
                Some(&fixture.command.request.id)
            )
            .await
            .unwrap()
            .unwrap()
            .journal
            .progress
            .phase,
        TransactionPhase::Prepared
    );
    assert_eq!(
        fixture
            .backend
            .store
            .editor_recovery(fixture.user, fixture.project)
            .await
            .unwrap()
            .revision,
        1
    );
}

#[tokio::test]
async fn replacing_root_mid_operation_stops_io_and_original_folder_can_recover() {
    let fixture = fixture().await;
    fixture.backend.pause.store(true, Ordering::SeqCst);
    let host = Host::default();
    let running = tokio::spawn({
        let (host, backend, command, root) = (
            host.clone(),
            fixture.backend.clone(),
            fixture.command.clone(),
            fixture.directory.path().to_path_buf(),
        );
        async move { host.execute(root, command, backend).await }
    });
    fixture.backend.entered.notified().await;
    let original = fixture.directory.path().join("original-project");
    std::fs::rename(fixture.directory.path().join("project"), &original).unwrap();
    std::fs::create_dir(fixture.directory.path().join("project")).unwrap();
    std::fs::copy(
        original.join("before.rs"),
        fixture.directory.path().join("project/before.rs"),
    )
    .unwrap();
    fixture.backend.release.notify_one();
    let outcome = running.await.unwrap().unwrap();
    assert_eq!(
        outcome.error.unwrap().recovery,
        transaction::RecoveryState::Pending
    );
    assert!(!fixture.directory.path().join("project/after.rs").exists());
    assert!(!original.join("after.rs").exists());
    std::fs::remove_dir_all(fixture.directory.path().join("project")).unwrap();
    std::fs::rename(original, fixture.directory.path().join("project")).unwrap();
    let mut recovery = fixture.command;
    recovery.request.action = WorkspaceTransactionAction::Recover;
    let outcome = host
        .execute(
            fixture.directory.path().into(),
            recovery,
            fixture.backend.clone(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.phase, Some(TransactionPhase::Restored));
    assert!(outcome.error.is_none());
    assert_eq!(
        fixture
            .backend
            .store
            .editor_recovery(fixture.user, fixture.project)
            .await
            .unwrap()
            .revision,
        1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn native_permission_loss_retains_journal_until_access_is_regranted() {
    use std::os::unix::fs::PermissionsExt;
    if std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| output.stdout == b"0\n")
    {
        eprintln!("Permission denial requires an unprivileged test process");
        return;
    }
    struct RestorePermissions(PathBuf);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let fixture = fixture().await;
    let project = fixture.directory.path().join("project");
    let _permissions = RestorePermissions(project.clone());
    fixture.backend.pause_step.store(1, Ordering::SeqCst);
    fixture.backend.pause.store(true, Ordering::SeqCst);
    let host = Host::default();
    let running = tokio::spawn({
        let (host, backend, command, root) = (
            host.clone(),
            fixture.backend.clone(),
            fixture.command.clone(),
            fixture.directory.path().to_path_buf(),
        );
        async move { host.execute(root, command, backend).await }
    });
    fixture.backend.entered.notified().await;
    assert!(project.join("after.rs").exists());
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o555)).unwrap();
    fixture.backend.release.notify_one();
    let outcome = running.await.unwrap().unwrap();
    assert_eq!(
        outcome.error.unwrap().recovery,
        transaction::RecoveryState::Pending
    );
    assert_eq!(outcome.phase, Some(TransactionPhase::Reverting));
    assert_eq!(
        std::fs::read_to_string(project.join("before.rs")).unwrap(),
        "disk 文😀\r\n"
    );
    assert_eq!(
        fixture
            .backend
            .store
            .editor_recovery(fixture.user, fixture.project)
            .await
            .unwrap()
            .revision,
        1
    );
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut recovery = fixture.command;
    recovery.request.action = WorkspaceTransactionAction::Recover;
    let outcome = host
        .execute(
            fixture.directory.path().into(),
            recovery,
            fixture.backend.clone(),
        )
        .await
        .unwrap();
    assert!(outcome.error.is_none());
    assert_eq!(outcome.phase, Some(TransactionPhase::Restored));
    assert!(!project.join("after.rs").exists());
    assert_eq!(
        std::fs::read_to_string(project.join("before.rs")).unwrap(),
        "disk 文😀\r\n"
    );
}
