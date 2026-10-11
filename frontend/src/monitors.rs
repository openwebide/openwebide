//! Conversation-scoped monitor facade; both modes use the same durable host dispatcher.
use crate::{
    backend::Api,
    state::{auth::AuthState, chat::ChatState, monitors::MonitorsState, projects::ProjectsState},
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::scheduled::{MonitorCommand, TaskCommand};
#[derive(Clone, Copy)]
pub struct MonitorActions {
    pub command: Callback<MonitorCommand>,
    pub authorize: Callback<()>,
}
impl MonitorActions {
    pub fn new(
        api: Api,
        state: MonitorsState,
        auth: AuthState,
        projects: ProjectsState,
        chat: ChatState,
    ) -> Self {
        let host = expect_context::<crate::project_host::ProjectHost>();
        let generation = StoredValue::new(0u64);
        let command = Callback::new(move |command| {
            let Some(session) = chat.active_session.get_untracked() else {
                return;
            };
            if state.busy.get_untracked() {
                return;
            }
            let account = auth.generation.get_untracked();
            let project = projects.active_project.get_untracked();
            let ticket = generation.get_value();
            state.busy.set(true);
            spawn_local(async move {
                let current = move || {
                    generation.try_get_value() == Some(ticket)
                        && auth.generation.try_get_untracked() == Some(account)
                        && projects.active_project.try_get_untracked() == Some(project)
                        && chat.active_session.try_get_untracked() == Some(Some(session))
                };
                let result = async {
                    let backend = api.with_value(Clone::clone);
                    if !matches!(command, MonitorCommand::List {}) {
                        if matches!(command, MonitorCommand::Start { .. })
                            && let Some(binding) = host.background_binding(project, current).await?
                        {
                            if !current() {
                                return Err("Plugin action context changed".into());
                            }
                            backend
                                .bind_background_host(
                                    project.ok_or("Project is no longer available")?,
                                    &binding,
                                )
                                .await?;
                        }
                        let call = TaskCommand::Monitor {
                            session_id: session,
                            command,
                        }
                        .plugin_call()?;
                        let outcome = crate::plugin_actions::invoke_scoped_plugin_action(
                            api, host, project, &call, true, current,
                        )
                        .await?;
                        if !outcome.ok {
                            return Err(outcome.content);
                        }
                    }
                    if !current() {
                        return Err("Plugin action context changed".into());
                    }
                    backend.scheduled_monitors(session).await
                }
                .await;
                if generation.try_get_value() != Some(ticket)
                    || auth.generation.try_get_untracked() != Some(account)
                    || projects.active_project.try_get_untracked() != Some(project)
                    || chat.active_session.try_get_untracked() != Some(Some(session))
                {
                    return;
                }
                state.busy.set(false);
                match result {
                    Ok(entries) => {
                        state.entries.set(entries);
                        state.error.set(None);
                    }
                    Err(error) => state.error.set(Some(error)),
                }
            });
        });
        Effect::new(move |_| {
            auth.generation.get();
            projects.active_project.get();
            chat.active_session.get();
            generation.update_value(|value| *value += 1);
            state.entries.set(Vec::new());
            state.error.set(None);
            state.busy.set(false);
        });
        let authorize = Callback::new(move |()| {
            let Some(session) = chat.active_session.get_untracked() else {
                return;
            };
            let Some(project) = projects.active_project.get_untracked() else {
                return;
            };
            if state.busy.get_untracked() {
                return;
            }
            let account = auth.generation.get_untracked();
            let ticket = generation.get_value();
            let current = move || {
                generation.try_get_value() == Some(ticket)
                    && auth.generation.try_get_untracked() == Some(account)
                    && projects.active_project.try_get_untracked() == Some(Some(project))
                    && chat.active_session.try_get_untracked() == Some(Some(session))
            };
            state.busy.set(true);
            spawn_local(async move {
                let result = async {
                    let binding = host.scheduled_binding(project, current).await?;
                    let backend = api.with_value(Clone::clone);
                    if !current() {
                        return Err("Plugin action context changed".into());
                    }
                    backend.bind_background_host(project, &binding).await?;
                    if !current() {
                        return Err("Plugin action context changed".into());
                    }
                    backend.scheduled_monitors(session).await
                }
                .await;
                if !current() {
                    return;
                }
                state.busy.set(false);
                match result {
                    Ok(entries) => {
                        state.entries.set(entries);
                        state.error.set(None);
                    }
                    Err(error) => state.error.set(Some(error)),
                }
            });
        });
        Self { command, authorize }
    }
}
