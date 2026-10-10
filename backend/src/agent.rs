//! Agentic coding for remote-mode projects: the backend's `ToolExecutor`
//! (workspace-confined file tools) and the SSE stream that wraps the agent
//! loop from the `openwebide-agent` crate.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::files::HostFsVfs;
use crate::http_client::SpinHttpClient;
use crate::state::now;
use futures::{Stream, StreamExt, stream};
use openwebide_agent::{AgentConfig, AgentEvent, CancelCheck, PermissionGate};
use openwebide_agent::{VfsToolExecutor, vfs_tools};
use openwebide_core::{
    ChatMessage, ChatRequest, Role, RunEvent, ToolCall, ToolDefinition, TurnTelemetry,
};
use openwebide_llm::registry::Provider;
use openwebide_storage::Store;

use crate::state::AppDb;

/// The workspace tools offered to the model.
pub fn workspace_tools() -> Vec<ToolDefinition> {
    vfs_tools()
}

/// A per-session cancel flag backed by SQLite. The cancel POST arrives as a
/// separate Spin request (stateless, possibly another component instance),
/// so the flag lives in the database; the in-flight stream polls it at step
/// boundaries and during interruptible tools.
#[derive(Clone)]
pub struct CancelFlag {
    store: Arc<Store<AppDb>>,
    session_id: i64,
    started_ms: i64,
    poll_interval: Duration,
    lease: Option<(openwebide_core::UserId, String)>,
    touched: Arc<std::sync::atomic::AtomicI64>,
}

impl CancelFlag {
    pub fn new(store: Arc<Store<AppDb>>, session_id: i64, started_ms: i64) -> Self {
        Self {
            store,
            session_id,
            started_ms,
            poll_interval: Duration::from_millis(250),
            lease: None,
            touched: Arc::new(std::sync::atomic::AtomicI64::new(started_ms)),
        }
    }
    pub fn with_lease(mut self, user: openwebide_core::UserId, token: String) -> Self {
        self.lease = Some((user, token));
        self
    }
}

impl CancelCheck for CancelFlag {
    async fn cancelled(&self) {
        while !self.check().await {
            if self.poll_interval.is_zero() {
                let mut yielded = false;
                futures::future::poll_fn(|cx| {
                    if yielded {
                        std::task::Poll::Ready(())
                    } else {
                        yielded = true;
                        cx.waker().wake_by_ref();
                        std::task::Poll::Pending
                    }
                })
                .await;
            } else {
                spin_sdk::time::sleep(self.poll_interval).await;
            }
        }
    }

    fn check(&self) -> impl Future<Output = bool> + Send {
        let store = self.store.clone();
        let session_id = self.session_id;
        let started_ms = self.started_ms;
        let lease = self.lease.clone();
        let touched = self.touched.clone();
        async move {
            let at = crate::state::now_ms() / 1000;
            if let Some((user, token)) = lease
                && at.saturating_mul(1000) - touched.load(std::sync::atomic::Ordering::Relaxed)
                    > 15000
            {
                if store
                    .session_run_lease(user, session_id, &token, false, at)
                    .await
                    .is_err()
                {
                    return true;
                }
                touched.store(
                    at.saturating_mul(1000),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            match store.cancel_requested_since(session_id, started_ms).await {
                Ok(cancelled) => cancelled,
                Err(error) => {
                    eprintln!("session {session_id}: cancel_requested_since: {error}");
                    false
                }
            }
        }
    }
}

/// How long the gate waits for the user's decision before denying.
const PERMISSION_TIMEOUT: Duration = Duration::from_secs(300);
const PERMISSION_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A per-session permission gate backed by SQLite. The user's decision arrives
/// as a separate Spin request (stateless, possibly another component
/// instance), so the in-flight stream polls the database until the decision
/// is recorded, the run is cancelled, or the wait times out.
#[derive(Clone)]
pub struct PermissionPoller {
    store: Arc<Store<AppDb>>,
    session_id: i64,
    started_ms: i64,
    poll_interval: Duration,
    timeout: Duration,
}

impl PermissionPoller {
    pub fn new(store: Arc<Store<AppDb>>, session_id: i64, started_ms: i64) -> Self {
        Self {
            store,
            session_id,
            started_ms,
            poll_interval: PERMISSION_POLL_INTERVAL,
            timeout: PERMISSION_TIMEOUT,
        }
    }
}

impl PermissionGate for PermissionPoller {
    fn approve(&self, call: &ToolCall) -> impl Future<Output = bool> + Send {
        let store = self.store.clone();
        let session_id = self.session_id;
        let started_ms = self.started_ms;
        let tool_call_id = call.id.clone();
        let poll_interval = self.poll_interval;
        let timeout = self.timeout;
        async move {
            let started = Instant::now();
            loop {
                match store.take_tool_permission(session_id, &tool_call_id).await {
                    Ok(Some(decision)) => return decision,
                    Ok(None) => {}
                    Err(error) => eprintln!(
                        "session {session_id}: take_tool_permission {tool_call_id}: {error}"
                    ),
                }
                // A cancel landing while waiting also denies the call; the
                // loop re-checks the cancel flag and reports `Cancelled`.
                match store.cancel_requested_since(session_id, started_ms).await {
                    Ok(true) => return false,
                    Ok(false) => {}
                    Err(error) => {
                        eprintln!("session {session_id}: cancel_requested_since: {error}");
                    }
                }
                if started.elapsed() >= timeout {
                    return false;
                }
                if !poll_interval.is_zero() {
                    spin_sdk::time::sleep(poll_interval).await;
                }
            }
        }
    }
}

/// Build the SSE event stream for one agentic message: the user message, the
/// agent's tool steps, and (on success) the persisted assistant message.
///
/// The store is shared: the agent loop polls the cancel flag and permission
/// decisions from inside the stream and the tail persists the reply, so the
/// response body outlives the request handler.
#[allow(clippy::too_many_arguments)]
pub fn agent_stream(
    store: Arc<Store<AppDb>>,
    user_id: openwebide_core::UserId,
    session_id: i64,
    user_message: ChatMessage,
    request: ChatRequest,
    plugin_skills: Vec<openwebide_core::ProjectSkill>,
    plugin_executables: Vec<openwebide_core::plugins::PreparedPlugin>,
    plugin_grants: std::collections::BTreeMap<String, String>,
    provider: Provider<SpinHttpClient>,
    base: Option<String>,
    environment: openwebide_core::RunEnvironment,
    config: AgentConfig,
    cancel: CancelFlag,
    gate: PermissionPoller,
) -> Pin<Box<dyn Stream<Item = RunEvent> + Send + 'static>> {
    let anchor_id = user_message.id;
    let factory = SpinTaskFactory {
        store: store.clone(),
        user: user_id,
        session: session_id,
        anchor: anchor_id,
        plugin_skills: Arc::new(plugin_skills),
        plugin_executables: Arc::new(plugin_executables),
        plugin_grants: Arc::new(plugin_grants),
        base,
        environment,
        manual: gate,
    };
    let executor = factory.executor();
    let gate = factory.gate(&request);
    let events = openwebide_agent::tasks::host::run_tree(
        factory,
        crate::api::model_operations::ModelSource {
            store: store.clone(),
            user: user_id,
        },
        cancel,
        provider,
        executor,
        gate,
        request,
        config,
        anchor_id,
    );
    let tail = map_agent_events(store, user_id, session_id, anchor_id, events);
    Box::pin(
        stream::iter([RunEvent::Message {
            message: user_message,
        }])
        .chain(tail),
    )
}

pub(crate) struct SessionPersistence {
    pub(crate) store: Arc<Store<AppDb>>,
    pub(crate) user: openwebide_core::UserId,
    pub(crate) session: i64,
    pub(crate) anchor: i64,
}
impl openwebide_agent::session::RunPersistence for SessionPersistence {
    async fn notify(&self, event: &openwebide_core::push::RunNotification) -> Result<(), String> {
        self.store
            .queue_run_notification(self.user, self.session, event, crate::state::now())
            .await
            .map_err(|error| error.to_string())
    }

    async fn task(
        &self,
        anchor: i64,
        snapshot: &openwebide_core::TaskSnapshot,
    ) -> Result<(), String> {
        self.store
            .save_task(self.user, self.session, anchor, snapshot)
            .await
            .map_err(|error| error.to_string())
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }
    async fn timing(&self, id: &str, timing: &openwebide_core::ToolTiming) -> Result<(), String> {
        self.store
            .save_tool_timing(self.user, self.session, id, timing)
            .await
            .map_err(|error| error.to_string())
    }

    fn now(&self) -> i64 {
        now()
    }
    async fn message(
        &self,
        role: Role,
        content: &str,
        usage: Option<&TurnTelemetry>,
        calls: Option<&[ToolCall]>,
    ) -> Result<ChatMessage, String> {
        if calls.is_some() || role == Role::System {
            self.store
                .insert_interim_message(self.session, role, content, now(), usage, calls)
                .await
        } else {
            self.store
                .insert_message_with_usage(self.session, role, content, now(), usage)
                .await
        }
        .map_err(|error| error.to_string())
    }
    async fn step(
        &self,
        anchor: i64,
        id: &str,
        name: &str,
        summary: &str,
        diff: Option<&openwebide_core::FileDiff>,
    ) -> Result<(), String> {
        self.store
            .upsert_tool_step(self.session, anchor, id, name, summary, now(), diff)
            .await
            .map_err(|error| error.to_string())
    }
    async fn checkpoint(
        &self,
        id: &str,
        checkpoint: &openwebide_core::rewind::ProjectCheckpoint,
    ) -> Result<(), String> {
        self.store
            .save_project_checkpoint(self.session, id, checkpoint)
            .await
            .map_err(|e| e.to_string())
    }
    async fn result(
        &self,
        id: &str,
        ok: bool,
        summary: &str,
        diff: Option<&openwebide_core::FileDiff>,
    ) -> Result<(), String> {
        self.store
            .complete_tool_step(self.user, self.session, id, ok, summary, diff)
            .await
            .map_err(|error| error.to_string())
    }
    async fn finish(&self) {
        let _ = self
            .store
            .question_command(
                self.user,
                self.session,
                &openwebide_core::questions::QuestionCommand::CancelRun {
                    anchor: self.anchor,
                },
                now(),
            )
            .await;
        let _ = self
            .store
            .clear_tool_permissions_for_run(self.session, self.anchor)
            .await;
    }
}
fn map_agent_events(
    store: Arc<Store<AppDb>>,
    user: openwebide_core::UserId,
    session: i64,
    anchor: i64,
    events: impl Stream<Item = AgentEvent> + Send + 'static,
) -> impl Stream<Item = RunEvent> + Send {
    openwebide_agent::session::events(
        SessionPersistence {
            store,
            user,
            session,
            anchor,
        },
        session,
        anchor,
        events,
    )
}

struct TodoPersistence {
    store: Arc<Store<AppDb>>,
    user: openwebide_core::UserId,
    session: i64,
    anchor: i64,
}
impl openwebide_agent::todo::TodoStore for TodoPersistence {
    async fn read(&self) -> Result<Option<openwebide_core::TodoUpdate>, String> {
        self.store
            .get_todo_plan(self.user, self.session)
            .await
            .map_err(|error| error.to_string())
    }
    async fn write(
        &self,
        plan: &openwebide_core::TodoPlan,
    ) -> Result<openwebide_core::TodoUpdate, String> {
        self.store
            .write_todo_plan(self.user, self.session, self.anchor, plan, now())
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_core::UserRole;

    use openwebide_agent::{ToolExecutor, ToolOutcome};
    use openwebide_core::{ChatCompletion, ChatResponse, ModelInfo, ProviderKind};
    use openwebide_llm::{LlmProvider, ProviderError, StreamChunk, ToolStreamChunk};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[test]
    fn sse_plan_adapter_preserves_success_and_failure_contracts_in_every_workspace() {
        futures::executor::block_on(async {
            for mode in [
                Some(openwebide_core::WorkspaceMode::Local),
                Some(openwebide_core::WorkspaceMode::Remote),
                None,
            ] {
                let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("owner", "hash", UserRole::Admin, 1)
                    .await
                    .unwrap()
                    .id;
                let project = if let Some(mode) = mode {
                    Some(
                        store
                            .create_project(
                                &openwebide_core::NewProject {
                                    name: "project".into(),
                                    mode,
                                    path: Some("test".into()),
                                },
                                user,
                                1,
                            )
                            .await
                            .unwrap()
                            .id,
                    )
                } else {
                    None
                };
                let session = store
                    .create_session("session", None, None, project, user, 1)
                    .await
                    .unwrap()
                    .id;
                let prompt = store
                    .insert_message(session, Role::User, "task", 1)
                    .await
                    .unwrap();
                let executor = openwebide_agent::todo::TodoTools::new(
                    VfsToolExecutor::new(openwebide_core::MemoryVfs::new()),
                    TodoPersistence {
                        store: store.clone(),
                        user,
                        session,
                        anchor: prompt.id,
                    },
                );
                let call = ToolCall { id: "plan".into(), name: "todo_write".into(), arguments: r#"{"todos":[{"id":"inspect","content":"Inspect the code","status":"in_progress"}]}"#.into() };
                let result = executor.execute(&call).await;
                assert!(result.ok);
                let saved = store.get_todo_plan(user, session).await.unwrap().unwrap();
                assert_eq!(
                    serde_json::from_str::<openwebide_core::TodoPlan>(&result.content).unwrap(),
                    saved.plan
                );
                store
                    .insert_message(session, Role::User, "new task", 2)
                    .await
                    .unwrap();
                assert!(!executor.execute(&call).await.ok);
                assert_eq!(
                    store.get_todo_plan(user, session).await.unwrap(),
                    Some(saved)
                );
            }
        });
    }

    struct ScriptedProvider(Mutex<VecDeque<ChatResponse>>);

    impl LlmProvider for ScriptedProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Ollama
        }
        async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
            Ok(vec![])
        }
        async fn chat(&self, _: &ChatRequest) -> Result<String, ProviderError> {
            unreachable!()
        }
        fn chat_stream(
            &self,
            _: &ChatRequest,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>> {
            unreachable!()
        }
        async fn chat_tools(&self, _: &ChatRequest) -> Result<ChatCompletion, ProviderError> {
            unreachable!()
        }
        fn chat_tools_stream(
            &self,
            _: &ChatRequest,
        ) -> Pin<Box<dyn Stream<Item = Result<ToolStreamChunk, ProviderError>> + Send>> {
            let response = self.0.lock().unwrap().pop_front().unwrap();
            Box::pin(stream::iter([Ok(ToolStreamChunk::Response(response))]))
        }
        async fn context_limit(&self, _: Option<&str>) -> Result<Option<usize>, ProviderError> {
            Ok(None)
        }
    }

    struct RecordingExecutor(Arc<Mutex<Vec<String>>>);

    impl ToolExecutor for RecordingExecutor {
        fn describe(&self, call: &ToolCall) -> String {
            call.name.clone()
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutcome {
            self.0.lock().unwrap().push(call.name.clone());
            ToolOutcome {
                ok: true,
                content: "ok".into(),
                summary: "ok".into(),
                diff: None,
            }
        }
    }

    fn tool_turn(name: &str) -> ChatResponse {
        ChatResponse::ToolCalls(vec![ToolCall {
            id: "call_0".into(),
            name: name.into(),
            arguments: "{}".into(),
        }])
    }

    fn request() -> ChatRequest {
        ChatRequest {
            model_settings: Default::default(),
            connection_id: 1,
            system_prompt: None,
            model: None,
            messages: vec![],
            tools: workspace_tools(),
        }
    }

    async fn test_store() -> (Arc<Store<AppDb>>, i64) {
        let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
        store.migrate().await.unwrap();
        let user = store
            .insert_user("tester", "hash", UserRole::Admin, 1)
            .await
            .unwrap();
        let session = store
            .create_session("s", None, None, None, user.id, 1)
            .await
            .unwrap();
        (store, session.id)
    }

    #[test]
    fn context_is_persisted_without_replacing_tool_anchor_or_reentering_history() {
        futures::executor::block_on(async {
            let (store, session_id) = test_store().await;
            let user = store
                .insert_message(session_id, Role::User, "go", 1)
                .await
                .unwrap();
            let id = format!("{}1c0", openwebide_agent::step_id_prefix(user.id));
            let events = stream::iter([
                AgentEvent::Context("Root instruction".into()),
                AgentEvent::ToolCall {
                    id,
                    name: "read_file".into(),
                    summary: "read a".into(),
                },
            ]);
            let mapped: Vec<_> = map_agent_events(
                store.clone(),
                openwebide_core::UserId::new(1),
                session_id,
                user.id,
                events,
            )
            .collect()
            .await;
            assert!(
                matches!(&mapped[0], RunEvent::Message { message } if message.role == Role::System && message.content.contains("Root instruction"))
            );
            let steps = store.list_tool_steps(session_id).await.unwrap();
            assert_eq!(steps[0].anchor_message_id, user.id);
            let messages = store.list_messages(session_id).await.unwrap();
            assert_eq!(messages[1].role, Role::System);
            let history = openwebide_core::tool_history(messages, &steps);
            assert!(history.iter().all(|message| {
                !message
                    .content
                    .contains(openwebide_core::RUN_CONTEXT_PREFIX)
            }));
        });
    }

    fn test_gate(store: Arc<Store<AppDb>>, session_id: i64, started_ms: i64) -> PermissionPoller {
        PermissionPoller {
            store,
            session_id,
            started_ms,
            poll_interval: Duration::ZERO,
            timeout: Duration::from_millis(20),
        }
    }

    #[test]
    fn repeated_provider_ids_do_not_reuse_approval() {
        futures::executor::block_on(async {
            let (store, session_id) = test_store().await;
            let executed = Arc::new(Mutex::new(vec![]));
            let events = openwebide_agent::run(
                ScriptedProvider(Mutex::new(VecDeque::from([
                    tool_turn("write_file"),
                    tool_turn("run_command"),
                    ChatResponse::Text("done".into()),
                ]))),
                RecordingExecutor(executed.clone()),
                request(),
                AgentConfig::default(),
                CancelFlag::new(store.clone(), session_id, 1000),
                test_gate(store.clone(), session_id, 1000),
                7,
            );
            let mut events = Box::pin(events);
            let mut prompts = vec![];
            let mut denied = None;
            while let Some(event) = events.next().await {
                match event {
                    AgentEvent::PermissionRequest { id, .. } => {
                        prompts.push(id.clone());
                        if prompts.len() == 1 {
                            store
                                .set_tool_permission(session_id, &id, true)
                                .await
                                .unwrap();
                        }
                    }
                    AgentEvent::ToolResult {
                        id,
                        ok: false,
                        summary,
                        ..
                    } => denied = Some((id, summary)),
                    _ => {}
                }
            }
            assert_eq!(prompts, ["a7t1c0", "a7t2c0"]);
            assert_eq!(*executed.lock().unwrap(), ["write_file"]);
            assert_eq!(denied, Some(("a7t2c0".into(), "denied by user".into())));
        });
    }

    #[test]
    fn cleanup_preserves_other_run_and_session_permissions() {
        futures::executor::block_on(async {
            let (store, session_id) = test_store().await;
            let other = store
                .create_session(
                    "other",
                    None,
                    None,
                    None,
                    openwebide_core::UserId::new(1),
                    1,
                )
                .await
                .unwrap();
            for (session, id) in [
                (session_id, "a7t1c0"),
                (session_id, "a7t2c0"),
                (session_id, "a70t1c0"),
                (other.id, "a7t1c0"),
            ] {
                store.set_tool_permission(session, id, true).await.unwrap();
            }
            let events = [
                AgentEvent::TurnCalls {
                    text: "interim".into(),
                    calls: vec![],
                },
                AgentEvent::Cancelled,
            ];
            map_agent_events(
                store.clone(),
                openwebide_core::UserId::new(1),
                session_id,
                7,
                stream::iter(events),
            )
            .collect::<Vec<_>>()
            .await;
            for id in ["a7t1c0", "a7t2c0"] {
                assert_eq!(
                    store.take_tool_permission(session_id, id).await.unwrap(),
                    None
                );
            }
            assert_eq!(
                store
                    .take_tool_permission(session_id, "a70t1c0")
                    .await
                    .unwrap(),
                Some(true)
            );
            assert_eq!(
                store
                    .take_tool_permission(other.id, "a7t1c0")
                    .await
                    .unwrap(),
                Some(true)
            );
        });
    }

    #[test]
    fn cancel_only_applies_after_run_start() {
        futures::executor::block_on(async {
            let (store, session_id) = test_store().await;
            let cancel = CancelFlag::new(store.clone(), session_id, 2000);
            store.request_cancel(session_id, 1000).await.unwrap();
            assert!(!cancel.check().await);
            store.request_cancel(session_id, 3000).await.unwrap();
            assert!(cancel.check().await);
        });
    }

    #[test]
    fn stop_then_resend_keeps_waiting_run_cancelled() {
        futures::executor::block_on(async {
            let (store, session_id) = test_store().await;
            let executed = Arc::new(Mutex::new(vec![]));
            let gate = test_gate(store.clone(), session_id, 1000);
            let mut run_a = Box::pin(openwebide_agent::run(
                ScriptedProvider(Mutex::new(VecDeque::from([tool_turn("write_file")]))),
                RecordingExecutor(executed.clone()),
                request(),
                AgentConfig::default(),
                CancelFlag::new(store.clone(), session_id, 1000),
                test_gate(store.clone(), session_id, 1000),
                7,
            ));
            assert!(matches!(
                run_a.next().await,
                Some(AgentEvent::TurnCalls { .. })
            ));
            let Some(AgentEvent::PermissionRequest { id, name, .. }) = run_a.next().await else {
                panic!("missing permission prompt")
            };
            assert_eq!(
                store.take_tool_permission(session_id, &id).await.unwrap(),
                None
            );
            store.request_cancel(session_id, 1500).await.unwrap();
            let cancel_b = CancelFlag::new(store.clone(), session_id, 1600);
            let _gate_b = test_gate(store.clone(), session_id, 1600);
            assert!(
                !gate
                    .approve(&ToolCall {
                        id,
                        name,
                        arguments: "{}".into()
                    })
                    .await
            );
            assert_eq!(run_a.next().await, Some(AgentEvent::Cancelled));
            assert_eq!(run_a.next().await, None);
            assert!(!cancel_b.check().await);
            assert!(executed.lock().unwrap().is_empty());
        });
    }

    #[test]
    fn empty_tool_turn_persists_wire_calls() {
        futures::executor::block_on(async {
            let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
            store.migrate().await.unwrap();
            let user = store
                .insert_user("u", "hash", UserRole::Admin, 1)
                .await
                .unwrap();
            let session = store
                .create_session("s", None, None, None, user.id, 1)
                .await
                .unwrap();
            let calls = vec![openwebide_core::ToolCall {
                id: "wire-id".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }];
            let events = map_agent_events(
                store.clone(),
                openwebide_core::UserId::new(1),
                session.id,
                7,
                stream::iter([AgentEvent::TurnCalls {
                    text: String::new(),
                    calls: calls.clone(),
                }]),
            )
            .collect::<Vec<_>>()
            .await;
            let RunEvent::Interim { message } = &events[0] else {
                panic!("missing interim")
            };
            assert!(message.content.is_empty());
            assert_eq!(message.tool_calls.as_ref(), Some(&calls));
            assert_eq!(
                store.list_messages(session.id).await.unwrap(),
                vec![message.clone()]
            );
        });
    }

    #[test]
    fn maps_and_persists_the_awaiting_preview_then_replaces_it_with_the_result() {
        futures::executor::block_on(async {
            let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
            store.migrate_with(&|_| true).await.unwrap();
            let user = store
                .insert_user("preview", "hash", UserRole::Admin, 1)
                .await
                .unwrap();
            let session = store
                .create_session("preview", None, None, None, user.id, 1)
                .await
                .unwrap();
            let anchor = store
                .insert_message(session.id, Role::User, "write", 2)
                .await
                .unwrap();
            let mut diff = openwebide_core::FileDiff {
                path: "file".into(),
                old: Some("before".into()),
                new: "after".into(),
                old_unavailable: false,
                backup_path: None,
            };
            let permission = AgentEvent::PermissionRequest {
                id: "a1t1c0".into(),
                name: "write_file".into(),
                summary: "write file".into(),
                diff: Some(diff.clone()),
                note: Some("preview".into()),
            };
            diff.old = Some("changed before execution".into());
            let result = AgentEvent::ToolResult {
                id: "a1t1c0".into(),
                name: "write_file".into(),
                ok: true,
                summary: "written".into(),
                diff: Some(diff.clone()),
            };
            let events = map_agent_events(
                store.clone(),
                openwebide_core::UserId::new(1),
                session.id,
                anchor.id,
                stream::iter([permission, result]),
            );
            futures::pin_mut!(events);
            let Some(RunEvent::PermissionRequest {
                diff: preview,
                note,
                ..
            }) = events.next().await
            else {
                panic!("missing permission")
            };
            assert_eq!(note.as_deref(), Some("preview"));
            let steps = store.list_tool_steps(session.id).await.unwrap();
            assert_eq!(steps[0].diff, preview);
            assert_eq!(steps[0].ok, None);
            assert_eq!(steps[0].result_summary, None);
            events.next().await.unwrap();
            let steps = store.list_tool_steps(session.id).await.unwrap();
            assert_eq!(steps[0].diff, Some(diff));
            assert_eq!(steps[0].ok, Some(true));
        });
    }

    #[test]
    fn maps_interim_text_usage_and_display_anchors() {
        futures::executor::block_on(async {
            let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
            store.migrate_with(&|_| true).await.unwrap();
            let user = store
                .insert_user("alice", "hash", UserRole::Admin, 1)
                .await
                .unwrap();
            let session = store
                .create_session("s", None, None, None, user.id, 1)
                .await
                .unwrap();
            let user_message = store
                .insert_message(session.id, Role::User, "go", 2)
                .await
                .unwrap();
            let anchor_id = user_message.id;
            let before = format!("a{anchor_id}t1c0");
            let after = format!("a{anchor_id}t2c0");
            let first_usage = TurnTelemetry {
                context: None,
                prompt_tokens: 10,
                completion_tokens: 2,
                ..Default::default()
            };
            let last_usage = TurnTelemetry {
                context: None,
                prompt_tokens: 20,
                completion_tokens: 3,
                ..Default::default()
            };
            let events = vec![
                AgentEvent::ToolCall {
                    id: before.clone(),
                    name: "read_file".into(),
                    summary: "first".into(),
                },
                AgentEvent::TextDelta("checking".into()),
                AgentEvent::Telemetry(first_usage),
                AgentEvent::TurnCalls {
                    text: "checking".into(),
                    calls: vec![],
                },
                AgentEvent::PermissionRequest {
                    id: after.clone(),
                    name: "write_file".into(),
                    summary: "second".into(),
                    diff: None,
                    note: None,
                },
                AgentEvent::ToolCall {
                    id: after.clone(),
                    name: "write_file".into(),
                    summary: "second".into(),
                },
                AgentEvent::ToolResult {
                    id: after.clone(),
                    name: "write_file".into(),
                    ok: true,
                    summary: "wrote".into(),
                    diff: None,
                },
                AgentEvent::TextDelta("done".into()),
                AgentEvent::Telemetry(last_usage),
                AgentEvent::FinalText("done".into()),
            ];
            let mapped = map_agent_events(
                store.clone(),
                openwebide_core::UserId::new(1),
                session.id,
                anchor_id,
                stream::iter(events),
            )
            .collect::<Vec<_>>()
            .await;
            assert_eq!(
                mapped
                    .iter()
                    .filter(|event| matches!(event, RunEvent::ToolTiming { .. }))
                    .count(),
                4
            );
            let mapped: Vec<_> = mapped
                .into_iter()
                .filter(|event| !matches!(event, RunEvent::ToolTiming { .. }))
                .collect();
            assert!(matches!(&mapped[1], RunEvent::Delta { content: text } if text == "checking"));
            assert!(matches!(&mapped[2], RunEvent::Telemetry { usage } if *usage == first_usage));
            let RunEvent::Interim { message: interim } = &mapped[3] else {
                panic!("missing interim")
            };
            assert_eq!(interim.content, "checking");
            assert_eq!(interim.usage, Some(first_usage));
            assert!(matches!(&mapped[4], RunEvent::PermissionRequest { id, .. } if id == &after));
            let steps = store.list_tool_steps(session.id).await.unwrap();
            assert_eq!(steps[0].tool_call_id, before);
            assert_eq!(steps[0].anchor_message_id, anchor_id);
            assert_eq!(steps[1].tool_call_id, after);
            assert_eq!(steps[1].anchor_message_id, interim.id);
            assert_eq!(steps[1].ok, Some(true));
            assert!(
                steps
                    .iter()
                    .all(|step| step.timing.is_some_and(|timing| timing.finished))
            );
            let RunEvent::Done {
                message: final_message,
            } = mapped.last().unwrap()
            else {
                panic!("missing final")
            };
            assert_eq!(final_message.usage, Some(last_usage));
            let messages = store.list_messages(session.id).await.unwrap();
            assert_eq!(
                messages,
                vec![user_message, interim.clone(), final_message.clone()]
            );
        });
    }

    #[test]
    fn truncated_reply_does_not_inherit_tool_turn_usage() {
        futures::executor::block_on(async {
            for (has_text, denied) in [(true, false), (false, false), (true, true), (false, true)] {
                let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
                store.migrate_with(&|_| true).await.unwrap();
                let user = store
                    .insert_user("alice", "hash", UserRole::Admin, 1)
                    .await
                    .unwrap();
                let session = store
                    .create_session("s", None, None, None, user.id, 1)
                    .await
                    .unwrap();
                let usage = TurnTelemetry {
                    context: None,
                    prompt_tokens: 100,
                    completion_tokens: 20,
                    ..Default::default()
                };
                let mut events = vec![AgentEvent::Telemetry(usage)];
                if has_text {
                    events.push(AgentEvent::TurnCalls {
                        text: "checking".into(),
                        calls: vec![],
                    });
                }
                if denied {
                    events.push(AgentEvent::PermissionRequest {
                        id: "a7t1c0".into(),
                        name: "write_file".into(),
                        summary: "write".into(),
                        diff: None,
                        note: None,
                    });
                    events.push(AgentEvent::ToolResult {
                        id: "a7t1c0".into(),
                        name: "write_file".into(),
                        ok: false,
                        summary: "denied".into(),
                        diff: None,
                    });
                } else {
                    events.push(AgentEvent::ToolCall {
                        id: "a7t1c0".into(),
                        name: "read_file".into(),
                        summary: "read".into(),
                    });
                }
                let partial = format!("partial{}", openwebide_core::REPLY_TRUNCATED_MARKER);
                events.push(AgentEvent::TextDelta("partial".into()));
                events.push(AgentEvent::FinalText(partial.clone()));
                let mapped = map_agent_events(
                    store.clone(),
                    openwebide_core::UserId::new(1),
                    session.id,
                    7,
                    stream::iter(events),
                )
                .collect::<Vec<_>>()
                .await;
                let RunEvent::Done { message: reply } = mapped.last().unwrap() else {
                    panic!("missing final")
                };
                assert_eq!(reply.content, partial);
                assert_eq!(reply.usage, None);
                let messages = store.list_messages(session.id).await.unwrap();
                assert_eq!(messages.last(), Some(reply));
                if has_text {
                    assert_eq!(messages[0].usage, Some(usage));
                }
                assert_eq!(messages.len(), if has_text { 2 } else { 1 });
            }
        });
    }

    #[test]
    fn completion_persistence_failure_is_reported() {
        futures::executor::block_on(async {
            let (store, session_id) = test_store().await;
            let mapped = map_agent_events(
                store,
                openwebide_core::UserId::new(1),
                session_id,
                7,
                stream::iter([AgentEvent::ToolResult {
                    id: "missing".into(),
                    name: "write_file".into(),
                    ok: true,
                    summary: "written".into(),
                    diff: None,
                }]),
            )
            .collect::<Vec<_>>()
            .await;
            assert!(matches!(&mapped[0], RunEvent::ToolResult { .. }));
            assert_eq!(mapped.len(), 2);
            assert!(
                matches!(&mapped[1], RunEvent::Error { message } if message.starts_with("failed to save tool result"))
            );
        });
    }

    #[test]
    fn interim_persistence_failure_stops_before_any_tool() {
        futures::executor::block_on(async {
            let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
            store.migrate_with(&|_| true).await.unwrap();
            let mapped = map_agent_events(
                store,
                openwebide_core::UserId::new(1),
                999,
                7,
                stream::iter([
                    AgentEvent::TurnCalls {
                        text: "checking".into(),
                        calls: vec![],
                    },
                    AgentEvent::ToolCall {
                        id: "unsafe".into(),
                        name: "write_file".into(),
                        summary: "never execute".into(),
                    },
                ]),
            )
            .collect::<Vec<_>>()
            .await;
            assert_eq!(mapped.len(), 1);
            assert!(
                matches!(&mapped[0], RunEvent::Error { message } if message.starts_with("Could not save tool turn"))
            );
        });
    }
    #[test]
    fn cancellation_waiter_observes_request() {
        futures::executor::block_on(async {
            let (store, session_id) = test_store().await;
            let mut cancel = CancelFlag::new(store.clone(), session_id, 2000);
            cancel.poll_interval = Duration::ZERO;
            store.request_cancel(session_id, 1000).await.unwrap();
            let mut waiter = Box::pin(cancel.cancelled());
            assert!(futures::poll!(&mut waiter).is_pending());
            store.request_cancel(session_id, 3000).await.unwrap();
            waiter.await;
        });
    }
    #[test]
    fn reasoning_prefix_is_persisted_per_turn() {
        futures::executor::block_on(async {
            let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
            store.migrate().await.unwrap();
            let user = store
                .insert_user("u", "hash", UserRole::Admin, 1)
                .await
                .unwrap();
            let session = store
                .create_session("s", None, None, None, user.id, 1)
                .await
                .unwrap();
            let answer = format!("answer{}", openwebide_core::REPLY_CUT_OFF_MARKER);
            let events = map_agent_events(
                store.clone(),
                openwebide_core::UserId::new(1),
                session.id,
                7,
                stream::iter([
                    AgentEvent::ReasoningDelta("first".into()),
                    AgentEvent::TurnCalls {
                        text: "checking".into(),
                        calls: vec![],
                    },
                    AgentEvent::ReasoningDelta("r".into()),
                    AgentEvent::TextDelta("answer".into()),
                    AgentEvent::FinalText(answer.clone()),
                ]),
            )
            .collect::<Vec<_>>()
            .await;
            assert!(
                matches!(&events[0], RunEvent::ReasoningDelta { content } if content == "first")
            );
            let messages = store.list_messages(session.id).await.unwrap();
            assert_eq!(messages[0].content, "<think>first</think>checking");
            assert_eq!(messages[1].content, format!("<think>r</think>{answer}"));
        });
    }
}

struct SpinMemoryPersistence {
    store: Arc<Store<AppDb>>,
    user: openwebide_core::UserId,
    session: i64,
}
impl openwebide_agent::memory::MemoryStore for SpinMemoryPersistence {
    async fn execute(
        &self,
        command: &openwebide_core::MemoryCommand,
    ) -> Result<openwebide_core::ProjectMemories, String> {
        let project = self
            .store
            .get_session(self.session, self.user)
            .await
            .map_err(|error| error.to_string())?
            .project_id
            .ok_or("Project memory requires a project")?;
        let command = crate::api::naming::memory(
            &self.store,
            self.user,
            project,
            Some(self.session),
            command.clone(),
        )
        .await
        .map_err(|error| error.to_string())?;
        self.store
            .session_memory_command(self.user, self.session, &command, now())
            .await
            .map_err(|error| error.to_string())
    }
}

struct SpinScheduledPersistence {
    store: Arc<Store<AppDb>>,
    user: openwebide_core::UserId,
    session: i64,
}
impl openwebide_agent::scheduled::TaskStore for SpinScheduledPersistence {
    async fn execute(
        &self,
        command: &openwebide_core::scheduled::TaskCommand,
    ) -> Result<Vec<openwebide_core::scheduled::ScheduledTask>, String> {
        let project = self
            .store
            .get_session(self.session, self.user)
            .await
            .map_err(|error| error.to_string())?
            .project_id;
        let mut command = command.clone();
        if let openwebide_core::scheduled::TaskCommand::Create { draft }
        | openwebide_core::scheduled::TaskCommand::Update { draft, .. } = &mut command
            && draft.session_target == openwebide_core::scheduled::SessionTarget::Existing
            && draft.session_id == 0
        {
            draft.session_id = self.session;
        }
        let command = crate::api::naming::task(&self.store, self.user, project, command, true)
            .await
            .map_err(|error| error.to_string())?;
        self.store
            .scheduled_session_command(self.user, self.session, &command, crate::state::now())
            .await
            .map_err(|error| error.to_string())
    }
}
type SpinMemoryExecutor = openwebide_agent::memory::MemoryTools<
    openwebide_agent::todo::TodoTools<
        openwebide_agent::vfs_executor::SessionToolExecutor<
            VfsToolExecutor<
                HostFsVfs,
                crate::web::SpinWebClient,
                crate::bridge_client::SpinBridgeClient,
            >,
            crate::web::SpinWebClient,
            crate::bridge_client::SpinBridgeClient,
        >,
        TodoPersistence,
    >,
    SpinMemoryPersistence,
>;
type SpinScheduledExecutor =
    openwebide_agent::scheduled::ScheduledTools<SpinMemoryExecutor, SpinScheduledPersistence>;
struct SpinSkillPersistence {
    pinned: Arc<Vec<openwebide_core::ProjectSkill>>,
    store: Arc<Store<AppDb>>,
    user: openwebide_core::UserId,
    session: i64,
}
impl openwebide_agent::skills::SkillStore for SpinSkillPersistence {
    async fn execute(
        &self,
        command: &openwebide_core::SkillCommand,
    ) -> Result<openwebide_core::ProjectSkills, String> {
        openwebide_agent::skills::pinned_command(command, &self.pinned, async {
            self.store
                .session_skill_command(self.user, self.session, command, now())
                .await
                .map_err(|error| error.to_string())
        })
        .await
    }
}
type SpinBuiltinTaskExecutor =
    openwebide_agent::skills::SkillTools<SpinScheduledExecutor, SpinSkillPersistence>;
type SpinBaseTaskExecutor = openwebide_agent::plugins::execution::PluginTools<
    openwebide_agent::skills::packages::PackageSkillTools<SpinBuiltinTaskExecutor>,
    crate::api::plugins::PlanningHost<'static>,
    openwebide_agent::plugins::execution::GrantedServices<
        crate::api::plugins::PlanningHost<'static>,
    >,
>;
type SpinTaskGate = openwebide_agent::plugins::execution::PluginGate<
    openwebide_agent::policy::PolicyGate<PermissionPoller, crate::api::approvals::ApprovalAdapter>,
>;
#[derive(Clone)]
struct SpinTaskFactory {
    plugin_executables: Arc<Vec<openwebide_core::plugins::PreparedPlugin>>,
    plugin_grants: Arc<std::collections::BTreeMap<String, String>>,
    plugin_skills: Arc<Vec<openwebide_core::ProjectSkill>>,
    store: Arc<Store<AppDb>>,
    user: openwebide_core::UserId,
    session: i64,
    anchor: i64,
    base: Option<String>,
    environment: openwebide_core::RunEnvironment,
    manual: PermissionPoller,
}
impl SpinTaskFactory {
    fn executor(&self) -> SpinTaskExecutor {
        openwebide_agent::questions::QuestionTools::new(
            self.base_executor(),
            SpinQuestionPersistence {
                store: self.store.clone(),
                user: self.user,
                session: self.session,
            },
            self.anchor,
        )
    }
    fn base_executor(&self) -> SpinBaseTaskExecutor {
        let host =
            crate::api::plugins::PlanningHost::owned(self.store.clone(), self.user, self.session);
        openwebide_agent::plugins::execution::PluginTools {
            executor: openwebide_agent::skills::packages::PackageSkillTools::new(
                self.builtin_executor(),
                &self.plugin_skills,
            ),
            transport: host.clone(),
            services: openwebide_agent::plugins::execution::GrantedServices {
                host,
                grants: self.plugin_grants.clone(),
            },
            plugins: self.plugin_executables.clone(),
        }
    }
    fn builtin_executor(&self) -> SpinBuiltinTaskExecutor {
        let workspace = self.base.as_ref().map(|base| {
            VfsToolExecutor::with_web_and_bridge(
                HostFsVfs::new(base.clone()),
                crate::web::SpinWebClient,
                crate::bridge_client::SpinBridgeClient::for_project(
                    self.store.clone(),
                    base.clone(),
                ),
            )
            .with_context(self.environment.clone())
        });
        let executor = openwebide_agent::vfs_executor::SessionToolExecutor::new(
            workspace,
            crate::web::SpinWebClient,
            self.environment.clone(),
        )
        .with_host(crate::bridge_client::SpinBridgeClient::for_host(
            self.store.clone(),
            self.user.get(),
            self.session,
        ));
        openwebide_agent::skills::SkillTools::new(
            openwebide_agent::scheduled::ScheduledTools::new(
                openwebide_agent::memory::MemoryTools::new(
                    openwebide_agent::todo::TodoTools::new(
                        executor,
                        TodoPersistence {
                            store: self.store.clone(),
                            user: self.user,
                            session: self.session,
                            anchor: self.anchor,
                        },
                    ),
                    SpinMemoryPersistence {
                        store: self.store.clone(),
                        user: self.user,
                        session: self.session,
                    },
                ),
                SpinScheduledPersistence {
                    store: self.store.clone(),
                    user: self.user,
                    session: self.session,
                },
            ),
            SpinSkillPersistence {
                pinned: self.plugin_skills.clone(),
                store: self.store.clone(),
                user: self.user,
                session: self.session,
            },
        )
    }
    fn gate(&self, request: &ChatRequest) -> SpinTaskGate {
        openwebide_agent::plugins::execution::PluginGate {
            plugins: self.plugin_executables.clone(),
            gate: openwebide_agent::policy::PolicyGate {
                manual: self.manual.clone(),
                source: crate::api::approvals::ApprovalAdapter {
                    store: self.store.clone(),
                    user: self.user,
                    session: self.session,
                    connection_id: request.connection_id,
                    model: request.model.clone(),
                },
            },
        }
    }
}
impl openwebide_agent::tasks::host::TaskFactory for SpinTaskFactory {
    type Provider = Provider<SpinHttpClient>;
    type Executor = SpinTaskExecutor;
    type Gate = SpinTaskGate;
    fn now_ms(&self) -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }
    async fn prepare(
        &self,
        request: &ChatRequest,
    ) -> Result<(Self::Provider, Self::Executor, Self::Gate), String> {
        let runtime = crate::api::model_setup::runtime_store(
            &self.store,
            self.user,
            request.connection_id,
            request.model.as_deref(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let provider = Provider::for_connection(
            &runtime.connection,
            SpinHttpClient::default().with_transport(runtime.transport),
        );
        Ok((provider, self.executor(), self.gate(request)))
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use openwebide_agent::ToolExecutor;
    #[test]
    fn spin_sdk_tools_never_fall_back_to_builtin_memory_when_the_host_is_unavailable() {
        futures::executor::block_on(async {
            for mode in [
                openwebide_core::WorkspaceMode::Local,
                openwebide_core::WorkspaceMode::Remote,
            ] {
                let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("owner", "hash", openwebide_core::UserRole::Admin, 0)
                    .await
                    .unwrap()
                    .id;
                let project = store
                    .create_project(
                        &openwebide_core::NewProject {
                            name: "p".into(),
                            mode,
                            path: Some("p".into()),
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                let session = store
                    .create_session("s", None, None, Some(project), user, 0)
                    .await
                    .unwrap()
                    .id;
                let record = store
                    .memory_command(
                        user,
                        project,
                        &openwebide_core::MemoryCommand::Create {
                            auto_title: false,
                            title: "UI fact".into(),
                            content: "BUILTIN MEMORY DATA".into(),
                        },
                        false,
                        0,
                    )
                    .await
                    .unwrap()
                    .entries
                    .remove(0);
                let mut plugin = openwebide_core::plugins::testing::receipt();
                plugin.manifest.compatibility.plugin_api = 3;
                plugin.manifest.contributions.skills.clear();
                plugin.manifest.contributions.tools = vec![openwebide_core::plugins::PluginTool {
                    name: "memory_read".into(),
                    description: "Read a memory through source code".into(),
                    parameters: serde_json::json!({"type":"object"}),
                    requires_approval: false,
                }];
                plugin.manifest.executable = Some(openwebide_core::plugins::RustPlugin {
                    manifest: "Cargo.toml".into(),
                    library: "memory".into(),
                    sdk_version: "0.1.0".into(),
                    capabilities: vec!["collections".into()],
                });
                let factory = SpinTaskFactory {
                    plugin_grants: Arc::new(std::collections::BTreeMap::from([(
                        plugin.digest.clone(),
                        "a".repeat(32),
                    )])),
                    plugin_executables: Arc::new(vec![plugin]),
                    plugin_skills: Arc::new(vec![]),
                    store: store.clone(),
                    user,
                    session,
                    anchor: 1,
                    base: None,
                    environment: Default::default(),
                    manual: PermissionPoller::new(store, session, 0),
                };
                let call = ToolCall {
                    id: "read".into(),
                    name: "memory_read".into(),
                    arguments: serde_json::json!({"id":record.id}).to_string(),
                };
                assert!(factory.builtin_executor().execute(&call).await.ok);
                let outcome = factory.executor().execute(&call).await;
                assert!(!outcome.ok);
                assert!(!outcome.content.contains("BUILTIN MEMORY DATA"));
            }
        });
    }
    #[test]
    fn memory_tools_use_owned_session_adapter_and_propagate_disabled_conflicts_and_failures() {
        futures::executor::block_on(async {
            for mode in [
                openwebide_core::WorkspaceMode::Local,
                openwebide_core::WorkspaceMode::Remote,
            ] {
                let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("owner", "hash", openwebide_core::UserRole::Admin, 0)
                    .await
                    .unwrap()
                    .id;
                let project = store
                    .create_project(
                        &openwebide_core::NewProject {
                            name: "project".into(),
                            mode,
                            path: Some("test".into()),
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                let session = store
                    .create_session("session", None, None, Some(project), user, 0)
                    .await
                    .unwrap()
                    .id;
                let executor = openwebide_agent::memory::MemoryTools::new(
                    VfsToolExecutor::new(openwebide_core::MemoryVfs::new()),
                    SpinMemoryPersistence {
                        store: store.clone(),
                        user,
                        session,
                    },
                );
                let mut call = ToolCall {
                    id: "memory".into(),
                    name: "memory_create".into(),
                    arguments: r#"{"title":"Build","content":"Run cargo test"}"#.into(),
                };
                let preview = executor.preview(&call).await.unwrap();
                assert!(preview.diff.is_none());
                assert!(preview.note.unwrap().contains("Run cargo test"));
                let outcome = executor.execute(&call).await;
                assert!(outcome.ok, "{}", outcome.content);
                let entry: openwebide_core::ProjectMemory =
                    serde_json::from_str(&outcome.content).unwrap();
                call.name = "memory_search".into();
                call.arguments = r#"{"query":"cargo"}"#.into();
                assert!(executor.execute(&call).await.ok);
                call.name = "memory_read".into();
                call.arguments = serde_json::json!({"id":entry.id}).to_string();
                assert!(
                    executor
                        .execute(&call)
                        .await
                        .content
                        .contains("Run cargo test")
                );
                call.name = "memory_update".into();
                call.arguments=serde_json::json!({"id":entry.id,"revision":1,"title":"Build","content":"Run cargo test --offline"}).to_string();
                assert!(executor.execute(&call).await.ok);
                assert!(!executor.execute(&call).await.ok);
                call.arguments=r#"{"id":1,"revision":2,"title":"Build","content":"new","action":"set_enabled"}"#.into();
                assert!(!executor.execute(&call).await.ok);
                let mut request = ChatRequest {
                    connection_id: 1,
                    model: None,
                    system_prompt: None,
                    messages: Vec::new(),
                    tools: vfs_tools(),
                    model_settings: Default::default(),
                };
                let data = store.session_memories(user, session).await.unwrap();
                openwebide_agent::memory::configure(
                    &mut request.tools,
                    &mut request.system_prompt,
                    &data,
                    request.model_settings.context_limit,
                );
                assert!(
                    request
                        .tools
                        .iter()
                        .any(|tool| tool.name == "memory_create")
                );
                assert!(
                    request
                        .system_prompt
                        .as_ref()
                        .unwrap()
                        .contains("--offline")
                );
                store
                    .memory_command(
                        user,
                        project,
                        &openwebide_core::MemoryCommand::SetEnabled { enabled: false },
                        false,
                        1,
                    )
                    .await
                    .unwrap();
                call.name = "memory_read".into();
                call.arguments = serde_json::json!({"id":entry.id}).to_string();
                assert!(!executor.execute(&call).await.ok);
                let mut request = ChatRequest {
                    connection_id: 1,
                    model: None,
                    system_prompt: None,
                    messages: Vec::new(),
                    tools: vfs_tools(),
                    model_settings: Default::default(),
                };
                openwebide_agent::memory::configure(
                    &mut request.tools,
                    &mut request.system_prompt,
                    &store.session_memories(user, session).await.unwrap(),
                    request.model_settings.context_limit,
                );
                assert!(
                    !request
                        .tools
                        .iter()
                        .any(|tool| tool.name.starts_with("memory_"))
                );
                assert!(request.system_prompt.is_none());
                store
                    .memory_command(
                        user,
                        project,
                        &openwebide_core::MemoryCommand::SetEnabled { enabled: true },
                        false,
                        2,
                    )
                    .await
                    .unwrap();
                call.name = "memory_delete".into();
                call.arguments = serde_json::json!({"id":entry.id,"revision":2}).to_string();
                assert!(executor.execute(&call).await.ok);
                call.name = "list_dir".into();
                call.arguments = "{}".into();
                assert!(executor.execute(&call).await.ok);
            }
        });
    }
}

#[cfg(test)]
mod skill_tests {
    use super::*;
    use openwebide_agent::ToolExecutor;
    use openwebide_core::{SkillCommand, SkillDraft, ToolCall, WorkspaceMode};
    #[test]
    fn skill_tools_share_crud_creator_progressive_loading_and_permission_contract_in_both_modes() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Arc::new(Store::new(AppDb::open_in_memory().unwrap()));
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("owner", "hash", openwebide_core::UserRole::Admin, 0)
                    .await
                    .unwrap()
                    .id;
                let project = store
                    .create_project(
                        &openwebide_core::NewProject {
                            name: "project".into(),
                            mode,
                            path: Some("test".into()),
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                let session = store
                    .create_session("first", None, None, Some(project), user, 0)
                    .await
                    .unwrap()
                    .id;
                let executor = openwebide_agent::skills::SkillTools::new(
                    VfsToolExecutor::new(openwebide_core::MemoryVfs::new()),
                    SpinSkillPersistence {
                        pinned: Arc::new(Vec::new()),
                        store: store.clone(),
                        user,
                        session,
                    },
                );
                let draft = SkillDraft {
                    name: "build-check".into(),
                    description: "Use when checking builds".into(),
                    instructions: "PRIVATE INSTRUCTIONS".into(),
                    enabled: true,
                    resources: vec![openwebide_core::SkillResource {
                        name: "references/check.md".into(),
                        content: "PRIVATE RESOURCE".into(),
                        binary: false,
                    }],
                    metadata: Default::default(),
                };
                let call = |name: &str, args: serde_json::Value| ToolCall {
                    id: "skill".into(),
                    name: name.into(),
                    arguments: args.to_string(),
                };
                let create = call("skill_create", serde_json::json!({"draft":draft}));
                assert!(openwebide_agent::requires_approval(&create));
                assert!(
                    executor
                        .preview(&create)
                        .await
                        .unwrap()
                        .note
                        .unwrap()
                        .contains("PRIVATE INSTRUCTIONS")
                );
                let result = executor.execute(&create).await;
                assert!(result.ok, "{}", result.content);
                let saved = store.project_skills(user, project).await.unwrap();
                let id = saved.entries[0].id;
                for read in [
                    call("skill_list", serde_json::json!({})),
                    call("skill_read", serde_json::json!({"id":id})),
                    call(
                        "skill_creator",
                        serde_json::json!({"goal":"Improve build review","id":id}),
                    ),
                ] {
                    assert!(!openwebide_agent::requires_approval(&read));
                    let result = executor.execute(&read).await;
                    assert!(result.ok, "{}", result.content);
                    assert!(!result.content.contains("PRIVATE RESOURCE"));
                    if read.name == "skill_list" {
                        assert!(!result.content.contains("PRIVATE INSTRUCTIONS"));
                    }
                    if read.name == "skill_creator" {
                        assert!(result.content.contains("Skill Authoring plugin"));
                        assert!(result.content.contains("skill_read"));
                    }
                }
                let resource = executor
                    .execute(&call(
                        "skill_read",
                        serde_json::json!({"id":id,"resource":"references/check.md"}),
                    ))
                    .await;
                assert!(resource.ok);
                assert!(resource.content.contains("PRIVATE RESOURCE"));
                assert!(!resource.content.contains("PRIVATE INSTRUCTIONS"));
                let mut tools = Vec::new();
                let mut prompt = None;
                openwebide_agent::skills::configure(&mut tools, &mut prompt, &saved, Some(4096));
                assert_eq!(tools.len(), 6);
                let prompt = prompt.unwrap();
                assert!(prompt.contains("build-check"));
                assert!(!prompt.contains("PRIVATE INSTRUCTIONS"));
                let update = call(
                    "skill_update",
                    serde_json::json!({"id":id,"revision":1,"draft":draft}),
                );
                assert!(executor.execute(&update).await.ok);
                assert!(!executor.execute(&update).await.ok);
                assert!(
                    !executor
                        .execute(&call(
                            "skill_delete",
                            serde_json::json!({"id":id,"revision":1})
                        ))
                        .await
                        .ok
                );
                assert!(
                    !executor
                        .execute(&call("skill_list", serde_json::json!({"action":"delete"})))
                        .await
                        .ok
                );
                store
                    .skill_command(
                        user,
                        project,
                        &SkillCommand::SetEnabled { enabled: false },
                        false,
                        2,
                    )
                    .await
                    .unwrap();
                for name in ["skill_list", "skill_read", "skill_creator", "skill_create"] {
                    let args = match name {
                        "skill_list" => serde_json::json!({}),
                        "skill_read" => serde_json::json!({"id":id}),
                        "skill_creator" => serde_json::json!({"goal":"Create build check"}),
                        _ => serde_json::json!({"draft":draft}),
                    };
                    assert!(!executor.execute(&call(name, args)).await.ok);
                }
                openwebide_agent::skills::configure(
                    &mut tools,
                    &mut None,
                    &store.project_skills(user, project).await.unwrap(),
                    None,
                );
                assert!(tools.is_empty());
                store
                    .skill_command(
                        user,
                        project,
                        &SkillCommand::SetEnabled { enabled: true },
                        false,
                        3,
                    )
                    .await
                    .unwrap();
                assert!(
                    executor
                        .execute(&call(
                            "skill_delete",
                            serde_json::json!({"id":id,"revision":2})
                        ))
                        .await
                        .ok
                );
                store.delete_session(session, user).await.unwrap();
                assert!(
                    !executor
                        .execute(&call("skill_list", serde_json::json!({})))
                        .await
                        .ok
                );
            }
        });
    }
}

struct SpinQuestionPersistence {
    store: Arc<Store<AppDb>>,
    user: openwebide_core::UserId,
    session: i64,
}
impl openwebide_agent::questions::QuestionStore for SpinQuestionPersistence {
    async fn command(
        &self,
        command: &openwebide_core::questions::QuestionCommand,
    ) -> Result<openwebide_core::questions::QuestionResult, String> {
        self.store
            .question_command(self.user, self.session, command, now())
            .await
            .map_err(|error| error.to_string())
    }
    async fn wait(&self) {
        spin_sdk::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

type SpinTaskExecutor =
    openwebide_agent::questions::QuestionTools<SpinBaseTaskExecutor, SpinQuestionPersistence>;
