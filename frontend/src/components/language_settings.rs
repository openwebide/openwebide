use super::{
    dropdown::{DropdownSelect, SelectOption},
    ui::{FormField, FormSection},
};
use crate::{
    project_languages::ProjectLanguageActions,
    state::{languages::LanguagesState, projects::ProjectsState},
};
use leptos::prelude::*;
use openwebide_core::plugins::languages::ProviderPreference;

#[component]
pub fn LanguageProviderSettings() -> impl IntoView {
    let (Some(state), Some(actions), Some(projects)) = (
        use_context::<LanguagesState>(),
        use_context::<ProjectLanguageActions>(),
        use_context::<ProjectsState>(),
    ) else {
        return ().into_any();
    };
    let project_scope = RwSignal::new(false);
    Effect::new(move |_| {
        if projects.active_project.get().is_none() {
            project_scope.set(false);
        }
    });
    view! {
        <FormSection title="Idle language analysis" description="Prepare diagnostics and symbols while idle in all projects. Uses additional CPU and memory.">
            <super::ui::CheckboxField label="Warm language services while idle"
                checked=Signal::derive(move || state.prefetch_enabled.get())
                disabled=Signal::derive(move || !actions.ready.get())
                on_change=actions.set_prefetch />
            <Show when=move || state.prefetch_error.with(Option::is_some)>
                <p class="form-hint" role="alert">{move || state.prefetch_error.get()}</p>
            </Show>
            <Show when=move || state.prefetch_saving.get()>
                <p class="form-hint" role="status">"Saving idle analysis preference…"</p>
            </Show>
        </FormSection>
        <FormSection title="Language providers" description="Choose installed language support. Project choices override your defaults on every device.">
            <FormField label="Apply choices to">
                <DropdownSelect label="Language settings scope"
                    value=Signal::derive(move || if project_scope.get() && projects.active_project.get().is_some() { "project" } else { "default" }.to_string())
                    options=Signal::derive(move || {
                        let mut options = vec![SelectOption::new("default", "My defaults")];
                        if projects.active_project.get().is_some() { options.push(SelectOption::new("project", "Current project")); }
                        options
                    })
                    on_change=Callback::new(move |value: String| project_scope.set(value == "project")) />
            </FormField>
            <Show when=move || state.settings_error.with(Option::is_some)>
                <p class="form-hint" role="alert">{move || state.settings_error.get()}</p>
                <button type="button" class="btn ghost" disabled=move || state.settings_saving.get()
                    on:click=move |_| actions.reload.run(())>"Reload preferences"</button>
            </Show>
            <For each=move || actions.registry.with(|registry| registry.providers.iter()
                    .filter(|provider| provider.supports_editor())
                    .map(|provider| (provider.definition.id.clone(), provider.definition.name.clone()))
                    .collect::<std::collections::BTreeMap<_, _>>().into_iter().collect::<Vec<_>>())
                key=|(id, _)| id.clone() children=move |(id, name)| {
                    let value_id = id.clone(); let options_id = id.clone(); let change_id = id;
                    view! {
                        <FormField label=name>
                            <DropdownSelect label="Language provider"
                                value=Signal::derive(move || state.settings.with(|settings| {
                                    let preferences = if project_scope.get() { projects.active_project.get().and_then(|id| settings.projects.get(&id)) } else { Some(&settings.defaults) };
                                    match preferences.and_then(|preferences| preferences.providers.get(&value_id)) {
                                        None if project_scope.get() => "inherit".into(),
                                        Some(ProviderPreference::Disabled) => "disabled".into(),
                                        Some(ProviderPreference::Provider { namespace }) => namespace.clone(),
                                        _ => "automatic".into(),
                                    }
                                }))
                                options=Signal::derive(move || {
                                    let mut options = Vec::new();
                                    if project_scope.get() { options.push(SelectOption::new("inherit", "Use my default")); }
                                    options.push(SelectOption::new("automatic", "Automatic"));
                                    options.push(SelectOption::new("disabled", "Disabled"));
                                    actions.registry.with(|registry| {
                                        options.extend(registry.providers.iter().filter(|provider| provider.definition.id == options_id && provider.supports_editor())
                                            .map(|provider| SelectOption::new(provider.namespace.clone(), provider.plugin_label.clone())));
                                    });
                                    options
                                })
                                disabled=Signal::derive(move || !actions.ready.get())
                                on_change=Callback::new(move |value: String| {
                                    let mut settings = state.settings.get_untracked();
                                    let preferences = if project_scope.get_untracked() {
                                        let Some(project) = projects.active_project.get_untracked() else { return };
                                        settings.projects.entry(project).or_default()
                                    } else { &mut settings.defaults };
                                    if value == "inherit" { preferences.providers.remove(&change_id); }
                                    else {
                                        let preference = match value.as_str() {
                                            "automatic" => ProviderPreference::Automatic,
                                            "disabled" => ProviderPreference::Disabled,
                                            _ => ProviderPreference::Provider { namespace: value },
                                        };
                                        preferences.providers.insert(change_id.clone(), preference);
                                    }
                                    actions.save.run(settings);
                                }) />
                        </FormField>
                    }
                } />
            <For each=move || actions.registry.with(|registry| registry.providers.iter()
                    .filter(|provider| provider.definition.features.language_server.as_ref().is_some_and(|server| !server.role.is_language()))
                    .cloned().collect::<Vec<_>>())
                key=|provider| provider.namespace.clone() children=move |provider| {
                    let value_namespace = provider.namespace.clone();
                    let change_namespace = provider.namespace;
                    view! {
                        <FormField label=format!("{} · {} diagnostics", provider.plugin_label, provider.definition.name)>
                            <DropdownSelect label="Diagnostics provider"
                                value=Signal::derive(move || state.settings.with(|settings| {
                                    let preferences = if project_scope.get() { projects.active_project.get().and_then(|id| settings.projects.get(&id)) } else { Some(&settings.defaults) };
                                    match preferences.and_then(|preferences| preferences.diagnostics.get(&value_namespace)) {
                                        Some(false) => "disabled",
                                        None if project_scope.get() => "inherit",
                                        _ => "enabled",
                                    }.to_string()
                                }))
                                options=Signal::derive(move || {
                                    let mut options = Vec::new();
                                    if project_scope.get() { options.push(SelectOption::new("inherit", "Use my default")); }
                                    options.push(SelectOption::new("enabled", "Enabled"));
                                    options.push(SelectOption::new("disabled", "Disabled"));
                                    options
                                })
                                disabled=Signal::derive(move || !actions.ready.get())
                                on_change=Callback::new(move |value: String| {
                                    let mut settings = state.settings.get_untracked();
                                    let preferences = if project_scope.get_untracked() {
                                        let Some(project) = projects.active_project.get_untracked() else { return };
                                        settings.projects.entry(project).or_default()
                                    } else { &mut settings.defaults };
                                    if value == "inherit" { preferences.diagnostics.remove(&change_namespace); }
                                    else { preferences.diagnostics.insert(change_namespace.clone(), value == "enabled"); }
                                    actions.save.run(settings);
                                }) />
                        </FormField>
                    }
                } />
            <Show when=move || actions.registry.with(|registry| registry.providers.is_empty())>
                <p class="form-hint">"Enable a language package in Plugins to add language support."</p>
            </Show>
            <Show when=move || state.settings_saving.get()><p class="form-hint" role="status">"Saving language choices…"</p></Show>
        </FormSection>
    }.into_any()
}
