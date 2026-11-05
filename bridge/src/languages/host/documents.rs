use super::*;
use openwebide_core::plugins::languages::intelligence::Diagnostic;
use std::path::{Path, PathBuf};

const MAX_DOCUMENTS: usize = 64;
const MAX_SOURCES: usize = 32 * 1024 * 1024;
const MAX_ACTIONS: usize = 128;
const MAX_ACTION_BYTES: usize = 256 * 1024;
mod results;

#[derive(Default)]
pub(super) struct Documents {
    documents: BTreeMap<String, Document>,
    version: i32,
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
    actions: BTreeMap<String, Value>,
    completions: BTreeMap<String, Value>,
    resources: BTreeMap<String, PathBuf>,
}
pub(super) struct Diagnostics {
    pub version: i32,
    pub raw: Vec<Value>,
    pub ready: bool,
}
impl Documents {
    pub(super) async fn query(
        &mut self,
        session: &Session,
        input: &Request,
        path: &Path,
        language: &str,
    ) -> Result<Response, String> {
        if matches!(input.query, Query::Close) {
            if self.documents.get(&input.document).is_some_and(|document| {
                input.request <= document.request || input.revision < document.revision
            }) {
                return Err("Language close request is stale".into());
            }
            if let Some(document) = self.documents.remove(&input.document) {
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
            return Ok(response(session, input, Value::Null, vec![], true));
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
                actions: BTreeMap::new(),
                completions: BTreeMap::new(),
                resources: BTreeMap::new(),
            });
        if document.digest != input.source_digest || is_new {
            self.version = self
                .version
                .checked_add(1)
                .ok_or("Language document version exhausted")?;
            document.version = self.version;
            document.source = Arc::from(input.source.as_str());
            document.digest = input.source_digest.clone();
            document.actions.clear();
            document.completions.clear();
            document.resources.clear();
            session.diagnostics.lock().unwrap().insert(
                uri.clone(),
                Diagnostics {
                    version: document.version,
                    raw: vec![],
                    ready: false,
                },
            );
            if is_new {
                session.client.notify("textDocument/didOpen",json!({"textDocument":{"uri":uri,"languageId":language,"version":document.version,"text":input.source}})).await.map_err(|error|error.to_string())?;
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
        if matches!(input.query, Query::Synchronize | Query::Saved)
            && supported(&session.capabilities, "diagnosticProvider")
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
                        raw,
                        ready: true,
                    },
                );
            }
        }
        let mut result = Value::Null;
        if let Query::ReadResource { id } = &input.query {
            let path = document
                .resources
                .get(id)
                .ok_or("Language resource is stale or belongs to another document")?;
            let source = results::read_source(path).await?;
            result = json!({"name":path.file_name().unwrap_or_default().to_string_lossy(),"source":source,"readOnly":true});
        }
        if let Some(method) = input.query.method() {
            let capability = match &input.query {
                Query::Hover { .. } => "hoverProvider",
                Query::Completion { .. } | Query::ResolveCompletion { .. } => "completionProvider",
                Query::Definition { .. } => "definitionProvider",
                Query::References { .. } => "referencesProvider",
                Query::Symbols => "documentSymbolProvider",
                Query::Rename { .. } => "renameProvider",
                Query::Formatting { .. } => "documentFormattingProvider",
                Query::CodeActions { .. } | Query::ResolveAction { .. } => "codeActionProvider",
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
                | Query::References { position }
                | Query::Rename { position, .. } => {
                    params["position"] = json!(position);
                }
                _ => {}
            }
            match &input.query {
                Query::References { .. } => params["context"] = json!({"includeDeclaration":true}),
                Query::Rename { new_name, .. } => params["newName"] = json!(new_name),
                Query::Formatting {
                    tab_width,
                    insert_spaces,
                } => params["options"] = json!({"tabSize":tab_width,"insertSpaces":insert_spaces}),
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
                Query::ResolveAction { id } => {
                    let action = document
                        .actions
                        .get(id)
                        .cloned()
                        .ok_or("Code action is stale or belongs to another document")?;
                    if action.get("edit").is_some()
                        || !session
                            .capabilities
                            .pointer("/codeActionProvider/resolveProvider")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
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
                    document.actions.insert(id.clone(), action.clone());
                    visible.push(json!({"id":id,"title":action["title"],"kind":action["kind"],"isPreferred":action["isPreferred"],"disabled":action["disabled"]}));
                }
                result = json!(visible);
            }
        }
        let document_version = document.version;
        result = self.normalize_result(session, input, result).await?;
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
        Ok(response)
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
