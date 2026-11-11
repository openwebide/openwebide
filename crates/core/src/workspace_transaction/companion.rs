//! Folder capabilities and individual filesystem calls. Transaction planning,
//! journal transitions and recovery policy stay in the shared interpreter.
use super::{
    AdvanceWorkspaceTransaction, WorkspaceJournalDelegation, WorkspaceTransactionAdvanced,
    WorkspaceTransactionWorkerKey,
};
use super::{MAX_RESOURCE_STEPS, ResourceLocation, ResourceScope, valid_id};
use crate::{FileEntry, workspace_entries::entry_path};
use serde::{Deserialize, Serialize};

pub const REQUEST_LIMIT: usize = 64 * 1024;
mod files;
mod inspection;
pub use files::{CompanionFiles, FolderTransport};
pub use inspection::{FolderInspection, FolderInspector, InspectedFolder, NativeFolderInspection};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderRequest {
    pub user: i64,
    pub project: i64,
    pub action: FolderAction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FolderAction {
    Open {
        path: String,
        expected: Option<String>,
    },
    Prove {
        session: String,
        proof: String,
    },
    File {
        session: String,
        sequence: u64,
        action: FileAction,
    },
    Admit {
        session: String,
        api_url: String,
        delegation: WorkspaceJournalDelegation,
    },
    /// The native worker owns the entire transaction and lease after dispatch.
    Execute {
        session: String,
        api_url: String,
        delegation: WorkspaceJournalDelegation,
        request: super::ExecuteWorkspaceTransaction,
    },
    Advance {
        session: String,
        request: AdvanceWorkspaceTransaction,
    },
    Close {
        session: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum FolderReply {
    Opened {
        session: String,
        challenge: String,
        identity: String,
    },
    Ready {
        identity: String,
        worker: WorkspaceTransactionWorkerKey,
    },
    Admitted,
    Executed {
        outcome: super::WorkspaceTransactionOutcome,
    },
    Advanced {
        response: WorkspaceTransactionAdvanced,
    },
    File {
        outcome: Result<FileValue, String>,
    },
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileAction {
    Canonicalize {
        path: String,
    },
    List {
        path: String,
    },
    Read {
        path: String,
    },
    Bind {
        scope: ResourceScope,
    },
    Create {
        ordinal: usize,
        path: String,
        is_dir: bool,
    },
    ReconcileCreation {
        ordinal: usize,
        path: String,
        is_dir: bool,
    },
    CreatedMatches {
        ordinal: usize,
        path: String,
        is_dir: bool,
    },
    RemoveCreated {
        ordinal: usize,
        path: String,
        is_dir: bool,
    },
    RemovedMatches {
        ordinal: usize,
        path: String,
        is_dir: bool,
    },
    RetainedCanonicalize {
        path: String,
    },
    RetainedList {
        path: String,
    },
    RetainedRead {
        path: String,
    },
    Transfer {
        from: ResourceLocation,
        to: ResourceLocation,
        is_dir: bool,
    },
    RemoveRetained {
        path: String,
        is_dir: bool,
    },
    Finish,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum FileValue {
    Unit,
    Matches(bool),
    Path(String),
    Entries(Vec<FileEntry>),
    Bytes(#[serde(with = "super::encoded_bytes")] Vec<u8>),
}

pub fn fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
pub fn challenge_name(session: &str) -> Result<String, String> {
    if !valid_id(session) {
        return Err("Invalid folder session".into());
    }
    Ok(format!(".openwebide-proof-{session}"))
}
fn normalized(path: &str) -> Result<(), String> {
    if path.len() > 4096
        || path.chars().any(char::is_control)
        || crate::vfs::normalize_vfs_path(path).map_err(|error| error.to_string())? != path
    {
        return Err("Invalid folder-relative path".into());
    }
    Ok(())
}
fn workspace(path: &str, root: bool) -> Result<(), String> {
    normalized(path)?;
    if !(root && path.is_empty()) && entry_path(path)? != path {
        return Err("Invalid workspace file path".into());
    }
    Ok(())
}
impl FolderRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.user <= 0 || self.project <= 0 {
            return Err("Invalid folder account or project".into());
        }
        match &self.action {
            FolderAction::Open { path, expected } => {
                if path.len() > 4096
                    || path.chars().any(char::is_control)
                    || expected.as_deref().is_some_and(|value| !fingerprint(value))
                {
                    return Err("Invalid native folder selection".into());
                }
            }
            FolderAction::Prove { session, proof } => {
                if !valid_id(session) || !fingerprint(proof) {
                    return Err("Invalid folder proof".into());
                }
            }
            FolderAction::Close { session } => {
                if !valid_id(session) {
                    return Err("Invalid folder session".into());
                }
            }
            FolderAction::Admit {
                session,
                api_url,
                delegation,
            }
            | FolderAction::Execute {
                session,
                api_url,
                delegation,
                ..
            } => {
                delegation.validate()?;
                if !valid_id(session)
                    || api_url.len() > 4096
                    || api_url.chars().any(char::is_control)
                    || delegation.user != self.user
                    || delegation.project != self.project
                {
                    return Err("Invalid paired journal admission".into());
                }
            }
            FolderAction::Advance { session, request } => {
                if !valid_id(session)
                    || !valid_id(&request.id)
                    || !valid_id(&request.generation)
                    || request.revision == 0
                {
                    return Err("Invalid paired journal cursor".into());
                }
            }
            FolderAction::File {
                session,
                sequence,
                action,
            } => {
                if !valid_id(session) || *sequence == 0 {
                    return Err("Invalid folder request sequence".into());
                }
                action.validate()?;
            }
        }
        if let FolderAction::Execute {
            delegation,
            request,
            ..
        } = &self.action
            && (request.id != delegation.scope.id
                || request.generation != delegation.scope.generation
                || request
                    .language_grant
                    .as_deref()
                    .is_some_and(|grant| !valid_id(grant)))
        {
            return Err("Invalid paired transaction execution".into());
        }
        Ok(())
    }
}
impl FileAction {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Canonicalize { path } | Self::List { path } => workspace(path, true),
            Self::Read { path } => workspace(path, false),
            Self::Create { ordinal, path, .. }
            | Self::ReconcileCreation { ordinal, path, .. }
            | Self::CreatedMatches { ordinal, path, .. }
            | Self::RemoveCreated { ordinal, path, .. }
            | Self::RemovedMatches { ordinal, path, .. } => {
                if *ordinal >= MAX_RESOURCE_STEPS {
                    return Err("Invalid resource creation ordinal".into());
                }
                workspace(path, false)
            }
            Self::RetainedCanonicalize { path }
            | Self::RetainedList { path }
            | Self::RetainedRead { path }
            | Self::RemoveRetained { path, .. } => normalized(path),
            Self::Bind { scope } => {
                if !valid_id(&scope.transaction) || !fingerprint(&scope.fingerprint) {
                    return Err("Invalid reviewed resource scope".into());
                }
                Ok(())
            }
            Self::Transfer { from, to, .. } => {
                for location in [from, to] {
                    match location {
                        ResourceLocation::Workspace(path) => workspace(path, false)?,
                        ResourceLocation::Retained(slot)
                            if (*slot as usize) < super::MAX_OPERATIONS => {}
                        ResourceLocation::Retained(_) => {
                            return Err("Invalid retained resource slot".into());
                        }
                    }
                }
                Ok(())
            }
            Self::Finish => Ok(()),
        }
    }
    pub fn needs_scope(&self) -> bool {
        !matches!(
            self,
            Self::Canonicalize { .. } | Self::List { .. } | Self::Read { .. } | Self::Bind { .. }
        )
    }
    pub fn mutates(&self) -> bool {
        matches!(
            self,
            Self::Create { .. }
                | Self::ReconcileCreation { .. }
                | Self::RemoveCreated { .. }
                | Self::Transfer { .. }
                | Self::RemoveRetained { .. }
                | Self::Finish
        )
    }
}
