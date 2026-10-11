//! Session checklist policy above filesystem execution and persistence adapters.
use std::future::Future;

use openwebide_core::{
    FileDiff, TodoPlan, TodoUpdate, ToolCall, ToolDefinition, rewind::ProjectSnapshot,
};

use crate::{
    ToolExecutor, ToolOutcome, ToolPreview,
    tools::{self, Tool},
};

/// Owned-session database operations supplied by the run host.
pub trait TodoStore: Send + Sync {
    fn read(&self) -> impl Future<Output = Result<Option<TodoUpdate>, String>> + Send;
    fn write(&self, plan: &TodoPlan) -> impl Future<Output = Result<TodoUpdate, String>> + Send;
}

/// Every host installs this facade around its normal executor.
pub struct TodoTools<E, S> {
    executor: E,
    store: S,
}
impl<E, S> TodoTools<E, S> {
    pub const fn new(executor: E, store: S) -> Self {
        Self { executor, store }
    }
}

impl<E: ToolExecutor + Sync, S: TodoStore> ToolExecutor for TodoTools<E, S> {
    fn reserve_tool_call(&self) -> Result<(), String> {
        self.executor.reserve_tool_call()
    }
    fn has_context(&self) -> bool {
        true
    }
    async fn context(
        &mut self,
        definitions: &[ToolDefinition],
        call: Option<&ToolCall>,
    ) -> Option<String> {
        let mut context = self.executor.context(definitions, call).await;
        if call.is_none()
            && definitions.iter().any(|tool| tool.name == "todo_write")
            && let Ok(Some(update)) = self.store.read().await
            && !update.plan.todos.is_empty()
        {
            let plan = serde_json::to_string(&update.plan).expect("plan serializes");
            context.get_or_insert_with(String::new).push_str(&format!("\n\nCurrent session checklist:\n{plan}\nUse todo_write to keep this checklist current as work progresses."));
        }
        context
    }
    fn describe(&self, call: &ToolCall) -> String {
        self.executor.describe(call)
    }
    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        self.executor.preview(call).await
    }
    async fn checkpoint(&self, call: &ToolCall) -> Result<Option<FileDiff>, String> {
        self.executor.checkpoint(call).await
    }
    async fn project_checkpoint(&self, call: &ToolCall) -> Result<Option<ProjectSnapshot>, String> {
        self.executor.project_checkpoint(call).await
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutcome {
        if call.name != "todo_write" {
            return self.executor.execute(call).await;
        }
        let result = async {
            let Tool::TodoWrite(plan) = tools::parse(call).map_err(|error| error.to_string())?
            else {
                return Err("Expected a checklist update".into());
            };
            plan.validate()?;
            self.store.write(&plan).await
        }
        .await;
        match result {
            Ok(update) => ToolOutcome {
                ok: true,
                content: serde_json::to_string(&update.plan).expect("plan serializes"),
                summary: format!("Plan updated ({} items)", update.plan.todos.len()),
                diff: None,
            },
            Err(error) => ToolOutcome {
                ok: false,
                content: format!("Could not update plan: {error}"),
                summary: format!("Plan update failed: {error}"),
                diff: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VfsToolExecutor;
    use futures::executor::block_on;
    use openwebide_core::{MemoryVfs, TodoItem, TodoStatus, Vfs};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    #[derive(Default)]
    struct Store {
        plan: Mutex<Option<TodoUpdate>>,
        fail: AtomicBool,
    }
    impl TodoStore for Arc<Store> {
        async fn read(&self) -> Result<Option<TodoUpdate>, String> {
            Ok(self.plan.lock().unwrap().clone())
        }
        async fn write(&self, plan: &TodoPlan) -> Result<TodoUpdate, String> {
            if self.fail.load(Ordering::Relaxed) {
                return Err("database unavailable".into());
            }
            let update = TodoUpdate {
                id: 1,
                session_id: 1,
                anchor_message_id: 1,
                created_at: 0,
                plan: plan.clone(),
            };
            *self.plan.lock().unwrap() = Some(update.clone());
            Ok(update)
        }
    }
    #[test]
    fn checklist_tools_validate_persist_before_success_preserve_failure_and_delegate_other_tools() {
        block_on(async {
            let store = Arc::new(Store::default());
            let fs = MemoryVfs::new();
            fs.write("a.txt", "original").await.unwrap();
            let mut tools = TodoTools::new(VfsToolExecutor::new(fs.clone()), store.clone());
            let plan = TodoPlan {
                todos: vec![TodoItem {
                    id: "inspect".into(),
                    content: "Inspect the code".into(),
                    status: TodoStatus::InProgress,
                }],
            };
            let mut call = ToolCall {
                id: "call".into(),
                name: "todo_write".into(),
                arguments: serde_json::to_string(&plan).unwrap(),
            };
            let outcome = tools.execute(&call).await;
            assert!(outcome.ok);
            assert_eq!(store.read().await.unwrap().unwrap().plan, plan);
            assert_eq!(
                serde_json::from_str::<TodoPlan>(&outcome.content).unwrap(),
                plan
            );
            assert!(
                tools
                    .context(&crate::vfs_tools(), None)
                    .await
                    .unwrap()
                    .contains("Inspect the code")
            );
            store.fail.store(true, Ordering::Relaxed);
            call.arguments = "{\"todos\":[]}".into();
            assert!(!tools.execute(&call).await.ok);
            assert_eq!(store.read().await.unwrap().unwrap().plan, plan);
            store.fail.store(false, Ordering::Relaxed);
            for arguments in [
                "{}",
                "not JSON",
                "{\"todos\":[{\"id\":\"x\",\"content\":\"\",\"status\":\"pending\"}]}",
                "{\"todos\":[{\"id\":\"x\",\"content\":\"x\",\"status\":\"unknown\"}]}",
            ] {
                call.arguments = arguments.into();
                assert!(!tools.execute(&call).await.ok);
                assert_eq!(store.read().await.unwrap().unwrap().plan, plan);
            }
            call.name = "read_file".into();
            call.arguments = "{\"path\":\"a.txt\"}".into();
            assert!(tools.execute(&call).await.content.contains("original"));
            assert_eq!(fs.read("a.txt").await.unwrap(), "original");
            call.name = "todo_write".into();
            call.arguments = "{\"todos\":[]}".into();
            assert!(tools.execute(&call).await.ok);
            assert!(store.read().await.unwrap().unwrap().plan.todos.is_empty());
            assert!(tools.context(&[], None).await.is_none());
        });
    }
}
