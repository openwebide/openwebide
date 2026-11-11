//! Ordered resource operations and unsaved text edits share one logical tree.
//! Both native LSP normalization and editor review use this planner; adapters
//! supply only source bytes and canonical filesystem entries.
use super::results::{FileEdit, WorkspaceChange, editor_edits};
use crate::plugins::languages::source_digest;
use crate::{
    editor::{
        Document, Edit, EditorRecovery, EditorRecoveryFile, EditorRecoveryRecord, RecoveryScroll,
        Selection,
    },
    workspace_entries::{WorkspaceEntries, contains_path, entry_path, moved_path},
    workspace_transaction::{self as resource, EntryState, FileOperation, Snapshot, WorkspacePlan},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone)]
pub struct WorkspaceDocument {
    pub document: Document,
    pub revision: Option<u64>,
    pub server_version: Option<i64>,
    pub original_path: Option<String>,
    pub read_only: bool,
}
impl WorkspaceDocument {
    pub fn new(path: &str, document: Document) -> Self {
        Self {
            document,
            revision: None,
            server_version: None,
            original_path: Some(path.into()),
            read_only: false,
        }
    }
}
pub struct WorkspaceDraft {
    pub plan: WorkspacePlan,
    pub documents: BTreeMap<String, WorkspaceDocument>,
    pub before_documents: BTreeMap<String, WorkspaceDocument>,
    pub order: Vec<String>,
    pub selected: Option<String>,
    pub touched: BTreeSet<String>,
    pub changes: Vec<WorkspaceChange>,
    disk: Snapshot,
    origins: BTreeMap<String, Option<String>>,
    next_resource: usize,
    review_paths: BTreeSet<String>,
}
impl WorkspaceDraft {
    pub async fn prepare(
        files: &impl WorkspaceEntries,
        root: String,
        operations: Vec<FileOperation>,
        documents: BTreeMap<String, WorkspaceDocument>,
        order: Vec<String>,
        selected: Option<String>,
        current: impl Fn() -> bool,
    ) -> Result<Self, String> {
        Self::prepare_draft(
            files,
            (1, root, operations),
            documents,
            order,
            selected,
            current,
        )
        .await
    }
    /// Use native resource moves without changing ordered editor/text planning.
    pub async fn prepare_native(
        files: &impl WorkspaceEntries,
        root: String,
        operations: Vec<FileOperation>,
        documents: BTreeMap<String, WorkspaceDocument>,
        order: Vec<String>,
        selected: Option<String>,
        current: impl Fn() -> bool,
    ) -> Result<Self, String> {
        Self::prepare_draft(
            files,
            (2, root, operations),
            documents,
            order,
            selected,
            current,
        )
        .await
    }
    async fn prepare_draft(
        files: &impl WorkspaceEntries,
        (version, root, operations): (u32, String, Vec<FileOperation>),
        documents: BTreeMap<String, WorkspaceDocument>,
        order: Vec<String>,
        selected: Option<String>,
        current: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if documents.len() > crate::editor::MAX_RECOVERY_FILES
            || order.len() > crate::editor::MAX_RECOVERY_FILES
            || order.iter().collect::<BTreeSet<_>>().len() != order.len()
            || selected.as_ref().is_some_and(|path| !order.contains(path))
        {
            return Err("Invalid open documents for workspace review".into());
        }
        for path in documents.keys().chain(order.iter()) {
            if !super::relative_path(path) {
                return Err("Invalid retained workspace document path".into());
            }
        }
        let plan = if version == 2 {
            resource::prepare_native(files, root, operations, current).await?
        } else {
            resource::prepare(files, root, operations, current).await?
        };
        let origins = plan
            .before
            .iter()
            .filter(|(_, value)| matches!(value, EntryState::File { .. }))
            .map(|(path, _)| (path.clone(), Some(path.clone())))
            .collect();
        Ok(Self {
            disk: plan.before.clone(),
            plan,
            before_documents: documents.clone(),
            documents,
            order,
            selected,
            touched: BTreeSet::new(),
            changes: vec![],
            origins,
            next_resource: 0,
            review_paths: BTreeSet::new(),
        })
    }
    pub async fn document(
        &mut self,
        files: &impl WorkspaceEntries,
        path: &str,
        current: impl Fn() -> bool,
    ) -> Result<&mut WorkspaceDocument, String> {
        entry_path(path)?;
        if !current() {
            return Err("Workspace review was superseded".into());
        }
        if !self.documents.contains_key(path) {
            if self.documents.len() >= crate::editor::MAX_RECOVERY_FILES {
                return Err("Workspace edit would exceed the recoverable tab limit".into());
            }
            let bytes = match self.disk.get(path) {
                Some(EntryState::File { bytes }) => bytes.clone(),
                _ if self.plan.roots.iter().any(|root| contains_path(root, path)) => {
                    return Err(format!(
                        "Text edit targets a missing file or folder: {path}"
                    ));
                }
                _ => {
                    if files.canonicalize(path).await? != path {
                        return Err("Workspace text edits cannot follow symbolic links".into());
                    }
                    files.read_bytes(path).await?
                }
            };
            if !current() {
                return Err("Workspace review was superseded".into());
            }
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("Text edit targets a binary file: {path}"))?;
            let mut document = WorkspaceDocument::new(path, Document::new(&text));
            document.original_path = self
                .origins
                .get(path)
                .cloned()
                .unwrap_or_else(|| Some(path.into()));
            if let Some(original) = &document.original_path {
                self.before_documents
                    .entry(original.clone())
                    .or_insert_with(|| document.clone());
            }
            self.documents.insert(path.into(), document);
        }
        Ok(self.documents.get_mut(path).unwrap())
    }
    pub fn text(&mut self, file: FileEdit) -> Result<(), String> {
        entry_path(&file.path)?;
        let document = self
            .documents
            .get_mut(&file.path)
            .ok_or("Workspace text source has not been loaded")?;
        if document.read_only {
            return Err(format!("{} is read-only", file.path));
        }
        if source_digest(document.document.text()) != file.source_digest {
            return Err(format!("{} changed; request the edit again", file.path));
        }
        self.review_paths.insert(
            document
                .original_path
                .as_ref()
                .unwrap_or(&file.path)
                .clone(),
        );
        let edits = editor_edits(document.document.text(), &file.edits)?;
        let caret = edits
            .first()
            .map_or(0, |edit| edit.range.start + edit.text.len());
        document
            .document
            .apply(edits, vec![Selection::caret(caret)], None)
            .map_err(|e| e.to_string())?;
        if !self.order.contains(&file.path) {
            self.order.push(file.path.clone());
        }
        self.touched.insert(file.path.clone());
        self.changes.push(WorkspaceChange::Text { file });
        self.check_budget()
    }
    pub fn resource(&mut self, operation: FileOperation) -> Result<(), String> {
        if self.plan.operations.get(self.next_resource) != Some(&operation) {
            return Err("Workspace resource operations changed order".into());
        }
        // Inputs remain part of the review even when an overwrite, ignored
        // operation or text edit leaves an identical recovery record behind.
        let roots = operation.paths();
        let paths = self
            .order
            .iter()
            .filter(|path| roots.iter().any(|root| contains_path(root, path)))
            .map(|path| self.original_path(path).unwrap_or(path).to_owned())
            .collect::<Vec<_>>();
        self.review_paths.extend(paths);
        let after = resource::apply_operations(
            &self.disk,
            &self.plan.roots,
            std::slice::from_ref(&operation),
        )?;
        let exists = |path: &str| {
            self.disk
                .get(path)
                .is_some_and(|state| !matches!(state, EntryState::Missing))
        };
        let ignored = match &operation {
            FileOperation::Create {
                path,
                overwrite,
                ignore_if_exists,
            } => exists(path) && !overwrite && *ignore_if_exists,
            FileOperation::Rename {
                from,
                to,
                overwrite,
                ignore_if_exists,
            } => from == to || (exists(to) && !overwrite && *ignore_if_exists),
            FileOperation::Delete {
                path,
                ignore_if_missing,
                ..
            } => !exists(path) && *ignore_if_missing,
        };
        if !ignored {
            for (path, document) in &self.documents {
                if document.read_only
                    && operation
                        .paths()
                        .iter()
                        .any(|root| contains_path(root, path))
                {
                    return Err(format!("{path} is read-only"));
                }
            }
            match &operation {
                FileOperation::Create { path, .. } => {
                    if let Some(document) = self.documents.get_mut(path) {
                        document
                            .document
                            .apply(
                                vec![Edit::replace(0..document.document.text().len(), "")],
                                vec![Selection::caret(0)],
                                None,
                            )
                            .map_err(|e| e.to_string())?;
                        document.document.mark_saved_source(Arc::new(String::new()));
                    }
                    self.origins.insert(path.clone(), None);
                    self.touched.insert(path.clone());
                }
                FileOperation::Rename { from, to, .. } => {
                    let moved = self
                        .documents
                        .iter()
                        .filter_map(|(path, document)| {
                            moved_path(path, from, to).map(|target| (target, document.clone()))
                        })
                        .collect::<Vec<_>>();
                    self.documents
                        .retain(|path, _| !contains_path(from, path) && !contains_path(to, path));
                    self.documents.extend(moved);
                    let origins = self
                        .origins
                        .iter()
                        .filter_map(|(path, original)| {
                            moved_path(path, from, to).map(|target| (target, original.clone()))
                        })
                        .collect::<Vec<_>>();
                    self.origins
                        .retain(|path, _| !contains_path(from, path) && !contains_path(to, path));
                    self.origins.extend(origins);
                    self.order = self
                        .order
                        .iter()
                        .filter_map(|path| {
                            moved_path(path, from, to)
                                .or_else(|| (!contains_path(to, path)).then(|| path.clone()))
                        })
                        .collect();
                    self.selected = self.selected.as_ref().and_then(|path| {
                        moved_path(path, from, to)
                            .or_else(|| (!contains_path(to, path)).then(|| path.clone()))
                    });
                    self.touched.insert(from.clone());
                    self.touched.insert(to.clone());
                }
                FileOperation::Delete { path, .. } => {
                    self.documents
                        .retain(|candidate, _| !contains_path(path, candidate));
                    self.origins
                        .retain(|candidate, _| !contains_path(path, candidate));
                    self.order
                        .retain(|candidate| !contains_path(path, candidate));
                    if self
                        .selected
                        .as_ref()
                        .is_some_and(|selected| contains_path(path, selected))
                    {
                        self.selected = None;
                    }
                    self.touched.insert(path.clone());
                }
            }
        }
        self.disk = after;
        self.next_resource += 1;
        self.changes.push(WorkspaceChange::File { operation });
        self.check_budget()
    }
    pub fn original_path<'a>(&'a self, path: &'a str) -> Option<&'a str> {
        self.documents
            .get(path)
            .map(|document| document.original_path.as_deref())
            .or_else(|| self.origins.get(path).map(|original| original.as_deref()))
            .unwrap_or_else(|| {
                (!self.plan.roots.iter().any(|root| contains_path(root, path))).then_some(path)
            })
    }
    pub fn recovery(&self, before: &EditorRecovery) -> Result<EditorRecovery, String> {
        self.finish()?;
        let root = before
            .root
            .clone()
            .ok_or("The reviewed editor folder is unavailable")?;
        let mut files = Vec::new();
        for path in &self.order {
            let original = self.original_path(path).unwrap_or(path);
            let before = before.files.iter().find(|file| file.path == original);
            let file = if let Some(document) = self.documents.get(path) {
                EditorRecoveryFile {
                    path: path.clone(),
                    document: Some(document.document.recovery()),
                    scroll: before.map_or_else(RecoveryScroll::default, |file| file.scroll),
                    read_only: document.read_only,
                }
            } else {
                let mut file = before
                    .cloned()
                    .ok_or("A retained media tab lost its original recovery record")?;
                file.path.clone_from(path);
                file
            };
            files.push(file);
        }
        let recovery = EditorRecovery {
            format: 1,
            root: Some(root),
            selected: self
                .selected
                .clone()
                .or_else(|| self.order.first().cloned()),
            files,
        };
        recovery.validate()?;
        Ok(recovery)
    }
    pub fn recovery_patch(
        &self,
        record: &EditorRecoveryRecord,
        after: &EditorRecovery,
    ) -> Result<resource::EditorRecoveryPatch, String> {
        let patch = resource::EditorRecoveryPatch {
            revision: record.revision,
            root: after
                .root
                .clone()
                .ok_or("The reviewed editor folder is unavailable")?,
            before: record
                .state
                .files
                .iter()
                .filter(|before| {
                    self.review_paths.contains(&before.path)
                        || after.files.iter().find(|after| after.path == before.path)
                            != Some(*before)
                })
                .cloned()
                .collect(),
            after: after
                .files
                .iter()
                .filter(|after| {
                    self.review_paths
                        .contains(self.original_path(&after.path).unwrap_or(&after.path))
                        || record
                            .state
                            .files
                            .iter()
                            .find(|before| before.path == after.path)
                            != Some(*after)
                })
                .cloned()
                .collect(),
            order_before: record
                .state
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect(),
            order_after: after.files.iter().map(|file| file.path.clone()).collect(),
            selected_before: record.state.selected.clone(),
            selected_after: after.selected.clone(),
        };
        patch.validate()?;
        Ok(patch)
    }
    pub fn finish(&self) -> Result<(), String> {
        if self.next_resource != self.plan.operations.len() || self.disk != self.plan.after {
            return Err("Workspace edit did not consume its complete ordered resource plan".into());
        }
        self.check_budget()
    }
    fn check_budget(&self) -> Result<(), String> {
        let bytes = self
            .before_documents
            .values()
            .chain(self.documents.values())
            .map(|document| document.document.text().len())
            .sum::<usize>();
        if self.documents.len() > crate::editor::MAX_RECOVERY_FILES
            || bytes > resource::MAX_EDITOR_PATCH_BYTES
        {
            return Err("Workspace edit exceeds its recoverable document budget".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
