use super::*;
use crate::plugins::execution::{GrantedHost, PluginTransport};
use openwebide_core::plugins::execution::*;
use openwebide_core::plugins::languages::intelligence as server_protocol;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Clone)]
struct Host {
    current: Arc<AtomicBool>,
    starts: Arc<AtomicUsize>,
    preparations: Arc<AtomicUsize>,
    grants: Arc<AtomicUsize>,
    deny_grant: Arc<AtomicBool>,
    result: Arc<Mutex<String>>,
    change_on_resume: bool,
    host_call: bool,
    prepared_host: Arc<Mutex<Option<String>>>,
    requests: Arc<Mutex<Vec<LanguageInput>>>,
    dynamic: bool,
    needs_source: Arc<AtomicUsize>,
    cancel_on_yield: bool,
    analyses: Arc<Mutex<std::collections::BTreeMap<String, SyntaxAnalysis>>>,
    rejected_packages: Arc<Mutex<std::collections::BTreeSet<String>>>,
    server_calls: Arc<Mutex<Vec<openwebide_core::plugins::languages::intelligence::Call>>>,
    stale_server_result: bool,
}
impl PluginActionHost for Host {
    async fn language_server(
        &self,
        call: &openwebide_core::plugins::languages::intelligence::Call,
    ) -> Result<openwebide_core::plugins::languages::intelligence::Response, String> {
        use openwebide_core::plugins::languages::intelligence;
        assert_eq!(call.grant, "authority");
        self.server_calls.lock().unwrap().push(call.clone());
        if self.change_on_resume {
            self.current.store(false, Ordering::Relaxed);
        }
        Ok(intelligence::Response {
            version: intelligence::VERSION,
            client: call.input.client.clone(),
            document: call.input.document.clone(),
            request: call.input.request,
            revision: call.input.revision + u64::from(self.stale_server_result),
            source_digest: call.input.source_digest.clone(),
            server: "test-native-service".into(),
            capabilities: serde_json::json!({"hoverProvider":true}),
            result: serde_json::json!({"contents":"a type"}),
            diagnostics: vec![],
            diagnostics_ready: false,
        })
    }
    async fn yield_task(&self) {
        if self.cancel_on_yield {
            self.current.store(false, Ordering::Relaxed);
        }
    }
    async fn prepare(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.preparations.fetch_add(1, Ordering::Relaxed);
        let mut prepared = plugin.clone();
        if let Some(host) = &*self.prepared_host.lock().unwrap() {
            prepared.host_id = host.clone();
        }
        Ok(prepared)
    }
    async fn grant(&self, prepared: &PreparedPlugin) -> Result<String, String> {
        self.grants.fetch_add(1, Ordering::Relaxed);
        if self.deny_grant.load(Ordering::Relaxed)
            || self
                .rejected_packages
                .lock()
                .unwrap()
                .contains(&prepared.manifest.name)
        {
            return Err("Plugin is no longer enabled".into());
        }
        Ok("authority".into())
    }
    fn is_current(&self) -> bool {
        self.current.load(Ordering::Relaxed)
    }
}
impl GrantedHost for Host {
    async fn request(&self, _: &PluginHostRequest) -> Result<String, String> {
        panic!("Language services must never request host capabilities")
    }
}
impl PluginTransport for Host {
    async fn ensure_prepared(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        Ok(plugin.clone())
    }
    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        self.starts.fetch_add(1, Ordering::Relaxed);
        assert!(matches!(call.operation, PluginOperation::Language));
        assert_eq!(call.name, SERVICE_HANDLER);
        let input: LanguageInput = serde_json::from_str(&call.arguments).unwrap();
        if self.dynamic {
            let output = match &input {
                LanguageInput::Analyze(request) => {
                    let missing = self
                        .needs_source
                        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                            count.checked_sub(1)
                        })
                        .is_ok();
                    let mut output = analysis_response(
                        request,
                        if missing {
                            AnalysisStatus::NeedsSource
                        } else {
                            AnalysisStatus::Ready { incremental: false }
                        },
                    );
                    if !missing && let LanguageOutput::Analyzed(response) = &mut output {
                        let analyses = self.analyses.lock().unwrap();
                        response.analysis = Some(
                            analyses
                                .get(&format!(
                                    "{}:{}",
                                    call.prepared.manifest.name, request.language
                                ))
                                .or_else(|| analyses.get(&request.language))
                                .cloned()
                                .unwrap_or(SyntaxAnalysis {
                                    highlights: None,
                                    folds: vec![],
                                    structure: None,
                                }),
                        );
                    }
                    output
                }
                LanguageInput::Close { version, document } => LanguageOutput::Closed {
                    version: *version,
                    document: document.clone(),
                },
            };
            *self.result.lock().unwrap() = serde_json::to_string(&output).unwrap();
        }
        self.requests.lock().unwrap().push(input);
        Ok(PluginInvocation {
            id: "language".into(),
            step: PluginStep::Ready,
        })
    }
    async fn resume(&self, _: ContinuePlugin) -> Result<PluginInvocation, String> {
        if self.change_on_resume {
            self.current.store(false, Ordering::Relaxed);
        }
        let step = if self.host_call {
            PluginStep::HostCall {
                sequence: 1,
                capability: "records".into(),
                payload: "{}".into(),
            }
        } else {
            PluginStep::Complete {
                ok: true,
                content: self.result.lock().unwrap().clone(),
                summary: "Analysis".into(),
            }
        };
        Ok(PluginInvocation {
            id: "language".into(),
            step,
        })
    }
    fn cancel(&self, _: String) {}
}
fn setup(profile: &str) -> (Host, Vec<ProjectPlugin>, RegisteredLanguage, LanguageInput) {
    let mut plugin = openwebide_core::plugins::testing::receipt();
    plugin.host_id = profile.into();
    plugin.manifest.compatibility.plugin_api = 5;
    plugin.manifest.executable = Some(openwebide_core::plugins::RustPlugin {
        manifest: "Cargo.toml".into(),
        library: "language".into(),
        sdk_version: "0.1.0".into(),
        capabilities: vec![],
    });
    plugin.manifest.contributions.languages = serde_json::from_value(serde_json::json!([{"id":"custom","name":"Custom","extensions":["custom"],"features":{"syntax":true}}])).unwrap();
    let bindings = vec![ProjectPlugin {
        id: 1,
        revision: 1,
        enabled: true,
        prepared: plugin,
    }];
    let provider = LanguageRegistry::from_bindings(&bindings)
        .providers
        .remove(0);
    let request = AnalysisRequest {
        version: VERSION,
        document: "account/project/client/document".into(),
        request: 1,
        revision: 1,
        language: "custom".into(),
        path: "file.custom".into(),
        source_digest: source_digest("文😀\r\n"),
        source: SourceChange::Full {
            text: "文😀\r\n".into(),
        },
        tab_width: 4,
    };
    let mut output = analysis_response(&request, AnalysisStatus::Ready { incremental: false });
    let LanguageOutput::Analyzed(response) = &mut output else {
        unreachable!()
    };
    response.analysis = Some(SyntaxAnalysis {
        highlights: Some(vec![
            vec![TokenSpan {
                end: 8,
                kind: TokenKind::Keyword,
            }],
            vec![],
        ]),
        folds: vec![],
        structure: None,
    });
    let host = Host {
        current: Arc::new(AtomicBool::new(true)),
        starts: Arc::default(),
        preparations: Arc::default(),
        grants: Arc::default(),
        deny_grant: Arc::default(),
        result: Arc::new(Mutex::new(serde_json::to_string(&output).unwrap())),
        change_on_resume: false,
        host_call: false,
        prepared_host: Arc::default(),
        requests: Arc::default(),
        dynamic: false,
        needs_source: Arc::default(),
        cancel_on_yield: false,
        analyses: Arc::default(),
        rejected_packages: Arc::default(),
        server_calls: Arc::default(),
        stale_server_result: false,
    };
    (host, bindings, provider, LanguageInput::Analyze(request))
}

#[test]
fn native_formatting_honors_disabled_and_explicit_formatter_preferences() {
    futures::executor::block_on(async {
        for disabled in [false, true] {
            let (host, mut bindings, _, _) = setup("server");
            bindings[0].prepared.manifest.compatibility.plugin_api = 6;
            bindings[0].prepared.manifest.contributions.languages[0]
                .features
                .language_server =
                Some(serde_json::from_value(serde_json::json!({"command":"custom-lsp"})).unwrap());
            let mut formatter = bindings[0].clone();
            formatter.id = 2;
            formatter.prepared.source.path = "plugins/formatter".into();
            formatter.prepared.manifest.name = "formatter".into();
            formatter.prepared.digest = "b".repeat(64);
            formatter.prepared.manifest.executable = None;
            formatter.prepared.manifest.contributions.languages[0]
                .features
                .syntax = false;
            let formatter_namespace = formatter.prepared.storage_namespace();
            bindings.push(formatter);
            let registry = LanguageRegistry::from_bindings(&bindings);
            let provider = registry
                .providers
                .iter()
                .find(|provider| provider.plugin_namespace != formatter_namespace)
                .unwrap()
                .clone();
            let formatter = registry
                .providers
                .iter()
                .find(|provider| provider.plugin_namespace == formatter_namespace)
                .unwrap();
            let mut preferences = LanguagePreferences::default();
            preferences.providers.insert(
                "custom".into(),
                ProviderPreference::Provider {
                    namespace: provider.namespace.clone(),
                },
            );
            preferences.formatters.insert(
                "custom".into(),
                if disabled {
                    ProviderPreference::Disabled
                } else {
                    ProviderPreference::Provider {
                        namespace: formatter.namespace.clone(),
                    }
                },
            );
            let service = LanguageService::new_with_preferences(
                host.clone(),
                bindings,
                provider,
                preferences,
            )
            .unwrap();
            let response = service
                .query_intelligence(
                    "client",
                    LanguageDocumentRequest {
                        document: "file".into(),
                        path: "file.custom".into(),
                        revision: 1,
                        source: Arc::new("source".into()),
                        tab_width: 4,
                    },
                    server_protocol::Query::Formatting {
                        tab_width: 4,
                        insert_spaces: true,
                    },
                )
                .await;
            if disabled {
                assert!(response.unwrap_err().contains("disabled"));
                assert!(host.server_calls.lock().unwrap().is_empty());
                assert_eq!(host.grants.load(Ordering::Relaxed), 0);
            } else {
                response.unwrap();
                let calls = host.server_calls.lock().unwrap();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].prepared.storage_namespace(), formatter_namespace);
                assert_eq!(host.starts.load(Ordering::Relaxed), 0);
            }
        }
    });
}

#[test]
fn shared_code_intelligence_checks_fresh_authority_without_component_execution() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let (host, mut bindings, _, _) = setup(profile);
            let manifest = &mut bindings[0].prepared.manifest;
            manifest.compatibility.plugin_api = 6;
            manifest.executable = None;
            manifest.contributions.languages[0].features.syntax = false;
            manifest.contributions.languages[0].features.language_server =
                Some(serde_json::from_value(serde_json::json!({"command":"custom-lsp"})).unwrap());
            let provider = LanguageRegistry::from_bindings(&bindings)
                .providers
                .remove(0);
            let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
            for revision in 1..=2 {
                let response = service
                    .query_intelligence(
                        "client-a",
                        LanguageDocumentRequest {
                            document: "document-a".into(),
                            path: "file.custom".into(),
                            revision,
                            source: Arc::new("文😀\r\n".into()),
                            tab_width: 4,
                        },
                        server_protocol::Query::Hover {
                            position: server_protocol::Position {
                                line: 0,
                                character: 1,
                            },
                        },
                    )
                    .await
                    .unwrap();
                assert_eq!(response.revision, revision);
            }
            assert_eq!(host.preparations.load(Ordering::Relaxed), 1);
            assert_eq!(host.grants.load(Ordering::Relaxed), 2);
            assert_eq!(host.starts.load(Ordering::Relaxed), 0);
            let calls = host.server_calls.lock().unwrap().clone();
            assert_eq!(calls.len(), 2);
            assert!(calls[0].input.request < calls[1].input.request);
            assert_eq!(calls[0].prepared.host_id, profile);
            host.deny_grant.store(true, Ordering::Relaxed);
            assert!(
                service
                    .query_intelligence(
                        "client-a",
                        LanguageDocumentRequest {
                            document: "document-a".into(),
                            path: "file.custom".into(),
                            revision: 3,
                            source: Arc::new("changed".into()),
                            tab_width: 4,
                        },
                        server_protocol::Query::Synchronize
                    )
                    .await
                    .is_err()
            );
            assert_eq!(host.server_calls.lock().unwrap().len(), 2);
        }
    });
}

#[test]
fn stale_native_response_and_changed_client_scope_are_rejected() {
    futures::executor::block_on(async {
        for change_scope in [false, true] {
            let (mut host, mut bindings, _, _) = setup("server");
            host.stale_server_result = !change_scope;
            host.change_on_resume = change_scope;
            let manifest = &mut bindings[0].prepared.manifest;
            manifest.compatibility.plugin_api = 6;
            manifest.contributions.languages[0].features.language_server =
                Some(serde_json::from_value(serde_json::json!({"command":"custom-lsp"})).unwrap());
            let provider = LanguageRegistry::from_bindings(&bindings)
                .providers
                .remove(0);
            let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
            assert!(
                service
                    .query_intelligence(
                        "client-a",
                        LanguageDocumentRequest {
                            document: "document-a".into(),
                            path: "file.custom".into(),
                            revision: 1,
                            source: Arc::new("source".into()),
                            tab_width: 4,
                        },
                        server_protocol::Query::Synchronize
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn both_host_adapters_use_the_same_pinned_provider_and_result_contract() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let (host, bindings, provider, input) = setup(profile);
            let output = invoke_language(&host, &bindings, &provider, &input, Some("文😀\r\n"))
                .await
                .unwrap();
            assert!(
                matches!(output, LanguageOutput::Analyzed(response) if matches!(response.status, AnalysisStatus::Ready { .. }))
            );
            assert_eq!(host.starts.load(Ordering::Relaxed), 1);
            let mut changed = bindings.clone();
            for field in ["enabled", "digest", "revision", "host", "configuration"] {
                match field {
                    "enabled" => changed[0].enabled = false,
                    "digest" => changed[0].prepared.digest = "c".repeat(64),
                    "revision" => changed[0].revision += 1,
                    "host" => changed[0].prepared.host_id = "another-host".into(),
                    _ => {
                        changed[0].prepared.manifest.contributions.languages[0]
                            .configuration
                            .line_comment = Some("#".into());
                    }
                }
                assert!(
                    invoke_language(&host, &changed, &provider, &input, Some("文😀\r\n"))
                        .await
                        .is_err()
                );
                changed = bindings.clone();
            }
            assert_eq!(host.starts.load(Ordering::Relaxed), 1);
        }
    });
}

#[test]
fn changed_context_stale_replies_and_capability_requests_cannot_publish_results() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            for failure in ["context", "revision", "host-call", "source"] {
                let (mut host, bindings, provider, input) = setup(profile);
                match failure {
                    "context" => host.change_on_resume = true,
                    "host-call" => host.host_call = true,
                    "revision" => {
                        let mut output: LanguageOutput =
                            serde_json::from_str(&host.result.lock().unwrap()).unwrap();
                        let LanguageOutput::Analyzed(response) = &mut output else {
                            unreachable!()
                        };
                        response.revision += 1;
                        *host.result.lock().unwrap() = serde_json::to_string(&output).unwrap();
                    }
                    _ => (),
                }
                let source = if failure == "source" {
                    "other"
                } else {
                    "文😀\r\n"
                };
                assert!(
                    invoke_language(&host, &bindings, &provider, &input, Some(source))
                        .await
                        .is_err()
                );
            }
        }
    });
}

#[test]
fn both_hosts_reuse_preparation_validate_each_request_and_back_off_after_failures() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let (host, bindings, provider, input) = setup(profile);
            let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
            let saved = host.result.lock().unwrap().clone();
            for now in [100, 200, 300_101] {
                service
                    .request(&input, Some("文😀\r\n"), now)
                    .await
                    .unwrap();
            }
            assert_eq!(host.preparations.load(Ordering::Relaxed), 1);
            assert_eq!(host.grants.load(Ordering::Relaxed), 3);
            *host.result.lock().unwrap() = "invalid output".into();
            assert!(
                service
                    .request(&input, Some("文😀\r\n"), 300_102)
                    .await
                    .is_err()
            );
            let starts = host.starts.load(Ordering::Relaxed);
            assert!(
                service
                    .request(&input, Some("文😀\r\n"), 300_500)
                    .await
                    .is_err()
            );
            assert_eq!(host.starts.load(Ordering::Relaxed), starts);
            *host.result.lock().unwrap() = saved;
            service
                .request(&input, Some("文😀\r\n"), 301_102)
                .await
                .unwrap();
            assert_eq!(host.preparations.load(Ordering::Relaxed), 2);
            assert_eq!(host.grants.load(Ordering::Relaxed), 5);
            host.current.store(false, Ordering::Relaxed);
            assert!(
                service
                    .request(&input, Some("文😀\r\n"), 301_103)
                    .await
                    .is_err()
            );
            assert_eq!(host.starts.load(Ordering::Relaxed), starts + 1);
        }
    });
}

#[test]
fn preparation_can_move_a_receipt_to_the_project_host_then_pins_that_host() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let (host, bindings, provider, input) = setup(profile);
            *host.prepared_host.lock().unwrap() = Some("selected-execution-host".into());
            let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
            service
                .request(&input, Some("文😀\r\n"), 100)
                .await
                .unwrap();
            assert_eq!(
                service.host.state.lock().unwrap().execution_host.as_deref(),
                Some("selected-execution-host")
            );
            *host.result.lock().unwrap() = "invalid output".into();
            assert!(
                service
                    .request(&input, Some("文😀\r\n"), 101)
                    .await
                    .is_err()
            );
            *host.prepared_host.lock().unwrap() = Some("replacement-host".into());
            let starts = host.starts.load(Ordering::Relaxed);
            assert!(
                service
                    .request(&input, Some("文😀\r\n"), 1101)
                    .await
                    .unwrap_err()
                    .contains("execution host changed")
            );
            assert_eq!(host.starts.load(Ordering::Relaxed), starts);
        }
    });
}

#[test]
fn a_warm_component_cannot_bypass_revoked_authority_on_either_host() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let (host, bindings, provider, input) = setup(profile);
            let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
            service
                .request(&input, Some("文😀\r\n"), 100)
                .await
                .unwrap();
            host.deny_grant.store(true, Ordering::Relaxed);
            assert!(
                service
                    .request(&input, Some("文😀\r\n"), 200)
                    .await
                    .is_err()
            );
            assert_eq!(host.preparations.load(Ordering::Relaxed), 1);
            assert_eq!(host.starts.load(Ordering::Relaxed), 1);
            assert_eq!(host.grants.load(Ordering::Relaxed), 2);
        }
    });
}

fn document(source: &str, revision: u64) -> LanguageDocumentRequest {
    LanguageDocumentRequest {
        document: "client/project/file".into(),
        path: "file.custom".into(),
        revision,
        source: Arc::new(source.into()),
        tab_width: 4,
    }
}

#[test]
fn shared_document_sync_sends_utf8_deltas_one_full_resync_and_releases_closed_bases() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let (mut host, bindings, provider, _) = setup(profile);
            host.dynamic = true;
            let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
            service
                .analyze(document("a文😀\r\n", 1), 100)
                .await
                .unwrap();
            service
                .analyze(document("a字😀\r\n", 2), 200)
                .await
                .unwrap();
            let requests = host.requests.lock().unwrap().clone();
            assert!(
                matches!(&requests[0], LanguageInput::Analyze(request) if matches!(request.source, SourceChange::Full { .. }))
            );
            assert!(
                matches!(&requests[1], LanguageInput::Analyze(request) if request.source == (SourceChange::Replace { base_revision:1, start:1, end:4, text:"字".into() }))
            );
            host.needs_source.store(1, Ordering::Relaxed);
            service
                .analyze(document("a字😀!\r\n", 3), 300)
                .await
                .unwrap();
            let requests = host.requests.lock().unwrap().clone();
            assert!(
                matches!(&requests[2], LanguageInput::Analyze(request) if matches!(request.source, SourceChange::Replace { base_revision:2, .. }))
            );
            assert!(
                matches!(&requests[3], LanguageInput::Analyze(request) if matches!(request.source, SourceChange::Full { .. }))
            );
            service.close("client/project/file", 400).await.unwrap();
            assert!(service.host.state.lock().unwrap().documents.is_empty());
            service
                .analyze(document("a字😀!\r\n", 4), 500)
                .await
                .unwrap();
            assert!(
                matches!(host.requests.lock().unwrap().last(), Some(LanguageInput::Analyze(request)) if matches!(request.source, SourceChange::Full { .. }))
            );
            host.needs_source.store(2, Ordering::Relaxed);
            let before = host.starts.load(Ordering::Relaxed);
            assert!(service.analyze(document("changed", 5), 600).await.is_err());
            assert_eq!(host.starts.load(Ordering::Relaxed), before + 2);
            assert!(
                service
                    .analyze(document("changed again", 6), 700)
                    .await
                    .is_err()
            );
            assert_eq!(host.starts.load(Ordering::Relaxed), before + 2);
        }
    });
}

#[test]
fn document_diff_yields_and_cannot_send_a_request_after_its_owner_changes() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let (mut host, bindings, provider, _) = setup(profile);
            host.dynamic = true;
            host.cancel_on_yield = true;
            let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
            let source = "x".repeat(40_000);
            service.analyze(document(&source, 1), 100).await.unwrap();
            assert!(
                service
                    .analyze(document(&(source + "文😀"), 2), 200)
                    .await
                    .is_err()
            );
            assert_eq!(host.starts.load(Ordering::Relaxed), 1);
        }
    });
}

#[test]
fn embedded_custom_languages_follow_preferences_and_close_both_host_stores() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            for disabled in [false, true] {
                let (mut host, mut bindings, provider, _) = setup(profile);
                host.dynamic = true;
                let mut child = bindings[0].clone();
                child.id = 2;
                child.prepared.manifest.name = "child-language".into();
                child.prepared.manifest.contributions.languages = serde_json::from_value(serde_json::json!([{"id":"embedded", "name":"Embedded", "features":{"syntax":true,"structure":true},"configuration":{"lineComment":"#"}}])).unwrap();
                bindings.push(child);
                let source = "[文😀]";
                let empty_structure = |language: &str| Structure {
                    language: language.into(),
                    scopes: vec![],
                    selections: vec![],
                    opaque_starts: vec![],
                    protected: vec![],
                    brackets: vec![],
                };
                let mut root = SyntaxAnalysis {
                    highlights: Some(vec![vec![TokenSpan {
                        end: source.len(),
                        kind: TokenKind::Keyword,
                    }]]),
                    folds: vec![],
                    structure: Some(empty_structure("custom")),
                };
                root.structure.as_mut().unwrap().scopes.push(LanguageScope {
                    range: TextRange {
                        start: 1,
                        end: source.len() - 1,
                    },
                    language: "embedded".into(),
                });
                let child = SyntaxAnalysis {
                    highlights: Some(vec![vec![TokenSpan {
                        end: "文😀".len(),
                        kind: TokenKind::String,
                    }]]),
                    folds: vec![],
                    structure: Some(empty_structure("embedded")),
                };
                host.analyses.lock().unwrap().insert("custom".into(), root);
                host.analyses
                    .lock()
                    .unwrap()
                    .insert("embedded".into(), child);
                let mut preferences = LanguagePreferences::default();
                if disabled {
                    preferences
                        .providers
                        .insert("embedded".into(), ProviderPreference::Disabled);
                }
                let service = LanguageService::new_with_preferences(
                    host.clone(),
                    bindings,
                    provider,
                    preferences,
                )
                .unwrap();
                assert_eq!(
                    service
                        .definitions()
                        .iter()
                        .any(|definition| definition.id == "embedded"),
                    !disabled
                );
                let output = service.analyze(document(source, 1), 100).await.unwrap();
                let LanguageOutput::Analyzed(response) = output else {
                    unreachable!()
                };
                let data = response.analysis.unwrap();
                validate_analysis(&data, source, "custom").unwrap();
                assert_eq!(
                    data.highlights.unwrap()[0],
                    vec![
                        TokenSpan {
                            end: 1,
                            kind: TokenKind::Keyword
                        },
                        TokenSpan {
                            end: 8,
                            kind: if disabled {
                                TokenKind::Plain
                            } else {
                                TokenKind::String
                            }
                        },
                        TokenSpan {
                            end: 9,
                            kind: TokenKind::Keyword
                        }
                    ]
                );
                let expected = if disabled { 1 } else { 2 };
                assert_eq!(host.preparations.load(Ordering::Relaxed), expected);
                if !disabled {
                    host.analyses
                        .lock()
                        .unwrap()
                        .get_mut("custom")
                        .unwrap()
                        .structure
                        .as_mut()
                        .unwrap()
                        .scopes
                        .clear();
                    service.analyze(document(source, 2), 150).await.unwrap();
                    assert!(host.requests.lock().unwrap().iter().any(|input| matches!(input, LanguageInput::Close { document, .. } if document.starts_with("embedded:"))), "Removed bodies must close before their parent document");
                    assert!(
                        service
                            .embedded_documents
                            .lock()
                            .unwrap()
                            .values()
                            .all(|entry| entry.documents.is_empty())
                    );
                }
                service.close("client/project/file", 200).await.unwrap();
                let requests = host.requests.lock().unwrap();
                assert_eq!(
                    requests
                        .iter()
                        .filter(|input| matches!(input, LanguageInput::Close { .. }))
                        .count(),
                    expected
                );
                assert_eq!(service.embedded_documents.lock().unwrap().len(), 0);
            }
        }
    });
}

#[test]
fn embedding_conflicts_replacement_aliases_and_failed_authority_preserve_plain_fallback() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            for scenario in [
                "missing",
                "conflict",
                "preferred",
                "unavailable-preference",
                "revoked",
                "alias",
            ] {
                let (mut host, mut bindings, provider, _) = setup(profile);
                host.dynamic = true;
                let mut child = bindings[0].clone();
                child.id = 2;
                child.prepared.manifest.name = "child-language".into();
                child.prepared.manifest.contributions.languages = serde_json::from_value(serde_json::json!([{"id":"embedded", "name":"Embedded", "aliases":["body"],"features":{"syntax":true,"structure":true}}])).unwrap();
                if scenario != "missing" {
                    bindings.push(child.clone());
                }
                if matches!(scenario, "conflict" | "preferred") {
                    child.id = 3;
                    child.prepared.manifest.name = "replacement-language".into();
                    child.revision = 2;
                    child.prepared.digest = "c".repeat(64);
                    bindings.push(child);
                }
                if scenario == "revoked" {
                    host.rejected_packages
                        .lock()
                        .unwrap()
                        .insert("child-language".into());
                }
                let data = |language: &str, kind| SyntaxAnalysis {
                    highlights: Some(vec![vec![TokenSpan { end: 1, kind }]]),
                    folds: vec![],
                    structure: Some(Structure {
                        language: language.into(),
                        scopes: vec![],
                        selections: vec![],
                        opaque_starts: vec![],
                        protected: vec![],
                        brackets: vec![],
                    }),
                };
                let mut root = data("custom", TokenKind::Keyword);
                root.structure.as_mut().unwrap().scopes.push(LanguageScope {
                    range: TextRange { start: 0, end: 1 },
                    language: if scenario == "alias" {
                        "body"
                    } else {
                        "embedded"
                    }
                    .into(),
                });
                host.analyses.lock().unwrap().extend([
                    ("custom".into(), root),
                    ("embedded".into(), data("embedded", TokenKind::String)),
                    (
                        "replacement-language:embedded".into(),
                        data("embedded", TokenKind::Number),
                    ),
                ]);
                let mut preferences = LanguagePreferences::default();
                if matches!(scenario, "preferred" | "unavailable-preference") {
                    preferences.providers.insert(
                        "embedded".into(),
                        ProviderPreference::Provider {
                            namespace: if scenario == "preferred" {
                                LanguageRegistry::from_bindings(&bindings)
                                    .providers
                                    .into_iter()
                                    .find(|provider| {
                                        provider.plugin_namespace
                                            == bindings.last().unwrap().prepared.storage_namespace()
                                    })
                                    .unwrap()
                                    .namespace
                            } else {
                                "plugin:missing/language:embedded".into()
                            },
                        },
                    );
                }
                let service = LanguageService::new_with_preferences(
                    host.clone(),
                    bindings,
                    provider,
                    preferences,
                )
                .unwrap();
                let output = service.analyze(document("x", 1), 100).await.unwrap();
                let LanguageOutput::Analyzed(response) = output else {
                    unreachable!()
                };
                let analysis = response.analysis.unwrap();
                validate_analysis(&analysis, "x", "custom").unwrap();
                let expected = match scenario {
                    "preferred" => TokenKind::Number,
                    "alias" => TokenKind::String,
                    _ => TokenKind::Plain,
                };
                assert_eq!(
                    analysis.highlights.unwrap()[0],
                    vec![TokenSpan {
                        end: 1,
                        kind: expected
                    }],
                    "{profile}/{scenario}"
                );
                if scenario == "alias" {
                    assert_eq!(analysis.structure.unwrap().scopes[0].language, "embedded");
                }
            }
        }
    });
}

#[test]
fn recursive_embedding_is_bounded_and_yields_cannot_publish_after_owner_changes() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            for cancel in [false, true] {
                let (mut host, bindings, provider, _) = setup(profile);
                host.dynamic = true;
                host.cancel_on_yield = cancel;
                host.analyses.lock().unwrap().insert(
                    "custom".into(),
                    SyntaxAnalysis {
                        highlights: Some(vec![vec![TokenSpan {
                            end: 1,
                            kind: TokenKind::Keyword,
                        }]]),
                        folds: vec![],
                        structure: Some(Structure {
                            language: "custom".into(),
                            scopes: vec![LanguageScope {
                                range: TextRange { start: 0, end: 1 },
                                language: "custom".into(),
                            }],
                            selections: vec![],
                            opaque_starts: vec![],
                            protected: vec![],
                            brackets: vec![],
                        }),
                    },
                );
                let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
                let result = service.analyze(document("x", 1), 100).await;
                if cancel {
                    assert!(result.is_err());
                    assert_eq!(host.starts.load(Ordering::Relaxed), 1);
                } else {
                    let LanguageOutput::Analyzed(response) = result.unwrap() else {
                        unreachable!()
                    };
                    let data = response.analysis.unwrap();
                    validate_analysis(&data, "x", "custom").unwrap();
                    assert_eq!(
                        data.highlights.unwrap()[0],
                        vec![TokenSpan {
                            end: 1,
                            kind: TokenKind::Plain
                        }]
                    );
                    assert_eq!(
                        host.starts.load(Ordering::Relaxed),
                        super::composition::MAX_EMBEDDING_DEPTH + 1
                    );
                }
            }
        }
    });
}
