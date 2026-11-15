use super::*;
use crate::folder_coordination::{Coordinator, Folder};
use openwebide_core::{
    WorkspaceMode,
    workspace_transaction::{DelegateWorkspaceTransaction, TransactionPhase},
};

#[tokio::test]
async fn private_recovery_scope_uses_paired_proof_and_sanitizes_bounded_responses() {
    use crate::workspace_transactions::JournalBackend;
    use openwebide_core::{
        editor::EditorRecoveryRoot,
        workspace_transaction::{WorkspaceJournalOperation, WorkspaceJournalRequest},
    };
    for padding in [0, RECOVERY_SCOPE_BODY_LIMIT] {
        let scope = WorkspaceTransactionScope {
            id: "a".repeat(32),
            generation: "b".repeat(32),
            revision: 1,
            root_identity: "c".repeat(64),
            root: EditorRecoveryRoot {
                mode: WorkspaceMode::Local,
                path: None,
            },
            worker: None,
            accepted_language_review_digest: None,
        };
        let delegation = WorkspaceJournalDelegation {
            id: "d".repeat(32),
            secret: "e".repeat(32),
            user: 7,
            project: 11,
            scope: scope.clone(),
        };
        let worker = WorkspaceTransactionWorker {
            id: "f".repeat(32),
            secret: "0".repeat(32),
        };
        let body =
            r#"{"result":"recovery_scope","scope":"private-disclosure-marker","authority":null}"#
                .to_string()
                + &" ".repeat(padding);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (url, captured) =
            crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str())).await;
        let client = PairedJournal::new(
            format!("{url}/api"),
            &scope.root_identity,
            delegation.clone(),
            worker.clone(),
        )
        .unwrap();
        let error = JournalBackend::request(
            &client,
            &WorkspaceJournalRequest {
                user: 7,
                project: 11,
                id: scope.id,
                operation: WorkspaceJournalOperation::RecoveryScope,
            },
        )
        .await
        .unwrap_err();
        assert!(!error.contains("private-disclosure-marker"));
        if padding == 0 {
            assert_eq!(error, "Invalid paired workspace response");
        } else {
            assert!(error.contains("exceeded its limit"));
        }
        let request = captured.await.unwrap();
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        let input: DelegatedWorkspaceJournalRequest =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(input.delegation, delegation);
        assert_eq!(input.worker.id, worker.id);
        assert_eq!(input.worker.secret, worker.secret);
        assert!(matches!(input.operation, Operation::RecoveryScope));
    }
}

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

#[tokio::test]
async fn compound_recovery_scope_activity_uses_the_same_contract_in_both_transports() {
    use crate::workspace_transactions::JournalBackend;
    use openwebide_core::{
        editor::EditorRecoveryRoot,
        workspace_transaction::{
            RecoveryAuthority, WorkspaceActivity, WorkspaceJournalOperation,
            WorkspaceJournalRequest,
        },
    };
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for activity in [
            WorkspaceActivity {
                file_recovery: true,
                language_command: false,
            },
            WorkspaceActivity {
                file_recovery: false,
                language_command: true,
            },
            WorkspaceActivity::default(),
        ] {
            for complete in [true, false] {
                let scope = WorkspaceTransactionScope {
                    id: "a".repeat(32),
                    generation: "b".repeat(32),
                    revision: 1,
                    root_identity: "c".repeat(64),
                    root: EditorRecoveryRoot {
                        mode,
                        path: (mode == WorkspaceMode::Remote).then(|| "original".into()),
                    },
                    worker: None,
                    accepted_language_review_digest: None,
                };
                let authority =
                    RecoveryAuthority::for_scope(7, 11, &scope, "A".repeat(43)).unwrap();
                let secret = authority.secret.clone();
                let mut body = serde_json::to_value(WorkspaceJournalResponse::RecoveryScope {
                    scope: scope.clone(),
                    authority: Box::new(authority.clone()),
                    activity,
                })
                .unwrap();
                if !complete {
                    body.as_object_mut().unwrap().remove("activity");
                }
                let body = serde_json::to_string(&body).unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let (url, captured) =
                    crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str()))
                        .await;
                let input = WorkspaceJournalRequest {
                    user: 7,
                    project: 11,
                    id: scope.id.clone(),
                    operation: WorkspaceJournalOperation::RecoveryScope,
                };
                let result = if mode == WorkspaceMode::Remote {
                    let client = crate::runs::backend_client::BackendClient::new(
                        format!("{url}/api"),
                        "service-secret".into(),
                        crate::runs::http_client::ReqwestHttpClient::default(),
                    );
                    JournalBackend::request(&client, &input).await
                } else {
                    let delegation = WorkspaceJournalDelegation {
                        id: "d".repeat(32),
                        secret: "e".repeat(32),
                        user: 7,
                        project: 11,
                        scope: scope.clone(),
                    };
                    let worker = WorkspaceTransactionWorker {
                        id: "f".repeat(32),
                        secret: "0".repeat(32),
                    };
                    let client = PairedJournal::new(
                        format!("{url}/api"),
                        &scope.root_identity,
                        delegation,
                        worker,
                    )
                    .unwrap();
                    JournalBackend::request(&client, &input).await
                };
                if complete {
                    let WorkspaceJournalResponse::RecoveryScope {
                        scope: current,
                        authority: returned,
                        activity: observed,
                    } = result.unwrap()
                    else {
                        panic!("Missing original recovery response")
                    };
                    assert_eq!(current, scope);
                    assert_eq!(*returned, authority);
                    assert_eq!(observed, activity);
                    assert_eq!(
                        observed.ensure_idle().is_ok(),
                        !activity.file_recovery && !activity.language_command
                    );
                } else {
                    let error = result.unwrap_err();
                    assert!(!error.contains(&secret));
                    assert!(error.contains("Invalid"));
                }
                let request = captured.await.unwrap();
                assert!(request.contains("recovery_scope"));
                assert!(
                    !request.contains(&secret),
                    "Never reflect an earlier private credential to the issuer"
                );
                if mode == WorkspaceMode::Local {
                    assert!(!request.to_ascii_lowercase().contains("authorization:"));
                }
            }
        }
    }
}

#[tokio::test]
async fn private_command_recovery_has_the_same_bounded_identity_contract_in_both_transports() {
    use crate::{
        runs::{backend_client::BackendClient, http_client::ReqwestHttpClient},
        workspace_transactions::JournalBackend,
    };
    use openwebide_core::{
        editor::EditorRecoveryRoot,
        plugins::languages::intelligence::actions::{
            ActionPhase, CommandInvocation, CommandRecoveryScope,
        },
        workspace_transaction::{
            RecoveryAuthority, WorkspaceActivity, WorkspaceCommandRecovery,
            WorkspaceJournalOperation, WorkspaceJournalRequest,
        },
    };
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for case in 0..10 {
            let scope = WorkspaceTransactionScope {
                id: "a".repeat(32),
                generation: "b".repeat(32),
                revision: 3,
                root_identity: "c".repeat(64),
                root: EditorRecoveryRoot { mode, path: None },
                worker: None,
                accepted_language_review_digest: None,
            };
            let command = CommandRecoveryScope {
                invocation: CommandInvocation {
                    id: scope.id.clone(),
                    client: "window".into(),
                    command: "d".repeat(32),
                    server: "original".into(),
                    origin: "e".repeat(64),
                    review: "f".repeat(64),
                    approval: "0".repeat(64),
                },
                revision: 2,
                phase: ActionPhase::Running,
                outcome_digest: None,
                worker: None,
                native_settlement_pending: false,
                command_cleanup_confirmed: false,
            };
            let recovery = WorkspaceCommandRecovery {
                scope: scope.clone(),
                activity: WorkspaceActivity {
                    file_recovery: false,
                    language_command: true,
                },
                command,
            };
            let authority = RecoveryAuthority::for_scope(7, 11, &scope, "A".repeat(43)).unwrap();
            let mut value = serde_json::to_value(WorkspaceJournalResponse::CommandRecoveryScope {
                recovery: Box::new(recovery),
                authority: Box::new(authority),
            })
            .unwrap();
            match case {
                0 | 9 => {}
                1 => value["authority"]["user"] = serde_json::json!(8),
                2 => value["authority"]["project"] = serde_json::json!(12),
                3 => value["recovery"]["scope"]["id"] = serde_json::json!("1".repeat(32)),
                4 => value["authority"]["generation"] = serde_json::json!("1".repeat(32)),
                5 => {
                    value["recovery"]
                        .as_object_mut()
                        .unwrap()
                        .remove("activity");
                }
                6 => {
                    value["recovery"].as_object_mut().unwrap().remove("command");
                }
                7 => value["recovery"]["command"]["phase"] = serde_json::json!("completed"),
                8 => {
                    value = serde_json::json!({"result":"command_scope","recovery":value["recovery"].clone()});
                }
                _ => unreachable!(),
            }
            let body = serde_json::to_string(&value).unwrap()
                + &" ".repeat(if case == 9 {
                    RECOVERY_SCOPE_BODY_LIMIT
                } else {
                    0
                });
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let (url, captured) =
                crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str()))
                    .await;
            let request = WorkspaceJournalRequest {
                user: 7,
                project: 11,
                id: scope.id.clone(),
                operation: WorkspaceJournalOperation::CommandRecoveryScope,
            };
            let result = if mode == WorkspaceMode::Remote {
                BackendClient::new(
                    format!("{url}/api"),
                    "service-secret".into(),
                    ReqwestHttpClient::default(),
                )
                .workspace_journal_service(&request)
                .await
            } else {
                let client = PairedJournal::new(
                    format!("{url}/api"),
                    &scope.root_identity,
                    WorkspaceJournalDelegation {
                        id: "2".repeat(32),
                        secret: "3".repeat(32),
                        user: 7,
                        project: 11,
                        scope: scope.clone(),
                    },
                    WorkspaceTransactionWorker {
                        id: "4".repeat(32),
                        secret: "5".repeat(32),
                    },
                )
                .unwrap();
                JournalBackend::request(&client, &request).await
            };
            assert_eq!(
                result.is_ok(),
                case == 0,
                "{mode:?} case={case} result={result:?}"
            );
            if let Err(error) = result {
                assert!(!error.contains(&"A".repeat(43)));
                if case == 9 {
                    assert!(error.contains("exceeded its limit"));
                }
            }
            let sent = captured.await.unwrap();
            assert!(sent.contains("\"command\":\"command_recovery_scope\""));
            if mode == WorkspaceMode::Local {
                assert!(!sent.to_ascii_lowercase().contains("authorization:"));
                assert!(!sent.contains("service-secret"));
            }
        }
    }
}
