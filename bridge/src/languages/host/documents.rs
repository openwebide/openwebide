use super::*;
use openwebide_core::plugins::languages::intelligence::Diagnostic;
use std::path::{Path, PathBuf};

const MAX_DOCUMENTS: usize = 64;
const MAX_SOURCES: usize = 32 * 1024 * 1024;
const MAX_ACTIONS: usize = 128;
const MAX_ACTION_BYTES: usize = 256 * 1024;
const MAX_RESOURCE_BYTES: usize = 256 * 1024;
mod results;
mod reuse;
mod symbols;

#[derive(Default)]
pub(super) struct Documents {
    documents: BTreeMap<String, Document>,
    version: i32,
    overlay_revision: u64,
    analysis_generation: Option<generation::Observation>,
    watermarks: BTreeMap<String, (u64, u64, Instant)>,
}
struct Document {
    path: PathBuf,
    uri: String,
    language: String,
    revision: u64,
    request: u64,
    digest: String,
    source: Arc<str>,
    version: i32,
    overlay_revision: u64,
    actions: BTreeMap<String, RetainedAction>,
    completions: BTreeMap<String, Value>,
    resources: BTreeMap<String, Resource>,
    symbols: BTreeMap<String, Value>,
}
enum Resource {
    File(PathBuf),
    Virtual { uri: String, name: String },
}
#[derive(Clone)]
struct RetainedAction {
    value: Value,
    created: Instant,
}
impl Resource {
    fn bytes(&self) -> usize {
        match self {
            Self::File(path) => path.as_os_str().len(),
            Self::Virtual { uri, name } => uri.len() + name.len(),
        }
    }
}
pub(super) struct Diagnostics {
    pub version: i32,
    pub minimum_version: i32,
    pub raw: Vec<Value>,
    pub ready: bool,
}
impl Documents {
    pub(super) fn overlay_revision(&self) -> u64 {
        self.overlay_revision
    }
    pub(super) fn observe_generation(&mut self, observation: generation::Observation) {
        if self.analysis_generation != Some(observation) {
            for document in self.documents.values_mut() {
                document.actions.clear();
                document.completions.clear();
                document.resources.clear();
                document.symbols.clear();
            }
            self.analysis_generation = Some(observation);
        }
    }
    pub(super) fn invalidate_generation(&mut self) {
        self.analysis_generation = None;
        // Unmeasurable coverage cannot authorize edits. Retain read-only
        // resource/navigation capabilities, whose existing scope and source
        // checks still apply, so optional observation cannot remove navigation.
        for document in self.documents.values_mut() {
            document.actions.clear();
            document.completions.clear();
        }
    }
    pub(super) fn validate_analysis(
        &mut self,
        session: &Session,
        input: &Request,
        path: &Path,
        context: &protocol::AnalysisContext,
        observation: generation::Observation,
    ) -> Result<(Response, analysis::affected::Snapshot), String> {
        self.admit(input)?;
        if !self.has_source(input, path) {
            return Err("The original analysis document changed or closed".into());
        }
        let affected = session.analysis.verify_at(
            context,
            &session.id,
            input,
            observation,
            self.overlay_revision,
            &session.root,
        )?;
        self.documents.get_mut(&input.document).unwrap().request = input.request;
        Ok((
            response(session, input, Value::Null, vec![], true),
            affected,
        ))
    }
    async fn open_document(session: &Session, document: &Document) -> Result<(), String> {
        // Opening an overlay can leave a server's previously indexed directory
        // unchanged. Follow the open with a full content synchronization so
        // restored drafts and newly opened files invalidate that index too.
        // Both messages carry the exact source; no filesystem write or Save is
        // implied. Both versions describe identical source; diagnostics can use
        // either until the server publishes the later version. Edit reviews
        // always use the final version. Startup requires live synchronization.
        let opening_version = document.version - 1;
        session
            .client
            .notify(
                "textDocument/didOpen",
                json!({"textDocument":{
                    "uri":document.uri,"languageId":document.language,
                    "version":opening_version,"text":document.source.as_ref()
                }}),
            )
            .await
            .map_err(|error| error.to_string())?;
        session
            .client
            .notify(
                "textDocument/didChange",
                json!({
                    "textDocument":{"uri":document.uri,"version":document.version},
                    "contentChanges":[{"text":document.source.as_ref()}]
                }),
            )
            .await
            .map_err(|error| error.to_string())
    }

    pub(super) fn workspace_documents(
        &self,
        root: &Path,
    ) -> BTreeMap<
        String,
        openwebide_core::plugins::languages::intelligence::workspace_edit::WorkspaceDocument,
    > {
        use openwebide_core::{
            editor::Document as EditorDocument,
            plugins::languages::intelligence::workspace_edit::WorkspaceDocument,
        };
        self.documents
            .values()
            .filter_map(|document| {
                results::relative(root, &document.path).map(|path| {
                    let mut value = WorkspaceDocument::new(
                        &path,
                        EditorDocument::new(document.source.as_ref()),
                    );
                    value.revision = Some(document.revision);
                    value.server_version = Some(i64::from(document.version));
                    (path, value)
                })
            })
            .collect()
    }
    pub(super) fn overlay_source(&self, path: &Path) -> Option<&str> {
        self.documents
            .values()
            .find(|document| document.path == path)
            .map(|document| document.source.as_ref())
    }
    pub(super) fn has_source(&self, input: &Request, path: &Path) -> bool {
        self.documents.get(&input.document).is_some_and(|document| {
            document.path == path
                && document.digest == input.source_digest
                && document.revision == input.revision
        })
    }
    pub(super) async fn query(
        &mut self,
        session: &Session,
        input: &Request,
        path: &Path,
        language: &str,
    ) -> Result<(Response, Option<analysis::affected::Snapshot>), String> {
        let mut affected = None;
        self.admit(input)?;
        if matches!(
            input.query,
            Query::PrepareAction { .. } | Query::ReleaseAction { .. }
        ) {
            return Ok((response(session, input, Value::Null, vec![], true), None));
        }
        if matches!(
            input.query,
            Query::Close | Query::SynchronizeAction { close: true, .. }
        ) {
            if let Some(document) = self.remove_closed(input)? {
                session.diagnostics.lock().unwrap().remove(&document.uri);
                session
                    .client
                    .notify(
                        "textDocument/didClose",
                        json!({"textDocument":{"uri":document.uri}}),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
            return Ok((response(session, input, Value::Null, vec![], true), None));
        }
        let uri = url::Url::from_file_path(path)
            .map_err(|()| "Invalid language document URI")?
            .to_string();
        if let Some(previous) = self.documents.get(&input.document) {
            if input.request <= previous.request
                || input.revision < previous.revision
                || (input.revision == previous.revision && input.source_digest != previous.digest)
            {
                return Err("Language document revision or request is stale".into());
            }
            if previous.uri != uri || previous.language != language {
                let previous = self
                    .documents
                    .remove(&input.document)
                    .expect("checked document");
                session.diagnostics.lock().unwrap().remove(&previous.uri);
                session
                    .client
                    .notify(
                        "textDocument/didClose",
                        json!({"textDocument":{"uri":previous.uri}}),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        if self
            .documents
            .iter()
            .any(|(id, document)| id != &input.document && document.uri == uri)
        {
            return Err(
                "This path already has an open language document in this editing client".into(),
            );
        }
        let previous_size = self
            .documents
            .get(&input.document)
            .map_or(0, |document| document.source.len());
        let total = self
            .documents
            .values()
            .map(|document| document.source.len())
            .sum::<usize>()
            - previous_size
            + input.source.len();
        if (!self.documents.contains_key(&input.document) && self.documents.len() >= MAX_DOCUMENTS)
            || total > MAX_SOURCES
        {
            return Err(
                "Language service document capacity is full; close unused documents".into(),
            );
        }
        let is_new = !self.documents.contains_key(&input.document);
        let source_changed = self
            .documents
            .get(&input.document)
            .is_none_or(|document| document.digest != input.source_digest);
        if is_new || source_changed {
            self.overlay_revision = self
                .overlay_revision
                .checked_add(1)
                .ok_or("Language overlay revision exhausted")?;
        }
        let document = self
            .documents
            .entry(input.document.clone())
            .or_insert_with(|| Document {
                path: path.to_path_buf(),
                uri: uri.clone(),
                language: language.to_owned(),
                revision: input.revision,
                request: 0,
                digest: String::new(),
                source: Arc::from(""),
                version: 0,
                overlay_revision: 0,
                actions: BTreeMap::new(),
                completions: BTreeMap::new(),
                resources: BTreeMap::new(),
                symbols: BTreeMap::new(),
            });
        // Some servers retain a translation unit until that document changes or
        // reopens, even when another unsaved overlay changes its dependencies.
        // Refresh once per overlay revision. A no-op didChange is insufficient
        // for servers that deliberately skip unchanged text. Keep the service
        // warm and the other unsaved documents open; never pretend a Save took
        // place or alter text to force parsing.
        let refresh =
            !is_new && !source_changed && document.overlay_revision != self.overlay_revision;
        if source_changed || is_new || refresh {
            self.version = self
                .version
                .checked_add(if is_new || refresh { 2 } else { 1 })
                .ok_or("Language document version exhausted")?;
            document.version = self.version;
            document.overlay_revision = self.overlay_revision;
            document.source = Arc::from(input.source.as_str());
            document.digest = input.source_digest.clone();
            document.actions.clear();
            document.completions.clear();
            document.resources.clear();
            document.symbols.clear();
            session.diagnostics.lock().unwrap().insert(
                uri.clone(),
                Diagnostics {
                    version: document.version,
                    minimum_version: if is_new || refresh {
                        document.version - 1
                    } else {
                        document.version
                    },
                    raw: vec![],
                    ready: false,
                },
            );
            if refresh {
                session
                    .client
                    .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                    .await
                    .map_err(|error| error.to_string())?;
            }
            if is_new || refresh {
                Self::open_document(session, document).await?;
            } else {
                session.client.notify("textDocument/didChange",json!({"textDocument":{"uri":uri,"version":document.version},"contentChanges":[{"text":input.source}]})).await.map_err(|error|error.to_string())?;
            }
        }
        document.revision = input.revision;
        document.request = input.request;
        if matches!(input.query, Query::Saved) {
            let mut params = json!({"textDocument":{"uri":uri}});
            if session
                .capabilities
                .pointer("/textDocumentSync/save/includeText")
                .and_then(Value::as_bool)
                == Some(true)
            {
                params["text"] = json!(input.source);
            }
            session
                .client
                .notify("textDocument/didSave", params)
                .await
                .map_err(|error| error.to_string())?;
        }
        if matches!(
            input.query,
            Query::Synchronize | Query::Saved | Query::SynchronizeAction { .. }
        ) && supported(&session.capabilities, "diagnosticProvider")
        {
            let diagnostics = request_current(
                &session.client,
                "textDocument/diagnostic",
                json!({"textDocument":{"uri":uri}}),
            )
            .await?;
            if diagnostics["kind"] == "full" {
                let raw = diagnostics["items"]
                    .as_array()
                    .filter(|items| items.len() <= protocol::MAX_DIAGNOSTICS)
                    .ok_or("Invalid or oversized language diagnostics")?
                    .clone();
                session.diagnostics.lock().unwrap().insert(
                    uri.clone(),
                    Diagnostics {
                        version: document.version,
                        minimum_version: document.version,
                        raw,
                        ready: true,
                    },
                );
            }
        }
        let mut result = Value::Null;
        if let Query::ReadResource { id } = &input.query {
            let resource = document
                .resources
                .get(id)
                .ok_or("Language resource is stale or belongs to another document")?;
            let (name, source) = match resource {
                Resource::File(path) => (
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                    results::read_source(path).await?,
                ),
                Resource::Virtual { uri, name } => {
                    let parsed =
                        url::Url::parse(uri).map_err(|_| "Invalid virtual document URI")?;
                    let declaration = session
                        .virtual_documents
                        .iter()
                        .find(|declaration| declaration.scheme == parsed.scheme())
                        .ok_or("This virtual document scheme is not declared")?;
                    let params = match declaration.parameters {
                        VirtualDocumentParameters::TextDocument => {
                            json!({"textDocument":{"uri":uri}})
                        }
                        VirtualDocumentParameters::Uri => json!({"uri":uri}),
                    };
                    let value =
                        request_current(&session.client, &declaration.method, params).await?;
                    let source = value.pointer(&declaration.text_pointer).and_then(Value::as_str)
                        .filter(|source| source.len() <= openwebide_core::plugins::languages::MAX_SOURCE_BYTES)
                        .ok_or("Virtual document response must contain source text within the source limit")?;
                    (name.clone(), source.to_owned())
                }
            };
            result = json!({"name":name,"source":source,"readOnly":true});
        }
        if let Some(method) = input.query.method() {
            let capability = match &input.query {
                Query::Hover { .. } => "hoverProvider",
                Query::Completion { .. } | Query::ResolveCompletion { .. } => "completionProvider",
                Query::Definition { .. } => "definitionProvider",
                Query::TypeDefinition { .. } => "typeDefinitionProvider",
                Query::Implementation { .. } => "implementationProvider",
                Query::References { .. } => "referencesProvider",
                Query::Symbols => "documentSymbolProvider",
                Query::WorkspaceSymbols { .. } | Query::ResolveSymbol { .. } => {
                    "workspaceSymbolProvider"
                }
                Query::Rename { .. } => "renameProvider",
                Query::Formatting { .. } => "documentFormattingProvider",
                Query::RangeFormatting { .. } => "documentRangeFormattingProvider",
                Query::CodeActions { .. }
                | Query::ResolveAction { .. }
                | Query::ReviewAction { .. } => "codeActionProvider",
                _ => unreachable!("query has a method"),
            };
            if !supported(&session.capabilities, capability) {
                return Err(format!("Language server does not support {method}"));
            }
            let mut params = json!({"textDocument":{"uri":uri}});
            match &input.query {
                Query::Hover { position }
                | Query::Completion { position }
                | Query::Definition { position }
                | Query::TypeDefinition { position }
                | Query::Implementation { position }
                | Query::References { position }
                | Query::Rename { position, .. } => {
                    params["position"] = json!(position);
                }
                _ => {}
            }
            match &input.query {
                Query::WorkspaceSymbols { query } => params = json!({"query":query}),
                Query::ResolveSymbol { id } => {
                    if session
                        .capabilities
                        .pointer("/workspaceSymbolProvider/resolveProvider")
                        .and_then(Value::as_bool)
                        != Some(true)
                    {
                        return Err(
                            "Language server does not support resolving workspace symbols".into(),
                        );
                    }
                    params = document
                        .symbols
                        .get(id)
                        .cloned()
                        .ok_or("Workspace symbol is stale or belongs to another document")?;
                }
                Query::References { .. } => params["context"] = json!({"includeDeclaration":true}),
                Query::Rename { new_name, .. } => params["newName"] = json!(new_name),
                Query::Formatting {
                    tab_width,
                    insert_spaces,
                } => params["options"] = json!({"tabSize":tab_width,"insertSpaces":insert_spaces}),
                Query::RangeFormatting {
                    range,
                    tab_width,
                    insert_spaces,
                } => {
                    params["range"] = json!(range);
                    params["options"] = json!({"tabSize":tab_width,"insertSpaces":insert_spaces});
                }
                Query::CodeActions { range } => {
                    params["range"] = json!(range);
                    let raw = session
                        .diagnostics
                        .lock()
                        .unwrap()
                        .get(&uri)
                        .map_or_else(Vec::new, |diagnostics| diagnostics.raw.clone());
                    params["context"] = json!({"diagnostics":raw,"triggerKind":1});
                }
                Query::ResolveAction { id } | Query::ReviewAction { id } => {
                    let action = document
                        .actions
                        .get(id)
                        .filter(|action| {
                            action.created.elapsed() < super::commands::COMMAND_LIFETIME
                        })
                        .map(|action| action.value.clone())
                        .ok_or("Code action is stale or belongs to another document")?;
                    let can_resolve = session
                        .capabilities
                        .pointer("/codeActionProvider/resolveProvider")
                        .and_then(Value::as_bool)
                        == Some(true)
                        // Older servers advertise a boolean provider while
                        // returning deferred actions after we advertise edit
                        // resolution support. Preserve their opaque data and
                        // resolve through the same source-bound review path.
                        || (session.capabilities.get("codeActionProvider")
                            == Some(&Value::Bool(true))
                            && action.get("data").is_some_and(|data| !data.is_null())
                            && action.get("command").is_none_or(Value::is_null));
                    if action.get("edit").is_some_and(|edit| !edit.is_null())
                        || action.get("command").is_some_and(Value::is_string)
                        || !can_resolve
                    {
                        result = action;
                    } else {
                        params = action;
                    }
                }
                Query::ResolveCompletion { id } => {
                    let completion = document
                        .completions
                        .get(id)
                        .cloned()
                        .ok_or("Completion is stale or belongs to another document")?;
                    if session
                        .capabilities
                        .pointer("/completionProvider/resolveProvider")
                        .and_then(Value::as_bool)
                        == Some(true)
                    {
                        params = completion;
                    } else {
                        result = completion;
                    }
                }
                _ => {}
            }
            if result.is_null() {
                result = request_current(&session.client, method, params).await?;
            }
            if matches!(input.query, Query::Completion { .. }) && !result.is_null() {
                let items = result
                    .as_array()
                    .or_else(|| result.get("items").and_then(Value::as_array))
                    .filter(|items| items.len() <= super::protocol::MAX_RECORDS)
                    .ok_or("Invalid or oversized completion list")?;
                document.completions.clear();
                let mut choices = Vec::new();
                for item in items {
                    let label = item["label"]
                        .as_str()
                        .filter(|label| !label.is_empty() && label.len() <= 4096)
                        .ok_or("Invalid completion label")?;
                    let id = format!("{:032x}", rand::random::<u128>());
                    document.completions.insert(id.clone(), item.clone());
                    choices.push(json!({"id":id,"label":label,"detail":item["detail"],"sortText":item["sortText"],"filterText":item["filterText"]}));
                }
                result = json!(choices);
            }
            if matches!(input.query, Query::CodeActions { .. }) && !result.is_null() {
                let actions = result
                    .as_array()
                    .filter(|actions| actions.len() <= MAX_ACTIONS)
                    .ok_or("Invalid or oversized code-action list")?;
                if serde_json::to_vec(actions)
                    .map_err(|error| error.to_string())?
                    .len()
                    > MAX_ACTION_BYTES
                {
                    return Err("Language-server code-action data exceeds its limit".into());
                }
                document.actions.clear();
                let mut visible = Vec::new();
                for action in actions {
                    let id = format!("{:032x}", rand::random::<u128>());
                    document.actions.insert(
                        id.clone(),
                        RetainedAction {
                            value: action.clone(),
                            created: Instant::now(),
                        },
                    );
                    visible.push(json!({"id":id,"title":action["title"],"kind":action["kind"],"isPreferred":action["isPreferred"],"disabled":action["disabled"]}));
                }
                result = json!(visible);
            }
        }
        let document_version = document.version;
        result = self
            .normalize_result(session, input, result, &mut affected)
            .await?;
        let (diagnostics, ready) = {
            let snapshots = session.diagnostics.lock().unwrap();
            match snapshots.get(&uri) {
                Some(snapshot) if snapshot.version == document_version => {
                    (normalize_diagnostics(&snapshot.raw)?, snapshot.ready)
                }
                _ => (vec![], false),
            }
        };
        let response = response(session, input, result, diagnostics, ready);
        response.validate(input)?;
        Ok((response, affected))
    }
}
fn supported(capabilities: &Value, name: &str) -> bool {
    capabilities
        .get(name)
        .is_some_and(|value| value == &Value::Bool(true) || value.is_object())
}
fn response(
    session: &Session,
    input: &Request,
    result: Value,
    diagnostics: Vec<Diagnostic>,
    ready: bool,
) -> Response {
    Response {
        version: protocol::VERSION,
        client: input.client.clone(),
        document: input.document.clone(),
        request: input.request,
        revision: input.revision,
        source_digest: input.source_digest.clone(),
        server: session.id.clone(),
        context: None,
        capabilities: session.capabilities.clone(),
        result,
        diagnostics,
        diagnostics_ready: ready,
    }
}
fn normalize_diagnostics(raw: &[Value]) -> Result<Vec<Diagnostic>, String> {
    raw.iter()
        .map(|value| {
            Ok(Diagnostic {
                range: serde_json::from_value(value["range"].clone())
                    .map_err(|_| "Invalid diagnostic range")?,
                severity: value
                    .get("severity")
                    .map_or(Some(1), Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or("Invalid diagnostic severity")?,
                message: value["message"]
                    .as_str()
                    .ok_or("Invalid diagnostic message")?
                    .into(),
                source: value
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                code: value
                    .get("code")
                    .filter(|value| value.is_string() || value.is_number())
                    .map(|value| {
                        value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned)
                    }),
            })
        })
        .collect()
}
pub(super) async fn source_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if !protocol::relative_path(relative) {
        return Err("Invalid project-relative language path".into());
    }
    let requested = root.join(relative);
    let canonical = match tokio::fs::canonicalize(&requested).await {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = tokio::fs::canonicalize(
                requested
                    .parent()
                    .ok_or("Invalid language document parent")?,
            )
            .await
            .map_err(|error| format!("Language document parent is unavailable: {error}"))?;
            parent.join(
                requested
                    .file_name()
                    .ok_or("Invalid language document name")?,
            )
        }
        Err(error) => return Err(format!("Language document is unavailable: {error}")),
    };
    if !canonical.starts_with(root) {
        return Err("Language document resolves outside the selected project".into());
    }
    Ok(canonical)
}

pub(super) async fn workspace_root(root: &Path, document: &Path, markers: &[String]) -> PathBuf {
    async fn has_marker(path: &Path, markers: &[String]) -> bool {
        for marker in markers {
            if tokio::fs::try_exists(path.join(marker))
                .await
                .unwrap_or(false)
            {
                return true;
            }
        }
        false
    }
    if has_marker(root, markers).await {
        return root.to_path_buf();
    }
    let mut current = document.parent();
    while let Some(path) = current.filter(|path| path.starts_with(root)) {
        if has_marker(path, markers).await {
            return path.to_path_buf();
        }
        if path == root {
            break;
        }
        current = path.parent();
    }
    root.to_path_buf()
}
