use super::*;
use crate::folder_coordination::{Coordinator, Folder};
use openwebide_core::{
    WorkspaceMode,
    workspace_transaction::{DelegateWorkspaceTransaction, TransactionPhase},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Journal<'a> {
    client: &'a PairedJournal,
    backend: std::sync::Arc<crate::workspace_transactions::tests::Backend>,
    user: openwebide_core::UserId,
    project: i64,
    id: String,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl openwebide_core::workspace_transaction::WorkspaceJournal for Journal<'_> {
    async fn prepare(
        &self,
        plan: &openwebide_core::workspace_transaction::WorkspacePlan,
    ) -> Result<openwebide_core::workspace_transaction::PreparedJournal, String> {
        self.load(None)
            .await?
            .ok_or("missing journal")?
            .prepared_for(plan)
    }
    async fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &TransactionProgress,
    ) -> Result<u64, String> {
        if id != self.id || generation != self.client.delegation.scope.generation {
            return Err("wrong journal".into());
        }
        self.client
            .advance(revision, progress.clone())
            .await
            .map(|response| response.revision)
    }
    async fn load(
        &self,
        _id: Option<&str>,
    ) -> Result<Option<openwebide_core::workspace_transaction::JournalRecord>, String> {
        self.backend
            .store
            .workspace_transaction(self.user, self.project, Some(&self.id))
            .await
            .map(|state| state.map(|state| state.journal))
            .map_err(|error| error.to_string())
    }
}

#[tokio::test]
async fn paired_journal_uses_its_scoped_credentials_over_http_without_a_service_secret() {
    let fixture = crate::workspace_transactions::tests::fixture_mode(2, WorkspaceMode::Local).await;
    let scope = fixture
        .backend
        .store
        .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
        .await
        .unwrap()
        .unwrap();
    let worker = WorkspaceTransactionWorker {
        id: "a".repeat(32),
        secret: "b".repeat(32),
    };
    let grant = fixture
        .backend
        .store
        .delegate_workspace_transaction(
            fixture.user,
            fixture.project,
            &scope.id,
            &DelegateWorkspaceTransaction {
                generation: scope.generation.clone(),
                revision: scope.revision,
                worker: worker.delegation_key(),
            },
            100,
        )
        .await
        .unwrap();
    let root = fixture.directory.path().join("project");
    let coordinator = Coordinator::default();
    let _physical = coordinator
        .acquire(Folder::capture(&root).unwrap())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let backend = fixture.backend.clone();
    let server = tokio::spawn(async move {
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
                            let request: DelegatedWorkspaceJournalRequest = serde_json::from_slice(
                                &request.into_body().collect().await.unwrap().to_bytes(),
                            )
                            .unwrap();
                            let result = futures::executor::block_on(
                                backend.store.delegated_workspace_journal(&request, 101),
                            );
                            let (status, value) = match result {
                                Ok(value) => (200, serde_json::to_vec(&value).unwrap()),
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
                                    .body(Full::new(Bytes::from(value)))
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
    let client = PairedJournal::new(
        format!("http://{address}/api"),
        &scope.root_identity,
        grant,
        worker.clone(),
    )
    .unwrap();
    assert_eq!(client.admit().await.unwrap().worker, Some(worker.state()));
    assert_eq!(client.scope().await.unwrap().revision, scope.revision);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let response = client
        .advance(
            scope.revision,
            TransactionProgress {
                phase: TransactionPhase::Applying,
                ..TransactionProgress::default()
            },
        )
        .await
        .unwrap();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        assert_eq!(response.revision, scope.revision + 1);
        assert!(response.editor.is_none());
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use openwebide_core::workspace_transaction::{self as transaction, WorkspaceJournal};
        let journal = Journal {
            client: &client,
            backend: fixture.backend.clone(),
            user: fixture.user,
            project: fixture.project,
            id: scope.id.clone(),
        };
        let record = journal.load(None).await.unwrap().unwrap();
        let original = same_file::Handle::from_path(root.join("before.rs")).unwrap();
        let root_handle = std::fs::File::open(&root).unwrap();
        let files = crate::runs::native_vfs::resources::ResourceFiles::new(
            &root_handle,
            &scope.root_identity,
            transaction::ResourceScope {
                transaction: scope.id.clone(),
                fingerprint: record.plan.fingerprint().unwrap(),
            },
        )
        .unwrap();
        let completed = transaction::execute(
            &files,
            &journal,
            record,
            transaction::WorkspaceTransactionAction::Apply,
            || true,
        )
        .await
        .unwrap();
        assert_eq!(completed.progress.phase, TransactionPhase::Committed);
        assert!(!root.join("before.rs").exists());
        assert!(root.join("after.rs").exists());
        let editor = fixture
            .backend
            .store
            .editor_recovery(fixture.user, fixture.project)
            .await
            .unwrap();
        assert_eq!(editor.revision, 2);
        assert_eq!(editor.state.files[0].path, "after.rs");
        assert_eq!(
            same_file::Handle::from_path(root.join("after.rs")).unwrap(),
            original
        );
    }
    client.release().await.unwrap();
    assert!(client.scope().await.is_err());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn paired_cursor_responses_are_bounded_before_decoding() {
    let fixture = crate::workspace_transactions::tests::fixture_mode(2, WorkspaceMode::Local).await;
    let scope = fixture
        .backend
        .store
        .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
        .await
        .unwrap()
        .unwrap();
    let worker = WorkspaceTransactionWorker {
        id: "a".repeat(32),
        secret: "b".repeat(32),
    };
    let grant = fixture
        .backend
        .store
        .delegate_workspace_transaction(
            fixture.user,
            fixture.project,
            &scope.id,
            &DelegateWorkspaceTransaction {
                generation: scope.generation.clone(),
                revision: scope.revision,
                worker: worker.delegation_key(),
            },
            100,
        )
        .await
        .unwrap();
    let body =
        r#"{"result":"scope","scope":null}"#.to_owned() + &" ".repeat(RECOVERY_SCOPE_BODY_LIMIT);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, captured) =
        crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str())).await;
    let client =
        PairedJournal::new(format!("{url}/api"), &scope.root_identity, grant, worker).unwrap();
    assert!(
        client
            .scope()
            .await
            .unwrap_err()
            .contains("exceeded its limit")
    );
    let request = captured.await.unwrap();
    assert!(!request.to_ascii_lowercase().contains("authorization:"));
}
