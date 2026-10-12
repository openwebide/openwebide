//! Workspace navigation and scoped host views for declarative plugin panels.
use super::{
    plugin_panel_view::{PanelSubmission, PluginPanelDocument},
    tool_panel::PanelFrame,
    ui::{FormNotice, Icon, IconButton, IconName, LoadingStatus, NoticeTone},
};
use crate::{
    backend::Api,
    project_host::ProjectHost,
    state::{
        auth::AuthState,
        layout::LayoutState,
        plugin_panels::PluginPanelsState,
        projects::ProjectsState,
        ui::{ConfirmRequest, UiState},
    },
    state_actions::layout::LayoutActions,
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::plugins::panels::{registry::RegisteredPanel, *};

fn icon(icon: PanelIcon) -> IconName {
    match icon {
        PanelIcon::Puzzle => IconName::Puzzle,
        PanelIcon::GitPullRequest => IconName::GitPullRequest,
        PanelIcon::List => IconName::List,
        PanelIcon::Calendar => IconName::Calendar,
        PanelIcon::Activity => IconName::Activity,
        PanelIcon::BookOpen => IconName::BookOpen,
        PanelIcon::Terminal => IconName::Terminal,
    }
}
fn visible(layout: LayoutState, namespace: &str) -> bool {
    if layout.phone.get() {
        layout.plugin_sheet.get().as_deref() == Some(namespace)
    } else {
        layout.preferences.with(|prefs| {
            prefs
                .plugin_panels
                .get(namespace)
                .is_some_and(|panel| panel.visible)
        })
    }
}
fn identity() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    web_sys::window()
        .ok_or("Browser unavailable")?
        .crypto()
        .map_err(|_| "Browser randomness unavailable")?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(|_| "Browser randomness unavailable")?;
    use std::fmt::Write;
    let mut id = String::with_capacity(32);
    for byte in bytes {
        write!(&mut id, "{byte:02x}").expect("String formatting");
    }
    Ok(id)
}

#[component]
pub fn PluginPanelRailItems() -> impl IntoView {
    let state = use_context::<PluginPanelsState>();
    let layout = expect_context::<LayoutState>();
    let actions = expect_context::<LayoutActions>();
    view! { <For each=move || state.map(|state|state.registry.get().panels).unwrap_or_default()
    key=|panel| (panel.namespace.clone(),panel.digest.clone()) children=move |panel| {
        let key = StoredValue::new(panel.namespace.clone());
        let controls = format!("panel-{}",panel.namespace);
        let title = format!("Show or collapse {}",panel.definition.title);
        view! { <div class="panel-tab-group"><button type="button" class="btn panel-tab" class:is-open=move || key.with_value(|key|visible(layout,key))
            aria-current=move ||if layout.phone.get() && key.with_value(|key|visible(layout,key)) {Some("page")}else{None}
            title=title aria-controls=controls aria-expanded=move ||key.with_value(|key|visible(layout,key)).to_string()
            on:click=move |_| { let namespace=key.get_value();let open=layout.phone.get_untracked() || !untrack(||visible(layout,&namespace));actions.plugin_panel.run((namespace,open)); }>
            <Icon name=icon(panel.definition.icon)/><span>{panel.definition.title}</span>
        </button></div> }
    }/> }
}

#[component]
pub fn PluginPanels() -> impl IntoView {
    let state = expect_context::<PluginPanelsState>();
    view! { <For each=move ||state.registry.get().panels key=|panel|(panel.namespace.clone(),panel.digest.clone()) children=move |panel|view! { <PluginPanel panel/> }/> }
}

#[component]
fn PluginPanel(panel: RegisteredPanel) -> impl IntoView {
    let layout = expect_context::<LayoutState>();
    let actions = expect_context::<LayoutActions>();
    let registration = StoredValue::new(panel.clone());
    let key = StoredValue::new(panel.namespace.clone());
    let open: Signal<bool> = Memo::new(move |_| key.with_value(|key| visible(layout, key))).into();
    let title = panel.definition.title.clone();
    view! { <PanelFrame id=panel.namespace label=title.clone() visible=open center=Signal::derive(||false)
        order=Signal::derive(move ||key.with_value(|key|layout.preferences.with(|prefs|prefs.order(key)))) width=Signal::derive(||360.0)>
        <div class="tool-panel-heading" data-context-menu=""><span>{title}</span>
            <super::dropdown::ActionMenu aria_label="Panel actions">
                <button role="menuitem" type="button" class="ui-dropdown-item recent-item" on:click=move |_| actions.move_plugin_panel.run((key.get_value(),false))><Icon name=IconName::ArrowLeft/><span>"Move panel left"</span></button>
                <button role="menuitem" type="button" class="ui-dropdown-item recent-item" on:click=move |_| actions.move_plugin_panel.run((key.get_value(),true))><Icon name=IconName::ArrowRight/><span>"Move panel right"</span></button>
            </super::dropdown::ActionMenu>
            <IconButton label=format!("Minimize {}",panel.definition.title) class="panel-minimize" on_click=Callback::new(move |_|actions.plugin_panel.run((key.get_value(),false)))><Icon name=IconName::Minus/></IconButton>
            <Show when=move ||layout.phone.get()><button type="button" class="btn ghost" title="Return to chat" on:click=move |_|actions.show.run(crate::state::layout::Panel::Chat)>"Back to chat"</button></Show>
        </div>
        <div class="tool-panel-content plugin-panel-content"><HostPanel registration=registration.get_value() visible=open/></div>
    </PanelFrame> }
}

#[component]
fn HostPanel(registration: RegisteredPanel, visible: Signal<bool>) -> impl IntoView {
    let api = expect_context::<Api>();
    let host = expect_context::<ProjectHost>();
    let auth = expect_context::<AuthState>();
    let projects = expect_context::<ProjectsState>();
    let registry = expect_context::<PluginPanelsState>();
    let ui = expect_context::<UiState>();
    let panel = StoredValue::new(registration);
    let document = RwSignal::new(None::<PanelDocument>);
    let instance = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let dirty = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let generation = RwSignal::new(0u64);
    let invoke = Callback::new(
        move |(submission, expected, approved): (
            Option<PanelSubmission>,
            Option<(String, u64)>,
            bool,
        )| {
            if busy.get_untracked() || !visible.get_untracked() {
                return;
            }
            let previous = document.get_untracked();
            let revision = previous.as_ref().map_or(0, |document| document.revision);
            if expected.is_some_and(|(expected_instance, expected)| {
                expected != revision
                    || instance.get_untracked().as_deref() != Some(expected_instance.as_str())
            }) {
                ui.notify("Panel changed; review the current view before trying again");
                return;
            }
            let id = match instance.get_untracked().map_or_else(identity, Ok) {
                Ok(id) => id,
                Err(message) => {
                    error.set(Some(message));
                    return;
                }
            };
            instance.set(Some(id.clone()));
            let request = match identity() {
                Ok(id) => id,
                Err(message) => {
                    error.set(Some(message));
                    return;
                }
            };
            let registration = panel.get_value();
            let (action, handler, values) = if let Some(submission) = submission {
                let Some(action) = registration
                    .definition
                    .actions
                    .iter()
                    .find(|action| action.id == submission.action)
                else {
                    error.set(Some("Panel action is not declared".into()));
                    return;
                };
                (
                    Some(submission.action),
                    action.handler.clone(),
                    submission.values,
                )
            } else {
                (
                    None,
                    registration.definition.view.clone(),
                    Default::default(),
                )
            };
            let input = PanelInput {
                panel: registration.definition.id.clone(),
                instance: id,
                request,
                revision,
                handler,
                action,
                values,
                state: previous.map_or(serde_json::Value::Null, |document| document.state),
            };
            let epoch = auth.generation.get_untracked();
            let project = projects.active_project.get_untracked();
            let current_generation = generation.get_untracked();
            let namespace = registration.namespace.clone();
            let digest = registration.digest.clone();
            let current = move || {
                auth.generation.try_get_untracked() == Some(epoch)
                    && projects.active_project.try_get_untracked() == Some(project)
                    && generation.try_get_untracked() == Some(current_generation)
                    && visible.try_get_untracked() == Some(true)
                    && registry
                        .registry
                        .try_with_untracked(|registry| {
                            registry
                                .find(&namespace)
                                .is_some_and(|panel| panel.digest == digest)
                        })
                        .unwrap_or(false)
            };
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                let result = crate::plugin_actions::invoke_scoped_plugin_panel(
                    api,
                    host,
                    project,
                    &registration,
                    &input,
                    approved,
                    current.clone(),
                )
                .await;
                if !current() {
                    return;
                }
                busy.set(false);
                match result {
                    Ok(view) => {
                        dirty.set(false);
                        document.set(Some(view));
                    }
                    Err(message) => error.set(Some(message)),
                }
            });
        },
    );
    Effect::new(move |_| {
        let open = visible.get();
        auth.generation.track();
        projects.active_project.track();
        generation.update(|value| *value += 1);
        busy.set(false);
        if let Some(id) = instance.get_untracked() {
            instance.set(None);
            let backend = api.with_value(Clone::clone);
            spawn_local(async move {
                let _ = backend.close_plugin_panel(&id).await;
            });
        }
        document.set(None);
        dirty.set(false);
        error.set(None);
        if open {
            invoke.run((None, None, false));
        }
    });
    let refresh = Callback::new(move |_| {
        if error.get_untracked().is_some() {
            generation.update(|value| *value += 1);
            if let Some(id) = instance.get_untracked() {
                let backend = api.with_value(Clone::clone);
                spawn_local(async move {
                    let _ = backend.close_plugin_panel(&id).await;
                });
            }
            instance.set(None);
            document.set(None);
            busy.set(false);
        }
        invoke.run((None, None, false));
    });
    let seconds = panel.with_value(|panel| panel.definition.refresh_seconds);
    let timer = seconds.and_then(|seconds| {
        set_interval_with_handle(
            move || {
                if visible.get_untracked()
                    && web_sys::window()
                        .and_then(|window| window.document())
                        .is_some_and(|document| !document.hidden())
                    && document.get_untracked().is_some()
                    && error.get_untracked().is_none()
                    && !dirty.get_untracked()
                    && ui.confirm.get_untracked().is_none()
                {
                    invoke.run((None, None, false));
                }
            },
            std::time::Duration::from_secs(u64::from(seconds)),
        )
        .ok()
    });
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
        if let Some(Some(id)) = instance.try_get_untracked()
            && let Some(backend) = api.try_with_value(Clone::clone)
        {
            spawn_local(async move {
                let _ = backend.close_plugin_panel(&id).await;
            });
        }
    });
    view! { <div class="plugin-panel-host">
        <div class="ui-inline-actions plugin-panel-feedback">
            <Show when=move ||busy.get()><LoadingStatus label="Loading panel…" compact=true/></Show>
            <IconButton label="Refresh panel" disabled=busy on_click=refresh><Icon name=IconName::RefreshCw/></IconButton>
        </div>
        <Show when=move ||error.get().is_some()><FormNotice tone=NoticeTone::Error>{move ||error.get().unwrap_or_default()}</FormNotice></Show>
        <For each={move ||document.get().and_then(|document|instance.get().map(|id|(id,document))).into_iter().collect::<Vec<_>>()} key=|(id,document)|(id.clone(),document.revision) children=move |(view_instance,view)| {
            let revision=view.revision;
            let submit=Callback::new(move |submission: PanelSubmission| {
                let registration=panel.get_value();
                let Some(action)=registration.definition.actions.iter().find(|action|action.id==submission.action) else {return;};
                if action.requires_approval {
                    let title=format!("{} — {}",registration.definition.title,action.title);
                    let message=format!("Plugin: {}\nAction: {}\nInputs:\n{}",registration.plugin_label,action.title,serde_json::to_string_pretty(&document.with_untracked(|view|view.as_ref().map_or_else(Default::default,|view|approval_values(view,&submission.values)))).unwrap_or_default());
                    let epoch=auth.generation.get_untracked();
                    let project=projects.active_project.get_untracked();
                    let request_generation=generation.get_untracked();
                    let view_instance=view_instance.clone();
                    ui.set_confirm(ConfirmRequest {title,message,confirm_label:action.title.clone(),action:Callback::new(move |()| {
                        if auth.generation.try_get_untracked()==Some(epoch)
                            && projects.active_project.try_get_untracked()==Some(project)
                            && generation.try_get_untracked()==Some(request_generation)
                            && visible.try_get_untracked()==Some(true)
                            && instance.try_get_untracked().flatten().as_deref()==Some(view_instance.as_str())
                            && document.try_with_untracked(|document|document.as_ref().is_some_and(|document|document.revision==revision)).unwrap_or(false) {
                            invoke.run((Some(submission.clone()),Some((view_instance.clone(),revision)),true));
                        } else {ui.notify("Panel changed; review the current view before trying again");}
                    })});
                } else {invoke.run((Some(submission),Some((view_instance.clone(),revision)),false));}
            });
            view! { <PluginPanelDocument panel=panel.with_value(|panel|panel.definition.clone()) document=view busy=busy.into() on_action=submit on_edit=Callback::new(move |()|dirty.set(true))/> }
        }/>
    </div> }
}
