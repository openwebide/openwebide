use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use futures::channel::oneshot;
use leptos::prelude::*;
use openwebide_core::{BridgeClientMessage, BridgeServerMessage, RunEvent, RunRejectCode};

use crate::{
    backend::Api,
    bridge::{BridgeConn, BridgeStatus},
    conversation::{ConversationItem, merge_snapshot, notice},
    state::chat::ChatState,
};

#[cfg(not(feature = "test-support"))]
const HELLO_WAIT_MS: u32 = 2000;
#[cfg(feature = "test-support")]
const HELLO_WAIT_MS: u32 = 40;

#[derive(Default)]
pub(super) struct RunControls {
    run_id: String,
    pending: Vec<BridgeClientMessage>,
    decisions: HashSet<String>,
    submitted: HashMap<String, u64>,
    sender: Option<oneshot::Sender<bool>>,
}

impl RunControls {
    fn track(&mut self, run_id: &str) {
        if self.run_id != run_id {
            *self = Self {
                run_id: run_id.into(),
                ..Self::default()
            };
        }
    }

    pub(super) fn send(
        &mut self,
        bridge: Option<&BridgeConn>,
        message: BridgeClientMessage,
    ) -> bool {
        let (BridgeClientMessage::RunPermission { run_id, .. }
        | BridgeClientMessage::RunCancel { run_id }) = &message
        else {
            return false;
        };
        self.track(run_id);
        if let BridgeClientMessage::RunPermission { tool_call_id, .. } = &message
            && (self.decisions.contains(tool_call_id)
                || bridge.is_some_and(|bridge| {
                    self.submitted.get(tool_call_id) == Some(&bridge.connection_id())
                }))
        {
            return true;
        }
        if let Some(bridge) = bridge.filter(|bridge| bridge.send(message.clone()).is_ok()) {
            if let BridgeClientMessage::RunPermission { tool_call_id, .. } = message {
                self.submitted.insert(tool_call_id, bridge.connection_id());
            }
            true
        } else {
            if !self.pending.contains(&message) {
                self.pending.push(message);
            }
            false
        }
    }
}

#[derive(Clone, Copy)]
pub struct RunActions {
    pub bridge: RwSignal<Option<BridgeConn>, LocalStorage>,
    pub chat: ChatState,
    pub api: Api,
    pub apply: Callback<(i64, RunEvent, bool)>,
    pub history: RwSignal<Option<(i64, u64)>>,
    controls: StoredValue<RunControls, LocalStorage>,
    listing: StoredValue<Option<(BridgeConn, String)>, LocalStorage>,
    observed: StoredValue<Option<(i64, u64, String, u64)>>,
}

impl RunActions {
    pub(super) fn new(
        bridge: RwSignal<Option<BridgeConn>, LocalStorage>,
        chat: ChatState,
        api: Api,
        apply: Callback<(i64, RunEvent, bool)>,
        controls: StoredValue<RunControls, LocalStorage>,
    ) -> Self {
        Self {
            bridge,
            chat,
            api,
            apply,
            history: RwSignal::new(None),
            controls,
            listing: StoredValue::new_local(None),
            observed: StoredValue::new(None),
        }
    }

    pub(crate) fn control(self, message: BridgeClientMessage) -> bool {
        let mut sent = false;
        self.controls.update_value(|controls| {
            sent = controls.send(self.bridge.get_untracked().as_ref(), message);
        });
        sent
    }

    pub async fn ready(self) -> Option<BridgeConn> {
        let bridge = self.bridge.get_untracked()?;
        for _ in 0..HELLO_WAIT_MS / 20 {
            if bridge.status().get_untracked() != BridgeStatus::Connecting {
                break;
            }
            crate::util::sleep_ms(20).await;
        }
        (bridge.status().get_untracked() == BridgeStatus::Ready { runs: true }).then_some(bridge)
    }

    pub async fn start(self, mut message: BridgeClientMessage) -> bool {
        let bridge = self.ready().await;
        if self
            .chat
            .local_cancel_flag
            .with_value(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
            || self
                .chat
                .abort
                .get_untracked()
                .is_some_and(|controller| controller.signal().aborted())
        {
            return true;
        }
        let Some(bridge) = bridge else {
            return false;
        };
        let BridgeClientMessage::RunStart {
            run_id, session_id, ..
        } = &mut message
        else {
            return false;
        };
        let run_id = run_id.clone();
        let session_id = *session_id;
        let (sender, receiver) = oneshot::channel();
        self.chat
            .active_run
            .set(Some((session_id, run_id.clone(), 0)));
        self.controls.update_value(|controls| {
            controls.track(&run_id);
            controls.sender = Some(sender);
        });
        self.register(bridge.clone(), session_id, run_id.clone());
        if bridge.send(message).is_err() {
            bridge.unregister_run(&run_id);
            self.chat.active_run.set(None);
            return false;
        }
        // A socket loss keeps the run tracked until reconnect and attach finish it.
        receiver.await.unwrap_or(true)
    }

    fn finish(self, bridge: &BridgeConn, run_id: &str) {
        bridge.unregister_run(run_id);
        if self
            .chat
            .active_run
            .get_untracked()
            .is_some_and(|(_, id, _)| id == run_id)
        {
            if let Some((session, id, seq)) = self.chat.active_run.get_untracked() {
                self.observed.set_value(Some((
                    session,
                    self.chat.history_gen.get_value(),
                    id,
                    seq,
                )));
            }
            self.chat.active_run.set(None);
            self.chat.streaming.set(false);
            self.chat.streaming_session.set(None);
            self.chat.current_run_anchor.set(None);
            self.chat.abort.set(None);
        }
    }

    fn register(self, bridge: BridgeConn, session_id: i64, run_id: String) {
        self.controls
            .update_value(|controls| controls.track(&run_id));
        let callback_bridge = bridge.clone();
        let registered_id = run_id.clone();
        let callback: Rc<dyn Fn(BridgeServerMessage)> = Rc::new(move |message| {
            if !self
                .chat
                .active_run
                .get_untracked()
                .is_some_and(|(session, ref id, _)| session == session_id && *id == run_id)
            {
                return;
            }
            let mut ended = false;
            let mut handled = true;
            match message {
                BridgeServerMessage::RunEvent { seq, event, .. } => {
                    let last_seq = self.chat.active_run.get_untracked().unwrap().2;
                    if seq <= last_seq {
                        return;
                    }
                    self.chat
                        .active_run
                        .set(Some((session_id, run_id.clone(), seq)));
                    ended = matches!(
                        event,
                        RunEvent::Done { .. } | RunEvent::Cancelled | RunEvent::Error { .. }
                    );
                    if let RunEvent::ToolCall { id, .. } | RunEvent::ToolResult { id, .. } = &event
                    {
                        self.controls.update_value(|controls| {
                            controls.submitted.remove(id);
                            controls.decisions.insert(id.clone());
                        });
                    }
                    self.apply.run((session_id, event, false));
                }
                BridgeServerMessage::RunSnapshot {
                    session_id: snapshot_session,
                    seq,
                    snapshot,
                    ..
                } => {
                    if snapshot_session != session_id
                        || seq < self.chat.active_run.get_untracked().unwrap().2
                    {
                        return;
                    }
                    self.chat
                        .active_run
                        .set(Some((session_id, run_id.clone(), seq)));
                    if self.chat.active_session.get_untracked() == Some(session_id) {
                        if let Some(anchor) = snapshot.items.iter().find_map(|item| match item {
                            openwebide_core::RunItem::Message(m)
                                if m.role == openwebide_core::Role::User =>
                            {
                                Some(m.id)
                            }
                            _ => None,
                        }) {
                            self.chat.current_run_anchor.set(Some(anchor));
                        }
                        self.chat
                            .reasoning_active
                            .set(!snapshot.reasoning.is_empty() && snapshot.text.is_empty());
                        self.chat
                            .messages
                            .reconcile(|items| merge_snapshot(items, &snapshot));
                        // Rebuild before recording the live turn so repeated snapshots don't add usage twice.
                        let entries = self
                            .chat
                            .messages
                            .snapshot()
                            .into_iter()
                            .filter_map(|item| match item {
                                ConversationItem::Task(task) => {
                                    Some(openwebide_core::ConversationEntry::Task(Box::new(
                                        openwebide_core::TaskHistory {
                                            anchor_message_id: 0,
                                            snapshot: *task,
                                        },
                                    )))
                                }
                                ConversationItem::Message(m) if m.id > 0 => {
                                    Some(openwebide_core::ConversationEntry::Message(m))
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        self.chat.session_telemetry.update(|telemetry| {
                            telemetry.restore_from_conversation(&entries);
                            telemetry.tool_calls_count = self
                                .chat
                                .messages
                                .snapshot()
                                .iter()
                                .map(|item| match item {
                                    ConversationItem::ToolStep { .. } => 1,
                                    ConversationItem::Task(task) => task.total_tools(),
                                    _ => 0,
                                })
                                .sum();
                            if let Some(usage) = snapshot.telemetry
                                && snapshot.telemetry_after_message_id
                                    == entries.iter().rev().find_map(|entry| match entry {
                                        openwebide_core::ConversationEntry::Message(m) => {
                                            Some(m.id)
                                        }
                                        openwebide_core::ConversationEntry::ToolStep(_)
                                        | openwebide_core::ConversationEntry::Task(_) => None,
                                    })
                            {
                                telemetry.record_turn(&usage);
                            }
                        });
                    }
                    for item in &snapshot.items {
                        if let openwebide_core::RunItem::Step(step) = item {
                            if !step.awaiting_permission || step.result.is_some() {
                                self.controls.update_value(|controls| {
                                    controls.submitted.remove(&step.id);
                                    controls.decisions.insert(step.id.clone());
                                });
                            }
                            if let Some(result) = &step.result {
                                self.apply.run((
                                    session_id,
                                    RunEvent::ToolResult {
                                        id: step.id.clone(),
                                        name: step.name.clone(),
                                        ok: result.ok,
                                        summary: result.summary.clone(),
                                        diff: result.diff.clone(),
                                    },
                                    true,
                                ));
                            } else if self.chat.active_session.get_untracked() == Some(session_id)
                                && step.awaiting_permission
                                && !self.controls.with_value(|controls| {
                                    controls.submitted.contains_key(&step.id)
                                        || controls.decisions.contains(&step.id)
                                })
                                && self
                                    .chat
                                    .approval_mode
                                    .get_untracked()
                                    .get(&session_id)
                                    .copied()
                                    .unwrap_or_default()
                                    .auto_approves(&step.name)
                            {
                                self.apply.run((
                                    session_id,
                                    RunEvent::PermissionRequest {
                                        id: step.id.clone(),
                                        name: step.name.clone(),
                                        summary: step.summary.clone(),
                                        diff: step.diff.as_deref().cloned(),
                                        note: step.note.clone(),
                                    },
                                    true,
                                ));
                            }
                        }
                    }
                    // A snapshot can be queued ahead of a decision on this socket.
                    // A new socket may restore an approval whose send was lost.
                    let connection_id = callback_bridge.connection_id();
                    let decisions = self.controls.with_value(|controls| {
                        let mut decisions = controls.decisions.clone();
                        decisions.extend(
                            controls
                                .submitted
                                .iter()
                                .filter(|(_, socket)| **socket == connection_id)
                                .map(|(id, _)| id.clone()),
                        );
                        decisions
                    });
                    for step in decisions {
                        self.clear_prompt(&step);
                    }
                    if let Some(event) = snapshot.finished {
                        ended = true;
                        self.apply.run((session_id, event, true));
                    }
                }
                BridgeServerMessage::RunRejected { code, message, .. } => {
                    ended = true;
                    match code {
                        RunRejectCode::ProjectUnavailable => {
                            self.chat.notice.set(Some(format!("Bridge can't see this project ({message}); running through the server instead.")));
                            handled = false;
                        }
                        RunRejectCode::Unauthorized | RunRejectCode::Unavailable => handled = false,
                        RunRejectCode::Busy | RunRejectCode::PlanFailed => {
                            self.chat.error.set(Some(message));
                        }
                    }
                }
                BridgeServerMessage::Error { message, .. } => {
                    ended = true;
                    if message == "run not found" {
                        self.recover_interrupted(session_id);
                    } else {
                        ended = false;
                        self.chat.error.set(Some(message));
                    }
                }
                _ => {}
            }
            if ended {
                if handled {
                    self.finish(&callback_bridge, &run_id);
                } else {
                    callback_bridge.unregister_run(&run_id);
                    self.chat.active_run.set(None);
                }
                self.controls.update_value(|controls| {
                    if let Some(sender) = controls.sender.take() {
                        let _ = sender.send(handled);
                    }
                    controls.pending.clear();
                });
            }
        });
        bridge.register_run(registered_id, callback);
    }

    fn recover_interrupted(self, session_id: i64) {
        let generation = self.chat.history_gen.get_value();
        let anchor = self.chat.current_run_anchor.get_untracked();
        leptos::task::spawn_local(async move {
            let result = self
                .api
                .with_value(Clone::clone)
                .list_messages(session_id)
                .await;
            if generation != self.chat.history_gen.get_value()
                || self.chat.active_session.get_untracked() != Some(session_id)
                || self.chat.streaming.get_untracked()
            {
                return;
            }
            match result {
                Ok(entries) => {
                    let anchor = anchor.or_else(|| {
                        entries.iter().rev().find_map(|entry| match entry {
                            openwebide_core::ConversationEntry::Message(m)
                                if m.role == openwebide_core::Role::User =>
                            {
                                Some(m.id)
                            }
                            _ => None,
                        })
                    });
                    let has_reply = entries.iter().any(|entry| matches!(entry, openwebide_core::ConversationEntry::Message(m) if m.role == openwebide_core::Role::Assistant && anchor.is_some_and(|id| m.id > id)));
                    self.chat
                        .session_telemetry
                        .update(|telemetry| telemetry.restore_from_conversation(&entries));
                    self.chat
                        .messages
                        .install_history(super::chat::history_items(entries));
                    if !has_reply {
                        self.chat
                            .messages
                            .reconcile(|items| items.push(notice("The run was interrupted.")));
                    }
                }
                Err(error) => self.chat.error.set(Some(error)),
            }
        });
    }

    pub async fn discover(self, session_id: i64, generation: u64) {
        let Some(bridge) = self.bridge.get_untracked() else {
            return;
        };
        if bridge.status().get_untracked() != (BridgeStatus::Ready { runs: true }) {
            return;
        }
        if self.chat.history_gen.get_value() != generation
            || self.chat.active_session.get_untracked() != Some(session_id)
        {
            return;
        }
        if let Some((session, run_id, _)) = self.chat.active_run.get_untracked() {
            if session == session_id {
                let _ = bridge.send(BridgeClientMessage::RunAttach {
                    run_id,
                    last_seq: None,
                });
            }
            return;
        }
        self.listing.update_value(|previous| {
            if let Some((connection, id)) = previous.take() {
                connection.unregister_run(&id);
            }
        });
        let listener = format!("list-{session_id}-{generation}");
        self.listing
            .set_value(Some((bridge.clone(), listener.clone())));
        let callback_bridge = bridge.clone();
        let callback_listener = listener.clone();
        bridge.register_run(
            listener,
            Rc::new(move |message| {
                if let BridgeServerMessage::Runs {
                    session_id: session,
                    runs,
                } = message
                {
                    if session != session_id {
                        return;
                    }
                    callback_bridge.unregister_run(&callback_listener);
                    if self.chat.history_gen.get_value() != generation
                        || self.chat.active_session.get_untracked() != Some(session_id)
                        || self.chat.streaming.get_untracked()
                    {
                        return;
                    }
                    let host_goal = self
                        .chat
                        .goal
                        .with_untracked(|goal| goal.as_ref().is_some_and(|goal| goal.worker));
                    let newest = runs
                        .into_iter()
                        .filter(|run| {
                            run.running || (host_goal && run.run_id.starts_with("scheduled-"))
                        })
                        .max_by_key(|run| {
                            (
                                run.running,
                                run.started_at,
                                run.run_id
                                    .strip_prefix("scheduled-")
                                    .and_then(|id| id.parse::<i64>().ok())
                                    .unwrap_or(0),
                            )
                        });
                    if let Some(run) = newest {
                        if self.observed.get_value()
                            == Some((session_id, generation, run.run_id.clone(), run.seq))
                        {
                            return;
                        }
                        self.chat.streaming.set(true);
                        self.chat.streaming_is_local.set(false);
                        self.chat.streaming_session.set(Some(session_id));
                        self.chat
                            .active_run
                            .set(Some((session_id, run.run_id.clone(), 0)));
                        self.register(callback_bridge.clone(), session_id, run.run_id.clone());
                        let _ = callback_bridge.send(BridgeClientMessage::RunAttach {
                            run_id: run.run_id,
                            last_seq: None,
                        });
                    }
                }
            }),
        );
        if bridge
            .send(BridgeClientMessage::RunList { session_id })
            .is_err()
        {
            bridge.unregister_run(&format!("list-{session_id}-{generation}"));
        }
    }

    fn clear_prompt(self, step: &str) {
        self.chat.messages.clear_prompt(step, false);
    }

    pub fn install_reconnect(self) {
        // Discover host-owned turns even when this browser did not launch them.
        let timer = set_interval_with_handle(
            move || {
                if self.chat.streaming.get_untracked()
                    || !self.chat.goal.with_untracked(|goal| {
                        goal.as_ref().is_some_and(|goal| {
                            goal.worker && goal.status == openwebide_core::GoalStatus::Active
                        })
                    })
                {
                    return;
                }
                if let Some((session, generation)) = self.history.get_untracked() {
                    leptos::task::spawn_local(async move {
                        self.discover(session, generation).await;
                    });
                }
            },
            std::time::Duration::from_secs(1),
        )
        .ok();
        on_cleanup(move || {
            if let Some(timer) = timer {
                timer.clear();
            }
        });

        let previous_history = StoredValue::new(None);
        let tracked_run = Memo::new(move |_| {
            self.chat
                .active_run
                .get()
                .map(|(session, id, _)| (session, id))
        });
        Effect::new(move |_| {
            let history = self.history.get();
            self.chat.goal.track();
            tracked_run.get();
            let Some(bridge) = self.bridge.get() else {
                return;
            };
            if bridge.status().get() != (BridgeStatus::Ready { runs: true }) {
                return;
            }
            let history_changed = previous_history.get_value() != history;
            previous_history.set_value(history);
            if let Some((session_id, run_id, last_seq)) = self.chat.active_run.get_untracked() {
                let last_seq = if history_changed && history.is_some_and(|(id, _)| id == session_id)
                {
                    None
                } else {
                    Some(last_seq)
                };
                self.register(bridge.clone(), session_id, run_id.clone());
                let _ = bridge.send(BridgeClientMessage::RunAttach {
                    run_id: run_id.clone(),
                    last_seq,
                });
                let pending = self
                    .controls
                    .with_value(|controls| controls.pending.clone());
                for message in pending {
                    let step = match &message {
                        BridgeClientMessage::RunPermission { tool_call_id, .. } => {
                            Some(tool_call_id.clone())
                        }
                        _ => None,
                    };
                    let mut sent = false;
                    self.controls.update_value(|controls| {
                        sent = controls.send(Some(&bridge), message.clone());
                        if sent {
                            controls.pending.retain(|queued| queued != &message);
                        }
                    });
                    if sent && let Some(step) = step {
                        self.clear_prompt(&step);
                    }
                }
            } else if let Some((session_id, generation)) = history {
                leptos::task::spawn_local(async move {
                    self.discover(session_id, generation).await;
                });
            }
        });
    }
}
