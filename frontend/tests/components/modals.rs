use leptos::prelude::*;
use openwebide_core::FileEntry;
use openwebide_frontend::{
    components::{ConfirmDialog, FileBrowser, PromptDialog, Settings},
    state::ui::{ConfirmRequest, PromptRequest},
    state_actions::lifecycle::install_keyboard_shortcuts,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;

use super::support::{mount_test, settle, wait_until};

#[wasm_bindgen::prelude::wasm_bindgen(
    inline_js = "export function noticeFetch(html) { const original=window.fetch; let requests=0; let fail=false; window.fetch=async request => { if (!request.url.includes('/about-software-')) return original(request); requests++; return new Response(html, {status:fail?503:200}); }; return { count:()=>requests, fail:value=>{fail=value;}, restore:()=>{window.fetch=original;} }; }"
)]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(js_name = noticeFetch)]
    fn notice_fetch(html: &str) -> js_sys::Object;
}

fn notice_call(mock: &js_sys::Object, name: &str) -> wasm_bindgen::JsValue {
    let function: js_sys::Function = js_sys::Reflect::get(mock, &name.into())
        .unwrap()
        .unchecked_into();
    function.call0(mock).unwrap()
}

fn active() -> web_sys::Element {
    document().active_element().unwrap()
}

fn key(target: &web_sys::HtmlElement, name: &str, shift: bool, ctrl: bool) -> bool {
    let init = web_sys::KeyboardEventInit::new();
    init.set_key(name);
    init.set_shift_key(shift);
    init.set_ctrl_key(ctrl);
    init.set_bubbles(true);
    init.set_cancelable(true);
    target
        .dispatch_event(
            &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap(),
        )
        .unwrap()
}

#[wasm_bindgen_test]
async fn confirm_focus_traps_live_controls_and_restores_opener() {
    let mounted = mount_test(|state| {
        state.seed_project();
        super::support::command_actions(state.clone());
        install_keyboard_shortcuts(state.chat);
        view! { <button class="opener">"Open"</button> <ConfirmDialog /> }
    });
    let opener = mounted.element(".opener");
    opener.focus().unwrap();
    mounted.state.ui.set_confirm(ConfirmRequest {
        title: "Delete?".into(),
        message: "Permanent deletion".into(),
        confirm_label: "Delete".into(),
        action: Callback::new(|()| ()),
    });
    settle().await;
    let panel = mounted.element(".modal");
    let primary = mounted.element(".danger");
    assert!(active().is_same_node(Some(&primary)));
    assert_eq!(panel.get_attribute("role").as_deref(), Some("dialog"));
    assert_eq!(panel.get_attribute("aria-modal").as_deref(), Some("true"));
    assert_eq!(
        document()
            .get_element_by_id(&panel.get_attribute("aria-labelledby").unwrap())
            .unwrap()
            .text_content()
            .as_deref(),
        Some("Delete?")
    );
    assert_eq!(
        document()
            .get_element_by_id(&panel.get_attribute("aria-describedby").unwrap())
            .unwrap()
            .text_content()
            .as_deref(),
        Some("Permanent deletion")
    );
    key(&primary, "Tab", false, false);
    assert!(active().is_same_node(Some(&mounted.element(".icon-btn"))));
    key(&mounted.element(".icon-btn"), "Tab", true, false);
    assert!(active().is_same_node(Some(&primary)));
    mounted
        .element(".modal-footer .btn")
        .set_attribute("disabled", "")
        .unwrap();
    primary.set_attribute("hidden", "").unwrap();
    key(&mounted.element(".icon-btn"), "Tab", false, false);
    assert!(active().is_same_node(Some(&mounted.element(".icon-btn"))));
    mounted
        .element(".icon-btn")
        .set_attribute("disabled", "")
        .unwrap();
    key(&panel, "Tab", false, false);
    assert!(active().is_same_node(Some(&panel)));
    opener.focus().unwrap();
    // An unfocused headless document can change activeElement without emitting focusin.
    opener
        .dispatch_event(&web_sys::Event::new("focusin").unwrap())
        .unwrap();
    assert!(
        panel.contains(Some(&active())),
        "active={}, panel={}",
        active().outer_html(),
        panel.outer_html()
    );
    key(&panel, "`", false, true);
    assert!(!mounted.state.chat.show_terminal.get_untracked());
    assert!(!key(&panel, "Escape", false, false));
    settle().await;
    assert!(mounted.state.ui.confirm.get_untracked().is_none());
    assert!(active().is_same_node(Some(&opener)));
    assert!(key(&opener, "Tab", false, false));
    key(&opener, "`", false, true);
    assert!(mounted.state.chat.show_terminal.get_untracked());
}

#[wasm_bindgen_test]
async fn stacked_prompt_submits_once_and_returns_focus_to_settings() {
    let submits = RwSignal::new(0);
    let count = submits;
    let mounted = mount_test(|state| {
        view! {
            <Show when=move || state.settings.show_settings.get()>
                <Settings on_set_notifications=Callback::new(|_| ()) on_set_theme=Callback::new(|_| ()) on_set_default_prompt=Callback::new(|_| ()) on_set_bridge_url=Callback::new(|_| ()) />
            </Show>
            <PromptDialog /> <ConfirmDialog />
        }
    });
    mounted.state.settings.show_settings.set(true);
    settle().await;
    let field = mounted.element(".ui-seg-btn");
    assert!(active().is_same_node(Some(&field)));
    mounted.state.ui.set_prompt(PromptRequest {
        title: "Name".into(),
        value: "Example".into(),
        placeholder: String::new(),
        submit_label: "Create".into(),
        on_submit: Callback::new(move |value: String| {
            assert_eq!(value, "Example");
            count.update(|count| *count += 1);
        }),
    });
    settle().await;
    let input = mounted.element(".modal-sm input");
    assert!(active().is_same_node(Some(&input)));
    mounted.state.ui.set_confirm(ConfirmRequest {
        title: "Confirm".into(),
        message: "Sure?".into(),
        confirm_label: "Yes".into(),
        action: Callback::new(|()| ()),
    });
    settle().await;
    key(&mounted.element(".danger"), "Escape", false, false);
    settle().await;
    assert!(mounted.state.ui.confirm.get_untracked().is_none());
    assert!(mounted.state.ui.prompt.get_untracked().is_some());
    assert!(active().is_same_node(Some(&input)));
    key(&input, "Enter", false, false);
    settle().await;
    assert_eq!(submits.get_untracked(), 1);
    assert!(mounted.state.ui.prompt.get_untracked().is_none());
    assert!(active().is_same_node(Some(&field)));
    key(&field, "Escape", false, false);
    settle().await;
    assert!(!mounted.state.settings.show_settings.get_untracked());
}

#[wasm_bindgen_test]
async fn folder_navigation_has_live_keyboard_controls_and_safe_missing_opener() {
    let selected = RwSignal::new(None::<String>);
    let shown = RwSignal::new(false);
    let mounted = mount_test(move |state| {
        state.fake.browse_entries.borrow_mut().insert(
            String::new(),
            vec![FileEntry {
                name: "folder".into(),
                path: "folder".into(),
                is_dir: true,
                size: 0,
            }],
        );
        view! {
            <button class="opener">"Open"</button>
            <Show when=move || shown.get()>
                <FileBrowser on_close=Callback::new(move |()| shown.set(false)) on_select=Callback::new(move |path| selected.set(Some(path))) />
            </Show>
        }
    });
    mounted.element(".opener").focus().unwrap();
    shown.set(true);
    settle().await;
    assert!(active().is_same_node(Some(&mounted.element(".send"))));
    let folder = mounted.element(".browser-item.dir");
    assert_eq!(folder.tag_name(), "BUTTON");
    assert!(!folder.has_attribute("disabled"));
    key(&mounted.element(".send"), "Tab", false, false);
    key(&mounted.element(".icon-btn"), "Tab", false, false);
    assert!(active().is_same_node(Some(&folder)));
    // Native buttons activate with Enter/Space; click is the resulting activation event.
    folder.click();
    settle().await;
    assert!(!folder.is_connected());
    assert!(
        mounted.element(".modal").contains(Some(&active())),
        "folder navigation lost dialog focus: {}",
        active().tag_name()
    );
    assert!(
        mounted
            .element(".browser-path")
            .text_content()
            .unwrap()
            .contains("folder")
    );
    let up = mounted.element(".browser-item");
    assert_eq!(up.tag_name(), "BUTTON");
    up.focus().unwrap();
    up.click();
    settle().await;
    assert!(!up.is_connected());
    assert!(
        mounted.element(".modal").contains(Some(&active())),
        "parent navigation lost dialog focus: {}",
        active().tag_name()
    );
    assert!(
        mounted
            .element(".browser-path")
            .text_content()
            .unwrap()
            .contains("~/source")
    );
    mounted.element(".opener").remove();
    mounted.element(".send").click();
    settle().await;
    assert_eq!(selected.get_untracked().as_deref(), Some(""));
    assert!(mounted.root.query_selector(".modal").unwrap().is_none());
    assert!(key(&mounted.root, "Tab", false, false));
    shown.set(true);
    settle().await;
    drop(mounted);
    settle().await;
    let body: web_sys::HtmlElement = document().body().unwrap().unchecked_into();
    assert!(key(&body, "Escape", false, false));
}

#[wasm_bindgen_test]
async fn about_shows_build_and_local_notices_in_both_modes_and_restores_focus() {
    use openwebide_core::{User, UserId, UserRole, WorkspaceMode};
    use openwebide_frontend::{
        commands::Command,
        components::{CommandDialogs, TopBar},
    };
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mock = notice_fetch(include_str!(concat!(
            env!("OUT_DIR"),
            "/about-software.html"
        )));
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "test".into(),
                role: UserRole::User,
                created_at: 0,
            });
            super::support::command_actions(state);
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <TopBar on_open_settings=Callback::new(|()| ()) on_logout=Callback::new(|()| ()) />
                <CommandDialogs />
            }
        });
        settle().await;
        mounted.element("[aria-label='App menu']").focus().unwrap();
        mounted.click("[aria-label='App menu']");
        settle().await;
        mounted.element("[aria-label='About']").focus().unwrap();
        mounted.click("[aria-label='About']");
        settle().await;
        let panel = mounted.element("[role='dialog']");
        let about_bounds = panel.get_bounding_client_rect();
        let text = panel.text_content().unwrap();
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        assert!(text.contains("Commit"));
        assert!(!text.contains("SIL OPEN FONT LICENSE"));
        let website = mounted.element(".about-overview a");
        assert_eq!(website.text_content().as_deref(), Some("openwebide.com"));
        assert_eq!(
            website.get_attribute("href").as_deref(),
            Some("https://openwebide.com/")
        );
        assert_eq!(website.get_attribute("target").as_deref(), Some("_blank"));
        assert!(
            mounted
                .root
                .query_selector(".about-logo svg")
                .unwrap()
                .is_some()
        );
        assert_eq!(notice_call(&mock, "count").as_f64(), Some(0.0));
        let overview = mounted
            .element(".about-overview")
            .get_bounding_client_rect();
        let details = mounted
            .element(".about-overview > div")
            .get_bounding_client_rect();
        assert!(
            (overview.x() + f64::from(mounted.element(".about-overview").client_width()) / 2.0
                - details.x()
                - details.width() / 2.0)
                .abs()
                < 2.0
        );
        // Safe centering starts at the top when the content needs scrolling.
        if mounted.element(".about-overview").scroll_height()
            <= mounted.element(".about-overview").client_height()
        {
            assert!(
                (overview.y() + overview.height() / 2.0 - details.y() - details.height() / 2.0)
                    .abs()
                    < 2.0
            );
        } else {
            assert!(details.y() >= overview.y() && details.y() < overview.bottom());
        }
        assert_eq!(
            mounted
                .element("#about-tab-overview")
                .get_attribute("aria-selected")
                .as_deref(),
            Some("true")
        );
        key(
            &mounted.element("#about-tab-overview"),
            "ArrowRight",
            false,
            false,
        );
        settle().await;
        assert!(active().is_same_node(Some(&mounted.element("#about-tab-software"))));
        assert_eq!(
            mounted
                .element("#about-tab-software")
                .get_attribute("aria-selected")
                .as_deref(),
            Some("true")
        );
        wait_until("license notices", || {
            panel
                .text_content()
                .unwrap()
                .contains("SIL OPEN FONT LICENSE")
        })
        .await;
        let text = panel.text_content().unwrap();
        assert!(!text.contains("Commit"));
        assert!(text.contains("SIL OPEN FONT LICENSE"));
        assert!(text.contains("Lucide icons"));
        assert!(text.contains("leptos"));
        assert!(text.contains("Copyright (c) 2022 Greg Johnston"));
        assert_eq!(notice_call(&mock, "count").as_f64(), Some(1.0));
        let software_bounds = panel.get_bounding_client_rect();
        assert!((software_bounds.height() - about_bounds.height()).abs() < 1.0);
        assert!((software_bounds.y() - about_bounds.y()).abs() < 1.0);
        let body = mounted.element(".about-body");
        let notices_panel = mounted.element(".about-software");
        assert!(notices_panel.scroll_height() > notices_panel.client_height());
        assert!(body.scroll_height() <= body.client_height());
        let contents = mounted.element(".about-content").inner_html();
        assert!(
            mounted
                .root
                .query_selector(".ui-dropdown-menu")
                .unwrap()
                .is_none()
        );
        key(&panel, "Escape", false, false);
        settle().await;
        assert!(
            mounted
                .root
                .query_selector("[role='dialog']")
                .unwrap()
                .is_none()
        );
        assert!(
            active().is_same_node(Some(&mounted.element("[aria-label='App menu']"))),
            "focus after About: {}",
            active().outer_html()
        );
        mounted.state.ui.palette_open.set(true);
        settle().await;
        let search: web_sys::HtmlInputElement = mounted.element(".command-search").unchecked_into();
        search.set_value("licenses");
        let input = web_sys::EventInit::new();
        input.set_bubbles(true);
        search
            .dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &input).unwrap())
            .unwrap();
        settle().await;
        mounted.click("#command-about");
        settle().await;
        assert!(!mounted.state.ui.palette_open.get_untracked());
        assert!(
            mounted
                .element(".about-content")
                .text_content()
                .unwrap()
                .contains("Commit")
        );
        let fail: js_sys::Function = js_sys::Reflect::get(&mock, &"fail".into())
            .unwrap()
            .unchecked_into();
        fail.call1(&mock, &true.into()).unwrap();
        mounted.click("#about-tab-software");
        settle().await;
        assert!(
            mounted
                .element("[role='alert']")
                .text_content()
                .unwrap()
                .contains("Could not load")
        );
        fail.call1(&mock, &false.into()).unwrap();
        mounted.click(".about-content .btn");
        wait_until("retried license notices", || {
            mounted
                .element(".about-content")
                .text_content()
                .unwrap()
                .contains("SIL OPEN FONT LICENSE")
        })
        .await;
        assert_eq!(mounted.element(".about-content").inner_html(), contents);
        key(
            &mounted.element("#about-tab-software"),
            "Home",
            false,
            false,
        );
        settle().await;
        assert!(
            mounted
                .element(".about-content")
                .text_content()
                .unwrap()
                .contains("Commit")
        );
        assert_eq!(notice_call(&mock, "count").as_f64(), Some(3.0));
        mounted.click("#about-tab-software");
        settle().await;
        assert_eq!(notice_call(&mock, "count").as_f64(), Some(3.0));
        mounted.state.ui.about_open.set(false);
        settle().await;
        notice_call(&mock, "restore");
        assert!(Command::About.unavailable(Default::default()).is_none());
    }
}

#[wasm_bindgen_test]
async fn responsive_dialog_sizes_keep_headers_actions_and_scroll_inside_viewport() {
    use openwebide_frontend::components::{
        Modal,
        ui::{DialogActions, DialogBody, DialogSize},
    };
    for phone in [false, true] {
        for size in [DialogSize::Small, DialogSize::Standard, DialogSize::Wide] {
            let mounted = mount_test(move |_| {
                view! {
                    <style>{include_str!("../../styles.css")}</style>
                    <div class:phone-layout=phone style="--visible-height:360px">
                        <Modal title="Sizing check".to_string().into() size=size on_close=Callback::new(|()|())>
                            <DialogBody><div style="height:1200px;flex:none">"Long content"</div></DialogBody>
                            <DialogActions><button class="btn">"Done"</button></DialogActions>
                        </Modal>
                    </div>
                }
            });
            settle().await;
            let panel = mounted.element(".modal").get_bounding_client_rect();
            let width = f64::from(document().document_element().unwrap().client_width());
            let gutter = if phone || width < 800.0 {
                8.0
            } else if width <= 1100.0 {
                16.0
            } else {
                24.0
            };
            assert!(panel.left() >= gutter - 1.0 && panel.right() <= width - gutter + 1.0);
            assert!(panel.top() >= gutter - 1.0 && panel.bottom() <= 360.0 - gutter + 1.0);
            let expected = if phone || width < 800.0 {
                width - 2.0 * gutter
            } else {
                let preferred: f64 = match size {
                    DialogSize::Small => 380.0,
                    DialogSize::Standard => 480.0,
                    DialogSize::Wide => 720.0,
                    DialogSize::Available => width,
                };
                preferred.min(width - 2.0 * gutter)
            };
            assert!(
                (panel.width() - expected).abs() < 1.0,
                "expected {expected}, got {}",
                panel.width()
            );
            for selector in [".modal-header", ".modal-footer"] {
                let bounds = mounted.element(selector).get_bounding_client_rect();
                assert!(bounds.top() >= panel.top() && bounds.bottom() <= panel.bottom());
            }
            let body = mounted.element(".modal-body");
            assert!(body.scroll_height() > body.client_height());
            assert!(body.client_height() > 0);
            assert!(
                mounted.element(".modal").scroll_height()
                    <= mounted.element(".modal").client_height()
            );
        }
    }
}

#[wasm_bindgen_test]
async fn dropdowns_wait_for_ready_content_and_preserve_option_nodes() {
    use openwebide_frontend::components::{
        BranchPicker,
        dropdown::{DropdownSelect, SelectOption},
    };
    let options = RwSignal::new(vec![SelectOption::new("one", "One")]);
    let mounted = mount_test(move |state| {
        state
            .git
            .status
            .set(Some(openwebide_core::git::GitRepoStatus::default()));
        view! {
            <style>{include_str!("../../styles.css")}</style>
            <BranchPicker on_load=Callback::new(move |()| state.git.branches_loading.set(true)) />
            <DropdownSelect label="Stable options" value=Signal::derive(|| "one".to_string()) options=Signal::derive(move || options.get()) on_change=Callback::new(|_|()) />
        }
    });
    mounted.click("[aria-label='Git branch']");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".ui-dropdown-menu")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        mounted
            .element("[aria-label='Git branch']")
            .get_attribute("aria-busy")
            .as_deref(),
        Some("true")
    );
    mounted
        .state
        .git
        .branches
        .set(vec![openwebide_core::git::GitBranchInfo {
            name: "feature".into(),
            is_current: false,
            is_remote: false,
            upstream: None,
        }]);
    mounted.state.git.branches_loading.set(false);
    settle().await;
    let menu = mounted.element(".ui-dropdown-menu");
    assert!(
        menu.get_attribute("style")
            .unwrap()
            .contains("visibility: visible")
    );
    assert!(
        menu.query_selector("[data-value='branch:feature']")
            .unwrap()
            .is_some()
    );
    mounted.click(".ui-dropdown-backdrop");
    settle().await;
    mounted.click("[aria-label='Stable options']");
    settle().await;
    let row = mounted.element("[data-value='one']");
    options.set(vec![SelectOption {
        value: "one".into(),
        label: "Renamed".into(),
        disabled: true,
    }]);
    settle().await;
    assert!(row.is_same_node(Some(&mounted.element("[data-value='one']"))));
    assert_eq!(row.text_content().as_deref(), Some("Renamed"));
    assert!(row.has_attribute("disabled"));
}

#[wasm_bindgen_test]
async fn grouped_file_tab_actions_match_menu_rows_and_selected_tab_click_is_a_no_op() {
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.projects.projects.update(|items| items[0].mode = mode);
            for path in ["a.txt", "b.txt"] {
                state.workspace.register_editor_tab(1, path.into());
            }
            state.workspace.open_file.set(Some("a.txt".into()));
            state.workspace.content.set("Source".into());
            view! { <style>{include_str!("../../styles.css")}</style>{super::support::editor_view(state)} }
        });
        settle().await;
        let revision = mounted.state.workspace.editor_read_revision.get_untracked();
        mounted.click("[data-editor-tab='a.txt']");
        settle().await;
        assert_eq!(
            mounted.state.workspace.editor_read_revision.get_untracked(),
            revision
        );
        mounted
            .element("[data-editor-tab='a.txt']")
            .dispatch_event(&web_sys::MouseEvent::new("contextmenu").unwrap())
            .unwrap();
        settle().await;
        let menu = mounted.element(".ui-dropdown-menu");
        assert_eq!(
            menu.query_selector(".ui-menu-heading")
                .unwrap()
                .unwrap()
                .text_content()
                .as_deref(),
            Some("Tab actions")
        );
        let rows = menu.query_selector_all(".ui-dropdown-item").unwrap();
        for index in 0..rows.length() {
            let row = rows
                .item(index)
                .unwrap()
                .dyn_into::<web_sys::Element>()
                .unwrap();
            assert!(row.class_list().contains("recent-item"));
        }
    }
}

#[wasm_bindgen_test]
async fn file_switches_retain_tab_geometry_scroll_and_view_controls() {
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.projects.projects.update(|items| items[0].mode = mode);
            for path in [
                "source.rs",
                "README.md",
                "third.txt",
                "fourth.txt",
                "fifth.txt",
            ] {
                state.workspace.register_editor_tab(1, path.into());
            }
            state.workspace.open_file.set(Some("source.rs".into()));
            state.workspace.content.set("Source".into());
            view! { <style>{include_str!("../../styles.css")}</style>{super::support::editor_view(state)} }
        });
        settle().await;
        let editor = mounted.element(".editor");
        for width in [380, 520, 760, 1000] {
            editor
                .style()
                .set_property("width", &format!("{width}px"))
                .unwrap();
            editor.style().set_property("flex", "none").unwrap();
            settle().await;
            let strip = mounted.element(".editor-file-tabs");
            strip.set_scroll_left(60.0);
            let original = strip.get_bounding_client_rect();
            let height = mounted
                .element(".editor-header")
                .get_bounding_client_rect()
                .height();
            let scroll = strip.scroll_left();
            let tab = mounted.element("[data-editor-tab='README.md']");
            let position = tab.get_bounding_client_rect();
            let control = mounted.element(".editor-view-desktop .ui-seg-btn");
            for path in ["README.md", "source.rs", "README.md", "source.rs"] {
                mounted.state.workspace.open_file.set(Some(path.into()));
                settle().await;
                let current = strip.get_bounding_client_rect();
                assert!(
                    (current.width() - original.width()).abs() < 0.1,
                    "strip width changed at {width}: {} -> {}",
                    original.width(),
                    current.width()
                );
                assert!(
                    (mounted
                        .element(".editor-header")
                        .get_bounding_client_rect()
                        .height()
                        - height)
                        .abs()
                        < 0.1,
                    "header height changed at {width}"
                );
                assert_eq!(
                    strip.scroll_left().to_bits(),
                    scroll.to_bits(),
                    "tab scroll changed at {width}"
                );
                assert!(tab.is_same_node(Some(&mounted.element("[data-editor-tab='README.md']"))));
                assert!(
                    (tab.get_bounding_client_rect().x() - position.x()).abs() < 0.1,
                    "tab moved at {width}"
                );
                assert!(
                    control
                        .is_same_node(Some(&mounted.element(".editor-view-desktop .ui-seg-btn"))),
                    "view controls remounted at {width}"
                );
            }
        }
    }
}

#[wasm_bindgen_test]
async fn long_project_tabs_keep_close_buttons_inside_their_bounds() {
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.projects.projects.update(|items| {
                items[0].name = "Carvana.ClaudeCloud.Api.with.a.long.name".into();
                items[0].mode = mode;
                let mut second = items[0].clone();
                second.id = 2;
                second.name = "open-webide".into();
                items.push(second);
            });
            state.projects.open_tab_ids.set(vec![1, 2]);
            let projects = state.projects;
            view! { <style>{include_str!("../../styles.css")}</style><div class="app-navigation" style="width:720px">
                <openwebide_frontend::components::TabBar on_select=Callback::new(move |id| projects.active_project.set(Some(id))) on_select_chat=Callback::new(|()| ()) on_close=Callback::new(|_| ()) />
            </div> }
        });
        settle().await;
        let first = mounted.element("[data-project-tab='1']");
        let second = mounted.element("[data-project-tab='2']");
        let first_position = first.get_bounding_client_rect();
        let second_position = second.get_bounding_client_rect();
        for project in [2, 1, 2, 1] {
            mounted.state.projects.active_project.set(Some(project));
            settle().await;
            assert!(first.is_same_node(Some(&mounted.element("[data-project-tab='1']"))));
            assert!(
                (first.get_bounding_client_rect().width() - first_position.width()).abs() < 0.1
            );
            assert!((second.get_bounding_client_rect().x() - second_position.x()).abs() < 0.1);
            let close = mounted
                .element("[data-project-tab='1'] .tab-close")
                .get_bounding_client_rect();
            assert!(
                close.right() <= first.get_bounding_client_rect().right(),
                "close button overlaps next project"
            );
        }
    }
}

#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function pointerFocusTab(button) {
    const event = new MouseEvent('mousedown', {bubbles:true,cancelable:true,button:0});
    if (button.dispatchEvent(event)) button.focus();
    button.click();
}
"#)]
extern "C" {
    fn pointerFocusTab(button: &web_sys::HtmlElement);
}

#[wasm_bindgen_test]
async fn pointer_file_tab_selection_preserves_overflow_scroll_and_focus_in_both_modes() {
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.projects.projects.update(|items| items[0].mode = mode);
            for index in 0..10 {
                state
                    .workspace
                    .register_editor_tab(1, format!("long-file-name-{index}.rs"));
            }
            state
                .workspace
                .open_file
                .set(Some("long-file-name-0.rs".into()));
            view! { <style>{include_str!("../../styles.css")}</style>{super::support::editor_view(state)} }
        });
        settle().await;
        let editor = mounted.element(".editor");
        editor.style().set_property("width", "520px").unwrap();
        editor.style().set_property("flex", "none").unwrap();
        settle().await;
        let tab = mounted.element("[data-editor-tab='long-file-name-3.rs']");
        let strip = mounted.element(".editor-file-tabs");
        strip.set_scroll_left(f64::from(tab.offset_left()) + 20.0);
        let scroll = strip.scroll_left();
        pointerFocusTab(&tab);
        settle().await;
        assert_eq!(
            strip.scroll_left().to_bits(),
            scroll.to_bits(),
            "Pointer focus moved the tab strip"
        );
        assert!(
            tab.is_same_node(
                mounted
                    .root
                    .owner_document()
                    .unwrap()
                    .active_element()
                    .as_ref()
                    .map(AsRef::as_ref)
            )
        );
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("long-file-name-3.rs")
        );
    }
}

#[wasm_bindgen_test]
async fn loading_file_tabs_do_not_flash_encoding_warning_or_shift_in_both_modes() {
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.projects.projects.update(|items| items[0].mode = mode);
            state.workspace.register_editor_tab(1, "first.rs".into());
            state.workspace.register_editor_tab(1, "second.md".into());
            state.workspace.open_file.set(Some("first.rs".into()));
            state.workspace.content.set("fn main() {}".into());
            view! { <style>{include_str!("../../styles.css")}</style>{super::support::editor_view(state)} }
        });
        settle().await;
        let editor = mounted.element(".editor");
        editor.style().set_property("flex", "none").unwrap();
        for width in [380, 760, 1000] {
            editor
                .style()
                .set_property("width", &format!("{width}px"))
                .unwrap();
            settle().await;
            let strip = mounted.element(".editor-file-tabs");
            let before = strip.get_bounding_client_rect();
            let header = mounted.element(".editor-header").get_bounding_client_rect();
            mounted.state.workspace.editor_loading.set(true);
            settle().await;
            assert!(
                !mounted
                    .element(".editor-header")
                    .text_content()
                    .unwrap()
                    .contains("Not valid UTF-8"),
                "Loading a valid file must not show an encoding warning"
            );
            let after = strip.get_bounding_client_rect();
            assert!((before.x() - after.x()).abs() < 0.1);
            assert!((before.y() - after.y()).abs() < 0.1);
            assert!((before.width() - after.width()).abs() < 0.1);
            assert!(
                (header.height()
                    - mounted
                        .element(".editor-header")
                        .get_bounding_client_rect()
                        .height())
                .abs()
                    < 0.1
            );
            let textarea: web_sys::HtmlTextAreaElement =
                mounted.element(".editor-textarea").unchecked_into();
            assert!(textarea.read_only(), "Loading must still prevent edits");
            mounted.state.workspace.editor_loading.set(false);
            settle().await;
        }
    }
}

#[wasm_bindgen_test]
async fn workspace_scroll_area_ends_at_the_last_visible_panel_in_both_modes() {
    use openwebide_frontend::{
        components::{PanelRail, ToolPanel},
        state::layout::{LayoutState, Panel},
    };
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_session();
            state.projects.projects.update(|items| items[0].mode = mode);
            super::support::command_actions(state.clone());
            let layout = expect_context::<LayoutState>();
            layout.tree_width.set(380.0);
            layout.chat_width.set(520.0);
            layout.panels.update(|panels| panels.sessions = false);
            for index in 0..17 {
                state
                    .workspace
                    .register_editor_tab(1, format!("long-file-name-{index}.rs"));
            }
            state
                .workspace
                .open_file
                .set(Some("long-file-name-0.rs".into()));
            state.workspace.content.set("fn main() {}".into());
            let editor = super::support::editor_view(state.clone());
            let chat = super::support::chat_view(state);
            view! { <style>{include_str!("../../styles.css")}</style><div class="app" style="width:1200px;height:700px"><div class="app-body"><div class="workspace-docks"><PanelRail panels=vec![Panel::Sessions, Panel::Files, Panel::Editor, Panel::Chat] /><ToolPanel panel=Panel::Files><div class="file-tree">"Files"</div></ToolPanel><ToolPanel panel=Panel::Editor><div class="center-pane">{editor}</div></ToolPanel><ToolPanel panel=Panel::Chat>{chat}</ToolPanel></div></div></div> }
        });
        settle().await;
        let docks = mounted.element(".workspace-docks");
        let last = mounted.element("#panel-chat").get_bounding_client_rect();
        let expected = (last.right() - docks.get_bounding_client_rect().left()).ceil();
        assert!(
            f64::from(docks.scroll_width()) <= expected.max(f64::from(docks.client_width())) + 1.0,
            "Workspace has empty overflow: scroll width {}, last panel ends at {expected}",
            docks.scroll_width()
        );
    }
}

#[wasm_bindgen_test]
async fn settings_tabs_autosave_choices_and_support_keyboard_navigation_in_every_scope() {
    for mode in [
        Some(openwebide_core::WorkspaceMode::Local),
        Some(openwebide_core::WorkspaceMode::Remote),
        None,
    ] {
        let mounted = mount_test(move |state| {
            if let Some(mode) = mode {
                state.seed_project();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
            }
            state.seed_connection();
            *state.fake.models.borrow_mut() = vec![openwebide_core::ModelInfo {
                name: "Tab test model".into(),
            }];
            view! { <style>{include_str!("../../styles.css")}</style><Settings on_set_notifications=Callback::new(|_| ()) on_set_theme=Callback::new(move |theme| state.settings.theme.set(theme)) on_set_default_prompt=Callback::new(|_| ()) on_set_bridge_url=Callback::new(|_| ()) /> }
        });
        settle().await;
        assert_eq!(
            mounted.element("[role=tab][aria-selected=true]").id(),
            "settings-tab-general"
        );
        assert_eq!(
            mounted
                .root
                .query_selector_all("[role=tabpanel]:not([hidden])")
                .unwrap()
                .length(),
            1
        );
        let height = mounted.element(".ui-tabbed-modal").offset_height();
        key(
            &mounted.element("#settings-tab-general"),
            "ArrowRight",
            false,
            false,
        );
        settle().await;
        assert_eq!(active().id(), "settings-tab-editor");
        assert!(
            mounted
                .element("#settings-panel-general")
                .has_attribute("hidden")
        );
        key(
            &mounted.element("#settings-tab-editor"),
            "End",
            false,
            false,
        );
        settle().await;
        assert_eq!(active().id(), "settings-tab-updates");
        mounted.click("#settings-tab-bridge");
        settle().await;
        let input: web_sys::HtmlInputElement = mounted
            .element("#settings-panel-bridge input[type=text]")
            .unchecked_into();
        input.set_value("ws://unsaved-draft.test:3001");
        mounted.click("#settings-tab-models");
        settle().await;
        wait_until("model choices ready", || {
            !mounted
                .element("#settings-panel-models .ui-inline-actions .btn")
                .has_attribute("disabled")
        })
        .await;
        mounted.click("button[aria-label='Default model']");
        settle().await;
        mounted.click_text("Tab test model");
        settle().await;
        mounted.click("#settings-tab-general");
        settle().await;
        mounted.click_text("Light");
        settle().await;
        assert_eq!(
            mounted.state.settings.theme.get_untracked(),
            openwebide_frontend::state::settings::Theme::Light
        );
        mounted.click("#settings-tab-models");
        settle().await;
        assert!(
            mounted
                .element("button[aria-label='Default model']")
                .text_content()
                .unwrap()
                .contains("Tab test model")
        );
        assert_eq!(
            mounted
                .state
                .fake
                .model_setup
                .borrow()
                .defaults
                .primary
                .as_ref()
                .unwrap()
                .model,
            "Tab test model"
        );
        mounted.click("#settings-tab-bridge");
        settle().await;
        assert_eq!(input.value(), "ws://unsaved-draft.test:3001");
        assert_eq!(mounted.element(".ui-tabbed-modal").offset_height(), height);
        key(
            &mounted.element("#settings-tab-bridge"),
            "ArrowRight",
            false,
            false,
        );
        settle().await;
        assert_eq!(active().id(), "settings-tab-host");
        key(
            &mounted.element("#settings-tab-host"),
            "ArrowRight",
            false,
            false,
        );
        settle().await;
        assert_eq!(active().id(), "settings-tab-plugins");
        key(
            &mounted.element("#settings-tab-plugins"),
            "ArrowRight",
            false,
            false,
        );
        settle().await;
        assert_eq!(active().id(), "settings-tab-updates");
        key(
            &mounted.element("#settings-tab-updates"),
            "ArrowRight",
            false,
            false,
        );
        settle().await;
        assert_eq!(active().id(), "settings-tab-general");
        key(
            &mounted.element("#settings-tab-general"),
            "ArrowLeft",
            false,
            false,
        );
        settle().await;
        assert_eq!(active().id(), "settings-tab-updates");
        key(
            &mounted.element("#settings-tab-updates"),
            "Home",
            false,
            false,
        );
        settle().await;
        assert_eq!(active().id(), "settings-tab-general");
    }
}

#[wasm_bindgen_test]
async fn shortcuts_tabs_show_only_the_selected_category_and_restore_focus() {
    let mounted = mount_test(|state| {
        view! {
            <style>{include_str!("../../styles.css")}</style>
            <button id="open-shortcuts" on:click=move |_| state.ui.shortcuts_open.set(true)>"Open shortcuts"</button>
            <openwebide_frontend::components::CommandDialogs />
        }
    });
    mounted.element("#open-shortcuts").focus().unwrap();
    mounted.click("#open-shortcuts");
    settle().await;
    let height = mounted.element(".ui-tabbed-modal").offset_height();
    for (tab, panel, text) in [
        ("chat", "chat", "Accept the inline hint"),
        ("search", "search", "Open the selected result"),
        ("workspace", "workspace", "Toggle terminal"),
    ] {
        mounted.click(&format!("#shortcuts-tab-{tab}"));
        settle().await;
        assert_eq!(
            mounted
                .root
                .query_selector_all("[role=tabpanel]:not([hidden])")
                .unwrap()
                .length(),
            1
        );
        assert!(
            mounted
                .element(&format!("#shortcuts-panel-{panel}"))
                .text_content()
                .unwrap()
                .contains(text)
        );
        assert_eq!(mounted.element(".ui-tabbed-modal").offset_height(), height);
    }
    key(
        &mounted.element("#shortcuts-tab-workspace"),
        "ArrowLeft",
        false,
        false,
    );
    settle().await;
    assert_eq!(active().id(), "shortcuts-tab-search");
    key(
        &mounted.element("#shortcuts-tab-search"),
        "Escape",
        false,
        false,
    );
    settle().await;
    assert!(!mounted.state.ui.shortcuts_open.get_untracked());
    assert_eq!(active().id(), "open-shortcuts");
}
