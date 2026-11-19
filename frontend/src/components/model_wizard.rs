use super::{
    dropdown::{DropdownSelect, SelectOption},
    ui::{
        DialogActions, DialogBody, DialogSize, DisclosurePanel, FormField, FormNotice, FormSection,
        NoticeTone,
    },
};
use crate::{
    backend::Api,
    model_setup::{self, DiscoveredModel},
    state::{
        auth::AuthState,
        settings::{ConfigurationSection, SettingsState},
    },
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::{ModelDetection, ModelProbe, ModelSettings, ProviderKind, ServerPreset};
use std::collections::BTreeMap;
use wasm_bindgen::JsCast;

const PRESETS: [(ServerPreset, &str, &str); 9] = [
    (ServerPreset::Auto, "auto", "Custom"),
    (ServerPreset::Ollama, "ollama", "Ollama"),
    (ServerPreset::LlamaCpp, "llama_cpp", "llama.cpp"),
    (ServerPreset::LmStudio, "lm_studio", "LM Studio"),
    (ServerPreset::Vllm, "vllm", "vLLM"),
    (ServerPreset::LiteLlm, "lite_llm", "LiteLLM"),
    (ServerPreset::OpenRouter, "open_router", "OpenRouter"),
    (ServerPreset::SgLang, "sg_lang", "SGLang"),
    (ServerPreset::KoboldCpp, "kobold_cpp", "KoboldCpp"),
];
type ModelDrafts = BTreeMap<String, Result<ModelSettings, String>>;

fn default_endpoint(preset: ServerPreset, kind: ProviderKind) -> &'static str {
    match (preset, kind) {
        (ServerPreset::Auto, ProviderKind::Ollama) => ServerPreset::Ollama.base_url(),
        _ => preset.base_url(),
    }
}

fn focus_after_render(id: &'static str) {
    leptos::leptos_dom::helpers::request_animation_frame(move || {
        if let Some(element) = document()
            .get_element_by_id(id)
            .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = element.focus();
        }
    });
}

fn chat_capable(model: &DiscoveredModel) -> bool {
    model
        .detection
        .as_ref()
        .is_none_or(ModelDetection::chat_capable)
}

#[component]
pub fn ModelSetupWizard(on_close: Callback<()>) -> impl IntoView {
    let api = expect_context::<Api>();
    let auth = expect_context::<AuthState>();
    let state = expect_context::<SettingsState>();
    let review = RwSignal::new(false);
    let kind = RwSignal::new(state.conn_kind.get_untracked());
    let initial_url = state.conn_base_url.get_untracked();
    let server_id = state.conn_edit_id.get_untracked();
    let initial_preset = if server_id.is_none()
        && initial_url.is_empty()
        && kind.get_untracked() == ProviderKind::Ollama
    {
        ServerPreset::Ollama
    } else {
        ServerPreset::Auto
    };
    let url_edited = RwSignal::new(!initial_url.is_empty());
    let url = RwSignal::new(if initial_url.is_empty() {
        default_endpoint(initial_preset, kind.get_untracked()).into()
    } else {
        initial_url
    });
    let token = RwSignal::new(String::new());
    let preset = RwSignal::new(initial_preset);
    let options = RwSignal::new(Ok::<_, String>(
        openwebide_core::ServerSettingsUpdate::default(),
    ));
    let pending = RwSignal::new(None::<ModelProbe>);
    let models = RwSignal::new(Vec::<DiscoveredModel>::new());
    let drafts = RwSignal::new(ModelDrafts::new());
    let busy = RwSignal::new(false);
    let inflight = RwSignal::new(BTreeMap::<String, bool>::new());
    let error = RwSignal::new(None::<String>);
    let generation = RwSignal::new(0_u64);
    let review_revision = RwSignal::new(0_u64);
    let has_key = RwSignal::new(false);
    let preset_edited = RwSignal::new(false);
    let chat_models = Memo::new(move |_| {
        models.with(|models| models.iter().filter(|model| chat_capable(model)).count())
    });
    let model_busy = move || inflight.with(|values| values.values().any(|busy| *busy));
    let open_review = move || {
        error.set(None);
        review.set(true);
        focus_after_render("model-review-heading");
    };
    let connection_error = move |message| {
        error.set(Some(message));
        focus_after_render("model-server-url");
    };

    let discover = move |_| {
        let endpoint = url.get_untracked();
        if endpoint.trim().is_empty() {
            connection_error("Enter the model server URL.".into());
            return;
        }
        let mut transport = match options.get_untracked() {
            Ok(value) => value,
            Err(message) => {
                connection_error(message);
                return;
            }
        };
        let token = token.get_untracked();
        transport.api_key = (!token.is_empty()).then_some(token);
        transport.preset = Some(preset.get_untracked());
        let probe = ModelProbe {
            server_id,
            kind: kind.get_untracked(),
            base_url: endpoint.trim().into(),
            transport,
            model: None,
        };
        let previous = pending.get_untracked();
        if previous.as_ref() == Some(&probe) {
            open_review();
            return;
        }
        let same_endpoint = previous.is_some_and(|previous| {
            previous.kind == probe.kind && previous.base_url == probe.base_url
        });
        let epoch = auth.generation.get_untracked();
        generation.update(|value| *value += 1);
        let request = generation.get_untracked();
        let current = move || {
            auth.generation.try_get_untracked() == Some(epoch)
                && generation.try_get_untracked() == Some(request)
        };
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            let backend = api.with_value(Clone::clone);
            let result = model_setup::preview(&*backend, &probe, current).await;
            if !current() {
                return;
            }
            busy.set(false);
            match result {
                Ok(found) if found.is_empty() => connection_error(
                    "No models found. Install or load a chat model on this server, then retry."
                        .into(),
                ),
                Ok(mut found) => {
                    if same_endpoint {
                        drafts.with_untracked(|drafts| {
                            for model in &mut found {
                                if let Some(Ok(settings)) =
                                    drafts.get(&model.profile.selection.model)
                                {
                                    model.profile.settings = settings.clone();
                                }
                            }
                        });
                    }
                    drafts.update(BTreeMap::clear);
                    inflight.update(BTreeMap::clear);
                    pending.set(Some(probe));
                    review_revision.set(request);
                    models.set(found);
                    open_review();
                }
                Err(message) => connection_error(message),
            }
        });
    };
    let apply = move |_| {
        let Some(probe) = pending.get_untracked() else {
            return;
        };
        let epoch = auth.generation.get_untracked();
        let request = generation.get_untracked();
        let current = move || {
            auth.generation.try_get_untracked() == Some(epoch)
                && generation.try_get_untracked() == Some(request)
        };
        let found = models.get_untracked();
        let reviewed = drafts.get_untracked();
        let mut profiles = Vec::new();
        for model in found.into_iter().filter(chat_capable) {
            let mut profile = model.profile;
            match reviewed.get(&profile.selection.model) {
                Some(Ok(settings)) => profile.settings = settings.clone(),
                Some(Err(message)) => {
                    error.set(Some(format!("{}: {message}", profile.selection.model)));
                    focus_after_render("model-review-error");
                    return;
                }
                None => {}
            }
            profiles.push(profile);
        }
        if profiles.is_empty() {
            return;
        }
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            let backend = api.with_value(Clone::clone);
            let result = model_setup::save_setup(&*backend, &probe, &profiles, current).await;
            if !current() {
                return;
            }
            busy.set(false);
            match result {
                Ok((server, setup)) => {
                    let id = server.id;
                    state.connections.update(|servers| {
                        if let Some(saved) = servers.iter_mut().find(|saved| saved.id == id) {
                            *saved = server;
                        } else {
                            servers.push(server);
                        }
                    });
                    state.default_connection.set(
                        setup
                            .defaults
                            .primary
                            .as_ref()
                            .map(|primary| primary.server_id),
                    );
                    state.model_setup.set(setup);
                    state.saved_server.set(Some(id));
                    state.configuration.set(Some(ConfigurationSection::Servers));
                    on_close.run(());
                }
                Err(message) => {
                    error.set(Some(message));
                    focus_after_render("model-review-error");
                }
            }
        });
    };

    view! {
        <super::modal::Modal title=Signal::derive(|| "Model setup".into()) on_close=on_close size=DialogSize::Wide class="modal model-wizard">
            <ol class="ui-steps model-setup-steps" aria-label="Model setup steps">
                <li class:is-current=move || !review.get() aria-current=move || (!review.get()).then_some("step")>
                    <span class="ui-step-number">"1"</span>"Connect server"
                </li>
                <li class:is-current=move || review.get() aria-current=move || review.get().then_some("step")>
                    <span class="ui-step-number">"2"</span>"Review models"
                </li>
            </ol>
            <DialogBody class="model-setup" selected_tab=Signal::derive(move || usize::from(review.get()))>
                <div class="model-setup-pane" hidden=move || review.get()>
                    <FormSection title="Server connection">
                        <FormField label="Server preset">
                            <DropdownSelect label="Server preset" disabled=Signal::derive(move || busy.get())
                                value=Signal::derive(move || PRESETS.iter().find(|(choice, _, _)| *choice == preset.get()).map_or("auto", |(_, value, _)| *value).to_string())
                                options=Signal::derive(|| PRESETS.iter().map(|(_, value, label)| SelectOption::new(*value, *label)).collect())
                                on_change=Callback::new(move |value: String| {
                                    if let Some((choice, _, _)) = PRESETS.iter().find(|(_, key, _)| *key == value) {
                                        preset_edited.set(true);
                                        preset.set(*choice);
                                        if *choice != ServerPreset::Auto {
                                            kind.set(choice.kind());
                                        }
                                        if !url_edited.get_untracked() {
                                            url.set(default_endpoint(*choice, kind.get_untracked()).into());
                                        }
                                        error.set(None);
                                    }
                                }) />
                        </FormField>
                        <Show when=move || preset.get() == ServerPreset::Auto>
                            <FormField label="Protocol">
                                <DropdownSelect label="Protocol" disabled=Signal::derive(move || busy.get())
                                    value=Signal::derive(move || kind.get().as_str().to_string())
                                    options=Signal::derive(|| vec![SelectOption::new("ollama", "Ollama"), SelectOption::new("llamacpp", "OpenAI-compatible")])
                                    on_change=Callback::new(move |value: String| {
                                        if let Some(protocol) = ProviderKind::parse(&value) {
                                            kind.set(protocol);
                                            if !url_edited.get_untracked() {
                                                url.set(default_endpoint(ServerPreset::Auto, protocol).into());
                                            }
                                            error.set(None);
                                        }
                                    }) />
                            </FormField>
                        </Show>
                        <FormField label="Server URL">
                            <input id="model-server-url" class="form-input" type="url" disabled=move || busy.get()
                                aria-invalid=move || error.get().is_some() && !review.get()
                                aria-describedby="model-server-url-hint model-server-url-error"
                                prop:value=move || url.get()
                                on:input=move |event| { url_edited.set(true); url.set(event_target_value(&event)); error.set(None); } />
                        </FormField>
                        <p id="model-server-url-hint" class="form-hint">"Use an address reachable from the Open WebIDE host. In a container, localhost means the container itself."</p>
                        <div id="model-server-url-error" hidden=move || review.get() || error.with(Option::is_none)>
                            <FormNotice tone=NoticeTone::Error>{move || error.get()}</FormNotice>
                        </div>
                        <FormField label="Auth token (optional)">
                            <input class="form-input" type="password" disabled=move || busy.get() autocomplete="new-password"
                                prop:value=move || token.get()
                                on:input=move |event| { token.set(event_target_value(&event)); error.set(None); } />
                        </FormField>
                        <Show when=move || has_key.get()><p class="form-hint">"Leave the token blank to keep the saved credential."</p></Show>
                        <super::model_setup::ServerOptions disabled=Signal::derive(move || busy.get()) id=server_id
                            on_load=Callback::new(move |settings: openwebide_core::ServerSettings| {
                                has_key.set(settings.has_api_key);
                                if !preset_edited.get_untracked() {
                                    preset.set(settings.preset);
                                    if settings.preset != ServerPreset::Auto {
                                        kind.set(settings.preset.kind());
                                    }
                                }
                            }) on_edit=Callback::new(move |value| options.set(value)) />
                    </FormSection>
                </div>

                <div class="model-setup-pane model-review" hidden=move || !review.get()>
                    <h3 id="model-review-heading" tabindex="-1">"Models to save"</h3>
                    <p class="form-hint">{move || match chat_models.get() {
                        0 => "This server reported no chat models.".into(),
                        1 => "1 chat model will be saved.".into(),
                        count => format!("{count} chat models will be saved."),
                    }}</p>
                    <For each=move || models.get() key=move |model| (review_revision.get_untracked(), model.profile.selection.model.clone())
                        children=move |model| {
                            pending.get_untracked().map(|probe| view! {
                                <ModelReviewRow model=model probe=probe drafts=drafts inflight=inflight />
                            })
                        } />
                    <Show when=move || review.get() && error.with(Option::is_some)>
                        <div id="model-review-error" tabindex="-1"><FormNotice tone=NoticeTone::Error>{move || error.get()}</FormNotice></div>
                    </Show>
                </div>
            </DialogBody>
            <DialogActions>
                <button type="button" class="btn" disabled=move || busy.get() on:click=move |_| on_close.run(())>"Cancel"</button>
                <Show when=move || !review.get()>
                    <button type="button" class="btn send" disabled=move || busy.get() || model_busy()
                        on:click=discover>{move || if busy.get() { "Discovering models…" } else { "Discover models" }}</button>
                </Show>
                <Show when=move || review.get()>
                    <button type="button" class="btn" disabled=move || busy.get() || model_busy() on:click=move |_| {
                        error.set(None);
                        review.set(false);
                        focus_after_render("model-server-url");
                    }>"Back"</button>
                    <button type="button" class="btn send" disabled=move || busy.get() || model_busy() || chat_models.get() == 0 on:click=apply>
                        {move || if busy.get() { "Saving server…" } else { "Save server" }}
                    </button>
                </Show>
            </DialogActions>
        </super::modal::Modal>
    }
}

#[component]
fn ModelReviewRow(
    model: DiscoveredModel,
    probe: ModelProbe,
    drafts: RwSignal<ModelDrafts>,
    inflight: RwSignal<BTreeMap<String, bool>>,
) -> impl IntoView {
    let title = model.profile.selection.model.clone();
    if !chat_capable(&model) {
        return view! {
            <div class="model-review-unsupported">
                <strong>{title}</strong><span>"Embedding only"</span>
            </div>
        }
        .into_any();
    }
    let context_key = title.clone();
    let existing_context = model.profile.settings.context_limit;
    let detection_status = model
        .detection
        .as_ref()
        .map(model_summary)
        .unwrap_or_else(|| "Settings detection unavailable".into());
    let expanded = RwSignal::new(false);
    let opened = RwSignal::new(false);
    Effect::new(move || {
        if expanded.get() {
            opened.set(true);
        }
    });
    let selection = StoredValue::new(model.profile.selection);
    let settings = StoredValue::new(model.profile.settings);
    let error = StoredValue::new(model.error);
    let detection = StoredValue::new(model.detection);
    let probe = StoredValue::new(probe);
    let status = StoredValue::new(detection_status);
    view! {
        <DisclosurePanel class="model-review-row" expanded=expanded summary=move || {
            let title = title.clone();
            let context_key = context_key.clone();
            view! {
                <span class="model-review-summary">
                    <strong>{title}</strong>
                    <span>{move || {
                        let context = drafts.with(|drafts| match drafts.get(&context_key) {
                            Some(Ok(settings)) => settings.context_limit,
                            _ => existing_context,
                        });
                        let status = status.get_value();
                        context.map_or_else(|| status.clone(), |count| format!("{count} context tokens · {status}"))
                    }}</span>
                </span>
            }
        }>
            <Show when=move || opened.get()>
                <super::model_setup::ModelSettingsEditor selection=selection.get_value() initial_error=error.get_value()
                    on_busy=Callback::new(move |busy| inflight.update(|values| { values.insert(selection.get_value().model, busy); }))
                    initial=Some(settings.get_value()) discovered=detection.get_value() auto_detect=false probe=Some(probe.get_value())
                    on_edit=Callback::new(move |settings| drafts.update(|values| { values.insert(selection.get_value().model, settings); })) />
            </Show>
        </DisclosurePanel>
    }.into_any()
}

fn model_summary(model: &ModelDetection) -> String {
    let mut details = Vec::new();
    if let Some(loaded) = model.loaded {
        details.push(if loaded {
            "Loaded".into()
        } else {
            "Available".into()
        });
    }
    if let Some(bytes) = model.size_bytes {
        details.push(openwebide_core::model_setup::model_size(bytes));
    }
    for (capability, label) in [
        ("tools", "Tools"),
        ("thinking", "Thinking"),
        ("vision", "Vision"),
    ] {
        if model.capabilities.iter().any(|value| value == capability) {
            details.push(label.into());
        }
    }
    if details.is_empty() {
        "Chat model".into()
    } else {
        details.join(" · ")
    }
}
