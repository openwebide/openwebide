use super::*;
use crate::plugins::execution::{GrantedHost, PluginTransport};
use openwebide_core::plugins::execution::*;
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
}
impl PluginActionHost for Host {
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
    async fn grant(&self, _: &PreparedPlugin) -> Result<String, String> {
        self.grants.fetch_add(1, Ordering::Relaxed);
        if self.deny_grant.load(Ordering::Relaxed) {
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
                        response.analysis = Some(SyntaxAnalysis {
                            highlights: None,
                            folds: vec![],
                            structure: None,
                        });
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
    };
    (host, bindings, provider, LanguageInput::Analyze(request))
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
