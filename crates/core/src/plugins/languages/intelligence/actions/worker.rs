//! Private native command claims. Browser grants contain only worker commitments.
use super::*;
use crate::workspace_transaction::{
    WorkspaceCommandRecovery, WorkspaceJournalDelegation, WorkspaceTransactionWorker,
    WorkspaceTransactionWorkerKey,
};

/// Retain an attached command's original worker with file admission. This
/// reserves recovery ownership only; Start still requires fresh authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandReservation {
    pub expected: Box<WorkspaceCommandRecovery>,
    pub worker: WorkspaceTransactionWorker,
}
/// Original native cleanup, independent of replaceable file/bootstrap grants.
/// This envelope cannot request a command start or replace its private worker.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalCommandRequest {
    pub authority: crate::workspace_transaction::RecoveryAuthority,
    pub worker: WorkspaceTransactionWorker,
    pub operation: CommandJournalOperation,
}
impl OriginalCommandRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.authority.validate()?;
        self.worker.validate()?;
        if let CommandJournalOperation::AcknowledgeSettlement { expected } = &self.operation {
            expected.validate_settlement()?;
            if !self.authority.matches_scope(
                self.authority.user,
                self.authority.project,
                &expected.scope,
            ) || expected
                .command
                .worker
                .as_ref()
                .is_none_or(|w| w.id != self.worker.id)
            {
                return Err("Original command settlement changed identity".into());
            }
        }
        if let CommandJournalOperation::Outcome { expected } = &self.operation {
            expected.validate()?;
            if !self.authority.matches_scope(
                self.authority.user,
                self.authority.project,
                &expected.scope,
            ) {
                return Err("Original command outcome belongs to another recovery".into());
            }
        }
        if let CommandJournalOperation::Worker { operation } = &self.operation {
            operation.validate()?;
            if matches!(**operation, CommandWorkerOperation::Claim { .. })
                || operation.worker().id != self.worker.id
                || operation.worker().secret != self.worker.secret
                || !self.authority.matches_scope(
                    self.authority.user,
                    self.authority.project,
                    &operation.expected().scope,
                )
            {
                return Err("Original command cleanup cannot start or replace a worker".into());
            }
        }
        Ok(())
    }
}
impl CommandReservation {
    pub fn validate(&self) -> Result<(), String> {
        self.expected.validate()?;
        self.worker.validate()?;
        if self.expected.command.phase != ActionPhase::Approved
            || self.expected.command.worker.is_some()
            || self.expected.command.native_settlement_pending
            || self.expected.command.outcome_digest.is_some()
        {
            return Err("Reserve only the original unstarted command with its file worker".into());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAuthority {
    pub grant: String,
    pub session_id: Option<i64>,
}
impl std::fmt::Debug for CommandAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandAuthority")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "purpose", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandAdmission {
    Start { authority: CommandAuthority },
    Recover,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandWorkerOperation {
    /// The original reserved native owner proved that it never dispatched and
    /// finished cleanup after the user cancelled. This records no outcome.
    ConfirmCancellation {
        expected: Box<WorkspaceCommandRecovery>,
        worker: WorkspaceTransactionWorker,
    },
    /// Original native intent exists, but this worker has not sent an LSP RPC.
    /// This can reconcile a failed admission without renewing dispatch authority.
    RejectUnclaimed {
        expected: Box<WorkspaceCommandRecovery>,
        worker: WorkspaceTransactionWorker,
        message: String,
    },
    Claim {
        expected: Box<WorkspaceCommandRecovery>,
        worker: WorkspaceTransactionWorker,
        admission: CommandAdmission,
    },
    Finish {
        expected: Box<WorkspaceCommandRecovery>,
        worker: WorkspaceTransactionWorker,
        outcome: CommandOutcome,
        cleaned: bool,
    },
    Release {
        expected: Box<WorkspaceCommandRecovery>,
        worker: WorkspaceTransactionWorker,
    },
    Settle {
        expected: Box<WorkspaceCommandRecovery>,
        worker: WorkspaceTransactionWorker,
    },
}
impl CommandWorkerOperation {
    pub fn expected(&self) -> &WorkspaceCommandRecovery {
        match self {
            Self::ConfirmCancellation { expected, .. }
            | Self::RejectUnclaimed { expected, .. }
            | Self::Claim { expected, .. }
            | Self::Finish { expected, .. }
            | Self::Release { expected, .. }
            | Self::Settle { expected, .. } => expected,
        }
    }
    pub fn worker(&self) -> &WorkspaceTransactionWorker {
        match self {
            Self::ConfirmCancellation { worker, .. }
            | Self::RejectUnclaimed { worker, .. }
            | Self::Claim { worker, .. }
            | Self::Finish { worker, .. }
            | Self::Release { worker, .. }
            | Self::Settle { worker, .. } => worker,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        self.expected().validate()?;
        self.worker().validate()?;
        if let Self::Claim {
            admission: CommandAdmission::Start { authority },
            ..
        } = self
            && (!crate::workspace_transaction::valid_id(&authority.grant)
                || authority.session_id.is_some_and(|id| id <= 0))
        {
            return Err("Fresh manual command authority is invalid".into());
        }
        if let Self::RejectUnclaimed { message, .. } = self {
            CommandOutcome::Rejected {
                message: message.clone(),
            }
            .validate()?;
        }
        if let Self::Finish { outcome, .. } = self {
            outcome.validate()?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDelegationRequest {
    pub id: String,
    pub worker: WorkspaceTransactionWorkerKey,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandJournalOperation {
    /// The native owner has durably retired its original fence. Discard only
    /// the retained terminal metadata; never authorize execution or cleanup.
    AcknowledgeSettlement {
        expected: Box<WorkspaceCommandRecovery>,
    },
    Scope,
    /// Read the exact bounded result already retained by the original backend.
    /// This supplies no new execution or cleanup permission.
    Outcome {
        expected: Box<WorkspaceCommandRecovery>,
    },
    Worker {
        operation: Box<CommandWorkerOperation>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegatedCommandRequest {
    pub delegation: WorkspaceJournalDelegation,
    pub worker: WorkspaceTransactionWorker,
    pub operation: CommandJournalOperation,
}
impl DelegatedCommandRequest {
    pub fn validate(&self) -> Result<(), String> {
        if matches!(
            self.operation,
            CommandJournalOperation::AcknowledgeSettlement { .. }
        ) {
            return Err("Settlement acknowledgement requires the original native authority".into());
        }
        self.delegation.validate()?;
        self.worker.validate()?;
        if self.delegation.scope.root.mode != crate::WorkspaceMode::Local {
            return Err("A paired command requires its original Local project".into());
        }
        if let CommandJournalOperation::Outcome { expected } = &self.operation {
            expected.validate()?;
            if expected.scope.id != self.delegation.scope.id
                || expected.scope.generation != self.delegation.scope.generation
                || expected.scope.root != self.delegation.scope.root
                || expected.scope.root_identity != self.delegation.scope.root_identity
            {
                return Err("The original paired command outcome changed scope".into());
            }
        }
        if let CommandJournalOperation::Worker { operation } = &self.operation {
            operation.validate()?;
            if operation.worker().id != self.worker.id
                || operation.worker().secret != self.worker.secret
                || operation.expected().scope.id != self.delegation.scope.id
                || operation.expected().scope.generation != self.delegation.scope.generation
            {
                return Err("The original paired command worker changed".into());
            }
        }
        Ok(())
    }
}
