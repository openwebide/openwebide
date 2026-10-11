use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{Stream, StreamExt};
use openwebide_agent::{AgentConfig, AgentEvent, VfsToolExecutor};
use openwebide_core::{
    BridgeServerMessage, ChatMessage, EditorContext, Role, RunEvent, RunInfo, RunKind, RunPlan,
    RunRejectCode, RunSnapshot, TurnTelemetry,
};
use openwebide_llm::LlmProvider;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::auth::Principal;
use crate::runs::agent_host::{BridgeCancel, BridgeGate, InProcessBridgeClient};
use crate::runs::backend_client::RunBackend;
use crate::runs::native_vfs::NativeFsVfs;
use crate::server::WriterCmd;
use crate::terminals::seq_ring::SeqRing;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

struct Delivery {
    ring: SeqRing<RunEvent>,
    snapshot: RunSnapshot,
    seq: u64,
}

pub struct Run {
    pub run_id: String,
    owner: i64,
    host_administration: AtomicBool,
    session_id: i64,
    started_at: u64,
    running: AtomicBool,
    finished_at: Mutex<Option<Instant>>,
    delivery: Mutex<Delivery>,
    pub cancel: BridgeCancel,
    pub gate: BridgeGate,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run").field("run_id", &self.run_id).finish()
    }
}

impl Run {
    fn new(run_id: String, owner: i64, session_id: i64, capacity: usize) -> Self {
        let cancel = BridgeCancel::default();
        Self {
            run_id,
            owner,
            host_administration: AtomicBool::new(false),
            session_id,
            started_at: now(),
            running: AtomicBool::new(true),
            finished_at: Mutex::new(None),
            delivery: Mutex::new(Delivery {
                ring: SeqRing::new(None, Some(capacity)),
                snapshot: RunSnapshot::default(),
                seq: 0,
            }),
            gate: BridgeGate::new(cancel.clone()),
            cancel,
        }
    }
    pub fn emit(&self, event: RunEvent) {
        let finished = matches!(
            event,
            RunEvent::Done { .. } | RunEvent::Cancelled | RunEvent::Error { .. }
        );
        let mut delivery = self.delivery.lock().unwrap();
        delivery.snapshot.apply(&event);
        delivery.seq = delivery.ring.push(|_| event, 0);
        if finished {
            self.gate.clear();
            *self.finished_at.lock().unwrap() = Some(Instant::now());
            self.running.store(false, Ordering::SeqCst);
        }
    }
    pub fn scheduled_status(&self) -> (Option<RunEvent>, Option<openwebide_core::RunStep>) {
        let delivery = self.delivery.lock().unwrap();
        (
            delivery.snapshot.finished.clone(),
            delivery.snapshot.items.iter().find_map(|item| match item {
                openwebide_core::RunItem::Step(step)
                    if step.awaiting_permission && step.result.is_none() =>
                {
                    Some(step.clone())
                }
                _ => None,
            }),
        )
    }
    fn snapshot_message(&self, delivery: &Delivery) -> BridgeServerMessage {
        let mut snapshot = delivery.snapshot.clone();
        snapshot.refresh_tool_timings(wall_time_ms());
        BridgeServerMessage::RunSnapshot {
            run_id: self.run_id.clone(),
            session_id: self.session_id,
            seq: delivery.seq,
            snapshot,
        }
    }
    pub(crate) async fn forward(
        self: Arc<Self>,
        last_seq: Option<u64>,
        sender: mpsc::Sender<WriterCmd>,
    ) {
        let mut watch = self.delivery.lock().unwrap().ring.subscribe();
        let mut cursor = last_seq;
        loop {
            let (messages, next, finished) = {
                let delivery = self.delivery.lock().unwrap();
                let (batch, lag) = delivery.ring.read_after(cursor.unwrap_or(0), 4096);
                if cursor.is_none() || lag.is_some() || cursor.is_some_and(|c| c > delivery.seq) {
                    (
                        vec![self.snapshot_message(&delivery)],
                        delivery.seq,
                        delivery.snapshot.finished.is_some(),
                    )
                } else {
                    let next = batch
                        .last()
                        .map(|(seq, _)| *seq)
                        .unwrap_or(cursor.unwrap_or(0));
                    (
                        batch
                            .into_iter()
                            .map(|(seq, event)| BridgeServerMessage::RunEvent {
                                run_id: self.run_id.clone(),
                                seq,
                                event,
                            })
                            .collect(),
                        next,
                        delivery.snapshot.finished.is_some() && next == delivery.seq,
                    )
                }
            };
            for message in messages {
                if sender.send(WriterCmd::send(message)).await.is_err() {
                    return;
                }
            }
            cursor = Some(next);
            if finished {
                return;
            }
            if watch.changed().await.is_err() {
                return;
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct RunRegistry {
    host_administration: AtomicBool,
    runs: Mutex<HashMap<String, Arc<Run>>>,
}

pub struct StartRun {
    pub run_id: String,
    pub session_id: i64,
    pub content: String,
    pub model: Option<String>,
    pub editor_context: Option<EditorContext>,
    pub browser_preferences: Option<openwebide_core::BrowserPreferences>,
    pub queued_prompt: Option<openwebide_core::QueuedPromptKey>,
    pub host_path: Option<String>,
}
pub(crate) struct RunHost {
    pub execution: Arc<dyn crate::exec::ToolExecution>,
    pub plugins: crate::plugins::transport::PluginExecutionHost,
}

/// A dropped preparation must not leave a conversation permanently reserved.
struct PreparationGuard<B: RunBackend + 'static> {
    run: Arc<Run>,
    backend: Arc<B>,
    armed: bool,
}
impl<B: RunBackend + 'static> Drop for PreparationGuard<B> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.run.cancel.cancel();
        self.run.emit(RunEvent::Cancelled);
        let run = self.run.clone();
        let backend = self.backend.clone();
        tokio::spawn(async move {
            let _ = backend
                .run_lease(run.owner, run.session_id, &run.run_id, true, 0, None)
                .await;
        });
    }
}

impl RunRegistry {
    /// Selected once at the server/paired-companion capability boundary.
    pub(crate) fn configure_host_administration(&self, allowed: bool) {
        self.host_administration.store(allowed, Ordering::SeqCst);
    }
    pub fn get(&self, principal: &Principal, run_id: &str) -> Result<Arc<Run>, String> {
        let runs = self.runs.lock().unwrap();
        runs.get(run_id)
            .filter(|run| matches!(principal, Principal::User { user_id } if *user_id == run.owner))
            .cloned()
            .ok_or_else(|| "run not found".into())
    }
    pub fn list(&self, principal: &Principal, session_id: i64) -> Vec<RunInfo> {
        self.runs
            .lock()
            .unwrap()
            .values()
            .filter(|run| {
                run.session_id == session_id
                    && matches!(principal, Principal::User { user_id } if *user_id == run.owner)
            })
            .map(|run| RunInfo {
                run_id: run.run_id.clone(),
                session_id,
                running: run.running.load(Ordering::SeqCst),
                seq: run.delivery.lock().unwrap().seq,
                started_at: run.started_at,
            })
            .collect()
    }
    pub async fn shutdown(&self) {
        let runs = self
            .runs
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for run in &runs {
            run.cancel.cancel();
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while runs.iter().any(|run| run.running.load(Ordering::SeqCst))
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    pub fn reap(&self) {
        self.runs.lock().unwrap().retain(|_, run| {
            run.finished_at
                .lock()
                .unwrap()
                .is_none_or(|at| at.elapsed() < Duration::from_secs(600))
        });
    }
    pub async fn start<B, P, F>(
        &self,
        principal: &Principal,
        start: StartRun,
        workspace: &Path,
        backend: Arc<B>,
        provider: F,
    ) -> Result<Arc<Run>, (RunRejectCode, String)>
    where
        B: RunBackend + 'static,
        P: LlmProvider + 'static,
        F: FnOnce(&RunPlan) -> P,
    {
        let run = self.reserve(principal, &start)?;
        self.prepare(
            run,
            start,
            workspace,
            backend,
            provider,
            RunHost {
                execution: Arc::new(crate::exec::HostExecution),
                plugins: crate::plugins::transport::PluginExecutionHost::default(),
            },
        )
        .await
    }

    pub(crate) fn reserve(
        &self,
        principal: &Principal,
        start: &StartRun,
    ) -> Result<Arc<Run>, (RunRejectCode, String)> {
        let Principal::User { user_id } = principal else {
            return Err((RunRejectCode::Unauthorized, "unauthorized".into()));
        };
        let run = Arc::new(Run::new(
            start.run_id.clone(),
            *user_id,
            start.session_id,
            4096,
        ));
        run.host_administration.store(
            self.host_administration.load(Ordering::SeqCst),
            Ordering::SeqCst,
        );
        {
            let mut runs = self.runs.lock().unwrap();
            if runs.contains_key(&start.run_id)
                || runs
                    .values()
                    .any(|r| r.session_id == start.session_id && r.running.load(Ordering::SeqCst))
            {
                return Err((
                    RunRejectCode::Busy,
                    "session already has an active run".into(),
                ));
            }
            runs.insert(start.run_id.clone(), run.clone());
        }
        Ok(run)
    }

    pub(crate) async fn prepare<B, P, F>(
        &self,
        run: Arc<Run>,
        start: StartRun,
        workspace: &Path,
        backend: Arc<B>,
        provider: F,
        host: RunHost,
    ) -> Result<Arc<Run>, (RunRejectCode, String)>
    where
        B: RunBackend + 'static,
        P: LlmProvider + 'static,
        F: FnOnce(&RunPlan) -> P,
    {
        let user_id = run.owner;
        if run.cancel.is_cancelled() {
            run.emit(RunEvent::Cancelled);
            return Ok(run);
        }
        let mut preparation = PreparationGuard {
            run: run.clone(),
            backend: backend.clone(),
            armed: true,
        };
        let prepared = async {
            let mut plan = match start.queued_prompt {
                Some(key) => {
                    backend
                        .queued_run_plan(
                            user_id,
                            start.session_id,
                            &start.content,
                            start.model.as_deref(),
                            start.editor_context.as_ref(),
                            key,
                        )
                        .await
                }
                None => {
                    backend
                        .run_plan(
                            user_id,
                            start.session_id,
                            &start.content,
                            start.model.as_deref(),
                            start.editor_context.as_ref(),
                        )
                        .await
                }
            }
            .map_err(|e| (RunRejectCode::PlanFailed, e))?;
            if !run.host_administration.load(Ordering::SeqCst) {
                plan.request
                    .tools
                    .retain(|tool| !openwebide_agent::host_admin::is_host_tool(&tool.name));
                if plan.kind == RunKind::WebChat && plan.request.tools.is_empty() {
                    plan.kind = RunKind::Chat;
                }
            }
            plan.environment.browser_preferences = start.browser_preferences.clone();
            if let Some(path) = &start.host_path {
                openwebide_agent::scheduled::authorize_host_plan(&mut plan, path);
            }
            let dir = match &plan.kind {
                RunKind::Chat | RunKind::WebChat => None,
                RunKind::Agent { project_path } => Some(
                    crate::paths::resolve_in_root(workspace, Some(project_path))
                        .map_err(|e| (RunRejectCode::ProjectUnavailable, e))?,
                ),
            };
            backend
                .run_lease(
                    user_id,
                    start.session_id,
                    &start.run_id,
                    false,
                    i64::try_from(run.started_at.saturating_mul(1000)).unwrap_or(i64::MAX),
                    None,
                )
                .await
                .map_err(|error| (RunRejectCode::Busy, error))?;
            let message = if let Some(key) = start.queued_prompt {
                backend
                    .consume_queued_prompt(user_id, start.session_id, key, &plan.user_content)
                    .await
            } else {
                backend
                    .persist_message(
                        user_id,
                        start.session_id,
                        Role::User,
                        &plan.user_content,
                        None,
                        None,
                    )
                    .await
            }
            .map_err(|e| (RunRejectCode::PlanFailed, e))?;
            plan.request.messages.push(message.clone());
            Ok((plan, dir, message))
        }
        .await;
        let (plan, dir, message) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = backend
                    .run_lease(user_id, start.session_id, &start.run_id, true, 0, None)
                    .await;
                preparation.armed = false;
                self.runs.lock().unwrap().remove(&start.run_id);
                return Err(error);
            }
        };
        preparation.armed = false;
        let provider = provider(&plan);
        run.emit(RunEvent::Message {
            message: message.clone(),
        });
        let body_run = run.clone();
        let final_run = run.clone();
        let control_run = run.clone();
        let control_backend = backend.clone();
        let control = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;
                let (_, permission) = control_run.scheduled_status();
                match control_backend
                    .run_lease(
                        control_run.owner,
                        control_run.session_id,
                        &control_run.run_id,
                        false,
                        i64::try_from(control_run.started_at.saturating_mul(1000))
                            .unwrap_or(i64::MAX),
                        permission.as_ref().map(|step| step.id.as_str()),
                    )
                    .await
                {
                    Ok(control) => {
                        if control.cancelled {
                            control_run.cancel.cancel();
                        }
                        if let (Some(permission), Some(approved)) = (permission, control.approved) {
                            let _ = control_run.gate.decide(&permission.id, approved);
                        }
                    }
                    Err(_) => {
                        control_run.cancel.cancel();
                        break;
                    }
                }
            }
        });
        tokio::spawn(async move {
            run_body(
                body_run,
                provider,
                backend.clone(),
                plan,
                dir,
                message.id,
                host,
            )
            .await;
            control.abort();
            let _ = backend
                .run_lease(
                    final_run.owner,
                    final_run.session_id,
                    &final_run.run_id,
                    true,
                    0,
                    None,
                )
                .await;
        });
        Ok(run)
    }
}

#[tracing::instrument(skip_all, fields(run_id = %run.run_id))]
async fn run_body<B: RunBackend + 'static, P: LlmProvider + 'static>(
    run: Arc<Run>,
    provider: P,
    backend: Arc<B>,
    mut plan: RunPlan,
    dir: Option<PathBuf>,
    anchor_id: i64,
    host: RunHost,
) {
    if plan.environment.timestamp == 0 {
        plan.environment.timestamp = i64::try_from(now()).unwrap_or(i64::MAX);
    }
    match plan.kind {
        RunKind::Chat => {
            let content =
                openwebide_agent::session::chat_context(&mut plan.request, &plan.environment);
            match backend
                .persist_message(
                    run.owner,
                    run.session_id,
                    Role::System,
                    &content,
                    None,
                    None,
                )
                .await
            {
                Ok(message) => run.emit(RunEvent::Message { message }),
                Err(_) => {
                    run.emit(RunEvent::Error {
                        message: "Could not save run context".into(),
                    });
                    return;
                }
            }
            let prepared = openwebide_agent::session::compact_request(
                &provider,
                &super::backend_client::ModelSource {
                    backend: backend.clone(),
                    user: run.owner,
                },
                &mut plan.request,
                &run.cancel,
                SessionPersistence {
                    anchor: anchor_id,
                    run: &run,
                    backend: &*backend,
                },
                run.session_id,
                anchor_id,
            )
            .await;
            for event in prepared.events {
                run.emit(event);
            }
            if prepared.terminal {
                return;
            }
            let mut events = Box::pin(openwebide_agent::session::chat_events(
                SessionPersistence {
                    anchor: anchor_id,
                    run: &run,
                    backend: &*backend,
                },
                provider.chat_stream(&plan.request),
                run.cancel.clone(),
                &plan.request,
            ));
            while let Some(event) = events.next().await {
                run.emit(event);
            }
        }
        RunKind::Agent { .. } | RunKind::WebChat => {
            if let Some(dir) = &dir {
                plan.environment.project_root = Some(dir.to_string_lossy().into_owned());
                plan.environment
                    .mode
                    .get_or_insert(openwebide_core::WorkspaceMode::Remote);
            }
            let factory = BridgeTaskFactory {
                run: run.clone(),
                backend: backend.clone(),
                dir,
                execution: host.execution,
                anchor: anchor_id,
                environment: plan.environment.clone(),
                plugin_skills: Arc::new(plan.plugin_skills.clone()),
                plugin_executables: Arc::new(plan.plugin_executables.clone()),
                plugin_grants: Arc::new(plan.plugin_grants.clone()),
                plugin_transport: host.plugins.transport(run.owner),
                primary: openwebide_core::ModelRuntime {
                    connection: plan.connection.clone(),
                    transport: plan.transport.clone(),
                    settings: plan.request.model_settings.clone(),
                },
            };
            let executor = factory.executor();
            let gate = factory.gate(&plan.request);
            let memo = provider.tool_stream_memo();
            let connection_id = plan.connection.id;
            let tool_stream_revision = plan.connection.tool_stream_revision;
            let memo_model = plan.connection.model.clone();
            let events = openwebide_agent::tasks::host::run_tree(
                factory,
                super::backend_client::ModelSource {
                    backend: backend.clone(),
                    user: run.owner,
                },
                run.cancel.clone(),
                provider,
                executor,
                gate,
                plan.request,
                AgentConfig::default(),
                anchor_id,
            );
            let events = events.then(|event| async {
                if let Some(memo) = &memo {
                    record_tool_stream_memo(
                        &*backend,
                        run.owner,
                        connection_id,
                        tool_stream_revision,
                        memo_model.as_deref(),
                        memo,
                    )
                    .await;
                }
                event
            });
            map_agent_events(&run, &*backend, anchor_id, events).await;
        }
    }
}

pub(crate) async fn record_tool_stream_memo<B: RunBackend>(
    backend: &B,
    user_id: i64,
    connection_id: i64,
    tool_stream_revision: i64,
    model: Option<&str>,
    memo: &openwebide_llm::ToolStreamMemo,
) {
    if memo.take_unrecorded()
        && let Err(error) = backend
            .set_tool_stream_unsupported(user_id, connection_id, tool_stream_revision, model)
            .await
    {
        tracing::warn!(%error, "failed to save streamed-tools memo");
    }
}

struct SessionPersistence<'a, B> {
    anchor: i64,
    run: &'a Run,
    backend: &'a B,
}
impl<B: RunBackend> openwebide_agent::session::RunPersistence for SessionPersistence<'_, B> {
    async fn notify(&self, event: &openwebide_core::push::RunNotification) -> Result<(), String> {
        let result = self
            .backend
            .notify(self.run.owner, self.run.session_id, event)
            .await;
        if result.is_err() {
            eprintln!(
                "Could not queue Web Push for session {}",
                self.run.session_id
            );
        }
        result
    }

    async fn task(
        &self,
        anchor: i64,
        snapshot: &openwebide_core::TaskSnapshot,
    ) -> Result<(), String> {
        self.backend
            .save_task(self.run.owner, self.run.session_id, anchor, snapshot)
            .await
    }
    fn now_ms(&self) -> u64 {
        wall_time_ms()
    }
    async fn timing(&self, id: &str, timing: &openwebide_core::ToolTiming) -> Result<(), String> {
        self.backend
            .save_tool_timing(self.run.owner, self.run.session_id, id, timing)
            .await
    }

    fn now(&self) -> i64 {
        i64::try_from(now()).unwrap_or(i64::MAX)
    }
    async fn message(
        &self,
        role: Role,
        content: &str,
        usage: Option<&TurnTelemetry>,
        calls: Option<&[openwebide_core::ToolCall]>,
    ) -> Result<ChatMessage, String> {
        self.backend
            .persist_message(
                self.run.owner,
                self.run.session_id,
                role,
                content,
                usage,
                calls,
            )
            .await
    }
    async fn step(
        &self,
        anchor: i64,
        id: &str,
        name: &str,
        summary: &str,
        diff: Option<&openwebide_core::FileDiff>,
    ) -> Result<(), String> {
        self.backend
            .upsert_tool_step(
                self.run.owner,
                self.run.session_id,
                anchor,
                id,
                name,
                summary,
                diff,
            )
            .await
    }
    async fn checkpoint(
        &self,
        id: &str,
        checkpoint: &openwebide_core::rewind::ProjectCheckpoint,
    ) -> Result<(), String> {
        self.backend
            .save_project_checkpoint(self.run.owner, self.run.session_id, id, checkpoint)
            .await
    }
    async fn result(
        &self,
        id: &str,
        ok: bool,
        summary: &str,
        diff: Option<&openwebide_core::FileDiff>,
    ) -> Result<(), String> {
        self.backend
            .complete_tool_step(self.run.owner, self.run.session_id, id, ok, summary, diff)
            .await
    }
    fn prepare_permission(&self, id: &str) {
        self.run.gate.prepare(id);
    }
    async fn finish(&self) {
        let _ = self
            .backend
            .question_command(
                self.run.owner,
                self.run.session_id,
                &openwebide_core::questions::QuestionCommand::CancelRun {
                    anchor: self.anchor,
                },
            )
            .await;
        self.run.gate.clear();
    }
}
async fn map_agent_events<B: RunBackend>(
    run: &Run,
    backend: &B,
    anchor: i64,
    events: impl Stream<Item = AgentEvent> + Send,
) {
    let mut events = Box::pin(openwebide_agent::session::events(
        SessionPersistence {
            run,
            backend,
            anchor,
        },
        run.session_id,
        anchor,
        events,
    ));
    while let Some(event) = events.next().await {
        run.emit(event);
    }
}

struct TodoPersistence<B> {
    backend: Arc<B>,
    user: i64,
    session: i64,
    anchor: i64,
}
impl<B: RunBackend> openwebide_agent::todo::TodoStore for TodoPersistence<B> {
    async fn read(&self) -> Result<Option<openwebide_core::TodoUpdate>, String> {
        self.backend.get_todo_plan(self.user, self.session).await
    }
    async fn write(
        &self,
        plan: &openwebide_core::TodoPlan,
    ) -> Result<openwebide_core::TodoUpdate, String> {
        self.backend
            .write_todo_plan(self.user, self.session, self.anchor, plan)
            .await
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

fn wall_time_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

type BridgeBuiltinTaskExecutor<B> = openwebide_agent::todo::TodoTools<
    openwebide_agent::vfs_executor::SessionToolExecutor<
        VfsToolExecutor<NativeFsVfs, openwebide_agent::NoopWebClient, InProcessBridgeClient>,
        openwebide_agent::NoopWebClient,
        crate::runs::agent_host::HostInfoClient<B>,
    >,
    TodoPersistence<B>,
>;
type BridgeBaseTaskExecutor<B> = openwebide_agent::plugins::execution::PluginTools<
    openwebide_agent::skills::packages::PackageSkillTools<BridgeBuiltinTaskExecutor<B>>,
    crate::plugins::transport::NativePluginTransport,
    openwebide_agent::plugins::execution::GrantedServices<BridgePluginServices<B>>,
>;
struct BridgePluginServices<B> {
    backend: Arc<B>,
    user: i64,
    session: i64,
}
impl<B: RunBackend> openwebide_agent::plugins::execution::GrantedHost for BridgePluginServices<B> {
    async fn request(
        &self,
        request: &openwebide_core::plugins::execution::PluginHostRequest,
    ) -> Result<String, String> {
        self.backend
            .plugin_request(self.user, self.session, request)
            .await
    }
}
type BridgeTaskGate<B> = openwebide_agent::plugins::execution::PluginGate<
    openwebide_agent::policy::PolicyGate<BridgeGate, super::backend_client::ApprovalAdapter<B>>,
>;
struct BridgeTaskFactory<B> {
    plugin_grants: Arc<std::collections::BTreeMap<String, String>>,
    plugin_executables: Arc<Vec<openwebide_core::plugins::PreparedPlugin>>,
    plugin_transport: crate::plugins::transport::NativePluginTransport,
    plugin_skills: Arc<Vec<openwebide_core::ProjectSkill>>,
    run: Arc<Run>,
    backend: Arc<B>,
    dir: Option<PathBuf>,
    execution: Arc<dyn crate::exec::ToolExecution>,
    anchor: i64,
    environment: openwebide_core::RunEnvironment,
    primary: openwebide_core::ModelRuntime,
}
impl<B> Clone for BridgeTaskFactory<B> {
    fn clone(&self) -> Self {
        Self {
            plugin_executables: self.plugin_executables.clone(),
            plugin_grants: self.plugin_grants.clone(),
            plugin_transport: self.plugin_transport.clone(),
            plugin_skills: self.plugin_skills.clone(),
            run: self.run.clone(),
            backend: self.backend.clone(),
            dir: self.dir.clone(),
            execution: self.execution.clone(),
            anchor: self.anchor,
            environment: self.environment.clone(),
            primary: self.primary.clone(),
        }
    }
}
impl<B: RunBackend + 'static> BridgeTaskFactory<B> {
    fn executor(&self) -> BridgeTaskExecutor<B> {
        openwebide_agent::questions::QuestionTools::new(
            self.base_executor(),
            BridgeQuestionPersistence {
                backend: self.backend.clone(),
                user: self.run.owner,
                session: self.run.session_id,
            },
            self.anchor,
        )
    }
    fn base_executor(&self) -> BridgeBaseTaskExecutor<B> {
        openwebide_agent::plugins::execution::PluginTools {
            executor: openwebide_agent::skills::packages::PackageSkillTools::new(
                self.builtin_executor(),
                &self.plugin_skills,
            ),
            transport: self.plugin_transport.clone(),
            services: openwebide_agent::plugins::execution::GrantedServices {
                grants: self.plugin_grants.clone(),
                host: BridgePluginServices {
                    backend: self.backend.clone(),
                    user: self.run.owner,
                    session: self.run.session_id,
                },
            },
            plugins: self.plugin_executables.clone(),
        }
    }
    fn builtin_executor(&self) -> BridgeBuiltinTaskExecutor<B> {
        let workspace = self.dir.as_ref().map(|dir| {
            VfsToolExecutor::with_web_and_bridge(
                NativeFsVfs { root: dir.clone() },
                openwebide_agent::NoopWebClient,
                InProcessBridgeClient {
                    dir: dir.clone(),
                    execution: self.execution.clone(),
                    cancel: self.run.cancel.clone(),
                },
            )
            .with_context(self.environment.clone())
        });
        let executor = openwebide_agent::vfs_executor::SessionToolExecutor::new(
            workspace,
            openwebide_agent::NoopWebClient,
            self.environment.clone(),
        )
        .with_host(crate::runs::agent_host::HostInfoClient {
            host_administration: self.run.host_administration.load(Ordering::SeqCst),
            execution: self.execution.clone(),
            backend: self.backend.clone(),
            user: self.run.owner,
            session: self.run.session_id,
        });
        openwebide_agent::todo::TodoTools::new(
            executor,
            TodoPersistence {
                backend: self.backend.clone(),
                user: self.run.owner,
                session: self.run.session_id,
                anchor: self.anchor,
            },
        )
    }
    fn gate(&self, request: &openwebide_core::ChatRequest) -> BridgeTaskGate<B> {
        openwebide_agent::plugins::execution::PluginGate {
            plugins: self.plugin_executables.clone(),
            gate: openwebide_agent::policy::PolicyGate {
                manual: self.run.gate.clone(),
                source: super::backend_client::ApprovalAdapter {
                    backend: self.backend.clone(),
                    user: self.run.owner,
                    session: self.run.session_id,
                    connection_id: request.connection_id,
                    model: request.model.clone(),
                },
            },
        }
    }
}
impl<B: RunBackend + 'static> openwebide_agent::tasks::host::TaskFactory for BridgeTaskFactory<B> {
    type Provider = openwebide_llm::registry::Provider<super::http_client::ReqwestHttpClient>;
    type Executor = BridgeTaskExecutor<B>;
    type Gate = BridgeTaskGate<B>;
    fn now_ms(&self) -> u64 {
        wall_time_ms()
    }
    async fn prepare(
        &self,
        request: &openwebide_core::ChatRequest,
    ) -> Result<(Self::Provider, Self::Executor, Self::Gate), String> {
        let runtime = if request.connection_id == self.primary.connection.id
            && request.model == self.primary.connection.model
        {
            self.primary.clone()
        } else {
            self.backend
                .model_runtime(
                    self.run.owner,
                    &openwebide_core::ModelSelection {
                        server_id: request.connection_id,
                        model: request.model.clone().ok_or("Child model is missing")?,
                    },
                )
                .await?
        };
        let provider = openwebide_llm::registry::Provider::for_connection(
            &runtime.connection,
            super::http_client::ReqwestHttpClient::default().with_transport(runtime.transport),
        );
        Ok((provider, self.executor(), self.gate(request)))
    }
}

struct BridgeQuestionPersistence<B> {
    backend: Arc<B>,
    user: i64,
    session: i64,
}
impl<B: RunBackend> openwebide_agent::questions::QuestionStore for BridgeQuestionPersistence<B> {
    async fn command(
        &self,
        command: &openwebide_core::questions::QuestionCommand,
    ) -> Result<openwebide_core::questions::QuestionResult, String> {
        self.backend
            .question_command(self.user, self.session, command)
            .await
    }
    async fn wait(&self) {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

type BridgeTaskExecutor<B> = openwebide_agent::questions::QuestionTools<
    BridgeBaseTaskExecutor<B>,
    BridgeQuestionPersistence<B>,
>;
