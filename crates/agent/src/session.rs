//! Shared run planning and persisted event lifecycle. Hosts supply storage and transport primitives.
mod chat;
#[cfg(test)]
mod plugin_context_tests;
pub use chat::{ChatPersistence, chat_events};

use crate::AgentEvent;
use openwebide_core::{
    ChatMessage, ChatRequest, EditorContext, FileDiff, ModelRuntime, Role, RunEvent, ToolCall,
    ToolDefinition, ToolStep, TurnTelemetry,
};
use std::future::Future;

pub fn history(messages: Vec<ChatMessage>, steps: &[ToolStep]) -> Vec<ChatMessage> {
    let latest = messages.iter().rev().find_map(|message| {
        (message.role == Role::System)
            .then(|| openwebide_core::Compaction::parse(&message.content))
            .flatten()
    });
    let mut messages = if let Some(compaction) = latest {
        let session = messages.first().map_or(0, |message| message.session_id);
        let mut history = compaction.messages(session);
        let tail = messages
            .into_iter()
            .filter(|message| {
                message.id > compaction.through_message_id
                    && !(message.role == Role::System
                        && message
                            .content
                            .starts_with(openwebide_core::COMPACTION_PREFIX))
            })
            .collect();
        history.extend(openwebide_core::tool_history(tail, steps));
        history
    } else {
        openwebide_core::tool_history(
            messages
                .into_iter()
                .filter(|message| {
                    !(message.role == Role::System
                        && message
                            .content
                            .starts_with(openwebide_core::COMPACTION_PREFIX))
                })
                .collect(),
            steps,
        )
    };
    for message in &mut messages {
        if message.role == Role::Assistant {
            message.content = openwebide_core::strip_reasoning(&message.content).to_owned();
        }
    }
    messages
}
pub fn user_content(content: String, editor: Option<&EditorContext>) -> String {
    match editor {
        Some(editor) => {
            let mut prompt = openwebide_core::PromptContent::decode(&content);
            prompt.text = format!("{}{}", editor.format_prompt_injection(), prompt.text);
            prompt.encode().unwrap_or(content)
        }
        None => content,
    }
}
pub fn tools_for_host(host_available: bool) -> Vec<ToolDefinition> {
    let mut tools = crate::vfs_tools();
    if !host_available {
        tools.retain(|tool| !crate::policy::BRIDGE_TOOLS.contains(&tool.name.as_str()));
    }
    tools
}

pub fn request(
    runtime: &ModelRuntime,
    system_prompt: Option<String>,
    messages: Vec<ChatMessage>,
    mut tools: Vec<ToolDefinition>,
) -> ChatRequest {
    if !tools.is_empty() && !tools.iter().any(|tool| tool.name == "task") {
        tools.push(crate::tasks::executor::definition());
    }
    let mut request = ChatRequest {
        connection_id: runtime.connection.id,
        model: None,
        system_prompt,
        messages,
        tools,
        model_settings: Default::default(),
    };
    runtime.apply_to(&mut request);
    request
}

/// Add the same chat-only context to the request and return its persisted representation.
pub fn chat_context(
    request: &mut ChatRequest,
    environment: &openwebide_core::RunEnvironment,
) -> String {
    let content = crate::context::chat_context(environment);
    request
        .system_prompt
        .get_or_insert_with(String::new)
        .push_str(&format!("\n\n{content}"));
    format!("{}{content}", openwebide_core::RUN_CONTEXT_PREFIX)
}

/// Inputs supplied by the host; run policy and shaping are shared across every adapter.
pub struct PlanInput {
    pub environment: openwebide_core::RunEnvironment,
    pub system_prompt: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub content: String,
    pub editor: Option<EditorContext>,
}

pub fn plan(runtime: &ModelRuntime, input: PlanInput) -> openwebide_core::RunPlan {
    plan_with_executables(runtime, input, Vec::new())
}

/// Prepare plugin tools before applying connection/model selection. Both host
/// adapters use this entry point; only advertised plugins receive run authority.
pub fn plan_with_plugins(
    runtime: &ModelRuntime,
    mut input: PlanInput,
    bindings: &[openwebide_core::plugins::ProjectPlugin],
) -> Result<openwebide_core::RunPlan, String> {
    let plugins = crate::plugins::execution::configure_tools(&mut input.tools, bindings)?;
    Ok(plan_with_executables(runtime, input, plugins))
}

/// Execute read-only plugin planning hooks before model/tool selection. Hosts
/// supply only authority, transport and callback primitives.
pub async fn plan_with_plugin_context<T, S, A, F>(
    runtime: &ModelRuntime,
    mut input: PlanInput,
    bindings: &[openwebide_core::plugins::ProjectPlugin],
    transport: &T,
    host: S,
    authorize: A,
) -> Result<openwebide_core::RunPlan, String>
where
    T: crate::plugins::execution::PluginTransport,
    S: crate::plugins::execution::GrantedHost,
    A: FnOnce(Vec<openwebide_core::plugins::PreparedPlugin>) -> F,
    F: Future<Output = Result<std::collections::BTreeMap<String, String>, String>>,
{
    use crate::plugins::execution::{GrantedServices, invoke_plugin};
    use openwebide_core::plugins::execution::{ContextContribution, InvokePlugin, PluginOperation};
    let plugins = crate::plugins::execution::configure_tools(&mut input.tools, bindings)?;
    if plugins.is_empty() {
        return Ok(plan_with_executables(runtime, input, plugins));
    }
    let grants = authorize(plugins.clone()).await?;
    if plugins
        .iter()
        .any(|plugin| grants.get(&plugin.digest).is_none_or(String::is_empty))
    {
        return Err("Plugin planning authority is unavailable".into());
    }
    let services = GrantedServices {
        host,
        grants: std::sync::Arc::new(grants.clone()),
    };
    let mut budget = runtime
        .settings
        .context_limit
        .unwrap_or(32768)
        .saturating_mul(3)
        / 10;
    budget = budget.min(8192);
    for plugin in &plugins {
        let outcome = invoke_plugin(
            transport,
            &services,
            InvokePlugin {
                operation: PluginOperation::Context,
                prepared: plugin.clone(),
                name: String::new(),
                arguments: serde_json::json!({"budget_bytes":budget.saturating_sub(2)}).to_string(),
            },
        )
        .await
        .map_err(|error| format!("{} context failed: {error}", plugin.manifest.display_name))?;
        if !outcome.ok {
            return Err(format!(
                "{} context failed: {}",
                plugin.manifest.display_name, outcome.summary
            ));
        }
        let contribution: ContextContribution =
            serde_json::from_str(&outcome.content).map_err(|error| error.to_string())?;
        let mut disabled = std::collections::BTreeSet::new();
        if contribution.disabled_tools.iter().any(|name| {
            !disabled.insert(name)
                || !plugin
                    .manifest
                    .contributions
                    .tools
                    .iter()
                    .any(|tool| tool.name == *name)
        }) {
            return Err("Plugin context can disable only its own tools".into());
        }
        input.tools.retain(|tool| !disabled.contains(&tool.name));
        if let Some(prompt) = contribution.prompt.filter(|prompt| !prompt.is_empty()) {
            if prompt.len().saturating_add(2) > budget {
                return Err("Plugin context exceeds its budget".into());
            }
            budget -= prompt.len() + 2;
            input
                .system_prompt
                .get_or_insert_with(String::new)
                .push_str(&format!("\n\n{prompt}"));
        }
    }
    let mut plan = plan_with_executables(runtime, input, plugins);
    plan.plugin_grants = grants
        .into_iter()
        .filter(|(digest, _)| {
            plan.plugin_executables
                .iter()
                .any(|plugin| plugin.digest == *digest)
        })
        .collect();
    Ok(plan)
}

fn plan_with_executables(
    runtime: &ModelRuntime,
    input: PlanInput,
    mut plugins: Vec<openwebide_core::plugins::PreparedPlugin>,
) -> openwebide_core::RunPlan {
    let projectless =
        input.environment.project_name.is_none() && input.environment.project_root.is_none();
    let tools = if projectless {
        input
            .tools
            .into_iter()
            .filter(|tool| {
                is_projectless_tool(&tool.name)
                    || crate::host_admin::is_host_tool(&tool.name)
                    || crate::scheduled::is_scheduled_tool(&tool.name)
                    || plugins.iter().any(|plugin| {
                        plugin
                            .manifest
                            .contributions
                            .tools
                            .iter()
                            .any(|declared| declared.name == tool.name)
                    })
            })
            .collect()
    } else {
        input.tools
    };
    let request = request(runtime, input.system_prompt, input.messages, tools);
    plugins.retain(|plugin| {
        plugin
            .manifest
            .contributions
            .tools
            .iter()
            .any(|declared| request.tools.iter().any(|tool| tool.name == declared.name))
    });
    let kind = match input.environment.project_root.as_ref() {
        Some(root) if !request.tools.is_empty() => openwebide_core::RunKind::Agent {
            project_path: root.clone(),
        },
        _ if !request.tools.is_empty() => openwebide_core::RunKind::WebChat,
        _ => openwebide_core::RunKind::Chat,
    };
    openwebide_core::RunPlan {
        plugin_executables: plugins,
        plugin_grants: Default::default(),
        plugin_skills: Vec::new(),
        connection: runtime.connection.clone(),
        transport: runtime.transport.clone(),
        request,
        environment: input.environment,
        user_content: user_content(
            input.content,
            input.editor.as_ref().filter(|_| !projectless),
        ),
        kind,
    }
}

pub fn conversation_history(entries: Vec<openwebide_core::ConversationEntry>) -> Vec<ChatMessage> {
    let (mut messages, mut steps) = (Vec::new(), Vec::new());
    for entry in entries {
        match entry {
            openwebide_core::ConversationEntry::Message(message) => messages.push(message),
            openwebide_core::ConversationEntry::ToolStep(step) => steps.push(step),
            openwebide_core::ConversationEntry::Task(_) => (),
        }
    }
    history(messages, &steps)
}

pub trait RunPersistence: Send + Sync {
    fn now(&self) -> i64;
    fn now_ms(&self) -> u64;
    fn timing(
        &self,
        id: &str,
        timing: &openwebide_core::ToolTiming,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn message(
        &self,
        role: Role,
        content: &str,
        usage: Option<&TurnTelemetry>,
        calls: Option<&[ToolCall]>,
    ) -> impl Future<Output = Result<ChatMessage, String>> + Send;
    fn step(
        &self,
        anchor: i64,
        id: &str,
        name: &str,
        summary: &str,
        diff: Option<&FileDiff>,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn result(
        &self,
        id: &str,
        ok: bool,
        summary: &str,
        diff: Option<&FileDiff>,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn checkpoint(
        &self,
        _id: &str,
        _checkpoint: &openwebide_core::rewind::ProjectCheckpoint,
    ) -> impl Future<Output = Result<(), String>> + Send {
        std::future::ready(Ok(()))
    }
    fn task(
        &self,
        _anchor: i64,
        _snapshot: &openwebide_core::TaskSnapshot,
    ) -> impl Future<Output = Result<(), String>> + Send {
        std::future::ready(Err("Child task persistence unavailable".into()))
    }
    fn notify(
        &self,
        _event: &openwebide_core::push::RunNotification,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }
    fn prepare_permission(&self, _id: &str) {}
    fn finish(&self) -> impl Future<Output = ()> + Send {
        async {}
    }
}

pub struct Recorded {
    pub events: Vec<RunEvent>,
    pub terminal: bool,
}
pub struct RunRecorder<P> {
    pub persistence: P,
    anchor: i64,
    reasoning: String,
    usage: Option<TurnTelemetry>,
    timings: std::collections::BTreeMap<String, openwebide_core::ToolTiming>,
    checkpoint_warnings: std::collections::BTreeMap<String, String>,
    tasks: std::collections::BTreeMap<String, openwebide_core::TaskSnapshot>,
}
impl<P: RunPersistence> RunRecorder<P> {
    pub fn new(persistence: P, _session: i64, anchor: i64) -> Self {
        Self {
            persistence,
            anchor,
            reasoning: String::new(),
            usage: None,
            checkpoint_warnings: Default::default(),
            timings: Default::default(),
            tasks: Default::default(),
        }
    }
    pub async fn record(&mut self, event: AgentEvent) -> Recorded {
        let mut recorded = self.record_inner(event).await;
        if recorded.terminal {
            let mut timing_events = self.finish_tasks().await;
            timing_events.extend(self.finish_timings().await);
            let terminal = recorded.events.pop();
            recorded.events.append(&mut timing_events);
            recorded.events.extend(terminal);
        }
        for event in &recorded.events {
            if let Some(notification) = openwebide_core::push::RunNotification::from_event(event) {
                // Notifications are optional side effects; delivery failures never replace a run outcome.
                let _ = self.persistence.notify(&notification).await;
            }
        }
        recorded
    }
    async fn record_task(
        &mut self,
        mut update: openwebide_core::TaskUpdate,
    ) -> Result<Vec<RunEvent>, String> {
        use openwebide_core::TaskEvent;
        // Nested contexts keep their own history; only their tool effects use the
        // parent's owned persistence boundary. Never publish raw checkpoints.
        fn leaf(update: &mut openwebide_core::TaskUpdate) -> &mut openwebide_core::TaskUpdate {
            if matches!(update.event, TaskEvent::Nested { .. }) {
                let TaskEvent::Nested { update } = &mut update.event else {
                    unreachable!()
                };
                leaf(update)
            } else {
                update
            }
        }
        let root_id = update.task.id.clone();
        let leaf = leaf(&mut update);
        let mut visible = true;
        let mut durable = true;
        match &mut leaf.event {
            TaskEvent::Run { event } => match &mut **event {
                RunEvent::ToolCall { id, name, summary } => {
                    self.persistence
                        .step(self.anchor, id, name, summary, None)
                        .await?;
                }
                RunEvent::PermissionRequest {
                    id,
                    name,
                    summary,
                    diff,
                    ..
                } => {
                    self.persistence.prepare_permission(id);
                    self.persistence
                        .step(self.anchor, id, name, summary, diff.as_ref())
                        .await?;
                }
                RunEvent::ToolResult {
                    id,
                    ok,
                    summary,
                    diff,
                    ..
                } => {
                    if let Some(warning) = self.checkpoint_warnings.remove(id) {
                        *summary = format!("{summary}\n{warning}");
                    }
                    self.persistence
                        .result(id, *ok, summary, diff.as_ref())
                        .await?;
                }
                RunEvent::ToolTiming { id, timing } => {
                    // Optional timing metadata must never interrupt a mutation.
                    let _ = self.persistence.timing(id, timing).await;
                }
                RunEvent::Done { .. } | RunEvent::Cancelled | RunEvent::Error { .. } => {
                    let pending = self
                        .tasks
                        .get(&root_id)
                        .and_then(|root| root.find_task(&leaf.task.id))
                        .map_or_else(Vec::new, |task| {
                            task.run
                                .items
                                .iter()
                                .filter_map(|item| match item {
                                    openwebide_core::RunItem::Step(step)
                                        if step.result.is_none() =>
                                    {
                                        Some((step.id.clone(), step.timing))
                                    }
                                    _ => None,
                                })
                                .collect()
                        });
                    for (id, timing) in pending {
                        self.persistence
                            .result(
                                &id,
                                false,
                                "Child stopped before a result was recorded",
                                None,
                            )
                            .await?;
                        if let Some(timing) = timing {
                            let _ = self
                                .persistence
                                .timing(&id, &timing.sample(self.persistence.now_ms(), true))
                                .await;
                        }
                    }
                }
                RunEvent::Delta { .. }
                | RunEvent::ReasoningDelta { .. }
                | RunEvent::Telemetry { .. } => durable = false,
                _ => (),
            },
            TaskEvent::FileCheckpoint { id, diff } => {
                self.persistence
                    .step(
                        self.anchor,
                        id,
                        "write_file",
                        &format!("write {}", diff.path),
                        Some(diff),
                    )
                    .await?;
                visible = false;
            }
            TaskEvent::ProjectCheckpoint { id, checkpoint } => {
                if let Some(warning) =
                    openwebide_core::rewind::coverage_warning(&checkpoint.skipped)
                {
                    self.checkpoint_warnings.insert(id.clone(), warning);
                }
                self.persistence.checkpoint(id, checkpoint).await?;
                visible = false;
            }
            TaskEvent::Nested { .. } => unreachable!("Nested events flattened"),
        }
        let snapshot = self
            .tasks
            .entry(update.task.id.clone())
            .or_insert_with(|| openwebide_core::TaskSnapshot::new(update.task.clone()));
        snapshot.apply(&update);
        if durable {
            self.persistence.task(self.anchor, snapshot).await?;
        }
        Ok(if visible {
            vec![RunEvent::Task {
                update: Box::new(update),
            }]
        } else {
            vec![]
        })
    }
    async fn finish_tasks(&mut self) -> Vec<RunEvent> {
        let now = self.persistence.now_ms();
        let mut events = Vec::new();
        for task in self
            .tasks
            .values_mut()
            .filter(|task| task.run.finished.is_none())
        {
            let mut pending = vec![&*task];
            let mut steps = Vec::new();
            while let Some(child) = pending.pop() {
                for item in &child.run.items {
                    if let openwebide_core::RunItem::Step(step) = item
                        && step.result.is_none()
                    {
                        steps.push((step.id.clone(), step.timing));
                    }
                }
                pending.extend(&child.children);
            }
            for (id, timing) in steps {
                let _ = self
                    .persistence
                    .result(&id, false, "Interrupted before a result was recorded", None)
                    .await;
                if let Some(timing) = timing {
                    let _ = self
                        .persistence
                        .timing(&id, &timing.sample(now, true))
                        .await;
                }
            }
            task.stop(now);
            if let Err(error) = self.persistence.task(self.anchor, task).await {
                events.push(RunEvent::Error {
                    message: format!("Could not persist stopped child task: {error}"),
                });
            }
            events.push(RunEvent::Task {
                update: Box::new(openwebide_core::TaskUpdate {
                    task: task.task.clone(),
                    event: openwebide_core::TaskEvent::Run {
                        event: Box::new(RunEvent::Cancelled),
                    },
                }),
            });
        }
        events
    }
    async fn finish_timings(&mut self) -> Vec<RunEvent> {
        let mut events = Vec::new();
        for (id, timing) in std::mem::take(&mut self.timings) {
            let timing = timing.sample(self.persistence.now_ms(), true);
            // Timing is optional metadata; a failed save must not interrupt a
            // command or replace its terminal outcome. Live clients still get
            // the measured final duration even when history cannot retain it.
            let _ = self.persistence.timing(&id, &timing).await;
            events.push(RunEvent::ToolTiming { id, timing });
        }
        events
    }
    async fn record_inner(&mut self, event: AgentEvent) -> Recorded {
        let mut terminal = false;
        let mut preceding = Vec::new();
        let event = match event {
            AgentEvent::TaskUpdate(update) => {
                return match self.record_task(*update).await {
                    Ok(events) => Recorded {
                        events,
                        terminal: false,
                    },
                    Err(error) => Recorded {
                        events: vec![RunEvent::Error {
                            message: format!("Could not persist child task: {error}"),
                        }],
                        terminal: true,
                    },
                };
            }
            AgentEvent::Context(content) => {
                let content = format!("{}{content}", openwebide_core::RUN_CONTEXT_PREFIX);
                match self
                    .persistence
                    .message(Role::System, &content, None, None)
                    .await
                {
                    Ok(message) => RunEvent::Message { message },
                    Err(error) => {
                        terminal = true;
                        RunEvent::Error {
                            message: format!("Could not save run context: {error}"),
                        }
                    }
                }
            }
            AgentEvent::Compacted(mut compaction) => {
                compaction.through_message_id = self.anchor;
                let saved = match compaction.stored_content() {
                    Ok(content) => {
                        self.persistence
                            .message(Role::System, &content, None, None)
                            .await
                    }
                    Err(error) => Err(error.to_string()),
                };
                match saved {
                    Ok(message) => RunEvent::Message { message },
                    Err(error) => {
                        terminal = true;
                        RunEvent::Error {
                            message: format!("Could not save conversation summary: {error}"),
                        }
                    }
                }
            }
            AgentEvent::ReasoningDelta(content) => {
                self.reasoning.push_str(&content);
                RunEvent::ReasoningDelta { content }
            }
            AgentEvent::TextDelta(content) => RunEvent::Delta { content },
            AgentEvent::Telemetry(usage) => {
                self.usage = Some(usage);
                RunEvent::Telemetry { usage }
            }
            AgentEvent::TurnCalls { text, calls } => {
                let text =
                    openwebide_core::with_reasoning(&std::mem::take(&mut self.reasoning), &text);
                let usage = self.usage.take();
                let message = match self
                    .persistence
                    .message(Role::Assistant, &text, usage.as_ref(), Some(&calls))
                    .await
                {
                    Ok(message) => {
                        self.anchor = message.id;
                        message
                    }
                    Err(error) => {
                        return Recorded {
                            events: vec![RunEvent::Error {
                                message: format!("Could not save tool turn: {error}"),
                            }],
                            terminal: true,
                        };
                    }
                };
                RunEvent::Interim { message }
            }
            AgentEvent::ToolCall { id, name, summary } => {
                self.usage = None;
                if let Err(error) = self
                    .persistence
                    .step(self.anchor, &id, &name, &summary, None)
                    .await
                {
                    return Recorded {
                        events: vec![RunEvent::Error {
                            message: format!("Could not checkpoint tool call: {error}"),
                        }],
                        terminal: true,
                    };
                }
                let timing = *self.timings.entry(id.clone()).or_insert_with(|| {
                    openwebide_core::ToolTiming::start(self.persistence.now_ms())
                });
                preceding.push(RunEvent::ToolCall {
                    id: id.clone(),
                    name,
                    summary,
                });
                let _ = self.persistence.timing(&id, &timing).await;
                RunEvent::ToolTiming { id, timing }
            }
            AgentEvent::PermissionRequest {
                id,
                name,
                summary,
                diff,
                note,
            } => {
                self.usage = None;
                self.persistence.prepare_permission(&id);
                if let Err(error) = self
                    .persistence
                    .step(self.anchor, &id, &name, &summary, diff.as_ref())
                    .await
                {
                    return Recorded {
                        events: vec![RunEvent::Error {
                            message: format!("Could not checkpoint tool call: {error}"),
                        }],
                        terminal: true,
                    };
                }
                RunEvent::PermissionRequest {
                    id,
                    name,
                    summary,
                    diff,
                    note,
                }
            }
            AgentEvent::ProjectCheckpoint { id, checkpoint } => {
                if let Some(warning) =
                    openwebide_core::rewind::coverage_warning(&checkpoint.skipped)
                {
                    self.checkpoint_warnings.insert(id.clone(), warning);
                }
                return match self.persistence.checkpoint(&id, &checkpoint).await {
                    Ok(()) => Recorded {
                        events: vec![],
                        terminal: false,
                    },
                    Err(error) => {
                        // Clear the unfinished tool marker when storage recovers;
                        // an incomplete durable snapshot still refuses rewind.
                        let _ = self
                            .persistence
                            .result(&id, false, "checkpoint persistence failed", None)
                            .await;
                        Recorded {
                            events: vec![RunEvent::Error {
                                message: format!("Could not persist project checkpoint: {error}"),
                            }],
                            terminal: true,
                        }
                    }
                };
            }
            AgentEvent::FileCheckpoint { id, diff } => {
                let saved = self
                    .persistence
                    .step(
                        self.anchor,
                        &id,
                        "write_file",
                        &format!("write {}", diff.path),
                        Some(&diff),
                    )
                    .await;
                return match saved {
                    Ok(()) => Recorded {
                        events: vec![],
                        terminal: false,
                    },
                    Err(error) => Recorded {
                        events: vec![RunEvent::Error {
                            message: format!("Could not save file checkpoint: {error}"),
                        }],
                        terminal: true,
                    },
                };
            }
            AgentEvent::ToolResult {
                id,
                name,
                ok,
                summary,
                diff,
            } => {
                let summary = match self.checkpoint_warnings.remove(&id) {
                    Some(warning) => format!("{summary}\n{warning}"),
                    None => summary,
                };
                let saved = self
                    .persistence
                    .result(&id, ok, &summary, diff.as_ref())
                    .await;
                let result = RunEvent::ToolResult {
                    id: id.clone(),
                    name,
                    ok,
                    summary,
                    diff,
                };
                match saved {
                    Ok(()) => {
                        if let Some(timing) = self.timings.remove(&id) {
                            let timing = timing.sample(self.persistence.now_ms(), true);
                            preceding.push(result);
                            let _ = self.persistence.timing(&id, &timing).await;
                            RunEvent::ToolTiming { id, timing }
                        } else {
                            result
                        }
                    }
                    Err(error) => {
                        preceding.push(result);
                        terminal = true;
                        RunEvent::Error {
                            message: format!("failed to save tool result: {error}"),
                        }
                    }
                }
            }
            AgentEvent::FinalText(text) => {
                terminal = true;
                let text = openwebide_core::with_reasoning(&self.reasoning, &text);
                match self
                    .persistence
                    .message(Role::Assistant, &text, self.usage.take().as_ref(), None)
                    .await
                {
                    Ok(message) => RunEvent::Done { message },
                    Err(error) => RunEvent::Error {
                        message: format!("failed to save reply: {error}"),
                    },
                }
            }
            AgentEvent::Cancelled => {
                terminal = true;
                RunEvent::Cancelled
            }
            AgentEvent::Error(message) => {
                terminal = true;
                RunEvent::Error { message }
            }
        };
        preceding.push(event);
        Recorded {
            events: preceding,
            terminal,
        }
    }
}

/// Compact and durably record the replacement before any completion stream starts.
#[allow(
    clippy::too_many_arguments,
    reason = "The shared preparation uses model, cancellation and persistence primitives"
)]
pub async fn compact_request<P, S, C, D>(
    provider: &P,
    source: &S,
    request: &mut ChatRequest,
    cancel: &C,
    persistence: D,
    session: i64,
    anchor: i64,
) -> Recorded
where
    P: openwebide_llm::LlmProvider,
    S: crate::compaction::CompactionSource,
    C: crate::CancelCheck + Sync,
    D: RunPersistence,
{
    if cancel.check().await {
        return Recorded {
            events: vec![RunEvent::Cancelled],
            terminal: true,
        };
    }
    let result = {
        let prepare = Box::pin(crate::compaction::prepare(provider, source, request));
        let cancelled = Box::pin(async { cancel.cancelled().await });
        match futures::future::select(prepare, cancelled).await {
            futures::future::Either::Left((result, _)) => result,
            futures::future::Either::Right(_) => {
                return Recorded {
                    events: vec![RunEvent::Cancelled],
                    terminal: true,
                };
            }
        }
    };
    if cancel.check().await {
        return Recorded {
            events: vec![RunEvent::Cancelled],
            terminal: true,
        };
    }
    match result {
        Ok(Some(compaction)) => {
            RunRecorder::new(persistence, session, anchor)
                .record(AgentEvent::Compacted(compaction))
                .await
        }
        Ok(None) => Recorded {
            events: vec![],
            terminal: false,
        },
        Err(message) => Recorded {
            events: vec![RunEvent::Error { message }],
            terminal: true,
        },
    }
}

/// Shared streaming driver. Stops/drops the agent before publishing a terminal event.
pub fn events<'a, P: RunPersistence + 'a>(
    persistence: P,
    session: i64,
    anchor: i64,
    input: impl futures::Stream<Item = AgentEvent> + Send + 'a,
) -> impl futures::Stream<Item = RunEvent> + Send + 'a {
    use futures::StreamExt;
    let input: std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send + 'a>> =
        Box::pin(input);
    futures::stream::unfold(
        (
            RunRecorder::new(persistence, session, anchor),
            Some(input),
            std::collections::VecDeque::new(),
        ),
        |(mut recorder, mut input, mut pending)| async move {
            loop {
                if let Some(event) = pending.pop_front() {
                    return Some((event, (recorder, input, pending)));
                }
                let next = match input.as_mut() {
                    Some(input) => input.next().await,
                    None => return None,
                };
                let Some(next) = next else {
                    input.take();
                    pending.extend(recorder.finish_timings().await);
                    recorder.persistence.finish().await;
                    continue;
                };
                let recorded = recorder.record(next).await;
                if recorded.terminal {
                    input.take();
                    recorder.persistence.finish().await;
                }
                pending.extend(recorded.events);
            }
        },
    )
}

/// Read-only host information and web tools available without opening a workspace.
pub fn projectless_tools() -> Vec<ToolDefinition> {
    let mut tools = crate::vfs_tools()
        .into_iter()
        .filter(|tool| is_projectless_tool(&tool.name))
        .collect();
    crate::scheduled::configure(&mut tools);
    tools
}

pub fn is_projectless_tool(name: &str) -> bool {
    matches!(
        name,
        "search_web"
            | "fetch_web_page"
            | "host_info"
            | "todo_write"
            | "ask_user_question"
            | "plugin_skill_list"
            | "plugin_skill_read"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct Persistence(Arc<AtomicUsize>);
    impl RunPersistence for Persistence {
        fn now(&self) -> i64 {
            0
        }
        fn now_ms(&self) -> u64 {
            0
        }
        async fn timing(
            &self,
            _id: &str,
            _timing: &openwebide_core::ToolTiming,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn message(
            &self,
            _role: Role,
            _content: &str,
            _usage: Option<&TurnTelemetry>,
            _calls: Option<&[ToolCall]>,
        ) -> Result<ChatMessage, String> {
            Err("offline".into())
        }
        async fn step(
            &self,
            _anchor: i64,
            _id: &str,
            _name: &str,
            _summary: &str,
            _diff: Option<&FileDiff>,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn result(
            &self,
            _id: &str,
            _ok: bool,
            _summary: &str,
            _diff: Option<&FileDiff>,
        ) -> Result<(), String> {
            Err("offline".into())
        }
        async fn finish(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[derive(Default)]
    struct TimedPersistence {
        clock: std::sync::atomic::AtomicU64,
        saved: std::sync::Mutex<Vec<(String, openwebide_core::ToolTiming)>>,
        fail: AtomicBool,
        notifications: std::sync::Mutex<Vec<openwebide_core::push::RunNotification>>,
    }
    impl RunPersistence for TimedPersistence {
        async fn notify(
            &self,
            event: &openwebide_core::push::RunNotification,
        ) -> Result<(), String> {
            self.notifications.lock().unwrap().push(event.clone());
            Err("push unavailable".into())
        }
        fn now(&self) -> i64 {
            0
        }
        fn now_ms(&self) -> u64 {
            self.clock.load(Ordering::Relaxed)
        }
        async fn timing(
            &self,
            id: &str,
            timing: &openwebide_core::ToolTiming,
        ) -> Result<(), String> {
            if self.fail.load(Ordering::Relaxed) {
                return Err("offline".into());
            }
            self.saved.lock().unwrap().push((id.into(), *timing));
            Ok(())
        }
        async fn message(
            &self,
            _: Role,
            _: &str,
            _: Option<&TurnTelemetry>,
            _: Option<&[ToolCall]>,
        ) -> Result<ChatMessage, String> {
            Err("unused".into())
        }
        async fn step(
            &self,
            _: i64,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&FileDiff>,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn result(
            &self,
            _: &str,
            _: bool,
            _: &str,
            _: Option<&FileDiff>,
        ) -> Result<(), String> {
            Ok(())
        }
    }
    fn tool_call(id: &str) -> AgentEvent {
        AgentEvent::ToolCall {
            id: id.into(),
            name: "host_info".into(),
            summary: "host".into(),
        }
    }
    #[test]
    fn recorder_times_execution_after_approval_and_persists_before_publishing() {
        futures::executor::block_on(async {
            let mut recorder = RunRecorder::new(TimedPersistence::default(), 1, 1);
            recorder.persistence.clock.store(1000, Ordering::Relaxed);
            let permission = recorder
                .record(AgentEvent::PermissionRequest {
                    id: "t".into(),
                    name: "host_info".into(),
                    summary: "host".into(),
                    diff: None,
                    note: None,
                })
                .await;
            assert_eq!(permission.events.len(), 1);
            assert_eq!(
                *recorder.persistence.notifications.lock().unwrap(),
                vec![openwebide_core::push::RunNotification::Approval {
                    tool_call_id: "t".into()
                }]
            );
            assert!(recorder.persistence.saved.lock().unwrap().is_empty());
            recorder.persistence.clock.store(5000, Ordering::Relaxed);
            let started = recorder.record(tool_call("t")).await;
            assert!(
                matches!(started.events.as_slice(), [RunEvent::ToolCall { .. }, RunEvent::ToolTiming { timing, .. }] if timing.elapsed_ms == 0 && !timing.finished)
            );
            assert_eq!(recorder.persistence.saved.lock().unwrap().len(), 1);
            recorder.persistence.clock.store(6500, Ordering::Relaxed);
            let result = recorder
                .record(AgentEvent::ToolResult {
                    id: "t".into(),
                    name: "host_info".into(),
                    ok: false,
                    summary: "failed".into(),
                    diff: None,
                })
                .await;
            assert!(
                matches!(result.events.as_slice(), [RunEvent::ToolResult { ok:false, .. }, RunEvent::ToolTiming { timing, .. }] if timing.elapsed_ms == 1500 && timing.finished)
            );
            assert_eq!(
                recorder.persistence.saved.lock().unwrap()[1].1.elapsed_ms,
                1500
            );
            recorder.record(tool_call("cancelled")).await;
            recorder.persistence.clock.store(7000, Ordering::Relaxed);
            let stopped = recorder.record(AgentEvent::Cancelled).await;
            assert!(
                matches!(stopped.events.as_slice(), [RunEvent::ToolTiming { timing, .. }, RunEvent::Cancelled] if timing.finished && timing.elapsed_ms == 500)
            );
            assert!(stopped.terminal);
            let failed = recorder.record(AgentEvent::FinalText("done".into())).await;
            assert!(matches!(failed.events.last(), Some(RunEvent::Error { .. })));
            assert_eq!(recorder.persistence.notifications.lock().unwrap().len(), 1);
        });
    }
    #[test]
    fn timing_write_failure_preserves_execution_and_denied_calls_have_no_execution_time() {
        futures::executor::block_on(async {
            let mut recorder = RunRecorder::new(TimedPersistence::default(), 1, 1);
            let denied = recorder
                .record(AgentEvent::ToolResult {
                    id: "denied".into(),
                    name: "host_info".into(),
                    ok: false,
                    summary: "denied".into(),
                    diff: None,
                })
                .await;
            assert!(matches!(
                denied.events.as_slice(),
                [RunEvent::ToolResult { .. }]
            ));
            assert!(recorder.persistence.saved.lock().unwrap().is_empty());
            recorder.persistence.fail.store(true, Ordering::Relaxed);
            let started = recorder.record(tool_call("t")).await;
            assert!(!started.terminal);
            assert!(
                matches!(started.events.as_slice(), [RunEvent::ToolCall { .. }, RunEvent::ToolTiming { timing, .. }] if !timing.finished)
            );
            recorder.persistence.clock.store(1500, Ordering::Relaxed);
            let completed = recorder
                .record(AgentEvent::ToolResult {
                    id: "t".into(),
                    name: "host_info".into(),
                    ok: true,
                    summary: "host".into(),
                    diff: None,
                })
                .await;
            assert!(!completed.terminal);
            assert!(
                matches!(completed.events.as_slice(), [RunEvent::ToolResult { ok: true, .. }, RunEvent::ToolTiming { timing, .. }] if timing.finished && timing.elapsed_ms == 1500)
            );
            assert!(recorder.timings.is_empty());
            recorder.record(tool_call("cancelled")).await;
            recorder.persistence.clock.store(2000, Ordering::Relaxed);
            let cancelled = recorder.record(AgentEvent::Cancelled).await;
            assert!(cancelled.terminal);
            assert!(
                matches!(cancelled.events.as_slice(), [RunEvent::ToolTiming { timing, .. }, RunEvent::Cancelled] if timing.finished && timing.elapsed_ms == 500)
            );
            assert!(recorder.persistence.saved.lock().unwrap().is_empty());
        });
    }
    struct DropGuard(Arc<AtomicBool>);
    impl Drop for DropGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn summary_history_retains_original_task_and_replays_only_new_messages() {
        let message = |id, role, content: &str| ChatMessage {
            id,
            session_id: 1,
            role,
            content: content.into(),
            created_at: 0,
            tool_calls: None,
            tool_call_id: None,
            usage: None,
        };
        let old = message(1, Role::User, "old task");
        let current = message(3, Role::User, "current exact task");
        let compaction = openwebide_core::Compaction {
            summary: "Old work completed".into(),
            retained: vec![current.clone()],
            through_message_id: 3,
        };
        let stored = message(4, Role::System, &compaction.stored_content().unwrap());
        let originals = vec![
            old.clone(),
            message(2, Role::Assistant, "old answer"),
            current.clone(),
            stored,
            message(5, Role::Assistant, "continued answer"),
            message(6, Role::User, "next task"),
        ];
        let history = history(originals.clone(), &[]);
        assert_eq!(originals[0], old);
        assert_eq!(history.len(), 4);
        assert!(history[0].content.contains("Old work completed"));
        assert_eq!(history[1], current);
        assert_eq!(history[2].id, 5);
        assert_eq!(history[3].id, 6);
        let broken = vec![
            old.clone(),
            message(2, Role::System, "[conversation compaction]\ninvalid"),
        ];
        assert_eq!(super::history(broken, &[]), vec![old]);
    }

    #[test]
    fn failed_summary_save_stops_before_the_next_model_request() {
        futures::executor::block_on(async {
            let finished = Arc::new(AtomicUsize::new(0));
            let compaction = openwebide_core::Compaction {
                summary: "summary".into(),
                retained: vec![],
                through_message_id: 0,
            };
            let output: Vec<_> = events(
                Persistence(finished.clone()),
                1,
                3,
                futures::stream::iter([
                    AgentEvent::Compacted(compaction),
                    AgentEvent::TextDelta("must not continue".into()),
                ]),
            )
            .collect()
            .await;
            assert!(
                matches!(output.as_slice(), [RunEvent::Error { message }] if message.contains("save conversation summary"))
            );
            assert_eq!(finished.load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn fatal_save_failure_stops_source_and_preserves_actual_tool_result() {
        futures::executor::block_on(async {
            let finished = Arc::new(AtomicUsize::new(0));
            let dropped = Arc::new(AtomicBool::new(false));
            let polled = Arc::new(AtomicUsize::new(0));
            let observed = polled.clone();
            let guard = DropGuard(dropped.clone());
            let mut source = [
                AgentEvent::ToolResult {
                    id: "write".into(),
                    name: "write_file".into(),
                    ok: true,
                    summary: "written".into(),
                    diff: None,
                },
                AgentEvent::TextDelta("must not continue".into()),
            ]
            .into_iter();
            let input = futures::stream::poll_fn(move |_| {
                let _ = &guard;
                observed.fetch_add(1, Ordering::Relaxed);
                std::task::Poll::Ready(source.next())
            });
            let mut output = Box::pin(events(Persistence(finished.clone()), 1, 2, input));
            assert!(matches!(
                output.next().await,
                Some(RunEvent::ToolResult { ok: true, .. })
            ));
            assert!(dropped.load(Ordering::Relaxed));
            assert_eq!(finished.load(Ordering::Relaxed), 1);
            assert!(matches!(output.next().await, Some(RunEvent::Error { .. })));
            assert!(output.next().await.is_none());
            assert_eq!(polled.load(Ordering::Relaxed), 1);
        });
    }
    #[test]
    fn nested_task_checkpoints_are_acknowledged_privately_and_cancelled_approvals_are_finalized() {
        #[derive(Clone, Default)]
        struct TaskPersistence {
            logs: Arc<std::sync::Mutex<Vec<String>>>,
            snapshots: Arc<std::sync::Mutex<Vec<openwebide_core::TaskSnapshot>>>,
        }
        impl RunPersistence for TaskPersistence {
            fn now(&self) -> i64 {
                1
            }
            fn now_ms(&self) -> u64 {
                2000
            }
            async fn timing(
                &self,
                id: &str,
                _: &openwebide_core::ToolTiming,
            ) -> Result<(), String> {
                self.logs.lock().unwrap().push(format!("timing:{id}"));
                Ok(())
            }
            async fn message(
                &self,
                _: Role,
                _: &str,
                _: Option<&TurnTelemetry>,
                _: Option<&[ToolCall]>,
            ) -> Result<ChatMessage, String> {
                Err("Root messages not needed".into())
            }
            async fn step(
                &self,
                _: i64,
                id: &str,
                _: &str,
                _: &str,
                _: Option<&FileDiff>,
            ) -> Result<(), String> {
                self.logs.lock().unwrap().push(format!("step:{id}"));
                Ok(())
            }
            async fn result(
                &self,
                id: &str,
                ok: bool,
                _: &str,
                _: Option<&FileDiff>,
            ) -> Result<(), String> {
                self.logs.lock().unwrap().push(format!("result:{id}:{ok}"));
                Ok(())
            }
            async fn checkpoint(
                &self,
                id: &str,
                _: &openwebide_core::rewind::ProjectCheckpoint,
            ) -> Result<(), String> {
                self.logs.lock().unwrap().push(format!("checkpoint:{id}"));
                Ok(())
            }
            async fn task(
                &self,
                _: i64,
                snapshot: &openwebide_core::TaskSnapshot,
            ) -> Result<(), String> {
                self.snapshots.lock().unwrap().push(snapshot.clone());
                Ok(())
            }
            fn prepare_permission(&self, id: &str) {
                self.logs.lock().unwrap().push(format!("permission:{id}"));
            }
        }
        futures::executor::block_on(async {
            let persistence = TaskPersistence::default();
            let mut recorder = RunRecorder::new(persistence.clone(), 1, 1);
            let task = openwebide_core::NewAgentTask {
                description: "Inspect".into(),
                prompt: "Inspect".into(),
            };
            let mut parent =
                crate::tasks::TaskRun::new("a1t1c0.task1".into(), "a1t1c0".into(), &task, 1000);
            let start = recorder
                .record(AgentEvent::TaskUpdate(Box::new(
                    parent.started(&task, 1000),
                )))
                .await;
            assert!(!start.terminal && matches!(start.events.as_slice(), [RunEvent::Task { .. }]));
            let nested_parent = "a1t1c0.task1.a1t1c0";
            for update in parent.observe(
                AgentEvent::ToolCall {
                    id: nested_parent.into(),
                    name: "task".into(),
                    summary: "Delegate".into(),
                },
                1000,
            ) {
                recorder
                    .record(AgentEvent::TaskUpdate(Box::new(update)))
                    .await;
            }
            let mut child = crate::tasks::TaskRun::new(
                format!("{nested_parent}.task1"),
                nested_parent.into(),
                &task,
                1000,
            );
            for update in parent.observe(
                AgentEvent::TaskUpdate(Box::new(child.started(&task, 1000))),
                1000,
            ) {
                recorder
                    .record(AgentEvent::TaskUpdate(Box::new(update)))
                    .await;
            }
            let id = format!("{nested_parent}.task1.a1t1c0");
            let events = [
                AgentEvent::PermissionRequest {
                    id: id.clone(),
                    name: "write_file".into(),
                    summary: "write".into(),
                    diff: None,
                    note: None,
                },
                AgentEvent::ProjectCheckpoint {
                    id: id.clone(),
                    checkpoint: openwebide_core::rewind::ProjectCheckpoint {
                        before: Default::default(),
                        after: None,
                        skipped: Default::default(),
                    },
                },
                AgentEvent::Cancelled,
            ];
            for event in events {
                let checkpoint = matches!(event, AgentEvent::ProjectCheckpoint { .. });
                for update in child.observe(event, 1500) {
                    for update in parent.observe(AgentEvent::TaskUpdate(Box::new(update)), 1500) {
                        let recorded = recorder
                            .record(AgentEvent::TaskUpdate(Box::new(update)))
                            .await;
                        assert!(!recorded.terminal);
                        if checkpoint {
                            assert!(recorded.events.is_empty());
                        }
                    }
                }
            }
            {
                let logs = persistence.logs.lock().unwrap();
                assert!(logs.contains(&format!("permission:{id}")));
                assert!(logs.contains(&format!("checkpoint:{id}")));
                assert!(logs.contains(&format!("result:{id}:false")));
            }
            let final_event = recorder.record(AgentEvent::Cancelled).await;
            assert!(final_event.terminal);
            let snapshots = persistence.snapshots.lock().unwrap();
            let snapshot = snapshots.last().unwrap();
            assert_eq!(snapshot.task.status, openwebide_core::TaskStatus::Cancelled);
            assert_eq!(
                snapshot.children[0].task.status,
                openwebide_core::TaskStatus::Cancelled
            );
            assert!(snapshot.pending_permission().is_none());
            assert!(
                persistence
                    .logs
                    .lock()
                    .unwrap()
                    .contains(&format!("result:{nested_parent}:false"))
            );
        });
    }
}

#[cfg(test)]
mod tool_selection_contract {
    use super::*;
    use openwebide_core::{ToolSelection, WorkspaceMode};
    #[test]
    fn plugin_planning_applies_selection_and_model_settings_for_both_hosts() {
        use openwebide_core::plugins::{PluginTool, ProjectPlugin, RustPlugin};
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 3;
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.tools = vec![PluginTool {
            name: "community_lookup".into(),
            description: "Look up information".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: false,
        }];
        prepared.manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "lookup".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["http".into()],
        });
        let binding = ProjectPlugin {
            id: 1,
            revision: 1,
            prepared,
            enabled: true,
        };
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for projectless in [false, true] {
                for enabled in [false, true] {
                    for selection in [
                        ToolSelection::All,
                        ToolSelection::Selected(vec!["community_lookup".into()]),
                        ToolSelection::ChatOnly,
                    ] {
                        let mut runtime: ModelRuntime = serde_json::from_value(serde_json::json!({"connection":{"id":1,"name":"test","kind":"ollama","base_url":"http://localhost","model":"test","enabled":true},"settings":{},"transport":{}})).unwrap();
                        runtime.settings.tools = Some(enabled);
                        runtime.connection.tool_selection = selection.clone();
                        let plan = plan_with_plugins(
                            &runtime,
                            PlanInput {
                                environment: openwebide_core::RunEnvironment {
                                    project_name: (!projectless).then(|| "p".into()),
                                    project_root: (!projectless).then(|| "p".into()),
                                    mode: Some(mode),
                                    ..Default::default()
                                },
                                system_prompt: None,
                                messages: vec![],
                                tools: vec![],
                                content: "go".into(),
                                editor: None,
                            },
                            std::slice::from_ref(&binding),
                        )
                        .unwrap();
                        let advertised = enabled && selection.allows("community_lookup");
                        assert_eq!(
                            plan.request
                                .tools
                                .iter()
                                .any(|tool| tool.name == "community_lookup"),
                            advertised
                        );
                        assert_eq!(plan.plugin_executables.len(), usize::from(advertised));
                        assert_eq!(
                            matches!(plan.kind, openwebide_core::RunKind::Chat),
                            !advertised
                        );
                        if advertised {
                            let child = crate::tasks::child_request(
                                &plan.request,
                                &openwebide_core::NewAgentTask {
                                    description: "child".into(),
                                    prompt: "go".into(),
                                },
                                1,
                            )
                            .unwrap();
                            assert!(
                                child
                                    .tools
                                    .iter()
                                    .any(|tool| tool.name == "community_lookup")
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn both_hosts_filter_before_run_kind_context_and_children() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for host in [false, true] {
                let mut runtime: ModelRuntime = serde_json::from_value(serde_json::json!({"connection":{"id":1,"name":"test","kind":"ollama","base_url":"http://localhost","model":"test","enabled":true},"settings":{},"transport":{}})).unwrap();
                for selection in [
                    ToolSelection::All,
                    ToolSelection::Selected(vec!["read_file".into(), "run_command".into()]),
                    ToolSelection::Selected(vec![]),
                    ToolSelection::ChatOnly,
                    ToolSelection::Selected(vec!["unknown_tool".into()]),
                ] {
                    runtime.connection.tool_selection = selection.clone();
                    let plan = plan(
                        &runtime,
                        PlanInput {
                            environment: openwebide_core::RunEnvironment {
                                project_name: Some("p".into()),
                                project_root: Some("p".into()),
                                mode: Some(mode),
                                ..Default::default()
                            },
                            system_prompt: None,
                            messages: vec![],
                            tools: tools_for_host(host),
                            content: "go".into(),
                            editor: None,
                        },
                    );
                    assert!(
                        plan.request
                            .tools
                            .iter()
                            .all(|tool| selection.allows(&tool.name))
                    );
                    assert_eq!(
                        plan.request
                            .tools
                            .iter()
                            .any(|tool| tool.name == "run_command"),
                        host && selection.allows("run_command")
                    );
                    assert_eq!(
                        matches!(plan.kind, openwebide_core::RunKind::Chat),
                        plan.request.tools.is_empty()
                    );
                    assert_eq!(
                        plan.request.tools.iter().any(|tool| tool.name == "task"),
                        selection == ToolSelection::All
                    );
                    if !plan.request.tools.is_empty() {
                        let child = crate::tasks::child_request(
                            &plan.request,
                            &openwebide_core::NewAgentTask {
                                description: "child".into(),
                                prompt: "go".into(),
                            },
                            1,
                        )
                        .unwrap();
                        assert!(
                            child
                                .tools
                                .iter()
                                .all(|tool| plan.request.tools.contains(tool))
                        );
                        assert_eq!(
                            child.tools,
                            plan.request
                                .tools
                                .into_iter()
                                .filter(|tool| tool.name != "todo_write")
                                .collect::<Vec<_>>()
                        );
                    }
                }
                runtime.connection.tool_selection = ToolSelection::All;
                runtime.settings.tools = Some(false);
                assert!(
                    request(&runtime, None, vec![], tools_for_host(host))
                        .tools
                        .is_empty()
                );
                runtime.settings.tools = None;
                runtime.connection.tool_selection = ToolSelection::ChatOnly;
                let projectless = plan(
                    &runtime,
                    PlanInput {
                        environment: Default::default(),
                        system_prompt: None,
                        messages: vec![],
                        tools: vec![],
                        content: "go".into(),
                        editor: None,
                    },
                );
                assert!(matches!(projectless.kind, openwebide_core::RunKind::Chat));
                assert!(projectless.request.tools.is_empty());
            }
        }
    }
}
