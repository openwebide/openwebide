//! Project-host selection and thin UI transports for the shared plugin action policy.
use crate::{
    backend::{Api, Backend},
    plugin_bridge::PluginBridgeClient,
    project_host::{PluginExecutionHost, ProjectHost},
};
use leptos::prelude::*;
use openwebide_agent::plugins::{
    actions::PluginActionHost,
    execution::{GrantedHost, PluginTransport},
};
use openwebide_core::{
    ToolCall,
    plugins::{PreparedPlugin, execution::*},
};
use send_wrapper::SendWrapper;
use std::{
    collections::BTreeMap,
    rc::Rc,
    sync::{Arc, Mutex},
};

pub async fn invoke_project_plugin_action(
    api: Api,
    host: ProjectHost,
    project: i64,
    call: &ToolCall,
    approved: bool,
    current: impl Fn() -> bool + Clone + 'static,
) -> Result<openwebide_agent::ToolOutcome, String> {
    invoke_scoped_plugin_action(api, host, Some(project), call, approved, current).await
}

pub async fn invoke_scoped_plugin_action(
    api: Api,
    host: ProjectHost,
    project: Option<i64>,
    call: &ToolCall,
    approved: bool,
    current: impl Fn() -> bool + Clone + 'static,
) -> Result<openwebide_agent::ToolOutcome, String> {
    let revision = host.revision();
    let current = move || current() && host.revision() == revision;
    if !current() {
        return Err("Plugin action context changed".into());
    }
    let backend = api
        .try_with_value(Clone::clone)
        .ok_or("Plugin action context changed")?;
    let bindings = match project {
        Some(project) => backend.project_plugins(project).await?,
        None => openwebide_core::plugins::default_bindings(&backend.plugin_installations().await?),
    };
    openwebide_agent::plugins::actions::select_plugin_action(&bindings, call, approved)?;
    let execution = host.plugin_host(project, current.clone())?;
    let transport = match execution {
        PluginExecutionHost::Remote {
            api, project_id, ..
        } => Transport::Remote(api, project_id),
        PluginExecutionHost::Local(client) => Transport::Local(client),
    };
    let adapter = ActionHost {
        api: SendWrapper::new(api),
        transport: SendWrapper::new(transport),
        context: PluginExecutionContext {
            user_action: true,
            project_id: project,
            session_id: None,
            primary: None,
        },
        current: SendWrapper::new(Rc::new(current)),
        grants: Arc::default(),
    };
    openwebide_agent::plugins::actions::invoke_plugin_action(&adapter, &bindings, call, approved)
        .await
}

#[derive(Clone)]
enum Transport {
    Remote(Api, Option<i64>),
    Local(PluginBridgeClient),
}
#[derive(Clone)]
struct ActionHost {
    api: SendWrapper<Api>,
    transport: SendWrapper<Transport>,
    context: PluginExecutionContext,
    current: SendWrapper<Rc<dyn Fn() -> bool>>,
    grants: Arc<Mutex<BTreeMap<String, String>>>,
}
impl ActionHost {
    fn backend(&self) -> Result<Rc<dyn Backend>, String> {
        self.api
            .try_with_value(Clone::clone)
            .ok_or_else(|| "Plugin action context changed".into())
    }
}
impl PluginActionHost for ActionHost {
    async fn prepare(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        SendWrapper::new(async move {
            let transport = match &*self.transport {
                Transport::Remote(api, project) => {
                    crate::project_plugins::PluginTransport::Remote(*api, *project)
                }
                Transport::Local(client) => {
                    crate::project_plugins::PluginTransport::Local(client.clone())
                }
            };
            crate::project_plugins::prepare_on_host(
                transport,
                &plugin.source,
                || (self.current)(),
                || false,
                |_| {},
            )
            .await
        })
        .await
    }
    async fn grant(&self, plugin: &PreparedPlugin) -> Result<String, String> {
        SendWrapper::new(async move {
            let grants = self
                .backend()?
                .plugin_context_grants(&PluginGrantRequest {
                    context: self.context.clone(),
                    plugins: vec![plugin.clone()],
                })
                .await?;
            let grant = grants
                .get(&plugin.digest)
                .filter(|grant| !grant.is_empty())
                .ok_or("Plugin action authority is unavailable")?
                .clone();
            self.grants
                .lock()
                .unwrap()
                .insert(plugin.digest.clone(), grant.clone());
            Ok(grant)
        })
        .await
    }
    fn is_current(&self) -> bool {
        (self.current.as_ref())()
    }
}
impl PluginTransport for ActionHost {
    async fn ensure_prepared(&self, expected: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        <Self as PluginActionHost>::prepare(self, expected).await
    }

    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        SendWrapper::new(async move {
            match &*self.transport {
                Transport::Remote(..) => {
                    let grant = self
                        .grants
                        .lock()
                        .unwrap()
                        .get(&call.prepared.digest)
                        .cloned()
                        .ok_or("Plugin action authority is unavailable")?;
                    self.backend()?
                        .start_plugin_invocation(&PluginStartRequest {
                            grant,
                            session_id: self.context.session_id,
                            call,
                        })
                        .await
                }
                Transport::Local(client) => {
                    client
                        .plugin_request("invoke", serde_json::json!({"call":call}))
                        .await
                }
            }
        })
        .await
    }
    async fn resume(&self, continuation: ContinuePlugin) -> Result<PluginInvocation, String> {
        SendWrapper::new(async move {
            match &*self.transport {
                Transport::Remote(..) => {
                    self.backend()?
                        .continue_plugin_invocation(&continuation)
                        .await
                }
                Transport::Local(client) => {
                    client
                        .plugin_request(
                            "continue",
                            serde_json::json!({"continuation":continuation}),
                        )
                        .await
                }
            }
        })
        .await
    }
    fn cancel(&self, id: String) {
        let host = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match &*host.transport {
                Transport::Remote(..) => {
                    if let Ok(backend) = host.backend() {
                        let _ = backend.cancel_plugin_invocation(&id).await;
                    }
                }
                Transport::Local(client) => {
                    let _ = client
                        .plugin_request::<serde_json::Value>("cancel", serde_json::json!({"id":id}))
                        .await;
                }
            }
        });
    }
}
impl GrantedHost for ActionHost {
    async fn request(&self, request: &PluginHostRequest) -> Result<String, String> {
        SendWrapper::new(async move { self.backend()?.plugin_context_host_request(request).await })
            .await
    }
}
