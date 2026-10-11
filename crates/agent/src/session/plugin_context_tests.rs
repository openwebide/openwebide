use super::*;
use crate::plugins::execution::{GrantedHost, PluginTransport};
use openwebide_core::{
    ToolSelection, WorkspaceMode,
    plugins::{
        PluginTool, ProjectPlugin, RustPlugin,
        execution::{
            ContinuePlugin, InvokePlugin, PluginHostRequest, PluginInvocation, PluginOperation,
            PluginStep,
        },
    },
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Transport {
    calls: Arc<Mutex<Vec<InvokePlugin>>>,
    cancelled: Arc<Mutex<Vec<String>>>,
    output: serde_json::Value,
    operation: &'static str,
}
impl PluginTransport for Transport {
    async fn ensure_prepared(
        &self,
        expected: &openwebide_core::plugins::PreparedPlugin,
    ) -> Result<openwebide_core::plugins::PreparedPlugin, String> {
        Ok(expected.clone())
    }

    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        self.calls.lock().unwrap().push(call);
        Ok(PluginInvocation {
            id: "context".into(),
            step: PluginStep::Ready,
        })
    }
    async fn resume(&self, response: ContinuePlugin) -> Result<PluginInvocation, String> {
        let step = match response.sequence {
            0 => PluginStep::HostCall {
                sequence: 1,
                capability: "records".into(),
                payload: serde_json::json!({"collection":"notes","operation":if self.operation == "create" {
                    serde_json::json!({"action":"create","value":{"text":"mutation attempt"}})
                } else { serde_json::json!({"action":self.operation}) }}).to_string(),
            },
            1 => {
                assert_eq!(response.response, Ok("{}".into()));
                PluginStep::Complete {
                    ok: true,
                    content: self.output.to_string(),
                    summary: "context".into(),
                }
            }
            _ => panic!("unexpected continuation"),
        };
        Ok(PluginInvocation {
            id: response.id,
            step,
        })
    }
    fn cancel(&self, id: String) {
        self.cancelled.lock().unwrap().push(id);
    }
}
#[derive(Clone, Default)]
struct Host(Arc<Mutex<Vec<PluginHostRequest>>>);
impl GrantedHost for Host {
    async fn request(&self, request: &PluginHostRequest) -> Result<String, String> {
        self.0.lock().unwrap().push(request.clone());
        Ok("{}".into())
    }
}
fn binding() -> ProjectPlugin {
    let mut prepared = openwebide_core::plugins::testing::receipt();
    prepared.manifest.compatibility.plugin_api = 3;
    prepared.manifest.contributions.skills.clear();
    prepared.manifest.contributions.tools = vec![PluginTool {
        name: "community_lookup".into(),
        description: "Look up stored facts".into(),
        parameters: serde_json::json!({"type":"object"}),
        requires_approval: false,
    }];
    prepared.manifest.executable = Some(RustPlugin {
        manifest: "Cargo.toml".into(),
        library: "lookup".into(),
        sdk_version: "0.1.0".into(),
        capabilities: vec!["records".into()],
    });
    ProjectPlugin {
        id: 1,
        revision: 1,
        prepared,
        enabled: true,
    }
}
fn runtime() -> ModelRuntime {
    serde_json::from_value(serde_json::json!({"connection":{"id":1,"name":"test","kind":"ollama","base_url":"http://localhost","model":"test","enabled":true},"settings":{},"transport":{}})).unwrap()
}
fn input(mode: WorkspaceMode, projectless: bool) -> PlanInput {
    PlanInput {
        environment: openwebide_core::RunEnvironment {
            mode: Some(mode),
            project_name: (!projectless).then(|| "p".into()),
            project_root: (!projectless).then(|| "p".into()),
            ..Default::default()
        },
        system_prompt: Some("BASE".into()),
        messages: vec![],
        tools: vec![],
        content: "go".into(),
        editor: None,
    }
}
fn transport(output: serde_json::Value) -> Transport {
    Transport {
        calls: Default::default(),
        cancelled: Default::default(),
        output,
        operation: "list",
    }
}

#[test]
fn context_precedes_model_selection_and_retains_only_executable_authority_in_both_modes() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for projectless in [false, true] {
                for tools in [false, true] {
                    for disabled in [false, true] {
                        for selection in [ToolSelection::All, ToolSelection::ChatOnly] {
                            let binding = binding();
                            let mut runtime = runtime();
                            runtime.settings.tools = Some(tools);
                            runtime.connection.tool_selection = selection.clone();
                            let output = serde_json::json!({"prompt":"PLUGIN MEMORY","disabled_tools":if disabled {vec!["community_lookup"]} else {vec![]}});
                            let transport = transport(output);
                            let host = Host::default();
                            let plan = plan_with_plugin_context(
                                &runtime,
                                input(mode, projectless),
                                std::slice::from_ref(&binding),
                                &transport,
                                host.clone(),
                                |plugins| async move {
                                    assert_eq!(plugins.len(), 1);
                                    Ok(std::collections::BTreeMap::from([(
                                        plugins[0].digest.clone(),
                                        "opaque-grant".into(),
                                    )]))
                                },
                            )
                            .await
                            .unwrap();
                            assert!(
                                plan.request
                                    .system_prompt
                                    .unwrap()
                                    .contains("BASE\n\nPLUGIN MEMORY")
                            );
                            let advertised =
                                tools && !disabled && selection.allows("community_lookup");
                            assert_eq!(
                                plan.request
                                    .tools
                                    .iter()
                                    .any(|tool| tool.name == "community_lookup"),
                                advertised
                            );
                            assert_eq!(plan.plugin_executables.len(), usize::from(advertised));
                            assert_eq!(plan.plugin_grants.len(), usize::from(advertised));
                            let calls = transport.calls.lock().unwrap();
                            assert_eq!(calls.len(), 1);
                            assert!(matches!(calls[0].operation, PluginOperation::Context));
                            assert!(calls[0].name.is_empty());
                            assert_eq!(
                                serde_json::from_str::<serde_json::Value>(&calls[0].arguments)
                                    .unwrap()["budget_bytes"],
                                8190
                            );
                            assert_eq!(host.0.lock().unwrap()[0].grant, "opaque-grant");
                            assert!(transport.cancelled.lock().unwrap().is_empty());
                        }
                    }
                }
            }
        }
    });
}

#[test]
fn planning_failures_never_fall_back_to_builtin_context_or_forward_mutations() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let binding = binding();
            let runtime = runtime();
            for output in [
                serde_json::json!({"prompt":"x".repeat(8192),"disabled_tools":[]}),
                serde_json::json!({"prompt":null,"disabled_tools":["another_plugin_tool"]}),
                serde_json::json!({"prompt":null,"disabled_tools":["community_lookup","community_lookup"]}),
            ] {
                let transport = transport(output);
                assert!(
                    plan_with_plugin_context(
                        &runtime,
                        input(mode, false),
                        std::slice::from_ref(&binding),
                        &transport,
                        Host::default(),
                        |plugins| async move {
                            Ok(std::collections::BTreeMap::from([(
                                plugins[0].digest.clone(),
                                "grant".into(),
                            )]))
                        }
                    )
                    .await
                    .is_err()
                );
            }
            let mut transport = transport(serde_json::json!({"prompt":null,"disabled_tools":[]}));
            transport.operation = "create";
            let host = Host::default();
            assert!(
                plan_with_plugin_context(
                    &runtime,
                    input(mode, false),
                    std::slice::from_ref(&binding),
                    &transport,
                    host.clone(),
                    |plugins| async move {
                        Ok(std::collections::BTreeMap::from([(
                            plugins[0].digest.clone(),
                            "grant".into(),
                        )]))
                    }
                )
                .await
                .is_err()
            );
            assert!(host.0.lock().unwrap().is_empty());
            assert_eq!(*transport.cancelled.lock().unwrap(), vec!["context"]);
            let denied = transport.calls.lock().unwrap().len();
            assert!(
                plan_with_plugin_context(
                    &runtime,
                    input(mode, false),
                    std::slice::from_ref(&binding),
                    &transport,
                    Host::default(),
                    |_| async { Err("Account changed".into()) }
                )
                .await
                .is_err()
            );
            assert_eq!(transport.calls.lock().unwrap().len(), denied);
        }
    });
}
