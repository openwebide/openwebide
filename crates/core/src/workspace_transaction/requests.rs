//! Owned API envelopes. Authentication and project identity come from the route,
//! not from model/server-supplied identifiers inside these records.
use super::{
    EditorRecoveryPatch, JournalRecord, PreparedJournal, TransactionProgress, WorkspacePlan,
};
use crate::editor::EditorRecoveryRecord;
use serde::{Deserialize, Serialize};

pub const JOURNAL_API_BODY_LIMIT: usize = 192 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginWorkspaceTransaction {
    pub id: String,
    pub plan: WorkspacePlan,
    pub editor: EditorRecoveryPatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_review: Option<Box<super::LanguageReview>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTransactionPrepared {
    pub journal: PreparedJournal,
    pub editor_revision: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvanceWorkspaceTransaction {
    pub id: String,
    pub generation: String,
    pub revision: u64,
    pub progress: TransactionProgress,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTransactionAdvanced {
    pub generation: String,
    pub revision: u64,
    pub editor: Option<EditorRecoveryRecord>,
}
impl WorkspaceTransactionAdvanced {
    pub fn validate(&self, generation: &str, revision: u64) -> Result<(), String> {
        if !valid_id(generation)
            || self.generation != generation
            || revision.checked_add(1) != Some(self.revision)
        {
            return Err("Workspace recovery acknowledgement changed identity or revision".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTransactionState {
    pub journal: JournalRecord,
    pub editor: EditorRecoveryPatch,
    pub editor_revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_review: Option<Box<super::LanguageReview>>,
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTransactionAction {
    Apply,
    Recover,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteWorkspaceTransaction {
    pub id: String,
    pub generation: String,
    pub action: WorkspaceTransactionAction,
    /// Fresh UI authority for first admission only. Never stored in recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_grant: Option<String>,
}

/// Backend-authenticated dispatch; the browser never selects its account scope.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeWorkspaceTransaction {
    pub user: i64,
    pub project: i64,
    pub request: ExecuteWorkspaceTransaction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTransactionOutcome {
    pub generation: String,
    pub phase: Option<super::TransactionPhase>,
    pub error: Option<super::TransactionError>,
}
impl WorkspaceTransactionOutcome {
    pub fn validate(&self, generation: &str) -> Result<(), String> {
        if !valid_id(generation) || self.generation != generation {
            return Err("Workspace execution acknowledgement changed identity".into());
        }
        Ok(())
    }
}

/// Only the authenticated backend service interface accepts these envelopes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceJournalRequest {
    pub user: i64,
    pub project: i64,
    pub id: String,
    pub operation: WorkspaceJournalOperation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceJournalOperation {
    Scope,
    Load,
    /// Trusted runtime admission after obtaining the physical folder gate.
    ClaimWorker {
        scope: super::WorkspaceTransactionScope,
        worker: super::WorkspaceTransactionWorker,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review_digest: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language_grant: Option<String>,
    },
    ReleaseWorker {
        generation: String,
        worker: super::WorkspaceTransactionWorker,
    },
    AdvanceOwned {
        worker: super::WorkspaceTransactionWorker,
        generation: String,
        revision: u64,
        progress: TransactionProgress,
    },
    /// Legacy native callbacks are decoded for an explicit rejection. Native
    /// execution must first claim a worker and use `AdvanceOwned`.
    Advance {
        generation: String,
        revision: u64,
        progress: TransactionProgress,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceJournalResponse {
    WorkerClaimed {
        scope: super::WorkspaceTransactionScope,
    },
    WorkerReleased {
        generation: String,
        worker: String,
    },
    Scope {
        scope: Option<super::WorkspaceTransactionScope>,
    },
    Loaded {
        state: Option<Box<WorkspaceTransactionState>>,
    },
    Advanced {
        response: WorkspaceTransactionAdvanced,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRootIdentity {
    pub identity: String,
}
