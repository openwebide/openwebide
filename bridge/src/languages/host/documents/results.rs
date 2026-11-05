use super::*;
use protocol::results::{FileEdit, Symbol, Target, TextEdit, WorkspaceEdit};
use tokio::io::AsyncReadExt;

impl Documents {
    pub(super) async fn normalize_result(
        &mut self,
        session: &Session,
        input: &Request,
        result: Value,
    ) -> Result<Value, String> {
        let mut sources = self
            .documents
            .values()
            .map(|document| (document.path.clone(), Arc::clone(&document.source)))
            .collect::<BTreeMap<_, _>>();
        match &input.query {
            Query::Definition { .. } | Query::References { .. } => {
                let values = if result.is_null() {
                    vec![]
                } else if let Some(values) = result.as_array() {
                    values.clone()
                } else {
                    vec![result]
                };
                if values.len() > protocol::MAX_RECORDS {
                    return Err("Too many navigation targets".into());
                }
                let document = self
                    .documents
                    .get_mut(&input.document)
                    .ok_or("Language document is closed")?;
                let mut targets = Vec::new();
                for value in values {
                    targets.push(
                        target(session, &mut document.resources, &mut sources, &value).await?,
                    );
                }
                serde_json::to_value(targets).map_err(|error| error.to_string())
            }
            Query::Symbols => {
                let mut pending = result
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .rev()
                    .map(|value| (0_usize, value))
                    .collect::<Vec<_>>();
                let document = self
                    .documents
                    .get_mut(&input.document)
                    .ok_or("Language document is closed")?;
                let mut symbols = Vec::new();
                while let Some((depth, value)) = pending.pop() {
                    if depth > 64 || symbols.len() >= protocol::MAX_RECORDS {
                        return Err("Language-server symbol tree exceeds its limit".into());
                    }
                    let name = value["name"]
                        .as_str()
                        .filter(|name| !name.is_empty() && name.len() <= 4096)
                        .ok_or("Invalid document symbol name")?
                        .to_string();
                    let kind = value["kind"]
                        .as_u64()
                        .and_then(|kind| u32::try_from(kind).ok())
                        .filter(|kind| (1..=26).contains(kind))
                        .ok_or("Invalid symbol kind")?;
                    let target = if value.get("location").is_some() {
                        target(
                            session,
                            &mut document.resources,
                            &mut sources,
                            &value["location"],
                        )
                        .await?
                    } else {
                        let range: protocol::Range = serde_json::from_value(
                            value
                                .get("selectionRange")
                                .unwrap_or(&value["range"])
                                .clone(),
                        )
                        .map_err(|_| "Invalid symbol range")?;
                        Target {
                            path: Some(input.path.clone()),
                            resource: None,
                            source_digest: Some(input.source_digest.clone()),
                            label: input.path.clone(),
                            range,
                        }
                    };
                    target.validate()?;
                    if target.path.as_deref() == Some(&input.path) {
                        target.range.offsets(&input.source)?;
                    }
                    symbols.push(Symbol {
                        name,
                        kind,
                        target,
                        depth,
                    });
                    if let Some(children) = value["children"].as_array() {
                        pending.extend(
                            children
                                .iter()
                                .rev()
                                .cloned()
                                .map(|child| (depth + 1, child)),
                        );
                    }
                }
                serde_json::to_value(symbols).map_err(|error| error.to_string())
            }
            Query::Rename { .. } => self.workspace_edit(session, result).await,
            Query::ResolveAction { .. } => {
                if let Some(edit) = result.get("edit") {
                    self.workspace_edit(session, edit.clone()).await
                } else {
                    Err(
                        "This code action requires a server command rather than a workspace edit"
                            .into(),
                    )
                }
            }
            _ => Ok(result),
        }
    }

    async fn workspace_edit(&self, session: &Session, result: Value) -> Result<Value, String> {
        if result.is_null() {
            return Ok(json!(WorkspaceEdit { files: vec![] }));
        }
        let mut changes = Vec::new();
        if let Some(files) = result.get("documentChanges").and_then(Value::as_array) {
            for file in files {
                if file.get("kind").is_some() {
                    return Err("This workspace edit includes file creation, movement or deletion and requires file-operation review".into());
                }
                let uri = file["textDocument"]["uri"]
                    .as_str()
                    .ok_or("Invalid workspace edit document")?
                    .to_string();
                let version = file["textDocument"]["version"].as_i64();
                changes.push((uri, version, file["edits"].clone()));
            }
        } else if let Some(files) = result.get("changes").and_then(Value::as_object) {
            for (uri, edits) in files {
                changes.push((uri.clone(), None, edits.clone()));
            }
        }
        if changes.len() > MAX_DOCUMENTS {
            return Err("Workspace edit affects too many files".into());
        }
        let mut files = Vec::new();
        for (uri, version, edits) in changes {
            let path = file_path(&uri)?;
            let relative = relative(&session.root, &path)
                .ok_or("Workspace edits cannot modify files outside the selected project")?;
            let path = source_path(&session.root, &relative).await?;
            let open = self
                .documents
                .values()
                .find(|document| document.path == path);
            if let Some(version) = version
                && open.is_none_or(|document| i64::from(document.version) != version)
            {
                return Err("Workspace edit document version is stale".into());
            }
            let source = if let Some(document) = open {
                document.source.to_string()
            } else {
                read_source(&path).await?
            };
            let edits: Vec<TextEdit> =
                serde_json::from_value(edits).map_err(|_| "Invalid workspace text edits")?;
            protocol::results::editor_edits(&source, &edits)?;
            files.push(FileEdit {
                path: relative,
                source_digest: openwebide_core::plugins::languages::source_digest(&source),
                revision: open.map(|document| document.revision),
                edits,
            });
        }
        serde_json::to_value(WorkspaceEdit { files }).map_err(|error| error.to_string())
    }
}

async fn target(
    session: &Session,
    resources: &mut BTreeMap<String, Resource>,
    sources: &mut BTreeMap<PathBuf, Arc<str>>,
    value: &Value,
) -> Result<Target, String> {
    let uri = value
        .get("uri")
        .or_else(|| value.get("targetUri"))
        .and_then(Value::as_str)
        .ok_or("Invalid navigation URI")?;
    let range: protocol::Range = serde_json::from_value(
        value
            .get("range")
            .or_else(|| value.get("targetSelectionRange"))
            .ok_or("Missing navigation range")?
            .clone(),
    )
    .map_err(|_| "Invalid navigation range")?;
    let parsed = url::Url::parse(uri).map_err(|_| "Invalid language-server URI")?;
    if parsed.scheme() != "file" {
        if uri.len() > 4096
            || !session
                .virtual_documents
                .iter()
                .any(|declaration| declaration.scheme == parsed.scheme())
        {
            return Err(
                "This language-server resource scheme is not declared or its URI is too large"
                    .into(),
            );
        }
        let name = parsed
            .path()
            .rsplit('/')
            .next()
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= 256
                    && !matches!(*name, "." | "..")
                    && !name.contains(['\\'])
                    && !name.chars().any(char::is_control)
            })
            .unwrap_or("definition.txt")
            .to_owned();
        return resource_target(
            resources,
            Resource::Virtual {
                uri: uri.to_owned(),
                name: name.clone(),
            },
            name,
            range,
        );
    }
    let root = &session.root;
    let path = file_path(uri)?;
    let target = if let Some(path) = relative(root, &path) {
        let canonical = source_path(root, &path).await?;
        if !sources.contains_key(&canonical) {
            if sources.len() >= MAX_DOCUMENTS {
                return Err("Navigation affects too many source files".into());
            }
            let source = read_source(&canonical).await?;
            if sources.values().map(|source| source.len()).sum::<usize>() + source.len()
                > MAX_SOURCES
            {
                return Err("Navigation source snapshots exceed their memory limit".into());
            }
            sources.insert(canonical.clone(), Arc::from(source));
        }
        let source = &sources[&canonical];
        range.offsets(source)?;
        Target {
            label: path.clone(),
            path: Some(path),
            resource: None,
            source_digest: Some(openwebide_core::plugins::languages::source_digest(source)),
            range,
        }
    } else {
        let label = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        resource_target(resources, Resource::File(path), label, range)?
    };
    target.validate()?;
    Ok(target)
}
fn resource_target(
    resources: &mut BTreeMap<String, Resource>,
    resource: Resource,
    label: String,
    range: protocol::Range,
) -> Result<Target, String> {
    if resources.len() >= 4096
        || resources.values().map(Resource::bytes).sum::<usize>() + resource.bytes()
            > MAX_RESOURCE_BYTES
    {
        return Err("Language resource limit reached".into());
    }
    let id = format!("{:032x}", rand::random::<u128>());
    let target = Target {
        path: None,
        resource: Some(id.clone()),
        source_digest: None,
        label,
        range,
    };
    target.validate()?;
    resources.insert(id, resource);
    Ok(target)
}
fn file_path(uri: &str) -> Result<PathBuf, String> {
    let uri = url::Url::parse(uri).map_err(|_| "Invalid language-server URI")?;
    if uri.scheme() != "file" {
        return Err("This language-server resource scheme is not supported".into());
    }
    let path = uri
        .to_file_path()
        .map_err(|()| "Invalid language-server file URI")?;
    Ok(std::fs::canonicalize(&path).unwrap_or(path))
}
fn relative(root: &Path, path: &Path) -> Option<String> {
    let path = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    protocol::relative_path(&path).then_some(path)
}
pub(super) async fn read_source(path: &Path) -> Result<String, String> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| format!("Language resource is unavailable: {error}"))?;
    if !metadata.is_file()
        || metadata.len()
            > u64::try_from(openwebide_core::plugins::languages::MAX_SOURCE_BYTES)
                .expect("source limit fits u64")
    {
        return Err("Language resource must be a regular file within the source limit".into());
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("Cannot read language resource: {error}"))?;
    let mut bytes = Vec::new();
    file.take(
        u64::try_from(openwebide_core::plugins::languages::MAX_SOURCE_BYTES + 1)
            .expect("source limit fits u64"),
    )
    .read_to_end(&mut bytes)
    .await
    .map_err(|error| error.to_string())?;
    if bytes.len() > openwebide_core::plugins::languages::MAX_SOURCE_BYTES {
        return Err("Language resource exceeds its source limit".into());
    }
    String::from_utf8(bytes).map_err(|_| "Language resource is not valid UTF-8".into())
}
