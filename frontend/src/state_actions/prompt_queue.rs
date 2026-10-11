//! Queue policy above shared workspace/run facades and database transport.
use crate::{
    backend::Api,
    project_git::ProjectGit,
    state::{
        auth::AuthState,
        chat::ChatState,
        projects::ProjectsState,
        ui::{ConfirmRequest, UiState},
    },
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::{PromptContent, QueuedPrompt, QueuedPromptKey};

#[derive(Clone, Copy)]
pub struct PromptQueueActions {
    pub enqueue: Callback<()>,
    pub steer: Callback<()>,
    pub edit: Callback<QueuedPromptKey>,
    pub remove: Callback<QueuedPromptKey>,
    pub toggle: Callback<()>,
    pub cancel_edit: Callback<()>,
}

pub fn actions(
    api: Api,
    chat: ChatState,
    projects: ProjectsState,
    git: ProjectGit,
    ui: UiState,
    send: Callback<QueuedPrompt>,
    stop: Callback<()>,
) -> PromptQueueActions {
    let auth = expect_context::<AuthState>();
    let generation = StoredValue::new(0u64);
    let refresh = Callback::new(move |()| {
        let Some(session) = chat.active_session.get_untracked() else {
            return;
        };
        if chat.queue_loading.get_untracked() || chat.queue_busy.get_untracked() {
            return;
        }
        let account = auth.generation.get_untracked();
        let project = projects.active_project.get_untracked();
        let ticket = generation.get_value();
        let epoch = chat.queue_epoch.get_value();
        chat.queue_loading.set(true);
        spawn_local(async move {
            let Some(backend) = api.try_with_value(Clone::clone) else {
                return;
            };
            let result = backend.list_queued_prompts(session).await;
            if auth.generation.try_get_untracked() != Some(account)
                || generation.try_get_value() != Some(ticket)
                || chat.active_session.try_get_untracked() != Some(Some(session))
                || projects.active_project.try_get_untracked() != Some(project)
            {
                return;
            }
            chat.queue_loading.set(false);
            if chat.queue_epoch.get_value() != epoch {
                return;
            }
            match result {
                Ok(entries) => {
                    if chat.queued_prompts.get_untracked() != entries {
                        chat.queued_prompts.set(entries);
                    }
                }
                Err(error) => {
                    chat.queue_running.update(|sessions| {
                        sessions.remove(&session);
                    });
                    chat.error
                        .set(Some(format!("Could not load queued prompts: {error}")));
                }
            }
        });
    });
    Effect::new(move |_| {
        let account = auth.generation.get();
        let project = projects.active_project.get();
        let session = chat.active_session.get();
        generation.update_value(|value| *value += 1);
        chat.queued_prompts.set(Vec::new());
        chat.queue_busy.set(false);
        chat.queue_loading.set(false);
        chat.queue_edit.set(None);
        chat.prompt_edit.set(None);
        // Installing a fork changes session before its composer is restored.
        // Keep that operation busy; ordinary context switches cancel it.
        if chat.branch_draft_context.get_value()
            != session.map(|session| (account, project, session))
            || session.is_none()
        {
            chat.branch_draft_context.set_value(None);
            chat.branching.set(false);
        }
        // Returning to a session restores pending prompts without starting work
        // until its history/run recovery is ready and the user continues it.
        chat.queue_running.set(Default::default());
        refresh.run(());
    });
    let timer =
        set_interval_with_handle(move || refresh.run(()), std::time::Duration::from_secs(2)).ok();
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
    });
    Effect::new(move |_| {
        let session = chat.active_session.get();
        let streaming = chat.streaming.get();
        if !streaming && chat.queue_steering.get().is_some() {
            chat.queue_steering.set(None);
        }
        if streaming
            || chat.compacting.get()
            || chat.goal_busy.get()
            || chat.queue_busy.get()
            || chat.queue_loading.get()
            || chat.loading_history.get().is_some()
            || chat.rewinding.get()
            || chat.creating_session.get()
            || chat.reading_images.get()
            || chat.connection_changing.get()
            || chat.queue_edit.get().is_some()
            || chat.prompt_edit.get().is_some()
            || chat.branching.get()
        {
            return;
        }
        let Some(session) = session else {
            return;
        };
        if !chat
            .queue_running
            .with(|sessions| sessions.contains(&session))
        {
            return;
        }
        if let Some(prompt) = chat.queued_prompts.with(|entries| entries.first().cloned()) {
            if !prompt.is_host_delivered() {
                untrack(move || send.run(prompt));
            }
        } else {
            chat.queue_running.update(|sessions| {
                sessions.remove(&session);
            });
        }
    });
    let save = Callback::new(move |guidance: bool| {
        let Some(session) = chat.active_session.get_untracked() else {
            return;
        };
        if chat.queue_busy.get_untracked()
            || chat.reading_images.get_untracked()
            || chat.rewinding.get_untracked()
        {
            return;
        }
        let account = auth.generation.get_untracked();
        let project = projects.active_project.get_untracked();
        let ticket = generation.get_value();
        let current = move || {
            auth.generation.try_get_untracked() == Some(account)
                && generation.try_get_value() == Some(ticket)
                && chat.active_session.try_get_untracked() == Some(Some(session))
                && projects.active_project.try_get_untracked() == Some(project)
        };
        let prompt = PromptContent {
            text: chat.draft.get_untracked(),
            images: chat.prompt_images.get_untracked(),
            ..Default::default()
        };
        if prompt.text.trim().is_empty() && prompt.images.is_empty() {
            return;
        }
        let editor = chat.active_editor_context.get_untracked();
        let editing = chat.queue_edit.get_untracked();
        let original = editing.and_then(|key| {
            chat.queued_prompts
                .with_untracked(|entries| entries.iter().find(|entry| entry.key() == key).cloned())
        });
        chat.queue_busy.set(true);
        spawn_local(async move {
            let result = async {
                let mut content =
                    crate::prompt::prepare(api, projects, git, prompt.clone(), current).await?;
                if let Some(editor) = &editor {
                    content = openwebide_agent::session::user_content(content, Some(editor));
                } else if let Some(original) = &original {
                    let original = PromptContent::decode(&original.content);
                    if let (Some(context), _) =
                        openwebide_core::tui::extract_editor_context_prelude(&original.text)
                    {
                        let mut edited = PromptContent::decode(&content);
                        edited.text = format!(
                            "<active_editor_context>\n{context}</active_editor_context>\n\n{}",
                            edited.text
                        );
                        content = edited.encode()?;
                    }
                }
                if !current() {
                    return Err("Session changed while preparing the queued prompt".into());
                }
                if let Some(key) = editing {
                    api.with_value(Clone::clone)
                        .update_queued_prompt(session, key, &content)
                        .await
                } else {
                    api.with_value(Clone::clone)
                        .enqueue_prompt(session, &content, guidance)
                        .await
                }
            }
            .await;
            if !current() {
                return;
            }
            chat.queue_busy.set(false);
            match result {
                Ok(saved) => {
                    chat.queue_epoch.update_value(|value| *value += 1);
                    chat.queued_prompts.update(|entries| {
                        entries.retain(|entry| entry.id != saved.id);
                        entries.push(saved);
                        entries.sort_by_key(|entry| (!entry.guidance, entry.id));
                    });
                    // Keep text/images entered while attachment capture was pending.
                    if chat.draft.get_untracked() == prompt.text
                        && chat.prompt_images.get_untracked() == prompt.images
                    {
                        chat.draft.set(String::new());
                        chat.prompt_images.set(Vec::new());
                        if chat.active_editor_context.get_untracked() == editor {
                            chat.active_editor_context.set(None);
                        }
                    }
                    chat.queue_edit.set(None);
                    if editing.is_none() {
                        chat.queue_running.update(|sessions| {
                            sessions.insert(session);
                        });
                    }
                    if guidance && chat.streaming.get_untracked() {
                        chat.queue_steering
                            .set(chat.streaming_session.get_untracked().or(Some(session)));
                        stop.run(());
                    }
                }
                Err(error) => {
                    chat.queue_running.update(|sessions| {
                        sessions.remove(&session);
                    });
                    chat.error.set(Some(error));
                    refresh.run(());
                }
            }
        });
    });
    let edit = Callback::new(move |key: QueuedPromptKey| {
        let Some(prompt) = chat
            .queued_prompts
            .with_untracked(|entries| entries.iter().find(|entry| entry.key() == key).cloned())
        else {
            return;
        };
        let account = auth.generation.get_untracked();
        let session = chat.active_session.get_untracked();
        let ticket = generation.get_value();
        let restore = Callback::new(move |()| {
            if auth.generation.get_untracked() != account
                || generation.get_value() != ticket
                || chat.active_session.get_untracked() != session
                || chat.queue_busy.get_untracked()
            {
                return;
            }
            if !chat
                .queued_prompts
                .with_untracked(|entries| entries.iter().any(|entry| entry.key() == key))
            {
                ui.notify("Queued prompt changed. Refresh the queue.");
                return;
            }
            let content = PromptContent::decode(&prompt.content);
            chat.queue_running.update(|sessions| {
                sessions.remove(&prompt.session_id);
            });
            chat.queue_edit.set(Some(key));
            chat.prompt_edit.set(None);
            chat.active_editor_context.set(None);
            chat.draft.set(
                openwebide_core::tui::extract_editor_context_prelude(&content.text)
                    .1
                    .into(),
            );
            chat.prompt_images.set(content.images);
        });
        if !chat.draft.with_untracked(String::is_empty)
            || !chat.prompt_images.with_untracked(Vec::is_empty)
        {
            ui.set_confirm(ConfirmRequest {
                title: "Edit queued prompt?".into(),
                message: "Replace the current draft with this queued prompt?".into(),
                confirm_label: "Edit".into(),
                action: restore,
            });
        } else {
            restore.run(());
        }
    });
    let remove = Callback::new(move |key: QueuedPromptKey| {
        let Some(session) = chat.active_session.get_untracked() else {
            return;
        };
        if chat.queue_busy.get_untracked() {
            return;
        }
        let account = auth.generation.get_untracked();
        let ticket = generation.get_value();
        chat.queue_busy.set(true);
        spawn_local(async move {
            let result = api
                .with_value(Clone::clone)
                .remove_queued_prompt(session, key)
                .await;
            if auth.generation.try_get_untracked() != Some(account)
                || generation.try_get_value() != Some(ticket)
                || chat.active_session.try_get_untracked() != Some(Some(session))
            {
                return;
            }
            chat.queue_busy.set(false);
            match result {
                Ok(()) => {
                    chat.queue_epoch.update_value(|value| *value += 1);
                    chat.queued_prompts
                        .update(|entries| entries.retain(|entry| entry.key() != key));
                    if chat.queue_edit.get_untracked() == Some(key) {
                        chat.queue_edit.set(None);
                    }
                }
                Err(error) => {
                    chat.error.set(Some(error));
                    refresh.run(());
                }
            }
        });
    });
    PromptQueueActions {
        enqueue: Callback::new(move |()| save.run(false)),
        steer: Callback::new(move |()| save.run(true)),
        edit,
        remove,
        toggle: Callback::new(move |()| {
            if let Some(session) = chat.active_session.get_untracked() {
                chat.queue_running.update(|sessions| {
                    if !sessions.remove(&session) {
                        sessions.insert(session);
                    }
                });
            }
        }),
        cancel_edit: Callback::new(move |()| chat.queue_edit.set(None)),
    }
}
