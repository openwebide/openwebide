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
    pub selection: Selection,
    pub invocation: CommandInvocation,
    pub operation: CommandOperation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCommandRequest {
    pub user: Option<i64>,
    pub root: String,
    pub command: AuthorizedCommand,
}
