//! Read-only agent queries over the same provider and language-service facade as
//! the editor. Hosts supply current authority and transport, never selection.
use crate::ToolOutcome;
use crate::plugins::{
    actions::PluginActionHost,
    languages::{IntelligenceDocuments, LanguageDocumentRequest, LanguageService},
};
use openwebide_core::{
    Vfs,
    plugins::languages::{
        intelligence::{self as protocol, Query},
        tools::*,
        *,
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

pub trait AgentLanguageHost: PluginActionHost {
    fn language_context(&self) -> impl Future<Output = Result<LanguageToolContext, String>> + Send;
}

/// Object-safe attachment to the universal VFS executor.
pub trait CodeIntelligence: Send + Sync {
    fn query<'a>(
        &'a self,
        files: &'a dyn Vfs,
        args: &'a LanguageToolArgs,
        budget: usize,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;
}

pub struct LanguageTools<H> {
    host: H,
    scope: LanguageToolScope,
    client: String,
    sequence: Arc<AtomicU64>,
    revision: AtomicU64,
    documents: IntelligenceDocuments,
    cache: Mutex<Option<Services<H>>>,
    // Agent batches can contain concurrent calls; a client must not close a
    // dependency while another call still reads its source-bound identities.
    active: futures::lock::Mutex<()>,
}
struct Services<H> {
    context: LanguageToolContext,
    entries: BTreeMap<String, LanguageService<H>>,
}
impl<H: AgentLanguageHost> LanguageTools<H> {
    /// The runtime supplies a cryptographically random nonce for every client.
    /// Editor and other-device overlays remain separate. Tasks sharing this
    /// instance share its serialized, read-only workspace inventory.
    pub fn new(host: H, scope: LanguageToolScope, nonce: u128) -> Self {
        Self {
            host,
            scope,
            client: format!("agent-{nonce:032x}"),
            sequence: Arc::default(),
            revision: AtomicU64::new(0),
            documents: IntelligenceDocuments::default(),
            cache: Mutex::new(None),
            active: futures::lock::Mutex::new(()),
        }
    }
    async fn execute(
        &self,
        files: &dyn Vfs,
        args: &LanguageToolArgs,
        budget: usize,
    ) -> Result<String, String> {
        let query = args.query()?;
        let _active = self.active.lock().await;
        if !self.host.is_current() {
            return Err("Agent language context changed".into());
        }
        let context = self.host.language_context().await?;
        if context.scope != self.scope {
            return Err("The session's project or workspace changed; start a new run".into());
        }
        context.preferences.validate()?;
        let path = files
            .canonicalize(&args.path)
            .await
            .map_err(|error| error.to_string())?;
        if !protocol::relative_path(&path) {
            return Err("Language queries require a workspace file".into());
        }
        let source = files.read(&path).await.map_err(|error| error.to_string())?;
        if source.len() > MAX_SOURCE_BYTES {
            return Err("File exceeds the language-service source limit".into());
        }
        let registry = LanguageRegistry::from_bindings(&context.bindings);
        let selected = match registry.select_server(&path, &source, None, &context.preferences) {
            LanguageSelection::Selected(provider) => provider,
            LanguageSelection::Plain if args.operation == LanguageToolOperation::Diagnostics => {
                registry
                    .diagnostic_providers(&path, &source, None, &context.preferences)?
                    .first()
                    .copied()
                    .ok_or("No enabled diagnostics provider is installed for this file")?
            }
            LanguageSelection::Plain => {
                return Err("No enabled language server is installed for this file".into());
            }
            LanguageSelection::Ambiguous(_) => {
                return Err(
                    "Choose a language provider in project settings before querying this file"
                        .into(),
                );
            }
            LanguageSelection::Unavailable(_) => {
                return Err("The preferred language provider is unavailable".into());
            }
        };
        let service = {
            let mut cache = self.cache.lock().unwrap();
            if cache.as_ref().is_none_or(|cache| cache.context != context) {
                *cache = Some(Services {
                    context: context.clone(),
                    entries: BTreeMap::new(),
                });
            }
            let cache = cache.as_mut().unwrap();
            if !cache.entries.contains_key(&selected.namespace) {
                if cache.entries.len() >= 16 {
                    return Err("Agent language-service limit reached; start a new run".into());
                }
                cache.entries.insert(
                    selected.namespace.clone(),
                    LanguageService::new_with_preferences(
                        self.host.clone(),
                        context.bindings.clone(),
                        selected.clone(),
                        context.preferences.clone(),
                    )?
                    .with_sequence(Arc::clone(&self.sequence)),
                );
            }
            cache.entries[&selected.namespace].clone()
        };
        let revision = self
            .revision
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or("Agent language revision exhausted")?;
        let digest = source_digest(&source);
        let document = LanguageDocumentRequest {
            document: source_digest(&path),
            path: path.clone(),
            source: Arc::new(source),
            revision,
            tab_width: 4,
        };
        let mut records = Vec::new();
        let ready;
        if args.operation == LanguageToolOperation::Diagnostics {
            let reports = service
                .query_diagnostics(
                    self.scope.project,
                    &self.client,
                    document.clone(),
                    &[],
                    &self.documents,
                    false,
                )
                .await?;
            ready = reports
                .iter()
                .all(|report| report.ready && report.error.is_none());
            for report in reports {
                records.push(json!({"kind":"provider","provider":report.provider.namespace,"label":report.provider.plugin_label,"digest":report.provider.digest,"installation_revision":report.provider.installation_revision,"server":report.server,"ready":report.ready,"error":report.error}));
                for diagnostic in report.diagnostics {
                    records.push(json!({"kind":"diagnostic","provider":report.provider.namespace,"diagnostic":diagnostic}));
                }
            }
        } else {
            service
                .synchronize_intelligence(
                    self.scope.project,
                    &self.client,
                    &document,
                    &[],
                    &self.documents,
                )
                .await?;
            let response = service
                .query_intelligence(&self.client, document.clone(), query.clone())
                .await?;
            ready = true;
            records.push(json!({"kind":"provider","provider":selected.namespace,"label":selected.plugin_label,"digest":selected.digest,"installation_revision":selected.installation_revision,"server":response.server}));
            records.extend(result_records(&query, response.result, args, budget)?);
        }
        // Neither stale DB authority nor external file changes may be presented
        // as current results, even if the transport completed successfully.
        if !self.host.is_current()
            || self.host.language_context().await? != context
            || files
                .canonicalize(&args.path)
                .await
                .map_err(|error| error.to_string())?
                != path
            || source_digest(&files.read(&path).await.map_err(|error| error.to_string())?) != digest
        {
            return Err(
                "Workspace source or language provider changed during the query; retry".into(),
            );
        }
        bounded_output(args.operation, &path, &digest, ready, records, budget)
    }
}
impl<H: AgentLanguageHost> CodeIntelligence for LanguageTools<H> {
    fn query<'a>(
        &'a self,
        files: &'a dyn Vfs,
        args: &'a LanguageToolArgs,
        budget: usize,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            match self.execute(files, args, budget).await {
                Ok(content) => ToolOutcome {
                    summary: format!("inspect code intelligence for {}", args.path),
                    ok: true,
                    content,
                    diff: None,
                },
                Err(error) => {
                    let mut content = format!("Code intelligence: {error}");
                    if content.len() > budget {
                        content.truncate(content.floor_char_boundary(budget.saturating_sub(3)));
                        if budget >= 3 {
                            content.push('…');
                        }
                    }
                    ToolOutcome {
                        summary: format!("inspect code intelligence for {}", args.path),
                        ok: false,
                        content,
                        diff: None,
                    }
                }
            }
        })
    }
}

fn result_records(
    query: &Query,
    result: Value,
    args: &LanguageToolArgs,
    budget: usize,
) -> Result<Vec<Value>, String> {
    fn targets(value: &Value) -> Result<(), String> {
        let target: protocol::results::Target =
            serde_json::from_value(value.clone()).map_err(|_| "Invalid navigation target")?;
        target.validate()
    }
    let values = match query {
        Query::Definition { .. } | Query::References { .. } | Query::ResolveSymbol { .. } => {
            let values = result.as_array().ok_or("Invalid navigation result")?;
            for value in values {
                targets(value)?;
            }
            values.clone()
        }
        Query::Symbols => {
            let symbols: Vec<protocol::results::Symbol> =
                serde_json::from_value(result.clone()).map_err(|_| "Invalid document symbols")?;
            for symbol in symbols {
                symbol.target.validate()?;
            }
            result.as_array().unwrap().clone()
        }
        Query::WorkspaceSymbols { .. } => {
            let values = result
                .as_array()
                .ok_or("Invalid workspace symbols")?
                .clone();
            for value in &values {
                let symbol: protocol::results::WorkspaceSymbol =
                    serde_json::from_value(value.clone())
                        .map_err(|_| "Invalid workspace symbol")?;
                if let Some(target) = symbol.target {
                    target.validate()?;
                } else if symbol
                    .id
                    .as_ref()
                    .is_none_or(|id| id.is_empty() || id.len() > 256)
                {
                    return Err("Workspace symbol has no location or resolution ID".into());
                }
            }
            values
        }
        Query::ReadResource { .. } => {
            let source = result["source"]
                .as_str()
                .ok_or("Invalid read-only resource")?;
            if result["readOnly"] != true {
                return Err("Language resource is not read-only".into());
            }
            let offset = args.offset.unwrap_or(1) as usize;
            let limit = args.limit.unwrap_or(200) as usize;
            let mut text = String::new();
            let mut count = 0;
            let mut clipped = false;
            let allowance = budget.saturating_sub(1024) / 6; // worst-case JSON escaping
            let total = source.split('\n').count();
            for line in source.split('\n').skip(offset - 1).take(limit) {
                let available = allowance.saturating_sub(text.len());
                if line.len().saturating_add(1) > available {
                    text.push_str(&line[..line.floor_char_boundary(available)]);
                    clipped = true;
                    break;
                }
                text.push_str(line);
                text.push('\n');
                count += 1;
            }
            vec![
                json!({"name":result["name"],"read_only":true,"source_digest":source_digest(source),"offset":offset,"lines":count,"total_lines":total,"source":text,"truncated":clipped || offset.saturating_add(count) <= total,"clipped_line":clipped}),
            ]
        }
        _ => return Err("This agent language operation is unavailable".into()),
    };
    if values.len() > protocol::MAX_RECORDS {
        return Err("Too many language-query results".into());
    }
    Ok(values)
}

fn bounded_output(
    operation: LanguageToolOperation,
    path: &str,
    digest: &str,
    ready: bool,
    records: Vec<Value>,
    budget: usize,
) -> Result<String, String> {
    let total = records.len();
    let mut output = json!({"operation":operation,"path":path,"source_digest":digest,"source":"workspace_files","positions":"zero_based_utf16","ready":ready,"records":[],"omitted":total,"truncated":false});
    let mut used = serde_json::to_string(&output)
        .map_err(|error| error.to_string())?
        .len();
    for record in records {
        let size = serde_json::to_string(&record)
            .map_err(|error| error.to_string())?
            .len()
            + 1;
        if used.saturating_add(size) > budget {
            continue;
        }
        used += size;
        output["records"].as_array_mut().unwrap().push(record);
    }
    let omitted = total - output["records"].as_array().unwrap().len();
    output["omitted"] = json!(omitted);
    output["truncated"] = json!(omitted > 0);
    let output = serde_json::to_string(&output).map_err(|error| error.to_string())?;
    if output.len() > budget {
        return Err("Code-intelligence metadata exceeds the tool output budget".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests;
