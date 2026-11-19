use super::ui::{CheckboxField, FormField, FormNotice, FormSection, InlineActions, NoticeTone};
use crate::{
    backend::Api,
    state::{auth::AuthState, settings::SettingsState},
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use openwebide_core::{ModelSelection, ModelSettings, ServerSettingsUpdate};

#[derive(Clone, Debug)]
struct ModelOption {
    selection: ModelSelection,
    label: String,
}
fn selection_value(selection: &Option<ModelSelection>) -> String {
    selection
        .as_ref()
        .and_then(|selection| serde_json::to_string(selection).ok())
        .unwrap_or_default()
}

#[component]
fn ModelChoice(
    label: &'static str,
    value: Signal<Option<ModelSelection>>,
    on_change: Callback<Option<ModelSelection>>,
    choices: ReadSignal<Vec<ModelOption>>,
    empty: &'static str,
) -> impl IntoView {
    view! { <FormField label=label><super::dropdown::DropdownSelect label=label value=Signal::derive(move || selection_value(&value.get())) options=Signal::derive(move || {
        let mut options = vec![super::dropdown::SelectOption::new("", empty)];
        options.extend(choices.get().into_iter().map(|item| super::dropdown::SelectOption::new(selection_value(&Some(item.selection)), item.label))); options
    }) on_change=Callback::new(move |selection: String| on_change.run(serde_json::from_str(&selection).ok())) /></FormField> }
}

#[component]
fn TextSetting(
    label: &'static str,
    value: RwSignal<String>,
    #[prop(default = "text")] input_type: &'static str,
    #[prop(default = "")] placeholder: &'static str,
) -> impl IntoView {
    view! {
        <FormField label=label>
            <input class="form-input" type=input_type placeholder=placeholder prop:value=move || value.get()
                on:input=move |event| value.set(event_target_value(&event)) />
        </FormField>
    }
}

#[component]
pub fn ModelSetupPanel(
    #[prop(default = false)] defaults_only: bool,
    #[prop(optional_no_strip)] server: Option<i64>,
) -> impl IntoView {
    let api = expect_context::<Api>();
    let auth = expect_context::<AuthState>();
    let settings = expect_context::<SettingsState>();
    let choices = RwSignal::new(Vec::<ModelOption>::new());
    let ui = expect_context::<crate::state::ui::UiState>();
    let loading = RwSignal::new(false);
    let error = RwSignal::new(Option::<String>::None);
    let reload = RwSignal::new(0_u64);
    let generation = StoredValue::new(0_u64);
    Effect::new(move |_| {
        reload.track();
        let connections = settings
            .connections
            .get()
            .into_iter()
            .filter(|connection| server.is_none_or(|id| id == connection.id))
            .collect::<Vec<_>>();
        let epoch = auth.generation.get();
        generation.update_value(|value| *value += 1);
        let request = generation.get_value();
        let defaults_revision = settings.model_defaults_revision.get_untracked();
        loading.set(true);
        spawn_local(async move {
            let backend = api.with_value(Clone::clone);
            let results =
                futures::future::join_all(connections.iter().filter(|server| server.enabled).map(
                    |server| {
                        let backend = &backend;
                        async move {
                            let models = backend.list_models(server.id).await;
                            (server, models)
                        }
                    },
                ))
                .await;
            let setup = backend.model_setup().await;
            if generation.try_get_value() != Some(request)
                || auth.generation.get_untracked() != epoch
            {
                return;
            }
            if let Ok(mut setup) = setup {
                if settings.model_defaults_revision.get_untracked() != defaults_revision
                    || settings.model_defaults_saving.get_untracked() == Some(epoch)
                    || settings.model_defaults_error.with_untracked(|error| {
                        error.as_ref().is_some_and(|(account, _)| *account == epoch)
                    })
                {
                    setup.defaults = settings.model_setup.get_untracked().defaults;
                }
                settings.model_setup.set(setup);
            }
            let mut items = Vec::new();
            let mut failures = Vec::new();
            for (server, result) in results {
                match result {
                    Ok(models) => {
                        for model in models {
                            items.push(ModelOption {
                                label: model.name.clone(),
                                selection: ModelSelection {
                                    server_id: server.id,
                                    model: model.name,
                                },
                            });
                        }
                    }
                    Err(message) => failures.push(format!("{}: {message}", server.name)),
                }
            }
            let mut names = std::collections::BTreeMap::new();
            for item in &items {
                *names.entry(item.label.clone()).or_insert(0_usize) += 1;
            }
            for item in &mut items {
                if names[&item.label] > 1
                    && let Some(server) = connections
                        .iter()
                        .find(|server| server.id == item.selection.server_id)
                {
                    item.label = format!("{} @ {}", item.label, server.base_url);
                }
            }
            if !failures.is_empty() {
                error.set(Some(failures.join("; ")));
            }
            choices.set(items);
            loading.set(false);
        });
    });
    let project_host = expect_context::<crate::project_host::ProjectHost>();
    let candidates = RwSignal::new(Vec::<openwebide_core::ServerDiscovery>::new());
    let discovering = RwSignal::new(false);
    let project = expect_context::<crate::state::projects::ProjectsState>().active_project;
    let discover = Callback::new(move |()| {
        let epoch = auth.generation.get_untracked();
        let revision = project_host.revision();
        let project_id = project.get_untracked();
        discovering.set(true);
        error.set(None);
        spawn_local(async move {
            let result = project_host.discover_servers().await;
            if auth.generation.try_get_untracked() != Some(epoch)
                || discovering.try_get_untracked().is_none()
            {
                return;
            }
            discovering.set(false);
            if project_host.revision() != revision
                || project.try_get_untracked() != Some(project_id)
            {
                return;
            }
            match result {
                Ok(found) => candidates.set(found),
                Err(message) => error.set(Some(message)),
            }
        });
    });
    let first_discovery = StoredValue::new(false);
    Effect::new(move |_| {
        if !defaults_only && settings.connections.get().is_empty() && !first_discovery.get_value() {
            first_discovery.set_value(true);
            discover.run(());
        }
    });
    let save = Callback::new(move |defaults| {
        crate::state_actions::settings::set_model_defaults(api, settings, auth, ui, defaults);
    });
    let save_error = Memo::new(move |_| {
        settings
            .model_defaults_error
            .get()
            .filter(|(epoch, _)| *epoch == auth.generation.get())
            .map(|(_, error)| error)
    });
    view! {
        <div class="model-setup">
            <Show when=move || !defaults_only>
                <p class="form-hint">"Configuration is shared by everyone using this server and model."</p>
            <button class="btn" disabled=move || discovering.get() on:click=move |_| discover.run(())>{move || if discovering.get() { "Looking for local model servers…" } else { "Discover local servers" }}</button>
            <p class="form-hint">"Checks standard model-server ports on the project’s execution host. Add other machines by URL in Servers."</p>
            <For each=move || candidates.get() key=|server| server.base_url.clone() children=move |candidate| {
                let label = format!("Set up {} @ {}", candidate.kind.display_name(), candidate.base_url);
                let url = candidate.base_url.clone();
                view! { <button class="btn" on:click=move |_| {
                    let existing = settings.connections.with_untracked(|servers| servers.iter().find(|server| server.base_url.trim_end_matches('/') == url.trim_end_matches('/')).map(|server| server.id));
                    settings.begin_model_setup(existing);
                    settings.conn_kind.set(candidate.kind);
                    settings.conn_base_url.set(url.clone());
                }>{label}</button> }
            } />
            <Show when=move || loading.get()><p class="form-hint">"Loading models from saved servers…"</p></Show>
            </Show>
            <Show when=move || defaults_only>
            <ModelChoice label="Default model" value=Signal::derive(move || settings.model_setup.get().defaults.primary) on_change=Callback::new(move |selection| {
                let mut defaults = settings.model_setup.get_untracked().defaults; defaults.primary = selection; save.run(defaults);
            }) choices=choices.read_only() empty="Use default server’s model" />
            <ModelChoice label="Assistance model" value=Signal::derive(move || settings.model_setup.get().defaults.fast) on_change=Callback::new(move |selection| {
                let mut defaults = settings.model_setup.get_untracked().defaults; defaults.fast = selection; save.run(defaults);
            }) choices=choices.read_only() empty="Use the primary model" />
            <p class="form-hint">"Used for automatic names, summaries, suggestions, Git drafts, search, Auto approvals, context compaction and delegated tasks. Falls back to the primary model."</p>

            </Show>
            <InlineActions><button class="btn" disabled=move || loading.get() on:click=move |_| reload.update(|value| *value += 1)>"Refresh models"</button><Show when=move || defaults_only><span class="form-hint" role="status" aria-live="polite">{move || if settings.model_defaults_saving.get() == Some(auth.generation.get()) { "Saving…" } else if save_error.get().is_some() { "Not saved" } else { "Changes save automatically." }}</span></Show></InlineActions>
            <Show when=move || defaults_only && save_error.get().is_some()><FormNotice tone=NoticeTone::Error>{move || save_error.get()}<button class="btn" on:click=move |_| save.run(settings.model_setup.get_untracked().defaults)>"Retry"</button></FormNotice></Show>
            <Show when=move || error.get().is_some()><FormNotice tone=NoticeTone::Error>{move || error.get()}</FormNotice></Show>

        </div>
    }
}

#[component]
pub(crate) fn ModelSettingsEditor(
    selection: ModelSelection,
    #[prop(optional_no_strip)] initial: Option<ModelSettings>,
    #[prop(optional_no_strip)] discovered: Option<openwebide_core::ModelDetection>,
    #[prop(optional)] on_edit: Option<Callback<Result<ModelSettings, String>>>,
    #[prop(default = true)] auto_detect: bool,
    #[prop(optional)] on_busy: Option<Callback<bool>>,
    #[prop(optional_no_strip)] initial_error: Option<String>,
    #[prop(optional_no_strip)] probe: Option<openwebide_core::ModelProbe>,
) -> impl IntoView {
    let api = expect_context::<Api>();
    let auth = expect_context::<AuthState>();
    let state = expect_context::<SettingsState>();
    let budget_selection = probe
        .as_ref()
        .and_then(|probe| probe.transport.tool_selection.clone());
    let budget_server = selection.server_id;
    let saved = initial.unwrap_or_else(|| {
        state.model_setup.with_untracked(|setup| {
            setup
                .profiles
                .iter()
                .find(|profile| profile.selection == selection)
                .map(|profile| profile.settings.clone())
                .unwrap_or_default()
        })
    });
    let context = RwSignal::new(
        saved
            .context_limit
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    let output = RwSignal::new(
        saved
            .max_output_tokens
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    let threshold = RwSignal::new(
        saved
            .auto_compact_threshold
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    let thinking = RwSignal::new(saved.thinking);
    let tools = RwSignal::new(saved.tools);
    let vision = RwSignal::new(saved.vision);
    let sampling = [
        "temperature",
        "top_p",
        "top_k",
        "min_p",
        "repeat_penalty",
        "seed",
    ]
    .map(|name| {
        (
            name,
            RwSignal::new(
                saved
                    .sampling
                    .get(name)
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
        )
    });
    let error = RwSignal::new(initial_error);
    let has_discovery = discovered.is_some();
    let detection = RwSignal::new(discovered);
    let detecting = RwSignal::new(false);
    let selection_for_detection = selection.clone();
    let testing = RwSignal::new(false);
    let tested = RwSignal::new(None::<openwebide_core::ModelTestResult>);
    let stream_tools = RwSignal::new(saved.stream_tools);
    let test_probe = probe.clone();
    if let Some(on_busy) = on_busy {
        Effect::new(move |_| on_busy.run(detecting.get() || testing.get()));
    }
    let detect = Callback::new(move |()| {
        let selection = selection_for_detection.clone();
        let probe = probe.clone();
        let epoch = auth.generation.get_untracked();
        detecting.set(true);
        error.set(None);
        spawn_local(async move {
            let backend = api.with_value(Clone::clone);
            let result = crate::model_setup::refresh(&*backend, &selection, probe).await;
            if auth.generation.get_untracked() != epoch || detecting.try_get_untracked().is_none() {
                return;
            }
            detecting.set(false);
            match result {
                Ok(result) => {
                    let detected = result.defaults_for(&ModelSettings::default());
                    if let Some(value) = detected.context_limit {
                        context.set(value.to_string());
                    }
                    if let Some(value) = detected.max_output_tokens {
                        output.set(value.to_string());
                    }
                    for (name, value) in sampling {
                        if let Some(detected_value) = detected.sampling.get(name) {
                            value.set(detected_value.to_string());
                        }
                    }
                    if let Some(value) = detected.thinking {
                        thinking.set(Some(value));
                    }
                    if let Some(value) = detected.tools {
                        tools.set(Some(value));
                    }
                    vision.set(detected.vision);
                    stream_tools.set(None);
                    tested.set(None);
                    detection.set(Some(result));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    });
    let test_model = move |_| {
        let Some(mut probe) = test_probe.clone() else {
            return;
        };
        probe.model = Some(selection.model.clone());
        let epoch = auth.generation.get_untracked();
        testing.set(true);
        error.set(None);
        spawn_local(async move {
            let backend = api.with_value(Clone::clone);
            let result = crate::model_setup::test(&*backend, &probe).await;
            if auth.generation.try_get_untracked() != Some(epoch)
                || testing.try_get_untracked().is_none()
            {
                return;
            }
            testing.set(false);
            match result {
                Ok(result) => {
                    tools.set(Some(result.structured_tools));
                    stream_tools.set(Some(result.streamed_tools));
                    tested.set(Some(result));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    if auto_detect && !has_discovery {
        detect.run(());
    }
    let read_settings = std::rc::Rc::new(move || -> Result<ModelSettings, String> {
        let number = |signal: RwSignal<String>| -> Result<Option<usize>, String> {
            let value = signal.get_untracked();
            if value.trim().is_empty() {
                Ok(None)
            } else {
                value
                    .parse::<usize>()
                    .map(Some)
                    .map_err(|_| "Token limits must be positive whole numbers.".into())
            }
        };
        let mut settings = ModelSettings {
            context_limit: number(context)?,
            max_output_tokens: number(output)?,
            thinking: thinking.get_untracked(),
            tools: tools.get_untracked(),
            vision: vision.get_untracked(),
            stream_tools: stream_tools.get_untracked(),
            auto_compact_threshold: number(threshold)?
                .map(u8::try_from)
                .transpose()
                .map_err(|_| "Invalid auto-compaction percentage.".to_string())?,
            ..saved.clone()
        };
        for (name, value) in sampling {
            let value = value.get_untracked();
            settings.sampling.remove(name);
            if !value.trim().is_empty() {
                settings.sampling.insert(
                    name.into(),
                    serde_json::from_str(&value)
                        .map_err(|_| format!("Enter a number for {name}."))?,
                );
            }
        }
        settings.validate()?;
        Ok(settings)
    });
    if let Some(on_edit) = on_edit {
        let read_settings = read_settings.clone();
        Effect::new(move |_| {
            context.track();
            output.track();
            threshold.track();
            thinking.track();
            tools.track();
            vision.track();
            stream_tools.track();
            for (_, value) in sampling {
                value.track();
            }
            on_edit.run(read_settings());
        });
    }
    view! {
        <div class="model-settings-editor">
            <InlineActions><button class="btn" disabled=move || detecting.get() || testing.get() on:click=move |_| detect.run(())>{move || if detecting.get() { "Detecting settings…" } else { "Detect settings" }}</button>
            <button class="btn" disabled=move || testing.get() || detecting.get() on:click=test_model>{move || if testing.get() { "Testing model…" } else { "Test model" }}</button></InlineActions>
            <Show when=move || tested.get().is_some()><p class="form-hint">{move || tested.get().map(|result| format!("Structured tools: {} · Streamed tools: {} · Plain chat first token: {} · {} tokens/sec{}{}", result.structured_tools, result.streamed_tools, result.first_token_ms.map_or_else(|| "unknown".into(), |ms| format!("{ms} ms")), result.tokens_per_second.map_or_else(|| "unknown".into(), |speed| format!("{speed:.1}")), if result.estimated { " (estimated)" } else { "" }, result.notice.map_or_else(String::new, |notice| format!(" · {notice}"))))}</p></Show>
            <Show when=move || detection.get().is_some()>
                <p class="form-hint">{move || detection.get().map(|result| result.details())}</p>

            </Show>

            <FormSection title="Context and output" class="ui-form-grid">
            <p class="form-hint tool-budget">{move || {
                let selection = budget_selection.clone().unwrap_or_else(|| state.connections.with(|connections| connections.iter().find(|connection| connection.id == budget_server).map_or_else(Default::default, |connection| connection.tool_selection.clone())));
                super::tool_budget::budget(&selection, tools.get() != Some(false), context.get().parse().ok())
            }}</p>
            <TextSetting label="Context tokens" value=context input_type="number" placeholder="Detect from server" />
            <TextSetting label="Max output tokens" value=output input_type="number" placeholder="Remaining context" />
            </FormSection>
            <FormSection title="Sampling" description="Blank values use the server default." class="ui-form-grid">
            {sampling.into_iter().map(|(name, value)| view! { <TextSetting label=name value=value placeholder="Model default" /> }).collect::<Vec<_>>()}
            </FormSection>
            <FormSection title="Capabilities" class="ui-form-grid">
            <BooleanSetting label="Vision (image input)" value=vision />
            <Show when=move || detection.get().is_none_or(|result| result.capabilities.is_empty() || result.capabilities.iter().any(|capability| capability == "thinking"))><BooleanSetting label="Thinking" value=thinking /></Show>
            <Show when=move || detection.get().is_none_or(|result| result.capabilities.is_empty() || result.capabilities.iter().any(|capability| capability == "tools"))><BooleanSetting label="Tool calling" value=tools /></Show>
            </FormSection>
            <FormSection title="Context compaction">
            <TextSetting label="Auto-compact (%)" value=threshold input_type="number" placeholder="85 (default)" />
            <p class="form-hint">"Auto-compact defaults to 85%; 0 disables it. Original conversation history is retained."</p>
            </FormSection>
            <Show when=move || error.get().is_some()><FormNotice tone=NoticeTone::Error>{move || error.get()}</FormNotice></Show>
        </div>
    }
}

#[component]
fn BooleanSetting(label: &'static str, value: RwSignal<Option<bool>>) -> impl IntoView {
    view! { <FormField label=label><super::dropdown::DropdownSelect label=label value=Signal::derive(move || value.get().map(|value| value.to_string()).unwrap_or_default()) options=Signal::derive(|| vec![super::dropdown::SelectOption::new("", "Model default"), super::dropdown::SelectOption::new("true", "On"), super::dropdown::SelectOption::new("false", "Off")]) on_change=Callback::new(move |selection: String| value.set(selection.parse().ok())) /></FormField> }
}

#[component]
pub fn ServerOptions(
    #[prop(optional_no_strip)] id: Option<i64>,
    on_edit: Callback<Result<ServerSettingsUpdate, String>>,
    #[prop(optional)] on_load: Option<Callback<openwebide_core::ServerSettings>>,
    #[prop(default = Signal::derive(|| false))] disabled: Signal<bool>,
) -> impl IntoView {
    let api = expect_context::<Api>();
    let auth = expect_context::<AuthState>();
    let tool_selection = RwSignal::new(openwebide_core::ToolSelection::All);
    let edited = RwSignal::new(false);
    let loading = RwSignal::new(id.is_some());
    let load_error = RwSignal::new(None::<String>);
    let has_key = RwSignal::new(false);
    let clear_key = RwSignal::new(false);
    let timeout = RwSignal::new("300".to_string());
    let keep_alive = RwSignal::new(String::new());
    let headers = RwSignal::new(String::new());
    let clear_headers = RwSignal::new(false);
    if let Some(id) = id {
        let epoch = auth.generation.get_untracked();
        spawn_local(async move {
            let result = api.with_value(Clone::clone).server_settings(id).await;
            if auth.generation.try_get_untracked() != Some(epoch)
                || loading.try_get_untracked().is_none()
            {
                return;
            }
            loading.set(false);
            match result {
                Ok(settings) => {
                    has_key.set(settings.has_api_key);
                    if let Some(on_load) = on_load {
                        on_load.run(settings.clone());
                    }
                    timeout.set(settings.timeout_seconds.to_string());
                    keep_alive.set(settings.keep_alive.unwrap_or_default());
                    if !edited.get_untracked() {
                        tool_selection.set(settings.tool_selection);
                    }
                }
                Err(error) => load_error.set(Some(error)),
            }
        });
    }
    Effect::new(move |_| {
        if loading.get() {
            on_edit.run(Err("Wait for server options to load.".into()));
            return;
        }
        if let Some(error) = load_error.get() {
            on_edit.run(Err(error));
            return;
        }
        let selection = tool_selection.get();
        let timeout = timeout.get();
        let keep_alive = keep_alive.get();
        let headers = headers.get();
        let clear_headers = clear_headers.get();
        let clear_api_key = clear_key.get();
        let result = (|| {
            let timeout_seconds = timeout
                .parse::<u32>()
                .map_err(|_| "Enter a timeout in seconds.".to_string())?;
            let mut extra_headers = std::collections::BTreeMap::new();
            for line in headers.lines().filter(|line| !line.trim().is_empty()) {
                let (name, value) = line
                    .split_once(':')
                    .ok_or_else(|| "Use one Name: Value header per line.".to_string())?;
                extra_headers.insert(name.trim().to_string(), value.trim().to_string());
            }
            let update = ServerSettingsUpdate {
                tool_selection: Some(selection),
                clear_api_key,
                headers: (clear_headers || !headers.trim().is_empty()).then_some(extra_headers),
                timeout_seconds: Some(timeout_seconds),
                keep_alive: Some(keep_alive),
                ..Default::default()
            };
            openwebide_core::ServerTransport::default().updated(&update)?;
            Ok(update)
        })();
        on_edit.run(result);
    });
    view! {
        <Show when=move || loading.get()><p class="form-hint">"Loading server options…"</p></Show>
        <Show when=move || load_error.get().is_some()><FormNotice tone=NoticeTone::Error>{move || load_error.get()}</FormNotice></Show>
        <details class="ui-disclosure"><summary>"Advanced server options"</summary>
        <fieldset class="server-options" disabled=disabled>
        <super::tool_budget::ToolBudget disabled=disabled selection=tool_selection on_edit=Callback::new(move |()| edited.set(true)) />
        <Show when=move || has_key.get()><CheckboxField label="Clear stored auth token" checked=clear_key.into() disabled=disabled on_change=Callback::new(move |value| clear_key.set(value)) /></Show>
        <TextSetting label="Timeout (seconds)" value=timeout input_type="number" />
        <TextSetting label="Ollama keep alive" value=keep_alive placeholder="Server default (e.g. 5m)" />
        <FormField label="Extra headers"><textarea class="form-input" placeholder="One Name: Value per line" rows="3" on:input=move |event| headers.set(event_target_value(&event)) /></FormField>
        <CheckboxField label="Clear stored extra headers" checked=clear_headers.into() on_change=Callback::new(move |value| clear_headers.set(value)) />
        <p class="form-hint">"Blank preserves stored headers. New headers replace them; stored values are never read back."</p>
        </fieldset>
    </details> }
}
