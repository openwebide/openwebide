//! Owned API envelopes. Authentication and project identity come from the route,
//! not from model/server-supplied identifiers inside these records.
use super::{
    EditorRecoveryPatch, JournalRecord, PreparedJournal, TransactionProgress, WorkspacePlan,
};
use crate::editor::EditorRecoveryRecord;
use serde::{Deserialize, Serialize};

pub const JOURNAL_API_BODY_LIMIT: usize = 192 * 1024 * 1024;
/// One atomic observation of an original journal and its attached command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCommandRecovery {
    pub scope: super::WorkspaceTransactionScope,
    pub activity: super::WorkspaceActivity,
    pub command: crate::plugins::languages::intelligence::actions::CommandRecoveryScope,
}
impl WorkspaceCommandRecovery {
    /// Metadata disposal is permitted only after both original phases settled.
    /// This is not permission to execute or clear an active native owner.
    pub fn validate_settlement(&self) -> Result<(), String> {
        self.validate()?;
        self.activity.ensure_idle()?;
        if !self.command.phase.terminal()
            || self.command.native_settlement_pending
            || !self.command.command_cleanup_confirmed
            || self.command.worker.as_ref().is_none_or(|w| w.active)
        {
            return Err("The original command settlement is not complete".into());
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        self.scope.validate()?;
        self.command.validate()?;
        if self.scope.id != self.command.invocation.id
            || self.activity.language_command
                != (!self.command.phase.terminal() || self.command.native_settlement_pending)
        {
            return Err("Original workspace command recovery changed identity or phase".into());
        }
        Ok(())
    }
}
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
    /// Private native issuer preflight. Never authorizes a filesystem write.
    RecoveryScope,
    /// Private command identity/phase observation. This grants no dispatch or
    /// filesystem permission by itself.
    CommandRecoveryScope,
    Command {
        operation: Box<crate::plugins::languages::intelligence::actions::CommandWorkerOperation>,
    },
    Load,
    /// Trusted runtime admission after obtaining the physical folder gate.
    ClaimWorker {
        scope: super::WorkspaceTransactionScope,
        worker: super::WorkspaceTransactionWorker,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review_digest: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language_grant: Option<String>,
        #[serde(
            default,
            rename = "command_reservation",
            skip_serializing_if = "Option::is_none"
        )]
        command: Option<Box<crate::plugins::languages::intelligence::actions::CommandReservation>>,
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
    /// Confirms disposal of a final receipt, not execution or native cleanup.
    CommandSettlementAcknowledged {
        settlement: Box<WorkspaceCommandRecovery>,
    },
    CommandOutcome {
        recovery: Box<WorkspaceCommandRecovery>,
        outcome: crate::plugins::languages::intelligence::actions::CommandOutcome,
    },
    CommandRecoveryScope {
        recovery: Box<WorkspaceCommandRecovery>,
        authority: Box<super::RecoveryAuthority>,
    },
    CommandScope {
        recovery: Option<Box<WorkspaceCommandRecovery>>,
    },
    RecoveryScope {
        scope: super::WorkspaceTransactionScope,
        authority: Box<super::RecoveryAuthority>,
        /// Atomic observation of this original journal, including a retained
        /// command after its file phase commits. Not a mutation lease.
        activity: super::WorkspaceActivity,
    },
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
impl WorkspaceJournalResponse {
    pub fn command_settlement_acknowledged(
        &self,
        expected: &WorkspaceCommandRecovery,
    ) -> Result<(), String> {
        expected.validate_settlement()?;
        if let Self::CommandSettlementAcknowledged { settlement } = self
            && settlement.as_ref() == expected
        {
            return Ok(());
        }
        Err("The original command settlement acknowledgement changed".into())
    }
    pub fn command_outcome(
        &self,
        expected: &WorkspaceCommandRecovery,
    ) -> Result<&crate::plugins::languages::intelligence::actions::CommandOutcome, String> {
        use crate::plugins::languages::intelligence::actions::{ActionPhase, CommandOutcome};
        expected.validate()?;
        let Self::CommandOutcome { recovery, outcome } = self else {
            return Err("Original command outcome is unavailable".into());
        };
        recovery.validate()?;
        outcome.validate()?;
        let digest = crate::plugins::content_digest(
            &serde_json::to_vec(outcome).map_err(|_| "Invalid original command outcome")?,
        );
        if recovery.as_ref() != expected
            || recovery.command.outcome_digest.as_deref() != Some(digest.as_str())
            || !(recovery.command.phase == outcome.phase()
                || (recovery.command.phase == ActionPhase::Acknowledged
                    && matches!(
                        outcome,
                        CommandOutcome::Rejected { .. } | CommandOutcome::Uncertain { .. }
                    )))
        {
            return Err("Original command outcome changed identity, phase or fingerprint".into());
        }
        Ok(outcome)
    }
    /// Validate a private command mutation acknowledgement without granting any
    /// new dispatch authority or accepting a private issuer in its place.
    pub fn command_worker(
        &self,
        operation: &crate::plugins::languages::intelligence::actions::CommandWorkerOperation,
    ) -> Result<&WorkspaceCommandRecovery, String> {
        use crate::plugins::languages::intelligence::actions::{
            ActionPhase, CommandWorkerOperation,
        };
        operation.validate()?;
        let Self::CommandScope {
            recovery: Some(current),
        } = self
        else {
            return Err("Original native command acknowledgement is unavailable".into());
        };
        current.validate()?;
        let expected = operation.expected();
        let original = current.scope.id == expected.scope.id
            && current.scope.generation == expected.scope.generation
            && current.scope.root_identity == expected.scope.root_identity
            && current.scope.root == expected.scope.root
            && current.command.invocation == expected.command.invocation
            && current.command.revision >= expected.command.revision
            && expected
                .command
                .outcome_digest
                .as_ref()
                .is_none_or(|digest| current.command.outcome_digest.as_ref() == Some(digest));
        let worker = current
            .command
            .worker
            .as_ref()
            .filter(|worker| worker.id == operation.worker().id)
            .ok_or("Original native command worker changed")?;
        let valid = match operation {
            CommandWorkerOperation::ConfirmCancellation { .. } => {
                worker.active
                    && current.command.native_settlement_pending
                    && current.command.command_cleanup_confirmed
                    && !current.activity.file_recovery
                    && current.scope == expected.scope
                    && current.command.phase == ActionPhase::Cancelled
                    && expected.command.phase == ActionPhase::Cancelled
                    && current.command.revision == expected.command.revision
                    && current.command.outcome_digest.is_none()
            }
            CommandWorkerOperation::RejectUnclaimed { message, .. } => {
                let outcome =
                    crate::plugins::languages::intelligence::actions::CommandOutcome::Rejected {
                        message: message.clone(),
                    };
                let digest = crate::plugins::content_digest(
                    &serde_json::to_vec(&outcome).map_err(|_| "Invalid command outcome")?,
                );
                worker.active
                    && current.command.native_settlement_pending
                    && current.command.command_cleanup_confirmed
                    && !current.activity.file_recovery
                    && current.command.phase == ActionPhase::Rejected
                    && current.command.revision
                        == expected
                            .command
                            .revision
                            .checked_add(1)
                            .ok_or("Invalid command revision")?
                    && current.command.outcome_digest.as_deref() == Some(digest.as_str())
            }
            CommandWorkerOperation::Claim { .. } => {
                worker.active
                    && current.command.native_settlement_pending
                    && current.scope == expected.scope
                    && current.activity.file_recovery == expected.activity.file_recovery
                    && current.command.revision == expected.command.revision
                    && current.command.phase == expected.command.phase
                    && current.command.outcome_digest == expected.command.outcome_digest
                    && current.command.command_cleanup_confirmed
                        == expected.command.command_cleanup_confirmed
            }
            CommandWorkerOperation::Finish {
                outcome, cleaned, ..
            } => {
                let digest = crate::plugins::content_digest(
                    &serde_json::to_vec(outcome).map_err(|_| "Invalid command outcome")?,
                );
                worker.active
                    && current.command.native_settlement_pending
                    && (expected.command.outcome_digest.is_some()
                        || expected
                            .command
                            .revision
                            .checked_add(1)
                            .is_some_and(|revision| current.command.revision >= revision))
                    && current.command.outcome_digest.as_deref() == Some(digest.as_str())
                    && ((!cleaned && !expected.command.command_cleanup_confirmed)
                        || current.command.command_cleanup_confirmed)
                    && (current.command.phase == outcome.phase()
                        || (current.command.phase == ActionPhase::Acknowledged
                            && matches!(
                                outcome.phase(),
                                ActionPhase::Rejected | ActionPhase::Uncertain
                            )))
            }
            CommandWorkerOperation::Release { .. } => {
                !worker.active
                    && current.command.native_settlement_pending
                    && current.command.command_cleanup_confirmed
                    && !current.activity.file_recovery
                    && matches!(
                        current.command.phase,
                        ActionPhase::Rejected | ActionPhase::Uncertain | ActionPhase::Acknowledged
                    )
            }
            CommandWorkerOperation::Settle { .. } => {
                !worker.active
                    && !current.command.native_settlement_pending
                    && current.command.command_cleanup_confirmed
                    && current.activity.ensure_idle().is_ok()
                    && current.command.phase.terminal()
            }
        };
        if !original || !valid {
            return Err("Original native command acknowledgement changed identity or phase".into());
        }
        Ok(current)
    }
    pub fn command_recovery(
        &self,
        user: i64,
        project: i64,
        id: &str,
    ) -> Result<(&WorkspaceCommandRecovery, &super::RecoveryAuthority), String> {
        let Self::CommandRecoveryScope {
            recovery,
            authority,
        } = self
        else {
            return Err("Original private command recovery is unavailable".into());
        };
        recovery.validate()?;
        if recovery.scope.id != id || !authority.matches_scope(user, project, &recovery.scope) {
            return Err("Original private command recovery changed identity".into());
        }
        Ok((recovery, authority))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRootIdentity {
    pub identity: String,
}
