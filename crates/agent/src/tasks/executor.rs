//! Shared delegation tool facade. Hosts provide child-loop construction only.
use super::{
    budget::TaskBudget,
    scheduler::{TaskExecutionEvent, TaskHost, run_tasks},
};
use crate::{
    AgentEvent, PermissionGate, ToolExecutionEvent, ToolExecutionStream, ToolExecutor, ToolOutcome,
    ToolPreview,
};
use futures::{StreamExt, stream};
use openwebide_core::{
    ChatRequest, FileDiff, TaskRequest, ToolCall, ToolDefinition, rewind::ProjectSnapshot,
};
use std::sync::Arc;

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "task".into(),
        description: "Delegate 1–4 independent tasks in parallel. Include required context; children inherit project, tools and approvals. Return concise results. Avoid overlapping file changes.".into(),
        parameters: serde_json::json!({"type":"object","properties":{"tasks":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"description":{"type":"string","minLength":1,"maxLength":80},"prompt":{"type":"string","minLength":1,"maxLength":16000}},"required":["description","prompt"],"additionalProperties":false}}},"required":["tasks"],"additionalProperties":false}),
    }
}
pub struct TaskExecutor<E, H> {
    inner: E,
    host: H,
    parent: ChatRequest,
    depth: usize,
    budget: TaskBudget,
}
impl<E, H> TaskExecutor<E, H> {
    pub fn new(inner: E, host: H, parent: ChatRequest, depth: usize, budget: TaskBudget) -> Self {
        Self {
            inner,
            host,
            parent,
            depth,
            budget,
        }
    }
}
fn error(content: String) -> ToolOutcome {
    ToolOutcome {
        ok: false,
        summary: content.clone(),
        content,
        diff: None,
    }
}
impl<E: ToolExecutor + Sync + 'static, H: TaskHost> ToolExecutor for TaskExecutor<E, H> {
    fn reserve_tool_call(&self) -> Result<(), String> {
        self.budget.reserve_tool()
    }
    fn has_context(&self) -> bool {
        self.inner.has_context()
    }
    async fn context(
        &mut self,
        tools: &[ToolDefinition],
        call: Option<&ToolCall>,
    ) -> Option<String> {
        self.inner.context(tools, call).await
    }
    fn describe(&self, call: &ToolCall) -> String {
        if call.name == "task" {
            "Run delegated tasks".into()
        } else {
            self.inner.describe(call)
        }
    }
    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        self.inner.preview(call).await
    }
    async fn checkpoint(&self, call: &ToolCall) -> Result<Option<FileDiff>, String> {
        self.inner.checkpoint(call).await
    }
    async fn project_checkpoint(&self, call: &ToolCall) -> Result<Option<ProjectSnapshot>, String> {
        self.inner.project_checkpoint(call).await
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutcome {
        if let Err(message) = self.budget.reserve_tool() {
            return error(message);
        }
        if !self.parent.tools.iter().any(|tool| tool.name == call.name) {
            return error("This tool is unavailable in the inherited task context".into());
        }
        if call.name == "task" {
            error("Delegation requires a streaming agent run".into())
        } else {
            self.inner.execute(call).await
        }
    }
    fn acknowledge(&self, id: &str) {
        self.inner.acknowledge(id);
    }
    fn set_context_limit(&self, limit: Option<usize>) {
        self.inner.set_context_limit(limit);
    }
    fn execute_stream(self: Arc<Self>, call: ToolCall) -> ToolExecutionStream {
        if call.name != "task" {
            return Box::pin(stream::once(async move {
                ToolExecutionEvent::Finished(self.execute(&call).await)
            }));
        }
        if let Err(message) = self.budget.reserve_tool() {
            return Box::pin(stream::iter([ToolExecutionEvent::Finished(error(message))]));
        }
        let request = if self.parent.tools.iter().any(|tool| tool.name == "task") {
            serde_json::from_str::<TaskRequest>(&call.arguments)
                .map_err(|e| format!("Invalid task arguments: {e}"))
        } else {
            Err("Delegation is unavailable in this child context".into())
        };
        let events = request.and_then(|request| {
            run_tasks(
                self.host.clone(),
                self.parent.clone(),
                call.id,
                request,
                self.depth,
                self.budget.clone(),
            )
        });
        // Release the executor before emitting Finished so context can be updated exclusively.
        match events {
            Ok(events) => Box::pin(events.map(|event| match event {
                TaskExecutionEvent::Update(update) => {
                    ToolExecutionEvent::Progress(AgentEvent::TaskUpdate(update))
                }
                TaskExecutionEvent::Finished(outcome) => ToolExecutionEvent::Finished(outcome),
            })),
            Err(message) => Box::pin(stream::iter([ToolExecutionEvent::Finished(error(message))])),
        }
    }
}

/// Delegation has no side effect itself; child tools still use the inherited gate.
pub struct TaskGate<G>(pub G, pub super::model_budget::ModelBudget);
impl<G: PermissionGate + Sync> PermissionGate for TaskGate<G> {
    fn needs_approval(&self, call: &ToolCall) -> bool {
        call.name != "task" && self.0.needs_approval(call)
    }
    fn uses_automatic_approval(&self) -> bool {
        self.0.uses_automatic_approval()
    }
    async fn automatically_approve(&self, call: &ToolCall) -> bool {
        let _permit = self.1.acquire().await;
        self.0.automatically_approve(call).await
    }
    fn approve(&self, call: &ToolCall) -> impl std::future::Future<Output = bool> + Send {
        self.0.approve(call)
    }
}
