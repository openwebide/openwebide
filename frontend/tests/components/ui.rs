use leptos::prelude::*;
use openwebide_core::{User, UserId, UserRole};
use openwebide_frontend::{
    components::ui::{Button, ButtonSize, ButtonVariant},
    state::{auth::AuthState, layout::LayoutState, settings::Theme},
    state_actions::{
        lifecycle::{ProjectEffectContext, install_project_effects},
        settings::{SettingsActionContext, build_settings_actions},
    },
};
use wasm_bindgen_test::*;

use super::support::{mount_test, settle};

async fn prepare_tooltip_fixture(mounted: &super::support::Mounted) {
    // Harness output can put ordinary fixtures below the viewport. Keep the
    // tooltip target visible and finish stylesheet/layout-induced scrolling
    // before testing focus or hover. Explicit scroll dismissal stays covered.
    mounted
        .root
        .style()
        .set_property("position", "fixed")
        .unwrap();
    mounted.root.style().set_property("top", "8px").unwrap();
    mounted.root.style().set_property("left", "8px").unwrap();
    let _ = mounted.root.get_bounding_client_rect();
    openwebide_frontend::util::yield_frame().await;
    settle().await;
}

#[wasm_bindgen_test]
async fn button_variants_sizes_and_disabled_callbacks_share_the_base() {
    let clicks = RwSignal::new(0);
    let disabled = RwSignal::new(true);
    let mounted = mount_test(move |_| {
        view! {
            <div>
                <Button class="default" on_click=Callback::new(move |_| clicks.update(|n| *n += 1))>"Default"</Button>
                <Button variant=ButtonVariant::Primary size=ButtonSize::Sm class="primary">"Primary"</Button>
                <Button variant=ButtonVariant::Success class="success">"Success"</Button>
                <Button variant=ButtonVariant::Danger class="dangerous">"Danger"</Button>
                <Button variant=ButtonVariant::Ghost class="ghostly">"Ghost"</Button>
                <Button class="disabled" disabled=Signal::from(disabled) on_click=Callback::new(move |_| clicks.update(|n| *n += 1))>"Disabled"</Button>
            </div>
        }
    });
    settle().await;
    for (selector, modifier) in [
        (".default", "md"),
        (".primary", "send"),
        (".success", "approve"),
        (".dangerous", "danger"),
        (".ghostly", "ghost"),
    ] {
        let classes = mounted.element(selector).class_name();
        assert!(classes.split_whitespace().any(|class| class == "btn"));
        assert!(classes.split_whitespace().any(|class| class == modifier));
        assert!(!classes.contains("ui-btn"));
    }
    assert!(mounted.element(".primary").class_name().contains("sm"));
    mounted.click(".disabled");
    assert_eq!(clicks.get_untracked(), 0);
    mounted.click(".default");
    assert_eq!(clicks.get_untracked(), 1);
    disabled.set(false);
    settle().await;
    mounted.click(".disabled");
    assert_eq!(clicks.get_untracked(), 2);
}

#[wasm_bindgen_test]
async fn startup_retains_prepaint_theme_until_database_settings_arrive() {
    let document = web_sys::window().unwrap().document().unwrap();
    let root = document.document_element().unwrap();
    let original = root.get_attribute("data-theme");
    let storage = web_sys::window().unwrap().local_storage().unwrap().unwrap();
    storage.set_item("owide-theme", "dark").unwrap();
    // The blocking backend script has already painted the saved light preference.
    root.set_attribute("data-theme", "light").unwrap();
    let html = include_str!("../../index.html");
    assert!(html.contains("<script src=\"/api/theme.js\"></script>"));
    assert!(!html.contains("localStorage"));
    let (release, pending) = futures::channel::oneshot::channel();
    let (request_started, started) = futures::channel::oneshot::channel();
    let notifications = RwSignal::new(false);
    let mounted = mount_test(move |mut state| {
        // Keep the app's shared settings store, including its startup request.
        state.settings.theme.set(Theme::from_root());
        state.settings.browser_notifications = notifications;
        build_settings_actions(SettingsActionContext {
            api: state.api,
            settings: state.settings,
            ui: state.ui,
        });
        state
            .fake
            .settings_load_results
            .borrow_mut()
            .push_back(pending);
        *state.fake.settings_load_started.borrow_mut() = Some(request_started);
        let auth = expect_context::<AuthState>();
        auth.set_user(User {
            id: UserId::new(1),
            username: "test".into(),
            role: UserRole::User,
            created_at: 0,
        });
        install_project_effects(ProjectEffectContext {
            api: state.api,
            health: RwSignal::new(None),
            auth,
            settings: state.settings,
            projects: state.projects,
            chat: state.chat,
            layout: expect_context::<LayoutState>(),
            select_project: Callback::new(|_| ()),
        });
        view! { <div /> }
    });
    settle().await;
    assert_eq!(root.get_attribute("data-theme").as_deref(), Some("light"));
    // Cold IndexedDB setup precedes the request. Its browser I/O is not the
    // theme-application deadline: release only after the real request is waiting.
    started.await.unwrap();
    assert_eq!(root.get_attribute("data-theme").as_deref(), Some("light"));
    release
        .send(Ok(std::collections::BTreeMap::from([
            ("theme".into(), "dark".into()),
            ("browser_notifications".into(), "true".into()),
        ])))
        .unwrap();
    // Keep the ordinary deadline for applying the released database response.
    super::support::wait_until("startup database theme and notifications", || {
        root.get_attribute("data-theme").as_deref() == Some("dark") && notifications.get_untracked()
    })
    .await;
    assert_eq!(root.get_attribute("data-theme").as_deref(), Some("dark"));
    assert!(notifications.get_untracked());
    drop(mounted);
    storage.remove_item("owide-theme").unwrap();
    if let Some(original) = original {
        root.set_attribute("data-theme", &original).unwrap();
    } else {
        root.remove_attribute("data-theme").unwrap();
    }
}

#[wasm_bindgen_test]
async fn typed_theme_controls_update_and_save_the_same_database_values() {
    use openwebide_frontend::components::Settings;
    let root = document().document_element().unwrap();
    let original = root.get_attribute("data-theme");
    let mounted = mount_test(|state| {
        let actions = build_settings_actions(SettingsActionContext {
            api: state.api,
            settings: state.settings,
            ui: state.ui,
        });
        view! {
            <Settings on_set_notifications=actions.on_set_notifications on_set_theme=actions.on_set_theme

                on_set_default_prompt=actions.on_set_default_prompt
                on_set_bridge_url=actions.on_set_bridge_url />
        }
    });
    for (index, theme) in [Theme::System, Theme::Dark, Theme::Light]
        .into_iter()
        .enumerate()
    {
        mounted.click(&format!(
            ".ui-field-group:nth-of-type(2) .ui-seg-btn:nth-child({})",
            index + 1
        ));
        settle().await;
        assert_eq!(mounted.state.settings.theme.get_untracked(), theme);
        assert_eq!(
            mounted.state.fake.settings.borrow()["theme"],
            theme.as_str()
        );
        let effective = if theme == Theme::System {
            if web_sys::window()
                .unwrap()
                .match_media("(prefers-color-scheme: dark)")
                .unwrap()
                .unwrap()
                .matches()
            {
                "dark"
            } else {
                "light"
            }
        } else {
            theme.as_str()
        };
        assert_eq!(root.get_attribute("data-theme").as_deref(), Some(effective));
    }
    drop(mounted);
    if let Some(original) = original {
        root.set_attribute("data-theme", &original).unwrap();
    } else {
        root.remove_attribute("data-theme").unwrap();
    }
}

#[wasm_bindgen_test]
async fn statusline_preserves_compact_telemetry_text() {
    use openwebide_core::SessionTelemetry;
    let mounted = mount_test(super::support::chat_view);
    settle().await;
    mounted.state.chat.session_telemetry.set(SessionTelemetry {
        context: None,
        model: "café".into(),
        context_tokens: 12345,
        context_limit: 65536,
        current_speed_tps: Some(12.345),
        context_estimated: true,
        context_limit_estimated: true,
        speed_estimated: true,
        tool_calls_count: 7,
        ..SessionTelemetry::default()
    });
    settle().await;
    assert_eq!(
        mounted.element(".tui-model-name").text_content().as_deref(),
        Some("café")
    );
    assert_eq!(
        mounted.element(".tui-ctx-gauge").text_content().as_deref(),
        Some("Ctx: ~12.3k/~66k (19%)")
    );
    assert_eq!(
        mounted.element(".tui-speed").text_content().as_deref(),
        Some("~12.3 t/s")
    );
    assert_eq!(
        mounted
            .element(".tui-tools-count")
            .text_content()
            .as_deref(),
        Some("7 tools")
    );
    mounted.click(".tui-ctx-gauge");
    assert!(mounted.state.ui.context_open.get_untracked());
    mounted.state.chat.session_telemetry.set(SessionTelemetry {
        context_limit: 0,
        context_limit_estimated: false,
        ..SessionTelemetry::default()
    });
    settle().await;
    assert_eq!(
        mounted.element(".tui-ctx-gauge").text_content().as_deref(),
        Some("Ctx: 0.0k/0k (0%)")
    );
    assert_eq!(
        mounted.element(".tui-speed").text_content().as_deref(),
        Some("-- t/s")
    );
}

#[wasm_bindgen_test]
async fn shared_icons_and_tooltips_are_labeled_themeable_and_cleaned_up() {
    use super::support::wait_until;
    use openwebide_frontend::components::ui::{Icon, IconButton, IconName};
    let mounted = mount_test(|_| {
        openwebide_frontend::viewport::install_action_tooltips();
        view! {
            <style>{include_str!("../../styles.css")}</style>
            <IconButton label="Settings" on_click=Callback::new(|_| ())><Icon name=IconName::Settings /></IconButton>
        }
    });
    prepare_tooltip_fixture(&mounted).await;
    let button = mounted.element("button");
    assert_eq!(
        button.get_attribute("aria-label").as_deref(),
        Some("Settings")
    );
    let icon = mounted.element("svg");
    assert_eq!(icon.get_attribute("width").as_deref(), Some("20"));
    assert_eq!(
        icon.get_attribute("stroke").as_deref(),
        Some("currentColor")
    );
    assert_eq!(
        mounted
            .element(".ui-icon-glyph")
            .get_attribute("aria-hidden")
            .as_deref(),
        Some("true")
    );
    let focus = web_sys::FocusOptions::new();
    focus.set_prevent_scroll(true);
    button.focus_with_options(&focus).unwrap();
    button
        .dispatch_event(&web_sys::Event::new("focus").unwrap())
        .unwrap();
    wait_until("keyboard action tooltip", || {
        web_sys::window()
            .unwrap()
            .document()
            .unwrap()
            .get_element_by_id("ui-action-tooltip")
            .is_some_and(|tip| !tip.has_attribute("hidden"))
    })
    .await;
    let document = web_sys::window().unwrap().document().unwrap();
    let tooltip = document.get_element_by_id("ui-action-tooltip").unwrap();
    assert_eq!(tooltip.text_content().as_deref(), Some("Settings"));
    assert_eq!(
        button.get_attribute("aria-describedby").as_deref(),
        Some("ui-action-tooltip")
    );
    drop(mounted);
    assert!(document.get_element_by_id("ui-action-tooltip").is_none());
    assert_eq!(button.get_attribute("title").as_deref(), Some("Settings"));
}

#[wasm_bindgen_test]
async fn shared_dropdowns_fit_the_viewport_skip_disabled_options_and_dismiss_before_modals() {
    use openwebide_frontend::components::{
        Modal,
        dropdown::{DropdownSelect, SelectOption},
        ui::{DialogBody, PanelSearchRow},
    };
    use wasm_bindgen::JsCast;
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let shown = RwSignal::new(true);
        let value = RwSignal::new("b".to_string());
        let options = RwSignal::new(vec![
            SelectOption::new("a", "Alpha"),
            SelectOption::new("b", "Beta"),
            SelectOption {
                value: "disabled".into(),
                label: "Disabled".into(),
                disabled: true,
            },
            SelectOption::new("g", "Gamma"),
        ]);
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            view! { <style>{include_str!("../../styles.css")}</style>
                <Show when=move || shown.get()>
                    <Modal title=Signal::derive(|| "Choices".into()) on_close=Callback::new(move |()| shown.set(false))>
                        <DialogBody>
                            <DropdownSelect label="Choice" value=Signal::derive(move || value.get()) options=Signal::derive(move || options.get()) on_change=Callback::new(move |selection| value.set(selection)) />
                            <PanelSearchRow><input class="form-input panel-search-input" aria-label="Files search" /></PanelSearchRow>
                            <PanelSearchRow><input class="form-input panel-search-input" aria-label="Sessions search" /></PanelSearchRow>
                        </DialogBody>
                    </Modal>
                </Show>
            }
        });
        settle().await;
        mounted.click(".ui-dropdown-trigger");
        settle().await;
        let keyboard = |key: &str| {
            let init = web_sys::KeyboardEventInit::new();
            init.set_key(key);
            init.set_bubbles(true);
            init.set_cancelable(true);
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .active_element()
                .unwrap()
                .dispatch_event(
                    &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
                        .unwrap(),
                )
                .unwrap();
        };
        assert_eq!(
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .active_element()
                .unwrap()
                .get_attribute("data-value")
                .as_deref(),
            Some("b")
        );
        keyboard("ArrowDown");
        assert_eq!(
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .active_element()
                .unwrap()
                .get_attribute("data-value")
                .as_deref(),
            Some("g")
        );
        keyboard("Home");
        assert_eq!(
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .active_element()
                .unwrap()
                .get_attribute("data-value")
                .as_deref(),
            Some("a")
        );
        keyboard("b");
        assert_eq!(
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .active_element()
                .unwrap()
                .get_attribute("data-value")
                .as_deref(),
            Some("b")
        );
        options.update(|options| {
            options.extend(
                (0..60)
                    .map(|index| SelectOption::new(index.to_string(), format!("Choice {index}"))),
            );
        });
        settle().await;
        let menu = mounted
            .element(".ui-dropdown-menu")
            .get_bounding_client_rect();
        let viewport = web_sys::window()
            .unwrap()
            .inner_height()
            .unwrap()
            .as_f64()
            .unwrap();
        assert!(menu.top() >= 0.0 && menu.bottom() <= viewport);
        assert!(menu.height() <= 320.0);
        for theme in ["", "light"] {
            let html = web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .document_element()
                .unwrap();
            html.set_attribute("data-theme", theme).unwrap();
            let inputs = mounted
                .root
                .query_selector_all(".panel-search-input")
                .unwrap();
            let first: web_sys::Element = inputs.item(0).unwrap().unchecked_into();
            let second: web_sys::Element = inputs.item(1).unwrap().unchecked_into();
            let style = |element| {
                web_sys::window()
                    .unwrap()
                    .get_computed_style(element)
                    .unwrap()
                    .unwrap()
            };
            for property in ["padding", "height", "font-size", "background-color"] {
                assert_eq!(
                    style(&first).get_property_value(property).unwrap(),
                    style(&second).get_property_value(property).unwrap()
                );
            }
        }
        web_sys::window()
            .unwrap()
            .document()
            .unwrap()
            .document_element()
            .unwrap()
            .remove_attribute("data-theme")
            .unwrap();
        keyboard("Escape");
        settle().await;
        assert!(shown.get_untracked());
        assert!(
            mounted
                .root
                .query_selector(".ui-dropdown-menu")
                .unwrap()
                .is_none()
        );
        assert!(
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .active_element()
                .unwrap()
                .is_same_node(Some(mounted.element(".ui-dropdown-trigger").as_ref()))
        );
        super::support::choose_dropdown(&mounted, ".ui-dropdown-trigger", "g").await;
        assert_eq!(value.get_untracked(), "g");
        mounted.click(".ui-dropdown-trigger");
        settle().await;
        mounted.click(".ui-dropdown-backdrop");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".ui-dropdown-menu")
                .unwrap()
                .is_none()
        );
        keyboard("Escape");
        settle().await;
        assert!(!shown.get_untracked());
    }
}

#[wasm_bindgen_test]
async fn overflow_actions_close_before_dialogs_and_keep_dialog_callbacks_alive() {
    use openwebide_frontend::{
        components::{PromptDialog, dropdown::ActionMenu},
        state::ui::PromptRequest,
    };
    let submitted = RwSignal::new(String::new());
    let mounted = mount_test(move |state| {
        let ui = state.ui;
        view! {
            <style>{include_str!("../../styles.css")}</style>
            <ActionMenu aria_label="Example actions">
                <button role="menuitem" disabled=true>"Unavailable"</button>
                <button role="menuitem" class="rename" on:click=move |_| ui.set_prompt(PromptRequest {
                    title: "Rename".into(), value: "Original".into(), placeholder: String::new(), submit_label: "Save".into(),
                    on_submit: Callback::new(move |value| submitted.set(value)),
                })>"Rename"</button>
            </ActionMenu>
            <PromptDialog />
        }
    });
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".ui-dropdown-menu")
            .unwrap()
            .is_none()
    );
    mounted.click(".ui-dropdown-trigger");
    settle().await;
    assert_eq!(
        web_sys::window()
            .unwrap()
            .document()
            .unwrap()
            .active_element()
            .unwrap(),
        mounted.element(".rename").into()
    );
    mounted.click(".rename");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".ui-dropdown-menu")
            .unwrap()
            .is_none()
    );
    assert!(
        mounted.element(".modal").contains(
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .active_element()
                .as_ref()
                .map(AsRef::as_ref)
        )
    );
    use wasm_bindgen::JsCast;
    mounted
        .element(".modal input")
        .unchecked_into::<web_sys::HtmlInputElement>()
        .set_value("Changed");
    mounted.click(".modal-footer .send");
    settle().await;
    assert_eq!(submitted.get_untracked(), "Changed");
    assert!(mounted.root.query_selector(".modal").unwrap().is_none());
    assert_eq!(
        web_sys::window()
            .unwrap()
            .document()
            .unwrap()
            .active_element()
            .unwrap(),
        mounted.element(".ui-dropdown-trigger").into()
    );
}

#[wasm_bindgen_test]
async fn action_menus_open_from_rows_headers_and_touch_without_activating_them() {
    use openwebide_frontend::components::dropdown::ActionMenu;
    use wasm_bindgen::JsCast;
    let activations = RwSignal::new(0_u32);
    let mounted = mount_test(move |_| {
        view! {
            { ["session", "connection", "tui-user", "tool-panel-heading", "panel-toolbar", "git-diff-actions"].into_iter().map(|class| view! {
                <div class=class data-context-menu="">
                    <button class="primary" on:click=move |_| activations.update(|count| *count += 1)>"Primary action"</button>
                    <div class="nested-actions"><ActionMenu aria_label="Context actions"><button role="menuitem">"Secondary action"</button></ActionMenu></div>
                </div>
            }).collect_view() }
        }
    });
    settle().await;
    for class in [
        "session",
        "connection",
        "tui-user",
        "tool-panel-heading",
        "panel-toolbar",
        "git-diff-actions",
    ] {
        let row = mounted.element(&format!(".{class}"));
        let event = web_sys::MouseEventInit::new();
        event.set_bubbles(true);
        event.set_cancelable(true);
        event.set_client_x(100);
        event.set_client_y(80);
        let event =
            web_sys::MouseEvent::new_with_mouse_event_init_dict("contextmenu", &event).unwrap();
        row.dispatch_event(&event).unwrap();
        settle().await;
        assert!(event.default_prevented());
        assert!(row.query_selector(".ui-dropdown-menu").unwrap().is_some());
        mounted.click(".ui-dropdown-backdrop");
        settle().await;
        assert!(row.query_selector(".ui-dropdown-menu").unwrap().is_none());
        let primary: web_sys::HtmlElement = row
            .query_selector(".primary")
            .unwrap()
            .unwrap()
            .unchecked_into();
        let pointer = web_sys::PointerEventInit::new();
        pointer.set_bubbles(true);
        pointer.set_pointer_type("touch");
        pointer.set_client_x(100);
        pointer.set_client_y(80);
        primary
            .dispatch_event(
                &web_sys::PointerEvent::new_with_event_init_dict("pointerdown", &pointer).unwrap(),
            )
            .unwrap();
        openwebide_frontend::util::sleep_ms(550).await;
        settle().await;
        assert!(row.query_selector(".ui-dropdown-menu").unwrap().is_some());
        primary.click(); // Synthetic click following the completed long press.
        assert_eq!(activations.get_untracked(), 0);
        mounted.click(".ui-dropdown-backdrop");
        settle().await;
        assert!(row.query_selector(".ui-dropdown-menu").unwrap().is_none());
        assert!(mounted.root.contains(Some(row.as_ref())));
    }
}

#[wasm_bindgen_test]
async fn browser_context_menu_is_suppressed_even_without_an_app_menu() {
    let mounted = mount_test(|_| {
        openwebide_frontend::components::context_menu::disable_browser_menu();
        view! {<div class="blank">"Blank area"<input /></div>}
    });
    settle().await;
    for selector in [".blank", "input"] {
        let init = web_sys::MouseEventInit::new();
        init.set_bubbles(true);
        init.set_cancelable(true);
        let event =
            web_sys::MouseEvent::new_with_mouse_event_init_dict("contextmenu", &init).unwrap();
        mounted.element(selector).dispatch_event(&event).unwrap();
        assert!(event.default_prevented());
        assert!(
            mounted
                .root
                .query_selector(".ui-dropdown-menu")
                .unwrap()
                .is_none()
        );
    }
}

#[wasm_bindgen_test]
async fn editor_defaults_serialize_writes_rollback_failures_and_ignore_previous_accounts() {
    use openwebide_core::editor::{EditorPreferences, IndentStyle};
    use openwebide_frontend::components::Settings;
    use openwebide_frontend::testing::fake_backend::Call;
    use wasm_bindgen::JsCast;
    let slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let captured = slot.clone();
    let mounted = mount_test(move |state| {
        let actions = build_settings_actions(SettingsActionContext {
            api: state.api,
            settings: state.settings,
            ui: state.ui,
        });
        captured.set(Some(actions.on_set_editor_preferences));
        view! { <Settings on_set_notifications=actions.on_set_notifications on_set_theme=actions.on_set_theme on_set_editor_preferences=actions.on_set_editor_preferences on_set_default_prompt=actions.on_set_default_prompt on_set_bridge_url=actions.on_set_bridge_url /> }
    });
    settle().await;
    let save = slot.get().unwrap();
    let (release, waiting) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .editor_save_results
        .borrow_mut()
        .push_back(waiting);
    let mut preferences = EditorPreferences::default();
    preferences.indentation.style = IndentStyle::Tabs;
    save.run(preferences);
    settle().await;
    preferences.indentation.width = 2;
    save.run(preferences);
    settle().await;
    // Reading controls share this database writer and preserve indentation.
    for label in [
        "Word wrap",
        "Show whitespace",
        "Texture healing",
        "Coding ligatures",
    ] {
        let labels = mounted.root.query_selector_all("label.ui-check").unwrap();
        let mut clicked = false;
        for index in 0..labels.length() {
            let node = labels.item(index).unwrap();
            if node.text_content().as_deref() == Some(label) {
                node.unchecked_into::<web_sys::HtmlElement>().click();
                clicked = true;
                break;
            }
        }
        assert!(clicked, "Missing reading control {label}");
        settle().await;
    }
    preferences.word_wrap = true;
    preferences.show_whitespace = true;
    preferences.texture_healing = false;
    preferences.ligatures = false;
    mounted
        .root
        .query_selector("button[aria-label='Editor font']")
        .unwrap()
        .unwrap()
        .unchecked_into::<web_sys::HtmlElement>()
        .click();
    settle().await;
    mounted
        .root
        .query_selector("[data-value='Radon']")
        .unwrap()
        .unwrap()
        .unchecked_into::<web_sys::HtmlElement>()
        .click();
    settle().await;
    preferences.font = openwebide_core::editor::EditorFont::Radon;
    assert_eq!(
        mounted.state.settings.editor_preferences.get_untracked(),
        preferences
    );
    let count = || {
        mounted
            .state
            .fake
            .calls
            .borrow()
            .iter()
            .filter(
                |call| matches!(call, Call::SetSetting { key, .. } if key == "editor_preferences"),
            )
            .count()
    };
    assert_eq!(count(), 1, "only one preference write may be in flight");
    release.send(Ok(())).unwrap();
    settle().await;
    assert_eq!(count(), 2);
    let stored: EditorPreferences =
        serde_json::from_str(&mounted.state.fake.settings.borrow()["editor_preferences"]).unwrap();
    assert_eq!(stored, preferences);
    let (release, waiting) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .editor_save_results
        .borrow_mut()
        .push_back(waiting);
    let mut changed = preferences;
    changed.indentation.width = 8;
    save.run(changed);
    settle().await;
    release.send(Err("offline".into())).unwrap();
    settle().await;
    assert_eq!(
        mounted.state.settings.editor_preferences.get_untracked(),
        preferences
    );
    assert!(
        mounted
            .state
            .ui
            .toast
            .get_untracked()
            .unwrap()
            .contains("Could not save editor preferences")
    );
    let (release, waiting) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .editor_save_results
        .borrow_mut()
        .push_back(waiting);
    save.run(changed);
    settle().await;
    mounted
        .state
        .auth
        .generation
        .update(|generation| *generation += 1);
    mounted
        .state
        .settings
        .editor_preferences
        .set(Default::default());
    settle().await;
    release.send(Err("old account".into())).unwrap();
    settle().await;
    assert_eq!(
        mounted.state.settings.editor_preferences.get_untracked(),
        EditorPreferences::default()
    );
}

#[wasm_bindgen_test]
async fn editor_defaults_restore_from_database_without_overwriting_a_newer_choice() {
    use openwebide_core::editor::{EditorPreferences, IndentStyle};
    use openwebide_frontend::testing::fake_backend::Call;
    for changed_during_load in [false, true] {
        let (release, waiting) = futures::channel::oneshot::channel();
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let captured = slot.clone();
        let mounted = mount_test(move |state| {
            let actions = build_settings_actions(SettingsActionContext {
                api: state.api,
                settings: state.settings,
                ui: state.ui,
            });
            captured.set(Some(actions.on_set_editor_preferences));
            state
                .fake
                .settings_load_results
                .borrow_mut()
                .push_back(waiting);
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "test".into(),
                role: UserRole::User,
                created_at: 0,
            });
            install_project_effects(ProjectEffectContext {
                api: state.api,
                health: RwSignal::new(None),
                auth: state.auth,
                settings: state.settings,
                projects: state.projects,
                chat: state.chat,
                layout: expect_context::<LayoutState>(),
                select_project: Callback::new(|_| ()),
            });
            view! { <div /> }
        });
        super::support::wait_until("editor defaults database load", || {
            mounted.state.fake.calls.borrow().iter().any(|call| {
                matches!(
                    call,
                    Call::Request {
                        method: "get_settings"
                    }
                )
            })
        })
        .await;
        let mut saved = EditorPreferences::default();
        saved.indentation.style = IndentStyle::Tabs;
        saved.indentation.width = 6;
        saved.word_wrap = true;
        saved.show_whitespace = true;
        saved.font = openwebide_core::editor::EditorFont::Krypton;
        saved.texture_healing = false;
        saved.ligatures = false;
        let mut chosen = saved;
        chosen.indentation.width = 2;
        chosen.font = openwebide_core::editor::EditorFont::Xenon;
        chosen.ligatures = true;
        if changed_during_load {
            slot.get().unwrap().run(chosen);
            settle().await;
        }
        release
            .send(Ok(std::collections::BTreeMap::from([
                (
                    "editor_preferences".into(),
                    serde_json::to_string(&saved).unwrap(),
                ),
                ("browser_notifications".into(), "true".into()),
            ])))
            .unwrap();
        super::support::wait_until("settings load completion", || {
            mounted.state.settings.browser_notifications.get_untracked()
        })
        .await;
        assert_eq!(
            mounted.state.settings.editor_preferences.get_untracked(),
            if changed_during_load { chosen } else { saved }
        );
    }
}

#[wasm_bindgen_test]
async fn narrow_chat_keeps_model_approval_and_context_controls_accessible_in_both_modes() {
    use openwebide_core::{SessionTelemetry, WorkspaceMode};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            view! { <style>{include_str!("../../styles.css")}</style><div class="phone-layout" style="width:280px">{super::support::chat_view(state)}</div> }
        });
        mounted.state.chat.session_telemetry.set(SessionTelemetry {
            model: "a very long model name with many parameters and versions".into(),
            context_tokens: 12000,
            context_limit: 32000,
            ..SessionTelemetry::default()
        });
        settle().await;
        let status = mounted.element(".tui-statusline");
        assert!(
            status.scroll_width() <= status.client_width(),
            "status overflow in {mode:?}"
        );
        for selector in [".tui-mode-badge", ".tui-model-name", ".tui-ctx-gauge"] {
            let bounds = mounted.element(selector).get_bounding_client_rect();
            assert!(bounds.width() > 0.0 && bounds.height() >= 44.0);
            assert!(bounds.right() <= status.get_bounding_client_rect().right() + 1.0);
        }
        mounted.click(".tui-ctx-gauge");
        assert!(mounted.state.ui.context_open.get_untracked());
    }
}

#[wasm_bindgen_test]
async fn pointer_tab_focus_and_editor_scroll_do_not_reopen_tooltips() {
    use super::support::wait_until;
    let mounted = mount_test(|_| {
        openwebide_frontend::viewport::install_action_tooltips();
        // The fixed tooltip overlay must use the production stylesheet; an
        // unstyled popup changes page layout and can dismiss itself on scroll.
        view! { <style>{include_str!("../../styles.css")}</style><button title="File path">"File"</button><div class="scroll-source"></div> }
    });
    prepare_tooltip_fixture(&mounted).await;
    let button = mounted.element("button");
    let event = |name| {
        let init = web_sys::MouseEventInit::new();
        init.set_bubbles(true);
        button
            .dispatch_event(
                &web_sys::MouseEvent::new_with_mouse_event_init_dict(name, &init).unwrap(),
            )
            .unwrap();
    };
    event("pointerover");
    let tip = document().get_element_by_id("ui-action-tooltip").unwrap();
    wait_until("hovered file tooltip", || !tip.has_attribute("hidden")).await;
    event("pointerdown");
    button.focus().unwrap();
    button
        .dispatch_event(&web_sys::Event::new("focus").unwrap())
        .unwrap();
    mounted
        .element(".scroll-source")
        .dispatch_event(&web_sys::Event::new("scroll").unwrap())
        .unwrap();
    openwebide_frontend::util::sleep_ms(250).await;
    settle().await;
    assert!(
        tip.has_attribute("hidden"),
        "pointer focus/scroll reopened the tooltip"
    );
    assert!(
        !button.has_attribute("title"),
        "native tooltip can reopen while the pointer is still over the clicked tab"
    );
    event("pointerout");
    assert_eq!(button.get_attribute("title").as_deref(), Some("File path"));
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Tab");
    init.set_bubbles(true);
    button
        .dispatch_event(
            &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap(),
        )
        .unwrap();
    button
        .dispatch_event(&web_sys::Event::new("focus").unwrap())
        .unwrap();
    wait_until("keyboard file tooltip", || !tip.has_attribute("hidden")).await;
    mounted
        .element(".scroll-source")
        .dispatch_event(&web_sys::Event::new("scroll").unwrap())
        .unwrap();
    settle().await;
    openwebide_frontend::util::sleep_ms(20).await;
    assert!(
        tip.has_attribute("hidden"),
        "scroll reopened a keyboard tooltip"
    );
}

#[wasm_bindgen_test]
async fn prompt_responses_cannot_update_another_account_in_both_modes() {
    use openwebide_core::{SystemPrompt, WorkspaceMode};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for operation in ["create", "update", "delete"] {
            let (save_done, save_wait) = futures::channel::oneshot::channel();
            let (delete_done, delete_wait) = futures::channel::oneshot::channel();
            let actions = std::rc::Rc::new(std::cell::Cell::new(None));
            let slot = actions.clone();
            let mounted = mount_test(move |state| {
                state.seed_project();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
                state.auth.set_user(User {
                    id: UserId::new(1),
                    username: "alice".into(),
                    role: UserRole::User,
                    created_at: 0,
                });
                state
                    .fake
                    .prompt_save_results
                    .borrow_mut()
                    .push_back(save_wait);
                state
                    .fake
                    .prompt_delete_results
                    .borrow_mut()
                    .push_back(delete_wait);
                slot.set(Some(build_settings_actions(SettingsActionContext {
                    api: state.api,
                    settings: state.settings,
                    ui: state.ui,
                })));
                view! { <div /> }
            });
            let actions = actions.get().unwrap();
            mounted
                .state
                .settings
                .prompt_name
                .set("Alice's prompt".into());
            mounted
                .state
                .settings
                .prompt_content
                .set("Alice's instructions".into());
            if operation == "delete" {
                actions.on_delete_prompt.run(1);
                mounted
                    .state
                    .ui
                    .confirm
                    .get_untracked()
                    .unwrap()
                    .action
                    .run(());
            } else {
                mounted
                    .state
                    .settings
                    .prompt_edit_id
                    .set((operation == "update").then_some(1));
                actions.on_save_prompt.run(());
            }
            settle().await;
            mounted.state.auth.set_user(User {
                id: UserId::new(2),
                username: "bob".into(),
                role: UserRole::User,
                created_at: 0,
            });
            let bob = SystemPrompt {
                id: 1,
                name: "Bob's prompt".into(),
                content: "Bob's instructions".into(),
            };
            mounted.state.settings.system_prompts.set(vec![bob.clone()]);
            mounted.state.settings.default_prompt.set(Some(1));
            mounted.state.settings.show_prompt_form.set(true);
            if operation == "delete" {
                delete_done.send(Ok(())).unwrap();
            } else {
                save_done
                    .send(Ok(SystemPrompt {
                        id: 1,
                        name: "Alice's prompt".into(),
                        content: "Alice's instructions".into(),
                    }))
                    .unwrap();
            }
            settle().await;
            assert_eq!(
                mounted.state.settings.system_prompts.get_untracked(),
                vec![bob]
            );
            assert_eq!(
                mounted.state.settings.default_prompt.get_untracked(),
                Some(1)
            );
            assert!(mounted.state.settings.show_prompt_form.get_untracked());
        }
    }
}

#[wasm_bindgen_test]
async fn overflow_actions_close_when_the_action_immediately_disables_its_button() {
    let mounted = mount_test(move |_| {
        let busy = RwSignal::new(false);
        view! {<openwebide_frontend::components::dropdown::ActionMenu aria_label="Busy action"><button role="menuitem" disabled=move ||busy.get() on:click=move |_|busy.set(true)>"Run action"</button></openwebide_frontend::components::dropdown::ActionMenu>}
    });
    mounted.click("[aria-label=\"Busy action\"]");
    settle().await;
    mounted.click("[role=menuitem]");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".ui-dropdown-menu")
            .unwrap()
            .is_none()
    );
}
