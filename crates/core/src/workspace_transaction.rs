//! Reviewed workspace changes and durable recovery, shared above filesystem adapters.
use crate::workspace_entries::{WorkspaceEntries, contains_path, entry_path, parent};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod editor;
pub use editor::{EditorRecoveryPatch, MAX_EDITOR_PATCH_BYTES};
mod activity;
mod delegation;
mod language_review;
mod requests;
pub use language_review::{LanguageReview, MAX_LANGUAGE_REVIEW_BYTES, language_review_admission};
mod scope;
mod worker;
pub use activity::WorkspaceActivity;
pub use delegation::{
    DelegateWorkspaceTransaction, DelegatedWorkspaceJournalOperation,
    DelegatedWorkspaceJournalRequest, WorkspaceJournalDelegation, WorkspaceTransactionWorkerKey,
};
pub use requests::{
    AdvanceWorkspaceTransaction, BeginWorkspaceTransaction, ExecuteWorkspaceTransaction,
    JOURNAL_API_BODY_LIMIT, NativeWorkspaceTransaction, WorkspaceJournalOperation,
    WorkspaceJournalRequest, WorkspaceJournalResponse, WorkspaceRootIdentity,
    WorkspaceTransactionAction, WorkspaceTransactionAdvanced, WorkspaceTransactionOutcome,
    WorkspaceTransactionPrepared, WorkspaceTransactionState, valid_id,
};
pub use scope::WorkspaceTransactionScope;
pub use worker::{WorkspaceTransactionWorker, WorkspaceTransactionWorkerState};
mod adapter;
pub use adapter::VfsTransactionFiles;
pub mod companion;
mod resources;
pub use resources::{
    MAX_RESOURCE_STEPS, OriginalMutation, ResourceLocation, ResourceScope, ResourceScript,
    ResourceStep,
};
mod journal;
pub use journal::{
    JournalRecord, PreparedJournal, RecoveryState, TransactionError, TransactionFiles,
    TransactionPhase, TransactionProgress, WorkspaceJournal, WorkspaceMutation, apply, execute,
    preview_recovery, recover,
};

pub const MAX_OPERATIONS: usize = 128;
pub const MAX_ENTRIES: usize = 10_000;
pub const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
pub const JOURNAL_BODY_LIMIT: usize = 96 * 1024 * 1024;
pub const RECOVERY_SCOPE_BODY_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileOperation {
    Create {
        path: String,
        #[serde(default)]
        overwrite: bool,
        #[serde(default)]
        ignore_if_exists: bool,
    },
    Rename {
        from: String,
        to: String,
        #[serde(default)]
        overwrite: bool,
        #[serde(default)]
        ignore_if_exists: bool,
    },
    Delete {
        path: String,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        ignore_if_missing: bool,
    },
}
impl FileOperation {
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Self::Create { path, .. } | Self::Delete { path, .. } => vec![path],
            Self::Rename { from, to, .. } => vec![from, to],
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        for path in self.paths() {
            if path.len() > 4096 || entry_path(path)? != path || path.chars().any(char::is_control)
            {
                return Err("Workspace operation paths must be normalized project paths".into());
            }
        }
        if let Self::Rename { from, to, .. } = self
            && from != to
            && (contains_path(from, to) || contains_path(to, from))
        {
            return Err("A workspace rename cannot contain its source or destination".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EntryState {
    Missing,
    Directory,
    File {
        #[serde(with = "encoded_bytes")]
        bytes: Vec<u8>,
    },
}
impl EntryState {
    pub fn file(bytes: impl Into<Vec<u8>>) -> Self {
        Self::File {
            bytes: bytes.into(),
        }
    }
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::File { bytes } => Some(bytes),
            _ => None,
        }
    }
}
pub type Snapshot = BTreeMap<String, EntryState>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePlan {
    pub version: u32,
    /// Opaque root identity supplied by the owning workspace, never a filesystem path.
    pub root: String,
    pub operations: Vec<FileOperation>,
    pub roots: Vec<String>,
    pub before: Snapshot,
    pub after: Snapshot,
}
impl WorkspacePlan {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.version, 1 | 2)
            || self.root.is_empty()
            || self.root.len() > 4096
            || self.root.chars().any(char::is_control)
            || self.operations.len() > MAX_OPERATIONS
            || self.roots.len() > 2 * MAX_OPERATIONS
            || (self.operations.is_empty() != self.roots.is_empty())
        {
            return Err("Invalid workspace transaction identity or operation limits".into());
        }
        for operation in &self.operations {
            operation.validate()?;
        }
        let mut seen = BTreeSet::new();
        for root in &self.roots {
            if root.len() > 4096
                || entry_path(root)? != *root
                || !seen.insert(root)
                || self
                    .roots
                    .iter()
                    .any(|other| other != root && contains_path(other, root))
            {
                return Err("Workspace transaction roots overlap or repeat".into());
            }
        }
        for snapshot in [&self.before, &self.after] {
            if snapshot.len() > MAX_ENTRIES
                || snapshot
                    .values()
                    .filter_map(EntryState::bytes)
                    .map(<[u8]>::len)
                    .sum::<usize>()
                    > MAX_SNAPSHOT_BYTES
            {
                return Err(
                    "Workspace transaction snapshot exceeds 32 MiB or 10,000 entries".into(),
                );
            }
            for root in &self.roots {
                if !snapshot.contains_key(root) {
                    return Err("Workspace transaction omits a root".into());
                }
            }
            for (path, state) in snapshot {
                if path.len() > 4096
                    || entry_path(path)? != *path
                    || !self.roots.iter().any(|root| contains_path(root, path))
                    || (!self.roots.contains(path) && matches!(state, EntryState::Missing))
                    || (self.version == 2
                        && path
                            .split('/')
                            .any(|part| part.eq_ignore_ascii_case(".openwebide")))
                {
                    return Err("Invalid workspace transaction snapshot path".into());
                }
                if !self.roots.contains(path)
                    && snapshot.get(parent(path)) != Some(&EntryState::Directory)
                {
                    return Err("Workspace snapshot has a child without its directory".into());
                }
            }
        }
        if apply_operations(&self.before, &self.roots, &self.operations)? != self.after {
            return Err("Workspace transaction does not match its reviewed operations".into());
        }
        if self.version == 2 {
            ResourceScript::compile(self)?;
        }
        Ok(())
    }
    /// Hash the exact bounded recovery payload without allocating another full
    /// JSON copy. The storage acknowledgement binds this concrete reviewed plan.
    pub fn fingerprint(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        struct Fingerprint {
            hash: Sha256,
            bytes: usize,
        }
        impl std::io::Write for Fingerprint {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes = self.bytes.saturating_add(bytes.len());
                if self.bytes > JOURNAL_BODY_LIMIT {
                    return Err(std::io::Error::other(
                        "Workspace recovery payload exceeds 96 MiB",
                    ));
                }
                self.hash.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut output = Fingerprint {
            hash: Sha256::new(),
            bytes: 0,
        };
        serde_json::to_writer(&mut output, self).map_err(|error| error.to_string())?;
        Ok(output
            .hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
    pub fn changed_paths(&self) -> Vec<&str> {
        self.before
            .keys()
            .chain(self.after.keys())
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|path| state(&self.before, path) != state(&self.after, path))
            .collect()
    }
}
fn state<'a>(snapshot: &'a Snapshot, path: &str) -> &'a EntryState {
    snapshot.get(path).unwrap_or(&EntryState::Missing)
}
fn remove_tree(snapshot: &mut Snapshot, path: &str) {
    snapshot.retain(|candidate, _| !contains_path(path, candidate));
}
fn ensure_parents(
    snapshot: &mut Snapshot,
    path: &str,
    roots: &[String],
    emit: &mut impl FnMut(ResourceStep) -> Result<(), String>,
) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut directory = parent(path);
    while roots.iter().any(|root| contains_path(root, directory)) {
        match state(snapshot, directory) {
            EntryState::File { .. } => return Err(format!("Not a directory: {directory}")),
            EntryState::Missing => missing.push(directory.to_owned()),
            EntryState::Directory => break,
        }
        directory = parent(directory);
    }
    for directory in missing.into_iter().rev() {
        emit(ResourceStep::Create {
            path: directory.clone(),
            kind: crate::vfs::VfsEntryKind::Directory,
        })?;
        snapshot.insert(directory, EntryState::Directory);
    }
    Ok(())
}

/// Interpret the ordered resource script once. Text edits use the resulting
/// path mapping; adapters never invent their own overwrite or directory rules.
pub fn apply_operations(
    before: &Snapshot,
    roots: &[String],
    operations: &[FileOperation],
) -> Result<Snapshot, String> {
    interpret_operations(before, roots, operations, |_| Ok(()))
}

fn interpret_operations(
    before: &Snapshot,
    roots: &[String],
    operations: &[FileOperation],
    mut emit: impl FnMut(ResourceStep) -> Result<(), String>,
) -> Result<Snapshot, String> {
    if operations.len() > MAX_OPERATIONS {
        return Err("Too many workspace operations".into());
    }
    let mut result = before.clone();
    let mut retained = 0_u32;
    for operation in operations {
        operation.validate()?;
        if operation
            .paths()
            .iter()
            .any(|path| !roots.iter().any(|root| contains_path(root, path)))
        {
            return Err("A workspace operation is outside its captured roots".into());
        }
        match operation {
            FileOperation::Create {
                path,
                overwrite,
                ignore_if_exists,
            } => {
                if !matches!(state(&result, path), EntryState::Missing) {
                    if *overwrite {
                        if matches!(state(&result, path), EntryState::Directory) {
                            return Err(format!("Cannot overwrite a folder with a file: {path}"));
                        }
                        resources::retain(path, state(&result, path), &mut retained, &mut emit)?;
                    } else if *ignore_if_exists {
                        continue;
                    } else {
                        return Err(format!("Path already exists: {path}"));
                    }
                }
                ensure_parents(&mut result, path, roots, &mut emit)?;
                emit(ResourceStep::Create {
                    path: path.clone(),
                    kind: crate::vfs::VfsEntryKind::File,
                })?;
                result.insert(path.clone(), EntryState::file(Vec::new()));
            }
            FileOperation::Rename {
                from,
                to,
                overwrite,
                ignore_if_exists,
            } => {
                if matches!(state(&result, from), EntryState::Missing) {
                    return Err(format!("Path not found: {from}"));
                }
                if from == to {
                    continue;
                }
                if !matches!(state(&result, to), EntryState::Missing) {
                    if *overwrite {
                        resources::retain(to, state(&result, to), &mut retained, &mut emit)?;
                        remove_tree(&mut result, to);
                    } else if *ignore_if_exists {
                        continue;
                    } else {
                        return Err(format!("Destination already exists: {to}"));
                    }
                }
                ensure_parents(&mut result, to, roots, &mut emit)?;
                emit(ResourceStep::Transfer {
                    from: ResourceLocation::Workspace(from.clone()),
                    to: ResourceLocation::Workspace(to.clone()),
                    kind: resources::kind(state(&result, from))?,
                })?;
                let moved = result
                    .iter()
                    .filter(|(path, _)| contains_path(from, path))
                    .map(|(path, value)| (format!("{to}{}", &path[from.len()..]), value.clone()))
                    .collect::<Vec<_>>();
                remove_tree(&mut result, from);
                result.extend(moved);
            }
            FileOperation::Delete {
                path,
                recursive,
                ignore_if_missing,
            } => {
                if matches!(state(&result, path), EntryState::Missing) {
                    if *ignore_if_missing {
                        continue;
                    }
                    return Err(format!("Path not found: {path}"));
                }
                if !recursive
                    && result
                        .keys()
                        .any(|candidate| candidate != path && contains_path(path, candidate))
                {
                    return Err(format!("Folder is not empty: {path}"));
                }
                resources::retain(path, state(&result, path), &mut retained, &mut emit)?;
                remove_tree(&mut result, path);
            }
        }
        result.retain(|path, state| !matches!(state, EntryState::Missing) || roots.contains(path));
        for root in roots {
            result.entry(root.clone()).or_insert(EntryState::Missing);
        }
        if result.len() > MAX_ENTRIES
            || result
                .values()
                .filter_map(EntryState::bytes)
                .map(<[u8]>::len)
                .sum::<usize>()
                > MAX_SNAPSHOT_BYTES
        {
            return Err("Workspace transaction exceeds 32 MiB or 10,000 entries".into());
        }
    }
    Ok(result)
}

async fn validate_path(files: &impl WorkspaceEntries, path: &str) -> Result<(), String> {
    if files.canonicalize(path).await? != path {
        return Err("Workspace transactions do not follow symbolic links or path aliases".into());
    }
    entry_path(path)?;
    Ok(())
}
async fn capture_root(files: &impl WorkspaceEntries, path: &str) -> Result<String, String> {
    let mut directory = String::new();
    for segment in path.split('/') {
        let candidate = if directory.is_empty() {
            segment.to_owned()
        } else {
            format!("{directory}/{segment}")
        };
        validate_path(files, &candidate).await?;
        let entry = files
            .list(&directory)
            .await?
            .into_iter()
            .find(|entry| entry.path == candidate);
        if entry.is_none() || candidate == path {
            return Ok(candidate);
        }
        if !entry.unwrap().is_dir {
            return Err(format!("Not a directory: {candidate}"));
        }
        directory = candidate;
    }
    Err("Workspace operation has no path".into())
}

/// Capture all affected subtrees, including absent parents which exclusive
/// creation may introduce. No file mutation occurs during preparation.
pub async fn prepare(
    files: &impl WorkspaceEntries,
    root: String,
    operations: Vec<FileOperation>,
    current: impl Fn() -> bool,
) -> Result<WorkspacePlan, String> {
    prepare_plan(files, (1, root), operations, current).await
}

/// New reviews require the owning native host's physical root. Version one is
/// retained only for callers recovering existing byte-based transactions.
pub async fn prepare_native(
    files: &impl WorkspaceEntries,
    root: String,
    operations: Vec<FileOperation>,
    current: impl Fn() -> bool,
) -> Result<WorkspacePlan, String> {
    if !companion::fingerprint(&root) {
        return Err("Native workspace review requires a physical folder identity".into());
    }
    prepare_plan(files, (2, root), operations, current).await
}

async fn prepare_plan(
    files: &impl WorkspaceEntries,
    (version, root): (u32, String),
    operations: Vec<FileOperation>,
    current: impl Fn() -> bool,
) -> Result<WorkspacePlan, String> {
    if !current() {
        return Err("Workspace review was superseded".into());
    }
    if operations.len() > MAX_OPERATIONS {
        return Err("Choose at most 128 workspace operations".into());
    }
    let mut candidates = BTreeSet::new();
    for operation in &operations {
        operation.validate()?;
        if version == 2
            && operation.paths().iter().any(|path| {
                path.split('/')
                    .any(|part| part.eq_ignore_ascii_case(".openwebide"))
            })
        {
            return Err("Native workspace operations cannot modify the recovery namespace".into());
        }
    }
    for operation in &operations {
        for path in operation.paths() {
            if !current() {
                return Err("Workspace review was superseded".into());
            }
            candidates.insert(capture_root(files, path).await?);
        }
    }
    let roots = candidates
        .iter()
        .filter(|path| {
            !candidates
                .iter()
                .any(|other| other != *path && contains_path(other, path))
        })
        .cloned()
        .collect::<Vec<_>>();
    let before = capture(files, &roots, &current).await?;
    let after = apply_operations(&before, &roots, &operations)?;
    let plan = WorkspacePlan {
        version,
        root,
        operations,
        roots,
        before,
        after,
    };
    plan.validate()?;
    if !current() {
        return Err("Workspace review was superseded".into());
    }
    Ok(plan)
}

pub async fn capture(
    files: &impl WorkspaceEntries,
    roots: &[String],
    current: impl Fn() -> bool,
) -> Result<Snapshot, String> {
    let mut snapshot = Snapshot::new();
    let mut pending = roots.to_vec();
    let mut bytes = 0_usize;
    while let Some(path) = pending.pop() {
        if !current() {
            return Err("Workspace changed; file operation stopped".into());
        }
        if snapshot.len() >= MAX_ENTRIES || snapshot.contains_key(&path) {
            return Err("Invalid or oversized workspace snapshot".into());
        }
        validate_path(files, &path).await?;
        let entries = files.list(parent(&path)).await?;
        let entry = entries.into_iter().find(|entry| entry.path == path);
        let value = if let Some(entry) = entry {
            if entry.is_dir {
                let children = files.list(&path).await?;
                if children
                    .len()
                    .saturating_add(pending.len())
                    .saturating_add(snapshot.len())
                    >= MAX_ENTRIES
                {
                    return Err("Workspace snapshot exceeds 10,000 entries".into());
                }
                for child in children {
                    if parent(&child.path) != path {
                        return Err("Invalid filesystem child path".into());
                    }
                    pending.push(child.path);
                }
                EntryState::Directory
            } else {
                let content = files.read_bytes(&path).await?;
                bytes = bytes.saturating_add(content.len());
                if bytes > MAX_SNAPSHOT_BYTES {
                    return Err("Workspace snapshot exceeds 32 MiB".into());
                }
                EntryState::file(content)
            }
        } else {
            EntryState::Missing
        };
        snapshot.insert(path, value);
    }
    Ok(snapshot)
}

mod encoded_bytes {
    use super::*;
    pub fn serialize<S: serde::Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() > (MAX_SNAPSHOT_BYTES * 4 / 3 + 4) {
            return Err(serde::de::Error::custom(
                "Workspace file exceeds its recovery limit",
            ));
        }
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests;
