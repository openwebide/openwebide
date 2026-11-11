//! The same worker contract runs through service and private paired transports.
use super::*;
use futures::FutureExt;
use http_body_util::BodyExt;
use openwebide_core::{
    workspace_entries::WorkspaceEntries,
    workspace_transaction::{
        DelegateWorkspaceTransaction, DelegatedWorkspaceJournalOperation as Operation,
        DelegatedWorkspaceJournalRequest, ResourceScope,
        companion::{CompanionFiles, FolderAction, FolderReply, FolderRequest, FolderTransport},
    },
};

struct Client {
    host: Host,
    workspace: PathBuf,
    operations: Arc<Mutex<Vec<String>>>,
}
impl FolderTransport for Client {
    async fn request(&self, input: &FolderRequest) -> Result<FolderReply, String> {
        let value = serde_json::to_value(input).unwrap();
        let action = &value["action"];
        let label = if action["operation"] == "file" {
            format!("file:{}", action["action"]["operation"].as_str().unwrap())
        } else {
            action["operation"].as_str().unwrap().into()
        };
        self.operations.lock().unwrap().push(label);
        let request = serde_json::from_value(value).unwrap();
        let result = self
            .host
            .folder_request(
                self.workspace.clone(),
                "paired/user:1".into(),
                request,
                None,
            )
            .await?;
        Ok(serde_json::from_slice(&serde_json::to_vec(&result).unwrap()).unwrap())
    }
}
struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(backend: Arc<Backend>) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let backend = backend.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(
                    move |request: hyper::Request<hyper::body::Incoming>| {
                        let backend = backend.clone();
                        async move {
                            assert_eq!(
                                request.uri().path(),
                                "/api/workspace-transactions/delegated"
                            );
                            for header in ["authorization", "cookie", "origin"] {
                                assert!(!request.headers().contains_key(header));
                            }
                            let input: DelegatedWorkspaceJournalRequest = serde_json::from_slice(
                                &request.into_body().collect().await.unwrap().to_bytes(),
                            )
                            .unwrap();
                            match &input.operation {
                                Operation::Inspect => {
                                    backend.scope_reads.fetch_add(1, Ordering::SeqCst);
                                }
                                Operation::Load => {
                                    backend.snapshot_reads.fetch_add(1, Ordering::SeqCst);
                                }
                                Operation::Advance { progress, .. }
                                    if progress.active
                                        && progress.completed
                                            == backend.pause_step.load(Ordering::SeqCst)
                                        && backend.pause.swap(false, Ordering::SeqCst) =>
                                {
                                    backend.entered.notify_one();
                                    backend.release.notified().await;
                                }
                                _ => {}
                            }
                            let response = futures::executor::block_on(
                                backend.store.delegated_workspace_journal(&input, 101),
                            );
                            if matches!(
                                response,
                                Ok(WorkspaceJournalResponse::WorkerClaimed { .. })
                            ) {
                                *backend.last_worker.lock().unwrap() = Some(input.worker);
                            }
                            let (status, body) = match response {
                                Ok(response) => (200, serde_json::to_vec(&response).unwrap()),
                                Err(error) => (
                                    409,
                                    serde_json::to_vec(
                                        &serde_json::json!({"error":error.to_string()}),
                                    )
                                    .unwrap(),
                                ),
                            };
                            Ok::<_, std::convert::Infallible>(
                                hyper::Response::builder()
                                    .status(status)
                                    .body(http_body_util::Full::new(bytes::Bytes::from(body)))
                                    .unwrap(),
                            )
                        }
                    },
                );
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    Server { url, task }
}
async fn files(
    host: &Host,
    fixture: &Fixture,
    operations: Arc<Mutex<Vec<String>>>,
) -> Arc<CompanionFiles<Client>> {
    let client = Client {
        host: host.clone(),
        workspace: fixture.directory.path().into(),
        operations,
    };
    let request = |action| FolderRequest {
        user: fixture.user.get(),
        project: fixture.project,
        action,
    };
    let FolderReply::Opened {
        session,
        challenge,
        identity,
    } = client
        .request(&request(FolderAction::Open {
            path: "project".into(),
            expected: None,
        }))
        .await
        .unwrap()
    else {
        panic!("Missing folder proof")
    };
    let proof =
        std::fs::read_to_string(fixture.directory.path().join("project").join(challenge)).unwrap();
    let FolderReply::Ready { worker, .. } = client
        .request(&request(FolderAction::Prove {
            session: session.clone(),
            proof,
        }))
        .await
        .unwrap()
    else {
        panic!("Missing native worker commitment")
    };
    let mut files = CompanionFiles::new(
        client,
        fixture.user.get(),
        fixture.project,
        session,
        identity,
        worker,
    )
    .unwrap();
    let state = fixture
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
    files
        .bind(ResourceScope {
            transaction: state.journal.id,
            fingerprint: state.journal.plan.fingerprint().unwrap(),
        })
        .await
        .unwrap();
    Arc::new(files)
}
async fn delegation(
    fixture: &Fixture,
    files: &CompanionFiles<Client>,
) -> transaction::WorkspaceJournalDelegation {
    let scope = fixture
        .backend
        .store
        .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
        .await
        .unwrap()
        .unwrap();
    fixture
        .backend
        .store
        .delegate_workspace_transaction(
            fixture.user,
            fixture.project,
            &scope.id,
            &DelegateWorkspaceTransaction {
                generation: scope.generation,
                revision: scope.revision,
                worker: files.worker().clone(),
            },
            100,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn complete_worker_retains_its_physical_gate_and_commits_after_observer_loss_in_both_modes() {
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let fixture = fixture_mode(2, mode).await;
        let host = Host::new();
        let root = fixture.directory.path().join("project");
        let original = same_file::Handle::from_path(root.join("before.rs")).unwrap();
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut local = None;
        let mut server = None;
        let command = fixture.command.clone();
        let backend = fixture.backend.clone();
        fixture.backend.pause.store(true, Ordering::SeqCst);
        let observer = if mode == WorkspaceMode::Local {
            let files = files(&host, &fixture, operations.clone()).await;
            let delegated = delegation(&fixture, &files).await;
            let authority = serve(backend).await;
            let url = authority.url.clone();
            local = Some(files.clone());
            server = Some(authority);
            tokio::spawn(async move { files.execute(url, delegated, command.request).await })
        } else {
            let pending_host = host.clone();
            let workspace = fixture.directory.path().to_path_buf();
            tokio::spawn(async move { pending_host.execute(workspace, command, backend).await })
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            fixture.backend.entered.notified(),
        )
        .await
        .unwrap();
        let mut competing = Box::pin(host.coordinator.acquire(Folder::capture(&root).unwrap()));
        assert!(competing.as_mut().now_or_never().is_none());
        assert_eq!(host.coordinator.waiting(), 1);
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        assert!(competing.as_mut().now_or_never().is_none());
        assert!(root.join("before.rs").exists());
        let current = fixture
            .backend
            .store
            .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
            .await
            .unwrap()
            .unwrap();
        assert!(current.worker.unwrap().active);
        if let Some(files) = &local {
            assert!(
                files
                    .read_bytes("before.rs")
                    .await
                    .unwrap_err()
                    .contains("acknowledgement was lost")
            );
        }
        fixture.backend.release.notify_one();
        let lease = tokio::time::timeout(std::time::Duration::from_secs(10), competing)
            .await
            .unwrap()
            .unwrap();
        let state = fixture
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
        assert_eq!(state.journal.progress.phase, TransactionPhase::Committed);
        let current = fixture
            .backend
            .store
            .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!current.worker.unwrap().active);
        assert_eq!(
            same_file::Handle::from_path(root.join("after.rs")).unwrap(),
            original
        );
        assert!(!root.join("before.rs").exists());
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
        drop(lease);
        if let Some(files) = local {
            files.close().await.unwrap();
            assert_eq!(
                *operations.lock().unwrap(),
                vec!["open", "prove", "file:bind", "execute", "close"]
            );
        }
        drop(server);
    }
}

#[tokio::test]
async fn queued_complete_workers_load_no_snapshot_and_reject_changed_recovery_in_both_modes() {
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let fixture = fixture_mode(2, mode).await;
        let host = Host::new();
        let root = fixture.directory.path().join("project");
        let mut local = None;
        let mut server = None;
        let operations = Arc::new(Mutex::new(Vec::new()));
        let command = fixture.command.clone();
        let backend = fixture.backend.clone();
        let prepared = if mode == WorkspaceMode::Local {
            let files = files(&host, &fixture, operations).await;
            let delegated = delegation(&fixture, &files).await;
            let authority = serve(backend.clone()).await;
            let url = authority.url.clone();
            server = Some(authority);
            local = Some(files.clone());
            Some((files, delegated, url))
        } else {
            None
        };
        let held = host
            .coordinator
            .acquire(Folder::capture(&root).unwrap())
            .await
            .unwrap();
        let observer = if let Some((files, delegated, url)) = prepared {
            tokio::spawn(async move { files.execute(url, delegated, command.request).await })
        } else {
            let pending_host = host.clone();
            let workspace = fixture.directory.path().to_path_buf();
            tokio::spawn(async move { pending_host.execute(workspace, command, backend).await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while host.coordinator.waiting() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.backend.scope_reads.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.backend.snapshot_reads.load(Ordering::SeqCst), 0);
        drop(host.slots.clone().try_acquire_owned().unwrap());
        fixture
            .backend
            .store
            .advance_workspace_transaction(
                fixture.user,
                fixture.project,
                &AdvanceWorkspaceTransaction {
                    id: fixture.command.request.id.clone(),
                    generation: fixture.command.request.generation.clone(),
                    revision: 1,
                    progress: TransactionProgress::prepared_cancellation(),
                },
                102,
            )
            .await
            .unwrap();
        drop(held);
        assert!(observer.await.unwrap().is_err());
        let scope = fixture
            .backend
            .store
            .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
            .await
            .unwrap()
            .unwrap();
        assert!(scope.worker.is_none());
        assert!(root.join("before.rs").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("after.rs")).unwrap(),
            "overwritten original"
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
        if let Some(files) = local {
            files.close().await.unwrap();
        }
        drop(server);
    }
}
