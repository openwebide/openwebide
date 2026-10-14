use std::sync::atomic::Ordering;

use crate::conversation::{ConversationItem, next_item_nonce};
use crate::state::{
    chat::{ChatEffect, ChatState, InterruptedRun},
    git::GitState,
    projects::ProjectsState,
    settings::SettingsState,
    slash::{SlashAction, dispatch as dispatch_slash},
    ui::{ConfirmRequest, PromptRequest, UiState},
    workspace::WorkspaceState,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use openwebide_agent::policy::ApprovalMode;
use openwebide_core::{
    ConversationEntry, GitCommitRequest,
    tui::{DEFAULT_CONTEXT_LIMIT, SlashCommand},
};
use web_sys::AbortController;

use crate::{backend::Api, components::ToolStepResult};

pub struct ChatActionContext {
    pub api: Api,
    pub chat: ChatState,
    pub projects: ProjectsState,
    pub workspace: WorkspaceState,
    pub settings: SettingsState,
    pub ui: UiState,
    pub git: GitState,
    pub bridge: RwSignal<Option<crate::bridge::BridgeConn>, LocalStorage>,
    pub request_open: Callback<String>,
    pub refresh_git: Callback<()>,
    pub on_sync_click: Callback<()>,
    pub on_commit: Callback<GitCommitRequest>,
    pub on_checkout: Callback<(String, bool)>,
}

/// App-owned actions for sending chat, handling approvals, choosing models,
/// and executing slash-command intents.
type RunIntent = (
    Option<InterruptedRun>,
    Option<openwebide_core::QueuedPrompt>,
    Option<String>,
    Option<String>,
);

pub struct ChatActions {
    pub conversation: super::conversation::ConversationActions,
    pub queue: super::prompt_queue::PromptQueueActions,
    pub send: Callback<()>,
    pub send_prompt: Callback<String>,
    pub rewind: Callback<i64>,
    pub resume_run: Callback<()>,
    pub stop: Callback<()>,
    pub permission: Callback<(String, bool)>,
    pub permission_always: Callback<String>,
    pub select_model: Callback<Option<String>>,
    pub select_connection_model: Callback<(i64, String)>,
    pub slash_command: Callback<SlashCommand>,
    pub on_select_session: Callback<i64>,
    pub on_new_session: Callback<()>,
    pub on_rename_session: Callback<i64>,
    pub on_delete_session: Callback<i64>,
}

impl ChatActions {
    pub fn new(context: ChatActionContext) -> Self {
        let ChatActionContext {
            api,
            chat,
            projects,
            workspace,
            settings,
            ui,
            git,
            bridge,
            request_open,
            refresh_git,
            on_sync_click,
            on_commit,
            on_checkout,
        } = context;
        let todos = super::todos::install(api, chat, projects);
        let project_git = expect_context::<crate::project_git::ProjectGit>();
        let project_host = expect_context::<crate::project_host::ProjectHost>();
        let run_controls = StoredValue::new_local(super::runs::RunControls::default());
        let project_runs_slot = StoredValue::new(None::<crate::project_runs::ProjectRuns>);
        let rewind = super::rewind::actions(
            api,
            chat,
            projects,
            workspace,
            ui,
            request_open,
            refresh_git,
        );
        let stop = Callback::new(move |()| {
            if let Some(facade) = project_runs_slot.get_value() {
                facade.stop();
            }
        });
        let permission = Callback::new(move |(id, approved): (String, bool)| {
            if let Some(facade) = project_runs_slot.get_value() {
                facade.permission(id, approved);
            }
        });

        let permission_always = {
            let auth = expect_context::<crate::state::auth::AuthState>();
            Callback::new(move |tool_call_id: String| {
                let Some(session) = chat.streaming_session.get_untracked() else {
                    return;
                };
                let generation = auth.generation.get_untracked();
                spawn_local(async move {
                    let mode = ApprovalMode::AutoAcceptEdits;
                    match super::approvals::save_session_mode(api, session, mode).await {
                        Ok(()) => {
                            if auth.generation.try_get_untracked() != Some(generation)
                                || chat.streaming_session.try_get_untracked() != Some(Some(session))
                            {
                                return;
                            }
                            chat.set_approval_mode(session, mode);
                            permission.run((tool_call_id, true));
                        }
                        Err(error) => ui.notify(format!("Could not save approval mode: {error}")),
                    }
                });
            })
        };

        let notifications = crate::notifications::RunNotifications::from_context();
        let notification_layout = use_context::<crate::state::layout::LayoutState>();
        let notification_auth = expect_context::<crate::state::auth::AuthState>();
        let refresh_pending = super::workspace::pending_refresh(api, projects, workspace, ui);
        let apply_stream_event = Callback::new(
            move |(session_id, event, replayed): (i64, openwebide_core::RunEvent, bool)| {
                if matches!(
                    &event,
                    openwebide_core::RunEvent::Error { .. } | openwebide_core::RunEvent::Cancelled
                ) && chat.queue_steering.get_untracked() != Some(session_id)
                {
                    chat.queue_running.update(|sessions| {
                        sessions.remove(&session_id);
                    });
                }
                if matches!(&event, openwebide_core::RunEvent::Message { message } if message.role == openwebide_core::Role::User)
                    && let Some((queued_session, key)) = chat.queue_delivering.get_untracked()
                    && queued_session == session_id
                {
                    chat.queue_epoch.update_value(|epoch| *epoch += 1);
                    chat.queued_prompts
                        .update(|entries| entries.retain(|entry| entry.key() != key));
                    chat.queue_delivering.set(None);
                }
                notifications.event(
                    crate::notifications::NotificationContext {
                        chat_visible: notification_layout
                            .is_none_or(|layout| layout.visible_panels.get_untracked().chat),
                        auth: notification_auth,
                        chat,
                        settings,
                        ui,
                    },
                    session_id,
                    &event,
                    replayed,
                );
                if matches!(&event, openwebide_core::RunEvent::ToolResult { name, .. } if name == "todo_write")
                    || matches!(&event, openwebide_core::RunEvent::Done { .. })
                {
                    todos.refresh.run(session_id);
                }
                let run_project_id = chat
                    .sessions
                    .get_untracked()
                    .into_iter()
                    .find(|session| session.id == session_id)
                    .and_then(|session| session.project_id);
                let write_id = match &event {
                    openwebide_core::RunEvent::ToolResult { id, ok: true, .. } => Some(id.clone()),
                    _ => None,
                };
                for effect in chat.apply_event_for_session(session_id, event) {
                    match effect {
                        ChatEffect::ApprovePermission { id } => {
                            permission.run((id, true));
                        }
                        ChatEffect::ToolDiff(diff) => {
                            let Some(id) = &write_id else {
                                continue;
                            };
                            if let Some(project_id) = run_project_id {
                                let mut first_write = false;
                                workspace.counted_agent_writes.update(|counted| {
                                    first_write = counted.insert((session_id, id.clone()));
                                });
                                if !replayed || first_write {
                                    workspace.agent_writes.update(|writes| {
                                        *writes
                                            .entry((project_id, diff.path.clone()))
                                            .or_default() += 1;
                                    });
                                }
                                refresh_pending.run(project_id);
                            }
                            if chat.active_session.get_untracked() != Some(session_id) {
                                continue;
                            }
                            if projects.active_project.get_untracked() == run_project_id {
                                if !workspace.dirty.get_untracked() {
                                    request_open.run(diff.path.clone());
                                } else {
                                    chat.notify(format!(
                                        "Agent edited `{}`; review it with `/diff {}`.",
                                        diff.path, diff.path
                                    ));
                                }
                            }
                        }
                    }
                }
            },
        );
        let runs =
            super::runs::RunActions::new(bridge, chat, api, apply_stream_event, run_controls);
        runs.install_reconnect();
        let project_runs = crate::project_runs::ProjectRuns::new(
            api,
            chat,
            projects,
            settings,
            project_host,
            runs,
            bridge,
        );
        project_runs_slot.set_value(Some(project_runs));

        let start = {
            let reviews = use_context::<crate::state::reviews::ReviewsState>();
            let auth = expect_context::<crate::state::auth::AuthState>();
            let local_cancel = chat.local_cancel_flag.get_value();
            let local_permissions = chat.local_permissions.get_value();
            Callback::new(move |intent: RunIntent| {
                let (resume, queued, shortcut, goal_objective) = intent;
                let content = shortcut.clone().unwrap_or_else(|| {
                    queued.as_ref().map_or_else(
                        || chat.draft.with(|draft| draft.trim().to_string()),
                        |prompt| prompt.content.clone(),
                    )
                });
                let images = if queued.is_some() || shortcut.is_some() {
                    Vec::new()
                } else {
                    chat.prompt_images.get_untracked()
                };
                if (resume.is_none() && content.is_empty() && images.is_empty())
                    || chat.reading_images.get_untracked()
                    || chat.streaming.get()
                    || chat.compacting.get()
                    || chat.goal_busy.get()
                    || chat.rewinding.get()
                    || reviews.is_some_and(|state| state.busy.get().is_some())
                    || chat.connection_changing.get()
                    || chat.creating_session.get()
                    || chat.branching.get_untracked()
                    || chat.queue_busy.get_untracked()
                {
                    return;
                }
                if queued.as_ref().is_some_and(|queued| {
                    Some(queued.session_id) != chat.active_session.get_untracked()
                }) {
                    return;
                }
                if let Some(queued) = &queued {
                    chat.queue_delivering
                        .set(Some((queued.session_id, queued.key())));
                }
                if resume.is_none() {
                    chat.interrupted_run.set(None);
                }
                let editing = if resume.is_none() && queued.is_none() && shortcut.is_none() {
                    chat.prompt_edit.get_untracked()
                } else {
                    None
                };
                let editor_snapshot = if resume.is_none() && queued.is_none() && shortcut.is_none()
                {
                    chat.active_editor_context.get_untracked()
                } else {
                    None
                };
                let original_context = editing
                    .and_then(|(_, id)| chat.messages.message(id))
                    .and_then(|row| {
                        row.item.with_untracked(|item| match item {
                            ConversationItem::Message(message) => {
                                let original =
                                    openwebide_core::PromptContent::decode(&message.content);
                                openwebide_core::tui::extract_editor_context_prelude(&original.text)
                                    .0
                                    .map(str::to_owned)
                            }
                            _ => None,
                        })
                    });
                local_cancel.store(false, Ordering::Relaxed);
                chat.streaming_is_local
                    .set(project_runs.browser_owned(projects.active_project.get_untracked()));
                if let Ok(mut map) = local_permissions.lock() {
                    map.clear();
                }
                // Clear the draft and mark streaming synchronously so a second
                // send can't fire while the session is (possibly) created.
                if resume.is_none() && queued.is_none() && shortcut.is_none() {
                    chat.draft.set(String::new());
                    chat.prompt_images.set(Vec::new());
                    chat.interrupted_run.set(None);
                }
                chat.streaming.set(true);
                chat.error.set(None);
                chat.notice.set(None);
                let generation = auth.generation.get_untracked();
                chat.send_generation.update_value(|ticket| *ticket += 1);
                let ticket = chat.send_generation.get_value();
                let project = projects.active_project.get_untracked();
                let active_session = chat.active_session.get_untracked();
                let expected_session = std::rc::Rc::new(std::cell::Cell::new(active_session));
                let expected = expected_session.clone();
                let same_run = move || {
                    auth.generation.try_get_untracked() == Some(generation)
                        && chat.send_generation.try_get_value() == Some(ticket)
                };
                let current = move || {
                    same_run()
                        && projects.active_project.try_get_untracked() == Some(project)
                        && chat.active_session.try_get_untracked() == Some(expected.get())
                };
                let local_cancel = local_cancel.clone();
                let prompt = queued.as_ref().map_or_else(
                    || openwebide_core::PromptContent {
                        text: content,
                        images,
                        ..Default::default()
                    },
                    |queued| openwebide_core::PromptContent::decode(&queued.content),
                );
                // Show a fresh prompt immediately; the server's saved copy replaces
                // it in place, and failures before that roll it back.
                if resume.is_none()
                    && queued.is_none()
                    && shortcut.is_none()
                    && goal_objective.is_none()
                    && editing.is_none()
                {
                    let shown = prompt.encode().unwrap_or_else(|_| prompt.text.clone());
                    chat.messages
                        .push_optimistic_user(active_session.unwrap_or_default(), shown);
                }
                let queued_key = queued.as_ref().map(openwebide_core::QueuedPrompt::key);
                spawn_local(async move {
                    let mut content = if let Some(queued) = &queued {
                        queued.content.clone()
                    } else if resume.is_none() {
                        match crate::prompt::prepare(
                            api,
                            projects,
                            project_git,
                            prompt.clone(),
                            || current() && !local_cancel.load(Ordering::Relaxed),
                        )
                        .await
                        {
                            Ok(content) => content,
                            Err(error) => {
                                if current() {
                                    if queued.is_none() {
                                        crate::prompt::restore_draft(chat, &prompt);
                                    }
                                    if !local_cancel.load(Ordering::Relaxed) {
                                        chat.error.set(Some(error));
                                    }
                                }
                                if same_run() {
                                    chat.streaming.set(false);
                                }
                                return;
                            }
                        }
                    } else {
                        prompt.text.clone()
                    };
                    if editor_snapshot.is_none()
                        && let Some(context) = original_context
                    {
                        let mut edited = openwebide_core::PromptContent::decode(&content);
                        edited.text = format!(
                            "<active_editor_context>\n{context}</active_editor_context>\n\n{}",
                            edited.text
                        );
                        content = match edited.encode() {
                            Ok(content) => content,
                            Err(error) => {
                                if current() {
                                    crate::prompt::restore_draft(chat, &prompt);
                                    chat.error.set(Some(error));
                                }
                                if same_run() {
                                    chat.streaming.set(false);
                                }
                                return;
                            }
                        };
                    }
                    // Use the active session, or create one named from the first
                    // prompt when a fresh chat has no session selected.
                    let session_id = if let Some((source, selected)) = editing {
                        let result = api
                            .with_value(Clone::clone)
                            .fork_session(source, selected)
                            .await;
                        if !current() || local_cancel.load(Ordering::Relaxed) {
                            if current() {
                                crate::prompt::restore_draft(chat, &prompt);
                            }
                            if same_run() {
                                chat.streaming.set(false);
                            }
                            return;
                        }
                        match result {
                            Ok(branch) => {
                                let next_draft = chat.draft.get_untracked();
                                let next_images = chat.prompt_images.get_untracked();
                                expected_session.set(Some(branch.session.id));
                                super::conversation::install_branch(chat, source, &branch);
                                crate::util::sleep_ms(0).await;
                                if !current() || local_cancel.load(Ordering::Relaxed) {
                                    if same_run() {
                                        chat.streaming.set(false);
                                    }
                                    return;
                                }
                                if chat.draft.get_untracked() == next_draft
                                    && chat.prompt_images.with_untracked(Vec::is_empty)
                                {
                                    chat.prompt_images.set(next_images);
                                }
                                branch.session.id
                            }
                            Err(error) => {
                                crate::prompt::restore_draft(chat, &prompt);
                                chat.error
                                    .set(Some(format!("Could not send this edit: {error}")));
                                chat.streaming.set(false);
                                return;
                            }
                        }
                    } else {
                        match active_session {
                            Some(id) => id,
                            None => {
                                let project_id = project;
                                let name = derive_session_name(&content);
                                let connection_id = chat
                                    .draft_connection
                                    .get()
                                    .or_else(|| {
                                        settings
                                            .model_setup
                                            .get()
                                            .defaults
                                            .primary
                                            .map(|selection| selection.server_id)
                                    })
                                    .or(settings.default_connection.get())
                                    .or_else(|| {
                                        settings
                                            .connections
                                            .get()
                                            .into_iter()
                                            .find(|connection| connection.enabled)
                                            .map(|connection| connection.id)
                                    });
                                let prompt_id = settings.default_prompt.get();
                                match api
                                    .with_value(Clone::clone)
                                    .create_session(&name, connection_id, prompt_id, project_id)
                                    .await
                                {
                                    Ok(session) => {
                                        if !current() || local_cancel.load(Ordering::Relaxed) {
                                            if same_run() {
                                                chat.streaming.set(false);
                                                if current() && queued.is_none() {
                                                    crate::prompt::restore_draft(chat, &prompt);
                                                }
                                            }
                                            return;
                                        }
                                        let mode = chat.draft_approval_mode.get_untracked();
                                        if let Err(error) = super::approvals::save_session_mode(
                                            api, session.id, mode,
                                        )
                                        .await
                                        {
                                            if current() {
                                                chat.error.set(Some(error));
                                                if queued.is_none() {
                                                    crate::prompt::restore_draft(chat, &prompt);
                                                }
                                            }
                                            if same_run() {
                                                chat.streaming.set(false);
                                            }
                                            return;
                                        }
                                        if !current() || local_cancel.load(Ordering::Relaxed) {
                                            if same_run() {
                                                chat.streaming.set(false);
                                                if current() && queued.is_none() {
                                                    crate::prompt::restore_draft(chat, &prompt);
                                                }
                                            }
                                            return;
                                        }
                                        chat.set_approval_mode(session.id, mode);
                                        chat.sessions
                                            .update(|sessions| sessions.push(session.clone()));
                                        chat.skip_history_load.set_value(Some(session.id));
                                        expected_session.set(Some(session.id));
                                        chat.active_session.set(Some(session.id));
                                        if let Some(model) = chat.selected_model.get() {
                                            chat.session_model.update(|models| {
                                                let _ = models.insert(session.id, Some(model));
                                            });
                                        }
                                        session.id
                                    }
                                    Err(error) => {
                                        if current() {
                                            chat.error.set(Some(error));
                                            if queued.is_none() {
                                                crate::prompt::restore_draft(chat, &prompt);
                                            }
                                            chat.streaming.set(false);
                                        }
                                        return;
                                    }
                                }
                            }
                        }
                    };
                    if !current() || local_cancel.load(Ordering::Relaxed) {
                        if current() && queued.is_none() {
                            crate::prompt::restore_draft(chat, &prompt);
                        }
                        if same_run() {
                            chat.streaming.set(false);
                        }
                        return;
                    }
                    if let Some(objective) = goal_objective {
                        let host = project_host;
                        let binding = match host.background_binding(project, current.clone()).await
                        {
                            Ok(binding) => binding,
                            Err(error) => {
                                if current() {
                                    chat.goal_error.set(Some(error));
                                    chat.streaming.set(false);
                                }
                                return;
                            }
                        };
                        let revision = chat
                            .goal
                            .with_untracked(|goal| goal.as_ref().map_or(0, |goal| goal.revision));
                        match api
                            .with_value(Clone::clone)
                            .dispatch_goal(
                                session_id,
                                revision,
                                &openwebide_core::GoalCommand::Start { objective },
                                binding.as_ref(),
                            )
                            .await
                        {
                            Ok(goal) if current() => chat.goal.set(Some(goal)),
                            Ok(_) => {
                                if same_run() {
                                    chat.streaming.set(false);
                                }
                                return;
                            }
                            Err(error) => {
                                if current() {
                                    chat.goal_error.set(Some(error));
                                    chat.streaming.set(false);
                                }
                                return;
                            }
                        }
                        if same_run() {
                            chat.streaming.set(false);
                        }
                        return;
                    }
                    let model = chat
                        .session_model
                        .get()
                        .get(&session_id)
                        .cloned()
                        .flatten()
                        .or_else(|| chat.selected_model.get());
                    let Ok(controller) = AbortController::new() else {
                        if current() && queued.is_none() {
                            crate::prompt::restore_draft(chat, &prompt);
                        }
                        chat.streaming.set(false);
                        return;
                    };
                    chat.abort.set(Some(controller.clone()));
                    chat.streaming_session.set(Some(session_id));

                    let prompt_saved = std::rc::Rc::new(std::cell::Cell::new(false));
                    let saved = prompt_saved.clone();
                    let on_event = move |event| {
                        if matches!(&event, openwebide_core::RunEvent::Message { message } if message.role == openwebide_core::Role::User)
                        {
                            saved.set(true);
                        }
                        if resume.is_some()
                            && matches!(event, openwebide_core::RunEvent::Done { .. })
                            && chat.active_session.get_untracked() == Some(session_id)
                        {
                            chat.interrupted_run.set(None);
                        }
                        apply_stream_event.run((session_id, event, false));
                    };

                    if resume.is_none()
                        && queued.is_none()
                        && shortcut.is_none()
                        && chat.active_editor_context.get_untracked() == editor_snapshot
                    {
                        chat.active_editor_context.set(None);
                    }
                    let editor_context = editor_snapshot;

                    let result = project_runs
                        .run(
                            crate::project_runs::RunInput {
                                session: session_id,
                                content,
                                model,
                                editor: editor_context,
                                controller: controller.clone(),
                                resume,
                                queued_prompt: queued_key,
                            },
                            on_event,
                        )
                        .await;
                    if let Err(error) = result
                        && !controller.signal().aborted()
                        && current()
                    {
                        if resume.is_none() && !prompt_saved.get() && queued.is_none() {
                            crate::prompt::restore_draft(chat, &prompt);
                        }
                        chat.queue_running.update(|sessions| {
                            sessions.remove(&session_id);
                        });
                        chat.error.set(Some(error));
                    }

                    if same_run()
                        && chat
                            .abort
                            .get_untracked()
                            .as_ref()
                            .is_none_or(|active| active == &controller)
                    {
                        chat.streaming.set(false);
                        if let Some(key) = queued_key
                            && chat.queue_delivering.get_untracked() == Some((session_id, key))
                        {
                            chat.queue_delivering.set(None);
                            chat.queue_running.update(|sessions| {
                                sessions.remove(&session_id);
                            });
                        }
                        chat.abort.set(None);
                        chat.streaming_session.set(None);
                        chat.current_run_anchor.set(None);
                    }
                });
            })
        };

        let queue = super::prompt_queue::actions(
            api,
            chat,
            projects,
            project_git,
            ui,
            Callback::new(move |prompt| start.run((None, Some(prompt), None, None))),
            stop,
        );
        let stop = Callback::new(move |()| {
            if chat.compacting.get_untracked() {
                chat.compact_epoch.update_value(|epoch| *epoch += 1);
                let epoch = chat.compact_epoch.get_value();
                let project = projects.active_project.get_untracked();
                if let Some(session) = chat.active_session.get_untracked() {
                    spawn_local(async move {
                        let result = api.with_value(Clone::clone).cancel_session(session).await;
                        if chat.compact_epoch.try_get_value() != Some(epoch)
                            || projects.active_project.try_get_untracked() != Some(project)
                            || chat.active_session.try_get_untracked() != Some(Some(session))
                        {
                            return;
                        }
                        chat.compacting.set(false);
                        if let Err(error) = result {
                            chat.notify(format!("Could not stop compaction: {error}"));
                        }
                    });
                } else {
                    chat.compacting.set(false);
                }
                return;
            }
            chat.queue_running.set(Default::default());
            chat.queue_steering.set(None);
            stop.run(());
        });
        let auth = expect_context::<crate::state::auth::AuthState>();
        let select_model = Callback::new(move |model: Option<String>| {
            chat.selected_model.set(model.clone());
            if let Some(session_id) = chat.active_session.get() {
                chat.session_model.update(|models| {
                    let _ = models.insert(session_id, model.clone());
                });
                let connection = chat.sessions.with_untracked(|sessions| {
                    sessions
                        .iter()
                        .find(|entry| entry.id == session_id)
                        .and_then(|entry| entry.connection_id)
                });
                let account = auth.generation.get_untracked();
                spawn_local(async move {
                    if auth.generation.try_get_untracked() == Some(account) {
                        let _ = api
                            .with_value(Clone::clone)
                            .set_setting(
                                &format!("session_model_{session_id}"),
                                &serde_json::json!({"connection_id":connection,"model":model})
                                    .to_string(),
                            )
                            .await;
                    }
                });
            }
        });

        let auth = expect_context::<crate::state::auth::AuthState>();
        let select_connection_model = Callback::new(
            move |(connection_id, model): (i64, String)| {
                if chat.streaming.get_untracked()
                    || chat.connection_changing.get_untracked()
                    || !settings.connections.with_untracked(|connections| {
                        connections
                            .iter()
                            .any(|connection| connection.id == connection_id && connection.enabled)
                    })
                {
                    return;
                }
                let Some(session_id) = chat.active_session.get_untracked() else {
                    chat.draft_connection.set(Some(connection_id));
                    select_model.run(Some(model));
                    return;
                };
                if chat.sessions.with_untracked(|sessions| {
                    sessions.iter().any(|session| {
                        session.id == session_id && session.connection_id == Some(connection_id)
                    })
                }) {
                    select_model.run(Some(model));
                    return;
                }
                let generation = auth.generation.get_untracked();
                chat.connection_changing.set(true);
                spawn_local(async move {
                    let result = api
                        .with_value(Clone::clone)
                        .set_session_connection(session_id, connection_id)
                        .await;
                    if auth.generation.get_untracked() != generation {
                        return;
                    }
                    chat.connection_changing.set(false);
                    match result {
                        Ok(updated) => {
                            chat.session_model.update(|models| {
                                models.insert(session_id, Some(model.clone()));
                            });
                            chat.sessions.update(|sessions| {
                                if let Some(session) =
                                    sessions.iter_mut().find(|session| session.id == session_id)
                                {
                                    *session = updated;
                                }
                            });
                            if chat.active_session.get_untracked() == Some(session_id) {
                                chat.selected_model.set(Some(model.clone()));
                            }
                            let _ = api.with_value(Clone::clone).set_setting(
                                &format!("session_model_{session_id}"),
                                &serde_json::json!({"connection_id":connection_id,"model":model}).to_string(),
                            ).await;
                        }
                        Err(error) => chat.error.set(Some(error)),
                    }
                });
            },
        );

        let send = Callback::new(move |()| start.run((None, None, None, None)));
        let send_prompt =
            Callback::new(move |prompt: String| start.run((None, None, Some(prompt), None)));
        let controls = super::chat_controls::install(
            api,
            chat,
            projects,
            Callback::new(move |objective: String| {
                let prompt = openwebide_core::Goal {
                    session_id: 0,
                    objective: objective.clone(),
                    status: openwebide_core::GoalStatus::Active,
                    worker: true,
                    note: None,
                    revision: 0,
                    updated_at: 0,
                    started_at: None,
                }
                .prompt();
                start.run((None, None, Some(prompt), Some(objective)));
            }),
            stop,
        );
        let resume_run = Callback::new(move |()| {
            let Some(resume) = chat.interrupted_run.get_untracked() else {
                return;
            };
            let session_project = chat
                .sessions
                .get_untracked()
                .into_iter()
                .find(|session| Some(session.id) == chat.active_session.get_untracked())
                .and_then(|session| session.project_id);
            if !project_runs.can_resume(session_project) {
                return;
            }
            start.run((Some(resume), None, None, None));
        });

        let on_select_session =
            Callback::new(move |session_id: i64| chat.active_session.set(Some(session_id)));
        let auth = expect_context::<crate::state::auth::AuthState>();
        let on_new_session = Callback::new(move |()| {
            if chat.creating_session.get_untracked() || chat.streaming.get_untracked() {
                return;
            }
            let project_id = projects.active_project.get_untracked();
            let primary = settings.model_setup.get_untracked().defaults.primary;
            let connection = primary
                .as_ref()
                .map(|model| model.server_id)
                .or(settings.default_connection.get_untracked());
            let model = primary.map(|model| model.model);
            let prompt = settings.default_prompt.get_untracked();
            let generation = auth.generation.get_untracked();
            let mode = chat.draft_approval_mode.get_untracked();
            chat.creating_session.set(true);
            let current = move || {
                auth.generation.try_get_untracked() == Some(generation)
                    && projects.active_project.try_get_untracked() == Some(project_id)
            };
            spawn_local(async move {
                let result = async {
                    let backend = api.with_value(Clone::clone);
                    let session = backend
                        .create_session("New chat", connection, prompt, project_id)
                        .await?;
                    super::approvals::save_session_mode(api, session.id, mode).await?;
                    if !current() {
                        return Ok::<_, String>(());
                    }
                    chat.set_approval_mode(session.id, mode);
                    chat.sessions
                        .update(|sessions| sessions.push(session.clone()));
                    chat.draft_connection.set(None);
                    chat.selected_model.set(model.clone());
                    chat.session_model.update(|models| {
                        models.insert(session.id, model.clone());
                    });
                    chat.skip_history_load.set_value(Some(session.id));
                    chat.messages.install_history(Vec::new());
                    chat.active_session.set(Some(session.id));
                    let Some(project_id) = project_id else {
                        return Ok(());
                    };
                    let content = project_host
                        .startup_context(project_id, connection, model.as_deref())
                        .await?;
                    if !current() {
                        return Ok(());
                    }
                    let content = format!("{}{content}", openwebide_core::RUN_CONTEXT_PREFIX);
                    let message = backend
                        .persist_message(
                            session.id,
                            openwebide_core::Role::System,
                            &content,
                            None,
                            None,
                        )
                        .await?;
                    if current()
                        && chat.active_session.try_get_untracked() == Some(Some(session.id))
                    {
                        chat.apply_event_for_session(
                            session.id,
                            openwebide_core::RunEvent::Message { message },
                        );
                    }
                    Ok(())
                }
                .await;
                if auth.generation.try_get_untracked() == Some(generation) {
                    chat.creating_session.set(false);
                    if let Err(error) = result
                        && current()
                    {
                        chat.error.set(Some(error));
                    }
                }
            });
        });

        let on_rename_session = Callback::new(move |session_id: i64| {
            let current = chat
                .sessions
                .with(|sessions| {
                    sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .map(|session| session.name.clone())
                })
                .unwrap_or_default();
            ui.set_prompt(PromptRequest {
                title: "Rename session".to_string(),
                value: current,
                placeholder: "Session name".to_string(),
                submit_label: "Rename".to_string(),
                on_submit: Callback::new(move |name: String| {
                    spawn_local(async move {
                        match api
                            .with_value(Clone::clone)
                            .rename_session(session_id, &name)
                            .await
                        {
                            Ok(updated) => chat.sessions.update(|sessions| {
                                if let Some(session) =
                                    sessions.iter_mut().find(|session| session.id == session_id)
                                {
                                    *session = updated;
                                }
                            }),
                            Err(error) => ui.notify(error),
                        }
                    });
                }),
            });
        });

        let on_delete_session = Callback::new(move |session_id: i64| {
            ui.set_confirm(ConfirmRequest {
                title: "Delete session".to_string(),
                message: "Delete this session and its messages?".to_string(),
                confirm_label: "Delete".to_string(),
                action: Callback::new(move |()| {
                    spawn_local(async move {
                        if let Err(error) = api
                            .with_value(Clone::clone)
                            .delete_session(session_id)
                            .await
                        {
                            ui.notify(error);
                            return;
                        }
                        chat.sessions
                            .update(|sessions| sessions.retain(|session| session.id != session_id));
                        chat.approval_mode.update(|modes| {
                            modes.remove(&session_id);
                        });
                        if chat.active_session.get() == Some(session_id) {
                            chat.active_session.set(None);
                        }
                    });
                }),
            });
        });

        let slash_command = {
            Callback::new(move |command: SlashCommand| {
                match dispatch_slash(command, &chat, &git, &workspace) {
                    SlashAction::Compact => controls.compact.run(()),
                    SlashAction::Goal(args) => controls.goal.run(args),
                    SlashAction::Context => ui.context_open.set(true),
                    SlashAction::Notify(text) => chat.notify(text),
                    SlashAction::SelectModel(name) => {
                        select_model.run(Some(name.clone()));
                        chat.notify(format!("Switched model to `{name}`."));
                    }
                    SlashAction::DefaultModel => {
                        select_model.run(None);
                        chat.notify("Using the default model.");
                    }
                    SlashAction::Clear => chat.messages.install_history(Vec::new()),
                    SlashAction::GitDiff { project_id, path } => {
                        spawn_local(async move {
                            match async {
                                project_git
                                    .repository(project_id)
                                    .await?
                                    .diff(path.as_deref())
                                    .await
                            }
                            .await
                            {
                                Ok(diff) if !diff.trim().is_empty() => chat.notify(format!(
                                    "**Git Repository Diff:**\n```diff\n{diff}\n```"
                                )),
                                Ok(_) => {
                                    chat.notify("Working tree is clean (no uncommitted diffs).");
                                }
                                Err(error) => chat.notify(format!("Git diff failed: {error}")),
                            }
                        });
                    }
                    SlashAction::OpenPendingDiff(diff) => {
                        request_open.run(diff.path.clone());
                        chat.notify(format!(
                            "Opened pending diff for `{}` in editor.",
                            diff.path
                        ));
                    }
                    SlashAction::RunTests { project_id, filter } => {
                        if !bridge
                            .get_untracked()
                            .is_some_and(|bridge| bridge.status().get_untracked().terminal_ready())
                        {
                            chat.notify("The terminal bridge isn't connected; start openwebide-bridge and try again.");
                            return;
                        }
                        let tooling = projects.tooling.with_untracked(|tools| {
                            project_id.and_then(|id| tools.get(&id).cloned())
                        });
                        let cmd = tooling.as_ref().map_or_else(
                            || {
                                crate::text::test_command(
                                    (!filter.is_empty()).then_some(filter.as_str()),
                                )
                            },
                            |tools| tools.test_command(&filter),
                        );
                        chat.show_terminal.set(true);
                        expect_context::<crate::state::layout::LayoutState>()
                            .terminal_cmd
                            .set(Some(cmd.clone()));
                        let suggestion = tooling.map_or_else(String::new, |tools| {
                            format!(
                                " {} project; suggested linter: `{}`.",
                                tools.language, tools.linters
                            )
                        });
                        chat.notify(format!("Running `{cmd}` in the terminal.{suggestion}"));
                    }
                    SlashAction::Commit {
                        project_id,
                        message,
                    } => {
                        if project_id == projects.active_project.get_untracked() {
                            on_commit.run(GitCommitRequest {
                                message,
                                paths: None,
                                include_untracked: false,
                                staged_only: false,
                            });
                        }
                    }
                    SlashAction::Checkout { project_id, branch } => {
                        if project_id == projects.active_project.get_untracked() {
                            on_checkout.run((branch, false));
                        }
                    }
                    SlashAction::CreateBranch { project_id, branch } => {
                        if project_id == projects.active_project.get_untracked() {
                            on_checkout.run((branch, true));
                        }
                    }
                    SlashAction::ListBranches { project_id } => {
                        spawn_local(async move {
                            match async {
                                project_git.repository(project_id).await?.branches().await
                            }
                            .await
                            {
                                Ok(branches) => {
                                    let branch_list = if branches.is_empty() {
                                        "No branches found.".to_string()
                                    } else {
                                        branches
                                            .into_iter()
                                            .map(|branch| {
                                                if branch.is_current {
                                                    format!("* **{}** (current)", branch.name)
                                                } else {
                                                    format!("  {}", branch.name)
                                                }
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    };
                                    chat.notify(format!(
                                        "**Repository Branches:**\n\n{branch_list}"
                                    ));
                                }
                                Err(error) => {
                                    chat.notify(format!("Failed to list branches: {error}"));
                                }
                            }
                        });
                    }
                    SlashAction::Sync { .. } => on_sync_click.run(()),
                    SlashAction::Stop => stop.run(()),
                }
            })
        };

        let conversation = super::conversation::actions(api, chat, projects, ui);
        install_effects(api, chat, projects, settings, runs, project_runs);

        Self {
            conversation,
            queue,
            rewind,
            send,
            send_prompt,
            resume_run,
            stop,
            permission,
            permission_always,
            select_model,
            select_connection_model,
            slash_command,
            on_select_session,
            on_new_session,
            on_rename_session,
            on_delete_session,
        }
    }
}

fn install_effects(
    api: Api,
    chat: ChatState,
    projects: ProjectsState,
    settings: SettingsState,
    runs: super::runs::RunActions,
    project_runs: crate::project_runs::ProjectRuns,
) {
    let active_session = chat.active_session;
    let sessions = chat.sessions;
    let connections = settings.connections;
    let default_connection = settings.default_connection;
    let selected_model = chat.selected_model;
    let session_telemetry = chat.session_telemetry;

    let auth = expect_context::<crate::state::auth::AuthState>();
    let effective_connection = Memo::new(move |_| {
        let id = active_session
            .get()
            .and_then(|id| {
                sessions.with(|sessions| {
                    sessions
                        .iter()
                        .find(|session| session.id == id)
                        .and_then(|session| session.connection_id)
                })
            })
            .or_else(|| {
                chat.draft_connection
                    .get()
                    .filter(|_| active_session.get().is_none())
                    .or_else(|| {
                        settings
                            .model_setup
                            .get()
                            .defaults
                            .primary
                            .map(|selection| selection.server_id)
                    })
                    .or(default_connection.get())
                    .or_else(|| {
                        connections.with(|connections| {
                            connections
                                .iter()
                                .find(|connection| connection.enabled)
                                .map(|connection| connection.id)
                        })
                    })
            });
        let connection = id.and_then(|id| {
            connections.with(|connections| {
                connections
                    .iter()
                    .find(|connection| connection.id == id)
                    .cloned()
            })
        });
        (id, connection)
    });
    let model_request = Memo::new(move |_| {
        let (id, connection) = effective_connection.get();
        (
            auth.generation.get(),
            id,
            connection.map(|connection| (connection.kind, connection.base_url, connection.enabled)),
        )
    });
    let context_request = Memo::new(move |_| {
        let (_, connection) = effective_connection.get();
        let model = selected_model
            .get()
            .or_else(|| {
                settings
                    .model_setup
                    .get()
                    .defaults
                    .primary
                    .filter(|selection| Some(selection.server_id) == effective_connection.get().0)
                    .map(|selection| selection.model)
            })
            .or_else(|| {
                connection
                    .as_ref()
                    .and_then(|connection| connection.model.clone())
            });
        let limit = connection
            .as_ref()
            .and_then(|connection| connection.context_limit);
        (model_request.get(), model, limit)
    });

    Effect::new(move |_| {
        let (_, connection) = effective_connection.get();
        let effective_model = selected_model
            .get()
            .or_else(|| {
                settings
                    .model_setup
                    .get()
                    .defaults
                    .primary
                    .filter(|selection| Some(selection.server_id) == effective_connection.get().0)
                    .map(|selection| selection.model)
            })
            .or_else(|| {
                connection
                    .as_ref()
                    .and_then(|connection| connection.model.clone())
            });
        let display_model = effective_model.unwrap_or_else(|| {
            format!(
                "Default ({})",
                connection
                    .as_ref()
                    .map_or("unknown", |connection| connection.name.as_str())
            )
        });
        session_telemetry.update(|telemetry| telemetry.model = display_model);
    });

    let ctx_request_gen = StoredValue::new(0u64);
    Effect::new(move |_| {
        let request = context_request.get();
        ctx_request_gen.update_value(|generation| *generation += 1);
        let this_gen = ctx_request_gen.get_value();
        let Some(connection_id) = request.0.1 else {
            session_telemetry.update(|telemetry| {
                telemetry.context_limit = DEFAULT_CONTEXT_LIMIT;
                telemetry.context_limit_estimated = true;
            });
            return;
        };
        spawn_local(async move {
            let Some(backend) = api.try_with_value(Clone::clone) else {
                return;
            };
            let result = backend
                .model_context(connection_id, request.1.as_deref())
                .await;
            if ctx_request_gen.try_get_value() != Some(this_gen)
                || context_request.try_get_untracked() != Some(request)
            {
                return;
            }
            let resolved = result.ok().flatten();
            session_telemetry.update(|telemetry| {
                telemetry.context_limit = resolved.unwrap_or(DEFAULT_CONTEXT_LIMIT);
                telemetry.context_limit_estimated = resolved.is_none();
            });
        });
    });

    let history_gen = chat.history_gen;
    let skip_history_load = chat.skip_history_load;
    Effect::new(move |_| {
        let session_id = active_session.get();
        chat.interrupted_run.set(None);
        if let Some(id) = session_id
            && skip_history_load.get_value() == Some(id)
        {
            skip_history_load.set_value(None);
            return;
        }
        let this_gen = {
            history_gen.update_value(|generation| *generation += 1);
            history_gen.get_value()
        };
        chat.loading_history.set(session_id);
        let account = expect_context::<crate::state::auth::AuthState>();
        let account_generation = account.generation.get_untracked();
        spawn_local(async move {
            match session_id {
                Some(id) => {
                    let Some(backend) = api.try_with_value(Clone::clone) else {
                        return;
                    };
                    let result = backend.list_messages(id).await;
                    if account.generation.try_get_untracked() != Some(account_generation)
                        || history_gen.try_get_value() != Some(this_gen)
                        || active_session.try_get_untracked() != Some(Some(id))
                    {
                        return;
                    }
                    chat.loading_history.set(None);
                    match result {
                        Ok(entries) => {
                            session_telemetry
                                .update(|telemetry| telemetry.restore_from_conversation(&entries));
                            chat.messages.install_history(history_items(entries));
                            project_runs.restore(id);
                            runs.history.set(Some((id, this_gen)));
                        }
                        Err(error) => {
                            session_telemetry
                                .update(|telemetry| telemetry.restore_from_conversation(&[]));
                            chat.error.set(Some(error));
                        }
                    }
                }
                None => {
                    session_telemetry.update(|telemetry| telemetry.restore_from_conversation(&[]));
                    chat.messages.install_history(Vec::new());
                }
            }
        });
    });

    let model_request_gen = StoredValue::new(0u64);
    Effect::new(move |_| {
        let request = model_request.get();
        model_request_gen.update_value(|generation| *generation += 1);
        let this_gen = model_request_gen.get_value();
        spawn_local(async move {
            let Some(backend) = api.try_with_value(Clone::clone) else {
                return;
            };
            let result = match request.1 {
                Some(id) => backend.list_models(id).await,
                None => Ok(Vec::new()),
            };
            if model_request_gen.try_get_value() == Some(this_gen)
                && model_request.try_get_untracked() == Some(request)
            {
                match result {
                    Ok(models) => chat.models.set(models),
                    Err(message) => {
                        chat.models.set(Vec::new());
                        chat.error.set(Some(message));
                    }
                }
            }
        });
    });

    Effect::new(move |_| {
        projects.active_project.track();
        active_session.track();
        chat.draft_connection.set(None);
        if active_session.get_untracked().is_none() {
            chat.selected_model.set(None);
        }
    });

    Effect::new(move |_| {
        if let Some(session_id) = active_session.get() {
            let model = chat.session_model.get().get(&session_id).cloned().flatten();
            chat.selected_model.set(model);
        }
    });
}

fn derive_session_name(prompt: &str) -> String {
    let prompt = openwebide_core::PromptContent::decode(prompt);
    let line = prompt
        .text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.is_empty() {
        return "Image chat".into();
    }
    let mut name: String = line.chars().take(40).collect();
    if line.chars().count() > 40 {
        name.push('…');
    }
    name
}

pub(crate) fn history_items(entries: Vec<ConversationEntry>) -> Vec<ConversationItem> {
    entries
        .into_iter()
        .filter(|entry| !matches!(entry, ConversationEntry::ToolStep(step) if openwebide_core::tasks::task_step_scope(&step.tool_call_id).is_some()))
        .map(|entry| match entry {
            ConversationEntry::Task(task) => ConversationItem::Task(Box::new(task.snapshot)),
            ConversationEntry::Message(message) => ConversationItem::Message(message),
            ConversationEntry::ToolStep(step) => ConversationItem::ToolStep {
                timing: step.timing,
                key: next_item_nonce(),
                id: step.tool_call_id,
                name: step.name,
                summary: step.summary,
                result: step.ok.map(|ok| ToolStepResult {
                    ok,
                    summary: step.result_summary.clone().unwrap_or_default(),
                    diff: step.diff.clone(),
                }),
                awaiting_permission: false,
                diff: step.ok.is_none().then(|| step.diff.clone()).flatten(),
                note: None,
            },
        })
        .collect()
}
