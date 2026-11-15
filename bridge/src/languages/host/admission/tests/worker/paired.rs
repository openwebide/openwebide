use super::*;
use http_body_util::BodyExt;
use openwebide_core::workspace_transaction as transaction;
use openwebide_core::workspace_transaction::{
    DelegateWorkspaceTransaction, DelegatedWorkspaceJournalRequest, WorkspaceTransactionWorkerKey,
    companion::{FolderAction, FolderReply, FolderRequest},
};

pub(super) async fn call(
    host: &Transactions,
    fixture: &Fixture,
    action: FolderAction,
) -> FolderReply {
    let request = FolderRequest {
        user: 1,
        project: 1,
        action,
    };
    let request = serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
    host.folder_request(
        fixture.scope.root.parent().unwrap().to_path_buf(),
        fixture.scope.owner.clone(),
        request,
        Some(fixture.host.clone()),
    )
    .await
    .unwrap()
}
pub(super) async fn prove(
    host: &Transactions,
    fixture: &Fixture,
    expected: Option<String>,
) -> (String, WorkspaceTransactionWorkerKey, String) {
    let FolderReply::Opened {
        session,
        challenge,
        identity,
    } = call(
        host,
        fixture,
        FolderAction::Open {
            path: fixture
                .scope
                .root
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            expected,
        },
    )
    .await
    else {
        panic!("Missing original folder proof")
    };
    let proof = std::fs::read_to_string(fixture.root.path().join(challenge)).unwrap();
    let FolderReply::Ready { worker, .. } = call(
        host,
        fixture,
        FolderAction::Prove {
            session: session.clone(),
            proof,
        },
    )
    .await
    else {
        panic!("Missing original native worker")
    };
    (session, worker, identity)
}
pub(super) async fn delegate(
    backend: &Backend,
    state: &WorkspaceTransactionState,
    worker: WorkspaceTransactionWorkerKey,
    now: i64,
) -> transaction::WorkspaceJournalDelegation {
    let scope = backend
        .store
        .workspace_transaction_scope(UserId::new(1), 1, &state.journal.id)
        .await
        .unwrap()
        .unwrap();
    backend
        .store
        .delegate_workspace_transaction(
            UserId::new(1),
            1,
            &scope.id,
            &DelegateWorkspaceTransaction {
                generation: scope.generation,
                revision: scope.revision,
                worker,
            },
            now,
        )
        .await
        .unwrap()
}
pub(super) struct Server {
    pub(super) url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) async fn serve(backend: Arc<Backend>) -> Server {
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
                            let path = request.uri().path().to_string();
                            for header in ["cookie", "origin"] {
                                assert!(!request.headers().contains_key(header));
                            }
                            let service = path.ends_with("/service");
                            if service {
                                assert_eq!(
                                    request.headers()["authorization"],
                                    "Bearer native-command-test-service"
                                );
                            } else {
                                assert!(!request.headers().contains_key("authorization"));
                            }
                            let bytes = request.into_body().collect().await.unwrap().to_bytes();
                            let private: Option<protocol::actions::CommandRecoveryMessage> =
                                if path == "/api/language-commands/original-private" {
                                    Some(serde_json::from_slice(&bytes).unwrap())
                                } else {
                                    None
                                };

                            let response: Result<Reply, String> = if path
                                == "/api/workspace-transactions/delegated"
                            {
                                let request: DelegatedWorkspaceJournalRequest =
                                    serde_json::from_slice(&bytes).unwrap();
                                let response = futures::executor::block_on(async {
                                    if matches!(request.operation, transaction::DelegatedWorkspaceJournalOperation::Admit { .. }) {
                                        backend.observe_reservation(UserId::new(request.delegation.user), request.delegation.project, &request.delegation.scope.id).await?;
                                    }
                                    backend.store.delegated_workspace_journal(
                                        &request,
                                        backend.now.load(Ordering::SeqCst),
                                    ).await
                                })
                                .map_err(|e| e.to_string());
                                if response.is_ok() && let transaction::DelegatedWorkspaceJournalOperation::Advance {progress,..}=&request.operation {backend.after_progress(progress);}
                                if response.is_ok() && matches!(request.operation,transaction::DelegatedWorkspaceJournalOperation::RecoveryScope|transaction::DelegatedWorkspaceJournalOperation::CommandRecoveryScope) {
                                    futures::executor::block_on(backend.private_reply(UserId::new(request.delegation.user),request.delegation.project,&request.delegation.scope.id,matches!(request.operation,transaction::DelegatedWorkspaceJournalOperation::CommandRecoveryScope))).map_err(|e|e.to_string())
                                } else {response}
                            } else if let Some(message) = &private {
                                use protocol::actions::{
                                    COMMAND_RECOVERY_PLAINTEXT_LIMIT,
                                    COMMAND_RECOVERY_REQUEST_DOMAIN, CommandJournalOperation,
                                    OriginalCommandRequest,
                                };
                                futures::executor::block_on(async {
                                    let key = backend
                                        .store
                                        .get_setting("native_test_original_issuer")
                                        .await
                                        .map_err(|e| e.to_string())?
                                        .ok_or("Missing original fixture issuer")?;
                                    let binding =
                                        transaction::RecoveryAuthority::binding_for_scope(
                                            message.user,
                                            message.project,
                                            &message.scope,
                                        )?;
                                    let authority = transaction::RecoveryAuthority::for_scope(
                                        message.user,
                                        message.project,
                                        &message.scope,
                                        openwebide_auth::derive_secret(
                                            key.as_bytes(),
                                            transaction::RECOVERY_AUTHORITY_DOMAIN,
                                            &binding,
                                        ),
                                    )?;
                                    let body = openwebide_auth::private_messages::open(
                                        authority.secret.as_bytes(),
                                        COMMAND_RECOVERY_REQUEST_DOMAIN,
                                        &message.context()?,
                                        &message.nonce,
                                        &message.ciphertext,
                                        COMMAND_RECOVERY_PLAINTEXT_LIMIT,
                                    )?;
                                    let input: OriginalCommandRequest =
                                        serde_json::from_slice(&body)
                                            .map_err(|_| "Invalid private fixture request")?;
                                    if input.authority != authority {
                                        return Err("Original fixture authority changed".into());
                                    }
                                    let reply = backend
                                        .store
                                        .original_language_command(
                                            &input,
                                            backend.now.load(Ordering::SeqCst),
                                        )
                                        .await
                                        .map_err(|e| e.to_string())?;
                                    if matches!(input.operation, CommandJournalOperation::Scope) {
                                        let Reply::CommandScope {
                                            recovery: Some(recovery),
                                        } = reply
                                        else {
                                            return Err("Missing original fixture scope".into());
                                        };
                                        Ok(Reply::CommandRecoveryScope {
                                            recovery,
                                            authority: Box::new(authority),
                                        })
                                    } else {
                                        Ok(reply)
                                    }
                                })
                            } else if path == "/api/language-commands/delegated" {
                                use protocol::actions::{
                                    CommandJournalOperation, DelegatedCommandRequest,
                                };
                                let request: DelegatedCommandRequest =
                                    serde_json::from_slice(&bytes).unwrap();
                                let response = futures::executor::block_on(
                                    backend.store.delegated_language_command(
                                        &request,
                                        backend.now.load(Ordering::SeqCst),
                                    ),
                                )
                                .map_err(|e| e.to_string());
                                if response.is_ok()
                                    && matches!(request.operation, CommandJournalOperation::Scope)
                                {
                                    futures::executor::block_on(backend.private_reply(
                                        UserId::new(request.delegation.user),
                                        request.delegation.project,
                                        &request.delegation.scope.id,
                                        true,
                                    ))
                                    .map_err(|e| e.to_string())
                                } else {
                                    response
                                }
                            } else {
                                assert!(service);
                                let request: WorkspaceJournalRequest =
                                    serde_json::from_slice(&bytes).unwrap();
                                backend.request(&request).await
                            };
                            let (status, body) = match response {
                                Ok(reply) => {
                                    let body = serde_json::to_vec(&reply).unwrap();
                                    let body = if let Some(message) = &private {
                                        let key = backend
                                            .store
                                            .get_setting("native_test_original_issuer")
                                            .await
                                            .unwrap()
                                            .unwrap();
                                        let binding =
                                            transaction::RecoveryAuthority::binding_for_scope(
                                                message.user,
                                                message.project,
                                                &message.scope,
                                            )
                                            .unwrap();
                                        let secret = openwebide_auth::derive_secret(
                                            key.as_bytes(),
                                            transaction::RECOVERY_AUTHORITY_DOMAIN,
                                            &binding,
                                        );
                                        let (nonce, ciphertext) =
                                            openwebide_auth::private_messages::seal(
                                                secret.as_bytes(),
                                                protocol::actions::COMMAND_RECOVERY_REPLY_DOMAIN,
                                                &message.reply_context().unwrap(),
                                                &body,
                                            )
                                            .unwrap();
                                        serde_json::to_vec(
                                            &protocol::actions::CommandRecoveryReply {
                                                nonce,
                                                ciphertext,
                                            },
                                        )
                                        .unwrap()
                                    } else {
                                        body
                                    };
                                    (200, body)
                                }
                                Err(error) => (
                                    409,
                                    serde_json::to_vec(&json!({"error":error.to_string()}))
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
pub(super) async fn bind(
    host: &Transactions,
    fixture: &Fixture,
    state: &WorkspaceTransactionState,
    session: &str,
) {
    assert!(matches!(
        call(
            host,
            fixture,
            FolderAction::File {
                session: session.into(),
                sequence: 1,
                action: transaction::companion::FileAction::Bind {
                    scope: ResourceScope {
                        transaction: state.journal.id.clone(),
                        fingerprint: state.journal.plan.fingerprint().unwrap()
                    }
                },
            }
        )
        .await,
        FolderReply::File { outcome: Ok(_) }
    ));
}

#[tokio::test]
async fn paired_native_worker_verifies_original_language_review_and_recovers_accepted_identity_on_a_successor_session()
 {
    let host = Transactions::new();
    let mut fixture = Fixture::with_mode("paired/user:1", "inputs");
    std::fs::write(
        fixture.root.path().join("settings-old.json"),
        "{\"original\":true}",
    )
    .unwrap();
    // The original folder proof precedes discovery/analysis; no later proof may
    // refresh an unaccepted result or invalidate its directory generation.
    let (session, worker, identity) = prove(&host, &fixture, None).await;
    let state = fixture.state().await;
    fixture.prepare(&state).await;
    let (fixture, state, backend, mut command) = store_review(fixture, state, true).await;
    assert_eq!(state.editor.root.mode, WorkspaceMode::Local);
    assert_eq!(identity, state.journal.plan.root);
    bind(&host, &fixture, &state, &session).await;
    let delegation = delegate(&backend, &state, worker, 100).await;
    let server = serve(backend.clone()).await;
    let calls = fixture.calls();
    let mut unauthorized = command.request.clone();
    unauthorized.language_grant = Some("0".repeat(32));
    assert!(
        host.folder_request(
            fixture.scope.root.parent().unwrap().to_path_buf(),
            fixture.scope.owner.clone(),
            FolderRequest {
                user: 1,
                project: 1,
                action: FolderAction::Execute {
                    session: session.clone(),
                    api_url: server.url.clone(),
                    delegation: delegation.clone(),
                    request: unauthorized
                },
            },
            Some(fixture.host.clone())
        )
        .await
        .is_err()
    );
    let rejected = backend
        .store
        .workspace_transaction_scope(UserId::new(1), 1, &state.journal.id)
        .await
        .unwrap()
        .unwrap();
    assert!(rejected.worker.is_none() && rejected.accepted_language_review_digest.is_none());
    assert!(fixture.root.path().join("settings-old.json").exists());
    let FolderReply::Executed { outcome } = call(
        &host,
        &fixture,
        FolderAction::Execute {
            session: session.clone(),
            api_url: server.url.clone(),
            delegation,
            request: command.request.clone(),
        },
    )
    .await
    else {
        panic!("Missing whole native outcome")
    };
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
    assert!(!fixture.root.path().join("settings-old.json").exists());
    assert!(fixture.root.path().join("settings-new.json").exists());
    assert_eq!(fixture.calls(), calls);
    assert!(!fixture.root.path().join("executions").exists());
    let scope = backend
        .store
        .workspace_transaction_scope(UserId::new(1), 1, &state.journal.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        scope.accepted_language_review_digest,
        Some(
            state
                .language_review
                .as_ref()
                .unwrap()
                .fingerprint()
                .unwrap()
        )
    );
    assert!(!scope.worker.as_ref().unwrap().active);
    assert!(matches!(
        call(&host, &fixture, FolderAction::Close { session }).await,
        FolderReply::Closed
    ));
    fixture.host.shutdown();
    backend.now.store(100_001, Ordering::SeqCst);
    backend
        .store
        .set_user_setting(
            UserId::new(1),
            "language_settings",
            "{revoked configuration",
        )
        .await
        .unwrap();
    let (session, worker, _) = prove(&host, &fixture, Some(identity)).await;
    bind(&host, &fixture, &state, &session).await;
    let delegation = delegate(&backend, &state, worker, 100_000).await;
    command.request.language_grant = None;
    // Apply acknowledges an already committed decision, rather than replaying
    // files or its command. The expired grant and original service are unused.
    let FolderReply::Executed { outcome } = call(
        &host,
        &fixture,
        FolderAction::Execute {
            session: session.clone(),
            api_url: server.url.clone(),
            delegation,
            request: command.request,
        },
    )
    .await
    else {
        panic!("Missing accepted successor outcome")
    };
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
    assert!(fixture.root.path().join("settings-new.json").exists());
    assert!(!fixture.root.path().join("executions").exists());
    call(&host, &fixture, FolderAction::Close { session }).await;
}
