use super::{
    cron::CronInput,
    dropdown::{DropdownSelect, SelectOption},
    ui::{
        Button, ButtonVariant, FormField, Icon, IconButton, IconName, SegmentOption,
        SegmentedControl, TextArea, TextInput,
    },
};
use crate::{
    backend::Api,
    scheduled::TaskActions,
    state::{chat::ChatState, projects::ProjectsState, scheduled::TasksState},
};
use leptos::prelude::*;
use openwebide_core::scheduled::{SessionTarget, TaskCommand};
/// Use the shared dropdown surface, retaining saved choices when a server is offline.
#[component]
fn TaskModel() -> impl IntoView {
    let state = expect_context::<TasksState>();
    let api = expect_context::<Api>();
    let auth = expect_context::<crate::state::auth::AuthState>();
    let settings = expect_context::<crate::state::settings::SettingsState>();
    let choices = RwSignal::new(Vec::<SelectOption>::new());
    let error = RwSignal::new(None::<String>);
    let generation = StoredValue::new(0_u64);
    Effect::new(move |_| {
        let epoch = auth.generation.get();
        let servers = settings.connections.get();
        generation.update_value(|value| *value += 1);
        let request = generation.get_value();
        choices.set(Vec::new());
        error.set(None);
        leptos::task::spawn_local(async move {
            let backend = api.with_value(Clone::clone);
            let results =
                futures::future::join_all(servers.iter().filter(|server| server.enabled).map(
                    |server| {
                        let backend = &backend;
                        async move { (server, backend.list_models(server.id).await) }
                    },
                ))
                .await;
            if generation.try_get_value() != Some(request)
                || auth.generation.get_untracked() != epoch
            {
                return;
            }
            let mut options = Vec::new();
            let mut failures = Vec::new();
            for (server, result) in results {
                match result {
                    Ok(models) => {
                        for model in models {
                            let selection = openwebide_core::ModelSelection {
                                server_id: server.id,
                                model: model.name.clone(),
                            };
                            options.push(SelectOption::new(
                                serde_json::to_string(&selection)
                                    .expect("model selection serializes"),
                                format!("{} @ {}", model.name, server.name),
                            ));
                        }
                    }
                    Err(message) => failures.push(format!("{}: {message}", server.name)),
                }
            }
            choices.set(options);
            error.set((!failures.is_empty()).then(|| failures.join("; ")));
        });
    });
    view! {
        <FormField label="Model">
            <DropdownSelect label="Task model" disabled=Signal::derive(move ||state.busy.get())
                value=Signal::derive(move ||state.model.get().map(|model| serde_json::to_string(&model).expect("model selection serializes")).unwrap_or_default())
                options=Signal::derive(move || {
                    let mut options = vec![SelectOption::new("", "Current session model")];
                    options.extend(choices.get());
                    if let Some(model) = state.model.get() {
                        let value = serde_json::to_string(&model).expect("model selection serializes");
                        if !options.iter().any(|option| option.value == value) {
                            options.push(SelectOption::new(value, format!("{} @ {}", model.model, settings.connections.with(|servers| servers.iter().find(|server|server.id==model.server_id).map_or_else(||"Unavailable server".to_string(),|server|server.name.clone())))));
                        }
                    }
                    options
                })
                on_change=Callback::new(move |value:String|state.model.set(serde_json::from_str(&value).ok()))/>
            <p class="form-hint">"Overrides apply only to this task’s runs."</p>
            <Show when=move ||error.get().is_some()><p class="form-hint" role="alert">{move ||error.get().unwrap_or_default()}</p></Show>
        </FormField>
    }
}
#[component]
pub fn ScheduledTasks(on_select: Callback<i64>) -> impl IntoView {
    let state = expect_context::<TasksState>();
    let actions = expect_context::<TaskActions>();
    let projects = expect_context::<ProjectsState>();
    let chat = expect_context::<ChatState>();
    let api = expect_context::<Api>();
    let settings = expect_context::<crate::state::settings::SettingsState>();
    let paused = RwSignal::new(false);
    view! {
        <section class="sidebar-section scheduled-tasks" aria-label="Scheduled tasks">
            <div class="section-header"><h2>"Tasks"</h2><IconButton label="New task".to_string() disabled=Signal::derive(move ||state.busy.get()) on_click=Callback::new(move |_|actions.edit.run(None))><Icon name=IconName::Plus/></IconButton></div>
            <div class="session-filters"><super::ui::SegmentedControl options=vec![super::ui::SegmentOption::new("Active",false),super::ui::SegmentOption::new("Paused",true)] value=Signal::derive(move ||paused.get()) on_change=Callback::new(move |value|paused.set(value))/></div>
            <Show when=move ||state.error.get().is_some()><p class="form-hint" role="alert">{move ||state.error.get().unwrap_or_default()}</p></Show>
            <Show when=move ||state.editing.get()>
                <div class="memory-editor">
                    <FormField label="Title"><TextInput label="Task title" placeholder="Name automatically" maxlength=120 value=state.title on_change=Callback::new(move |value:String|{state.auto_title.set(value.trim().is_empty());state.title.set(value);}) disabled=Signal::derive(move ||state.busy.get())/></FormField>
                    <FormField label="Prompt"><TextArea label="Task prompt" maxlength=32768 value=state.prompt on_change=Callback::new(move |value|state.prompt.set(value)) disabled=Signal::derive(move ||state.busy.get())/></FormField>
                    <FormField label="Session">
                        <DropdownSelect label="Session" disabled=Signal::derive(move ||state.busy.get())
                            value=Signal::derive(move ||match state.session_target.get(){SessionTarget::New=>"new".into(),SessionTarget::Latest=>"latest".into(),SessionTarget::Existing=>state.session.get().to_string()})
                            options=Signal::derive(move ||{
                                let mut options=vec![SelectOption::new("new","New session each run"),SelectOption::new("latest","Latest active session")];
                                options.extend(chat.sessions.with(|sessions|sessions.iter().filter(|session|session.project_id==projects.active_project.get()).map(|session|SelectOption::new(session.id.to_string(),session.name.clone())).collect::<Vec<_>>()));options
                            })
                            on_change=Callback::new(move |value:String| match value.as_str(){"new"=>state.session_target.set(SessionTarget::New),"latest"=>state.session_target.set(SessionTarget::Latest),_=>{state.session.set(value.parse().unwrap_or(0));state.session_target.set(SessionTarget::Existing);}})/>
                    </FormField>
                    <TaskModel/>
                    <FormField label="Schedule"><SegmentedControl class="task-schedule" options=vec![SegmentOption::new("Repeating","weekly".to_string()),SegmentOption::new("One time","once".to_string()),SegmentOption::new("Cron","custom".to_string())].into_iter().map(|option|option.disabled_when(Signal::derive(move ||state.busy.get()))).collect()
                        value=Signal::derive(move ||state.kind.get()) on_change=Callback::new(move |kind|state.kind.set(kind))/></FormField>
                    <Show when=move ||state.kind.get()=="weekly">
                        <div class="task-weekdays" role="group" aria-label="Repeat on weekdays">{["Sun","Mon","Tue","Wed","Thu","Fri","Sat"].into_iter().enumerate().map(|(day,label)|{let day=u8::try_from(day).expect("weekday fits");view!{<super::ui::CheckboxField label=label checked=Signal::derive(move ||state.days.with(|days|days.contains(&day))) disabled=Signal::derive(move ||state.busy.get()) on_change=Callback::new(move |checked|state.days.update(|days|{days.retain(|value|*value!=day);if checked{days.push(day);}}))/>}}).collect::<Vec<_>>()}</div>
                        <FormField label="Time"><input class="form-input" aria-label="Task time" type="time" disabled=move ||state.busy.get() prop:value=move ||state.time.get() on:input=move |event|state.time.set(event_target_value(&event))/></FormField>
                    </Show>
                    <Show when=move ||state.kind.get()=="once"><FormField label="Date and time"><input class="form-input" aria-label="Task date and time" type="datetime-local" disabled=move ||state.busy.get() prop:value=move ||state.date.get() on:input=move |event|state.date.set(event_target_value(&event))/></FormField></Show>
                    <Show when=move ||state.kind.get()=="custom"><CronInput value=state.cron on_change=Callback::new(move |value|state.cron.set(value)) disabled=Signal::derive(move ||state.busy.get())/></Show>
                    <p class="form-hint">{move ||format!("Times in {}. {}",state.timezone.get(),match state.session_target.get(){SessionTarget::Existing=>"Runs use this session’s current model unless overridden, and its approval settings.",SessionTarget::New=>"Each run creates a session using your default model and normal approvals.",SessionTarget::Latest=>"Uses the most recently used, unarchived session; creates one using your defaults if needed."})}</p>
                    <div class="form-actions"><Button disabled=Signal::derive(move ||state.busy.get()) on_click=Callback::new(move |_|actions.save.run(()))>"Save task"</Button><Button variant=ButtonVariant::Ghost on_click=Callback::new(move |_|actions.cancel.run(()))>"Cancel"</Button></div>
                </div>
            </Show>
            <For each=move ||state.entries.with(|entries|entries.iter().filter(|entry|entry.draft.enabled!=paused.get()).cloned().collect::<Vec<_>>()) key=|entry|(entry.id,entry.revision,entry.next_run,entry.host_available,entry.last_run.clone()) children=move |entry|{
                let id=entry.id;let revision=entry.revision;let enabled=entry.draft.enabled;let session=entry.last_run.as_ref().and_then(|run|run.session_id).unwrap_or(if entry.draft.session_target==SessionTarget::Existing {entry.draft.session_id}else{0});let schedule_label=match &entry.draft.schedule{openwebide_core::scheduled::Schedule::Once{at}=>format!("One time: {}",crate::scheduled::time(*at)),openwebide_core::scheduled::Schedule::Cron{expression,timezone}=>openwebide_core::scheduled::calendar_cron(expression).map_or_else(||format!("Custom schedule ({timezone})"),|(time,days)|format!("{} at {time} ({timezone})",days.iter().map(|day|["Sun","Mon","Tue","Wed","Thu","Fri","Sat"][usize::from(*day)]).collect::<Vec<_>>().join(", ")))};
                let model_label=entry.draft.model.as_ref().map_or_else(||"Current session model".to_string(),|model|format!("{} @ {}",model.model,settings.connections.with(|servers|servers.iter().find(|server|server.id==model.server_id).map_or_else(||"Unavailable server".to_string(),|server|server.name.clone()))));let edit=entry.clone();let title=entry.draft.title.clone();let prompt=entry.draft.prompt;let next=entry.next_run;let host_available=entry.host_available;let last=entry.last_run;
                view!{<super::ui::DisclosurePanel class="memory-entry" summary=move ||view!{<span class="memory-title">{title.clone()}</span>}>
                    <p class="memory-content">{prompt}</p><p class="form-hint">{schedule_label}</p><p class="form-hint">{model_label}</p><p class="form-hint">{next.map_or_else(||"No upcoming run".into(),|next|format!("Next: {}",crate::scheduled::time(next)))}</p>
                    <Show when=move ||!host_available><p class="form-hint">"Execution host unavailable. This task will wait for it to reconnect."</p></Show>
                    {last.map(|last|{let running=matches!(last.status.as_str(),"running"|"blocked");let permission=last.permission_id;let detail=last.detail;view!{<p class="form-hint">{format!("Last: {} · {}",crate::scheduled::time(last.due_at),last.status)}</p><p class="form-hint">{detail}</p>
                        {permission.map(|permission|{let deny=permission.clone();view!{<div class="form-actions"><Button variant=ButtonVariant::Success on_click=Callback::new(move |_|{let permission=permission.clone();leptos::task::spawn_local(async move{if let Err(error)=api.with_value(Clone::clone).set_permission(session,&permission,true).await{state.error.set(Some(error));}});})>"Approve"</Button><Button variant=ButtonVariant::Danger on_click=Callback::new(move |_|{let deny=deny.clone();leptos::task::spawn_local(async move{if let Err(error)=api.with_value(Clone::clone).set_permission(session,&deny,false).await{state.error.set(Some(error));}});})>"Deny"</Button></div>}})}
                        {running.then(||view!{<Button variant=ButtonVariant::Danger class="stop" on_click=Callback::new(move |_|leptos::task::spawn_local(async move{if let Err(error)=api.with_value(Clone::clone).cancel_session(session).await{state.error.set(Some(error));}}))>"Stop"</Button>})}
                    }})}
                    <div class="form-actions">
                        <Button variant=ButtonVariant::Ghost disabled=Signal::derive(move ||session<=0) on_click=Callback::new(move |_|on_select.run(session))>"Open session"</Button>
                        <Button variant=ButtonVariant::Ghost disabled=Signal::derive(move ||state.busy.get()) on_click=Callback::new(move |_|actions.edit.run(Some(edit.clone())))>"Edit"</Button>
                        <Button variant=ButtonVariant::Ghost disabled=Signal::derive(move ||state.busy.get()) aria_label=if enabled {"Pause task"} else {"Resume task"} on_click=Callback::new(move |_|actions.command.run(TaskCommand::SetEnabled{id,revision,enabled:!enabled}))>{if enabled{"Pause"}else{"Resume"}}</Button>
                        <Button variant=ButtonVariant::Ghost disabled=Signal::derive(move ||state.busy.get()) on_click=Callback::new(move |_|actions.command.run(TaskCommand::Delete{id,revision}))>"Delete"</Button>
                    </div>
                </super::ui::DisclosurePanel>}
            }/>
            <Show when=move ||state.entries.with(|entries|!entries.iter().any(|entry|entry.draft.enabled!=paused.get())) && state.loaded.get()><p class="empty">{move ||if paused.get(){"No paused tasks."}else{"No scheduled tasks yet."}}</p></Show>
        </section>
    }
}
