//! Shared project-memory policy over session-bound database adapters.
use crate::{ToolExecutor, ToolOutcome, ToolPreview};
use openwebide_core::{
    FileDiff, MemoryCommand, ProjectMemories, ToolCall, ToolDefinition, rewind::ProjectSnapshot,
};
use std::future::Future;

pub const TOOL_NAMES: &[&str] = &[
    "memory_create",
    "memory_search",
    "memory_read",
    "memory_update",
    "memory_delete",
];
pub fn is_memory_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}
/// Extend planning inputs before the runtime applies the configured tool allow-list.
pub fn configure(
    tools: &mut Vec<ToolDefinition>,
    system_prompt: &mut Option<String>,
    memories: &ProjectMemories,
    context_limit: Option<usize>,
) {
    tools.retain(|tool| !is_memory_tool(&tool.name));
    if !memories.enabled {
        return;
    }
    tools.extend(TOOL_NAMES.iter().map(|name| {
        name.parse::<crate::tools::ToolName>()
            .expect("known memory tool")
            .definition()
    }));
    if let Some(context) = openwebide_core::memory::memory_context_with_budget(
        memories,
        context_limit.unwrap_or(32768).saturating_mul(3) / 10,
    ) {
        system_prompt
            .get_or_insert_with(String::new)
            .push_str(&format!("\n\n{context}"));
    }
}
pub trait MemoryStore: Send + Sync {
    fn execute(
        &self,
        command: &MemoryCommand,
    ) -> impl Future<Output = Result<ProjectMemories, String>> + Send;
}
pub struct MemoryTools<E, S> {
    executor: E,
    store: S,
}
impl<E, S> MemoryTools<E, S> {
    pub const fn new(executor: E, store: S) -> Self {
        Self { executor, store }
    }
}
impl<E: ToolExecutor + Sync, S: MemoryStore> ToolExecutor for MemoryTools<E, S> {
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
        if is_memory_tool(&call.name) {
            crate::tools::parse(call).map_or_else(|error| error.to_string(), |tool| tool.describe())
        } else {
            self.executor.describe(call)
        }
    }
    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        if !is_memory_tool(&call.name) {
            return self.executor.preview(call).await;
        }
        let note = async {
            let crate::tools::Tool::Memory(command) =
                crate::tools::parse(call).map_err(|error| error.to_string())?
            else {
                return Err("Invalid memory request".into());
            };
            command.validate()?;
            match command {
                MemoryCommand::Create { title, content, .. }
                | MemoryCommand::Update { title, content, .. } => {
                    Ok(format!("Project memory: {title}\n\n{content}"))
                }
                MemoryCommand::Delete { id, revision } => {
                    let data = self.store.execute(&MemoryCommand::Read { id }).await?;
                    let entry = data
                        .entries
                        .iter()
                        .find(|entry| entry.id == id)
                        .ok_or("Memory no longer exists")?;
                    if entry.revision != revision {
                        return Err("Memory changed. Read it again before deleting.".into());
                    }
                    Ok(format!(
                        "Delete project memory: {}\n\n{}",
                        entry.title, entry.content
                    ))
                }
                _ => Ok(String::new()),
            }
        }
        .await;
        match note {
            Ok(note) if note.is_empty() => None,
            Ok(note) | Err(note) => Some(ToolPreview {
                diff: None,
                note: Some(note),
            }),
        }
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
        if !is_memory_tool(&call.name) {
            return self.executor.execute(call).await;
        }
        let result = async {
            let crate::tools::Tool::Memory(command) = crate::tools::parse(call).map_err(|error| error.to_string())? else { return Err("Invalid memory tool".into()); };
            command.validate()?;
            let result = self.store.execute(&command).await?;
            let value = match command {
                MemoryCommand::Search { .. } => serde_json::json!(result.entries.iter().take(20).map(|entry| serde_json::json!({"id":entry.id,"revision":entry.revision,"title":entry.title,"excerpt":entry.content.chars().take(256).collect::<String>()})).collect::<Vec<_>>()),
                MemoryCommand::Read { id } | MemoryCommand::Update { id, .. } => serde_json::json!(result.entries.iter().find(|entry| entry.id == id)),
                MemoryCommand::Create { .. } => serde_json::json!(result.entries.iter().max_by_key(|entry| entry.id)),
                MemoryCommand::Delete { id, .. } => serde_json::json!({"deleted":id}),
                MemoryCommand::SetEnabled { .. } => return Err("Agents cannot toggle project memory".into()),
            };
            Ok(value.to_string())
        }.await;
        match result {
            Ok(content) => ToolOutcome {
                ok: true,
                content,
                summary: format!("{} complete", self.describe(call)),
                diff: None,
            },
            Err(error) => ToolOutcome {
                ok: false,
                summary: format!("Memory operation failed: {error}"),
                content: error,
                diff: None,
            },
        }
    }
}
