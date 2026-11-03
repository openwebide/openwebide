//! Shared pinned-provider execution policy for both project host adapters.
use super::actions::{PluginActionHost, invoke_selected_action};
use super::execution::{GrantedHost, PluginTransport};
use openwebide_core::plugins::execution::{
    ContinuePlugin, InvokePlugin, PluginHostRequest, PluginInvocation,
};
use openwebide_core::plugins::{
    PreparedPlugin, ProjectPlugin, execution::PluginOperation, languages::*,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
mod documents;
pub use documents::LanguageDocumentRequest;
#[cfg(test)]
mod tests;

/// One matching provider owns preparation, authority and recovery policy above
/// both transports. Dropping a request retains invocation cancellation guards.
#[derive(Clone)]
pub struct LanguageService<H> {
    host: CachedHost<H>,
    bindings: Arc<[ProjectPlugin]>,
    provider: Arc<RegisteredLanguage>,
    sequence: Arc<AtomicU64>,
}
#[derive(Default)]
struct ServiceState {
    prepared: Option<PreparedPlugin>,
    execution_host: Option<String>,
    failures: u32,
    retry_after: u64,
    documents: std::collections::BTreeMap<String, documents::ClientDocument>,
}
impl ServiceState {
    fn failed(&mut self, now_ms: u64, reprepare: bool) {
        if reprepare {
            self.prepared = None;
        }
        self.documents.clear();
        self.failures = self.failures.saturating_add(1);
        let delay = 1000_u64
            .saturating_mul(1_u64 << self.failures.min(5).saturating_sub(1))
            .min(30_000);
        self.retry_after = now_ms.saturating_add(delay);
    }
}
#[derive(Clone)]
struct CachedHost<H> {
    host: H,
    state: Arc<Mutex<ServiceState>>,
}
impl<H: PluginActionHost> LanguageService<H> {
    pub fn new(
        host: H,
        bindings: Vec<ProjectPlugin>,
        provider: RegisteredLanguage,
    ) -> Result<Self, String> {
        select_language_provider(&bindings, &provider)?;
        Ok(Self {
            host: CachedHost {
                host,
                state: Arc::default(),
            },
            bindings: bindings.into(),
            provider: Arc::new(provider),
            sequence: Arc::default(),
        })
    }
    pub async fn request(
        &self,
        input: &LanguageInput,
        source: Option<&str>,
        now_ms: u64,
    ) -> Result<LanguageOutput, String> {
        if !self.host.is_current() {
            return Err("Language service context changed".into());
        }
        if self.host.state.lock().unwrap().retry_after > now_ms {
            return Err("Language service is waiting to retry after a host failure".into());
        }
        let output =
            invoke_language(&self.host, &self.bindings, &self.provider, input, source).await;
        let mut state = self.host.state.lock().unwrap();
        if output.is_ok() {
            state.failures = 0;
            state.retry_after = 0;
        } else {
            state.failed(self.host.host.now_ms().unwrap_or(now_ms), true);
        }
        output
    }
}

impl<H: PluginActionHost> PluginActionHost for CachedHost<H> {
    async fn prepare(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        if let Some(prepared) = self.state.lock().unwrap().prepared.clone() {
            super::execution::validate_prepared_selection(plugin, &prepared)?;
            return Ok(prepared);
        }
        let prepared = self.host.prepare(plugin).await?;
        super::execution::validate_prepared_selection(plugin, &prepared)?;
        if !self.host.is_current() {
            return Err("Language execution host changed".into());
        }
        let mut state = self.state.lock().unwrap();
        if state
            .execution_host
            .as_ref()
            .is_some_and(|host| host != &prepared.host_id)
        {
            return Err("Language execution host changed; activate a new scoped service".into());
        }
        state.execution_host = Some(prepared.host_id.clone());
        state.prepared = Some(prepared.clone());
        Ok(prepared)
    }
    async fn grant(&self, plugin: &PreparedPlugin) -> Result<String, String> {
        // Fresh authority validates current DB enablement in both transports;
        // paired-host invocation starts do not otherwise pass through the DB.
        let grant = self.host.grant(plugin).await?;
        if !self.host.is_current() {
            return Err("Language service context changed".into());
        }
        Ok(grant)
    }
    fn is_current(&self) -> bool {
        self.host.is_current()
    }
}
impl<H: PluginActionHost> PluginTransport for CachedHost<H> {
    async fn ensure_prepared(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.prepare(plugin).await
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
    async fn flush_cancellations(&self) {
        self.host.flush_cancellations().await;
    }
}
impl<H: PluginActionHost> GrantedHost for CachedHost<H> {
    async fn request(&self, request: &PluginHostRequest) -> Result<String, String> {
        self.host.request(request).await
    }
}

pub fn select_language_provider<'a>(
    bindings: &'a [ProjectPlugin],
    expected: &RegisteredLanguage,
) -> Result<&'a PreparedPlugin, String> {
    let registry = LanguageRegistry::from_bindings(bindings);
    let selected = registry
        .find(&expected.namespace)
        .ok_or("Language provider is unavailable or disabled")?;
    if selected != expected {
        return Err("Language provider changed; refresh the document service".into());
    }
    bindings
        .iter()
        .find(|binding| {
            binding.enabled
                && binding.prepared.storage_namespace() == selected.plugin_namespace
                && binding.prepared.digest == selected.digest
                && binding.revision == selected.installation_revision
                && binding.prepared.host_id == selected.host_id
                && binding.prepared.manifest.executable.is_some()
        })
        .map(|binding| &binding.prepared)
        .ok_or_else(|| "Language provider has no executable service".into())
}

pub async fn invoke_language<H: PluginActionHost>(
    host: &H,
    bindings: &[ProjectPlugin],
    expected: &RegisteredLanguage,
    input: &LanguageInput,
    source: Option<&str>,
) -> Result<LanguageOutput, String> {
    validate_input(input)?;
    if let LanguageInput::Analyze(request) = input {
        if request.language != expected.definition.id {
            return Err("Language request does not match its selected provider".into());
        }
        if let Some(source) = source
            && (source.len() > MAX_SOURCE_BYTES || source_digest(source) != request.source_digest)
        {
            return Err("Language request source changed".into());
        }
    }
    let prepared = select_language_provider(bindings, expected)?;
    let arguments = serde_json::to_string(input).map_err(|error| error.to_string())?;
    let outcome = invoke_selected_action(
        host,
        prepared,
        PluginOperation::Language,
        SERVICE_HANDLER,
        &arguments,
    )
    .await?;
    if !outcome.ok {
        return Err("Language provider failed".into());
    }
    if outcome.content.len() > MAX_MESSAGE_BYTES {
        return Err("Language result exceeds its byte limit".into());
    }
    let output = serde_json::from_str(&outcome.content)
        .map_err(|_| "Language provider returned an invalid result")?;
    validate_output(input, &output, source)?;
    Ok(output)
}
