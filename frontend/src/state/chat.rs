use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, atomic::AtomicBool},
};

use leptos::prelude::*;
use openwebide_agent::policy::ApprovalMode;
use openwebide_core::{
    ChatSession, FileDiff, ModelInfo, RunEvent, SessionTelemetry, TurnTelemetry,
};

use crate::conversation::{
    ConversationItem, ToolStepResult, local_message, next_item_nonce, notice, stopped_marker,
};

pub use openwebide_core::run::InterruptedRun;

pub fn interrupted_run(items: &[ConversationItem]) -> Option<InterruptedRun> {
    let items: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Message(message) => {
                Some(openwebide_core::RunItem::Message(message.clone()))
            }
            ConversationItem::ToolStep {
                id,
                name,
                summary,
                result,
                awaiting_permission,
                ..
            } => Some(openwebide_core::RunItem::Step(openwebide_core::RunStep {
                timing: None,
                id: id.clone(),
                name: name.clone(),
                summary: summary.clone(),
                awaiting_permission: *awaiting_permission,
                diff: None,
                note: None,
                result: result
                    .as_ref()
                    .map(|result| openwebide_core::ToolStepResultWire {
                        ok: result.ok,
                        summary: result.summary.clone(),
                        diff: None,
                    }),
            })),
            _ => None,
        })
        .collect();
    openwebide_core::run::interrupted_run(&items)
}

/// Side effects that need a browser or another feature store to execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatEffect {
    /// Automatically approve a tool request under the active session policy.
    ApprovePermission { id: String },
    /// Merge the edit into the owning project's pending diff state.
    ToolDiff(FileDiff),
}

/// An independently owned conversation row. Its UI key outlives a local message ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConversationHandle {
    pub key: u64,
    pub item: RwSignal<ConversationItem>,
    pub visible: RwSignal<bool>,
    pub permission: RwSignal<Option<(String, String)>>,
}

#[derive(Clone, Copy)]
pub struct ConversationStore {
    pub handles: RwSignal<Vec<ConversationHandle>>,
    pub changed: RwSignal<u64>,
    owners: StoredValue<HashMap<u64, Owner>>,
    owner: StoredValue<WeakOwner>,
}

impl ConversationStore {
    pub(crate) fn new() -> Self {
        Self {
            handles: RwSignal::new(Vec::new()),
            changed: RwSignal::new(0),
            owners: StoredValue::new(HashMap::new()),
            owner: StoredValue::new(
                Owner::current()
                    .expect("chat requires an owner")
                    .downgrade(),
            ),
        }
    }

    fn create(self, payload: ConversationItem) -> ConversationHandle {
        let owner = self.owner.with_value(|parent| {
            parent
                .upgrade()
                .expect("chat owner is alive")
                .with(Owner::new)
        });
        let key = next_item_nonce();
        let handle = owner.with(|| {
            let visible = RwSignal::new(Self::is_visible(&payload));
            let permission = RwSignal::new(Self::permission(&payload));
            let item = RwSignal::new(payload);
            ConversationHandle {
                key,
                item,
                visible,
                permission,
            }
        });
        self.owners.update_value(|owners| {
            owners.insert(key, owner);
        });
        handle
    }

    fn is_visible(item: &ConversationItem) -> bool {
        !matches!(item, ConversationItem::Message(message) if message.role == openwebide_core::Role::Assistant && message.content.is_empty() && message.tool_calls.is_some())
    }

    fn permission(item: &ConversationItem) -> Option<(String, String)> {
        match item {
            ConversationItem::Task(task) => task.pending_permission(),
            ConversationItem::ToolStep {
                id,
                name,
                awaiting_permission: true,
                result: None,
                ..
            } => Some((id.clone(), name.clone())),
            _ => None,
        }
    }

    fn refresh_metadata(handle: ConversationHandle) {
        let (visible, permission) = handle
            .item
            .with_untracked(|item| (Self::is_visible(item), Self::permission(item)));
        if handle.visible.get_untracked() != visible {
            handle.visible.set(visible);
        }
        if handle
            .permission
            .with_untracked(|current| *current != permission)
        {
            handle.permission.set(permission);
        }
    }

    pub fn get_untracked(self) -> Vec<ConversationItem> {
        self.handles.with_untracked(|handles| {
            handles
                .iter()
                .map(|handle| handle.item.get_untracked())
                .collect()
        })
    }

    pub fn snapshot(self) -> Vec<ConversationItem> {
        self.get_untracked()
    }

    pub fn install_history(self, items: Vec<ConversationItem>) {
        self.set(items);
    }

    pub fn reconcile(self, update: impl FnOnce(&mut Vec<ConversationItem>)) {
        self.update(update);
    }

    /// Installing history starts a new row lifetime, even for reused server IDs.
    pub fn set(self, items: Vec<ConversationItem>) {
        let handles = items.into_iter().map(|item| self.create(item)).collect();
        let old = self.handles.get_untracked();
        self.handles.set(handles);
        self.remove(&old);
        self.changed.update(|version| *version += 1);
    }

    fn remove(self, handles: &[ConversationHandle]) {
        self.owners.update_value(|owners| {
            for handle in handles {
                if let Some(owner) = owners.remove(&handle.key) {
                    owner.cleanup();
                }
            }
        });
    }

    /// Reconcile structural events without publishing unchanged rows or ordering.
    pub fn update(self, update: impl FnOnce(&mut Vec<ConversationItem>)) {
        let old = self.handles.get_untracked();
        let mut items = self.snapshot();
        update(&mut items);
        let identities: HashSet<_> = items.iter().map(crate::conversation::item_key).collect();
        let mut available = old.clone();
        let mut handles = Vec::with_capacity(items.len());
        for (index, payload) in items.into_iter().enumerate() {
            let identity = crate::conversation::item_key(&payload);
            let found = available.iter().position(|handle| {
                handle.item.with_untracked(|item| crate::conversation::item_key(item) == identity)
            }).or_else(|| {
                let previous = old.get(index)?;
                // A live row can follow newly replayed messages; reserve its existing identity first.
                let replaces_local = previous.item.with_untracked(|item| {
                    matches!(item, ConversationItem::Message(message) if message.id <= 0 && message.role == openwebide_core::Role::Assistant)
                        && !identities.contains(&crate::conversation::item_key(item))
                }) && matches!(&payload, ConversationItem::Message(message) if message.role == openwebide_core::Role::Assistant);
                if replaces_local { available.iter().position(|handle| handle.key == previous.key) } else { None }
            });
            let handle = if let Some(position) = found {
                let handle = available.remove(position);
                if handle.item.with_untracked(|item| *item != payload) {
                    handle.item.set(payload);
                    Self::refresh_metadata(handle);
                }
                handle
            } else {
                self.create(payload)
            };
            handles.push(handle);
        }
        if handles != old {
            self.handles.set(handles);
        }
        self.remove(&available);
        self.changed.update(|version| *version += 1);
    }

    pub fn last(self) -> Option<ConversationHandle> {
        self.handles
            .with_untracked(|handles| handles.last().copied())
    }

    pub fn push(self, item: ConversationItem) {
        let handle = self.create(item);
        self.handles.update(|handles| handles.push(handle));
        self.changed.update(|version| *version += 1);
    }

    pub fn update_item(
        self,
        handle: ConversationHandle,
        update: impl FnOnce(&mut ConversationItem),
    ) {
        handle.item.update(update);
        Self::refresh_metadata(handle);
        self.changed.update(|version| *version += 1);
    }

    pub fn message(self, id: i64) -> Option<ConversationHandle> {
        self.handles.with_untracked(|handles| {
            handles.iter().copied().find(|handle| {
                handle.item.with_untracked(
                    |item| matches!(item, ConversationItem::Message(message) if message.id == id),
                )
            })
        })
    }

    pub fn tool(self, id: &str) -> Option<ConversationHandle> {
        self.handles.with_untracked(|handles| handles.iter().rev().copied().find(|handle| handle.item.with_untracked(|item| matches!(item, ConversationItem::ToolStep { id: step, .. } if step == id))))
    }

    fn pop(self) {
        let removed = self.handles.try_update(Vec::pop).flatten();
        if let Some(handle) = removed {
            self.remove(&[handle]);
        }
        self.changed.update(|version| *version += 1);
    }

    /// Show the user's prompt immediately, before the server has saved it.
    pub fn push_optimistic_user(self, session_id: i64, content: String) {
        self.push(crate::conversation::local_user_message(session_id, content));
    }

    /// Roll back a trailing optimistic prompt when the send failed before the
    /// server saved it.
    pub fn drop_optimistic_user(self) {
        if self
            .last()
            .is_some_and(|handle| handle.item.with_untracked(crate::conversation::is_local_user_message))
        {
            self.pop();
        }
    }

    pub fn clear_prompt(self, id: &str, first_only: bool) {
        let handles = self.handles.get_untracked();
        for handle in handles {
            if handle.item.with_untracked(|item| {
                matches!(item, ConversationItem::ToolStep { id: step, .. } if step == id)
                    || matches!(item, ConversationItem::Task(task) if task.contains_tool(id))
            }) {
                self.update_item(handle, |item| {
                    if let ConversationItem::Task(task) = item {
                        task.clear_permission(id);
                    }
                    if let ConversationItem::ToolStep {
                        awaiting_permission,
                        ..
                    } = item
                    {
                        *awaiting_permission = false;
                    }
                });
                if first_only {
                    break;
                }
            }
        }
    }

    fn append_delta(self, session_id: i64, content: &str, reasoning: bool, close_reasoning: bool) {
        let last = self
            .handles
            .with_untracked(|handles| handles.last().copied());
        if let Some(handle) = last.filter(|handle| handle.item.with_untracked(|item| matches!(item, ConversationItem::Message(message) if message.role == openwebide_core::Role::Assistant && message.id <= 0))) {
            self.update_item(handle, |item| {
                if let ConversationItem::Message(message) = item {
                    if close_reasoning { message.content.push_str("</think>"); }
                    message.content.push_str(content);
                }
            });
        } else {
            let text = if reasoning { format!("{}{content}", openwebide_core::ESCAPED_REASONING_OPEN) } else { content.to_string() };
            let handle = self.create(local_message(session_id, text));
            self.handles.update(|handles| handles.push(handle));
            self.changed.update(|version| *version += 1);
        }
    }
}

/// Signals shared by the chat pane and run handlers.
#[derive(Clone, Copy)]
pub struct ChatState {
    pub sessions: RwSignal<Vec<ChatSession>>,
    pub last_sessions: RwSignal<HashMap<i64, i64>>,
    pub last_chat_session: RwSignal<Option<i64>>,
    pub active_session: RwSignal<Option<i64>>,
    pub has_session: Memo<bool>,
    pub messages: ConversationStore,
    pub run_contexts: Memo<std::collections::BTreeMap<i64, ConversationHandle>>,
    pub turn_summaries: Memo<std::collections::BTreeMap<i64, crate::turn_summary::TurnSummary>>,
    pub history_gen: StoredValue<u64>,
    pub send_generation: StoredValue<u64>,
    pub skip_history_load: StoredValue<Option<i64>>,
    pub creating_session: RwSignal<bool>,
    pub streaming: RwSignal<bool>,
    pub rewinding: RwSignal<bool>,
    pub finished_tools: RwSignal<u64>,
    pub reasoning_active: RwSignal<bool>,
    pub active_run: RwSignal<Option<(i64, String, u64)>>,
    pub notice: RwSignal<Option<String>>,
    pub interrupted_run: RwSignal<Option<InterruptedRun>>,
    pub bridge_folder_notices: StoredValue<HashSet<i64>>,
    pub dismissed_interruptions: StoredValue<HashSet<i64>>,
    pub error: RwSignal<Option<String>>,
    pub draft: RwSignal<String>,
    pub prompt_images: RwSignal<Vec<openwebide_core::PromptImage>>,
    pub reading_images: RwSignal<bool>,
    pub image_picker_requested: RwSignal<bool>,
    pub compact_epoch: StoredValue<u64>,
    pub compacting: RwSignal<bool>,
    pub goal: RwSignal<Option<openwebide_core::Goal>>,
    pub goal_busy: RwSignal<bool>,
    pub goal_error: RwSignal<Option<String>>,
    pub todo_plan: RwSignal<Option<openwebide_core::TodoUpdate>>,
    pub todo_loading: RwSignal<bool>,
    pub todo_error: RwSignal<Option<String>>,
    pub queued_prompts: RwSignal<Vec<openwebide_core::QueuedPrompt>>,
    pub queue_busy: RwSignal<bool>,
    pub queue_loading: RwSignal<bool>,
    pub queue_running: RwSignal<HashSet<i64>>,
    pub queue_edit: RwSignal<Option<openwebide_core::QueuedPromptKey>>,
    pub queue_steering: RwSignal<Option<i64>>,
    pub queue_delivering: RwSignal<Option<(i64, openwebide_core::QueuedPromptKey)>>,
    pub queue_epoch: StoredValue<u64>,
    pub loading_history: RwSignal<Option<i64>>,
    pub prompt_edit: RwSignal<Option<(i64, i64)>>,
    pub branching: RwSignal<bool>,
    /// Account, project and branch awaiting composer restoration.
    pub branch_draft_context: StoredValue<Option<(u64, Option<i64>, i64)>>,
    pub prompt_history: RwSignal<Vec<String>>,
    pub show_terminal: RwSignal<bool>,
    #[cfg(target_arch = "wasm32")]
    pub abort: RwSignal<Option<web_sys::AbortController>>,
    pub streaming_session: RwSignal<Option<i64>>,
    pub streaming_is_local: RwSignal<bool>,
    pub local_cancel_flag: StoredValue<Arc<AtomicBool>>,
    pub local_permissions: StoredValue<Arc<Mutex<HashMap<String, bool>>>>,
    pub models: RwSignal<Vec<ModelInfo>>,
    pub session_model: RwSignal<HashMap<i64, Option<String>>>,
    pub selected_model: RwSignal<Option<String>>,
    pub draft_connection: RwSignal<Option<i64>>,
    pub connection_changing: RwSignal<bool>,
    pub session_telemetry: RwSignal<SessionTelemetry>,
    pub approval_mode: RwSignal<HashMap<i64, ApprovalMode>>,
    pub draft_approval_mode: RwSignal<ApprovalMode>,
    pub current_run_anchor: RwSignal<Option<i64>>,
    pub active_editor_context: RwSignal<Option<openwebide_core::EditorContext>>,
    pub awaiting_step_id: Memo<Option<String>>,
}

impl ChatState {
    pub fn request_image_picker(self) {
        if !self.reading_images.get_untracked() {
            self.image_picker_requested.set(true);
        }
    }

    pub fn new() -> Self {
        Self::with_active_session_and_toast(RwSignal::new(None), RwSignal::new(None))
    }

    pub fn with_active_session(active_session: RwSignal<Option<i64>>) -> Self {
        Self::with_active_session_and_toast(active_session, RwSignal::new(None))
    }

    pub fn with_active_session_and_toast(
        active_session: RwSignal<Option<i64>>,
        error: RwSignal<Option<String>>,
    ) -> Self {
        let messages = ConversationStore::new();
        let run_contexts = Memo::new(move |_| {
            let mut contexts = std::collections::BTreeMap::new();
            let mut prompt = None;
            let mut startup = None;
            messages.handles.with(|handles| {
                for handle in handles {
                    handle.item.with(|item| {
                        if let ConversationItem::Message(message) = item
                            && message.role == openwebide_core::Role::User
                            && message.id > 0
                        {
                            prompt = Some(message.id);
                            if let Some(context) = startup.take() {
                                contexts.insert(message.id, context);
                            }
                        } else if crate::conversation::is_run_context(item) {
                            if let Some(prompt) = prompt {
                                contexts.insert(prompt, *handle);
                            } else {
                                startup = Some(*handle);
                            }
                        }
                    });
                }
            });
            contexts
        });
        let turn_summaries = Memo::new(move |_| {
            let mut summaries = crate::turn_summary::TurnSummaries::default();
            messages.handles.with(|handles| {
                for handle in handles {
                    handle.item.with(|item| summaries.observe(item));
                }
            });
            summaries.finish()
        });
        let current_run_anchor = RwSignal::new(None);
        let has_session = Memo::new(move |_| active_session.get().is_some());
        let awaiting_step_id = Memo::new(move |_| {
            let anchor = current_run_anchor.get()?;
            let prefix = openwebide_agent::step_id_prefix(anchor);
            messages.handles.with(|handles| {
                handles.iter().rev().find_map(|handle| {
                    handle.permission.with(|permission| {
                        permission
                            .as_ref()
                            .filter(|(id, _)| id.starts_with(&prefix))
                            .map(|(id, _)| id.clone())
                    })
                })
            })
        });

        Self {
            sessions: RwSignal::new(Vec::new()),
            last_sessions: RwSignal::new(HashMap::new()),
            last_chat_session: RwSignal::new(None),
            active_session,
            has_session,
            messages,
            run_contexts,
            turn_summaries,
            history_gen: StoredValue::new(0),
            send_generation: StoredValue::new(0),
            skip_history_load: StoredValue::new(None),
            creating_session: RwSignal::new(false),
            streaming: RwSignal::new(false),
            rewinding: RwSignal::new(false),
            finished_tools: RwSignal::new(0),
            reasoning_active: RwSignal::new(false),
            active_run: RwSignal::new(None),
            notice: RwSignal::new(None),
            interrupted_run: RwSignal::new(None),
            bridge_folder_notices: StoredValue::new(HashSet::new()),
            dismissed_interruptions: StoredValue::new(HashSet::new()),
            error,
            draft: RwSignal::new(String::new()),
            prompt_images: RwSignal::new(Vec::new()),
            reading_images: RwSignal::new(false),
            image_picker_requested: RwSignal::new(false),
            compact_epoch: StoredValue::new(0),
            compacting: RwSignal::new(false),
            goal: RwSignal::new(None),
            goal_busy: RwSignal::new(false),
            goal_error: RwSignal::new(None),
            todo_plan: RwSignal::new(None),
            todo_loading: RwSignal::new(false),
            todo_error: RwSignal::new(None),
            queued_prompts: RwSignal::new(Vec::new()),
            queue_busy: RwSignal::new(false),
            queue_loading: RwSignal::new(false),
            queue_running: RwSignal::new(HashSet::new()),
            queue_edit: RwSignal::new(None),
            queue_steering: RwSignal::new(None),
            queue_delivering: RwSignal::new(None),
            queue_epoch: StoredValue::new(0),
            loading_history: RwSignal::new(None),
            prompt_edit: RwSignal::new(None),
            branching: RwSignal::new(false),
            branch_draft_context: StoredValue::new(None),
            prompt_history: RwSignal::new(Vec::new()),
            show_terminal: RwSignal::new(false),
            #[cfg(target_arch = "wasm32")]
            abort: RwSignal::new(None),
            streaming_session: RwSignal::new(None),
            streaming_is_local: RwSignal::new(false),
            local_cancel_flag: StoredValue::new(Arc::new(AtomicBool::new(false))),
            local_permissions: StoredValue::new(Arc::new(Mutex::new(HashMap::new()))),
            models: RwSignal::new(Vec::new()),
            session_model: RwSignal::new(HashMap::new()),
            selected_model: RwSignal::new(None),
            draft_connection: RwSignal::new(None),
            connection_changing: RwSignal::new(false),
            session_telemetry: RwSignal::new(SessionTelemetry::default()),
            approval_mode: RwSignal::new(HashMap::new()),
            draft_approval_mode: RwSignal::new(ApprovalMode::NEW_SESSION),
            current_run_anchor,
            active_editor_context: RwSignal::new(None),
            awaiting_step_id,
        }
    }

    pub fn detect_interrupted_run(&self, resumable: bool) {
        let interrupted = if resumable
            && !self.streaming.get_untracked()
            && self.active_run.get_untracked().is_none()
        {
            interrupted_run(&self.messages.get_untracked())
        } else {
            None
        };
        self.interrupted_run.set(interrupted.filter(|run| {
            !self
                .dismissed_interruptions
                .with_value(|dismissed| dismissed.contains(&run.anchor_id))
        }));
    }

    /// Restore a project's remembered session, falling back to its newest session.
    pub fn restore_project_session(&self, project_id: i64) {
        let remembered = self
            .last_sessions
            .with_untracked(|sessions| sessions.get(&project_id).copied());
        let existing = self.active_session.get_untracked();
        let selected = self.sessions.with_untracked(|sessions| {
            let belongs = |id| {
                sessions
                    .iter()
                    .any(|session| session.id == id && session.project_id == Some(project_id))
            };
            existing
                .filter(|id| belongs(*id))
                .or_else(|| remembered.filter(|id| belongs(*id)))
                .or_else(|| {
                    sessions
                        .iter()
                        .filter(|session| session.project_id == Some(project_id))
                        .max_by_key(|session| (session.created_at, session.id))
                        .map(|session| session.id)
                })
        });
        self.active_session.set(selected);
    }

    pub fn restore_chat_session(&self) {
        let remembered = self.last_chat_session.get_untracked();
        let selected = self.sessions.with_untracked(|sessions| {
            remembered
                .filter(|id| {
                    sessions
                        .iter()
                        .any(|session| session.id == *id && session.project_id.is_none())
                })
                .or_else(|| {
                    sessions
                        .iter()
                        .filter(|session| session.project_id.is_none())
                        .max_by_key(|session| (session.created_at, session.id))
                        .map(|session| session.id)
                })
        });
        self.active_session.set(selected);
    }

    pub fn dismiss_interrupted_run(&self) {
        if let Some(run) = self.interrupted_run.get_untracked() {
            self.dismissed_interruptions.update_value(|dismissed| {
                dismissed.insert(run.anchor_id);
            });
        }
        self.interrupted_run.set(None);
    }

    /// Append a transient notice to the active conversation.
    pub fn notify(&self, text: impl Into<String>) {
        self.messages.push(notice(text));
    }

    #[cfg(target_arch = "wasm32")]
    pub fn notify_bridge_folder_once(&self, session_id: i64) {
        if self.active_session.get_untracked() != Some(session_id) {
            return;
        }
        let first = self
            .bridge_folder_notices
            .try_update_value(|sessions| sessions.insert(session_id))
            .unwrap_or(false);
        if first {
            self.notify(crate::local_agent::BRIDGE_FOLDER_NOTICE);
        }
    }

    pub fn set_approval_mode(&self, session_id: i64, mode: ApprovalMode) {
        self.approval_mode.update(|modes| {
            modes.insert(session_id, mode);
        });
    }

    /// Mark prompts owned by this run as cancelled.
    pub fn cancel_run_prompts(&self, anchor: i64) {
        let prefix = openwebide_agent::step_id_prefix(anchor);
        for handle in self.messages.handles.get_untracked() {
            if handle.item.with_untracked(|item| matches!(item, ConversationItem::ToolStep { id, result: None, .. } if id.starts_with(&prefix)) || matches!(item, ConversationItem::Task(task) if task.task.parent_tool_call_id.starts_with(&prefix))) {
                self.messages.update_item(handle, |item| cancel_run_prompts(std::slice::from_mut(item), anchor));
            }
        }
    }

    pub fn mark_stopped(&self) {
        if let Some(anchor) = self.current_run_anchor.get_untracked() {
            self.cancel_run_prompts(anchor);
        }
        self.close_open_reasoning();
        self.reasoning_active.set(false);
        self.ensure_stopped_marker();
        self.current_run_anchor.set(None);
    }

    /// Close a reasoning tag left open by the abort so the "Thinking..."
    /// state clears and the partial trace renders as a collapsed summary.
    fn close_open_reasoning(&self) {
        let handle = self
            .messages
            .handles
            .with_untracked(|handles| {
                handles.iter().rev().copied().find(|handle| {
                    handle.item.with_untracked(|item| {
                        matches!(item, ConversationItem::Message(message)
                            if message.role == openwebide_core::Role::Assistant
                                && message.id <= 0
                                && openwebide_core::tui::parse_thinking(&message.content).is_thinking)
                    })
                })
            });
        if let Some(handle) = handle {
            self.messages.update_item(handle, |item| {
                if let ConversationItem::Message(message) = item {
                    message.content.push_str("\n</think>\n\n");
                }
            });
        }
    }

    fn ensure_stopped_marker(&self) {
        if !self.messages.last().is_some_and(|handle| {
            handle
                .item
                .with_untracked(|item| matches!(item, ConversationItem::Stopped { .. }))
        }) {
            self.messages.push(stopped_marker());
        }
    }

    /// Apply one stream event to chat state and return effects owned by the
    /// browser or another feature store.
    pub fn apply_event(&self, event: RunEvent) -> Vec<ChatEffect> {
        self.apply_event_for_run(self.streaming_session.get_untracked(), event)
    }

    /// Apply an event from a specific stream run, dropping state changes once
    /// the user has switched sessions while the request was in flight.
    pub fn apply_event_for_session(&self, session_id: i64, event: RunEvent) -> Vec<ChatEffect> {
        self.apply_event_for_run(Some(session_id), event)
    }

    fn apply_event_for_run(&self, run_session: Option<i64>, event: RunEvent) -> Vec<ChatEffect> {
        let mut effects = Vec::new();
        if let RunEvent::ToolResult {
            diff: Some(diff), ..
        } = &event
        {
            effects.push(ChatEffect::ToolDiff(diff.clone()));
        }

        if let RunEvent::Task { update } = &event
            && let openwebide_core::TaskEvent::Run { event } = update.leaf_event()
            && let RunEvent::ToolResult {
                diff: Some(diff), ..
            } = &**event
        {
            effects.push(ChatEffect::ToolDiff(diff.clone()));
        }
        if run_session.is_some() && self.active_session.get_untracked() != run_session {
            return effects;
        }
        let session_id = run_session
            .or_else(|| self.active_session.get_untracked())
            .unwrap_or_default();

        let close_reasoning = self.reasoning_active.get_untracked();
        match &event {
            RunEvent::ReasoningDelta { .. } => self.reasoning_active.set(true),
            RunEvent::Task { .. } | RunEvent::Telemetry { .. } | RunEvent::ToolTiming { .. } => {}
            _ => self.reasoning_active.set(false),
        }
        match event {
            RunEvent::Task { update } => {
                let existing = self.messages.handles.with_untracked(|handles| handles.iter().copied().find(|handle| handle.item.with_untracked(|item| matches!(item, ConversationItem::Task(task) if task.task.id == update.task.id))));
                if existing.is_some_and(|handle| handle.item.with_untracked(|item| matches!(item, ConversationItem::Task(task) if task.find_task(&update.leaf_update().task.id).is_some_and(|leaf| leaf.run.finished.is_some())))) {
                    return Vec::new();
                }
                let old_usage = existing.map_or_else(Vec::new, |handle| {
                    handle.item.with_untracked(|item| match item {
                        ConversationItem::Task(task) => task.usages(),
                        _ => Vec::new(),
                    })
                });
                if let openwebide_core::TaskEvent::Run { event } = update.leaf_event() {
                    match &**event {
                        RunEvent::PermissionRequest { id, name, .. } => {
                            let mode = self.approval_mode.with_untracked(|modes| {
                                modes.get(&session_id).copied().unwrap_or_default()
                            });
                            if mode == ApprovalMode::AlwaysForSession && mode.auto_approves(name) {
                                effects.push(ChatEffect::ApprovePermission { id: id.clone() });
                            }
                        }
                        RunEvent::ToolCall { .. } => self
                            .session_telemetry
                            .update(|telemetry| telemetry.tool_calls_count += 1),
                        RunEvent::ToolResult { .. } => {
                            self.finished_tools.update(|count| *count += 1);
                        }
                        _ => (),
                    }
                }

                if let Some(handle) = existing {
                    self.messages.update_item(handle, |item| {
                        if let ConversationItem::Task(task) = item {
                            task.apply(&update);
                        }
                    });
                } else {
                    let mut task = openwebide_core::TaskSnapshot::new(update.task.clone());
                    task.apply(&update);
                    self.messages.push(ConversationItem::Task(Box::new(task)));
                }
                let new_usage = self.messages.handles.with_untracked(|handles| {
                    handles
                        .iter()
                        .find_map(|handle| {
                            handle.item.with_untracked(|item| match item {
                                ConversationItem::Task(task) if task.task.id == update.task.id => {
                                    Some(task.usages())
                                }
                                _ => None,
                            })
                        })
                        .unwrap_or_default()
                });
                let totals = |usages: &[TurnTelemetry]| {
                    usages
                        .iter()
                        .fold((0usize, 0usize), |(prompt, completion), usage| {
                            (
                                prompt.saturating_add(usage.prompt_tokens),
                                completion.saturating_add(usage.completion_tokens),
                            )
                        })
                };
                let (old_prompt, old_completion) = totals(&old_usage);
                let (new_prompt, new_completion) = totals(&new_usage);
                self.session_telemetry.update(|telemetry| {
                    telemetry.total_prompt_tokens = telemetry
                        .total_prompt_tokens
                        .saturating_sub(old_prompt)
                        .saturating_add(new_prompt);
                    telemetry.total_completion_tokens = telemetry
                        .total_completion_tokens
                        .saturating_sub(old_completion)
                        .saturating_add(new_completion);
                    telemetry.totals_estimated |= new_usage.iter().any(|usage| usage.estimated);
                });
            }
            RunEvent::ToolTiming { id, timing } => {
                if let Some(handle) = self.messages.tool(&id) {
                    self.messages.update_item(handle, |item| {
                        if let ConversationItem::ToolStep {
                            timing: previous, ..
                        } = item
                            && let Ok(timing) =
                                previous.map_or(Ok(timing), |previous| previous.merge(timing))
                        {
                            *previous = Some(timing);
                        }
                    });
                }
            }
            RunEvent::Message { message: msg } => {
                if msg.role == openwebide_core::Role::User {
                    self.current_run_anchor.set(Some(msg.id));
                }
                if let Some(handle) = self.messages.message(msg.id) {
                    self.messages
                        .update_item(handle, |item| *item = ConversationItem::Message(msg));
                } else if msg.role == openwebide_core::Role::User
                    && let Some(handle) = self.messages.last().filter(|handle| {
                        handle
                            .item
                            .with_untracked(crate::conversation::is_local_user_message)
                    })
                {
                    self.messages
                        .update_item(handle, |item| *item = ConversationItem::Message(msg));
                } else {
                    self.messages.push(ConversationItem::Message(msg));
                }
            }
            RunEvent::ReasoningDelta { content } => self.messages.append_delta(
                session_id,
                &openwebide_core::escape_reasoning(&content),
                true,
                false,
            ),
            RunEvent::Delta { content } => {
                self.messages
                    .append_delta(session_id, &content, false, close_reasoning);
            }
            RunEvent::PermissionRequest {
                id,
                name,
                summary,
                diff,
                note,
            } => {
                let mode = self
                    .approval_mode
                    .with_untracked(|modes| modes.get(&session_id).copied().unwrap_or_default());
                if mode == ApprovalMode::AlwaysForSession && mode.auto_approves(&name) {
                    effects.push(ChatEffect::ApprovePermission { id });
                } else {
                    self.messages.push(ConversationItem::ToolStep {
                        timing: None,
                        key: next_item_nonce(),
                        id,
                        name,
                        summary,
                        result: None,
                        awaiting_permission: true,
                        diff,
                        note,
                    });
                }
            }
            RunEvent::ToolCall { id, name, summary } => {
                self.session_telemetry
                    .update(|telemetry| telemetry.tool_calls_count += 1);
                if let Some(handle) = self.messages.tool(&id) {
                    self.messages.update_item(handle, |item| {
                        if let ConversationItem::ToolStep {
                            summary: step_summary,
                            awaiting_permission,
                            ..
                        } = item
                        {
                            *step_summary = summary;
                            *awaiting_permission = false;
                        }
                    });
                } else {
                    self.messages.push(ConversationItem::ToolStep {
                        timing: None,
                        key: next_item_nonce(),
                        id,
                        name,
                        summary,
                        result: None,
                        awaiting_permission: false,
                        diff: None,
                        note: None,
                    });
                }
            }
            RunEvent::ToolResult {
                id,
                name: _,
                ok,
                summary,
                diff,
            } => {
                self.finished_tools.update(|count| *count += 1);
                if let Some(handle) = self.messages.tool(&id) {
                    self.messages.update_item(handle, |item| {
                        if let ConversationItem::ToolStep {
                            result,
                            awaiting_permission,
                            ..
                        } = item
                        {
                            *result = Some(ToolStepResult { ok, summary, diff });
                            *awaiting_permission = false;
                        }
                    });
                } else {
                    self.messages.push(ConversationItem::ToolStep {
                        timing: None,
                        key: next_item_nonce(),
                        id,
                        name: String::new(),
                        summary: summary.clone(),
                        result: Some(ToolStepResult { ok, summary, diff }),
                        awaiting_permission: false,
                        diff: None,
                        note: None,
                    });
                }
            }
            RunEvent::Interim { message } | RunEvent::Done { message } => {
                let placeholder = self.messages.last().filter(|handle| handle.item.with_untracked(|item| matches!(item, ConversationItem::Message(message) if message.role == openwebide_core::Role::Assistant && message.id <= 0)));
                if let Some(existing) = (message.id > 0)
                    .then(|| self.messages.message(message.id))
                    .flatten()
                {
                    self.messages
                        .update_item(existing, |item| *item = ConversationItem::Message(message));
                    if placeholder.is_some() {
                        self.messages.pop();
                    }
                } else if let Some(placeholder) = placeholder {
                    self.messages.update_item(placeholder, |item| {
                        *item = ConversationItem::Message(message);
                    });
                } else {
                    self.messages.push(ConversationItem::Message(message));
                }
            }
            RunEvent::Telemetry { usage: telemetry } => {
                self.session_telemetry
                    .update(|session| session.record_turn(&telemetry));
            }
            RunEvent::Cancelled => {
                self.mark_stopped();
            }
            RunEvent::Error { message: error } => {
                self.close_open_reasoning();
                if let Some(anchor) = self.current_run_anchor.get_untracked() {
                    self.cancel_run_prompts(anchor);
                }
                self.error.set(Some(error));
            }
        }

        effects
    }
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}

fn cancel_run_prompts(items: &mut [ConversationItem], anchor: i64) {
    let prefix = openwebide_agent::step_id_prefix(anchor);
    for item in items {
        if let ConversationItem::Task(task) = item
            && task.task.parent_tool_call_id.starts_with(&prefix)
        {
            let now = task.task.timing.map_or(0, |timing| {
                timing.started_at_ms.saturating_add(timing.elapsed_ms)
            });
            task.stop(now);
        }
        if let ConversationItem::ToolStep {
            id,
            awaiting_permission,
            result,
            ..
        } = item
            && id.starts_with(&prefix)
            && result.is_none()
        {
            *awaiting_permission = false;
            *result = Some(ToolStepResult {
                ok: false,
                summary: "cancelled".into(),
                diff: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use leptos::prelude::Owner;
    use openwebide_agent::policy::ApprovalMode;
    use openwebide_core::{ChatMessage, FileDiff, Role, TurnTelemetry};

    use super::*;

    fn message(id: i64, role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            id,
            session_id: 7,
            role,
            content: content.into(),
            created_at: 0,
            tool_calls: None,
            tool_call_id: None,
            usage: None,
        }
    }

    fn complete_step(id: &str) -> ConversationItem {
        ConversationItem::ToolStep {
            timing: None,
            key: next_item_nonce(),
            id: id.into(),
            name: "read_file".into(),
            summary: "read".into(),
            result: Some(ToolStepResult {
                ok: true,
                summary: "read".into(),
                diff: None,
            }),
            awaiting_permission: false,
            diff: None,
            note: None,
        }
    }

    fn interim(id: i64) -> ConversationItem {
        let mut msg = message(id, Role::Assistant, "checking");
        msg.tool_calls = Some(vec![openwebide_core::ToolCall {
            id: "wire".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }]);
        ConversationItem::Message(msg)
    }

    #[test]
    fn run_contexts_stay_with_their_prompt_and_ignore_compaction() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            let context = |id, text| {
                ConversationItem::Message(message(
                    id,
                    Role::System,
                    &format!("{}{text}", openwebide_core::RUN_CONTEXT_PREFIX),
                ))
            };
            let prompt = |id| ConversationItem::Message(message(id, Role::User, "prompt"));
            chat.messages
                .install_history(vec![context(1, "startup"), prompt(2)]);
            let context_id = |prompt| {
                chat.run_contexts.with_untracked(|contexts| {
                    contexts.get(&prompt).map(|handle| {
                        handle.item.with_untracked(|item| match item {
                            ConversationItem::Message(message) => message.id,
                            _ => 0,
                        })
                    })
                })
            };
            assert_eq!(context_id(2), Some(1));
            chat.messages.push(context(3, "first run"));
            chat.messages.push(ConversationItem::Message(message(
                4,
                Role::System,
                &format!("{}summary", openwebide_core::COMPACTION_PREFIX),
            )));
            chat.messages.push(prompt(5));
            chat.messages.push(context(6, "second run"));
            chat.messages.push(prompt(7));
            assert_eq!(context_id(2), Some(3));
            assert_eq!(context_id(5), Some(6));
            assert_eq!(context_id(7), None);
            chat.messages.install_history(Vec::new());
            assert!(
                chat.run_contexts
                    .with_untracked(std::collections::BTreeMap::is_empty)
            );
        });
    }

    #[test]
    fn snapshot_inserts_interim_before_the_existing_live_row_without_stealing_its_handle() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.messages.push(local_message(7, "live"));
            let live = chat.messages.last().unwrap();
            let snapshot = openwebide_core::RunSnapshot {
                items: vec![openwebide_core::RunItem::Message(message(8, Role::Assistant, "interim"))],
                text: "updated live".into(),
                ..Default::default()
            };
            for _ in 0..2 {
                chat.messages.reconcile(|items| crate::conversation::merge_snapshot(items, &snapshot));
                assert_eq!(chat.messages.last(), Some(live));
                assert!(matches!(chat.messages.snapshot()[0], ConversationItem::Message(ref message) if message.content == "interim"));
                assert!(matches!(live.item.get_untracked(), ConversationItem::Message(message) if message.content == "updated live"));
            }
        });
    }

    #[test]
    fn duplicate_tool_ids_update_the_newest_handle_and_results_before_calls_stay_completed() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            for _ in 0..2 {
                chat.apply_event(RunEvent::PermissionRequest { id: "legacy".into(), name: "write_file".into(), summary: "pending".into(), diff: None, note: None });
            }
            let handles = chat.messages.handles.get_untracked();
            chat.apply_event(RunEvent::ToolResult { id: "legacy".into(), name: "write_file".into(), ok: true, summary: "written".into(), diff: None });
            assert!(matches!(handles[0].item.get_untracked(), ConversationItem::ToolStep { result: None, .. }));
            assert!(matches!(handles[1].item.get_untracked(), ConversationItem::ToolStep { result: Some(_), .. }));
            chat.apply_event(RunEvent::ToolCall { id: "legacy".into(), name: "write_file".into(), summary: "call".into() });
            assert_eq!(chat.messages.handles.get_untracked(), handles);
            assert!(matches!(handles[1].item.get_untracked(), ConversationItem::ToolStep { result: Some(_), summary, .. } if summary == "call"));
        });
    }

    #[test]
    fn permission_selection_keeps_the_newest_unresolved_step_in_the_current_run() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.current_run_anchor.set(Some(7));
            for id in ["a7t0c0", "a3t0c0", "a7t0c1"] {
                chat.apply_event(RunEvent::PermissionRequest {
                    id: id.into(),
                    name: "write_file".into(),
                    summary: "write".into(),
                    diff: None,
                    note: None,
                });
            }
            assert_eq!(
                chat.awaiting_step_id.get_untracked().as_deref(),
                Some("a7t0c1")
            );
            let handles = chat.messages.handles.get_untracked();
            chat.apply_event(RunEvent::ToolResult {
                id: "a7t0c1".into(),
                name: "write_file".into(),
                ok: false,
                summary: "denied".into(),
                diff: None,
            });
            assert_eq!(
                chat.awaiting_step_id.get_untracked().as_deref(),
                Some("a7t0c0")
            );
            assert_eq!(chat.messages.handles.get_untracked(), handles);
            chat.cancel_run_prompts(7);
            assert!(chat.awaiting_step_id.get_untracked().is_none());
        });
    }

    #[test]
    fn dropping_the_chat_owner_disposes_rows() {
        let owner = Owner::new();
        let row = owner.with(|| {
            let chat = ChatState::new();
            chat.notify("notice");
            chat.messages.handles.get_untracked()[0]
        });
        drop(owner);
        assert!(row.item.try_get_untracked().is_none());
    }

    #[test]
    fn row_identity_survives_deltas_server_ids_and_equal_length_replacements() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.apply_event(RunEvent::Delta { content: "first".into() });
            let row = chat.messages.handles.get_untracked()[0];
            let publications = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = publications.clone();
            let list = Memo::new(move |_| {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                chat.messages.handles.with(|handles| handles.iter().copied().filter(|handle| handle.visible.get()).collect::<Vec<_>>())
            });
            list.get();
            for _ in 0..1000 {
                chat.apply_event(RunEvent::Delta { content: ".".into() });
                assert_eq!(list.get(), vec![row]);
            }
            assert_eq!(publications.load(std::sync::atomic::Ordering::Relaxed), 1);
            chat.apply_event(RunEvent::Done { message: message(9, openwebide_core::Role::Assistant, "final") });
            assert_eq!(chat.messages.handles.get_untracked()[0], row);
            chat.apply_event(RunEvent::Message { message: message(9, openwebide_core::Role::Assistant, "other") });
            assert_eq!(chat.messages.handles.get_untracked()[0], row);
            assert!(matches!(row.item.get_untracked(), ConversationItem::Message(message) if message.content == "other"));
        });
    }

    #[test]
    fn removing_and_resetting_rows_disposes_their_payloads() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.notify("notice");
            let row = chat.messages.handles.get_untracked()[0];
            chat.messages.reconcile(Vec::clear);
            assert!(row.item.try_get_untracked().is_none());
            chat.notify("another");
            let row = chat.messages.handles.get_untracked()[0];
            chat.messages
                .install_history(vec![local_message(1, "history")]);
            assert!(row.item.try_get_untracked().is_none());
            let replacement = chat.messages.handles.get_untracked()[0];
            assert_ne!(row.key, replacement.key);
        });
    }

    #[test]
    fn repeated_snapshots_keep_handles_and_order() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            let snapshot = openwebide_core::RunSnapshot {
                items: vec![openwebide_core::RunItem::Message(message(
                    7,
                    openwebide_core::Role::User,
                    "ask",
                ))],
                text: "live".into(),
                ..Default::default()
            };
            chat.messages
                .reconcile(|items| crate::conversation::merge_snapshot(items, &snapshot));
            let rows = chat.messages.handles.get_untracked();
            for _ in 0..2 {
                chat.messages
                    .reconcile(|items| crate::conversation::merge_snapshot(items, &snapshot));
                assert_eq!(chat.messages.handles.get_untracked(), rows);
            }
        });
    }

    #[test]
    fn detects_interrupted_users_and_completed_tool_turns() {
        let mut items = vec![ConversationItem::Message(message(7, Role::User, "go"))];
        assert_eq!(
            interrupted_run(&items),
            Some(InterruptedRun {
                anchor_id: 7,
                first_turn: 1
            })
        );
        items.extend([
            interim(8),
            complete_step("a7t1c0"),
            interim(9),
            complete_step("a7t2c0"),
        ]);
        assert_eq!(
            interrupted_run(&items),
            Some(InterruptedRun {
                anchor_id: 7,
                first_turn: 3
            })
        );
        items.push(ConversationItem::Message(message(
            10,
            Role::Assistant,
            "done",
        )));
        assert_eq!(interrupted_run(&items), None);
        assert_eq!(interrupted_run(&[]), None);
    }

    #[test]
    fn incomplete_or_missing_tool_results_offer_a_new_turn_without_replay() {
        let mut items = vec![
            ConversationItem::Message(message(7, Role::User, "go")),
            interim(8),
        ];
        assert_eq!(
            interrupted_run(&items),
            Some(InterruptedRun {
                anchor_id: 7,
                first_turn: 2
            })
        );
        let mut step = complete_step("a7t1c0");
        if let ConversationItem::ToolStep { result, .. } = &mut step {
            *result = None;
        }
        items.push(step);
        assert_eq!(
            interrupted_run(&items),
            Some(InterruptedRun {
                anchor_id: 7,
                first_turn: 2
            })
        );
        if let ConversationItem::ToolStep { id, .. } = &mut items[2] {
            *id = "a7t9c0".into();
        }
        items.push(interim(9));
        assert_eq!(
            interrupted_run(&items),
            Some(InterruptedRun {
                anchor_id: 7,
                first_turn: 11
            })
        );
        cancel_run_prompts(&mut items, 7);
        assert!(
            matches!(&items[2], ConversationItem::ToolStep { result: Some(result), awaiting_permission: false, .. } if result.summary == "cancelled")
        );
    }

    #[test]
    fn detection_is_local_idle_and_dismissal_lasts_across_history_loads() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.messages.set(vec![ConversationItem::Message(message(
                7,
                Role::User,
                "go",
            ))]);
            chat.detect_interrupted_run(false);
            assert_eq!(chat.interrupted_run.get_untracked(), None);
            chat.streaming.set(true);
            chat.detect_interrupted_run(true);
            assert_eq!(chat.interrupted_run.get_untracked(), None);
            chat.streaming.set(false);
            chat.active_run.set(Some((7, "run".into(), 0)));
            chat.detect_interrupted_run(true);
            assert_eq!(chat.interrupted_run.get_untracked(), None);
            chat.active_run.set(None);
            chat.detect_interrupted_run(true);
            assert!(chat.interrupted_run.get_untracked().is_some());
            chat.dismiss_interrupted_run();
            chat.detect_interrupted_run(true);
            assert_eq!(chat.interrupted_run.get_untracked(), None);
        });
    }

    #[test]
    fn interim_completes_placeholder_and_next_delta_starts_reply() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.active_session.set(Some(7));
            chat.apply_event(RunEvent::Message { message: message(90, Role::User, "go") });
            chat.apply_event(RunEvent::Delta { content: "checking".into() });
            chat.apply_event(RunEvent::Interim { message: message(91, Role::Assistant, "checking") });
            chat.apply_event(RunEvent::Delta { content: "finished".into() });
            chat.apply_event(RunEvent::Done { message: message(92, Role::Assistant, "finished") });
            let items = chat.messages.get_untracked();
            assert_eq!(items.len(), 3);
            for (item, (id, text)) in items.iter().zip([(90, "go"), (91, "checking"), (92, "finished")]) {
                assert!(matches!(item, ConversationItem::Message(msg) if msg.id == id && msg.content == text));
            }
        });
    }

    #[test]
    fn persisted_reply_events_reconcile_messages_loaded_from_history() {
        Owner::new().with(|| {
            for (interim, queued_delta) in
                [(true, false), (false, false), (true, true), (false, true)]
            {
                let chat = ChatState::new();
                chat.active_session.set(Some(7));
                let existing = message(91, Role::Assistant, "checking");
                let later = message(92, Role::Assistant, "finished");
                chat.messages.set(vec![
                    ConversationItem::Message(message(90, Role::User, "go")),
                    ConversationItem::Message(existing.clone()),
                    ConversationItem::Message(later.clone()),
                ]);
                let mut incoming = existing;
                incoming.usage = Some(TurnTelemetry {
                    context: None,
                    prompt_tokens: 100,
                    ..Default::default()
                });
                if queued_delta {
                    chat.apply_event(RunEvent::Delta {
                        content: "check".into(),
                    });
                    chat.apply_event(RunEvent::Delta {
                        content: "ing".into(),
                    });
                }
                chat.apply_event(if interim {
                    RunEvent::Interim {
                        message: incoming.clone(),
                    }
                } else {
                    RunEvent::Done {
                        message: incoming.clone(),
                    }
                });
                let items = chat.messages.get_untracked();
                assert_eq!(items.len(), 3);
                assert!(matches!(&items[1], ConversationItem::Message(msg) if msg == &incoming));
                assert!(matches!(&items[2], ConversationItem::Message(msg) if msg == &later));
            }
        });
    }

    #[test]
    fn applies_message_delta_tool_permission_result_and_done_events() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.active_session.set(Some(7));
            chat.streaming_session.set(Some(7));
            chat.apply_event(RunEvent::Message { message: message(90, Role::User, "edit this") });
            chat.apply_event(RunEvent::Delta { content: "working".into() });
            chat.apply_event(RunEvent::ToolCall {
                id: "a90t0c0".into(),
                name: "write_file".into(),
                summary: "draft.txt".into(),
            });
            chat.apply_event(RunEvent::PermissionRequest {
                id: "a90t1c0".into(),
                name: "write_file".into(),
                summary: "draft.txt".into(),
                diff: None,
                note: None,
            });
            chat.apply_event(RunEvent::ToolResult {
                id: "a90t1c0".into(),
                name: "write_file".into(),
                ok: true,
                summary: "wrote draft.txt".into(),
                diff: Some(FileDiff {
                    path: "draft.txt".into(),
                    old: None,
                    new: "done".into(),
                    old_unavailable: false,
                    backup_path: None,
                }),
            });
            chat.apply_event(RunEvent::Done { message: message(91, Role::Assistant, "finished") });

            let items = chat.messages.get_untracked();
            assert_eq!(items.len(), 5);
            assert!(matches!(&items[0], ConversationItem::Message(msg) if msg.id == 90));
            assert!(matches!(&items[1], ConversationItem::Message(msg) if msg.content == "working"));
            assert!(matches!(&items[2], ConversationItem::ToolStep { id, result: None, awaiting_permission: false, .. } if id == "a90t0c0"));
            assert!(matches!(&items[3], ConversationItem::ToolStep { id, result: Some(result), awaiting_permission: false, .. } if id == "a90t1c0" && result.ok));
            assert!(matches!(&items[4], ConversationItem::Message(msg) if msg.id == 91));
            assert_eq!(chat.session_telemetry.get_untracked().tool_calls_count, 1);
        });
    }

    #[test]
    fn project_session_restore_validates_ownership_and_preserves_a_snapshot() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            let session = |id, project_id| ChatSession {
                pinned: false,
                archived: false,
                auto_title: false,
                title_revision: 0,
                id,
                project_id: Some(project_id),
                name: "test".into(),
                connection_id: None,
                system_prompt_id: None,
                user_id: None,
                created_at: id,
            };
            chat.sessions
                .set(vec![session(1, 10), session(2, 10), session(3, 20)]);
            chat.last_sessions.update(|sessions| {
                sessions.insert(10, 1);
            });
            chat.restore_project_session(10);
            assert_eq!(chat.active_session.get_untracked(), Some(1));
            chat.active_session.set(Some(2));
            chat.restore_project_session(10);
            assert_eq!(chat.active_session.get_untracked(), Some(2));
            chat.active_session.set(None);
            chat.last_sessions.update(|sessions| {
                sessions.insert(10, 3);
            });
            chat.restore_project_session(10);
            assert_eq!(chat.active_session.get_untracked(), Some(2));
            chat.active_session.set(None);
            chat.last_sessions.update(|sessions| {
                sessions.insert(10, 99);
            });
            chat.restore_project_session(10);
            assert_eq!(chat.active_session.get_untracked(), Some(2));
            chat.restore_project_session(20);
            assert_eq!(chat.active_session.get_untracked(), Some(3));
            chat.restore_project_session(30);
            assert_eq!(chat.active_session.get_untracked(), None);
        });
    }

    #[test]
    fn cancelled_reasoning_closes_the_trace_and_resets_run_state() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.active_session.set(Some(7));
            chat.current_run_anchor.set(Some(90));
            chat.apply_event(RunEvent::ReasoningDelta {
                content: "checking </think> carefully".into(),
            });
            assert!(chat.reasoning_active.get_untracked());

            chat.apply_event(RunEvent::Cancelled);
            chat.apply_event(RunEvent::Cancelled);

            let items = chat.messages.get_untracked();
            let ConversationItem::Message(message) = &items[0] else {
                panic!("expected the partial assistant message");
            };
            let parsed = openwebide_core::tui::parse_thinking(&message.content);
            assert!(!parsed.is_thinking);
            assert_eq!(
                parsed.thinking.as_deref(),
                Some("checking </think> carefully")
            );
            assert!(parsed.answer.is_empty());
            assert!(!chat.reasoning_active.get_untracked());
            assert_eq!(chat.current_run_anchor.get_untracked(), None);
            assert_eq!(items.len(), 2);
            assert!(matches!(items[1], ConversationItem::Stopped { .. }));
        });
    }

    #[test]
    fn cancelled_event_marks_anchored_prompts_and_adds_one_stop_marker() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.active_session.set(Some(7));
            chat.streaming_session.set(Some(7));
            chat.current_run_anchor.set(Some(90));
            chat.messages.set(vec![ConversationItem::ToolStep {
                timing: None,
                key: 1,
                id: "a90t0c0".into(),
                name: "write_file".into(),
                summary: "draft.txt".into(),
                result: None,
                awaiting_permission: true,
                diff: None,
                note: None,
            }]);

            chat.apply_event(RunEvent::Cancelled);
            chat.apply_event(RunEvent::Cancelled);

            let items = chat.messages.get_untracked();
            assert_eq!(items.len(), 2);
            assert!(matches!(&items[0], ConversationItem::ToolStep { awaiting_permission: false, result: Some(result), .. } if result.summary == "cancelled"));
            assert!(matches!(items[1], ConversationItem::Stopped { .. }));
        });
    }

    #[test]
    fn late_events_from_a_finished_session_do_not_restore_chat_state() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            let effects = chat.apply_event_for_session(
                7,
                RunEvent::ToolResult {
                    id: "a90t0c0".into(),
                    name: "write_file".into(),
                    ok: true,
                    summary: "wrote draft.txt".into(),
                    diff: Some(FileDiff {
                        path: "draft.txt".into(),
                        old: None,
                        new: "done".into(),
                        old_unavailable: false,
                        backup_path: None,
                    }),
                },
            );

            assert!(matches!(effects.as_slice(), [ChatEffect::ToolDiff(_)]));
            assert!(chat.messages.get_untracked().is_empty());
        });
    }

    #[test]
    fn approval_mode_is_scoped_to_each_session() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.set_approval_mode(7, ApprovalMode::AlwaysForSession);

            assert_eq!(
                chat.approval_mode.get_untracked().get(&7),
                Some(&ApprovalMode::AlwaysForSession)
            );
            assert_eq!(chat.approval_mode.get_untracked().get(&8), None);
        });
    }

    #[test]
    fn always_approval_is_reported_as_an_effect_for_its_session() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.active_session.set(Some(7));
            chat.streaming_session.set(Some(7));
            chat.set_approval_mode(7, ApprovalMode::AlwaysForSession);

            let effects = chat.apply_event(RunEvent::PermissionRequest {
                id: "a90t0c0".into(),
                name: "write_file".into(),
                summary: "draft.txt".into(),
                diff: None,
                note: None,
            });

            assert_eq!(
                effects,
                vec![ChatEffect::ApprovePermission {
                    id: "a90t0c0".into()
                }]
            );
            assert!(chat.messages.get_untracked().is_empty());
        });
    }

    #[test]
    fn telemetry_events_update_the_session_counters() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.active_session.set(Some(7));
            chat.apply_event(RunEvent::Telemetry {
                usage: TurnTelemetry {
                    context: None,
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    estimated: false,
                    eval_duration_ms: 1000,
                },
            });

            let telemetry = chat.session_telemetry.get_untracked();
            assert_eq!(telemetry.total_prompt_tokens, 3);
            assert_eq!(telemetry.total_completion_tokens, 2);
        });
    }
    #[test]
    fn inline_thinking_deltas_remain_open_until_the_model_closes_them() {
        let owner = Owner::new();
        owner.with(|| {
            let chat = ChatState::new();
            for content in ["<think>", "r", "</think>answer"] {
                chat.apply_event(RunEvent::Delta { content: content.into() });
            }
            assert!(matches!(&chat.messages.get_untracked()[0], ConversationItem::Message(m) if m.content == "<think>r</think>answer"));
        });
    }
    #[test]
    fn child_progress_is_scoped_and_stopped_tasks_ignore_late_permissions_and_usage() {
        Owner::new().with(|| {
            let chat = ChatState::new();
            chat.active_session.set(Some(7));
            chat.streaming_session.set(Some(7));
            chat.streaming.set(true);
            chat.current_run_anchor.set(Some(1));
            chat.set_approval_mode(7, ApprovalMode::AlwaysForSession);
            let task = openwebide_core::NewAgentTask { description: "Inspect".into(), prompt: "Inspect".into() };
            let mut child = openwebide_agent::tasks::TaskRun::new("a1t1c0.task1".into(), "a1t1c0".into(), &task, 1000);
            let start = RunEvent::Task { update: Box::new(child.started(&task, 1000)) };
            assert!(chat.apply_event_for_session(8, start.clone()).is_empty());
            assert!(chat.messages.get_untracked().is_empty());
            chat.apply_event_for_session(7, start);
            for update in child.observe(openwebide_agent::AgentEvent::Telemetry(TurnTelemetry { prompt_tokens: 10, completion_tokens: 2, ..Default::default() }), 1000) {
                let event = RunEvent::Task { update: Box::new(update) };
                chat.apply_event_for_session(7, event.clone());
                chat.apply_event_for_session(7, event);
            }
            assert_eq!(chat.session_telemetry.get_untracked().total_prompt_tokens, 10);
            assert_eq!(chat.session_telemetry.get_untracked().total_completion_tokens, 2);
            let id = "a1t1c0.task1.a1t1c0";
            let permission = child.observe(openwebide_agent::AgentEvent::PermissionRequest { id: id.into(), name: "write_file".into(), summary: "write".into(), diff: None, note: None }, 1000).pop().unwrap();
            chat.apply_event_for_session(7, RunEvent::Task { update: Box::new(permission.clone()) });
            let handle = chat.messages.handles.get_untracked()[0];
            assert_eq!(handle.permission.get_untracked(), Some((id.into(), "write_file".into())));
            chat.messages.clear_prompt(id, true);
            assert!(handle.permission.get_untracked().is_none());
            chat.cancel_run_prompts(1);
            assert!(matches!(&chat.messages.get_untracked()[0], ConversationItem::Task(task) if task.task.status == openwebide_core::TaskStatus::Cancelled && task.task.timing.unwrap().finished));
            assert!(chat.apply_event_for_session(7, RunEvent::Task { update: Box::new(permission) }).is_empty());
            assert!(handle.permission.get_untracked().is_none());
        });
    }
}
