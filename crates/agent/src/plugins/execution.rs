//! Shared executable-plugin workflow; adapters supply transport and host primitives.
use crate::{ToolExecutor, ToolOutcome, ToolPreview};
use openwebide_core::plugins::{
    PreparedPlugin,
    execution::{ContinuePlugin, InvokePlugin, PluginInvocation, PluginStep},
};
use openwebide_core::{ToolCall, ToolDefinition};
use std::{future::Future, sync::Arc};

pub trait PluginTransport: Clone + Send + Sync {
    /// Prepare the selected immutable source in this host's cache.
    fn ensure_prepared(
        &self,
        expected: &PreparedPlugin,
    ) -> impl Future<Output = Result<PreparedPlugin, String>> + Send;

    fn start(
        &self,
        call: InvokePlugin,
    ) -> impl Future<Output = Result<PluginInvocation, String>> + Send;
    fn resume(
        &self,
        response: ContinuePlugin,
    ) -> impl Future<Output = Result<PluginInvocation, String>> + Send;
    /// Runtime primitive: cancellation must also run when the calling future drops.
    fn cancel(&self, id: String);
    /// Flush cancellation requests on adapters without a background executor.
    fn flush_cancellations(&self) -> impl Future<Output = ()> + Send {
        async {}
    }
}
pub trait PluginServices: Send + Sync {
    fn request(
        &self,
        plugin: &PreparedPlugin,
        capability: &str,
        payload: &str,
    ) -> impl Future<Output = Result<String, String>> + Send;
}
pub trait GrantedHost: Send + Sync {
    fn request(
        &self,
        request: &openwebide_core::plugins::execution::PluginHostRequest,
    ) -> impl Future<Output = Result<String, String>> + Send;
}
/// Both run adapters attach opaque authority using this shared policy.
pub struct GrantedServices<S> {
    pub host: S,
    pub grants: Arc<std::collections::BTreeMap<String, String>>,
}
impl<S: GrantedHost> PluginServices for GrantedServices<S> {
    async fn request(
        &self,
        plugin: &PreparedPlugin,
        capability: &str,
        payload: &str,
    ) -> Result<String, String> {
        let grant = self
            .grants
            .get(&plugin.digest)
            .ok_or("Plugin execution grant is unavailable")?;
        self.host
            .request(&openwebide_core::plugins::execution::PluginHostRequest {
                grant: grant.clone(),
                capability: capability.into(),
                payload: payload.into(),
            })
            .await
    }
}
/// Manifest requirements add to host policy; they never waive it.
pub struct PluginGate<G> {
    pub gate: G,
    pub plugins: Arc<Vec<PreparedPlugin>>,
}
impl<G: crate::PermissionGate> crate::PermissionGate for PluginGate<G> {
    fn needs_approval(&self, call: &ToolCall) -> bool {
        self.gate.needs_approval(call)
            || self.plugins.iter().any(|plugin| {
                plugin
                    .manifest
                    .contributions
                    .tools
                    .iter()
                    .any(|tool| tool.name == call.name && tool.requires_approval)
            })
    }
    fn uses_automatic_approval(&self) -> bool {
        self.gate.uses_automatic_approval()
    }
    fn automatically_approve(&self, call: &ToolCall) -> impl Future<Output = bool> + Send {
        self.gate.automatically_approve(call)
    }
    fn approve(&self, call: &ToolCall) -> impl Future<Output = bool> + Send {
        self.gate.approve(call)
    }
}
struct InvocationGuard<T: PluginTransport> {
    transport: T,
    id: Option<String>,
}
impl<T: PluginTransport> Drop for InvocationGuard<T> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.transport.cancel(id);
        }
    }
}

/// A run owns an immutable selection. Installation changes affect later runs.
pub struct PluginTools<E, T, S> {
    pub executor: E,
    pub transport: T,
    pub services: S,
    pub plugins: Arc<Vec<PreparedPlugin>>,
}
impl<E, T: PluginTransport, S: PluginServices> PluginTools<E, T, S> {
    fn plugin(&self, name: &str) -> Option<&PreparedPlugin> {
        self.plugins.iter().find(|plugin| {
            plugin
                .manifest
                .contributions
                .tools
                .iter()
                .any(|tool| tool.name == name)
        })
    }
    async fn invoke(
        &self,
        plugin: &PreparedPlugin,
        call: &ToolCall,
    ) -> Result<ToolOutcome, String> {
        invoke_plugin(
            &self.transport,
            &self.services,
            InvokePlugin {
                operation: openwebide_core::plugins::execution::PluginOperation::Tool,
                prepared: plugin.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            },
        )
        .await
    }
}

/// A different host may produce its own host ID, but cannot change the selected program.
pub fn validate_prepared_selection(
    expected: &PreparedPlugin,
    prepared: &PreparedPlugin,
) -> Result<(), String> {
    prepared.validate().map_err(|error| error.to_string())?;
    if prepared.source != expected.source
        || prepared.manifest != expected.manifest
        || prepared.digest != expected.digest
    {
        return Err("Plugin version changed during preparation".into());
    }
    Ok(())
}

/// One bounded invocation workflow for tools and planning hooks on every host.
pub async fn invoke_plugin<T: PluginTransport, S: PluginServices>(
    transport: &T,
    services: &S,
    call: InvokePlugin,
) -> Result<ToolOutcome, String> {
    let result = invoke_inner(transport, services, call).await;
    transport.flush_cancellations().await;
    result
}

async fn invoke_inner<T: PluginTransport, S: PluginServices>(
    transport: &T,
    services: &S,
    mut call: InvokePlugin,
) -> Result<ToolOutcome, String> {
    call.validate_event()?;
    call.prepared
        .validate()
        .map_err(|error| error.to_string())?;
    let prepared = transport.ensure_prepared(&call.prepared).await?;
    validate_prepared_selection(&call.prepared, &prepared)?;
    call.prepared = prepared;
    let context = matches!(
        call.operation,
        openwebide_core::plugins::execution::PluginOperation::Context
    );
    let plugin = call.prepared.clone();
    let mut invocation = transport.start(call).await?;
    let mut guard = InvocationGuard {
        transport: transport.clone(),
        id: Some(invocation.id.clone()),
    };
    let mut started = false;
    let mut last_sequence = 0;
    // Initial handshake, up to 128 imports, and the terminal result.
    for _ in 0..130 {
        if invocation.id != guard.id.as_deref().unwrap_or_default() {
            return Err("Plugin transport changed invocation identity.".into());
        }
        match invocation.step {
            PluginStep::Ready => {
                if started {
                    return Err("Plugin transport repeated its initial handshake.".into());
                }
                started = true;
                invocation = transport
                    .resume(ContinuePlugin {
                        id: invocation.id,
                        sequence: 0,
                        response: Ok(String::new()),
                    })
                    .await?;
            }
            PluginStep::Complete {
                ok,
                content,
                summary,
            } => {
                guard.id = None;
                return Ok(ToolOutcome {
                    ok,
                    content,
                    summary,
                    diff: None,
                });
            }
            PluginStep::Failed { error } => {
                guard.id = None;
                return Err(error);
            }
            PluginStep::HostCall {
                sequence,
                capability,
                payload,
            } => {
                if !started || sequence <= last_sequence || sequence > 128 {
                    return Err("Plugin transport returned a stale host call.".into());
                }
                last_sequence = sequence;
                let executable = plugin
                    .manifest
                    .executable
                    .as_ref()
                    .ok_or("Plugin has no executable")?;
                if !executable.capabilities.contains(&capability) {
                    return Err("Plugin requested an undeclared capability.".into());
                }
                if context
                    && !openwebide_core::plugins::execution::context_request_allowed(
                        &capability,
                        &payload,
                    )
                {
                    return Err("Plugin context hooks cannot mutate host state".into());
                }
                let response = services.request(&plugin, &capability, &payload).await;
                invocation = transport
                    .resume(ContinuePlugin {
                        id: invocation.id,
                        sequence,
                        response,
                    })
                    .await?;
            }
        }
    }
    Err("Plugin exceeded its host-call limit.".into())
}

impl<E: ToolExecutor + Sync, T: PluginTransport, S: PluginServices> ToolExecutor
    for PluginTools<E, T, S>
{
    fn has_context(&self) -> bool {
        self.executor.has_context()
    }
    async fn context(
        &mut self,
        tools: &[ToolDefinition],
        call: Option<&ToolCall>,
    ) -> Option<String> {
        self.executor.context(tools, call).await
    }
    fn describe(&self, call: &ToolCall) -> String {
        self.plugin(&call.name).map_or_else(
            || self.executor.describe(call),
            |plugin| format!("{}: {}", plugin.manifest.display_name, call.name),
        )
    }
    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        if let Some(plugin) = self.plugin(&call.name) {
            return Some(ToolPreview {
                diff: None,
                note: Some(format!(
                    "Plugin: {}/{} {}\n{}",
                    plugin.manifest.publisher,
                    plugin.manifest.name,
                    plugin.manifest.version,
                    call.arguments
                )),
            });
        }
        self.executor.preview(call).await
    }
    async fn checkpoint(
        &self,
        call: &ToolCall,
    ) -> Result<Option<openwebide_core::FileDiff>, String> {
        if self.plugin(&call.name).is_some() {
            return Ok(None);
        }
        self.executor.checkpoint(call).await
    }
    async fn project_checkpoint(
        &self,
        call: &ToolCall,
    ) -> Result<Option<openwebide_core::rewind::ProjectSnapshot>, String> {
        if self.plugin(&call.name).is_some() {
            return Ok(None);
        }
        self.executor.project_checkpoint(call).await
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutcome {
        let Some(plugin) = self.plugin(&call.name) else {
            return self.executor.execute(call).await;
        };
        self.invoke(plugin, call)
            .await
            .unwrap_or_else(|error| ToolOutcome {
                ok: false,
                content: error,
                summary: format!("Plugin {} failed", plugin.manifest.display_name),
                diff: None,
            })
    }
    fn acknowledge(&self, id: &str) {
        self.executor.acknowledge(id);
    }
}

pub fn configure_tools(
    tools: &mut Vec<ToolDefinition>,
    bindings: &[openwebide_core::plugins::ProjectPlugin],
) -> Result<Vec<PreparedPlugin>, String> {
    let mut plugins = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for binding in bindings
        .iter()
        .filter(|binding| binding.enabled && binding.prepared.manifest.executable.is_some())
    {
        binding
            .prepared
            .validate()
            .map_err(|error| error.to_string())?;
        for tool in &binding.prepared.manifest.contributions.tools {
            if !names.insert(tool.name.clone())
                || tools.iter().any(|existing| existing.name == tool.name)
            {
                return Err(format!(
                    "Plugin tool '{}' conflicts with another active tool.",
                    tool.name
                ));
            }
        }
        plugins.push(binding.prepared.clone());
    }
    for plugin in &plugins {
        tools.extend(
            plugin
                .manifest
                .contributions
                .tools
                .iter()
                .map(|tool| ToolDefinition {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                }),
        );
    }
    Ok(plugins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{FutureExt, executor::block_on};
    use std::{collections::VecDeque, sync::Mutex};

    #[derive(Clone, Default)]
    struct Transport {
        steps: Arc<Mutex<VecDeque<PluginInvocation>>>,
        replies: Arc<Mutex<Vec<ContinuePlugin>>>,
        cancellations: Arc<Mutex<Vec<String>>>,
    }
    impl Transport {
        fn next(&self) -> Result<PluginInvocation, String> {
            self.steps
                .lock()
                .unwrap()
                .pop_front()
                .ok_or("Disconnected".into())
        }
    }
    impl PluginTransport for Transport {
        async fn ensure_prepared(
            &self,
            expected: &PreparedPlugin,
        ) -> Result<PreparedPlugin, String> {
            Ok(expected.clone())
        }

        async fn start(&self, _: InvokePlugin) -> Result<PluginInvocation, String> {
            self.next()
        }
        async fn resume(&self, response: ContinuePlugin) -> Result<PluginInvocation, String> {
            self.replies.lock().unwrap().push(response);
            self.next()
        }
        fn cancel(&self, id: String) {
            self.cancellations.lock().unwrap().push(id);
        }
    }
    #[derive(Clone)]
    struct PreparingTransport {
        inner: Transport,
        prepared: Result<PreparedPlugin, String>,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }
    impl PluginTransport for PreparingTransport {
        async fn ensure_prepared(&self, _: &PreparedPlugin) -> Result<PreparedPlugin, String> {
            self.calls.lock().unwrap().push("prepare");
            self.prepared.clone()
        }
        async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
            self.calls.lock().unwrap().push("start");
            assert_eq!(call.prepared.host_id, "cold-host");
            self.inner.start(call).await
        }
        async fn resume(&self, response: ContinuePlugin) -> Result<PluginInvocation, String> {
            self.calls.lock().unwrap().push("resume");
            self.inner.resume(response).await
        }
        fn cancel(&self, id: String) {
            self.inner.cancel(id);
        }
    }
    #[test]
    fn cold_host_preparation_is_required_for_tools_context_and_events_in_both_modes() {
        use openwebide_core::{WorkspaceMode, plugins::execution::PluginOperation};
        block_on(async {
            for _mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                for operation in [
                    PluginOperation::Tool,
                    PluginOperation::Context,
                    PluginOperation::Event,
                ] {
                    let selected = plugin();
                    let mut prepared = selected.clone();
                    prepared.host_id = "cold-host".into();
                    let mut altered_source = prepared.clone();
                    altered_source.source.commit = "b".repeat(40);
                    let mut altered_digest = prepared.clone();
                    altered_digest.digest = "b".repeat(64);
                    let mut altered_manifest = prepared.clone();
                    altered_manifest.manifest.display_name = "Different behavior".into();
                    for (receipt, succeeds) in [
                        (Ok(prepared.clone()), true),
                        (Err("Compiler unavailable".into()), false),
                        (Ok(altered_source), false),
                        (Ok(altered_digest), false),
                        (Ok(altered_manifest), false),
                    ] {
                        let inner = Transport::default();
                        inner.steps.lock().unwrap().extend([
                            PluginInvocation {
                                id: "cold".into(),
                                step: PluginStep::Ready,
                            },
                            PluginInvocation {
                                id: "cold".into(),
                                step: PluginStep::Complete {
                                    ok: true,
                                    content: "{}".into(),
                                    summary: "Done".into(),
                                },
                            },
                        ]);
                        let transport = PreparingTransport {
                            inner,
                            prepared: receipt,
                            calls: Arc::default(),
                        };
                        let services = Services::default();
                        let result = invoke_plugin(
                            &transport,
                            &services,
                            InvokePlugin {
                                prepared: selected.clone(),
                                operation,
                                name: if matches!(operation, PluginOperation::Tool) {
                                    "fixture_echo".into()
                                } else {
                                    String::new()
                                },
                                arguments: if matches!(operation, PluginOperation::Event) {
                                    serde_json::json!({"name":"job_due","payload":null}).to_string()
                                } else {
                                    "{}".into()
                                },
                            },
                        )
                        .await;
                        assert_eq!(result.is_ok(), succeeds, "{result:?}");
                        let calls = transport.calls.lock().unwrap();
                        if succeeds {
                            assert_eq!(*calls, ["prepare", "start", "resume"]);
                        } else {
                            assert_eq!(*calls, ["prepare"]);
                            assert_eq!(transport.inner.steps.lock().unwrap().len(), 2);
                        }
                        assert_eq!(*services.calls.lock().unwrap(), 0);
                    }
                }
            }
        });
    }
    #[derive(Default)]
    struct Services {
        calls: Mutex<usize>,
        wait: bool,
    }
    impl PluginServices for Services {
        async fn request(&self, _: &PreparedPlugin, _: &str, _: &str) -> Result<String, String> {
            *self.calls.lock().unwrap() += 1;
            if self.wait {
                futures::future::pending::<()>().await;
            }
            Ok("{}".into())
        }
    }
    fn plugin() -> PreparedPlugin {
        let mut plugin = openwebide_core::plugins::testing::receipt();
        plugin.manifest.contributions.events = vec!["job_due".into()];
        plugin.manifest.compatibility.plugin_api = 3;
        plugin.manifest.executable = Some(openwebide_core::plugins::RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "fixture".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["records".into()],
        });
        plugin.manifest.contributions.tools = vec![openwebide_core::plugins::PluginTool {
            name: "fixture_echo".into(),
            description: "Echo".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: false,
        }];
        plugin
    }
    #[test]
    fn events_share_the_invocation_workflow_and_validate_before_transport_effects() {
        block_on(async {
            let transport = Transport::default();
            transport.steps.lock().unwrap().extend([
                PluginInvocation {id:"event".into(),step:PluginStep::Ready},
                PluginInvocation {id:"event".into(),step:PluginStep::HostCall {
                    sequence:1,capability:"records".into(),payload:serde_json::json!({"collection":"events","operation":{"action":"create","value":{"job_id":7}}}).to_string(),
                }},
                PluginInvocation {id:"event".into(),step:PluginStep::Complete {ok:true,content:"Plugin event result".into(),summary:"Handled".into()}},
            ]);
            let services = Services::default();
            let call = InvokePlugin {
                operation: openwebide_core::plugins::execution::PluginOperation::Event,
                prepared: plugin(),
                name: String::new(),
                arguments: serde_json::json!({"name":"job_due","payload":{"job_id":7}}).to_string(),
            };
            assert_eq!(
                invoke_plugin(&transport, &services, call.clone())
                    .await
                    .unwrap()
                    .content,
                "Plugin event result"
            );
            assert_eq!(*services.calls.lock().unwrap(), 1);
            assert_eq!(transport.replies.lock().unwrap().len(), 2);
            assert!(transport.cancellations.lock().unwrap().is_empty());
            for arguments in [
                serde_json::json!({"name":"../invalid","payload":null}),
                serde_json::json!({"name":"job_due","payload":null,"grant":"forged"}),
                serde_json::json!({"name":"job_due","payload":"x".repeat(256*1024)}),
            ] {
                let invalid = InvokePlugin {
                    arguments: arguments.to_string(),
                    ..call.clone()
                };
                let error = invoke_plugin(&transport, &services, invalid)
                    .await
                    .unwrap_err();
                assert!(
                    !error.contains("Disconnected"),
                    "validation must precede transport calls"
                );
            }
            let invalid = InvokePlugin {
                name: "fixture_echo".into(),
                ..call
            };
            assert!(
                invoke_plugin(&transport, &services, invalid)
                    .await
                    .unwrap_err()
                    .contains("tool name")
            );
            assert_eq!(*services.calls.lock().unwrap(), 1);
        });
    }
    fn call() -> ToolCall {
        ToolCall {
            id: "call".into(),
            name: "fixture_echo".into(),
            arguments: "{}".into(),
        }
    }
    fn host_call(sequence: u32) -> PluginStep {
        PluginStep::HostCall {
            sequence,
            capability: "records".into(),
            payload: "{}".into(),
        }
    }
    fn wrapper(steps: Vec<PluginStep>, wait: bool) -> PluginTools<(), Transport, Services> {
        let transport = Transport::default();
        *transport.steps.lock().unwrap() = steps
            .into_iter()
            .map(|step| PluginInvocation {
                id: "invocation".into(),
                step,
            })
            .collect();
        PluginTools {
            executor: (),
            transport,
            services: Services {
                wait,
                ..Services::default()
            },
            plugins: Arc::new(vec![plugin()]),
        }
    }
    #[test]
    fn full_host_call_budget_retains_the_terminal_result() {
        let mut steps = vec![PluginStep::Ready];
        steps.extend((1..=128).map(host_call));
        steps.push(PluginStep::Complete {
            ok: true,
            content: "done".into(),
            summary: "done".into(),
        });
        let wrapper = wrapper(steps, false);
        let result = block_on(wrapper.invoke(&plugin(), &call())).unwrap();
        assert!(result.ok);
        assert_eq!(*wrapper.services.calls.lock().unwrap(), 128);
        assert_eq!(wrapper.transport.replies.lock().unwrap().len(), 129);
        assert!(wrapper.transport.cancellations.lock().unwrap().is_empty());
    }
    #[test]
    fn invalid_callbacks_are_cancelled_before_their_side_effects() {
        for steps in [
            vec![PluginStep::Ready, PluginStep::Ready],
            vec![host_call(1)],
            vec![PluginStep::Ready, host_call(0)],
            vec![PluginStep::Ready, host_call(129)],
            vec![
                PluginStep::Ready,
                PluginStep::HostCall {
                    sequence: 1,
                    capability: "workspace".into(),
                    payload: "{}".into(),
                },
            ],
        ] {
            let wrapper = wrapper(steps, false);
            assert!(block_on(wrapper.invoke(&plugin(), &call())).is_err());
            assert_eq!(*wrapper.services.calls.lock().unwrap(), 0);
            assert_eq!(
                *wrapper.transport.cancellations.lock().unwrap(),
                ["invocation"]
            );
        }
        let wrapper = wrapper(vec![PluginStep::Ready, host_call(1), host_call(1)], false);
        assert!(block_on(wrapper.invoke(&plugin(), &call())).is_err());
        assert_eq!(*wrapper.services.calls.lock().unwrap(), 1);
        assert_eq!(
            *wrapper.transport.cancellations.lock().unwrap(),
            ["invocation"]
        );
    }
    #[test]
    fn dropping_an_inflight_host_call_cancels_its_host_invocation() {
        let wrapper = wrapper(vec![PluginStep::Ready, host_call(1)], true);
        let plugin = plugin();
        let call = call();
        let mut invocation = Box::pin(wrapper.invoke(&plugin, &call));
        assert!(invocation.as_mut().now_or_never().is_none());
        drop(invocation);
        assert_eq!(
            *wrapper.transport.cancellations.lock().unwrap(),
            ["invocation"]
        );
    }
    #[test]
    fn manifest_approval_requirements_cannot_weaken_host_policy() {
        use crate::PermissionGate;
        struct Gate;
        impl PermissionGate for Gate {
            fn approve(&self, _: &ToolCall) -> impl Future<Output = bool> + Send {
                std::future::ready(true)
            }
        }
        let mut plugin = plugin();
        let gate = PluginGate {
            gate: Gate,
            plugins: Arc::new(vec![plugin.clone()]),
        };
        // Unknown plugin tools remain subject to the host's default-deny policy.
        assert!(gate.needs_approval(&call()));
        plugin.manifest.contributions.tools[0].name = "memory_read".into();
        plugin.manifest.contributions.tools[0].requires_approval = true;
        let call = ToolCall {
            name: "memory_read".into(),
            ..call()
        };
        assert!(!Gate.needs_approval(&call));
        let gate = PluginGate {
            gate: Gate,
            plugins: Arc::new(vec![plugin]),
        };
        assert!(gate.needs_approval(&call));
    }
    #[test]
    fn shared_services_attach_pinned_authority_without_exposing_it_to_plugins() {
        struct Host(Mutex<Vec<openwebide_core::plugins::execution::PluginHostRequest>>);
        impl GrantedHost for Host {
            async fn request(
                &self,
                request: &openwebide_core::plugins::execution::PluginHostRequest,
            ) -> Result<String, String> {
                self.0.lock().unwrap().push(request.clone());
                Ok("{}".into())
            }
        }
        let plugin = plugin();
        let services = GrantedServices {
            host: Host(Mutex::new(Vec::new())),
            grants: Arc::new(
                [(plugin.digest.clone(), "opaque-grant".into())]
                    .into_iter()
                    .collect(),
            ),
        };
        block_on(services.request(&plugin, "records", "{\"operation\":{\"action\":\"list\"}}"))
            .unwrap();
        let mut other = plugin.clone();
        other.digest = "c".repeat(64);
        assert!(block_on(services.request(&other, "records", "{}")).is_err());
        let calls = services.host.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].grant, "opaque-grant");
        assert_eq!(calls[0].capability, "records");
        assert!(!calls[0].payload.contains("opaque-grant"));
    }
}
