use super::*;
use openwebide_core::plugins::languages::intelligence::{
    Query, Request,
    actions::{
        ActionJournalRequest, ActionPhase, ActionRecord, ActionReview, CommandDispatch,
        CommandOperation, CommandProgress, ServerCommand, origin_fingerprint,
    },
    results::WorkspaceEdit,
};

#[derive(Clone)]
struct ActionHost {
    host: Host,
    record: Arc<Mutex<Option<ActionRecord>>>,
    commands: Arc<Mutex<Vec<CommandDispatch>>>,
    lose_claim_reply: bool,
}
impl PluginActionHost for ActionHost {
    async fn language_server(
        &self,
        call: &server_protocol::Call,
    ) -> Result<server_protocol::Response, String> {
        self.host.language_server(call).await
    }
    async fn prepare(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.host.prepare(plugin).await
    }
    async fn grant(&self, plugin: &PreparedPlugin) -> Result<String, String> {
        self.host.grant(plugin).await
    }
    fn is_current(&self) -> bool {
        self.host.is_current()
    }
    async fn language_action_request(
        &self,
        request: &ActionJournalRequest,
    ) -> Result<Option<ActionRecord>, String> {
        let mut record = self.record.lock().unwrap();
        match request {
            ActionJournalRequest::Approve { call, approval } => {
                assert_eq!(call.input, approval.input);
                assert_eq!(call.grant, "authority");
                *record = Some(ActionRecord {
                    approval: approval.as_ref().clone(),
                    revision: 1,
                    phase: ActionPhase::Approved,
                    outcome: None,
                    acknowledgement: None,
                });
            }
            ActionJournalRequest::Claim { call, .. } => {
                assert_eq!(call.grant, "authority");
                *record = Some(record.clone().unwrap().claim()?);
                if self.lose_claim_reply {
                    return Err("Claim response lost".into());
                }
            }
            ActionJournalRequest::Load { .. } => {}
            ActionJournalRequest::Cancel { .. } => {
                *record = Some(record.clone().unwrap().cancel()?);
            }
            _ => panic!("Unexpected recovery write"),
        }
        Ok(record.clone())
    }
    async fn language_command(&self, request: &CommandDispatch) -> Result<CommandProgress, String> {
        self.commands.lock().unwrap().push(request.clone());
        if request.operation == CommandOperation::Start {
            assert_eq!(request.call.as_ref().unwrap().grant, "authority");
            Err("Dispatch response lost".into())
        } else {
            assert!(request.call.is_none());
            Ok(CommandProgress::Running)
        }
    }
}
impl GrantedHost for ActionHost {
    async fn request(&self, request: &PluginHostRequest) -> Result<String, String> {
        self.host.request(request).await
    }
}
impl PluginTransport for ActionHost {
    async fn ensure_prepared(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.host.ensure_prepared(plugin).await
    }
    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        self.host.start(call).await
    }
    async fn resume(&self, call: ContinuePlugin) -> Result<PluginInvocation, String> {
        self.host.resume(call).await
    }
    fn cancel(&self, id: String) {
        self.host.cancel(id);
    }
}

#[test]
fn action_client_does_not_retry_lost_claims_or_dispatch_and_keeps_read_only_recovery_available() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            for lose_claim_reply in [false, true] {
                let (host, mut bindings, _, _) = setup(profile);
                let plugin = &mut bindings[0].prepared;
                plugin.manifest.compatibility.plugin_api = 9;
                plugin.manifest.executable = None;
                plugin.manifest.contributions.tools.clear();
                plugin.manifest.contributions.skills.clear();
                plugin.manifest.contributions.languages=serde_json::from_value(serde_json::json!([{"id":"custom","name":"Custom","extensions":["custom"],"features":{"languageServer":{"command":"custom-lsp"}}}])).unwrap();
                let provider = LanguageRegistry::from_bindings(&bindings)
                    .providers
                    .remove(0);
                let adapter = ActionHost {
                    host: host.clone(),
                    record: Arc::default(),
                    commands: Arc::default(),
                    lose_claim_reply,
                };
                let client = LanguageActionClient::new(
                    adapter.clone(),
                    bindings.clone(),
                    LanguagePreferences::default(),
                )
                .unwrap();
                let input = Request {
                    version: server_protocol::VERSION,
                    client: "client".into(),
                    document: "document".into(),
                    request: 1,
                    revision: 1,
                    language: "custom".into(),
                    path: "file.custom".into(),
                    source: "文😀\r\n".into(),
                    source_digest: source_digest("文😀\r\n"),
                    query: Query::ReviewAction { id: "a".repeat(32) },
                };
                let review = ActionReview {
                    title: "Review command".into(),
                    edit: WorkspaceEdit {
                        files: vec![],
                        changes: vec![],
                    },
                    command: Some(ServerCommand {
                        id: "a".repeat(32),
                        server: "original-service".into(),
                        name: "custom.apply".into(),
                        arguments: vec![],
                    }),
                };
                let approval = client
                    .prepare("b".repeat(32), &provider.namespace, input, review)
                    .await
                    .unwrap();
                assert_eq!(host.grants.load(Ordering::Relaxed), 0);
                assert!(adapter.record.lock().unwrap().is_none());
                let approved = client.approve(&approval).await.unwrap();
                assert_eq!(host.grants.load(Ordering::Relaxed), 1);
                let result = client.claim(&approved).await;
                assert_eq!(result.is_err(), lose_claim_reply);
                let record = client
                    .request(&ActionJournalRequest::Load {
                        id: Some(approval.id.clone()),
                    })
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(record.phase, ActionPhase::Running);
                assert_eq!(record.revision, 2);
                assert_eq!(host.grants.load(Ordering::Relaxed), 2);
                assert!(adapter.commands.lock().unwrap().is_empty());
                if !lose_claim_reply {
                    assert!(
                        client
                            .command(&record, CommandOperation::Start)
                            .await
                            .is_err()
                    );
                    assert_eq!(adapter.commands.lock().unwrap().len(), 1);
                    assert_eq!(host.grants.load(Ordering::Relaxed), 3);
                }
                host.deny_grant.store(true, Ordering::Relaxed);
                bindings[0].enabled = false;
                let recovery = LanguageActionClient::new(
                    adapter.clone(),
                    bindings,
                    LanguagePreferences::default(),
                )
                .unwrap();
                let grants = host.grants.load(Ordering::Relaxed);
                assert!(
                    recovery
                        .command(&record, CommandOperation::Start)
                        .await
                        .is_err()
                );
                for operation in [CommandOperation::Status, CommandOperation::Stop] {
                    assert_eq!(
                        recovery.command(&record, operation).await.unwrap(),
                        CommandProgress::Running
                    );
                }
                assert_eq!(host.grants.load(Ordering::Relaxed), grants);
                assert!(
                    adapter
                        .commands
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|request| request.operation != CommandOperation::Start)
                        .all(|request| request.call.is_none())
                );
                host.current.store(false, Ordering::Relaxed);
                let requests = adapter.commands.lock().unwrap().len();
                assert!(
                    recovery
                        .command(&record, CommandOperation::Stop)
                        .await
                        .is_err()
                );
                assert_eq!(adapter.commands.lock().unwrap().len(), requests);
            }
        }
    });
}

#[test]
fn action_synchronization_keeps_the_origin_identity_and_closes_a_removed_origin() {
    futures::executor::block_on(async {
        use openwebide_core::{
            WorkspaceMode,
            editor::{
                Document, EditorRecovery, EditorRecoveryFile, EditorRecoveryRoot, RecoveryScroll,
            },
        };
        let (host, mut bindings, _, _) = setup("server");
        let plugin = &mut bindings[0].prepared;
        plugin.manifest.compatibility.plugin_api = 9;
        plugin.manifest.executable = None;
        plugin.manifest.contributions.tools.clear();
        plugin.manifest.contributions.skills.clear();
        plugin.manifest.contributions.languages=serde_json::from_value(serde_json::json!([{"id":"custom","name":"Custom","extensions":["custom"],"features":{"languageServer":{"command":"custom-lsp"}}}])).unwrap();
        let provider = LanguageRegistry::from_bindings(&bindings)
            .providers
            .remove(0);
        let client = LanguageActionClient::new(
            host.clone(),
            bindings.clone(),
            LanguagePreferences::default(),
        )
        .unwrap();
        let input = Request {
            version: server_protocol::VERSION,
            client: "client".into(),
            document: "opaque-origin".into(),
            request: 1,
            revision: 1,
            language: "custom".into(),
            path: "file.custom".into(),
            source: "文😀\r\n".into(),
            source_digest: source_digest("文😀\r\n"),
            query: Query::ReviewAction { id: "a".repeat(32) },
        };
        let review = ActionReview {
            title: "Command".into(),
            edit: WorkspaceEdit {
                files: vec![],
                changes: vec![],
            },
            command: Some(ServerCommand {
                id: "a".repeat(32),
                server: "test-native-service".into(),
                name: "custom.command".into(),
                arguments: vec![],
            }),
        };
        let approval = client
            .prepare("b".repeat(32), &provider.namespace, input, review)
            .await
            .unwrap();
        let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
        let mut changed_installation = approval.clone();
        changed_installation.binding_revision += 1;
        assert!(
            service
                .prepare_action_synchronization(&changed_installation, 4)
                .await
                .is_err()
        );
        assert!(host.server_calls.lock().unwrap().is_empty());
        service
            .prepare_action_synchronization(&approval, 4)
            .await
            .unwrap();
        {
            let calls = host.server_calls.lock().unwrap();
            let call = calls.last().unwrap();
            assert_eq!(call.input.source, approval.input.source);
            assert_eq!(call.input.document, approval.input.document);
            assert!(
                matches!(&call.input.query,Query::PrepareAction {action} if action.origin==origin_fingerprint(&approval.input).unwrap())
            );
        }
        let mut recovery = EditorRecovery {
            format: 1,
            root: Some(EditorRecoveryRoot {
                mode: WorkspaceMode::Remote,
                path: Some("project".into()),
            }),
            files: vec![EditorRecoveryFile {
                path: "file.custom".into(),
                document: Some(Document::new("after 文😀\r\n").recovery()),
                scroll: RecoveryScroll::default(),
                read_only: false,
            }],
            selected: Some("file.custom".into()),
        };
        let history = IntelligenceDocuments::default();
        service
            .synchronize_action(1, &approval, &recovery, 2, 4, &history)
            .await
            .unwrap();
        {
            let calls = host.server_calls.lock().unwrap();
            let active = calls.last().unwrap();
            assert_eq!(active.input.document, "opaque-origin");
            assert_eq!(active.input.source, "after 文😀\r\n");
            assert_eq!(active.input.revision, 2);
            assert!(
                matches!(&active.input.query, Query::SynchronizeAction {action,close:false}
                if action.origin==origin_fingerprint(&approval.input).unwrap() && action.review==approval.review.fingerprint().unwrap()
                && action.server==approval.review.command.as_ref().unwrap().server)
            );
        }
        recovery.files.clear();
        recovery.selected = None;
        service
            .synchronize_action(1, &approval, &recovery, 3, 4, &history)
            .await
            .unwrap();
        let calls = host.server_calls.lock().unwrap();
        let closed = calls.last().unwrap();
        assert_eq!(closed.input.document, "opaque-origin");
        assert!(
            matches!(&closed.input.query, Query::SynchronizeAction {action,close:true} if action.command==approval.review.command.as_ref().unwrap().id)
        );
        assert_eq!(host.starts.load(Ordering::Relaxed), 0);
    });
}

#[test]
fn durable_discard_releases_only_its_action_and_cleanup_failure_preserves_cancellation_on_both_hosts()
 {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            for authority_lost in [false, true] {
                let (host, mut bindings, _, _) = setup(profile);
                let plugin = &mut bindings[0].prepared;
                plugin.manifest.compatibility.plugin_api = 9;
                plugin.manifest.executable = None;
                plugin.manifest.contributions.tools.clear();
                plugin.manifest.contributions.skills.clear();
                plugin.manifest.contributions.languages=serde_json::from_value(serde_json::json!([{"id":"custom","name":"Custom","extensions":["custom"],"features":{"languageServer":{"command":"custom-lsp"}}}])).unwrap();
                let provider = LanguageRegistry::from_bindings(&bindings)
                    .providers
                    .remove(0);
                let adapter = ActionHost {
                    host: host.clone(),
                    record: Arc::default(),
                    commands: Arc::default(),
                    lose_claim_reply: false,
                };
                let client = LanguageActionClient::new(
                    adapter.clone(),
                    bindings,
                    LanguagePreferences::default(),
                )
                .unwrap();
                let input = Request {
                    version: server_protocol::VERSION,
                    client: "client".into(),
                    document: "origin".into(),
                    request: 400,
                    revision: 300,
                    language: "custom".into(),
                    path: "moved.custom".into(),
                    source: "draft".into(),
                    source_digest: source_digest("draft"),
                    query: Query::ReviewAction { id: "a".repeat(32) },
                };
                let review = ActionReview {
                    title: "Command".into(),
                    edit: WorkspaceEdit {
                        files: vec![],
                        changes: vec![],
                    },
                    command: Some(ServerCommand {
                        id: "a".repeat(32),
                        server: "test-native-service".into(),
                        name: "custom.command".into(),
                        arguments: vec![],
                    }),
                };
                let approval = client
                    .prepare("b".repeat(32), &provider.namespace, input, review)
                    .await
                    .unwrap();
                let approved = client.approve(&approval).await.unwrap();
                host.deny_grant.store(authority_lost, Ordering::Relaxed);
                let cancelled = client
                    .request(&ActionJournalRequest::Cancel {
                        id: approval.id.clone(),
                        revision: approved.revision,
                    })
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(cancelled.phase, ActionPhase::Cancelled);
                let calls = host.server_calls.lock().unwrap();
                if authority_lost {
                    assert!(calls.is_empty());
                } else {
                    assert_eq!(calls.len(), 1);
                    let call = &calls[0];
                    assert_ne!(call.input.document, approval.input.document);
                    assert_eq!(call.input.revision, 0);
                    assert!(call.input.source.is_empty());
                    assert!(
                        matches!(&call.input.query,Query::ReleaseAction {action} if action.command==approval.review.command.as_ref().unwrap().id && action.origin==origin_fingerprint(&approval.input).unwrap())
                    );
                }
                assert!(adapter.commands.lock().unwrap().is_empty());
            }
        }
    });
}
