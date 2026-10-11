//! Shared chat domain types.

use crate::{
    Connection, FileDiff, ModelSettings, ServerTransport, TurnTelemetry, UserId, WorkspaceMode,
    format_utc_timestamp,
};
use serde::{Deserialize, Serialize};

/// Appended to a reply when its provider stream ends incomplete (the model
/// was cut off before finishing). Every host that persists a reply appends
/// this marker so the truncation is visible.
pub const REPLY_TRUNCATED_MARKER: &str = "\n\n[reply truncated]";

/// Role of a chat message.
///
/// `Tool` is a transient role used only inside the agent loop to carry a
/// tool result back to the model; it is never persisted (the `messages`
/// table rejects it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "system" => Some(Self::System),
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }
}

/// A single chat message within a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Set by the server on insert; clients may omit it in requests.
    #[serde(default)]
    pub id: i64,
    /// Set by the server on insert; clients may omit it in requests.
    #[serde(default)]
    pub session_id: i64,
    pub role: Role,
    pub content: String,
    /// Set by the server on insert; clients may omit it in requests.
    #[serde(default)]
    pub created_at: i64,
    /// Tool calls this assistant message requested, persisted with their wire ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// For `role = Tool`: the id of the tool call this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Usage for the model call that produced this message, when the
    /// provider reported it. Persisted so a reloaded session's statusline
    /// and `/tokens` output match what a live session showed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TurnTelemetry>,
}

/// A chat session bound to an optional LLM connection and project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSession {
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    /// Automatic titles track recent activity until the user renames the session.
    #[serde(default)]
    pub auto_title: bool,
    #[serde(default)]
    pub title_revision: i64,
    pub id: i64,
    pub name: String,
    pub connection_id: Option<i64>,
    /// Optional system prompt attached to the session.
    #[serde(default)]
    pub system_prompt_id: Option<i64>,
    /// The project this session belongs to.
    #[serde(default)]
    pub project_id: Option<i64>,
    /// The owning user, once accounts exist.
    #[serde(default)]
    pub user_id: Option<UserId>,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkedSession {
    pub session: ChatSession,
    /// The selected prompt is restored as a draft, outside the copied history.
    pub prompt: String,
    pub history: Vec<ConversationEntry>,
}

/// Payload for creating a new chat session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSession {
    #[serde(default)]
    pub auto_title: bool,
    pub name: String,
    pub connection_id: Option<i64>,
    pub system_prompt_id: Option<i64>,
    #[serde(default)]
    pub project_id: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPreferences {
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearch {
    pub project_id: Option<i64>,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchResults {
    pub sessions: Vec<ChatSession>,
    pub explanations: std::collections::BTreeMap<i64, String>,
    pub rewritten_query: Option<String>,
}
impl SessionSearch {
    pub fn validate(&self) -> Result<(), String> {
        if self.query.trim().chars().count() > 256 {
            Err("Session search is limited to 256 characters".into())
        } else {
            Ok(())
        }
    }
}

/// A named system prompt the user can attach to a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemPrompt {
    pub id: i64,
    pub name: String,
    pub content: String,
}

/// A model reported by an LLM provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
}

/// Request to run a chat completion against a saved connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    #[serde(default)]
    pub model_settings: ModelSettings,
    pub connection_id: i64,
    pub system_prompt: Option<String>,
    /// Model override; falls back to the connection's model when absent.
    #[serde(default)]
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    /// Tools offered to the model; empty disables tool calling.
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
}

/// Convert a browser timestamp from milliseconds to whole Unix seconds.
#[allow(clippy::cast_possible_truncation)] // Browser timestamps are milliseconds within i64's range.
pub fn now_seconds(milliseconds: f64) -> i64 {
    (milliseconds / 1000.0) as i64
}

/// A persisted context message is display history, regenerated for each run.
pub const RUN_CONTEXT_PREFIX: &str = "[Open WebIDE run context]\n";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEnvironment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_preferences: Option<BrowserPreferences>,
    pub project_name: Option<String>,
    pub project_root: Option<String>,
    pub mode: Option<WorkspaceMode>,
    pub timestamp: i64,
}

/// Browser-reported defaults for interpreting dates and formatting replies.
/// A per-run snapshot, separate from the command execution host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserPreferences {
    pub timezone: Option<String>,
    pub locale: Option<String>,
    pub hour_cycle: Option<String>,
    /// Minutes east of UTC at capture time (the opposite sign of getTimezoneOffset).
    pub utc_offset_minutes: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEnvironment {
    pub os: String,
    pub shell: String,
}

/// A prepared run before its user message is persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPlan {
    #[serde(default)]
    pub plugin_grants: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub plugin_executables: Vec<crate::plugins::PreparedPlugin>,
    #[serde(default)]
    pub plugin_skills: Vec<crate::ProjectSkill>,
    /// Included only on shared-secret-authenticated native bridge responses.
    #[serde(default)]
    pub transport: ServerTransport,
    #[serde(default)]
    pub environment: RunEnvironment,
    pub user_content: String,
    /// Prior history only; the host appends the new user message after persistence.
    pub request: ChatRequest,
    pub connection: Connection,
    pub kind: RunKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunKind {
    Chat,
    WebChat,
    Agent { project_path: String },
}

/// Append current UTC date and time to the system prompt for zero-turn temporal context.
pub fn with_temporal_context(system_prompt: Option<String>, timestamp_secs: i64) -> String {
    let temporal = format!(
        "Current Date & Time: {}",
        format_utc_timestamp(timestamp_secs)
    );
    match system_prompt {
        Some(base) if !base.trim().is_empty() => format!("{base}\n\n{temporal}"),
        _ => temporal,
    }
}

/// A tool the agent may call, described by a name and a JSON-schema
/// parameter object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the tool's arguments.
    pub parameters: serde_json::Value,
}

/// A tool call requested by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The call's arguments, encoded as a JSON string.
    pub arguments: String,
}

/// The outcome of a tool-capable chat completion: either the model's text
/// reply or the tool calls it wants to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatResponse {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StopReason {
    #[default]
    Complete,
    Length,
}

pub const REPLY_CUT_OFF_MARKER: &str = "\n\n[reply cut off: output token limit reached]";

pub const ESCAPED_REASONING_OPEN: &str = "<think data-escaped>";

pub fn escape_reasoning(reasoning: &str) -> String {
    reasoning
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn with_reasoning(reasoning: &str, answer: &str) -> String {
    if reasoning.is_empty() {
        answer.to_string()
    } else if reasoning.contains(['&', '<', '>']) {
        format!(
            "{ESCAPED_REASONING_OPEN}{}</think>{answer}",
            escape_reasoning(reasoning)
        )
    } else {
        format!("<think>{reasoning}</think>{answer}")
    }
}

pub fn strip_reasoning(content: &str) -> &str {
    content
        .strip_prefix("<think>")
        .or_else(|| content.strip_prefix(ESCAPED_REASONING_OPEN))
        .and_then(|rest| rest.split_once("</think>"))
        .map_or(content, |(_, answer)| answer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ToolStreamChunk {
    Delta(String),
    Reasoning(String),
    Stop(StopReason),
    Usage(TurnTelemetry),
    Response(ChatResponse),
}

/// The wire body for `/api/chat-tools`: the completion plus the usage the
/// provider reported for the call, when it did.
///
/// `usage` is an `Option` so test fakes that only construct a `response`
/// stay valid; the real providers always return `Some`. This is not a wire
/// compatibility mechanism — frontend and backend ship together, and an
/// older bundle or backend does not deserialize this shape at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCompletion {
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub stop_reason: StopReason,
    #[serde(default)]
    pub preamble: String,
    pub response: ChatResponse,
    #[serde(default)]
    pub usage: Option<TurnTelemetry>,
}

/// A persisted agent tool step: the call and, once it finishes, its result.
///
/// Stored separately from chat messages (which feed the LLM context) so a
/// session's tool steps can be reloaded when switching tabs. The step renders
/// right after the user message that started its turn ([`ToolStep::anchor_message_id`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<crate::ToolTiming>,
    /// The agent loop's step id for the call (matches the SSE
    /// `tool_call`/`tool_result` id), not the provider's; unique within the
    /// session. Rows from before step ids carry the provider's `call_N`.
    pub tool_call_id: String,
    /// The tool name (e.g. `write_file`).
    pub name: String,
    /// A human-readable description of the call (e.g. `write foo.txt`).
    pub summary: String,
    /// Whether the call succeeded; `None` while it is still running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// A one-line result summary; `None` while the call is still running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    /// A file diff for edits; `None` for non-edit tools or while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<FileDiff>,
    /// The id of the user message that started this turn.
    pub anchor_message_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<crate::rewind::ProjectCheckpoint>,
}

/// An interrupted tool may have executed before its result reached storage.
pub const UNRECORDED_TOOL_RESULT: &str = "Result not recorded: this tool was interrupted and may or may not have executed. Inspect the current state before retrying; do not assume success or blindly repeat side effects.";

/// Reconstruct model history from persisted assistant calls and tool-step summaries.
pub fn tool_history(messages: Vec<ChatMessage>, steps: &[ToolStep]) -> Vec<ChatMessage> {
    let mut history = Vec::new();
    for mut message in messages {
        if message.role == Role::System && message.content.starts_with(RUN_CONTEXT_PREFIX) {
            continue;
        }
        let matching: Vec<_> = steps
            .iter()
            .filter(|step| {
                step.anchor_message_id == message.id
                    && crate::tasks::task_step_scope(&step.tool_call_id).is_none()
            })
            .collect();
        let indexed = matching
            .iter()
            .any(|step| crate::parse_step_id(&step.tool_call_id).is_some());
        let calls = if message.role == Role::Assistant {
            message.tool_calls.take()
        } else {
            None
        };
        message.tool_calls = calls.clone();
        let session_id = message.session_id;
        history.push(message);
        if let Some(calls) = calls {
            for (index, call) in calls.into_iter().enumerate() {
                let step = if indexed {
                    matching.iter().find(|step| {
                        crate::parse_step_id(&step.tool_call_id)
                            .is_some_and(|(_, _, call_index)| call_index == index)
                    })
                } else {
                    // Old histories used opaque IDs and preserved call order.
                    matching.get(index)
                };
                history.push(ChatMessage {
                    id: 0,
                    session_id,
                    role: Role::Tool,
                    content: step
                        .and_then(|step| step.result_summary.clone())
                        .unwrap_or_else(|| UNRECORDED_TOOL_RESULT.into()),
                    created_at: 0,
                    tool_calls: None,
                    tool_call_id: Some(call.id),
                    usage: None,
                });
            }
        }
    }
    history
}

/// One item in a session's persisted conversation, in the order it occurred:
/// a chat message or an agent tool step. Returned by the message-list
/// endpoint so a reloaded session shows its tool steps again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConversationEntry {
    Task(Box<crate::TaskHistory>),
    /// A user or assistant chat message.
    Message(ChatMessage),
    /// An agent tool step and, once it finished, its result.
    ToolStep(ToolStep),
}

pub fn step_id_prefix(anchor_id: i64) -> String {
    format!("a{anchor_id}t")
}

pub fn parse_step_id(id: &str) -> Option<(i64, usize, usize)> {
    let (anchor, rest) = id.strip_prefix('a')?.split_once('t')?;
    let (turn, index) = rest.split_once('c')?;
    if !turn.bytes().all(|b| b.is_ascii_digit()) || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((
        anchor.parse().ok()?,
        turn.parse().ok()?,
        index.parse().ok()?,
    ))
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn partial_tool_history_preserves_call_pairing_and_marks_unknown_outcomes() {
        let mut message = ChatMessage {
            id: 8,
            session_id: 1,
            role: Role::Assistant,
            content: String::new(),
            created_at: 0,
            tool_calls: Some(
                (0..3)
                    .map(|index| ToolCall {
                        id: format!("wire-{index}"),
                        name: "write_file".into(),
                        arguments: "{}".into(),
                    })
                    .collect(),
            ),
            tool_call_id: None,
            usage: None,
        };
        let steps = vec![
            ToolStep {
                timing: None,
                tool_call_id: "a7t1c1".into(),
                name: "write_file".into(),
                summary: "second call".into(),
                ok: Some(true),
                result_summary: Some("written once".into()),
                diff: None,
                anchor_message_id: 8,
                checkpoint: None,
            },
            ToolStep {
                timing: None,
                tool_call_id: "a7t1c2".into(),
                name: "write_file".into(),
                summary: "third call".into(),
                ok: None,
                result_summary: None,
                diff: None,
                anchor_message_id: 8,
                checkpoint: None,
            },
        ];
        let history = tool_history(vec![message.clone()], &steps);
        assert_eq!(history.len(), 4);
        assert_eq!(history[0], message);
        assert_eq!(history[1].content, UNRECORDED_TOOL_RESULT);
        assert_eq!(history[2].content, "written once");
        assert_eq!(history[3].content, UNRECORDED_TOOL_RESULT);
        for (index, result) in history[1..].iter().enumerate() {
            assert_eq!(result.role, Role::Tool);
            assert_eq!(
                result.tool_call_id.as_deref(),
                Some(format!("wire-{index}").as_str())
            );
        }
        let mut legacy_step = steps[0].clone();
        legacy_step.tool_call_id = "legacy-tool-id".into();
        let mut legacy_message = message.clone();
        legacy_message.tool_calls.as_mut().unwrap().truncate(1);
        assert_eq!(
            tool_history(vec![legacy_message], &[legacy_step])[1].content,
            "written once"
        );
        message.role = Role::User;
        assert_eq!(tool_history(vec![message], &[]).len(), 1);
    }
}
