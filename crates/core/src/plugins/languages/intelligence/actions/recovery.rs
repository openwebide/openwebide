//! Bounded original command identity and phase, independent of provider payloads.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRecoveryScope {
    pub invocation: CommandInvocation,
    pub revision: u64,
    pub phase: ActionPhase,
    pub outcome_digest: Option<String>,
    #[serde(default)]
    pub worker: Option<crate::workspace_transaction::WorkspaceTransactionWorkerState>,
    #[serde(default)]
    pub native_settlement_pending: bool,
    #[serde(default)]
    pub command_cleanup_confirmed: bool,
}
impl CommandRecoveryScope {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        let has_outcome = matches!(
            self.phase,
            ActionPhase::Completed
                | ActionPhase::Rejected
                | ActionPhase::Uncertain
                | ActionPhase::Acknowledged
        );
        if self.revision == 0
            || self.worker.as_ref().is_some_and(|worker| match self.phase {
                ActionPhase::Approved => {
                    !worker.active
                        || !self.native_settlement_pending
                        || self.command_cleanup_confirmed
                }
                ActionPhase::Cancelled if !self.command_cleanup_confirmed => {
                    !worker.active || !self.native_settlement_pending
                }
                _ => false,
            })
            || (self.command_cleanup_confirmed
                && self.outcome_digest.is_none()
                && self.phase != ActionPhase::Cancelled)
            || self.worker.as_ref().is_some_and(|worker| {
                !crate::workspace_transaction::valid_id(&worker.id)
                    || (worker.active && !self.native_settlement_pending)
            })
            || ((self.native_settlement_pending || self.command_cleanup_confirmed)
                && self.worker.is_none())
            || self.outcome_digest.is_some() != has_outcome
            || self
                .outcome_digest
                .as_deref()
                .is_some_and(|digest| !crate::workspace_transaction::companion::fingerprint(digest))
        {
            return Err("Original language command recovery metadata is invalid".into());
        }
        Ok(())
    }
}

/// Public recovery state contains no provider executable, source or command
/// arguments. Its immutable invocation identifies the original approved work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRecovery {
    pub command: CommandRecoveryScope,
    pub outcome: Option<CommandOutcome>,
    pub acknowledgement: Option<String>,
}
impl ActionRecovery {
    pub fn validate(&self) -> Result<(), String> {
        self.command.validate()?;
        if let Some(outcome) = &self.outcome {
            outcome.validate()?;
        }
        let digest = self
            .outcome
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|error| error.to_string())?
            .map(|bytes| crate::plugins::content_digest(&bytes));
        if digest != self.command.outcome_digest
            || self.outcome.as_ref().is_some_and(|outcome| {
                self.command.phase != ActionPhase::Acknowledged
                    && outcome.phase() != self.command.phase
            })
            || (self.command.phase == ActionPhase::Acknowledged) != self.acknowledgement.is_some()
            || self
                .acknowledgement
                .as_deref()
                .is_some_and(|note| !super::super::super::text(note, 4096))
            || (self.command.phase == ActionPhase::Acknowledged
                && !matches!(
                    self.outcome,
                    Some(CommandOutcome::Rejected { .. } | CommandOutcome::Uncertain { .. })
                ))
        {
            return Err("Original command recovery outcome is invalid".into());
        }
        Ok(())
    }
    pub fn pending(&self) -> bool {
        !self.command.phase.terminal() || self.command.native_settlement_pending
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionRecoveryRequest {
    Load {
        id: Option<String>,
    },
    Finish {
        id: String,
        revision: u64,
        outcome: CommandOutcome,
    },
    Acknowledge {
        id: String,
        revision: u64,
        note: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_phase_requires_the_original_command_and_its_outcome_fingerprint() {
        let invocation = CommandInvocation {
            id: "a".repeat(32),
            client: "window".into(),
            command: "b".repeat(32),
            server: "service".into(),
            origin: "c".repeat(64),
            review: "d".repeat(64),
            approval: "e".repeat(64),
        };
        for phase in [
            ActionPhase::Approved,
            ActionPhase::Running,
            ActionPhase::Completed,
            ActionPhase::Rejected,
            ActionPhase::Uncertain,
            ActionPhase::Acknowledged,
            ActionPhase::Cancelled,
        ] {
            let has_outcome = matches!(
                phase,
                ActionPhase::Completed
                    | ActionPhase::Rejected
                    | ActionPhase::Uncertain
                    | ActionPhase::Acknowledged
            );
            let mut scope = CommandRecoveryScope {
                invocation: invocation.clone(),
                revision: 4,
                phase,
                outcome_digest: has_outcome.then(|| "f".repeat(64)),
                worker: None,
                native_settlement_pending: false,
                command_cleanup_confirmed: false,
            };
            scope.validate().unwrap();
            let roundtrip: CommandRecoveryScope =
                serde_json::from_str(&serde_json::to_string(&scope).unwrap()).unwrap();
            assert_eq!(roundtrip, scope);
            scope.outcome_digest = (!has_outcome).then(|| "f".repeat(64));
            assert!(scope.validate().is_err());
            scope.outcome_digest = has_outcome.then(|| "f".repeat(64));
            scope.revision = 0;
            assert!(scope.validate().is_err());
            scope.revision = 4;
            scope.invocation.command = "different".into();
            assert!(scope.validate().is_err());
        }
    }
}
