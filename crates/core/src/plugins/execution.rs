//! One execution protocol for server hosts and paired local hosts.
use super::PreparedPlugin;
use serde::{Deserialize, Serialize};

/// Native HTTP attaches this marker after plugin-supplied headers.
/// App and bridge control endpoints reject plugin transport credentials.
pub const PLUGIN_HTTP_HEADER: &str = "x-openwebide-plugin";

/// Preserve public validation messages consistently across both transports.
/// Malformed responses and internal error bodies receive a generic message.
pub fn host_rpc_error(status: u16, response: &[u8]) -> String {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublicError {
        error: String,
    }
    if status == 400
        && response.len() <= 4096
        && let Ok(error) = serde_json::from_slice::<PublicError>(response)
        && !error.error.trim().is_empty()
    {
        return error.error;
    }
    format!("Plugin execution host request failed (HTTP {status})")
}

#[cfg(test)]
mod host_error_tests {
    use super::host_rpc_error;

    #[test]
    fn validation_errors_survive_transport_without_exposing_internal_errors() {
        let capacity = br#"{"error":"Too many active plugin invocations."}"#;
        assert_eq!(
            host_rpc_error(400, capacity),
            "Too many active plugin invocations."
        );
        assert_eq!(
            host_rpc_error(500, capacity),
            "Plugin execution host request failed (HTTP 500)"
        );
        for body in [
            b"not JSON".as_slice(),
            br#"{"error":""}"#,
            br#"{"error":"validation","private":"internal"}"#,
            &[b'a'; 4097],
        ] {
            assert_eq!(
                host_rpc_error(400, body),
                "Plugin execution host request failed (HTTP 400)"
            );
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginOperation {
    #[default]
    Tool,
    Context,
    Event,
}

/// Host-owned execution scope. This is never passed to the plugin or model.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginExecutionContext {
    pub project_id: Option<i64>,
    pub session_id: Option<i64>,
    pub primary: Option<crate::ModelSelection>,
    #[serde(default)]
    pub user_action: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginGrantRequest {
    pub context: PluginExecutionContext,
    pub plugins: Vec<PreparedPlugin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventInput {
    pub name: String,
    pub payload: serde_json::Value,
}
pub fn validate_event(name: &str, payload: &serde_json::Value) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    {
        return Err("Event names use 1–64 lowercase letters, digits or underscores".into());
    }
    if serde_json::to_vec(payload)
        .map_err(|error| error.to_string())?
        .len()
        > 256 * 1024
    {
        return Err("Plugin event data exceeds 256 KiB".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextContribution {
    pub prompt: Option<String>,
    pub disabled_tools: Vec<String>,
}

/// Read-only context authority is checked by the runtime and the orchestration
/// facade before forwarding a transport's host request.
pub fn context_request_allowed(capability: &str, payload: &str) -> bool {
    match capability {
        "clock" => true,
        "jobs" => serde_json::from_str::<super::jobs::JobRequest>(payload).is_ok_and(|request| {
            request.validate(0).is_ok()
                && matches!(
                    request,
                    super::jobs::JobRequest::List { .. } | super::jobs::JobRequest::Read { .. }
                )
        }),
        "runs" => serde_json::from_str::<super::runs::RunRequest>(payload).is_ok_and(|request| {
            request.validate().is_ok()
                && matches!(
                    request,
                    super::runs::RunRequest::List { .. } | super::runs::RunRequest::Read { .. }
                )
        }),
        "records" | "collections" => serde_json::from_str::<super::records::RecordRequest>(payload)
            .is_ok_and(|request| {
                request.validate().is_ok()
                    && matches!(
                        request.operation,
                        super::records::RecordOperation::List { .. }
                            | super::records::RecordOperation::Read { .. }
                    )
            }),
        _ => false,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvokePlugin {
    #[serde(default)]
    pub operation: PluginOperation,
    pub prepared: PreparedPlugin,
    pub name: String,
    pub arguments: String,
}
/// Authority stays in the transport envelope and never reaches plugin input.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStartRequest {
    pub grant: String,
    pub session_id: Option<i64>,
    pub call: InvokePlugin,
}
impl InvokePlugin {
    pub fn validate_event(&self) -> Result<(), String> {
        if matches!(self.operation, PluginOperation::Event) {
            if !self.name.is_empty() {
                return Err("Event callbacks cannot supply a tool name".into());
            }
            let input: EventInput =
                serde_json::from_str(&self.arguments).map_err(|error| error.to_string())?;
            validate_event(&input.name, &input.payload)?;
            if !self
                .prepared
                .manifest
                .contributions
                .events
                .contains(&input.name)
            {
                return Err("Event is not declared by this plugin".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuePlugin {
    pub id: String,
    pub sequence: u32,
    pub response: Result<String, String>,
}
/// The authenticated caller supplies the opaque run grant, never the plugin.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHostRequest {
    pub grant: String,
    pub capability: String,
    pub payload: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInvocation {
    pub id: String,
    pub step: PluginStep,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginStep {
    Ready,
    HostCall {
        sequence: u32,
        capability: String,
        payload: String,
    },
    Complete {
        ok: bool,
        content: String,
        summary: String,
    },
    Failed {
        error: String,
    },
}

#[cfg(test)]
mod context_job_tests {
    use super::context_request_allowed;
    #[test]
    fn planning_hooks_can_read_runs_but_cannot_submit_cancel_or_choose_a_scope() {
        for request in [r#"{"action":"list"}"#, r#"{"action":"read","id":1}"#] {
            assert!(context_request_allowed("runs", request));
        }
        for request in [
            r#"{"action":"list","user_id":2}"#,
            r#"{"action":"read","id":0}"#,
            r#"{"action":"submit","key":"next","prompt":"x","target":{"kind":"origin"}}"#,
            r#"{"action":"cancel","id":1,"revision":1}"#,
            r#"{"action":"delete","id":1,"revision":1}"#,
        ] {
            assert!(!context_request_allowed("runs", request));
        }
    }
    #[test]
    fn planning_hooks_can_read_jobs_but_cannot_schedule_cancel_or_choose_a_scope() {
        for request in [r#"{"action":"list"}"#, r#"{"action":"read","id":1}"#] {
            assert!(context_request_allowed("jobs", request));
        }
        for request in [
            r#"{"action":"read","id":0}"#,
            r#"{"action":"list","user_id":2}"#,
            r#"{"action":"schedule","key":"next","due_at":20,"expires_at":null,"event":"due","payload":null}"#,
            r#"{"action":"cancel","id":1,"revision":1}"#,
            r#"{"action":"delete","id":1,"revision":1}"#,
            r#"{"action":"grant","user_id":1}"#,
        ] {
            assert!(!context_request_allowed("jobs", request));
        }
    }
}
