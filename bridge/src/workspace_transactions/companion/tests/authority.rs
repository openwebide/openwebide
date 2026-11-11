//! Primitive filesystem fixtures use actual SQLite admission, independently of
//! the shared interpreter contract in journal.rs. Their journal inventories are
//! not an assertion that arbitrary test primitives form a complete transaction.
use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use openwebide_core::{
    UserId,
    workspace_transaction::{
        self as transaction, DelegateWorkspaceTransaction, DelegatedWorkspaceJournalRequest,
        WorkspaceJournalDelegation, WorkspaceTransactionWorkerKey,
    },
};

pub(super) struct Authority {
    pub backend: Arc<super::super::super::tests::Backend>,
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Authority {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Authority {
    pub async fn serve(backend: Arc<super::super::super::tests::Backend>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api", listener.local_addr().unwrap());
        let server = backend.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let backend = server.clone();
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
                                let request: DelegatedWorkspaceJournalRequest =
                                    serde_json::from_slice(
                                        &request.into_body().collect().await.unwrap().to_bytes(),
                                    )
                                    .unwrap();
                                let result = futures::executor::block_on(
                                    backend.store.delegated_workspace_journal(&request, 101),
                                );
                                let (status, body) = match result {
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
                                        .body(Full::new(Bytes::from(body)))
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
        Self { backend, url, task }
    }
    async fn primitive(root: &str, id: &str) -> Self {
        let fixture =
            super::super::super::tests::fixture_mode(2, openwebide_core::WorkspaceMode::Local)
                .await;
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
        fixture
            .backend
            .store
            .advance_workspace_transaction(
                fixture.user,
                fixture.project,
                &transaction::AdvanceWorkspaceTransaction {
                    id: state.journal.id,
                    generation: state.journal.generation,
                    revision: state.journal.revision,
                    progress: transaction::TransactionProgress::prepared_cancellation(),
                },
                100,
            )
            .await
            .unwrap();
        let mut plan = state.journal.plan;
        plan.root = root.into();
        fixture
            .backend
            .store
            .begin_workspace_transaction(
                fixture.user,
                fixture.project,
                &transaction::BeginWorkspaceTransaction {
                    language_review: None,
                    id: id.into(),
                    plan,
                    editor: state.editor,
                },
                100,
            )
            .await
            .unwrap();
        Self::serve(fixture.backend).await
    }
    pub async fn grant(
        &self,
        id: &str,
        worker: &WorkspaceTransactionWorkerKey,
    ) -> WorkspaceJournalDelegation {
        let scope = self
            .backend
            .store
            .workspace_transaction_scope(UserId::new(1), 1, id)
            .await
            .unwrap()
            .unwrap();
        self.backend
            .store
            .delegate_workspace_transaction(
                UserId::new(1),
                1,
                id,
                &DelegateWorkspaceTransaction {
                    generation: scope.generation,
                    revision: scope.revision,
                    worker: worker.clone(),
                },
                100,
            )
            .await
            .unwrap()
    }
}
impl Case {
    pub async fn bind(
        &self,
        files: &mut CompanionFiles<Client>,
        scope: ResourceScope,
    ) -> Result<(), String> {
        files.bind(scope.clone()).await?;
        let authority = Authority::primitive(files.root_identity(), &scope.transaction).await;
        let grant = authority.grant(&scope.transaction, files.worker()).await;
        let scope = grant.scope.clone();
        files.admit(authority.url.clone(), grant).await?;
        files
            .advance(
                &scope.id,
                &scope.generation,
                scope.revision,
                &transaction::TransactionProgress {
                    phase: transaction::TransactionPhase::Applying,
                    ..Default::default()
                },
            )
            .await?;
        self.authorities.lock().unwrap().push(authority);
        Ok(())
    }
    pub async fn admit_session(&self, id: &str, scope: &ResourceScope) {
        let (root, worker) = {
            let state = self.host.state.lock().await;
            let session = &state.sessions[id];
            (
                session.root.identity.clone(),
                session.worker.as_ref().unwrap().delegation_key(),
            )
        };
        let authority = Authority::primitive(&root, &scope.transaction).await;
        let delegation = authority.grant(&scope.transaction, &worker).await;
        let cursor = delegation.scope.clone();
        assert!(matches!(
            self.request(FolderAction::Admit {
                session: id.into(),
                api_url: authority.url.clone(),
                delegation
            })
            .await
            .unwrap(),
            FolderReply::Admitted
        ));
        assert!(matches!(
            self.request(FolderAction::Advance {
                session: id.into(),
                request: transaction::AdvanceWorkspaceTransaction {
                    id: cursor.id,
                    generation: cursor.generation,
                    revision: cursor.revision,
                    progress: transaction::TransactionProgress {
                        phase: transaction::TransactionPhase::Applying,
                        ..Default::default()
                    },
                }
            })
            .await
            .unwrap(),
            FolderReply::Advanced { .. }
        ));
        self.authorities.lock().unwrap().push(authority);
    }
}

#[tokio::test]
async fn proven_folder_returns_a_stable_commitment_without_disclosing_the_private_worker() {
    let case = Case::new();
    let (id, name, _) = case.open().await;
    let proof = case.prove(&id, &name).await;
    let reply = case
        .request(FolderAction::Prove {
            session: id.clone(),
            proof: proof.clone(),
        })
        .await
        .unwrap();
    let FolderReply::Ready { worker, .. } = &reply else {
        panic!()
    };
    let (key, secret) = {
        let state = case.host.state.lock().await;
        let private = state.sessions[&id].worker.as_ref().unwrap();
        (private.delegation_key(), private.secret.clone())
    };
    assert_eq!(*worker, key);
    assert!(!serde_json::to_string(&reply).unwrap().contains(&secret));
    let repeated = case
        .request(FolderAction::Prove { session: id, proof })
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(reply).unwrap(),
        serde_json::to_value(repeated).unwrap()
    );
}

#[tokio::test]
async fn a_proven_and_bound_session_cannot_mutate_without_journal_admission() {
    let case = Case::new();
    std::fs::write(case.root().join("source"), "original").unwrap();
    let mut files = case.files().await;
    files.bind(scope()).await.unwrap();
    let id = case
        .host
        .state
        .lock()
        .await
        .sessions
        .keys()
        .next()
        .unwrap()
        .clone();
    let actions = [
        FileAction::Create {
            ordinal: 0,
            path: "new".into(),
            is_dir: false,
        },
        FileAction::ReconcileCreation {
            ordinal: 0,
            path: "new".into(),
            is_dir: false,
        },
        FileAction::RemoveCreated {
            ordinal: 0,
            path: "source".into(),
            is_dir: false,
        },
        FileAction::Transfer {
            from: ResourceLocation::Workspace("source".into()),
            to: ResourceLocation::Workspace("moved".into()),
            is_dir: false,
        },
        FileAction::RemoveRetained {
            path: "0".into(),
            is_dir: false,
        },
        FileAction::Finish,
    ];
    for action in actions {
        assert!(action.mutates());
        let error = case.call(&id, 2, action).await.unwrap_err();
        assert!(error.contains("Admit the paired journal"), "{error}");
    }
    assert_eq!(
        std::fs::read_to_string(case.root().join("source")).unwrap(),
        "original"
    );
    assert!(!case.root().join("new").exists());
    assert!(!case.root().join("moved").exists());
    assert!(!case.root().join(".openwebide").exists());
}
