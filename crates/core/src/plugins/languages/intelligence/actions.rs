//! Review and durable outcomes for explicitly approved server actions.
//!
//! An edit is applied before its command, as required by LSP. Once execution
//! has been claimed, missing acknowledgement is never permission to replay it.
use super::{Request, results::WorkspaceEdit};
use crate::plugins::PreparedPlugin;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod transport;
pub use transport::*;

pub const MAX_COMMAND_BYTES: usize = 256 * 1024;
pub const MAX_APPROVAL_BYTES: usize = 8 * 1024 * 1024;

/// Source synchronization for one retained, reviewed action. This carries no
/// executable command or permission to dispatch it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSynchronization {
    pub command: String,
    pub server: String,
    pub origin: String,
    pub review: String,
}
impl ActionSynchronization {
    pub fn from_approval(approval: &ActionApproval) -> Result<Self, String> {
        approval.validate()?;
        let command = approval
            .review
            .command
            .as_ref()
            .ok_or("No reviewed command")?;
        Ok(Self {
            command: command.id.clone(),
            server: command.server.clone(),
            origin: origin_fingerprint(&approval.input)?,
            review: approval.review.fingerprint()?,
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        if !crate::workspace_transaction::valid_id(&self.command)
            || !super::super::text(&self.server, 256)
            || [&self.origin, &self.review].iter().any(|value| {
                value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err("Invalid reviewed action synchronization".into());
        }
        Ok(())
    }
}

pub fn origin_fingerprint(input: &Request) -> Result<String, String> {
    input.validate_archived()?;
    Ok(crate::plugins::content_digest(
        &serde_json::to_vec(&(
            &input.client,
            &input.document,
            &input.language,
            &input.path,
            input.revision,
            &input.source_digest,
        ))
        .map_err(|error| error.to_string())?,
    ))
}

/// A durable claim converted to native invocation data. Account/project/root
/// are supplied by authenticated adapters, never by this envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInvocation {
    pub id: String,
    pub client: String,
    pub command: String,
    pub server: String,
    pub origin: String,
    pub review: String,
    pub approval: String,
}
impl CommandInvocation {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::workspace_transaction::valid_id(&self.id)
            || !crate::workspace_transaction::valid_id(&self.command)
            || !super::super::text(&self.client, 256)
            || !super::super::text(&self.server, 256)
            || [&self.origin, &self.review, &self.approval]
                .iter()
                .any(|digest| {
                    digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        {
            return Err("Invalid language command claim".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandProgress {
    Ready,
    Running,
    Finished { outcome: CommandOutcome },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerCommand {
    /// Opaque action identity retained by the originating native service.
    pub id: String,
    pub server: String,
    /// Display data for approval. Execution uses retained native data, never
    /// command names or arguments supplied back by a client.
    pub name: String,
    #[serde(default)]
    pub arguments: Vec<Value>,
}
impl ServerCommand {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::workspace_transaction::valid_id(&self.id)
            || !super::super::text(&self.server, 256)
            || !super::super::text(&self.name, 1024)
            || serde_json::to_vec(self)
                .map_err(|error| error.to_string())?
                .len()
                > MAX_COMMAND_BYTES
        {
            return Err("Invalid or oversized language-server command review".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReview {
    pub title: String,
    pub edit: WorkspaceEdit,
    pub command: Option<ServerCommand>,
}
impl ActionReview {
    pub fn validate(&self) -> Result<(), String> {
        if !super::super::text(&self.title, 4096)
            || serde_json::to_vec(self)
                .map_err(|error| error.to_string())?
                .len()
                > super::MAX_RESULT_BYTES
        {
            return Err("Invalid or oversized language action review".into());
        }
        self.edit.validate()?;
        if let Some(command) = &self.command {
            command.validate()?;
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> Result<String, String> {
        self.validate()?;
        Ok(crate::plugins::content_digest(
            &serde_json::to_vec(self).map_err(|error| error.to_string())?,
        ))
    }
}

/// The journal ID also identifies the existing workspace transaction containing
/// the exact file plan and before/after editor patch. No second copy is stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionApproval {
    pub id: String,
    pub prepared: PreparedPlugin,
    pub binding_revision: i64,
    pub input: Request,
    pub review: ActionReview,
}
impl ActionApproval {
    pub fn validate(&self) -> Result<(), String> {
        self.prepared
            .validate()
            .map_err(|error| error.to_string())?;
        self.input.validate_archived()?;
        super::declared_server(&self.prepared, &self.input.language)?;
        self.review.validate()?;
        if !crate::workspace_transaction::valid_id(&self.id)
            || self.binding_revision <= 0
            || !matches!(self.input.query, super::Query::ReviewAction { .. })
            || self.review.command.is_none()
            || matches!((&self.input.query, &self.review.command),
                (super::Query::ReviewAction {id}, Some(command)) if id != &command.id)
            || serde_json::to_vec(self)
                .map_err(|error| error.to_string())?
                .len()
                > MAX_APPROVAL_BYTES
        {
            return Err("Invalid language action approval".into());
        }
        Ok(())
    }
    pub fn validate_provider(
        &self,
        registry: &super::super::LanguageRegistry,
        preferences: &super::super::LanguagePreferences,
    ) -> Result<(), String> {
        super::validate_review_provider(
            &self.prepared,
            self.binding_revision,
            &self.input,
            registry,
            preferences,
        )
    }
    pub fn fingerprint(&self) -> Result<String, String> {
        self.validate()?;
        Ok(crate::plugins::content_digest(
            &serde_json::to_vec(self).map_err(|error| error.to_string())?,
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPhase {
    Approved,
    Running,
    Completed,
    Rejected,
    Uncertain,
    Acknowledged,
    Cancelled,
}
impl ActionPhase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Acknowledged | Self::Cancelled)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandOutcome {
    Completed {
        result: Value,
    },
    /// The host established that no command request was sent.
    Rejected {
        message: String,
    },
    /// Includes a server error: commands may have effects before returning one.
    Uncertain {
        message: String,
    },
}
impl CommandOutcome {
    pub fn phase(&self) -> ActionPhase {
        match self {
            Self::Completed { .. } => ActionPhase::Completed,
            Self::Rejected { .. } => ActionPhase::Rejected,
            Self::Uncertain { .. } => ActionPhase::Uncertain,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if serde_json::to_vec(self)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_COMMAND_BYTES
            || matches!(self, Self::Rejected {message} | Self::Uncertain {message}
                if !super::super::text(message, 16 * 1024))
        {
            return Err("Invalid or oversized language command outcome".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRecord {
    pub approval: ActionApproval,
    pub revision: u64,
    pub phase: ActionPhase,
    pub outcome: Option<CommandOutcome>,
    pub acknowledgement: Option<String>,
}
impl ActionRecord {
    pub fn reject_unstarted(mut self, message: String) -> Result<Self, String> {
        self.validate()?;
        let outcome = CommandOutcome::Rejected { message };
        outcome.validate()?;
        if self.phase != ActionPhase::Approved {
            return Err("A claimed command cannot be recorded as unstarted".into());
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Language action revision exhausted")?;
        self.phase = ActionPhase::Rejected;
        self.outcome = Some(outcome);
        Ok(self)
    }
    pub fn invocation(&self) -> Result<CommandInvocation, String> {
        self.validate()?;
        if !matches!(self.phase, ActionPhase::Running | ActionPhase::Uncertain) {
            return Err("Claim the approved action durably before native command execution".into());
        }
        let command = self
            .approval
            .review
            .command
            .as_ref()
            .ok_or("No reviewed server command")?;
        Ok(CommandInvocation {
            id: self.approval.id.clone(),
            client: self.approval.input.client.clone(),
            command: command.id.clone(),
            server: command.server.clone(),
            origin: origin_fingerprint(&self.approval.input)?,
            review: self.approval.review.fingerprint()?,
            approval: self.approval.fingerprint()?,
        })
    }
    pub fn claim(mut self) -> Result<Self, String> {
        self.validate()?;
        if self.phase != ActionPhase::Approved {
            return Err("This language action has already been claimed; load its outcome instead of replaying it".into());
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Language action revision exhausted")?;
        self.phase = ActionPhase::Running;
        Ok(self)
    }
    pub fn finish(mut self, outcome: CommandOutcome) -> Result<Self, String> {
        self.validate()?;
        outcome.validate()?;
        if self.phase != ActionPhase::Running {
            return Err("Only the claimed language action can record a command outcome".into());
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Language action revision exhausted")?;
        self.phase = outcome.phase();
        self.outcome = Some(outcome);
        Ok(self)
    }
    pub fn acknowledge(mut self, note: String) -> Result<Self, String> {
        self.validate()?;
        if !matches!(self.phase, ActionPhase::Rejected | ActionPhase::Uncertain)
            || !super::super::text(&note, 4096)
        {
            return Err(
                "Review an interrupted command's effects before acknowledging its outcome".into(),
            );
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Language action revision exhausted")?;
        self.phase = ActionPhase::Acknowledged;
        self.acknowledgement = Some(note);
        Ok(self)
    }
    pub fn cancel(mut self) -> Result<Self, String> {
        self.validate()?;
        if self.phase != ActionPhase::Approved {
            return Err(
                "A claimed language command cannot be cancelled or retried as an unstarted action"
                    .into(),
            );
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Language action revision exhausted")?;
        self.phase = ActionPhase::Cancelled;
        Ok(self)
    }
    pub fn validate(&self) -> Result<(), String> {
        self.approval.validate()?;
        if self.revision == 0
            || match self.phase {
                ActionPhase::Approved | ActionPhase::Running | ActionPhase::Cancelled => {
                    self.outcome.is_some() || self.acknowledgement.is_some()
                }
                ActionPhase::Completed | ActionPhase::Rejected | ActionPhase::Uncertain => {
                    self.outcome
                        .as_ref()
                        .is_none_or(|outcome| outcome.phase() != self.phase)
                        || self.acknowledgement.is_some()
                }
                ActionPhase::Acknowledged => {
                    self.outcome.as_ref().is_none_or(|outcome| {
                        !matches!(
                            outcome,
                            CommandOutcome::Rejected { .. } | CommandOutcome::Uncertain { .. }
                        )
                    }) || self
                        .acknowledgement
                        .as_ref()
                        .is_none_or(|note| !super::super::text(note, 4096))
                }
            }
        {
            return Err("Invalid language action journal state".into());
        }
        if let Some(outcome) = &self.outcome {
            outcome.validate()?;
        }
        Ok(())
    }
}
