//! Serialize project mutations and their checkpoint pair across a child tree.
use super::budget::TaskBudget;
use crate::{ToolExecutor, ToolOutcome, ToolPreview};
use futures::lock::OwnedMutexGuard;
use openwebide_core::{FileDiff, ToolCall, ToolDefinition, rewind::ProjectSnapshot};
use std::sync::Mutex;

struct Mutation {
    id: String,
    _guard: OwnedMutexGuard<()>,
}

pub struct MutationExecutor<E> {
    inner: E,
    budget: TaskBudget,
    mutation: Mutex<Option<Mutation>>,
}
impl<E> MutationExecutor<E> {
    pub fn new(inner: E, budget: TaskBudget) -> Self {
        Self {
            inner,
            budget,
            mutation: Mutex::new(None),
        }
    }
    async fn acquire(&self, call: &ToolCall) {
        if self
            .mutation
            .lock()
            .expect("Mutation lock poisoned")
            .is_some()
        {
            return;
        }
        let guard = self.budget.mutations.clone().lock_owned().await;
        *self.mutation.lock().expect("Mutation lock poisoned") = Some(Mutation {
            id: call.id.clone(),
            _guard: guard,
        });
    }
    fn release(&self) {
        self.mutation.lock().expect("Mutation lock poisoned").take();
    }
}
fn mutates(call: &ToolCall) -> bool {
    matches!(
        call.name.as_str(),
        "write_file" | "run_command" | "git_commit" | "git_branch"
    )
}
impl<E: ToolExecutor + Sync> ToolExecutor for MutationExecutor<E> {
    fn reserve_tool_call(&self) -> Result<(), String> {
        self.inner.reserve_tool_call()
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
        self.inner.describe(call)
    }
    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        self.inner.preview(call).await
    }
    async fn checkpoint(&self, call: &ToolCall) -> Result<Option<FileDiff>, String> {
        if mutates(call) {
            self.acquire(call).await;
        }
        let result = self.inner.checkpoint(call).await;
        if result.is_err() {
            self.release();
        }
        result
    }
    async fn project_checkpoint(&self, call: &ToolCall) -> Result<Option<ProjectSnapshot>, String> {
        if !mutates(call) {
            return self.inner.project_checkpoint(call).await;
        }
        self.acquire(call).await;
        {
            let lock = self.mutation.lock().expect("Mutation lock poisoned");
            let mutation = lock.as_ref().expect("Mutation acquired");
            assert_eq!(
                mutation.id, call.id,
                "One executor cannot mutate concurrently"
            );
        }
        let result = self.inner.project_checkpoint(call).await;
        if matches!(result, Ok(None)) {
            self.release();
        }
        result
    }
    fn acknowledge(&self, id: &str) {
        let mut lock = self.mutation.lock().expect("Mutation lock poisoned");
        if lock.as_ref().is_some_and(|mutation| mutation.id == id) {
            lock.take();
        }
        self.inner.acknowledge(id);
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutcome {
        self.inner.execute(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{FutureExt, executor::block_on};
    struct Executor;
    impl ToolExecutor for Executor {
        fn describe(&self, _: &ToolCall) -> String {
            String::new()
        }
        async fn project_checkpoint(
            &self,
            _: &ToolCall,
        ) -> Result<Option<ProjectSnapshot>, String> {
            Ok(Some(ProjectSnapshot::default()))
        }
        async fn execute(&self, _: &ToolCall) -> ToolOutcome {
            unreachable!()
        }
    }
    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "run_command".into(),
            arguments: "{}".into(),
        }
    }
    #[test]
    fn checkpoint_pair_holds_project_lock_and_drop_releases_it() {
        block_on(async {
            let budget = TaskBudget::default();
            let first = MutationExecutor::new(Executor, budget.clone());
            let second = MutationExecutor::new(Executor, budget.clone());
            first.project_checkpoint(&call("first")).await.unwrap();
            assert!(
                second
                    .project_checkpoint(&call("second"))
                    .now_or_never()
                    .is_none()
            );
            first.project_checkpoint(&call("first")).await.unwrap();
            assert!(
                second
                    .project_checkpoint(&call("second"))
                    .now_or_never()
                    .is_none()
            );
            first.acknowledge("first");
            second.project_checkpoint(&call("second")).await.unwrap();
            drop(second);
            // Abandoning a run while its checkpoint is persisted releases the lock.
            let third = MutationExecutor::new(Executor, budget);
            third
                .project_checkpoint(&call("third"))
                .now_or_never()
                .unwrap()
                .unwrap();
        });
    }
}
