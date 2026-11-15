//! Transport envelopes contain durable identities, never executable commands.
use super::*;
use crate::plugins::languages::intelligence::{Call, Selection};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionJournalRequest {
    Load {
        id: Option<String>,
    },
    Approve {
        call: Box<Call>,
        approval: Box<ActionApproval>,
    },
    Claim {
        id: String,
        revision: u64,
        call: Box<Call>,
    },
    Finish {
        id: String,
        revision: u64,
        outcome: CommandOutcome,
    },
    RejectUnstarted {
        id: String,
        revision: u64,
        message: String,
    },
    Acknowledge {
        id: String,
        revision: u64,
        note: String,
    },
    Cancel {
        id: String,
        revision: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOperation {
    Start,
    Status,
    Stop,
    Cancel,
}

impl CommandOperation {
    /// Observation and fencing remain valid after native completion. They never
    /// turn an archived invocation into fresh permission to start it.
    pub fn invocation(self, record: &ActionRecord) -> Result<CommandInvocation, String> {
        record.validate()?;
        match self {
            Self::Cancel if record.phase == ActionPhase::Cancelled => {
                record.approval.command_invocation()
            }
            Self::Cancel => Err("Cancel only an already cancelled unstarted action".into()),
            Self::Start if record.phase == ActionPhase::Running => {
                record.approval.command_invocation()
            }
            Self::Start => Err("A command with a recorded outcome cannot be started again".into()),
            Self::Status | Self::Stop
                if matches!(record.phase, ActionPhase::Approved | ActionPhase::Cancelled) =>
            {
                Err("The approved command was not claimed".into())
            }
            Self::Status | Self::Stop => record.approval.command_invocation(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDispatch {
    pub id: String,
    pub operation: CommandOperation,
    /// Fresh manual authority is required to start. Inspecting or stopping an
    /// owned claim remains possible after its provider is disabled or removed.
    pub call: Option<Box<Call>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedCommand {
    pub user: i64,
    pub project: i64,
    /// Only Start carries provider/source data. Controls address original work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Selection>,
    pub invocation: CommandInvocation,
    pub operation: CommandOperation,
    /// A backend-recorded result can be observed without contacting the old
    /// host. Stop is resolved here only after positive native cleanup.
    /// This is a response to authorization, never native execution authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<CommandProgress>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCommandRequest {
    pub user: Option<i64>,
    pub root: String,
    pub command: AuthorizedCommand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<CommandAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<PairedCommandJournalRequest>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedCommandJournalRequest {
    pub api_url: String,
    pub delegation: crate::workspace_transaction::WorkspaceJournalDelegation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandPreparation {
    Worker {
        worker: crate::workspace_transaction::WorkspaceTransactionWorkerKey,
    },
    Existing {
        progress: CommandProgress,
    },
}
