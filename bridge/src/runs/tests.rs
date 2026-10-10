use super::*;
use futures::stream;
use openwebide_agent::PermissionGate;
use openwebide_core::REPLY_TRUNCATED_MARKER;
use openwebide_core::{
    ChatCompletion, ChatRequest, ChatResponse, Connection, FileDiff, ModelInfo, ProviderKind,
    ToolCall, WebSearchResult,
};
use openwebide_llm::{ProviderError, StreamChunk, ToolStreamChunk};
use std::pin::Pin;

#[derive(Default)]
struct FakeBackend {
    todo_plan: Mutex<Option<openwebide_core::TodoUpdate>>,
    fail_todo: AtomicBool,
    messages: Mutex<Vec<ChatMessage>>,
    operations: Mutex<Vec<String>>,
    kind: Mutex<Option<RunKind>>,
    compaction_request: Mutex<Option<ChatRequest>>,
    summaries: Mutex<Vec<ChatRequest>>,
    fail_final: AtomicBool,
    fail_completion: AtomicBool,
    fail_interim: AtomicBool,
    fail_plan: AtomicBool,
    hold_plan: AtomicBool,
    released_leases: Mutex<Vec<String>>,
    fail_queue: AtomicBool,
    queue_deliveries: Mutex<Vec<openwebide_core::QueuedPromptKey>>,
    approval_mode: Mutex<openwebide_core::ApprovalMode>,
    approval_checks: Mutex<Vec<openwebide_core::ApprovalCheck>>,
}

impl RunBackend for FakeBackend {
    async fn run_lease(
        &self,
        _user: i64,
        _session: i64,
        token: &str,
        release: bool,
        _since: i64,
        _permission: Option<&str>,
    ) -> Result<openwebide_core::scheduled::RunControl, String> {
        if release {
            self.released_leases.lock().unwrap().push(token.into());
        }
        Ok(Default::default())
    }

    async fn save_tool_timing(
        &self,
        _user: i64,
        _session: i64,
        _id: &str,
        _timing: &openwebide_core::ToolTiming,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn get_todo_plan(
        &self,
        _user: i64,
        _session: i64,
    ) -> Result<Option<openwebide_core::TodoUpdate>, String> {
        Ok(self.todo_plan.lock().unwrap().clone())
    }
    async fn write_todo_plan(
        &self,
        user: i64,
        session: i64,
        anchor: i64,
        plan: &openwebide_core::TodoPlan,
    ) -> Result<openwebide_core::TodoUpdate, String> {
        assert_eq!(user, 1);
        if self.fail_todo.load(Ordering::SeqCst) {
            return Err("plan database unavailable".into());
        }
        let update = openwebide_core::TodoUpdate {
            id: 1,
            session_id: session,
            anchor_message_id: anchor,
            plan: plan.clone(),
            created_at: 1,
        };
        *self.todo_plan.lock().unwrap() = Some(update.clone());
        Ok(update)
    }

    async fn consume_queued_prompt(
        &self,
        user: i64,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &str,
    ) -> Result<ChatMessage, String> {
        if self.fail_queue.load(Ordering::SeqCst) {
            return Err("Queued prompt changed".into());
        }
        self.queue_deliveries.lock().unwrap().push(key);
        self.persist_message(user, session, Role::User, content, None, None)
            .await
    }
    async fn model_complete(
        &self,
        _user: i64,
        request: &ChatRequest,
    ) -> Result<ChatCompletion, String> {
        self.summaries.lock().unwrap().push(request.clone());
        Ok(ChatCompletion {
            response: ChatResponse::Text(
                "Previous work completed; continue the current task.".into(),
            ),
            preamble: String::new(),
            reasoning: String::new(),
            stop_reason: openwebide_core::StopReason::Complete,
            usage: None,
        })
    }
    async fn approval_check(
        &self,
        _user: i64,
        _session: i64,
        check: &openwebide_core::ApprovalCheck,
    ) -> Result<openwebide_core::ApprovalDecision, String> {
        self.approval_checks.lock().unwrap().push(check.clone());
        Ok(openwebide_core::ApprovalDecision {
            approved: self
                .approval_mode
                .lock()
                .unwrap()
                .auto_approves(&check.call.name),
        })
    }
    async fn run_plan(
        &self,
        _user_id: i64,
        _session_id: i64,
        content: &str,
        _model: Option<&str>,
        _editor_context: Option<&EditorContext>,
    ) -> Result<RunPlan, String> {
        if self.fail_plan.load(Ordering::SeqCst) {
            return Err("plan failed".into());
        }
        if self.hold_plan.load(Ordering::SeqCst) {
            futures::future::pending::<()>().await;
        }
        let mut plan = plan(
            self.kind.lock().unwrap().clone().unwrap_or(RunKind::Chat),
            content,
        );
        if let Some(request) = self.compaction_request.lock().unwrap().clone() {
            plan.request = request;
        }
        Ok(plan)
    }
    async fn persist_message(
        &self,
        _user_id: i64,
        session_id: i64,
        role: Role,
        content: &str,
        usage: Option<&TurnTelemetry>,
        tool_calls: Option<&[openwebide_core::ToolCall]>,
    ) -> Result<ChatMessage, String> {
        if (content == "final" && self.fail_final.load(Ordering::SeqCst))
            || (content == "interim" && self.fail_interim.load(Ordering::SeqCst))
        {
            return Err("offline".into());
        }
        let mut messages = self.messages.lock().unwrap();
        let message = ChatMessage {
            id: i64::try_from(messages.len()).unwrap() + 10,
            session_id,
            role,
            content: content.into(),
            usage: usage.copied(),
            created_at: 1,
            tool_calls: tool_calls.map(<[ToolCall]>::to_vec),
            tool_call_id: None,
        };
        messages.push(message.clone());
        self.operations
            .lock()
            .unwrap()
            .push(format!("message:{}", message.id));
        Ok(message)
    }
    async fn upsert_tool_step(
        &self,
        _user_id: i64,
        _session_id: i64,
        anchor_id: i64,
        id: &str,
        _name: &str,
        _summary: &str,
        _diff: Option<&FileDiff>,
    ) -> Result<(), String> {
        self.operations
            .lock()
            .unwrap()
            .push(format!("step:{anchor_id}:{id}"));
        Ok(())
    }
    async fn complete_tool_step(
        &self,
        _user_id: i64,
        _session_id: i64,
        id: &str,
        _ok: bool,
        _summary: &str,
        _diff: Option<&FileDiff>,
    ) -> Result<(), String> {
        self.operations
            .lock()
            .unwrap()
            .push(format!("complete:{id}"));
        if self.fail_completion.load(Ordering::SeqCst) {
            return Err("offline".into());
        }
        Ok(())
    }
    async fn set_tool_stream_unsupported(
        &self,
        user_id: i64,
        connection_id: i64,
        tool_stream_revision: i64,
        _model: Option<&str>,
    ) -> Result<(), String> {
        self.operations.lock().unwrap().push(format!(
            "memo:{user_id}:{connection_id}:{tool_stream_revision}"
        ));
        Ok(())
    }
    async fn list_connections(&self, _user_id: i64) -> Result<Vec<Connection>, String> {
        Ok(vec![])
    }
    async fn web_search(
        &self,
        _user_id: i64,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<WebSearchResult>, String> {
        self.operations
            .lock()
            .unwrap()
            .push("legacy web search".into());
        Ok(vec![])
    }
    async fn web_fetch(&self, _user_id: i64, _url: &str) -> Result<String, String> {
        self.operations
            .lock()
            .unwrap()
            .push("legacy web fetch".into());
        Ok(String::new())
    }
}

fn plan(kind: RunKind, content: &str) -> RunPlan {
    let tools = match &kind {
        RunKind::Agent { .. } => openwebide_agent::vfs_tools(),
        RunKind::WebChat => openwebide_agent::session::projectless_tools(),
        RunKind::Chat => Vec::new(),
    };
    let mut tools = tools;
    openwebide_agent::plugins::configure(
        &mut tools,
        &mut None,
        &openwebide_agent::plugins::PluginContext {
            bindings: &[],
            memories: &Default::default(),
            skills: &Default::default(),
            context_limit: None,
        },
    );
    if !tools.is_empty() {
        tools.push(openwebide_agent::tasks::executor::definition());
    }
    RunPlan {
        plugin_executables: Vec::new(),
        plugin_grants: Default::default(),
        plugin_skills: Vec::new(),
        transport: Default::default(),
        environment: openwebide_core::RunEnvironment::default(),
        kind,
        user_content: content.into(),
        request: ChatRequest {
            model_settings: Default::default(),
            connection_id: 1,
            system_prompt: None,
            model: None,
            messages: vec![],
            tools,
        },
        connection: Connection {
            id: 1,
            name: "model".into(),
            kind: ProviderKind::Ollama,
            base_url: "http://model".into(),
            model: Some("model".into()),
            enabled: true,
            context_limit: None,
            tool_stream_unsupported: false,
            tool_stream_revision: 0,
            tool_selection: Default::default(),
        },
    }
}

#[derive(Default)]
struct FakeProvider {
    chat: Mutex<Vec<Result<StreamChunk, ProviderError>>>,
    tools: Mutex<Vec<Vec<Result<ToolStreamChunk, ProviderError>>>>,
    pending: bool,
}

impl LlmProvider for FakeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Ollama
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![])
    }
    async fn chat(&self, _request: &ChatRequest) -> Result<String, ProviderError> {
        unreachable!()
    }
    async fn chat_tools(&self, _request: &ChatRequest) -> Result<ChatCompletion, ProviderError> {
        unreachable!()
    }
    async fn context_limit(&self, _model: Option<&str>) -> Result<Option<usize>, ProviderError> {
        Ok(None)
    }
    fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send + 'static>> {
        let chunks = std::mem::take(&mut *self.chat.lock().unwrap());
        if self.pending {
            Box::pin(stream::iter(chunks).chain(stream::pending()))
        } else {
            Box::pin(stream::iter(chunks))
        }
    }
    fn chat_tools_stream(
        &self,
        _request: &ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<ToolStreamChunk, ProviderError>> + Send + 'static>> {
        let chunks = self.tools.lock().unwrap().remove(0);
        Box::pin(stream::iter(chunks))
    }
}

fn start(id: &str) -> StartRun {
    StartRun {
        host_path: None,
        run_id: id.into(),
        session_id: 1,
        content: "go".into(),
        model: None,
        editor_context: None,
        browser_preferences: None,
        queued_prompt: None,
    }
}
fn user(id: i64) -> Principal {
    Principal::User { user_id: id }
}
async fn finished(run: &Run) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while run.running.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn events(run: &Run) -> Vec<RunEvent> {
    run.delivery
        .lock()
        .unwrap()
        .ring
        .read_after(0, 4096)
        .0
        .into_iter()
        .map(|(_, e)| e)
        .collect()
}

#[tokio::test]
async fn buffered_start_cancel_survives_delayed_start_task() {
    use openwebide_core::BridgeClientMessage;

    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(RunRegistry::default());
    let backend = Arc::new(FakeBackend::default());
    let mut frames = stream::iter([
        r#"{"type":"run_start","run_id":"r","session_id":1,"content":"go"}"#,
        r#"{"type":"run_cancel","run_id":"r"}"#,
    ]);
    let (release, delayed) = tokio::sync::oneshot::channel::<()>();
    let mut delayed = Some(delayed);
    let mut task = None;
    while let Some(frame) = frames.next().await {
        match serde_json::from_str::<BridgeClientMessage>(frame).unwrap() {
            BridgeClientMessage::RunStart {
                run_id,
                session_id,
                content,
                model,
                editor_context,
                browser_preferences,
                queued_prompt,
            } => {
                let start = StartRun {
                    host_path: None,
                    run_id,
                    session_id,
                    content,
                    model,
                    editor_context,
                    browser_preferences,
                    queued_prompt,
                };
                let run = registry.reserve(&user(1), &start).unwrap();
                let registry = registry.clone();
                let backend = backend.clone();
                let workspace = dir.path().to_path_buf();
                let delayed = delayed.take().unwrap();
                task = Some(tokio::spawn(async move {
                    delayed.await.unwrap();
                    registry
                        .prepare(
                            run,
                            start,
                            &workspace,
                            backend,
                            |_| -> FakeProvider {
                                panic!("cancelled start must not call the model")
                            },
                            RunHost {
                                execution: Arc::new(crate::exec::HostExecution),
                                plugins: crate::plugins::transport::PluginExecutionHost::default(),
                            },
                        )
                        .await
                        .unwrap()
                }));
            }
            BridgeClientMessage::RunCancel { run_id } => {
                registry.get(&user(1), &run_id).unwrap().cancel.cancel();
            }
            _ => unreachable!(),
        }
    }
    assert!(registry.get(&user(1), "r").unwrap().cancel.is_cancelled());
    assert!(backend.messages.lock().unwrap().is_empty());
    release.send(()).unwrap();
    let run = task.unwrap().await.unwrap();
    finished(&run).await;
    assert_eq!(events(&run), [RunEvent::Cancelled]);
    assert!(backend.messages.lock().unwrap().is_empty());
    assert!(!registry.list(&user(1), 1)[0].running);
}

#[tokio::test]
async fn agent_mapping_persists_in_order_and_reanchors_with_last_usage() {
    let backend = FakeBackend::default();
    let run = Run::new("r".into(), 1, 1, 4096);
    let first = TurnTelemetry {
        context: None,
        prompt_tokens: 10,
        ..Default::default()
    };
    let last = TurnTelemetry {
        context: None,
        prompt_tokens: 20,
        ..Default::default()
    };
    map_agent_events(
        &run,
        &backend,
        7,
        stream::iter(vec![
            AgentEvent::ToolCall {
                id: "a".into(),
                name: "read_file".into(),
                summary: "a".into(),
            },
            AgentEvent::ToolResult {
                id: "a".into(),
                name: "read_file".into(),
                ok: true,
                summary: "read".into(),
                diff: None,
            },
            AgentEvent::Telemetry(first),
            AgentEvent::TurnCalls {
                text: "interim".into(),
                calls: vec![],
            },
            AgentEvent::PermissionRequest {
                id: "b".into(),
                name: "write_file".into(),
                summary: "b".into(),
                diff: None,
                note: None,
            },
            AgentEvent::Telemetry(last),
            AgentEvent::FinalText("final".into()),
        ]),
    )
    .await;
    assert_eq!(
        *backend.operations.lock().unwrap(),
        [
            "step:7:a",
            "complete:a",
            "message:10",
            "step:10:b",
            "message:11"
        ]
    );
    let messages = backend.messages.lock().unwrap();
    assert_eq!(messages[0].usage, Some(first));
    assert_eq!(messages[1].usage, Some(last));
    assert!(
        matches!(events(&run).last(), Some(RunEvent::Done { message }) if message.usage == Some(last))
    );
}

#[tokio::test]
async fn completion_persistence_failure_stops_agent_before_releasing_run() {
    struct StreamGuard<'a> {
        registry: &'a RunRegistry,
        run: &'a Run,
        dropped: &'a AtomicBool,
    }

    impl Drop for StreamGuard<'_> {
        fn drop(&mut self) {
            assert!(self.run.running.load(Ordering::SeqCst));
            assert_eq!(
                self.registry.reserve(&user(1), &start("r2")).unwrap_err().0,
                RunRejectCode::Busy
            );
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    let backend = FakeBackend::default();
    backend.fail_completion.store(true, Ordering::SeqCst);
    let registry = RunRegistry::default();
    let run = registry.reserve(&user(1), &start("r")).unwrap();
    let dropped = AtomicBool::new(false);
    let guard = StreamGuard {
        registry: &registry,
        run: &run,
        dropped: &dropped,
    };
    let mut result = Some(AgentEvent::ToolResult {
        id: "t".into(),
        name: "write_file".into(),
        ok: true,
        summary: "written".into(),
        diff: None,
    });
    let stream = stream::poll_fn(move |_| {
        let _ = &guard;
        match result.take() {
            Some(event) => std::task::Poll::Ready(Some(event)),
            None => std::task::Poll::Pending,
        }
    });
    tokio::time::timeout(
        Duration::from_secs(2),
        map_agent_events(&run, &backend, 7, stream),
    )
    .await
    .unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert!(!run.running.load(Ordering::SeqCst));
    assert!(registry.reserve(&user(1), &start("r2")).is_ok());
    assert_eq!(*backend.operations.lock().unwrap(), ["complete:t"]);

    let (sender, mut receiver) = mpsc::channel(8);
    run.clone().forward(Some(0), sender).await;
    assert!(matches!(
        recv(&mut receiver).await,
        BridgeServerMessage::RunEvent {
            seq: 1,
            event: RunEvent::ToolResult { id, .. },
            ..
        } if id == "t"
    ));
    assert!(matches!(
        recv(&mut receiver).await,
        BridgeServerMessage::RunEvent {
            seq: 2,
            event: RunEvent::Error { message },
            ..
        } if message == "failed to save tool result: offline"
    ));
    assert!(receiver.recv().await.is_none());
}

#[tokio::test]
async fn mapping_cancel_error_and_failed_persistence() {
    for event in [AgentEvent::Cancelled, AgentEvent::Error("boom".into())] {
        let backend = FakeBackend::default();
        let run = Run::new("r".into(), 1, 1, 4096);
        map_agent_events(&run, &backend, 7, stream::iter(vec![event])).await;
        assert!(backend.messages.lock().unwrap().is_empty());
    }
    let backend = FakeBackend::default();
    backend.fail_final.store(true, Ordering::SeqCst);
    let run = Run::new("r".into(), 1, 1, 4096);
    map_agent_events(
        &run,
        &backend,
        7,
        stream::iter(vec![AgentEvent::FinalText("final".into())]),
    )
    .await;
    assert!(
        matches!(events(&run).last(), Some(RunEvent::Error { message }) if message == "failed to save reply: offline")
    );
    backend.fail_final.store(false, Ordering::SeqCst);
    backend.fail_interim.store(true, Ordering::SeqCst);
    let run = Run::new("r2".into(), 1, 1, 4096);
    map_agent_events(
        &run,
        &backend,
        7,
        stream::iter(vec![
            AgentEvent::TurnCalls {
                text: "interim".into(),
                calls: vec![],
            },
            AgentEvent::ToolCall {
                id: "t".into(),
                name: "read_file".into(),
                summary: "read".into(),
            },
            AgentEvent::FinalText("final".into()),
        ]),
    )
    .await;
    assert!(
        !backend
            .operations
            .lock()
            .unwrap()
            .contains(&"step:7:t".to_string())
    );
    assert_eq!(events(&run).len(), 1);
    assert!(
        matches!(&events(&run)[0], RunEvent::Error { message } if message.starts_with("Could not save tool turn"))
    );
}

#[tokio::test]
async fn chat_success_truncation_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    for (tail, expected) in [
        (None, Some("reply")),
        (
            Some(ProviderError::Incomplete),
            Some("reply\n\n[reply truncated]"),
        ),
        (Some(ProviderError::Http("boom".into())), None),
    ] {
        let registry = RunRegistry::default();
        let backend = Arc::new(FakeBackend::default());
        let usage = TurnTelemetry {
            context: None,
            prompt_tokens: 10,
            ..Default::default()
        };
        let mut chunks = vec![
            Ok(StreamChunk::Delta("reply".into())),
            Ok(StreamChunk::Usage(usage)),
        ];
        if let Some(error) = tail {
            chunks.push(Err(error));
        }
        let run = registry
            .start(&user(1), start("r"), dir.path(), backend.clone(), |_| {
                FakeProvider {
                    chat: Mutex::new(chunks),
                    ..Default::default()
                }
            })
            .await
            .unwrap();
        finished(&run).await;
        let messages = backend.messages.lock().unwrap();
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::System);
        assert!(
            messages[1]
                .content
                .starts_with(openwebide_core::RUN_CONTEXT_PREFIX)
        );
        assert_eq!(messages.len(), if expected.is_some() { 3 } else { 2 });
        if let Some(text) = expected {
            assert_eq!(messages[2].content, text);
            assert_context_usage(messages[2].usage, usage);
        }
    }
}

#[tokio::test]
async fn busy_scoping_cancel_and_reaping() {
    let dir = tempfile::tempdir().unwrap();
    let registry = RunRegistry::default();
    let backend = Arc::new(FakeBackend::default());
    let run = registry
        .start(&user(1), start("r"), dir.path(), backend.clone(), |_| {
            FakeProvider {
                pending: true,
                ..Default::default()
            }
        })
        .await
        .unwrap();
    assert_eq!(
        registry
            .start(&user(1), start("r2"), dir.path(), backend.clone(), |_| {
                FakeProvider::default()
            })
            .await
            .unwrap_err()
            .0,
        RunRejectCode::Busy
    );
    assert_eq!(registry.get(&user(2), "r").unwrap_err(), "run not found");
    assert!(registry.list(&user(2), 1).is_empty());
    assert!(registry.get(&Principal::Paired, "r").is_err());
    assert_eq!(registry.list(&user(1), 1).len(), 1);
    registry.get(&user(1), "r").unwrap().cancel.cancel();
    finished(&run).await;
    assert_eq!(backend.messages.lock().unwrap().len(), 2);
    assert_eq!(events(&run).last(), Some(&RunEvent::Cancelled));
    registry.reap();
    assert!(registry.get(&user(1), "r").is_ok());
    *run.finished_at.lock().unwrap() = Some(Instant::now() - Duration::from_secs(601));
    registry.reap();
    assert!(registry.get(&user(1), "r").is_err());
}

#[tokio::test]
async fn rejection_never_persists_a_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeBackend::default());
    let registry = RunRegistry::default();
    let rejected = registry
        .start(
            &Principal::Paired,
            start("r"),
            dir.path(),
            backend.clone(),
            |_| FakeProvider::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.0, RunRejectCode::Unauthorized);
    for path in ["missing", "../outside"] {
        *backend.kind.lock().unwrap() = Some(RunKind::Agent {
            project_path: path.into(),
        });
        assert_eq!(
            registry
                .start(&user(1), start("r"), dir.path(), backend.clone(), |_| {
                    FakeProvider::default()
                })
                .await
                .unwrap_err()
                .0,
            RunRejectCode::ProjectUnavailable
        );
    }
    backend.fail_plan.store(true, Ordering::SeqCst);
    assert_eq!(
        registry
            .start(&user(1), start("r"), dir.path(), backend.clone(), |_| {
                FakeProvider::default()
            })
            .await
            .unwrap_err()
            .0,
        RunRejectCode::PlanFailed
    );
    assert!(backend.messages.lock().unwrap().is_empty());
    assert!(registry.list(&user(1), 1).is_empty());
}

#[tokio::test]
async fn cancel_while_agent_waits_for_permission_emits_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeBackend::default());
    *backend.kind.lock().unwrap() = Some(RunKind::Agent {
        project_path: "".into(),
    });
    let registry = RunRegistry::default();
    let provider = FakeProvider {
        tools: Mutex::new(vec![vec![Ok(ToolStreamChunk::Response(
            ChatResponse::ToolCalls(vec![ToolCall {
                id: "p".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"a","content":"x"}"#.into(),
            }]),
        ))]]),
        ..Default::default()
    };
    let run = registry
        .start(&user(1), start("r"), dir.path(), backend.clone(), |_| {
            provider
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !events(&run)
            .iter()
            .any(|e| matches!(e, RunEvent::PermissionRequest { .. }))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    run.cancel.cancel();
    finished(&run).await;
    assert_eq!(events(&run).last(), Some(&RunEvent::Cancelled));
    let messages = backend.messages.lock().unwrap();
    assert_eq!(messages.len(), 3);
    assert!(messages[2].content.is_empty());
    assert!(messages[2].tool_calls.is_some());
    assert!(!dir.path().join("a").exists());
}

async fn recv(receiver: &mut mpsc::Receiver<WriterCmd>) -> BridgeServerMessage {
    match tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
    {
        WriterCmd::Send(message) => *message,
        WriterCmd::Attach { .. } => unreachable!(),
    }
}

#[tokio::test]
async fn attach_replays_without_gaps_or_duplicates_and_snapshots_outside_ring() {
    for cursor in [Some(1), None, Some(0)] {
        let run = Arc::new(Run::new("r".into(), 1, 1, 2));
        for text in ["a", "b", "c"] {
            run.emit(RunEvent::Delta {
                content: text.into(),
            });
        }
        let (sender, mut receiver) = mpsc::channel(8);
        let forward = tokio::spawn(run.clone().forward(cursor, sender));
        if cursor == Some(1) {
            for expected in [2, 3] {
                assert!(
                    matches!(recv(&mut receiver).await, BridgeServerMessage::RunEvent { seq, .. } if seq == expected)
                );
            }
        } else {
            assert!(
                matches!(recv(&mut receiver).await, BridgeServerMessage::RunSnapshot { seq: 3, snapshot, .. } if snapshot.text == "abc")
            );
        }
        run.emit(RunEvent::Delta {
            content: "d".into(),
        });
        assert!(matches!(
            recv(&mut receiver).await,
            BridgeServerMessage::RunEvent { seq: 4, .. }
        ));
        run.emit(RunEvent::Cancelled);
        assert!(matches!(
            recv(&mut receiver).await,
            BridgeServerMessage::RunEvent {
                seq: 5,
                event: RunEvent::Cancelled,
                ..
            }
        ));
        forward.await.unwrap();
        assert!(receiver.recv().await.is_none());
    }
}

#[tokio::test]
async fn lagging_attachment_resyncs_snapshot_then_live() {
    let run = Arc::new(Run::new("r".into(), 1, 1, 2));
    let (sender, mut receiver) = mpsc::channel(1);
    let forward = tokio::spawn(run.clone().forward(None, sender));
    tokio::task::yield_now().await;
    for _ in 0..8 {
        run.emit(RunEvent::Delta {
            content: "x".into(),
        });
    }
    tokio::task::yield_now().await;
    assert!(matches!(
        recv(&mut receiver).await,
        BridgeServerMessage::RunSnapshot { seq: 0, .. }
    ));
    assert!(
        matches!(recv(&mut receiver).await, BridgeServerMessage::RunSnapshot { seq: 8, snapshot, .. } if snapshot.text == "xxxxxxxx")
    );
    run.emit(RunEvent::Cancelled);
    assert!(matches!(
        recv(&mut receiver).await,
        BridgeServerMessage::RunEvent { seq: 9, .. }
    ));
    forward.await.unwrap();
}

#[tokio::test]
async fn gate_default_policy_matches_other_hosts() {
    let gate = BridgeGate::new(BridgeCancel::default());
    assert!(gate.needs_approval(&ToolCall {
        id: "t".into(),
        name: "write_file".into(),
        arguments: "{}".into()
    }));
    assert!(!gate.needs_approval(&ToolCall {
        id: "t".into(),
        name: "read_file".into(),
        arguments: "{}".into()
    }));
}

#[cfg(feature = "tls")]
#[tokio::test]
async fn websocket_scopes_attach_cancel_permission_and_list() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let dir = tempfile::tempdir().unwrap();
    let config = crate::ServerConfig::new(dir.path().to_path_buf(), "secret".into(), None);
    let backend = Arc::new(FakeBackend::default());
    let run = config
        .runs
        .start(&user(1), start("owned"), dir.path(), backend, |_| {
            FakeProvider {
                pending: true,
                ..Default::default()
            }
        })
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(crate::run_server(listener, config));
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{address}/"))
        .await
        .unwrap();
    let token =
        openwebide_auth::sign_token_expires("secret", 2, i64::try_from(now()).unwrap() + 120, 0);
    ws.send(Message::Text(
        serde_json::to_string(&openwebide_core::BridgeClientMessage::Hello { token })
            .unwrap()
            .into(),
    ))
    .await
    .unwrap();
    ws.next().await.unwrap().unwrap();
    for command in [
        openwebide_core::BridgeClientMessage::RunAttach {
            run_id: "owned".into(),
            last_seq: None,
        },
        openwebide_core::BridgeClientMessage::RunCancel {
            run_id: "owned".into(),
        },
        openwebide_core::BridgeClientMessage::RunPermission {
            run_id: "owned".into(),
            tool_call_id: "t".into(),
            approved: true,
        },
    ] {
        ws.send(Message::Text(
            serde_json::to_string(&command).unwrap().into(),
        ))
        .await
        .unwrap();
        let message: BridgeServerMessage =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert!(
            matches!(message, BridgeServerMessage::Error { id, message } if id == "owned" && message == "run not found")
        );
    }
    assert!(!run.cancel.is_cancelled());
    ws.send(Message::Text(
        r#"{"type":"run_list","session_id":1}"#.into(),
    ))
    .await
    .unwrap();
    let message: BridgeServerMessage =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert!(matches!(message, BridgeServerMessage::Runs { runs, .. } if runs.is_empty()));
    run.cancel.cancel();
    finished(&run).await;
    server.abort();
}

#[tokio::test]
async fn agent_body_executes_native_tools_and_persists_interim_then_final() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a"), "native file").unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "Native root instruction").unwrap();
    let backend = Arc::new(FakeBackend::default());
    *backend.kind.lock().unwrap() = Some(RunKind::Agent {
        project_path: "".into(),
    });
    let first = TurnTelemetry {
        context: None,
        prompt_tokens: 1000,
        ..Default::default()
    };
    let last = TurnTelemetry {
        context: None,
        prompt_tokens: 2000,
        ..Default::default()
    };
    let provider = FakeProvider {
        tools: Mutex::new(vec![
            vec![
                Ok(ToolStreamChunk::Delta("interim".into())),
                Ok(ToolStreamChunk::Usage(first)),
                Ok(ToolStreamChunk::Response(ChatResponse::ToolCalls(vec![
                    ToolCall {
                        id: "p".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"a"}"#.into(),
                    },
                ]))),
            ],
            vec![
                Ok(ToolStreamChunk::Delta("final".into())),
                Ok(ToolStreamChunk::Usage(last)),
                Ok(ToolStreamChunk::Response(ChatResponse::Text(
                    "final".into(),
                ))),
            ],
        ]),
        ..Default::default()
    };
    let registry = RunRegistry::default();
    let run = registry
        .start(&user(1), start("r"), dir.path(), backend.clone(), |_| {
            provider
        })
        .await
        .unwrap();
    finished(&run).await;
    let messages = backend.messages.lock().unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role != Role::System)
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        ["go", "interim", "final"]
    );
    assert_eq!(messages[1].role, Role::System);
    assert!(messages[1].content.contains("Native root instruction"));
    assert_context_usage(messages[2].usage, first);
    assert_context_usage(messages[3].usage, last);
    assert!(messages[3].usage.unwrap().context.unwrap().files > 0);
    assert_eq!(
        *backend.operations.lock().unwrap(),
        [
            "message:10",
            "message:11",
            "message:12",
            "step:12:a10t1c0",
            "complete:a10t1c0",
            "message:13"
        ]
    );
    assert!(
        events(&run)
            .iter()
            .any(|event| matches!(event, RunEvent::ToolResult { ok: true, .. }))
    );
}

#[tokio::test]
async fn seeded_memo_and_new_detection_record_only_once() {
    let mut connection = plan(RunKind::Chat, "test").connection;
    connection.kind = ProviderKind::LlamaCpp;
    connection.tool_stream_unsupported = true;
    let memos = openwebide_llm::ToolStreamMemos::default();
    let backend = FakeBackend::default();
    let seeded = memos.get_or_insert(&connection);
    assert!(seeded.unsupported());
    record_tool_stream_memo(
        &backend,
        7,
        connection.id,
        connection.tool_stream_revision,
        connection.model.as_deref(),
        &seeded,
    )
    .await;
    assert!(backend.operations.lock().unwrap().is_empty());
    connection.base_url = "http://new-server".into();
    connection.tool_stream_revision += 1;
    connection.tool_stream_unsupported = false;
    let fresh = memos.get_or_insert(&connection);
    assert!(!fresh.unsupported());
    fresh.mark_unsupported();
    record_tool_stream_memo(
        &backend,
        7,
        connection.id,
        connection.tool_stream_revision,
        connection.model.as_deref(),
        &fresh,
    )
    .await;
    record_tool_stream_memo(
        &backend,
        7,
        connection.id,
        connection.tool_stream_revision,
        connection.model.as_deref(),
        &memos.get_or_insert(&connection),
    )
    .await;
    assert_eq!(*backend.operations.lock().unwrap(), vec!["memo:7:1:1"]);
    connection.kind = ProviderKind::Ollama;
    assert!(!memos.get_or_insert(&connection).unsupported());
}

#[tokio::test]
async fn empty_interim_persists_wire_calls_before_step_rows() {
    let backend = FakeBackend::default();
    let run = Run::new("r".into(), 1, 2, 4096);
    let calls = vec![ToolCall {
        id: "wire-id".into(),
        name: "read_file".into(),
        arguments: "{}".into(),
    }];
    map_agent_events(
        &run,
        &backend,
        7,
        stream::iter([
            AgentEvent::TurnCalls {
                text: String::new(),
                calls: calls.clone(),
            },
            AgentEvent::ToolCall {
                id: "a7t1c0".into(),
                name: "read_file".into(),
                summary: "read".into(),
            },
        ]),
    )
    .await;
    let messages = backend.messages.lock().unwrap();
    assert_eq!(messages[0].content, "");
    assert_eq!(messages[0].tool_calls.as_ref(), Some(&calls));
    assert_eq!(
        *backend.operations.lock().unwrap(),
        vec!["message:10", "step:10:a7t1c0"]
    );
}

#[tokio::test]
async fn cancel_running_command_kills_group_before_marker() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeBackend::default());
    *backend.kind.lock().unwrap() = Some(RunKind::Agent {
        project_path: "".into(),
    });
    let provider = FakeProvider {
        tools: Mutex::new(vec![vec![Ok(ToolStreamChunk::Response(ChatResponse::ToolCalls(vec![ToolCall {
            id: "p".into(), name: "run_command".into(),
            arguments: serde_json::json!({"command": "touch started; sleep 30; touch marker", "timeout_seconds": 60}).to_string(),
        }])) )]]), ..Default::default()
    };
    let registry = RunRegistry::default();
    let run = registry
        .start(&user(1), start("r"), dir.path(), backend, |_| provider)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(RunEvent::PermissionRequest { id, .. }) = events(&run)
                .iter()
                .find(|e| matches!(e, RunEvent::PermissionRequest { .. }))
            {
                run.gate.decide(id, true).unwrap();
                break;
            }
            tokio::task::yield_now().await;
        }
        while !dir.path().join("started").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let started = Instant::now();
    registry.get(&user(1), "r").unwrap().cancel.cancel();
    finished(&run).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    let emitted = events(&run);
    assert!(
        matches!(&emitted[emitted.len()-3], RunEvent::ToolResult { ok: false, summary, .. } if summary == "cancelled")
    );
    assert!(
        matches!(&emitted[emitted.len()-2], RunEvent::ToolTiming { timing, .. } if timing.finished)
    );
    assert_eq!(emitted.last(), Some(&RunEvent::Cancelled));
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert!(!dir.path().join("marker").exists());
}

#[tokio::test]
async fn reasoning_is_streamed_and_persisted_per_agent_turn() {
    let backend = FakeBackend::default();
    let run = Run::new("r".into(), 1, 1, 4096);
    let answer = format!("answer{}", openwebide_core::REPLY_CUT_OFF_MARKER);
    map_agent_events(
        &run,
        &backend,
        7,
        stream::iter([
            AgentEvent::ReasoningDelta("first".into()),
            AgentEvent::TurnCalls {
                text: "checking".into(),
                calls: vec![],
            },
            AgentEvent::ReasoningDelta("r".into()),
            AgentEvent::FinalText(answer.clone()),
        ]),
    )
    .await;
    let messages = backend.messages.lock().unwrap();
    assert_eq!(messages[0].content, "<think>first</think>checking");
    assert_eq!(messages[1].content, format!("<think>r</think>{answer}"));
    assert!(matches!(&events(&run)[0], RunEvent::ReasoningDelta { content } if content == "first"));
}

#[tokio::test]
async fn incomplete_reasoning_only_chat_is_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let registry = RunRegistry::default();
    let backend = Arc::new(FakeBackend::default());
    let reasoning = "analysis with </think> literal";
    let run = registry
        .start(&user(1), start("r"), dir.path(), backend.clone(), |_| {
            FakeProvider {
                chat: Mutex::new(vec![
                    Ok(StreamChunk::Reasoning(reasoning.into())),
                    Err(ProviderError::Incomplete),
                ]),
                ..Default::default()
            }
        })
        .await
        .unwrap();
    finished(&run).await;
    let messages = backend.messages.lock().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[2].content,
        openwebide_core::with_reasoning(reasoning, REPLY_TRUNCATED_MARKER)
    );
    assert!(matches!(events(&run).last(), Some(RunEvent::Done { .. })));
}

#[derive(Default)]
struct FakeExecution {
    calls: Mutex<Vec<(String, PathBuf)>>,
}

impl crate::exec::ToolExecution for FakeExecution {
    fn run_command(
        &self,
        spec: crate::exec::SpawnSpec,
    ) -> crate::exec::ExecutionFuture<crate::exec::ExecOutput> {
        self.calls
            .lock()
            .unwrap()
            .push((spec.args.last().unwrap().clone(), spec.cwd));
        Box::pin(async {
            Ok(openwebide_core::CommandOutcome {
                exit_code: Some(0),
                stdout: "fake execution".into(),
                stderr: String::new(),
            })
        })
    }

    fn git(
        &self,
        request: crate::exec::GitRequest,
    ) -> crate::exec::ExecutionFuture<crate::exec::GitResponse> {
        assert!(matches!(
            request.operation,
            crate::exec::GitOperation::Status
        ));
        self.calls
            .lock()
            .unwrap()
            .push(("git status".into(), request.cwd));
        Box::pin(async {
            Ok(crate::exec::GitResponse::Status(
                openwebide_core::GitRepoStatus {
                    branch: "fake".into(),
                    ..Default::default()
                },
            ))
        })
    }
}

#[tokio::test]
async fn http_and_agent_run_use_configured_execution() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let execution = Arc::new(FakeExecution::default());
    let mut config = crate::ServerConfig::new(root.clone(), "secret".into(), None);
    config.execution = execution.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server_config = config.clone();
    let server = tokio::spawn(crate::run_server_until(listener, server_config, async {
        let _ = stopped.await;
    }));
    for (path, body, expected) in [
        (
            "/exec",
            r#"{"command":"touch http-marker"}"#,
            "fake execution",
        ),
        ("/git/status", r#"{"cwd":"."}"#, "fake"),
    ] {
        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer secret\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            addr.port(),
            body.len()
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains(expected), "{response}");
    }
    let backend = Arc::new(FakeBackend::default());
    *backend.kind.lock().unwrap() = Some(RunKind::Agent {
        project_path: "".into(),
    });
    let provider = FakeProvider {
        tools: Mutex::new(vec![
            vec![Ok(ToolStreamChunk::Response(ChatResponse::ToolCalls(
                vec![
                    ToolCall {
                        id: "command".into(),
                        name: "run_command".into(),
                        arguments: r#"{"command":"touch run-marker"}"#.into(),
                    },
                    ToolCall {
                        id: "git".into(),
                        name: "git_status".into(),
                        arguments: "{}".into(),
                    },
                ],
            )))],
            vec![Ok(ToolStreamChunk::Response(ChatResponse::Text(
                "final".into(),
            )))],
        ]),
        ..Default::default()
    };
    let start = start("fake-run");
    let run = config.runs.reserve(&user(1), &start).unwrap();
    let run = config
        .runs
        .prepare(
            run,
            start,
            &config.workspace_root,
            backend,
            |_| provider,
            RunHost {
                execution: config.execution.clone(),
                plugins: crate::plugins::transport::PluginExecutionHost {
                    paired: false,
                    installer: config.plugins.clone(),
                    invocations: config.plugin_invocations.clone(),
                },
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(RunEvent::PermissionRequest { id, .. }) = events(&run)
                .iter()
                .find(|event| matches!(event, RunEvent::PermissionRequest { .. }))
            {
                run.gate.decide(id, true).unwrap();
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    finished(&run).await;
    assert!(matches!(events(&run).last(), Some(RunEvent::Done { .. })));
    assert_eq!(
        events(&run)
            .iter()
            .filter(|event| matches!(event, RunEvent::ToolResult { ok: true, .. }))
            .count(),
        2
    );
    assert_eq!(
        *execution.calls.lock().unwrap(),
        [
            ("touch http-marker".into(), root.clone()),
            ("git status".into(), root.clone()),
            ("git status".into(), root.clone()),
            ("touch run-marker".into(), root.clone()),
            ("git status".into(), root.clone()),
        ]
    );
    assert!(!root.join("http-marker").exists());
    assert!(!root.join("run-marker").exists());
    stop.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn remote_agent_uses_shared_policy_before_prompting_and_writes_the_file() {
    for mode in [
        openwebide_core::ApprovalMode::AutoAcceptEdits,
        openwebide_core::ApprovalMode::Yolo,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(FakeBackend::default());
        *backend.kind.lock().unwrap() = Some(RunKind::Agent {
            project_path: String::new(),
        });
        *backend.approval_mode.lock().unwrap() = mode;
        let provider = FakeProvider {
            tools: Mutex::new(vec![
                vec![Ok(ToolStreamChunk::Response(ChatResponse::ToolCalls(
                    vec![ToolCall {
                        id: "wire".into(),
                        name: "write_file".into(),
                        arguments: r#"{"path":"edited.txt","content":"changed"}"#.into(),
                    }],
                )))],
                vec![Ok(ToolStreamChunk::Response(ChatResponse::Text(
                    "final".into(),
                )))],
            ]),
            ..Default::default()
        };
        let registry = RunRegistry::default();
        let run = registry
            .start(&user(1), start("r"), dir.path(), backend.clone(), |_| {
                provider
            })
            .await
            .unwrap();
        finished(&run).await;
        assert_eq!(
            std::fs::read_to_string(dir.path().join("edited.txt")).unwrap(),
            "changed"
        );
        assert!(
            !events(&run)
                .iter()
                .any(|event| matches!(event, RunEvent::PermissionRequest { .. }))
        );
        let checks = backend.approval_checks.lock().unwrap();
        assert_eq!(checks.len(), 1);
        assert!(checks[0].call.arguments.contains("edited.txt"));
        assert!(checks[0].call.id.starts_with("a10t"));
    }
}

#[tokio::test]
async fn both_server_run_paths_save_compaction_before_the_reply_and_keep_originals() {
    let dir = tempfile::tempdir().unwrap();
    for kind in [
        RunKind::Chat,
        RunKind::WebChat,
        RunKind::Agent {
            project_path: ".".into(),
        },
    ] {
        let registry = RunRegistry::default();
        let backend = Arc::new(FakeBackend::default());
        *backend.kind.lock().unwrap() = Some(kind);
        let mut request = plan(RunKind::Chat, "go").request;
        request.model_settings.context_limit = Some(4096);
        let original = ChatMessage {
            id: 1,
            session_id: 1,
            role: Role::Assistant,
            content: "old findings ".repeat(1500),
            created_at: 0,
            tool_calls: None,
            tool_call_id: None,
            usage: None,
        };
        request.messages.push(original.clone());
        backend.messages.lock().unwrap().push(original.clone());
        *backend.compaction_request.lock().unwrap() = Some(request);
        let run = registry
            .start(
                &user(1),
                start("compact"),
                dir.path(),
                backend.clone(),
                |_| FakeProvider {
                    chat: Mutex::new(vec![Ok(StreamChunk::Delta("final".into()))]),
                    tools: Mutex::new(vec![vec![Ok(ToolStreamChunk::Response(
                        ChatResponse::Text("final".into()),
                    ))]]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        finished(&run).await;
        let messages = backend.messages.lock().unwrap();
        assert_eq!(messages[0], original);
        let summary = messages
            .iter()
            .find(|message| {
                message
                    .content
                    .starts_with(openwebide_core::COMPACTION_PREFIX)
            })
            .unwrap();
        let metadata = openwebide_core::Compaction::parse(&summary.content).unwrap();
        assert_eq!(metadata.retained[0].content, "go");
        assert!(metadata.through_message_id < summary.id);
        assert_eq!(messages.last().unwrap().content, "final");
        assert!(summary.id < messages.last().unwrap().id);
        assert!(backend.summaries.lock().unwrap().len() > 1);
        assert!(matches!(events(&run).last(), Some(RunEvent::Done { .. })));
    }
}

#[tokio::test]
async fn projectless_run_requires_plugin_web_tools_and_rejects_workspace_calls() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("private.txt"), "secret").unwrap();
    let backend = Arc::new(FakeBackend::default());
    *backend.kind.lock().unwrap() = Some(RunKind::WebChat);
    *backend.approval_mode.lock().unwrap() = openwebide_core::ApprovalMode::Yolo;
    let calls = vec![
        ToolCall {
            id: "host".into(),
            name: "host_info".into(),
            arguments: "{}".into(),
        },
        ToolCall {
            id: "web".into(),
            name: "search_web".into(),
            arguments: r#"{"query":"rust"}"#.into(),
        },
        ToolCall {
            id: "fetch".into(),
            name: "fetch_web_page".into(),
            arguments: r#"{"url":"https://example.com"}"#.into(),
        },
        ToolCall {
            id: "read".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"private.txt"}"#.into(),
        },
        ToolCall {
            id: "write".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"private.txt","content":"changed"}"#.into(),
        },
        ToolCall {
            id: "shell".into(),
            name: "run_command".into(),
            arguments: r#"{"command":"touch forbidden"}"#.into(),
        },
        ToolCall {
            id: "git".into(),
            name: "git_status".into(),
            arguments: "{}".into(),
        },
    ];
    let provider = FakeProvider {
        tools: Mutex::new(vec![
            vec![Ok(ToolStreamChunk::Response(ChatResponse::ToolCalls(
                calls,
            )))],
            vec![Ok(ToolStreamChunk::Response(ChatResponse::Text(
                "final".into(),
            )))],
        ]),
        ..Default::default()
    };
    let run = RunRegistry::default()
        .start(
            &user(1),
            start("web-chat"),
            dir.path(),
            backend.clone(),
            |_| provider,
        )
        .await
        .unwrap();
    finished(&run).await;
    let events = events(&run);
    for (name, expected) in [
        ("host_info", true),
        ("search_web", false),
        ("fetch_web_page", false),
        ("read_file", false),
        ("write_file", false),
        ("run_command", false),
        ("git_status", false),
    ] {
        assert!(events.iter().any(|event| matches!(event, RunEvent::ToolResult { name:actual, ok, .. } if actual == name && *ok == expected)));
    }
    assert!(matches!(events.last(), Some(RunEvent::Done { .. })));
    assert!(
        !backend
            .operations
            .lock()
            .unwrap()
            .iter()
            .any(|operation| operation.starts_with("legacy web"))
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("private.txt")).unwrap(),
        "secret"
    );
    assert!(!dir.path().join("forbidden").exists());
    assert!(
        !backend
            .messages
            .lock()
            .unwrap()
            .iter()
            .any(|message| message.content.contains("secret"))
    );
}

fn assert_context_usage(actual: Option<TurnTelemetry>, expected: TurnTelemetry) {
    let mut actual = actual.unwrap();
    let context = actual
        .context
        .take()
        .expect("run must persist its request breakdown");
    assert_eq!(context.total(), actual.prompt_tokens);
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn queued_run_uses_atomic_delivery_after_preflight_and_never_falls_back_to_plain_insert() {
    let dir = tempfile::tempdir().unwrap();
    let registry = RunRegistry::default();
    let backend = Arc::new(FakeBackend::default());
    let key = openwebide_core::QueuedPromptKey { id: 8, revision: 2 };
    let queued_start = |id| StartRun {
        queued_prompt: Some(key),
        ..start(id)
    };
    backend.fail_plan.store(true, Ordering::SeqCst);
    assert!(
        registry
            .start(
                &user(1),
                queued_start("preflight"),
                dir.path(),
                backend.clone(),
                |_| FakeProvider::default()
            )
            .await
            .is_err()
    );
    assert!(backend.queue_deliveries.lock().unwrap().is_empty());
    assert!(backend.messages.lock().unwrap().is_empty());
    backend.fail_plan.store(false, Ordering::SeqCst);
    backend.fail_queue.store(true, Ordering::SeqCst);
    assert!(
        registry
            .start(
                &user(1),
                queued_start("stale"),
                dir.path(),
                backend.clone(),
                |_| FakeProvider::default()
            )
            .await
            .is_err()
    );
    assert!(
        backend.messages.lock().unwrap().is_empty(),
        "a stale queue revision must not fall back to an ordinary send"
    );
    backend.fail_queue.store(false, Ordering::SeqCst);
    let run = registry
        .start(
            &user(1),
            queued_start("delivery"),
            dir.path(),
            backend.clone(),
            |_| FakeProvider {
                chat: Mutex::new(vec![Ok(StreamChunk::Delta("reply".into()))]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    finished(&run).await;
    assert_eq!(*backend.queue_deliveries.lock().unwrap(), [key]);
    let messages = backend.messages.lock().unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
    assert_eq!(messages[0].content, "go");
}

#[tokio::test]
async fn native_and_projectless_runs_persist_checklists_before_success_and_keep_failed_updates() {
    use openwebide_core::{TodoItem, TodoPlan, TodoStatus};
    for project in [true, false] {
        for failure in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let backend = Arc::new(FakeBackend::default());
            *backend.kind.lock().unwrap() = Some(if project {
                RunKind::Agent {
                    project_path: "".into(),
                }
            } else {
                RunKind::WebChat
            });
            backend.fail_todo.store(failure, Ordering::SeqCst);
            let plan = TodoPlan {
                todos: vec![TodoItem {
                    id: "inspect".into(),
                    content: "Inspect the code".into(),
                    status: TodoStatus::InProgress,
                }],
            };
            let provider = FakeProvider {
                tools: Mutex::new(vec![
                    vec![Ok(ToolStreamChunk::Response(ChatResponse::ToolCalls(
                        vec![ToolCall {
                            id: "plan".into(),
                            name: "todo_write".into(),
                            arguments: serde_json::to_string(&plan).unwrap(),
                        }],
                    )))],
                    vec![Ok(ToolStreamChunk::Response(ChatResponse::Text(
                        "final".into(),
                    )))],
                ]),
                ..Default::default()
            };
            let run = RunRegistry::default()
                .start(
                    &user(1),
                    start("plan-run"),
                    dir.path(),
                    backend.clone(),
                    |_| provider,
                )
                .await
                .unwrap();
            finished(&run).await;
            let emitted = events(&run);
            assert!(emitted.iter().any(|event| matches!(event, RunEvent::ToolResult { name, ok, .. } if name == "todo_write" && *ok != failure)));
            assert!(matches!(emitted.last(), Some(RunEvent::Done { .. })));
            assert!(
                !emitted
                    .iter()
                    .any(|event| matches!(event, RunEvent::PermissionRequest { .. }))
            );
            let stored = backend.todo_plan.lock().unwrap().clone();
            if failure {
                assert!(stored.is_none());
            } else {
                let stored = stored.unwrap();
                assert_eq!(stored.plan, plan);
                let messages = backend.messages.lock().unwrap();
                assert_eq!(
                    stored.anchor_message_id,
                    messages
                        .iter()
                        .find(|message| message.role == Role::User)
                        .unwrap()
                        .id
                );
                assert_eq!(stored.session_id, 1);
            }
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        }
    }
}

#[tokio::test]
async fn browser_preferences_reach_bridge_run_context() {
    for kind in [
        RunKind::Chat,
        RunKind::WebChat,
        RunKind::Agent {
            project_path: ".".into(),
        },
    ] {
        let dir = tempfile::tempdir().unwrap();
        let registry = RunRegistry::default();
        let backend = Arc::new(FakeBackend::default());
        *backend.kind.lock().unwrap() = Some(kind);
        let mut input = start("browser-defaults");
        input.browser_preferences = Some(openwebide_core::BrowserPreferences {
            timezone: Some("Asia/Kathmandu".into()),
            locale: Some("en-GB".into()),
            hour_cycle: Some("h23".into()),
            utc_offset_minutes: Some(345),
        });
        let run = registry
            .start(&user(1), input, dir.path(), backend.clone(), |_| {
                FakeProvider {
                    chat: Mutex::new(vec![Ok(StreamChunk::Delta("done".into()))]),
                    tools: Mutex::new(vec![vec![Ok(ToolStreamChunk::Response(
                        ChatResponse::Text("done".into()),
                    ))]]),
                    ..Default::default()
                }
            })
            .await
            .unwrap();
        finished(&run).await;
        let messages = backend.messages.lock().unwrap();
        let context = messages
            .iter()
            .find(|message| {
                message
                    .content
                    .starts_with(openwebide_core::RUN_CONTEXT_PREFIX)
            })
            .unwrap();
        assert!(context.content.contains("Asia/Kathmandu"));
        assert!(context.content.contains("en-GB"));
        assert!(context.content.contains("24-hour"));
        assert!(context.content.contains("UTC+05:45"));
    }
}

#[tokio::test]
async fn dropping_preparation_releases_the_reservation_and_owned_backend_lease() {
    use std::future::Future;
    let registry = RunRegistry::default();
    let backend = Arc::new(FakeBackend::default());
    backend.hold_plan.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let principal = user(1);
    let mut preparing = Box::pin(registry.start(
        &principal,
        start("preparing"),
        dir.path(),
        backend.clone(),
        |_| FakeProvider::default(),
    ));
    let mut context = std::task::Context::from_waker(futures::task::noop_waker_ref());
    assert!(preparing.as_mut().poll(&mut context).is_pending());
    let run = registry.get(&principal, "preparing").unwrap();
    assert!(registry.list(&principal, 1)[0].running);
    drop(preparing);
    assert!(matches!(
        run.scheduled_status().0,
        Some(RunEvent::Cancelled)
    ));
    assert!(!registry.list(&principal, 1)[0].running);
    tokio::task::yield_now().await;
    assert!(
        backend
            .released_leases
            .lock()
            .unwrap()
            .iter()
            .any(|id| id == "preparing")
    );
    assert!(registry.reserve(&principal, &start("replacement")).is_ok());
}

mod plugin_run_delivery {
    use super::*;
    use openwebide_agent::plugins::runs::RunHost as DeliveryHost;
    use openwebide_core::plugins::runs::*;

    #[derive(Clone)]
    struct Host {
        registry: Arc<RunRegistry>,
        backend: Arc<FakeBackend>,
        root: Arc<tempfile::TempDir>,
        reports: Arc<Mutex<Vec<RunReport>>>,
    }
    impl DeliveryHost for Host {
        type Run = Arc<Run>;
        async fn request(&self, command: RunServiceRequest) -> Result<RunServiceResponse, String> {
            match command {
                RunServiceRequest::Renew { .. } => {
                    Ok(RunServiceResponse::Renewed(RunLeaseStatus {
                        expires_at: 120,
                        cancel_requested: false,
                    }))
                }
                RunServiceRequest::Report { report, .. } => {
                    report.validate()?;
                    self.reports.lock().unwrap().push(report);
                    Ok(RunServiceResponse::Reported)
                }
                _ => unreachable!(),
            }
        }
        async fn start(&self, delivery: &RunDelivery) -> Result<Arc<Run>, (RunRejectCode, String)> {
            self.registry
                .start(
                    &user(delivery.user_id),
                    StartRun {
                        run_id: delivery.run_id(),
                        session_id: delivery.prompt.session_id,
                        queued_prompt: Some(delivery.prompt.key()),
                        content: delivery.prompt.content.clone(),
                        host_path: delivery.host_path.clone(),
                        model: None,
                        editor_context: None,
                        browser_preferences: None,
                    },
                    self.root.path(),
                    self.backend.clone(),
                    |_| FakeProvider {
                        chat: Mutex::new(vec![Ok(StreamChunk::Delta("Build passed".into()))]),
                        tools: Mutex::new(vec![vec![Ok(ToolStreamChunk::Response(
                            ChatResponse::Text("Build passed".into()),
                        ))]]),
                        ..Default::default()
                    },
                )
                .await
        }
        fn snapshot(&self, run: &Arc<Run>) -> (Option<RunEvent>, Option<openwebide_core::RunStep>) {
            run.scheduled_status()
        }
        fn cancel(&self, delivery: &RunDelivery) {
            if let Ok(run) = self
                .registry
                .get(&user(delivery.user_id), &delivery.run_id())
            {
                run.cancel.cancel();
            }
        }
        fn now(&self) -> i64 {
            0
        }
        async fn wait(&self, duration: Duration) {
            if duration.as_secs() == 30 {
                futures::future::pending::<()>().await;
            } else {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        fn report(&self, _: &str) {}
    }

    #[tokio::test]
    async fn raw_delivery_consumes_once_and_reports_persisted_runner_output_for_both_host_bindings()
    {
        for mode in ["server", "paired-host"] {
            let host = Host {
                registry: Arc::default(),
                backend: Arc::default(),
                root: Arc::new(tempfile::tempdir().unwrap()),
                reports: Arc::default(),
            };
            let delivery = RunDelivery {
                user_id: 1,
                run: PluginRun {
                    id: 5,
                    revision: 2,
                    key: "check:1".into(),
                    session_id: Some(1),
                    state: RunState::Leased,
                    created_at: 0,
                    detail: String::new(),
                    message_id: None,
                    permission_id: None,
                },
                prompt: openwebide_core::QueuedPrompt {
                    scheduled_task: None,
                    plugin_run: Some(5),
                    id: 8,
                    session_id: 1,
                    revision: 2,
                    content: "Check the build".into(),
                    created_at: 0,
                    guidance: false,
                },
                host_path: (mode != "server").then(|| host.root.path().to_string_lossy().into()),
                lease: RunLease {
                    host_id: mode.into(),
                    id: 5,
                    lease: "unique-nonce".into(),
                },
                lease_expires_at: 120,
            };
            tokio::time::timeout(
                Duration::from_secs(2),
                openwebide_agent::plugins::runs::deliver(&host, delivery.clone()),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                *host.backend.queue_deliveries.lock().unwrap(),
                [delivery.prompt.key()]
            );
            let messages = host.backend.messages.lock().unwrap();
            assert_eq!(messages.iter().filter(|m| m.role == Role::User).count(), 1);
            assert!(
                messages
                    .iter()
                    .any(|m| m.role == Role::Assistant && m.content == "Build passed")
            );
            let reports = host.reports.lock().unwrap();
            let terminal = reports.last().unwrap();
            assert_eq!(terminal.state, RunState::Completed);
            assert_eq!(terminal.detail, "Build passed");
        }
    }
}
