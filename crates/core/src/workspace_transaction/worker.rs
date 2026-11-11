//! Durable worker credentials. An adapter claims only while holding the physical
//! folder gate and keeps that gate through every accepted mutation and release.
//! Replacing a worker requires that same gate, never an elapsed-time takeover.
use super::valid_id;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTransactionWorkerState {
    pub id: String,
    pub active: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTransactionWorker {
    pub id: String,
    pub secret: String,
}
impl std::fmt::Debug for WorkspaceTransactionWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceTransactionWorker")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
impl WorkspaceTransactionWorker {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) || !valid_id(&self.secret) {
            return Err("Invalid workspace worker credentials".into());
        }
        Ok(())
    }
    pub fn state(&self) -> WorkspaceTransactionWorkerState {
        WorkspaceTransactionWorkerState {
            id: self.id.clone(),
            active: true,
        }
    }
    pub fn secret_digest(&self) -> String {
        crate::plugins::content_digest(self.secret.as_bytes())
    }
}
