//! One scoped panel handler workflow for server and paired execution hosts.
use super::actions::{PluginActionHost, invoke_selected_action};
use openwebide_core::plugins::{
    PreparedPlugin, ProjectPlugin,
    execution::PluginOperation,
    panels::{coordination::*, *},
};
use std::future::Future;

pub trait PanelActionHost: PluginActionHost {
    fn begin_panel(
        &self,
        plugin: &PreparedPlugin,
        input: &PanelInput,
        approved: bool,
    ) -> impl Future<Output = Result<PanelClaim, String>> + Send;
    fn finish_panel(
        &self,
        request: &FinishPanel,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn close_panel(&self, instance: &str) -> impl Future<Output = Result<(), String>> + Send;
}

pub async fn invoke_panel<H: PanelActionHost>(
    host: &H,
    bindings: &[ProjectPlugin],
    plugin_namespace: &str,
    expected_digest: &str,
    input: &PanelInput,
    approved: bool,
) -> Result<PanelDocument, String> {
    let mut candidates = bindings.iter().filter(|binding| {
        binding.enabled && binding.prepared.storage_namespace() == plugin_namespace
    });
    let binding = candidates.next().ok_or("Panel plugin is not enabled")?;
    if candidates.next().is_some() {
        return Err("Panel plugin identity is ambiguous".into());
    }
    if binding.prepared.digest != expected_digest {
        return Err("Panel plugin changed; refresh plugins before trying again".into());
    }
    let panel = binding
        .prepared
        .manifest
        .contributions
        .panels
        .iter()
        .find(|panel| panel.id == input.panel)
        .ok_or("Panel is not declared by this plugin")?;
    validate_input(panel, input, approved)?;
    if !host.is_current() {
        return Err("Panel context changed".into());
    }
    let claim = host.begin_panel(&binding.prepared, input, approved).await?;
    if !host.is_current() {
        let _ = host.close_panel(&input.instance).await;
        return Err("Panel context changed".into());
    }
    match claim {
        PanelClaim::Pending => {
            return Err(
                "This panel request is still active; interrupted actions are not replayed".into(),
            );
        }
        PanelClaim::Failed { message } => return Err(message),
        PanelClaim::Completed { document } => {
            validate_document(panel, &document)?;
            if input.revision.checked_add(1) != Some(document.revision) {
                return Err("Panel receipt revision changed".into());
            }
            return Ok(document);
        }
        PanelClaim::Claimed { input: canonical } if canonical != *input => {
            let _ = host.close_panel(&input.instance).await;
            return Err("Panel execution claim does not match its inputs".into());
        }
        PanelClaim::Claimed { .. } => {}
    }
    let result = async {
        let outcome = invoke_selected_action(
            host,
            &binding.prepared,
            PluginOperation::Panel,
            VIEW_HANDLER,
            &serde_json::to_string(input).map_err(|error| error.to_string())?,
        )
        .await?;
        if !outcome.ok {
            return Err(outcome.content);
        }
        let document: PanelDocument = serde_json::from_str(&outcome.content)
            .map_err(|_| "Plugin returned an invalid panel document")?;
        validate_document(panel, &document)?;
        if input.revision.checked_add(1) != Some(document.revision) {
            return Err("Panel returned a stale or invalid view revision".into());
        }
        Ok(document)
    }
    .await;
    if !host.is_current() {
        let _ = host.close_panel(&input.instance).await;
        return Err("Panel context changed".into());
    }
    host.finish_panel(&FinishPanel {
        instance: input.instance.clone(),
        request: input.request.clone(),
        outcome: result.clone(),
    })
    .await?;
    if !host.is_current() {
        return Err("Panel context changed".into());
    }
    result
}
