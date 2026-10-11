//! UI action policy over the same executor used by agent tools and host events.
use super::execution::{GrantedHost, GrantedServices, PluginTransport, invoke_plugin};
use crate::ToolOutcome;
use openwebide_core::{
    ToolCall,
    plugins::{
        PreparedPlugin, ProjectPlugin,
        execution::{
            ContinuePlugin, InvokePlugin, PluginHostRequest, PluginInvocation, PluginOperation,
        },
    },
};
use std::{collections::BTreeMap, future::Future, sync::Arc};

/// Adapters provide preparation, authority and transport, never feature behavior.
pub trait PluginActionHost: PluginTransport + GrantedHost {
    fn prepare(
        &self,
        plugin: &PreparedPlugin,
    ) -> impl Future<Output = Result<PreparedPlugin, String>> + Send;
    fn grant(&self, plugin: &PreparedPlugin)
    -> impl Future<Output = Result<String, String>> + Send;
    fn is_current(&self) -> bool;
}

pub async fn invoke_plugin_action<H: PluginActionHost>(
    host: &H,
    bindings: &[ProjectPlugin],
    call: &ToolCall,
    approved: bool,
) -> Result<ToolOutcome, String> {
    current(host)?;
    let selected = select_plugin_action(bindings, call, approved)?;
    let prepared = host.prepare(selected).await?;
    current(host)?;
    super::execution::validate_prepared_selection(selected, &prepared)?;
    let grant = host.grant(&prepared).await?;
    current(host)?;
    if grant.is_empty() {
        return Err("Plugin action authority is unavailable".into());
    }
    let guarded = GuardedHost(host.clone());
    let services = GrantedServices {
        host: guarded.clone(),
        grants: Arc::new(BTreeMap::from([(prepared.digest.clone(), grant)])),
    };
    let outcome = invoke_plugin(
        &guarded,
        &services,
        InvokePlugin {
            operation: PluginOperation::Tool,
            prepared,
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        },
    )
    .await?;
    current(host)?;
    Ok(outcome)
}
/// Validate declared action ownership before a caller resolves an execution host.
pub fn select_plugin_action<'a>(
    bindings: &'a [ProjectPlugin],
    call: &ToolCall,
    approved: bool,
) -> Result<&'a PreparedPlugin, String> {
    let mut candidates = bindings.iter().filter_map(|binding| {
        if !binding.enabled || binding.prepared.manifest.executable.is_none() {
            return None;
        }
        binding
            .prepared
            .manifest
            .contributions
            .tools
            .iter()
            .find(|tool| tool.name == call.name)
            .map(|tool| (&binding.prepared, tool))
    });
    let (selected, tool) = candidates
        .next()
        .ok_or("No enabled plugin provides this action")?;
    if candidates.next().is_some() {
        return Err("More than one enabled plugin provides this action".into());
    }
    if tool.requires_approval && !approved {
        return Err("This plugin action requires approval".into());
    }
    if call.arguments.len() > 4 * 1024 * 1024 {
        return Err("Plugin action input exceeds its limit".into());
    }
    Ok(selected)
}
fn current(host: &impl PluginActionHost) -> Result<(), String> {
    if host.is_current() {
        Ok(())
    } else {
        Err("Plugin action context changed".into())
    }
}

#[derive(Clone)]
struct GuardedHost<H>(H);
#[cfg(test)]
mod tests;
impl<H: PluginActionHost> PluginTransport for GuardedHost<H> {
    async fn ensure_prepared(&self, expected: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        current(&self.0)?;
        // Action policy already prepared and validated before issuing authority.
        Ok(expected.clone())
    }

    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        current(&self.0)?;
        self.0.start(call).await
    }
    async fn resume(&self, call: ContinuePlugin) -> Result<PluginInvocation, String> {
        current(&self.0)?;
        self.0.resume(call).await
    }
    fn cancel(&self, id: String) {
        self.0.cancel(id);
    }
    async fn flush_cancellations(&self) {
        self.0.flush_cancellations().await;
    }
}
impl<H: PluginActionHost> GrantedHost for GuardedHost<H> {
    async fn request(&self, request: &PluginHostRequest) -> Result<String, String> {
        current(&self.0)?;
        self.0.request(request).await
    }
}
