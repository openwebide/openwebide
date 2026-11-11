use super::*;
use crate::{
    ToolExecutor,
    plugins::execution::{GrantedHost, PluginTransport},
};
use openwebide_core::{
    MemoryVfs, ToolCall, WorkspaceMode,
    plugins::{PreparedPlugin, ProjectPlugin, execution::*},
};
use std::sync::atomic::AtomicBool;

#[derive(Clone)]
struct Host {
    context: Arc<Mutex<LanguageToolContext>>,
    calls: Arc<Mutex<Vec<protocol::Call>>>,
    current: Arc<AtomicBool>,
    deny: Arc<AtomicBool>,
    change_context: Arc<AtomicBool>,
    change_file: Arc<Mutex<Option<MemoryVfs>>>,
    ready: bool,
    result: Value,
}
impl AgentLanguageHost for Host {
    async fn language_context(&self) -> Result<LanguageToolContext, String> {
        Ok(self.context.lock().unwrap().clone())
    }
}
impl PluginTransport for Host {
    async fn ensure_prepared(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        Ok(plugin.clone())
    }
    async fn start(&self, _: InvokePlugin) -> Result<PluginInvocation, String> {
        panic!("Language server queries must not invoke a component")
    }
    async fn resume(&self, _: ContinuePlugin) -> Result<PluginInvocation, String> {
        panic!("No component invocation")
    }
    fn cancel(&self, _: String) {}
}
impl GrantedHost for Host {
    async fn request(&self, _: &PluginHostRequest) -> Result<String, String> {
        panic!("Read-only intelligence cannot request mutable host capabilities")
    }
}
impl PluginActionHost for Host {
    async fn prepare(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        Ok(plugin.clone())
    }
    async fn grant(&self, _: &PreparedPlugin) -> Result<String, String> {
        if self.deny.load(Ordering::Relaxed) {
            Err("Provider is disabled".into())
        } else {
            Ok("authority".into())
        }
    }
    fn is_current(&self) -> bool {
        self.current.load(Ordering::Relaxed)
    }
    async fn language_server(&self, call: &protocol::Call) -> Result<protocol::Response, String> {
        assert_eq!(call.grant, "authority");
        self.calls.lock().unwrap().push(call.clone());
        if self.change_context.load(Ordering::Relaxed) {
            self.context.lock().unwrap().bindings[0].revision += 1;
        }
        let files = self.change_file.lock().unwrap().take();
        if let Some(files) = files {
            files.write("file.custom", "external change").await.unwrap();
        }
        Ok(protocol::Response {
            version: protocol::VERSION,
            client: call.input.client.clone(),
            document: call.input.document.clone(),
            request: call.input.request,
            revision: call.input.revision,
            source_digest: call.input.source_digest.clone(),
            server: "native-service".into(),
            context: None,
            capabilities: json!({}),
            result: self.result.clone(),
            diagnostics: vec![],
            diagnostics_ready: self.ready,
        })
    }
}
fn setup(mode: WorkspaceMode) -> (Host, LanguageTools<Host>, MemoryVfs) {
    let mut prepared = openwebide_core::plugins::testing::receipt();
    prepared.host_id = if mode == WorkspaceMode::Local {
        "paired"
    } else {
        "server"
    }
    .into();
    prepared.manifest.compatibility.plugin_api = 9;
    prepared.manifest.executable = None;
    prepared.manifest.contributions.languages = serde_json::from_value(json!([{"id":"custom","name":"Custom","extensions":["custom"],"features":{"languageServer":{"command":"custom-lsp"}}}])).unwrap();
    let scope = LanguageToolScope {
        project: 1,
        mode,
        root: (mode == WorkspaceMode::Remote).then(|| "project".into()),
    };
    let host = Host {
        context: Arc::new(Mutex::new(LanguageToolContext {
            scope: scope.clone(),
            bindings: vec![ProjectPlugin {
                id: 1,
                revision: 1,
                enabled: true,
                prepared,
            }],
            preferences: LanguagePreferences::default(),
        })),
        calls: Arc::default(),
        current: Arc::new(AtomicBool::new(true)),
        deny: Arc::default(),
        change_context: Arc::default(),
        change_file: Arc::default(),
        ready: false,
        result: json!([]),
    };
    (
        host.clone(),
        LanguageTools::new(host, scope, 42),
        MemoryVfs::new(),
    )
}
fn args(operation: &str) -> LanguageToolArgs {
    serde_json::from_value(json!({"operation":operation,"path":"file.custom"})).unwrap()
}
#[test]
fn both_scopes_use_workspace_source_and_explicit_pending_diagnostics() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (host, tools, files) = setup(mode);
            files.write("file.custom", "文😀\r\n").await.unwrap();
            let output = tools.query(&files, &args("diagnostics"), 2048).await;
            assert!(output.ok, "{}", output.content);
            let result: Value = serde_json::from_str(&output.content).unwrap();
            assert_eq!(result["ready"], false);
            assert_eq!(result["source"], "workspace_files");
            assert_eq!(result["records"][0]["ready"], false);
            let call = &host.calls.lock().unwrap()[0];
            assert_eq!(call.input.source, "文😀\r\n");
            assert!(call.input.client.starts_with("agent-"));
            assert_eq!(
                call.prepared.host_id,
                if mode == WorkspaceMode::Local {
                    "paired"
                } else {
                    "server"
                }
            );
        }
    });
}
#[test]
fn agent_executor_loads_tool_and_preserves_read_only_policy() {
    futures::executor::block_on(async {
        let (host, tools, files) = setup(WorkspaceMode::Remote);
        files.write("file.custom", "text").await.unwrap();
        let executor = crate::VfsToolExecutor::new(files).with_code_intelligence(Arc::new(tools));
        let call = ToolCall {
            id: "query".into(),
            name: "code_intelligence".into(),
            arguments: json!({"operation":"symbols","path":"file.custom"}).to_string(),
        };
        let outcome = executor.execute(&call).await;
        assert!(outcome.ok, "{}", outcome.content);
        assert!(!crate::requires_approval(&call));
        assert!(crate::tools::ToolName::CodeIntelligence.cancellable());
        assert!(!host.calls.lock().unwrap().is_empty());
        let mut malicious = call;
        malicious.arguments =
            json!({"operation":"execute_command","path":"file.custom","command":"write"})
                .to_string();
        assert!(!executor.execute(&malicious).await.ok);
    });
}
#[test]
fn changed_scope_settings_file_and_denied_authority_never_publish_results() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for failure in [
                "scope",
                "disabled",
                "preferences",
                "ambiguity",
                "grant",
                "context",
                "file",
                "cancel",
            ] {
                let (host, tools, files) = setup(mode);
                files.write("file.custom", "text").await.unwrap();
                match failure {
                    "scope" => host.context.lock().unwrap().scope.project += 1,
                    "disabled" => host.context.lock().unwrap().bindings[0].enabled = false,
                    "preferences" => {
                        host.context
                            .lock()
                            .unwrap()
                            .preferences
                            .providers
                            .insert("custom".into(), ProviderPreference::Disabled);
                    }
                    "ambiguity" => {
                        let mut context = host.context.lock().unwrap();
                        let mut binding = context.bindings[0].clone();
                        binding.id = 2;
                        binding.prepared.manifest.name = "other".into();
                        binding.prepared.source.path = "plugins/other".into();
                        context.bindings.push(binding);
                    }
                    "grant" => host.deny.store(true, Ordering::Relaxed),
                    "context" => host.change_context.store(true, Ordering::Relaxed),
                    "file" => *host.change_file.lock().unwrap() = Some(files.clone()),
                    _ => host.current.store(false, Ordering::Relaxed),
                }
                let outcome = tools.query(&files, &args("symbols"), 2048).await;
                assert!(!outcome.ok, "{mode:?}/{failure}: {}", outcome.content);
                if !matches!(failure, "context" | "file") {
                    assert!(host.calls.lock().unwrap().is_empty(), "{failure}");
                }
            }
        }
    });
}
#[test]
fn navigation_validates_unicode_positions_and_closes_previous_documents() {
    futures::executor::block_on(async {
        let (host, tools, files) = setup(WorkspaceMode::Remote);
        files.write("file.custom", "文😀\r\n").await.unwrap();
        files.write("other.custom", "second").await.unwrap();
        let mut query = args("definitions");
        query.line = Some(1);
        query.column = Some(2);
        assert!(tools.query(&files, &query, 2048).await.ok);
        assert!(matches!(
            host.calls.lock().unwrap()[0].input.query,
            Query::Definition {
                position: protocol::Position {
                    line: 0,
                    character: 1
                }
            }
        ));
        query.column = Some(3); // inside the emoji surrogate pair
        assert!(!tools.query(&files, &query, 2048).await.ok);
        query = args("symbols");
        query.path = "other.custom".into();
        assert!(tools.query(&files, &query, 2048).await.ok);
        let calls = host.calls.lock().unwrap();
        assert!(calls.iter().any(
            |call| call.input.path == "file.custom" && matches!(call.input.query, Query::Close)
        ));
        assert!(
            calls
                .windows(2)
                .all(|pair| pair[0].input.request < pair[1].input.request)
        );
    });
}
#[test]
fn output_budget_preserves_json_and_reports_omitted_records() {
    let records = (0..1000)
        .map(|index| json!({"name":format!("文😀{index}"),"details":"x".repeat(500)}))
        .collect::<Vec<_>>();
    for budget in [2048, 8192, 65536] {
        let output = bounded_output(
            LanguageToolOperation::Symbols,
            "file.custom",
            &"a".repeat(64),
            true,
            records.clone(),
            budget,
        )
        .unwrap();
        assert!(output.len() <= budget);
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["truncated"], true);
        assert_eq!(
            value["records"].as_array().unwrap().len()
                + usize::try_from(value["omitted"].as_u64().unwrap()).unwrap(),
            1000
        );
    }
    let query = Query::ReadResource { id: "a".repeat(32) };
    let mut args = args("read_resource");
    args.id = Some("a".repeat(32));
    args.offset = Some(2);
    args.limit = Some(1);
    let resource = json!({"name":"framework.cs","source":"first\n文😀\nthird","readOnly":true});
    let result = result_records(&query, resource, &args, 2048).unwrap();
    assert_eq!(result[0]["source"], "文😀\n");
    assert_eq!(result[0]["truncated"], true);
}

#[test]
fn result_contract_keeps_locations_lazy_symbols_and_resources_usable() {
    let target = json!({"path":"file.custom","resource":null,"source_digest":source_digest("text"),"label":"file.custom","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":4}}});
    for query in [
        Query::Definition {
            position: protocol::Position::default(),
        },
        Query::References {
            position: protocol::Position::default(),
        },
        Query::ResolveSymbol { id: "a".repeat(32) },
    ] {
        let records = result_records(&query, json!([target]), &args("symbols"), 2048).unwrap();
        assert_eq!(records[0]["path"], "file.custom");
        let mut escaped = target.clone();
        escaped["path"] = "../outside".into();
        assert!(result_records(&query, json!([escaped]), &args("symbols"), 2048).is_err());
    }
    let symbol = json!({"name":"文","kind":12,"target":target,"depth":0});
    assert_eq!(
        result_records(&Query::Symbols, json!([symbol]), &args("symbols"), 2048).unwrap()[0]["name"],
        "文"
    );
    let query = Query::WorkspaceSymbols {
        query: "name".into(),
    };
    let lazy = json!({"name":"name","kind":12,"container":null,"target":null,"id":"a".repeat(32)});
    assert!(result_records(&query, json!([lazy]), &args("workspace_symbols"), 2048).is_ok());
    let invalid = json!({"name":"name","kind":12,"container":null,"target":null,"id":null});
    assert!(result_records(&query, json!([invalid]), &args("workspace_symbols"), 2048).is_err());
}
