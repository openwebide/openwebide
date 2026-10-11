//! Durable prompt submission primitives. Plugins own recurrence and result policy.
use serde::{Deserialize, Serialize};

pub const MAX_RUNS: i64 = 1000;
pub const RUN_PAGE_SIZE: i64 = 16;

/// A scoped record revision that must still exist when a new prompt is queued.
/// This prevents stale event handlers from submitting work after an edit/delete.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPrerequisite {
    pub capability: String,
    pub collection: String,
    pub id: i64,
    pub revision: i64,
}

/// A declared event receives `{run: PluginRun, data: payload}` after termination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCompletion {
    pub event: String,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunTarget {
    Origin,
    Session { id: i64 },
    New { title: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunRequest {
    List {
        #[serde(default)]
        after: i64,
    },
    Read {
        id: i64,
    },
    Submit {
        key: String,
        prompt: String,
        target: RunTarget,
        #[serde(default)]
        model: Option<crate::ModelSelection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completion: Option<RunCompletion>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prerequisites: Vec<RunPrerequisite>,
    },
    Cancel {
        id: i64,
        revision: i64,
    },
    Delete {
        id: i64,
        revision: i64,
    },
}
impl RunRequest {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::List { after } if *after < 0 => Err("Invalid run cursor".into()),
            Self::Read { id } if *id <= 0 => Err("Invalid run ID".into()),
            Self::Cancel { id, revision } | Self::Delete { id, revision }
                if *id <= 0 || *revision <= 0 =>
            {
                Err("Invalid run revision".into())
            }
            Self::Submit {
                key,
                prompt,
                target,
                model,
                completion,
                prerequisites,
            } => {
                if prerequisites.len() > 8 {
                    return Err("Run submissions accept at most eight record prerequisites".into());
                }
                for condition in prerequisites {
                    if !matches!(condition.capability.as_str(), "records" | "collections")
                        || condition.revision <= 0
                    {
                        return Err("Invalid run record prerequisite".into());
                    }
                    super::records::RecordRequest {
                        collection: condition.collection.clone(),
                        operation: super::records::RecordOperation::Read { id: condition.id },
                    }
                    .validate()?;
                }
                if key.is_empty()
                    || key.len() > 128
                    || !key.bytes().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b':')
                    })
                {
                    return Err("Run keys use 1–128 letters, numbers or -_.:".into());
                }
                if prompt.trim().is_empty() || prompt.len() > 32 * 1024 {
                    return Err("Run prompts need 1–32 KiB of text".into());
                }
                match target {
                    RunTarget::Session { id } if *id <= 0 => {
                        return Err("Invalid run conversation".into());
                    }
                    RunTarget::New { title }
                        if title.trim().is_empty()
                            || title.chars().count() > 120
                            || title.chars().any(char::is_control) =>
                    {
                        return Err("Conversation titles need 1–120 characters".into());
                    }
                    _ => (),
                }
                if model.as_ref().is_some_and(|model| {
                    model.server_id <= 0
                        || model.model.trim().is_empty()
                        || model.model.len() > 256
                        || model.model.chars().any(char::is_control)
                }) {
                    return Err("Choose a valid run model".into());
                }
                if let Some(completion) = completion {
                    super::execution::validate_event(&completion.event, &completion.payload)?;
                    if serde_json::to_vec(&completion.payload)
                        .map_err(|error| error.to_string())?
                        .len()
                        > 64 * 1024
                    {
                        return Err("Run completion data exceeds 64 KiB".into());
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Pending,
    Leased,
    Running,
    Blocked,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}
impl RunState {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRun {
    pub id: i64,
    pub revision: i64,
    pub key: String,
    pub session_id: Option<i64>,
    pub state: RunState,
    pub created_at: i64,
    pub detail: String,
    pub message_id: Option<i64>,
    pub permission_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunResult {
    pub runs: Vec<PluginRun>,
    pub next_after: Option<i64>,
}

/// Host-only delivery authority; not part of the SDK request envelope.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLease {
    pub host_id: String,
    pub id: i64,
    pub lease: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDelivery {
    pub user_id: i64,
    pub run: PluginRun,
    pub prompt: crate::QueuedPrompt,
    pub host_path: Option<String>,
    pub lease: RunLease,
    pub lease_expires_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDeliveryPage {
    pub runs: Vec<RunDelivery>,
    pub next_after: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLeaseStatus {
    pub expires_at: i64,
    pub cancel_requested: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReport {
    pub state: RunState,
    pub detail: String,
    pub permission_id: Option<String>,
}
impl RunReport {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.state,
            RunState::Running
                | RunState::Blocked
                | RunState::Completed
                | RunState::Failed
                | RunState::Cancelled
        ) || self.detail.len() > 4096
            || self.permission_id.as_ref().is_some_and(|id| {
                id.is_empty() || id.len() > 256 || id.chars().any(char::is_control)
            })
            || (self.state != RunState::Blocked && self.permission_id.is_some())
        {
            return Err("Invalid plugin run report".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunServiceRequest {
    Claim {
        host_id: String,
        #[serde(default)]
        after: i64,
    },
    Renew {
        delivery: RunLease,
    },
    Report {
        delivery: RunLease,
        report: RunReport,
    },
    Release {
        delivery: RunLease,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "action",
    content = "result",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RunServiceResponse {
    Claimed(RunDeliveryPage),
    Renewed(RunLeaseStatus),
    Reported,
    Released,
}

impl RunDelivery {
    pub fn run_id(&self) -> String {
        format!("plugin-run-{}-{}", self.lease.id, self.lease.lease)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prerequisites_are_bounded_scoped_and_omitted_for_legacy_submissions() {
        let old = json!({"action":"submit","key":"old","prompt":"Work","target":{"kind":"origin"},"model":null});
        let mut request: RunRequest = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), old);
        let condition = RunPrerequisite {
            capability: "collections".into(),
            collection: "tasks".into(),
            id: 1,
            revision: 1,
        };
        for invalid in [
            RunPrerequisite {
                capability: "http".into(),
                ..condition.clone()
            },
            RunPrerequisite {
                collection: "../tasks".into(),
                ..condition.clone()
            },
            RunPrerequisite {
                id: 0,
                ..condition.clone()
            },
            RunPrerequisite {
                revision: 0,
                ..condition.clone()
            },
        ] {
            if let RunRequest::Submit { prerequisites, .. } = &mut request {
                *prerequisites = vec![invalid];
            }
            assert!(request.validate().is_err());
        }
        if let RunRequest::Submit { prerequisites, .. } = &mut request {
            *prerequisites = vec![condition.clone(); 8];
        }
        request.validate().unwrap();
        if let RunRequest::Submit { prerequisites, .. } = &mut request {
            prerequisites.push(condition);
        }
        assert!(request.validate().is_err());
        assert!(serde_json::from_value::<RunPrerequisite>(json!({"capability":"records","collection":"notes","id":1,"revision":1,"user_id":42})).is_err());
    }
}
