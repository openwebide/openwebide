//! One-shot durable event jobs. Plugins compute recurrence and interpret outcomes.
use super::{PreparedPlugin, execution::PluginExecutionContext};
use serde::{Deserialize, Serialize};

pub const MAX_JOBS: i64 = 1000;
pub const JOB_PAGE_SIZE: i64 = 64;
pub const JOB_LEASE_SECONDS: i64 = 120;
pub const MAX_JOB_PAGE_BYTES: usize = 1024 * 1024;
pub const MAX_JOB_DELIVERY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_JOB_DELIVERIES: usize = 8;

/// Origin-bound events end with their chat; project events retain only project/model scope.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobScope {
    #[default]
    Origin,
    Project,
}
impl JobScope {
    pub fn is_origin(&self) -> bool {
        *self == Self::Origin
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Origin => "origin",
            Self::Project => "project",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobRequest {
    List {
        #[serde(default)]
        after: i64,
    },
    Read {
        id: i64,
    },
    Schedule {
        #[serde(default, skip_serializing_if = "JobScope::is_origin")]
        scope: JobScope,
        key: String,
        due_at: i64,
        expires_at: Option<i64>,
        event: String,
        payload: serde_json::Value,
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
impl JobRequest {
    pub fn validate(&self, now: i64) -> Result<(), String> {
        if now < 0 {
            return Err("Invalid job clock".into());
        }
        match self {
            Self::List { after } if *after < 0 => Err("Invalid job cursor".into()),
            Self::Read { id } if *id <= 0 => Err("Invalid job ID".into()),
            Self::Cancel { id, revision } | Self::Delete { id, revision }
                if *id <= 0 || *revision <= 0 =>
            {
                Err("Invalid job revision".into())
            }
            Self::Schedule {
                key,
                due_at,
                expires_at,
                event,
                payload,
                ..
            } => {
                if key.is_empty()
                    || key.len() > 128
                    || !key.bytes().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b':')
                    })
                {
                    return Err("Job keys use 1–128 letters, numbers or -_.:".into());
                }
                if *due_at < 0 || expires_at.is_some_and(|expiry| expiry <= *due_at) {
                    return Err("Job time or expiry is invalid".into());
                }
                super::execution::validate_event(event, payload)?;
                if serde_json::to_vec(payload)
                    .map_err(|error| error.to_string())?
                    .len()
                    > 64 * 1024
                {
                    return Err("Durable job data exceeds 64 KiB".into());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Waiting,
    Pending,
    Leased,
    Completed,
    Failed,
    Cancelled,
    Expired,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub id: i64,
    pub revision: i64,
    pub key: String,
    pub due_at: i64,
    pub expires_at: Option<i64>,
    pub event: String,
    pub payload: serde_json::Value,
    pub state: JobState,
    pub attempts: i64,
    pub detail: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobResult {
    pub jobs: Vec<Job>,
    pub next_after: Option<i64>,
}

/// Daemon-only delivery metadata; never passed to plugin code or model arguments.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobDelivery {
    pub user_id: i64,
    pub job: Job,
    pub prepared: PreparedPlugin,
    pub context: PluginExecutionContext,
    pub lease: String,
    pub lease_expires_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobDeliveryPage {
    pub jobs: Vec<JobDelivery>,
    pub next_after: Option<i64>,
}

/// Authenticated daemon RPC. This envelope is outside the plugin SDK capability.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobLease {
    pub host_id: String,
    pub id: i64,
    pub lease: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobServiceRequest {
    Claim {
        host_id: String,
        #[serde(default)]
        after: i64,
    },
    Renew {
        delivery: JobLease,
    },
    Finish {
        delivery: JobLease,
        success: bool,
        detail: String,
    },
    Grant {
        user_id: i64,
        delivery: JobLease,
    },
    Callback {
        user_id: i64,
        request: super::execution::PluginHostRequest,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "action",
    content = "result",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum JobServiceResponse {
    Claimed(JobDeliveryPage),
    Renewed(i64),
    Finished,
    Authorized {
        prepared: Box<PreparedPlugin>,
        context: PluginExecutionContext,
        grant: String,
    },
    Callback(String),
}
