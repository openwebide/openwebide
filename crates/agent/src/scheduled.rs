//! Shared host capability adaptation; a verified local binding supplies native primitives.
pub fn authorize_host_plan(plan: &mut openwebide_core::RunPlan, path: &str) {
    plan.environment.project_root = Some(path.into());
    if plan.request.model_settings.tools == Some(false) {
        return;
    }
    let mut tools = plan.request.tools.clone();
    for tool in crate::session::tools_for_host(true)
        .into_iter()
        .filter(|tool| crate::policy::BRIDGE_TOOLS.contains(&tool.name.as_str()))
    {
        if !tools.iter().any(|existing| existing.name == tool.name) {
            tools.push(tool);
        }
    }
    plan.connection.tool_selection.apply(&mut tools);
    plan.request.tools = tools;
    plan.kind = if plan.request.tools.is_empty() {
        openwebide_core::RunKind::Chat
    } else {
        openwebide_core::RunKind::Agent {
            project_path: path.into(),
        }
    };
}
use crate::{ToolExecutor, ToolOutcome, ToolPreview};
use openwebide_core::{
    FileDiff, ToolCall, ToolDefinition,
    rewind::ProjectSnapshot,
    scheduled::{ScheduledTask, TaskCommand},
};
use std::future::Future;
pub const TOOL_NAMES: &[&str] = &[
    "monitor",
    "schedule_list",
    "schedule_create",
    "schedule_update",
    "schedule_delete",
];
pub fn is_scheduled_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}
pub fn configure(tools: &mut Vec<ToolDefinition>) {
    tools.retain(|tool| !is_scheduled_tool(&tool.name));
    tools.extend(TOOL_NAMES.iter().map(|name| definition(name)));
}
pub fn definition(name: &str) -> ToolDefinition {
    if name == "monitor" {
        return ToolDefinition {
            name: name.into(),
            description: "Check later in this conversation. Start requires prompt and delay_seconds (5–86399); interval_seconds (5–86400), max_checks (1–24) optional. Cancel requires id/revision and prevents future checks. Host-owned checks survive browser closure, use normal approvals, and stop on failure or after 24h. Local projects require a paired host.".into(),
            parameters: serde_json::json!({"type":"object","properties":{
                "action":{"type":"string","enum":["start","list","cancel"]},
                "prompt":{"type":"string"},"delay_seconds":{"type":"integer"},
                "interval_seconds":{"type":"integer","default":600},"max_checks":{"type":"integer","default":1},
                "id":{"type":"integer"},"revision":{"type":"integer"}
            },"required":["action"],"additionalProperties":false}),
        };
    }
    let schedule = serde_json::json!({"type":"object","properties":{
        "kind":{"type":"string","enum":["once","cron"]},
        "at":{"type":"integer","description":"once: UTC epoch seconds (required)"},
        "expression":{"type":"string","description":"cron: five fields (required)"},
        "timezone":{"type":"string","description":"cron: IANA timezone (required)"}
    },"required":["kind"],"additionalProperties":false});
    let draft = serde_json::json!({"type":"object","properties":{"title":{"type":"string"},"prompt":{"type":"string"},"session_target":{"type":"string","enum":["existing","new","latest"],"description":"Default existing; new each run; latest at delivery"},"session_id":{"type":"integer","description":"Existing target: 0=this session"},"model":{"description":"Omit/null: current session model","oneOf":[{"type":"null"},{"type":"object","properties":{"server_id":{"type":"integer"},"model":{"type":"string"}},"required":["server_id","model"],"additionalProperties":false}]},"schedule":schedule,"enabled":{"type":"boolean"}},"required":["prompt","schedule","enabled"],"additionalProperties":false});
    let parameters = match name {
        "schedule_list" => {
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false})
        }
        "schedule_create" => {
            serde_json::json!({"type":"object","properties":{"draft":draft},"required":["draft"],"additionalProperties":false})
        }
        "schedule_update" => {
            serde_json::json!({"type":"object","properties":{"id":{"type":"integer"},"revision":{"type":"integer"},"draft":draft},"required":["id","revision","draft"],"additionalProperties":false})
        }
        _ => {
            serde_json::json!({"type":"object","properties":{"id":{"type":"integer"},"revision":{"type":"integer"}},"required":["id","revision"],"additionalProperties":false})
        }
    };
    ToolDefinition{name:name.into(),description:match name{"schedule_list"=>"List saved scheduled prompts for this project or projectless chat.","schedule_create"=>"Schedule a future prompt with normal approvals. Omit title for automatic naming. Local projects require a paired host.","schedule_update"=>"Replace a saved scheduled prompt using its current revision. Set enabled false to pause. Changes cancel undelivered prompts.",_=>"Remove a scheduled prompt using its current revision. Delivered runs remain in chat."}.into(),parameters}
}
pub trait TaskStore: Send + Sync {
    fn execute(
        &self,
        command: &TaskCommand,
    ) -> impl Future<Output = Result<Vec<ScheduledTask>, String>> + Send;
}
pub struct ScheduledTools<E, S> {
    executor: E,
    store: S,
}
impl<E, S> ScheduledTools<E, S> {
    pub fn new(executor: E, store: S) -> Self {
        Self { executor, store }
    }
}
impl<E: ToolExecutor + Sync, S: TaskStore> ToolExecutor for ScheduledTools<E, S> {
    fn reserve_tool_call(&self) -> Result<(), String> {
        self.executor.reserve_tool_call()
    }
    fn has_context(&self) -> bool {
        self.executor.has_context()
    }
    async fn context(
        &mut self,
        tools: &[ToolDefinition],
        call: Option<&ToolCall>,
    ) -> Option<String> {
        self.executor.context(tools, call).await
    }
    fn describe(&self, call: &ToolCall) -> String {
        if is_scheduled_tool(&call.name) {
            crate::tools::parse(call).map_or_else(|error| error.to_string(), |tool| tool.describe())
        } else {
            self.executor.describe(call)
        }
    }
    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        if !is_scheduled_tool(&call.name) {
            return self.executor.preview(call).await;
        }
        let note = match crate::tools::parse(call).ok()? {
            crate::tools::Tool::Scheduled(TaskCommand::Monitor { command, .. }) => {
                Some(serde_json::to_string(&command).ok()?)
            }
            crate::tools::Tool::Scheduled(
                TaskCommand::Create { draft } | TaskCommand::Update { draft, .. },
            ) => Some(format!(
                "Scheduled prompt: {}\nSchedule: {}\n\n{}",
                draft.title,
                serde_json::to_string(&draft.schedule).ok()?,
                draft.prompt
            )),
            crate::tools::Tool::Scheduled(TaskCommand::Delete { id, .. }) => {
                Some(format!("Remove scheduled task #{id}"))
            }
            _ => None,
        };
        note.map(|note| ToolPreview {
            diff: None,
            note: Some(note),
        })
    }
    async fn checkpoint(&self, call: &ToolCall) -> Result<Option<FileDiff>, String> {
        self.executor.checkpoint(call).await
    }
    async fn project_checkpoint(&self, call: &ToolCall) -> Result<Option<ProjectSnapshot>, String> {
        self.executor.project_checkpoint(call).await
    }
    fn acknowledge(&self, id: &str) {
        self.executor.acknowledge(id);
    }
    fn set_context_limit(&self, limit: Option<usize>) {
        self.executor.set_context_limit(limit);
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutcome {
        if !is_scheduled_tool(&call.name) {
            return self.executor.execute(call).await;
        }
        let result:Result<String,String>=async{let crate::tools::Tool::Scheduled(command)=crate::tools::parse(call).map_err(|error|error.to_string())?else{return Err("Invalid scheduled task".into());};let tasks=self.store.execute(&command).await?;Ok(serde_json::json!(tasks.iter().take(100).map(|task|serde_json::json!({"id":task.id,"revision":task.revision,"title":task.draft.title,"prompt_excerpt":task.draft.prompt.chars().take(256).collect::<String>(),"session_id":task.draft.session_id,"session_target":task.draft.session_target,"schedule":task.draft.schedule,"enabled":task.draft.enabled,"next_run":task.next_run,"host_available":task.host_available,"last_run":task.last_run})).collect::<Vec<_>>()).to_string())}.await;
        match result {
            Ok(content) => ToolOutcome {
                ok: true,
                content,
                summary: format!("{} complete", self.describe(call)),
                diff: None,
            },
            Err(error) => ToolOutcome {
                ok: false,
                content: error.clone(),
                summary: format!("Scheduled task failed: {error}"),
                diff: None,
            },
        }
    }
}
