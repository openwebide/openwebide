//! Account-authorized delegation to a paired execution host's private worker.
//! Bootstrap expiry never expires an admitted worker or permits age-based takeover.
use super::{TransactionProgress, WorkspaceTransactionScope, WorkspaceTransactionWorker, valid_id};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTransactionWorkerKey {
    pub id: String,
    pub secret_digest: String,
}
impl WorkspaceTransactionWorkerKey {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) || !super::companion::fingerprint(&self.secret_digest) {
            return Err("Invalid paired workspace worker key".into());
        }
        Ok(())
    }
}
impl WorkspaceTransactionWorker {
    pub fn delegation_key(&self) -> WorkspaceTransactionWorkerKey {
        WorkspaceTransactionWorkerKey {
            id: self.id.clone(),
            secret_digest: self.secret_digest(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegateWorkspaceTransaction {
    pub generation: String,
    pub revision: u64,
    pub worker: WorkspaceTransactionWorkerKey,
}
impl DelegateWorkspaceTransaction {
    pub fn validate(&self) -> Result<(), String> {
        self.worker.validate()?;
        if !valid_id(&self.generation) || self.revision == 0 {
            return Err("Invalid paired workspace recovery revision".into());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceJournalDelegation {
    pub id: String,
    pub secret: String,
    pub user: i64,
    pub project: i64,
    pub scope: WorkspaceTransactionScope,
}
impl std::fmt::Debug for WorkspaceJournalDelegation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceJournalDelegation")
            .field("id", &self.id)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}
impl WorkspaceJournalDelegation {
    pub fn validate(&self) -> Result<(), String> {
        self.scope.validate()?;
        if !valid_id(&self.id) || !valid_id(&self.secret) || self.user <= 0 || self.project <= 0 {
            return Err("Invalid paired workspace delegation".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum DelegatedWorkspaceJournalOperation {
    /// Private native preflight, before worker acceptance. Never grants writes.
    Inspect,
    Load,
    Admit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review_digest: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language_grant: Option<String>,
    },
    Scope,
    MutationScope,
    Advance {
        revision: u64,
        progress: TransactionProgress,
    },
    Release,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegatedWorkspaceJournalRequest {
    pub delegation: WorkspaceJournalDelegation,
    pub worker: WorkspaceTransactionWorker,
    pub operation: DelegatedWorkspaceJournalOperation,
}
impl DelegatedWorkspaceJournalRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.delegation.validate()?;
        self.worker.validate()?;
        if let DelegatedWorkspaceJournalOperation::Admit {
            review_digest,
            language_grant,
        } = &self.operation
        {
            super::language_review_admission(review_digest.as_deref(), language_grant.as_deref())?;
        }
        if self.delegation.scope.root.mode != crate::WorkspaceMode::Local
            || !super::companion::fingerprint(&self.delegation.scope.root_identity)
            || matches!(
                self.operation,
                DelegatedWorkspaceJournalOperation::Advance { revision: 0, .. }
            )
        {
            return Err("Invalid paired workspace journal request".into());
        }
        Ok(())
    }
}
