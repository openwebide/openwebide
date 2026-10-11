//! Saved-prompt UI facade shared across project modes and projectless chat.
use crate::{
    backend::Api,
    state::{auth::AuthState, chat::ChatState, projects::ProjectsState, scheduled::TasksState},
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::scheduled::{Schedule, ScheduledTask, TaskCommand, TaskDraft};
use wasm_bindgen::prelude::*;
#[wasm_bindgen(
    inline_js = "export function taskTime(at) { return new Date(at*1000).toLocaleString(); } export function taskDate(at) { const d=new Date(at*1000); const p=n=>String(n).padStart(2,'0'); return `${d.getFullYear()}-${p(d.getMonth()+1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`; } export function taskTimestamp(value) { return new Date(value).getTime()/1000; }"
)]
extern "C" {
    #[wasm_bindgen(js_name=taskTime)]
    fn task_time(at: f64) -> String;
    #[wasm_bindgen(js_name=taskDate)]
    fn task_date(at: f64) -> String;
    #[wasm_bindgen(js_name=taskTimestamp)]
    fn task_timestamp(value: &str) -> f64;
}
#[allow(
    clippy::cast_precision_loss,
    reason = "Epoch seconds fit exactly in JavaScript numbers"
)]
pub fn time(at: i64) -> String {
    task_time(at as f64)
}
#[derive(Clone, Copy)]
pub struct TaskActions {
    pub refresh: Callback<()>,
    pub command: Callback<TaskCommand>,
    pub edit: Callback<Option<ScheduledTask>>,
    pub save: Callback<()>,
    pub cancel: Callback<()>,
}
impl TaskActions {
    pub fn new(
        api: Api,
        state: TasksState,
        auth: AuthState,
        projects: ProjectsState,
        chat: ChatState,
    ) -> Self {
        let host = expect_context::<crate::project_host::ProjectHost>();
        let generation = StoredValue::new(0u64);
        let refresh = Callback::new(move |()| {
            if state.busy.get_untracked() || state.loading.get_untracked() {
                return;
            }
            let project = projects.active_project.get_untracked();
            let account = auth.generation.get_untracked();
            let ticket = generation.get_value();
            state.loading.set(true);
            spawn_local(async move {
                let backend = api.with_value(Clone::clone);
                let result = backend.scheduled_tasks(project).await;
                if auth.generation.try_get_untracked() != Some(account)
                    || projects.active_project.try_get_untracked() != Some(project)
                    || generation.try_get_value() != Some(ticket)
                {
                    return;
                }
                state.loading.set(false);
                state.loaded.set(true);
                match result {
                    Ok(entries) => {
                        if entries
                            .iter()
                            .filter_map(|entry| {
                                entry.last_run.as_ref().and_then(|run| run.session_id)
                            })
                            .any(|id| {
                                !chat.sessions.with_untracked(|sessions| {
                                    sessions.iter().any(|session| session.id == id)
                                })
                            })
                            && let Ok(sessions) = backend.list_sessions().await
                        {
                            if auth.generation.try_get_untracked() != Some(account)
                                || projects.active_project.try_get_untracked() != Some(project)
                                || generation.try_get_value() != Some(ticket)
                            {
                                return;
                            }
                            chat.sessions.update(|known| {
                                for session in sessions {
                                    if !known.iter().any(|entry| entry.id == session.id) {
                                        known.push(session);
                                    }
                                }
                            });
                        }
                        if state.entries.get_untracked() != entries {
                            state.entries.set(entries);
                        }
                        if !state.editing.get_untracked() {
                            state.error.set(None);
                        }
                    }
                    Err(error) => state.error.set(Some(error)),
                }
            });
        });
        let command = Callback::new(move |command: TaskCommand| {
            if state.busy.get_untracked() {
                return;
            }
            let project = projects.active_project.get_untracked();
            let account = auth.generation.get_untracked();
            generation.update_value(|value| *value += 1);
            let ticket = generation.get_value();
            state.busy.set(true);
            state.loading.set(false);
            spawn_local(async move {
                let current = move || {
                    auth.generation.try_get_untracked() == Some(account)
                        && projects.active_project.try_get_untracked() == Some(project)
                        && generation.try_get_value() == Some(ticket)
                };
                let saved = matches!(
                    command,
                    TaskCommand::Create { .. } | TaskCommand::Update { .. }
                );
                let result = async {
                    let backend = api.with_value(Clone::clone);
                    if (saved || matches!(command, TaskCommand::SetEnabled { enabled: true, .. }))
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
                    let outcome = crate::plugin_actions::invoke_scoped_plugin_action(
                        api,
                        host,
                        project,
                        &command.plugin_call()?,
                        true,
                        current,
                    )
                    .await?;
                    if !outcome.ok {
                        return Err(outcome.content);
                    }
                    if !current() {
                        return Err("Plugin action context changed".into());
                    }
                    backend.scheduled_tasks(project).await
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
                        if saved {
                            state.editing.set(false);
                        }
                    }
                    Err(error) => state.error.set(Some(error)),
                }
            });
        });
        let edit = Callback::new(move |entry: Option<ScheduledTask>| {
            state
                .auto_title
                .set(entry.as_ref().is_none_or(|entry| entry.draft.auto_title));
            state
                .edit_id
                .set(entry.as_ref().map(|entry| (entry.id, entry.revision)));
            state.title.set(
                entry
                    .as_ref()
                    .map_or_else(String::new, |entry| entry.draft.title.clone()),
            );
            state
                .model
                .set(entry.as_ref().and_then(|entry| entry.draft.model.clone()));
            state.prompt.set(
                entry
                    .as_ref()
                    .map_or_else(String::new, |entry| entry.draft.prompt.clone()),
            );
            state.session_target.set(
                entry
                    .as_ref()
                    .map_or(openwebide_core::scheduled::SessionTarget::Latest, |entry| {
                        entry.draft.session_target
                    }),
            );
            state.session.set(entry.as_ref().map_or_else(
                || chat.active_session.get_untracked().unwrap_or(0),
                |entry| entry.draft.session_id,
            ));
            state
                .enabled
                .set(entry.as_ref().is_none_or(|entry| entry.draft.enabled));
            if let Some(entry) = entry {
                match entry.draft.schedule {
                    Schedule::Once { at } => {
                        state.kind.set("once".into());
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "Epoch seconds fit in JS numbers"
                        )]
                        state.date.set(task_date(at as f64));
                    }
                    Schedule::Cron {
                        expression,
                        timezone,
                    } => {
                        if let Some((time, days)) =
                            openwebide_core::scheduled::calendar_cron(&expression)
                        {
                            state.kind.set("weekly".into());
                            state.time.set(time);
                            state.days.set(days);
                        } else {
                            state.kind.set("custom".into());
                        }
                        state.cron.set(expression);
                        state.timezone.set(timezone);
                    }
                }
            } else {
                state.kind.set("weekly".into());
                state.timezone.set(
                    crate::browser_preferences::capture()
                        .and_then(|value| value.timezone)
                        .unwrap_or_else(|| "UTC".into()),
                );
            }
            state.editing.set(true);
            state.error.set(None);
        });
        let save = Callback::new(move |()| {
            let schedule = match state.kind.get_untracked().as_str() {
                "once" => {
                    let at = task_timestamp(&state.date.get_untracked());
                    if !at.is_finite() {
                        state.error.set(Some("Choose a date and time.".into()));
                        return;
                    }
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "Validated browser epoch seconds fit in i64"
                    )]
                    let at = at as i64;
                    Schedule::Once { at }
                }
                "weekly" => match openwebide_core::scheduled::weekly_cron(
                    &state.time.get_untracked(),
                    &state.days.get_untracked(),
                ) {
                    Ok(expression) => Schedule::Cron {
                        expression,
                        timezone: state.timezone.get_untracked(),
                    },
                    Err(error) => {
                        state.error.set(Some(error));
                        return;
                    }
                },
                _ => Schedule::Cron {
                    expression: state.cron.get_untracked(),
                    timezone: state.timezone.get_untracked(),
                },
            };
            let draft = TaskDraft {
                model: state.model.get_untracked(),
                session_target: state.session_target.get_untracked(),
                auto_title: state.auto_title.get_untracked(),
                title: state.title.get_untracked(),
                prompt: state.prompt.get_untracked(),
                session_id: if state.session_target.get_untracked()
                    == openwebide_core::scheduled::SessionTarget::Existing
                {
                    state.session.get_untracked()
                } else {
                    0
                },
                schedule,
                enabled: state.enabled.get_untracked(),
            };
            command.run(state.edit_id.get_untracked().map_or_else(
                || TaskCommand::Create {
                    draft: draft.clone(),
                },
                |(id, revision)| TaskCommand::Update {
                    id,
                    revision,
                    draft: draft.clone(),
                },
            ));
        });
        Effect::new(move |_| {
            projects.active_project.track();
            auth.generation.track();
            generation.update_value(|value| *value += 1);
            state.entries.set(Vec::new());
            state.loaded.set(false);
            state.editing.set(false);
            state.busy.set(false);
            state.loading.set(false);
            state.error.set(None);
            refresh.run(());
        });
        let running = StoredValue::new(true);
        on_cleanup(move || running.set_value(false));
        spawn_local(async move {
            loop {
                crate::util::sleep_ms(5000).await;
                if running.try_get_value() != Some(true) {
                    break;
                }
                refresh.run(());
            }
        });
        let cancel = Callback::new(move |()| {
            // Dismissing the editor does not cancel or strand an in-flight save.
            state.editing.set(false);
            state.error.set(None);
        });
        Self {
            cancel,
            refresh,
            command,
            edit,
            save,
        }
    }
}
