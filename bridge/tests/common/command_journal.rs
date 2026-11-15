//! Thin HTTP/SQLite fixture: production Store owns every grant and journal CAS.
use http_body_util::BodyExt;
use openwebide_bridge::workspace_transactions::JournalBackend;
use openwebide_core::plugins::languages::intelligence::actions::{
    COMMAND_RECOVERY_PLAINTEXT_LIMIT, COMMAND_RECOVERY_REPLY_DOMAIN,
    COMMAND_RECOVERY_REQUEST_DOMAIN, CommandJournalOperation, CommandRecoveryMessage,
    CommandRecoveryReply, CommandWorkerOperation, DelegatedCommandRequest, OriginalCommandRequest,
};
use openwebide_core::{
    UserId,
    workspace_transaction::{
        AdvanceWorkspaceTransaction, DelegatedWorkspaceJournalOperation,
        DelegatedWorkspaceJournalRequest, RECOVERY_AUTHORITY_DOMAIN, RecoveryAuthority,
        WorkspaceJournalOperation as Operation, WorkspaceJournalRequest,
        WorkspaceJournalResponse as Reply,
    },
};
use openwebide_storage::{Store, rusqlite_db::RusqliteDb};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
};

pub struct Backend {
    pub store: Store<RusqliteDb>,
    pub now: AtomicI64,
    pub original_unavailable: AtomicBool,
    pub original_calls: AtomicUsize,
    pub original_scope_calls: AtomicUsize,
    pub block_scope_at: AtomicUsize,
    pub scope_blocked: AtomicBool,
    pub release_scope: tokio::sync::Semaphore,
    pub scope_released: AtomicBool,
    pub scope_abandoned: AtomicBool,
    pub drop_settlement_ack: AtomicBool,
    pub lost_settlement_acks: AtomicUsize,
    pub reject_file_admission: AtomicBool,
    pub drop_cancellation_ack: AtomicBool,
    pub lost_cancellation_acks: AtomicUsize,
    pub drop_outcome_reply: AtomicBool,
    pub lost_outcome_replies: AtomicUsize,
    pub block_command_claim: AtomicBool,
    pub block_claim_after_commit: AtomicBool,
    pub held_command_claim: std::sync::Mutex<Option<CommandWorkerOperation>>,
    pub release_command_claim: tokio::sync::Semaphore,
    pub reject_initial_settlement: AtomicBool,
    pub rejected_initial_settlements: AtomicUsize,
    pub drop_settlement_metadata_ack: AtomicBool,
    pub lost_settlement_metadata_acks: AtomicUsize,
    pub last_private_reply: std::sync::Mutex<Option<Vec<u8>>>,
}
struct ScopeGate<'a>(&'a Backend);
impl Drop for ScopeGate<'_> {
    fn drop(&mut self) {
        if !self.0.scope_released.load(Ordering::SeqCst) {
            self.0.scope_abandoned.store(true, Ordering::SeqCst);
        }
    }
}
impl Backend {
    fn settlement_reply(&self, operation: &CommandWorkerOperation) -> Result<(), std::io::Error> {
        if matches!(operation, CommandWorkerOperation::Settle { .. })
            && self.drop_settlement_ack.swap(false, Ordering::SeqCst)
        {
            // Commit through Store first, then close the actual adapter's
            // connection before its ACK and hold subsequent original callbacks.
            self.original_unavailable.store(true, Ordering::SeqCst);
            self.lost_settlement_acks.fetch_add(1, Ordering::SeqCst);
            return Err(std::io::Error::other(
                "Lost original settlement acknowledgement",
            ));
        }
        Ok(())
    }
    fn reject_settlement(&self, operation: &CommandWorkerOperation) -> bool {
        if matches!(operation, CommandWorkerOperation::Settle { .. })
            && self.reject_initial_settlement.load(Ordering::SeqCst)
        {
            self.rejected_initial_settlements
                .fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    }
    async fn command_claim_gate(&self, operation: &CommandWorkerOperation, after_commit: bool) {
        if matches!(operation, CommandWorkerOperation::Claim { .. })
            && self.block_command_claim.load(Ordering::SeqCst)
            && self.block_claim_after_commit.load(Ordering::SeqCst) == after_commit
        {
            *self.held_command_claim.lock().unwrap() = Some(operation.clone());
            self.release_command_claim.acquire().await.unwrap().forget();
        }
    }
    async fn original_reply(&self, input: &OriginalCommandRequest) -> Result<Reply, String> {
        self.original_calls.fetch_add(1, Ordering::SeqCst);
        if self.original_unavailable.load(Ordering::SeqCst) {
            return Err("Original callback temporarily unavailable".into());
        }
        input.validate()?;
        if let CommandJournalOperation::Worker { operation } = &input.operation
            && self.reject_settlement(operation)
        {
            return Err("Injected initial settlement transport failure".into());
        }
        let scope = openwebide_core::workspace_transaction::WorkspaceTransactionScope {
            id: input.authority.transaction.clone(),
            generation: input.authority.generation.clone(),
            revision: 1,
            root: input.authority.root.clone(),
            root_identity: input.authority.root_identity.clone(),
            worker: None,
            accepted_language_review_digest: None,
        };
        if self
            .authority(
                UserId::new(input.authority.user),
                input.authority.project,
                &scope,
            )
            .await?
            != input.authority
        {
            return Err("Original issuer changed".into());
        }
        let value = self
            .store
            .original_language_command(input, self.now.load(Ordering::SeqCst))
            .await
            .map_err(|e| e.to_string())?;
        if matches!(input.operation, CommandJournalOperation::Scope) {
            let Reply::CommandScope {
                recovery: Some(recovery),
            } = value
            else {
                return Err("Missing original command scope".into());
            };
            let authority = self
                .authority(
                    UserId::new(input.authority.user),
                    input.authority.project,
                    &recovery.scope,
                )
                .await?;
            Ok(Reply::CommandRecoveryScope {
                recovery,
                authority: Box::new(authority),
            })
        } else {
            Ok(value)
        }
    }
    pub async fn new() -> Arc<Self> {
        Self::database(RusqliteDb::open_in_memory().unwrap()).await
    }
    pub async fn persisted(path: &std::path::Path) -> Arc<Self> {
        Self::database(RusqliteDb::open(path).unwrap()).await
    }
    async fn database(database: RusqliteDb) -> Arc<Self> {
        let store = Store::new(database);
        store.migrate().await.unwrap();
        store
            .insert_setting_if_absent(
                "http_test_issuer",
                &format!("{:032x}", rand::random::<u128>()),
            )
            .await
            .unwrap();
        Arc::new(Self {
            store,
            now: AtomicI64::new(101),
            original_unavailable: AtomicBool::new(false),
            original_calls: AtomicUsize::new(0),
            original_scope_calls: AtomicUsize::new(0),
            block_scope_at: AtomicUsize::new(0),
            scope_blocked: AtomicBool::new(false),
            release_scope: tokio::sync::Semaphore::new(0),
            scope_released: AtomicBool::new(false),
            scope_abandoned: AtomicBool::new(false),
            drop_settlement_ack: AtomicBool::new(false),
            lost_settlement_acks: AtomicUsize::new(0),
            reject_file_admission: AtomicBool::new(false),
            drop_cancellation_ack: AtomicBool::new(false),
            lost_cancellation_acks: AtomicUsize::new(0),
            drop_outcome_reply: AtomicBool::new(false),
            lost_outcome_replies: AtomicUsize::new(0),
            block_command_claim: AtomicBool::new(false),
            block_claim_after_commit: AtomicBool::new(false),
            held_command_claim: std::sync::Mutex::new(None),
            release_command_claim: tokio::sync::Semaphore::new(0),
            reject_initial_settlement: AtomicBool::new(false),
            rejected_initial_settlements: AtomicUsize::new(0),
            drop_settlement_metadata_ack: AtomicBool::new(false),
            lost_settlement_metadata_acks: AtomicUsize::new(0),
            last_private_reply: std::sync::Mutex::new(None),
        })
    }
    async fn private_reply(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        command: bool,
    ) -> Result<Reply, String> {
        let (scope, activity) = self
            .store
            .workspace_transaction_activity(user, project, id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Missing original journal")?;
        let authority = self.authority(user, project, &scope).await?;
        Ok(if command {
            Reply::CommandRecoveryScope {
                recovery: Box::new(
                    self.store
                        .workspace_command_recovery(user, project, id)
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or("Missing original command")?,
                ),
                authority: Box::new(authority),
            }
        } else {
            Reply::RecoveryScope {
                scope,
                activity,
                authority: Box::new(authority),
            }
        })
    }
    async fn authority(
        &self,
        user: UserId,
        project: i64,
        scope: &openwebide_core::workspace_transaction::WorkspaceTransactionScope,
    ) -> Result<RecoveryAuthority, String> {
        let key = self
            .store
            .get_setting("http_test_issuer")
            .await
            .map_err(|e| e.to_string())?
            .unwrap();
        let binding = RecoveryAuthority::binding_for_scope(user.get(), project, scope)?;
        RecoveryAuthority::for_scope(
            user.get(),
            project,
            scope,
            openwebide_auth::derive_secret(key.as_bytes(), RECOVERY_AUTHORITY_DOMAIN, &binding),
        )
    }
}
impl JournalBackend for Backend {
    async fn request(&self, request: &WorkspaceJournalRequest) -> Result<Reply, String> {
        if let Operation::Command { operation } = &request.operation {
            if self.reject_settlement(operation) {
                return Err("Injected initial settlement transport failure".into());
            }
            self.command_claim_gate(operation, false).await;
        }
        // Store's target-neutral transaction future is synchronous SQLite here.
        // Keep it off the networking future's lifetime parameters.
        let result = futures::executor::block_on(async {
            let user = UserId::new(request.user);
            let project = request.project;
            let id = &request.id;
            let now = self.now.load(Ordering::SeqCst);
            if matches!(request.operation, Operation::ClaimWorker { .. })
                && self.reject_file_admission.load(Ordering::SeqCst)
            {
                return Err("Injected failure before first file admission commits".into());
            }
            Ok(match &request.operation {
                Operation::RecoveryScope => {
                    return self.private_reply(user, project, id, false).await;
                }
                Operation::CommandRecoveryScope => {
                    return self.private_reply(user, project, id, true).await;
                }
                Operation::Scope => Reply::Scope {
                    scope: self
                        .store
                        .workspace_transaction_scope(user, project, id)
                        .await
                        .map_err(|e| e.to_string())?,
                },
                Operation::Load => Reply::Loaded {
                    state: self
                        .store
                        .workspace_transaction(user, project, Some(id))
                        .await
                        .map_err(|e| e.to_string())?
                        .map(Box::new),
                },
                Operation::ClaimWorker {
                    command,
                    scope,
                    worker,
                    review_digest,
                    language_grant,
                } => Reply::WorkerClaimed {
                    scope: self
                        .store
                        .claim_workspace_transaction_worker_compound(
                            user,
                            project,
                            scope,
                            worker,
                            (
                                review_digest.as_deref().zip(language_grant.as_deref()),
                                command.as_deref(),
                            ),
                            now,
                        )
                        .await
                        .map_err(|e| e.to_string())?,
                },
                Operation::ReleaseWorker { generation, worker } => {
                    self.store
                        .release_workspace_transaction_worker(
                            user, project, id, generation, worker, now,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    Reply::WorkerReleased {
                        generation: generation.clone(),
                        worker: worker.id.clone(),
                    }
                }
                Operation::AdvanceOwned {
                    worker,
                    generation,
                    revision,
                    progress,
                } => Reply::Advanced {
                    response: self
                        .store
                        .advance_workspace_transaction_for_worker(
                            user,
                            project,
                            &AdvanceWorkspaceTransaction {
                                id: id.clone(),
                                generation: generation.clone(),
                                revision: *revision,
                                progress: progress.clone(),
                            },
                            Some(worker),
                            now,
                        )
                        .await
                        .map_err(|e| e.to_string())?,
                },
                Operation::Command { operation } => Reply::CommandScope {
                    recovery: Some(Box::new(
                        self.store
                            .native_language_command(user, project, id, operation, now)
                            .await
                            .map_err(|e| e.to_string())?,
                    )),
                },
                Operation::Advance { .. } => return Err("Owned worker required".into()),
            })
        });
        if result.is_ok()
            && let Operation::Command { operation } = &request.operation
        {
            self.command_claim_gate(operation, true).await;
        }
        result
    }
}
pub struct Server {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub async fn serve(backend: Arc<Backend>) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    serve_on(backend, listener)
}
pub fn serve_on(backend: Arc<Backend>, listener: tokio::net::TcpListener) -> Server {
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
                            let supported = matches!(
                                path.as_str(),
                                "/api/workspace-transactions/service"
                                    | "/api/language-commands/service"
                                    | "/api/workspace-transactions/delegated"
                                    | "/api/language-commands/delegated"
                                    | "/api/language-commands/original"
                                    | "/api/language-commands/original-private"
                            );
                            if !supported {
                                return Ok::<_, std::io::Error>(
                                    hyper::Response::builder()
                                        .status(404)
                                        .body(http_body_util::Full::new(bytes::Bytes::from_static(
                                            b"{}",
                                        )))
                                        .unwrap(),
                                );
                            }
                            for header in ["origin", "cookie"] {
                                assert!(!request.headers().contains_key(header));
                            }
                            if path.ends_with("/service") {
                                assert_eq!(
                                    request.headers()["authorization"],
                                    "Bearer dummy_secret"
                                );
                            } else {
                                assert!(!request.headers().contains_key("authorization"));
                            }
                            let bytes = request.into_body().collect().await.unwrap().to_bytes();
                            let private = if path == "/api/language-commands/original-private" {
                                let message: CommandRecoveryMessage =
                                    serde_json::from_slice(&bytes).unwrap();
                                let authority = backend
                                    .authority(
                                        UserId::new(message.user),
                                        message.project,
                                        &message.scope,
                                    )
                                    .await
                                    .unwrap();
                                Some((message, authority))
                            } else {
                                None
                            };
                            let bytes = if let Some((message, authority)) = &private {
                                let Ok(plaintext) = openwebide_auth::private_messages::open(
                                    authority.secret.as_bytes(),
                                    COMMAND_RECOVERY_REQUEST_DOMAIN,
                                    &message.context().unwrap(),
                                    &message.nonce,
                                    &message.ciphertext,
                                    COMMAND_RECOVERY_PLAINTEXT_LIMIT,
                                ) else {
                                    return Ok(hyper::Response::builder()
                                        .status(401)
                                        .body(http_body_util::Full::new(bytes::Bytes::from_static(
                                            b"{}",
                                        )))
                                        .unwrap());
                                };
                                plaintext
                            } else {
                                bytes.to_vec()
                            };
                            let result: Result<Reply, String> = if path
                                == "/api/workspace-transactions/delegated"
                            {
                                let input: DelegatedWorkspaceJournalRequest =
                                    serde_json::from_slice(&bytes).unwrap();
                                if matches!(
                                    input.operation,
                                    DelegatedWorkspaceJournalOperation::Admit { .. }
                                ) && backend.reject_file_admission.load(Ordering::SeqCst)
                                {
                                    let body=serde_json::to_string(&serde_json::json!({"error":"Injected failure before first file admission commits"})).unwrap();
                                    return Ok::<_, std::io::Error>(
                                        hyper::Response::builder()
                                            .status(400)
                                            .body(http_body_util::Full::new(bytes::Bytes::from(
                                                body,
                                            )))
                                            .unwrap(),
                                    );
                                }
                                let reply = futures::executor::block_on(
                                    backend.store.delegated_workspace_journal(
                                        &input,
                                        backend.now.load(Ordering::SeqCst),
                                    ),
                                )
                                .map_err(|e| e.to_string());
                                if reply.is_ok() && matches!(input.operation,DelegatedWorkspaceJournalOperation::RecoveryScope|DelegatedWorkspaceJournalOperation::CommandRecoveryScope) {futures::executor::block_on(backend.private_reply(UserId::new(input.delegation.user),input.delegation.project,&input.delegation.scope.id,matches!(input.operation,DelegatedWorkspaceJournalOperation::CommandRecoveryScope)))} else {reply}
                            } else if matches!(
                                path.as_str(),
                                "/api/language-commands/original"
                                    | "/api/language-commands/original-private"
                            ) {
                                let input: OriginalCommandRequest =
                                    serde_json::from_slice(&bytes).unwrap();
                                let scope = matches!(
                                    input.operation,
                                    CommandJournalOperation::Scope
                                )
                                .then(|| {
                                    backend.original_scope_calls.fetch_add(1, Ordering::SeqCst) + 1
                                });
                                let reply =
                                    futures::executor::block_on(backend.original_reply(&input));
                                if reply.is_ok()
                                    && matches!(
                                        input.operation,
                                        CommandJournalOperation::AcknowledgeSettlement { .. }
                                    )
                                    && backend
                                        .drop_settlement_metadata_ack
                                        .swap(false, Ordering::SeqCst)
                                {
                                    backend.original_unavailable.store(true, Ordering::SeqCst);
                                    backend
                                        .lost_settlement_metadata_acks
                                        .fetch_add(1, Ordering::SeqCst);
                                    return Err(std::io::Error::other(
                                        "Lost original settlement metadata acknowledgement",
                                    ));
                                }
                                if reply.is_ok()
                                    && scope.is_some_and(|index| {
                                        index == backend.block_scope_at.load(Ordering::SeqCst)
                                    })
                                {
                                    // Hold a real original-scope reply, after Store
                                    // observation and outside its transaction.
                                    backend.scope_blocked.store(true, Ordering::SeqCst);
                                    let _gate = ScopeGate(&backend);
                                    backend.release_scope.acquire().await.unwrap().forget();
                                    backend.scope_released.store(true, Ordering::SeqCst);
                                }
                                if reply.is_ok()
                                    && matches!(
                                        input.operation,
                                        CommandJournalOperation::Outcome { .. }
                                    )
                                    && backend.drop_outcome_reply.swap(false, Ordering::SeqCst)
                                {
                                    backend.original_unavailable.store(true, Ordering::SeqCst);
                                    backend.lost_outcome_replies.fetch_add(1, Ordering::SeqCst);
                                    return Err(std::io::Error::other(
                                        "Lost original retained outcome reply",
                                    ));
                                }
                                if reply.is_ok()
                                    && matches!(&input.operation, CommandJournalOperation::Worker { operation } if matches!(**operation, openwebide_core::plugins::languages::intelligence::actions::CommandWorkerOperation::ConfirmCancellation { .. }))
                                    && backend.drop_cancellation_ack.swap(false, Ordering::SeqCst)
                                {
                                    backend.original_unavailable.store(true, Ordering::SeqCst);
                                    backend
                                        .lost_cancellation_acks
                                        .fetch_add(1, Ordering::SeqCst);
                                    return Err(std::io::Error::other(
                                        "Lost original cancellation acknowledgement",
                                    ));
                                }
                                if reply.is_ok()
                                    && let CommandJournalOperation::Worker { operation } =
                                        &input.operation
                                {
                                    backend.settlement_reply(operation)?;
                                }
                                reply
                            } else if path == "/api/language-commands/delegated" {
                                let input: DelegatedCommandRequest =
                                    serde_json::from_slice(&bytes).unwrap();
                                if let CommandJournalOperation::Worker { operation } =
                                    &input.operation
                                {
                                    if backend.reject_settlement(operation) {
                                        return Ok(hyper::Response::builder()
                                            .status(409)
                                            .body(http_body_util::Full::new(
                                                bytes::Bytes::from_static(b"{}"),
                                            ))
                                            .unwrap());
                                    }
                                    backend.command_claim_gate(operation, false).await;
                                }
                                let reply = futures::executor::block_on(
                                    backend.store.delegated_language_command(
                                        &input,
                                        backend.now.load(Ordering::SeqCst),
                                    ),
                                )
                                .map_err(|e| e.to_string());
                                if reply.is_ok()
                                    && let CommandJournalOperation::Worker { operation } =
                                        &input.operation
                                {
                                    backend.command_claim_gate(operation, true).await;
                                    backend.settlement_reply(operation)?;
                                }
                                if reply.is_ok()
                                    && matches!(input.operation, CommandJournalOperation::Scope)
                                {
                                    futures::executor::block_on(backend.private_reply(
                                        UserId::new(input.delegation.user),
                                        input.delegation.project,
                                        &input.delegation.scope.id,
                                        true,
                                    ))
                                } else {
                                    reply
                                }
                            } else {
                                let input: WorkspaceJournalRequest =
                                    serde_json::from_slice(&bytes).unwrap();
                                assert_eq!(
                                    path == "/api/language-commands/service",
                                    matches!(input.operation, Operation::Command { .. })
                                );
                                let reply = backend.request(&input).await;
                                if reply.is_ok()
                                    && let Operation::Command { operation } = &input.operation
                                {
                                    backend.settlement_reply(operation)?;
                                }
                                reply
                            };
                            let (status, body) = match result {
                                Ok(reply) => {
                                    let body = serde_json::to_vec(&reply).unwrap();
                                    let body = if let Some((message, authority)) = &private {
                                        let (nonce, ciphertext) =
                                            openwebide_auth::private_messages::seal(
                                                authority.secret.as_bytes(),
                                                COMMAND_RECOVERY_REPLY_DOMAIN,
                                                &message.reply_context().unwrap(),
                                                &body,
                                            )
                                            .unwrap();
                                        serde_json::to_vec(&CommandRecoveryReply {
                                            nonce,
                                            ciphertext,
                                        })
                                        .unwrap()
                                    } else {
                                        body
                                    };
                                    if private.is_some() {
                                        *backend.last_private_reply.lock().unwrap() =
                                            Some(body.clone());
                                    }
                                    (200, body)
                                }
                                Err(error) => (
                                    409,
                                    serde_json::to_vec(&serde_json::json!({"error":error}))
                                        .unwrap(),
                                ),
                            };
                            Ok(hyper::Response::builder()
                                .status(status)
                                .body(http_body_util::Full::new(bytes::Bytes::from(body)))
                                .unwrap())
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
