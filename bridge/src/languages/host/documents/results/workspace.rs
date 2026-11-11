use super::*;
use openwebide_core::{
    plugins::languages::{
        intelligence::{results::WorkspaceChange, workspace_edit::WorkspaceDraft},
        source_digest,
    },
    workspace_transaction::FileOperation,
};

enum Change {
    Text {
        path: String,
        version: Option<i64>,
        edits: Vec<TextEdit>,
    },
    File(FileOperation),
}
fn relative_uri(root: &Path, value: &Value) -> Result<String, String> {
    let path = file_uri_path(value.as_str().ok_or("Workspace resource URI is missing")?)?;
    let path = relative(root, &path)
        .ok_or("Workspace edits cannot change files outside the selected project")?;
    openwebide_core::workspace_entries::entry_path(&path)?;
    Ok(path)
}
fn flag(value: &Value, field: &str) -> Result<bool, String> {
    let options = match value.get("options") {
        None | Some(Value::Null) => return Ok(false),
        Some(Value::Object(options)) => options,
        _ => return Err("Invalid workspace resource options".into()),
    };
    match options.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        _ => Err("Invalid workspace resource options".into()),
    }
}

impl Documents {
    pub(super) async fn ordered_workspace_edit(
        &self,
        session: &Session,
        result: &Value,
        files: &Files,
    ) -> Result<Value, String> {
        if result.get("changeAnnotations").is_some_and(|annotations| {
            !annotations.is_null()
                && annotations
                    .as_object()
                    .is_none_or(|annotations| !annotations.is_empty())
        }) {
            return Err(
                "Annotated workspace changes require annotation review; no changes were applied"
                    .into(),
            );
        }
        let input = result["documentChanges"]
            .as_array()
            .ok_or("Invalid ordered workspace edits")?;
        if input.len() > protocol::MAX_RECORDS {
            return Err("Too many ordered workspace changes".into());
        }
        let mut changes = Vec::new();
        let mut operations = Vec::new();
        for change in input {
            if change
                .get("annotationId")
                .is_some_and(|value| !value.is_null())
            {
                return Err(
                    "Annotated workspace resources are unsupported; no changes were applied".into(),
                );
            }
            let value = match change.get("kind").and_then(Value::as_str) {
                Some("create") => Change::File(FileOperation::Create {
                    path: relative_uri(&session.root, &change["uri"])?,
                    overwrite: flag(change, "overwrite")?,
                    ignore_if_exists: flag(change, "ignoreIfExists")?,
                }),
                Some("rename") => Change::File(FileOperation::Rename {
                    from: relative_uri(&session.root, &change["oldUri"])?,
                    to: relative_uri(&session.root, &change["newUri"])?,
                    overwrite: flag(change, "overwrite")?,
                    ignore_if_exists: flag(change, "ignoreIfExists")?,
                }),
                Some("delete") => Change::File(FileOperation::Delete {
                    path: relative_uri(&session.root, &change["uri"])?,
                    recursive: flag(change, "recursive")?,
                    ignore_if_missing: flag(change, "ignoreIfNotExists")?,
                }),
                Some(_) => {
                    return Err(
                        "Unsupported workspace resource operation; no changes were applied".into(),
                    );
                }
                None => {
                    let version = match &change["textDocument"]["version"] {
                        Value::Null => None,
                        version => Some(
                            version
                                .as_i64()
                                .ok_or("Invalid workspace document version")?,
                        ),
                    };
                    Change::Text {
                        path: relative_uri(&session.root, &change["textDocument"]["uri"])?,
                        version,
                        edits: serde_json::from_value(change["edits"].clone())
                            .map_err(|_| "Invalid ordered text edits")?,
                    }
                }
            };
            if let Change::File(operation) = &value {
                operations.push(operation.clone());
            }
            changes.push(value);
        }
        let documents = self.workspace_documents(&session.root);
        let order = documents.keys().cloned().collect();
        let mut draft = WorkspaceDraft::prepare(
            files,
            "language-edit-normalization".into(),
            operations,
            documents,
            order,
            None,
            || true,
        )
        .await?;
        for change in changes {
            match change {
                Change::File(operation) => draft.resource(operation)?,
                Change::Text {
                    path,
                    version,
                    edits,
                } => {
                    let document = draft.document(files, &path, || true).await?;
                    if version.is_some() && version != document.server_version {
                        return Err("Workspace edit document version is stale".into());
                    }
                    let edit = FileEdit {
                        path,
                        source_digest: source_digest(document.document.text()),
                        revision: document.revision,
                        edits,
                    };
                    draft.text(edit)?;
                }
            }
        }
        draft.finish()?;
        // This is a review description only. Neither disk nor open server overlays
        // change until the shared reviewed transaction is explicitly approved.
        let changes: Vec<WorkspaceChange> = draft.changes;
        serde_json::to_value(WorkspaceEdit {
            files: vec![],
            changes,
        })
        .map_err(|e| e.to_string())
    }
}
