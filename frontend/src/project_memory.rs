//! Project-memory UI facade; database transport is identical for both workspace modes.
use crate::{
    backend::Api,
    project_host::ProjectHost,
    state::{auth::AuthState, chat::ChatState, memories::MemoriesState, projects::ProjectsState},
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::{MemoryCommand, ProjectMemory};
#[derive(Clone, Copy)]
pub struct ProjectMemoryActions {
    pub refresh: Callback<()>,
    pub command: Callback<MemoryCommand>,
    pub edit: Callback<Option<ProjectMemory>>,
    pub save: Callback<()>,
}
impl ProjectMemoryActions {
    pub fn new(
        api: Api,
        state: MemoriesState,
        auth: AuthState,
        projects: ProjectsState,
        chat: ChatState,
        host: ProjectHost,
    ) -> Self {
        let generation = StoredValue::new(0u64);
        let refresh = Callback::new(move |()| {
            let Some(project) = projects.active_project.get_untracked() else {
                return;
            };
            if state.busy.get_untracked() {
                return;
            }
            generation.update_value(|value| *value += 1);
            let ticket = generation.get_value();
            let account = auth.generation.get_untracked();
            state.loading.set(true);
            spawn_local(async move {
                let Some(backend) = api.try_with_value(Clone::clone) else {
                    return;
                };
                let result = backend.project_memories(project).await;
                if auth.generation.try_get_untracked() != Some(account)
                    || projects.active_project.try_get_untracked() != Some(Some(project))
                    || generation.try_get_value() != Some(ticket)
                {
                    return;
                }
                state.loading.set(false);
                match result {
                    Ok(data) => {
                        state.data.set(Some(data));
                        state.error.set(None);
                    }
                    Err(error) => state.error.set(Some(error)),
                }
            });
        });
        let command = Callback::new(move |command: MemoryCommand| {
            let Some(project) = projects.active_project.get_untracked() else {
                return;
            };
            if state.busy.get_untracked() {
                return;
            }
            if let Err(error) = command.validate() {
                state.error.set(Some(error));
                return;
            }
            generation.update_value(|value| *value += 1);
            let ticket = generation.get_value();
            let account = auth.generation.get_untracked();
            let saved = matches!(
                command,
                MemoryCommand::Create { .. } | MemoryCommand::Update { .. }
            );
            state.busy.set(true);
            state.loading.set(false);
            spawn_local(async move {
                let Some(backend) = api.try_with_value(Clone::clone) else {
                    return;
                };
                let current = move || {
                    auth.generation.try_get_untracked() == Some(account)
                        && projects.active_project.try_get_untracked() == Some(Some(project))
                        && generation.try_get_value() == Some(ticket)
                };
                let result = async {
                    match command.plugin_call()? {
                        Some(call) => {
                            let outcome = crate::plugin_actions::invoke_project_plugin_action(
                                api, host, project, &call, true, current,
                            )
                            .await?;
                            if !outcome.ok {
                                return Err(outcome.content);
                            }
                            if !current() {
                                return Err("Plugin action context changed".into());
                            }
                            backend.project_memories(project).await
                        }
                        None => backend.memory_command(project, &command, false).await,
                    }
                }
                .await;
                if auth.generation.try_get_untracked() != Some(account)
                    || projects.active_project.try_get_untracked() != Some(Some(project))
                    || generation.try_get_value() != Some(ticket)
                {
                    return;
                }
                state.busy.set(false);
                match result {
                    Ok(data) => {
                        state.data.set(Some(data));
                        state.error.set(None);
                        if saved {
                            state.editing.set(false);
                        }
                    }
                    Err(error) => {
                        // Keep the selected state after a failed persistence request.
                        state.data.update(|_| ());
                        state.error.set(Some(error));
                    }
                }
            });
        });
        let edit = Callback::new(move |entry: Option<ProjectMemory>| {
            state
                .auto_title
                .set(entry.as_ref().is_none_or(|entry| entry.auto_title));
            state
                .edit_id
                .set(entry.as_ref().map(|entry| (entry.id, entry.revision)));
            state.title.set(
                entry
                    .as_ref()
                    .map_or_else(String::new, |entry| entry.title.clone()),
            );
            state
                .content
                .set(entry.map_or_else(String::new, |entry| entry.content));
            state.editing.set(true);
            state.error.set(None);
        });
        let save = Callback::new(move |()| {
            let title = state.title.get_untracked();
            let content = state.content.get_untracked();
            command.run(match state.edit_id.get_untracked() {
                Some((id, revision)) => MemoryCommand::Update {
                    auto_title: state.auto_title.get_untracked(),
                    id,
                    revision,
                    title,
                    content,
                },
                None => MemoryCommand::Create {
                    auto_title: state.auto_title.get_untracked(),
                    title,
                    content,
                },
            });
        });
        Effect::new(move |_| {
            auth.generation.get();
            projects.active_project.get();
            generation.update_value(|value| *value += 1);
            state.data.set(None);
            state.error.set(None);
            state.editing.set(false);
            state.title.set(String::new());
            state.content.set(String::new());
            state.edit_id.set(None);
            state.busy.set(false);
            state.loading.set(false);
            refresh.run(());
        });
        let was_streaming = StoredValue::new(false);
        Effect::new(move |_| {
            let streaming = chat.streaming.get();
            let previous = was_streaming.get_value();
            was_streaming.set_value(streaming);
            if previous && !streaming {
                refresh.run(());
            }
        });
        let timer = leptos::leptos_dom::helpers::set_interval_with_handle(
            move || refresh.run(()),
            std::time::Duration::from_secs(10),
        )
        .ok();
        on_cleanup(move || {
            if let Some(timer) = timer {
                timer.clear();
            }
        });
        Self {
            refresh,
            command,
            edit,
            save,
        }
    }
}
