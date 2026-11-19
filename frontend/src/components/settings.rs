use super::{
    modal::Modal,
    model_setup::ModelSetupPanel,
    ui::{
        Button, DialogActions, DialogBody, DialogSize, DisclosurePanel, FormField, FormSection,
        InlineActions,
    },
};
use crate::state::settings::{SettingsDestination, SettingsState, Theme};
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

const LANGUAGE_SUPPORT: &str = "settings-language-support";
const HOST_ADMINISTRATION: &str = "settings-host-administration";

fn focus_section(id: &str) {
    if let Some(button) = document()
        .get_element_by_id(id)
        .and_then(|section| {
            section
                .query_selector(".ui-disclosure-toggle")
                .ok()
                .flatten()
        })
        .and_then(|button| button.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = button.focus();
    }
}

/// Mount settings when first opened and retain their live state and drafts.
#[component]
fn SettingsDisclosure(
    id: &'static str,
    title: &'static str,
    #[prop(default = "")] description: &'static str,
    expanded: RwSignal<bool>,
    children: ChildrenFn,
) -> impl IntoView {
    let opened = RwSignal::new(expanded.get_untracked());
    Effect::new(move || {
        if expanded.get() {
            opened.set(true);
        }
    });
    view! {
        <div id=id>
            <DisclosurePanel class="settings-disclosure" expanded=expanded summary=move || view! {
                <span class="settings-disclosure-label">
                    <strong>{title}</strong>
                    {(!description.is_empty()).then(|| view! { <span>{description}</span> })}
                </span>
            }>
                <Show when=move || opened.get()>{children()}</Show>
            </DisclosurePanel>
        </div>
    }
}

/// User preferences and the model / system prompt defaults for new sessions.
#[component]
pub fn Settings(
    on_set_theme: Callback<Theme>,
    #[prop(optional)] on_set_editor_preferences: Option<
        Callback<openwebide_core::editor::EditorPreferences>,
    >,
    on_set_notifications: Callback<bool>,
    on_set_default_prompt: Callback<Option<i64>>,
    on_set_bridge_url: Callback<String>,
) -> impl IntoView {
    let settings = expect_context::<SettingsState>();
    let destination = settings.destination.get_untracked();
    settings.destination.set(SettingsDestination::General);
    let tab = RwSignal::new(match destination {
        SettingsDestination::General => 0,
        SettingsDestination::LanguageSupport => 1,
        SettingsDestination::Models => 2,
        SettingsDestination::HostAdministration => 3,
    });
    let language = RwSignal::new(destination == SettingsDestination::LanguageSupport);
    let bridge = RwSignal::new(false);
    let administration = RwSignal::new(destination == SettingsDestination::HostAdministration);
    let updates = RwSignal::new(false);
    if let Some(section) = match destination {
        SettingsDestination::General | SettingsDestination::Models => None,
        SettingsDestination::LanguageSupport => Some(LANGUAGE_SUPPORT),
        SettingsDestination::HostAdministration => Some(HOST_ADMINISTRATION),
    } {
        leptos::leptos_dom::helpers::request_animation_frame(move || focus_section(section));
    }
    let configure_ssh = Callback::new(move |()| {
        administration.set(true);
        focus_section(HOST_ADMINISTRATION);
    });
    let host_available = use_context::<crate::host_admin::HostState>().is_some();
    let layout = expect_context::<crate::state::layout::LayoutState>();
    let layout_actions = use_context::<crate::state_actions::layout::LayoutActions>();
    let theme = settings.theme.read_only();
    let notifications = crate::notifications::RunNotifications::from_context();
    let default_prompt = settings.default_prompt.read_only();
    let system_prompts = settings.system_prompts.read_only();
    let bridge_url = settings.bridge_url.read_only();
    let on_close = Callback::new(move |()| settings.show_settings.set(false));
    let (pairing_token, set_pairing_token) = leptos::prelude::signal(String::new());
    Effect::new(move || {
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(Some(pt)) = crate::idb::get_bridge_pairing_token().await {
                set_pairing_token.set(pt);
            }
        });
    });

    view! {
        <Modal title=Signal::derive(|| "Settings".to_string()) on_close=on_close size=DialogSize::Wide class="modal ui-tabbed-modal">
            <super::ui::DialogTabs label="Settings categories" options=vec![
                super::ui::DialogTab::new("General", "settings-tab-general", "settings-panel-general"),
                super::ui::DialogTab::new("Editor", "settings-tab-editor", "settings-panel-editor"),
                super::ui::DialogTab::new("Models", "settings-tab-models", "settings-panel-models"),
                super::ui::DialogTab::new("Host", "settings-tab-host", "settings-panel-host"),
            ] selected=tab.read_only().into() on_change=Callback::new(move |index| tab.set(index)) />
            <DialogBody class="settings-body ui-tabbed-body" selected_tab=tab.read_only()>
                <div id="settings-panel-general" class="ui-tab-panel" role="tabpanel" aria-labelledby="settings-tab-general" tabindex="0" hidden=move || tab.get() != 0>
                <FormSection title="Appearance" description="Changes apply immediately." class="ui-form-grid">
                <FormField label="Layout" group=true>
                    <super::ui::SegmentedControl options=vec![super::ui::SegmentOption::new("Automatic", crate::state::responsive::LayoutMode::Automatic), super::ui::SegmentOption::new("Desktop", crate::state::responsive::LayoutMode::Desktop), super::ui::SegmentOption::new("Phone", crate::state::responsive::LayoutMode::Phone)] value=Signal::derive(move || layout.preferences.with(|prefs| prefs.mode)) on_change=Callback::new(move |mode| { if let Some(actions) = layout_actions { actions.set_mode.run(mode); } }) />
                </FormField>
                <FormField label="Theme" group=true>
                    <super::ui::SegmentedControl options=vec![super::ui::SegmentOption::new("System", Theme::System), super::ui::SegmentOption::new("Dark", Theme::Dark), super::ui::SegmentOption::new("Light", Theme::Light)] value=Signal::derive(move || theme.get()) on_change=on_set_theme />
                </FormField>

                </FormSection>
                <FormSection title="Notifications">
                <FormField label="Browser notifications" group=true>
                    <InlineActions><button class="btn ghost notification-toggle" disabled=move || notifications.configuring.get()
                        on:click=move |_| on_set_notifications.run(!(settings.browser_notifications.get_untracked() && notifications.permission.get_untracked() != crate::notifications::NotificationPermission::Default))>
                        {move || if notifications.configuring.get() { "Saving…" } else if settings.browser_notifications.get() && notifications.permission.get() != crate::notifications::NotificationPermission::Default { "Disable" } else { "Enable" }}
                    </button>
                    </InlineActions>
                </FormField>
                <p class="form-hint notification-status">{move || {
                    use crate::notifications::NotificationPermission;
                    match notifications.permission.get() {
                        NotificationPermission::Unsupported => "Notifications need a supported browser on HTTPS or localhost. Chat approvals remain available in the app.",
                        NotificationPermission::Denied => "Notifications are blocked in this browser's site settings.",
                        NotificationPermission::Default if settings.browser_notifications.get() => "Enable on this browser to receive run and approval notifications.",
                        _ if settings.browser_notifications.get() && notifications.push_ready.get() => "Background notifications are enabled for remote projects and project-less chats. Local projects require this app to stay open.",
                        _ if settings.browser_notifications.get() => "Notify when a run finishes or needs approval while its chat is out of focus. Keep this app open to receive notifications.",
                        _ => "Enable notifications for finished runs and approval requests. Browser permission is required on each device.",
                    }
                }}</p>
                </FormSection>
                <FormSection title="Installation">
                    <super::install_app::InstallApp />
                </FormSection>
                </div>
                <div id="settings-panel-editor" class="ui-tab-panel" role="tabpanel" aria-labelledby="settings-tab-editor" tabindex="0" hidden=move || tab.get() != 1>
                <FormSection title="Typography" description="Changes apply immediately.">
                    <FormField label="Font family">
                        <super::dropdown::DropdownSelect label="Editor font" value=Signal::derive(move || settings.editor_preferences.get().font.name().to_string()) options=Signal::derive(|| openwebide_core::editor::EditorFont::ALL.into_iter().map(|font| super::dropdown::SelectOption::new(font.name(), format!("Monaspace {}", font.name()))).collect()) disabled=Signal::derive(move || on_set_editor_preferences.is_none()) on_change=Callback::new(move |name: String| {
                            if let Some(save) = on_set_editor_preferences
                                && let Some(font) = openwebide_core::editor::EditorFont::ALL.into_iter().find(|font| font.name() == name) {
                                let mut preferences = settings.editor_preferences.get_untracked(); preferences.font = font; save.run(preferences);
                            }
                        }) />
                    </FormField>
                    <super::ui::CheckboxField label="Texture healing" checked=Signal::derive(move || settings.editor_preferences.get().texture_healing) disabled=Signal::derive(move || on_set_editor_preferences.is_none()) on_change=Callback::new(move |checked| {
                        if let Some(save) = on_set_editor_preferences {
                            let mut preferences = settings.editor_preferences.get_untracked(); preferences.texture_healing = checked; save.run(preferences);
                        }
                    }) />
                    <super::ui::CheckboxField label="Coding ligatures" checked=Signal::derive(move || settings.editor_preferences.get().ligatures) disabled=Signal::derive(move || on_set_editor_preferences.is_none()) on_change=Callback::new(move |checked| {
                        if let Some(save) = on_set_editor_preferences {
                            let mut preferences = settings.editor_preferences.get_untracked(); preferences.ligatures = checked; save.run(preferences);
                        }
                    }) />
                </FormSection>
                <FormSection title="Editing" description="Indentation follows EditorConfig, then detected file style, then these defaults.">
                    <FormField label="Indentation" group=true>
                        <super::editor_options::IndentationControls value=Signal::derive(move || settings.editor_preferences.get().indentation) disabled=Signal::derive(move || on_set_editor_preferences.is_none()) on_change=Callback::new(move |indentation| {
                            if let Some(save) = on_set_editor_preferences {
                                let mut preferences = settings.editor_preferences.get_untracked(); preferences.indentation = indentation; save.run(preferences);
                            }
                        }) />
                    </FormField>
                    <p class="form-hint">"Tab indents in the editor. Ctrl+M lets Tab move focus."</p>
                    <super::ui::CheckboxField label="Word wrap" checked=Signal::derive(move || settings.editor_preferences.get().word_wrap) disabled=Signal::derive(move || on_set_editor_preferences.is_none()) on_change=Callback::new(move |checked| {
                        if let Some(save) = on_set_editor_preferences {
                            let mut preferences = settings.editor_preferences.get_untracked(); preferences.word_wrap = checked; save.run(preferences);
                        }
                    }) />
                    <super::ui::CheckboxField label="Show whitespace" checked=Signal::derive(move || settings.editor_preferences.get().show_whitespace) disabled=Signal::derive(move || on_set_editor_preferences.is_none()) on_change=Callback::new(move |checked| {
                        if let Some(save) = on_set_editor_preferences {
                            let mut preferences = settings.editor_preferences.get_untracked(); preferences.show_whitespace = checked; save.run(preferences);
                        }
                    }) />
                </FormSection>
                <SettingsDisclosure id=LANGUAGE_SUPPORT title="Language support"
                    description="Idle analysis and provider choices for your account and projects."
                    expanded=language>
                    <super::language_settings::LanguageProviderSettings />
                </SettingsDisclosure>
                </div>
                <div id="settings-panel-models" class="ui-tab-panel" role="tabpanel" aria-labelledby="settings-tab-models" tabindex="0" hidden=move || tab.get() != 2>
                <FormSection title="Model defaults" description="Choose the models and system prompt used for new chats. Changes save automatically.">
                <ModelSetupPanel defaults_only=true />
                <FormField label="Default system prompt">
                    <super::dropdown::DropdownSelect label="Default system prompt" value=Signal::derive(move || default_prompt.get().map(|id| id.to_string()).unwrap_or_default()) options=Signal::derive(move || {
                        let mut options = vec![super::dropdown::SelectOption::new("", "(none)")];
                        options.extend(system_prompts.get().into_iter().map(|prompt| super::dropdown::SelectOption::new(prompt.id.to_string(), prompt.name))); options
                    }) on_change=Callback::new(move |value: String| on_set_default_prompt.run(value.parse().ok())) />
                </FormField>

                </FormSection>
                </div>
                <div id="settings-panel-host" class="ui-tab-panel settings-host" role="tabpanel" aria-labelledby="settings-tab-host" tabindex="0" hidden=move || tab.get() != 3>
                <SettingsDisclosure id="settings-execution-bridge" title="Execution bridge"
                    description="Bridge URL and pairing token for this browser." expanded=bridge>
                <FormSection title="Execution bridge" description="Connect terminal and agent tools to your execution host.">
                <FormField label="Bridge URL">
                    <input
                        type="text"
                        class="form-input"
                        value=bridge_url
                        on:change=move |e: web_sys::Event| {
                            if let Some(target) = e.target()
                                && let Some(input) = target.dyn_ref::<web_sys::HtmlInputElement>()
                            {
                                on_set_bridge_url.run(input.value());
                            }
                        }
                    />
                </FormField>

                <FormField label="Bridge pairing token" group=true>
                    <div class="ui-input-group">
                        <input
                            type="password"
                            aria-label="Bridge pairing token"
                            class="form-input"
                            value=pairing_token
                            on:change=move |e: web_sys::Event| {
                                if let Some(target) = e.target()
                                    && let Some(input) = target.dyn_ref::<web_sys::HtmlInputElement>()
                                {
                                    let val = input.value();
                                    set_pairing_token.set(val.clone());
                                    wasm_bindgen_futures::spawn_local(async move {
                                        let _ = crate::idb::set_bridge_pairing_token(&val).await;
                                    });
                                }
                            }
                        />
                        <button
                            class="btn"
                            on:click=move |_| {
                                set_pairing_token.set(String::new());
                                wasm_bindgen_futures::spawn_local(async move {
                                    let _ = crate::idb::delete_bridge_pairing_token().await;
                                });
                            }
                        >
                            "Clear"
                        </button>
                    </div>
                    <div class="form-hint">
                        "Only needed when your local execution bridge requires a pairing token. Stored in this browser."
                    </div>
                </FormField>

                </FormSection>
                </SettingsDisclosure>
                <Show when=move || host_available>
                    <SettingsDisclosure id=HOST_ADMINISTRATION title="Host administration"
                        description="SSH access for the server running Open WebIDE." expanded=administration>
                        <super::host_admin::HostSettings />
                    </SettingsDisclosure>
                    <SettingsDisclosure id="settings-host-updates" title="Updates"
                        description="Release channel and installation for this server." expanded=updates>
                        <super::host_updates::HostUpdates on_configure_ssh=configure_ssh />
                    </SettingsDisclosure>
                </Show>
                </div>
            </DialogBody>
            <DialogActions>
                <Button on_click=Callback::new(move |_| on_close.run(()))>"Done"</Button>
            </DialogActions>
        </Modal>
    }
}
