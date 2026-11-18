use super::support::{mount_test, settle};
use leptos::prelude::*;
use openwebide_core::{User, UserId, UserRole, WorkspaceMode};
use openwebide_frontend::{
    components::{PanelRail, ToolPanel},
    state::{
        auth::AuthState,
        layout::{LayoutState, PANEL_VISIBILITY_KEY, Panel, PanelVisibility},
    },
    state_actions::{
        layout::LayoutActions,
        lifecycle::{ProjectEffectContext, install_project_effects},
    },
};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;

fn user(id: i64) -> User {
    User {
        id: UserId::new(id),
        username: format!("user-{id}"),
        role: UserRole::User,
        created_at: 0,
    }
}

#[wasm_bindgen_test]
async fn panels_collapse_without_unmounting_and_persist_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            let auth = expect_context::<AuthState>();
            auth.set_user(user(1));
            let layout = expect_context::<LayoutState>();
            provide_context(LayoutActions::new(state.api, layout, auth, state.ui));
            Effect::new(move |_| {
                layout.preferences.update(|prefs| {
                    prefs.mode = openwebide_frontend::state::responsive::LayoutMode::Desktop;
                });
            });
            view! {
                <PanelRail panels=vec![Panel::Sessions, Panel::Files, Panel::History, Panel::Editor, Panel::Chat] />
                <ToolPanel panel=Panel::Sessions><input value="server draft" /></ToolPanel>
                <ToolPanel panel=Panel::Files><input value="search draft" /></ToolPanel>
                <ToolPanel panel=Panel::History><input value="history query" /></ToolPanel>
                <ToolPanel panel=Panel::Editor><textarea>"unsaved editor"</textarea></ToolPanel>
                <ToolPanel panel=Panel::Chat><textarea>"unsent prompt"</textarea></ToolPanel>
            }
        });
        settle().await;
        mounted.click("button[aria-controls='panel-history']");
        settle().await;
        for panel in [
            Panel::Sessions,
            Panel::Files,
            Panel::History,
            Panel::Editor,
            Panel::Chat,
        ] {
            let selector = format!("#panel-{}", panel.id());
            let element = mounted.element(&selector);
            let child = element.first_element_child().unwrap();
            let button = format!("button[aria-controls='panel-{}']", panel.id());
            mounted.click(&button);
            settle().await;
            assert_eq!(
                mounted
                    .element(&button)
                    .get_attribute("aria-expanded")
                    .as_deref(),
                Some("false")
            );
            assert!(element.get_attribute("style").unwrap().contains("none"));
            assert!(child.is_same_node(element.first_element_child().as_ref().map(AsRef::as_ref)));
            mounted.click(&button);
            settle().await;
            assert!(child.is_same_node(element.first_element_child().as_ref().map(AsRef::as_ref)));
            assert_eq!(
                mounted
                    .element(&button)
                    .get_attribute("aria-expanded")
                    .as_deref(),
                Some("true")
            );
        }
        mounted.click("button[aria-controls='panel-editor']");
        mounted.click("button[aria-controls='panel-chat']");
        settle().await;
        let saved: PanelVisibility =
            serde_json::from_str(&mounted.state.fake.settings.borrow()[PANEL_VISIBILITY_KEY])
                .unwrap();
        assert!(!saved.editor && !saved.chat && saved.sessions && saved.files && saved.history);
        let draft: web_sys::HtmlTextAreaElement =
            mounted.element("#panel-chat textarea").unchecked_into();
        assert_eq!(draft.value(), "unsent prompt");
    }
}

#[wasm_bindgen_test]
async fn saved_panel_layout_restores_and_account_changes_clear_it() {
    let slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let auth_slot = slot.clone();
    let mounted = mount_test(move |state| {
        let auth = expect_context::<AuthState>();
        auth_slot.set(Some(auth));
        auth.set_user(user(1));
        let layout = expect_context::<LayoutState>();
        state.fake.settings.borrow_mut().insert(
            PANEL_VISIBILITY_KEY.into(),
            r#"{"sessions":false,"files":true,"editor":false,"chat":true}"#.into(),
        );
        provide_context(LayoutActions::new(state.api, layout, auth, state.ui));
        install_project_effects(ProjectEffectContext {
            api: state.api,
            health: RwSignal::new(None),
            auth,
            settings: state.settings,
            projects: state.projects,
            chat: state.chat,
            layout,
            select_project: Callback::new(|_| ()),
        });
        view! { <PanelRail panels=vec![Panel::Sessions, Panel::Files, Panel::Editor, Panel::Chat] /> }
    });
    for _ in 0..100 {
        openwebide_frontend::util::sleep_ms(10).await;
        if mounted
            .element("button[aria-controls='panel-sessions']")
            .get_attribute("aria-expanded")
            .as_deref()
            == Some("false")
        {
            break;
        }
    }
    assert_eq!(
        mounted
            .element("button[aria-controls='panel-sessions']")
            .get_attribute("aria-expanded")
            .as_deref(),
        Some("false")
    );
    assert_eq!(
        mounted
            .element("button[aria-controls='panel-editor']")
            .get_attribute("aria-expanded")
            .as_deref(),
        Some("false")
    );
    slot.get().unwrap().logout();
    settle().await;
    for panel in ["sessions", "files", "editor", "chat"] {
        assert_eq!(
            mounted
                .element(&format!("button[aria-controls='panel-{panel}']"))
                .get_attribute("aria-expanded")
                .as_deref(),
            Some(if matches!(panel, "files" | "editor") {
                "false"
            } else {
                "true"
            })
        );
    }
}

#[wasm_bindgen_test]
async fn scroll_regions_follow_dark_and_light_theme_tokens() {
    let document = web_sys::window().unwrap().document().unwrap();
    let root = document.document_element().unwrap();
    let original = root.get_attribute("data-theme");
    let mounted = mount_test(
        |_| view! { <style>{include_str!("../../styles.css")}</style><div class="scroll-check" style="overflow:auto;height:40px"><div style="height:200px">"scroll"</div></div> },
    );
    let mut colors = Vec::new();
    for theme in ["dark", "light"] {
        root.set_attribute("data-theme", theme).unwrap();
        let style = web_sys::window()
            .unwrap()
            .get_computed_style(&mounted.element(".scroll-check"))
            .unwrap()
            .unwrap();
        let colors_for_theme = style.get_property_value("scrollbar-color").unwrap();
        assert!(!colors_for_theme.is_empty() && colors_for_theme != "auto");
        colors.push(colors_for_theme);
    }
    assert_ne!(colors[0], colors[1]);
    if let Some(original) = original {
        root.set_attribute("data-theme", &original).unwrap();
    } else {
        root.remove_attribute("data-theme").unwrap();
    }
}

#[wasm_bindgen_test]
async fn rapid_panel_toggles_save_in_order_and_late_loading_preserves_changes() {
    let slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let state_slot = slot.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        let auth = expect_context::<AuthState>();
        auth.set_user(user(1));
        let layout = expect_context::<LayoutState>();
        state_slot.set(Some(layout));
        provide_context(LayoutActions::new(state.api, layout, auth, state.ui));
        view! { <PanelRail panels=vec![Panel::Sessions, Panel::Files, Panel::Editor, Panel::Chat] /> }
    });
    settle().await;
    let (release, pending) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .panel_save_results
        .borrow_mut()
        .push_back(pending);
    mounted.click("button[aria-controls='panel-chat']");
    settle().await;
    mounted.click("button[aria-controls='panel-editor']");
    mounted.click("button[aria-controls='panel-chat']");
    settle().await;
    // Settings arriving after the user's first interaction must not undo it.
    slot.get().unwrap().restore_panels(Some(
        &serde_json::to_string(&PanelVisibility::default()).unwrap(),
    ));
    assert_eq!(
        mounted
            .element("button[aria-controls='panel-editor']")
            .get_attribute("aria-expanded")
            .as_deref(),
        Some("false")
    );
    release.send(Ok(())).unwrap();
    settle().await;
    let saved: PanelVisibility =
        serde_json::from_str(&mounted.state.fake.settings.borrow()[PANEL_VISIBILITY_KEY]).unwrap();
    assert!(!saved.editor && saved.chat);
    let writes = mounted.state.fake.calls.borrow().iter().filter(|call| matches!(call, openwebide_frontend::testing::fake_backend::Call::SetSetting { key, .. } if key == PANEL_VISIBILITY_KEY)).count();
    assert_eq!(writes, 2);
}

#[wasm_bindgen_test]
async fn projectless_chat_hides_and_disables_files_and_editor_without_saving_over_preferences() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let actions_slot = RwSignal::new(None);
        let layout_slot = RwSignal::new(None);
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            let auth = expect_context::<AuthState>();
            auth.set_user(user(1));
            let layout = expect_context::<LayoutState>();
            let actions = LayoutActions::new(state.api, layout, auth, state.ui);
            actions_slot.set(Some(actions));
            layout_slot.set(Some(layout));
            provide_context(actions);
            view! {
                <PanelRail panels=vec![Panel::Sessions, Panel::Files, Panel::Editor, Panel::Chat] />
                <ToolPanel panel=Panel::Files><input value="file draft" /></ToolPanel>
                <ToolPanel panel=Panel::Editor><textarea>"editor draft"</textarea></ToolPanel>
                <ToolPanel panel=Panel::Chat><textarea>"chat draft"</textarea></ToolPanel>
            }
        });
        settle().await;
        let layout = layout_slot.get_untracked().unwrap();
        actions_slot
            .get_untracked()
            .unwrap()
            .toggle
            .run(Panel::Files);
        settle().await;
        let saved = mounted.state.fake.settings.borrow()[PANEL_VISIBILITY_KEY].clone();
        let preferences = layout.panels.get_untracked();
        assert!(!preferences.files && preferences.editor);
        let editor = mounted.element("#panel-editor textarea");
        mounted.state.projects.active_project.set(None);
        settle().await;
        for panel in [Panel::Files, Panel::Editor] {
            let button = mounted.element(&format!("button[aria-controls='panel-{}']", panel.id()));
            assert!(button.has_attribute("disabled"));
            assert_eq!(
                button.get_attribute("aria-expanded").as_deref(),
                Some("false")
            );
            assert!(
                mounted
                    .element(&format!("#panel-{}", panel.id()))
                    .get_attribute("style")
                    .unwrap()
                    .contains("none")
            );
            actions_slot.get_untracked().unwrap().toggle.run(panel);
        }
        settle().await;
        assert_eq!(layout.panels.get_untracked(), preferences);
        assert_eq!(
            mounted.state.fake.settings.borrow()[PANEL_VISIBILITY_KEY],
            saved
        );
        mounted.state.projects.active_project.set(Some(1));
        settle().await;
        assert_eq!(layout.visible_panels.get_untracked(), preferences);
        assert!(
            !mounted
                .element("button[aria-controls='panel-editor']")
                .has_attribute("disabled")
        );
        assert!(
            mounted
                .element("#panel-editor")
                .get_attribute("style")
                .unwrap()
                .contains("flex")
        );
        assert!(editor.is_same_node(Some(mounted.element("#panel-editor textarea").as_ref())));
    }
}

#[wasm_bindgen_test]
async fn phone_sheets_keep_drafts_and_desktop_preferences_in_both_modes() {
    use super::support::wait_until;
    use openwebide_frontend::state::responsive::{LAYOUT_PREFERENCES_KEY, LayoutMode, PanelSide};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let read = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            let auth = expect_context::<AuthState>();
            auth.set_user(user(1));
            let layout = expect_context::<LayoutState>();
            let actions = LayoutActions::new(state.api, layout, auth, state.ui);
            provide_context(actions);
            read.set(Some((layout, actions)));
            view! {
                <PanelRail panels=vec![Panel::Files, Panel::History, Panel::Chat, Panel::Terminal, Panel::Git, Panel::Search] />
                <ToolPanel panel=Panel::Files><input value="file draft" /></ToolPanel>
                <ToolPanel panel=Panel::History><input value="history query" /></ToolPanel>
                <ToolPanel panel=Panel::Chat><textarea>"chat draft"</textarea></ToolPanel>
                <ToolPanel panel=Panel::Search><input value="query" /></ToolPanel>
                <ToolPanel panel=Panel::Git><span>"Git"</span></ToolPanel>
                <ToolPanel panel=Panel::Terminal><input value="command" /></ToolPanel>
            }
        });
        settle().await;
        let (layout, actions) = slot.get().unwrap();
        let remembered = layout.panels.get_untracked();
        let draft = mounted.element("#panel-chat textarea");
        layout.fit(390.0);
        settle().await;
        assert!(layout.phone.get_untracked());
        assert!(layout.visible_panels.get_untracked().chat);
        for panel in [Panel::Files, Panel::History, Panel::Search, Panel::Git] {
            actions.show.run(panel);
            settle().await;
            assert!(layout.visible_panels.get_untracked().visible(
                if matches!(panel, Panel::Git | Panel::Search) {
                    Panel::Files
                } else {
                    panel
                }
            ));
            assert!(!layout.visible_panels.get_untracked().chat);
            mounted.click(&format!(
                "#panel-{} .tool-panel-heading > button[title='Return to chat']",
                if matches!(panel, Panel::Git | Panel::Search) {
                    Panel::Files
                } else {
                    panel
                }
                .id()
            ));
            settle().await;
            assert!(layout.visible_panels.get_untracked().chat);
        }
        assert_eq!(layout.panels.get_untracked(), remembered);
        assert!(draft.is_same_node(Some(mounted.element("#panel-chat textarea").as_ref())));
        actions.set_mode.run(LayoutMode::Desktop);
        settle().await;
        assert!(!layout.phone.get_untracked());
        assert_eq!(layout.panels.get_untracked(), remembered);
        actions.pin.run((Panel::Chat, PanelSide::Left));
        wait_until("saved workspace layout", || {
            mounted.state.fake.settings.borrow().get(LAYOUT_PREFERENCES_KEY)
                .and_then(|value| serde_json::from_str::<openwebide_frontend::state::responsive::LayoutPreferences>(value).ok())
                .is_some_and(|prefs| prefs.mode == LayoutMode::Desktop && prefs.side("chat") == PanelSide::Left)
        }).await;
        let saved: openwebide_frontend::state::responsive::LayoutPreferences =
            serde_json::from_str(&mounted.state.fake.settings.borrow()[LAYOUT_PREFERENCES_KEY])
                .unwrap();
        assert_eq!(saved.mode, LayoutMode::Desktop);
        assert_eq!(saved.side("chat"), PanelSide::Left);
        layout.active_project.set(None);
        actions.set_mode.run(LayoutMode::Phone);
        settle().await;
        for panel in [
            Panel::Files,
            Panel::History,
            Panel::Terminal,
            Panel::Git,
            Panel::Search,
        ] {
            assert!(!layout.available(panel));
        }
        assert!(layout.visible_panels.get_untracked().chat);
    }
}

#[wasm_bindgen_test]
async fn git_window_loads_diffs_only_when_open_in_a_repository_in_both_modes() {
    use openwebide_frontend::components::GitPane;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let loads = RwSignal::new(0);
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let read = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.workspace.open_file.set(Some("demo.rs".into()));
            let layout = expect_context::<LayoutState>();
            provide_context(LayoutActions::new(
                state.api,
                layout,
                expect_context::<AuthState>(),
                state.ui,
            ));
            read.set(Some(layout));
            view! { <GitPane on_open=Callback::new(|_| ()) on_load_git_diff=Callback::new(move |()| loads.update(|value| *value += 1)) on_discard_git_diff=Callback::new(|()| ()) /> }
        });
        settle().await;
        assert_eq!(loads.get_untracked(), 0);
        let layout = slot.get().unwrap();
        layout.preferences.update(|prefs| {
            prefs.files_view = openwebide_frontend::state::responsive::FilesView::Changes;
        });
        layout.panels.update(|panels| panels.files = true);
        settle().await;
        assert_eq!(loads.get_untracked(), 0);
        mounted
            .state
            .git
            .status
            .set(Some(openwebide_core::GitRepoStatus::default()));
        settle().await;
        assert_eq!(loads.get_untracked(), 1);
        layout.panels.update(|panels| panels.files = false);
        settle().await;
        mounted
            .state
            .workspace
            .open_file
            .set(Some("other.rs".into()));
        settle().await;
        assert_eq!(loads.get_untracked(), 1);
        layout.preferences.update(|prefs| {
            prefs.files_view = openwebide_frontend::state::responsive::FilesView::Changes;
        });
        layout.panels.update(|panels| panels.files = true);
        settle().await;
        assert_eq!(loads.get_untracked(), 2);
        mounted.state.projects.active_project.set(None);
        settle().await;
        assert_eq!(loads.get_untracked(), 2);
    }
}

#[wasm_bindgen_test]
async fn shared_panel_resizers_use_the_right_edge_and_save_widths_in_both_modes() {
    use openwebide_frontend::state::{
        layout::ActiveResizer,
        responsive::{LayoutMode, PanelSide},
    };
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let read = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            let auth = expect_context::<AuthState>();
            auth.set_user(user(1));
            let layout = expect_context::<LayoutState>();
            let actions = LayoutActions::new(state.api, layout, auth, state.ui);
            provide_context(actions);
            read.set(Some((layout, actions)));
            view! {
                <ToolPanel panel=Panel::Sessions><span>"Sessions"</span></ToolPanel>
                <ToolPanel panel=Panel::Files><span>"Files"</span></ToolPanel>
                <ToolPanel panel=Panel::History><span>"History"</span></ToolPanel>
                <ToolPanel panel=Panel::Editor><span>"Editor"</span></ToolPanel>
                <ToolPanel panel=Panel::Chat><span>"Chat"</span></ToolPanel>
                <ToolPanel panel=Panel::Terminal><span>"Terminal"</span></ToolPanel>
            }
        });
        settle().await;
        let (layout, actions) = slot.get().unwrap();
        actions.set_mode.run(LayoutMode::Desktop);
        layout.fit(2400.0);
        actions.show.run(Panel::Terminal);
        settle().await;
        for (panel, kind) in [
            (Panel::Sessions, ActiveResizer::Sidebar),
            (Panel::Files, ActiveResizer::Tree),
            (Panel::History, ActiveResizer::History),
            (Panel::Chat, ActiveResizer::Chat),
        ] {
            layout.panels.set(PanelVisibility {
                sessions: panel == Panel::Sessions,
                files: panel == Panel::Files,
                history: panel == Panel::History,
                chat: panel == Panel::Chat,
                editor: true,
                terminal: false,
                ..PanelVisibility::default()
            });
            for side in [PanelSide::Left, PanelSide::Right] {
                actions.pin.run((panel, side));
                settle().await;
                layout.fit(2400.0);
                let leading = layout.preferences.with_untracked(|prefs| {
                    prefs.order(panel.id()) > prefs.order(Panel::Editor.id())
                });
                if panel == Panel::History && leading {
                    assert!(
                        mounted
                            .element("#panel-history")
                            .class_list()
                            .contains("tool-panel-center")
                    );
                    assert!(
                        mounted
                            .root
                            .query_selector("#panel-editor [role=separator]")
                            .unwrap()
                            .is_none()
                    );
                    continue;
                }
                let owner = if leading { Panel::Editor } else { panel };
                let separator = mounted.element(&format!("#panel-{} [role=separator]", owner.id()));
                let before = layout.width(kind).get_untracked();
                let init = web_sys::KeyboardEventInit::new();
                init.set_key("ArrowRight");
                init.set_cancelable(true);
                separator
                    .dispatch_event(
                        &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict(
                            "keydown", &init,
                        )
                        .unwrap(),
                    )
                    .unwrap();
                settle().await;
                let expected = before + if leading { -20.0 } else { 20.0 };
                assert!((layout.width(kind).get_untracked() - expected).abs() < f64::EPSILON);
                assert_eq!(
                    mounted.state.fake.settings.borrow()[kind.setting_key()],
                    expected.to_string()
                );
            }
        }
        layout.panels.set(PanelVisibility {
            terminal: true,
            ..PanelVisibility::default()
        });
        settle().await;
        assert_eq!(
            mounted
                .root
                .query_selector_all(".panel-resizer")
                .unwrap()
                .length(),
            4
        );
        actions.move_panel.run((Panel::Files, true));
        settle().await;
        assert!(
            mounted
                .state
                .fake
                .settings
                .borrow()
                .contains_key("workspace_layout")
        );
        assert!(mounted.root.query_selector(".panel-pin").unwrap().is_none());
    }
}

#[wasm_bindgen_test]
#[allow(
    clippy::cast_possible_truncation,
    reason = "Browser hit testing uses f32 coordinates and pointer events use integer CSS pixels."
)]
async fn dock_resize_targets_are_visible_and_resize_actual_geometry_in_both_modes() {
    use openwebide_frontend::state::{
        layout::ActiveResizer,
        responsive::{LayoutMode, PanelSide},
    };
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let read = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.auth.set_user(user(1));
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            let layout = expect_context::<LayoutState>();
            let actions = LayoutActions::new(state.api, layout, state.auth, state.ui);
            provide_context(actions);
            read.set(Some((layout, actions)));
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <div class="app" style="height:400px;width:1000px"><div class="app-body">
                    <ToolPanel panel=Panel::Editor><span>"Editor"</span></ToolPanel>
                    <ToolPanel panel=Panel::Sessions><div class="sidebar" style="width:240px;flex:none">"Sessions"</div></ToolPanel>
                    <ToolPanel panel=Panel::Files><div class="search-pane">"Files"</div></ToolPanel>
                    <ToolPanel panel=Panel::Chat><div class="chat-pane" style="width:420px;flex:none">"Chat"</div></ToolPanel>

                </div></div>
            }
        });
        settle().await;
        mounted
            .root
            .set_attribute("style", "position:fixed;left:0;top:0;right:0;z-index:100")
            .unwrap();
        let (layout, actions) = slot.get().unwrap();
        actions.set_mode.run(LayoutMode::Desktop);
        for (panel, kind) in [
            (Panel::Sessions, ActiveResizer::Sidebar),
            (Panel::Files, ActiveResizer::Tree),
            (Panel::Chat, ActiveResizer::Chat),
        ] {
            layout.panels.set(PanelVisibility {
                sessions: panel == Panel::Sessions,
                files: panel == Panel::Files,
                chat: panel == Panel::Chat,
                terminal: panel == Panel::Terminal,
                editor: true,
                ..PanelVisibility::default()
            });
            for side in [PanelSide::Left, PanelSide::Right] {
                actions.pin.run((panel, side));
                settle().await;
                let dock = mounted.element(&format!("#panel-{}", panel.id()));
                let leading = layout.preferences.with_untracked(|prefs| {
                    prefs.order(panel.id()) > prefs.order(Panel::Editor.id())
                });
                let owner = if leading { Panel::Editor } else { panel };
                let separator =
                    mounted.element(&format!("#panel-{} > [role=separator]", owner.id()));
                let rect = separator.get_bounding_client_rect();
                assert!(rect.height() > 300.0);
                assert!(
                    (rect.right()
                        - if leading {
                            dock.get_bounding_client_rect().left()
                        } else {
                            dock.get_bounding_client_rect().right()
                        })
                    .abs()
                        < 1.0
                );
                let x = rect.x() + rect.width() / 2.0;
                let y = rect.y() + rect.height() / 2.0;
                let hit = web_sys::window()
                    .unwrap()
                    .document()
                    .unwrap()
                    .element_from_point(x as f32, y as f32)
                    .unwrap();
                assert!(
                    hit.is_same_node(Some(separator.as_ref())),
                    "{} {side:?} resize handle must receive pointer input",
                    panel.id()
                );
                let before = dock.get_bounding_client_rect().width();
                let event = |name, x: f64| {
                    let init = web_sys::PointerEventInit::new();
                    init.set_client_x(x as i32);
                    init.set_bubbles(true);
                    web_sys::PointerEvent::new_with_event_init_dict(name, &init).unwrap()
                };
                hit.dispatch_event(&event("pointerdown", x)).unwrap();
                settle().await;
                assert_eq!(
                    mounted
                        .root
                        .query_selector_all(".panel-resizer.is-active")
                        .unwrap()
                        .length(),
                    1
                );
                web_sys::window()
                    .unwrap()
                    .dispatch_event(&event(
                        "pointermove",
                        x + if leading { -30.0 } else { 30.0 },
                    ))
                    .unwrap();
                web_sys::window()
                    .unwrap()
                    .dispatch_event(&event("pointerup", x))
                    .unwrap();
                settle().await;
                assert!(
                    (dock.get_bounding_client_rect().width() - before - 30.0).abs() < 1.0,
                    "{} geometry follows drag",
                    panel.id()
                );
                assert_eq!(
                    mounted.state.fake.settings.borrow()[kind.setting_key()],
                    layout.width(kind).get_untracked().to_string()
                );
            }
        }
    }
}

#[wasm_bindgen_test]
async fn changes_and_explorer_share_file_layout_and_single_view_headings_in_both_modes() {
    use openwebide_core::{FileEntry, GitFileStatus, GitRepoStatus};
    use openwebide_frontend::components::{FileTree, GitPane};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.workspace.entries.update(|entries| {
                entries.insert(
                    String::new(),
                    vec![FileEntry {
                        name: "demo.rs".into(),
                        path: "demo.rs".into(),
                        is_dir: false,
                        size: 20,
                    }],
                );
            });
            state.workspace.open_file.set(Some("demo.rs".into()));
            state.git.status.set(Some(GitRepoStatus {
                branch: "main".into(),
                files: [("demo.rs".into(), GitFileStatus::Modified)].into(),
                ..GitRepoStatus::default()
            }));
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <div style="display:flex;width:700px;height:500px">
                    <FileTree on_toggle=Callback::new(|_| ()) on_open=Callback::new(|_| ()) />
                    <GitPane on_open=Callback::new(move |path| state.workspace.open_file.set(Some(path))) on_load_git_diff=Callback::new(|()| ()) on_discard_git_diff=Callback::new(|()| ()) />
                </div>
            }
        });
        settle().await;
        let style = |selector| {
            web_sys::window()
                .unwrap()
                .get_computed_style(&mounted.element(selector))
                .unwrap()
                .unwrap()
        };
        assert!(mounted.root.query_selector("h2").unwrap().is_none());
        for property in ["padding-left", "padding-right", "padding-top"] {
            assert_eq!(
                style(".file-tree").get_property_value(property).unwrap(),
                style(".git-pane").get_property_value(property).unwrap()
            );
        }
        for property in [
            "padding-left",
            "padding-top",
            "font-size",
            "gap",
            "background-color",
        ] {
            assert_eq!(
                style(".file-tree .tree-item")
                    .get_property_value(property)
                    .unwrap(),
                style(".git-files .tree-item")
                    .get_property_value(property)
                    .unwrap()
            );
        }
        super::support::assert_start_aligned(&mounted.element(".git-files .tree-item"));
        assert!(
            mounted
                .element(".git-files .tree-item")
                .class_list()
                .contains("selected")
        );
        assert!(
            mounted
                .element(".git-files .tree-item .tree-icon")
                .query_selector("svg")
                .unwrap()
                .is_some()
        );
        mounted.state.workspace.open_file.set(None);
        settle().await;
        assert!(
            !mounted
                .element(".git-files .tree-item")
                .class_list()
                .contains("selected")
        );
        mounted.click(".git-files .tree-item");
        settle().await;
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("demo.rs")
        );
    }
}

#[wasm_bindgen_test]
async fn overflow_actions_share_rows_with_labels_and_files_view_switcher_in_both_modes() {
    use openwebide_frontend::components::{FileTree, FilesPanel, SessionList};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let created = RwSignal::new(0);
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.seed_session();
            state.chat.sessions.update(|sessions| {
                sessions[0].name =
                    "A very long session title that must leave room for its actions menu".into();
            });
            let auth = expect_context::<AuthState>();
            auth.set_user(user(1));
            let layout = expect_context::<LayoutState>();
            provide_context(LayoutActions::new(state.api, layout, auth, state.ui));
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <div class="inline-menu-fixture">
                    <SessionList on_select=Callback::new(|_: i64| ()) on_new=Callback::new(|()| ()) on_rename=Callback::new(|_: i64| ()) on_delete=Callback::new(|_: i64| ()) />
                    <FilesPanel on_new_file=Callback::new(move |()| created.update(|count| *count += 1)) on_new_dir=Callback::new(move |()| created.update(|count| *count += 10))>
                        <FileTree on_toggle=Callback::new(|_: String| ()) on_open=Callback::new(|_: String| ()) />
                    </FilesPanel>
                </div>
            }
        });
        settle().await;
        for width in [220, 320, 390] {
            let fixture = mounted.element(".inline-menu-fixture");
            fixture
                .style()
                .set_property("width", &format!("{width}px"))
                .unwrap();
            fixture
                .class_list()
                .toggle_with_force("phone-layout", width == 390)
                .unwrap();
            mounted
                .element(".files-panel")
                .style()
                .set_property("width", "100%")
                .unwrap();
            settle().await;
            for (label, trigger) in [
                (".session-name", ".session .ui-dropdown-trigger"),
                (
                    ".files-panel-toolbar .ui-segmented-control",
                    ".files-panel-toolbar .ui-dropdown-trigger",
                ),
            ] {
                let label = mounted.element(label).get_bounding_client_rect();
                let trigger = mounted.element(trigger).get_bounding_client_rect();
                assert!(
                    (label.top() + label.height() / 2.0 - trigger.top() - trigger.height() / 2.0)
                        .abs()
                        < 1.0,
                    "Overflow menu wrapped at {width}px in {mode:?}"
                );
                assert!(trigger.left() >= label.right() - 1.0);
                assert!(trigger.right() <= fixture.get_bounding_client_rect().right());
            }
            let title = mounted.element(".session-label");
            assert!(title.scroll_width() > title.client_width());
            assert!(
                mounted
                    .root
                    .query_selector(".file-tree .ui-action-menu")
                    .unwrap()
                    .is_none()
            );
        }
        super::support::click_action(&mounted, ".files-panel-toolbar button[title='New file']")
            .await;
        super::support::click_action(&mounted, ".files-panel-toolbar button[title='New folder']")
            .await;
        assert_eq!(created.get_untracked(), 11);
        mounted.click(".files-panel-toolbar .ui-seg-btn:nth-child(2)");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".files-panel-toolbar .ui-action-menu")
                .unwrap()
                .is_some()
        );
        mounted.click(".files-panel-toolbar .ui-seg-btn:first-child");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".files-panel-toolbar .ui-action-menu")
                .unwrap()
                .is_some()
        );
    }
}

#[wasm_bindgen_test]
async fn editor_and_expanded_chat_resize_their_visible_neighbor_in_both_modes() {
    use openwebide_frontend::state::{layout::ActiveResizer, responsive::LayoutMode};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let read = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.auth.set_user(user(1));
            let layout = expect_context::<LayoutState>();
            let actions = LayoutActions::new(state.api, layout, state.auth, state.ui);
            provide_context(actions);
            read.set(Some((layout, actions)));
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <div class="app" style="height:400px;width:1400px"><div class="app-body" class:editor-collapsed=move || !layout.visible_panels.get().editor>
                    <ToolPanel panel=Panel::Files><span>"Files"</span></ToolPanel>
                    <ToolPanel panel=Panel::Editor><span>"Editor"</span></ToolPanel>
                    <ToolPanel panel=Panel::Chat><span>"Chat"</span></ToolPanel>
                </div></div>
            }
        });
        settle().await;
        let (layout, actions) = slot.get().unwrap();
        actions.set_mode.run(LayoutMode::Desktop);
        for (editor, terminal, panel, kind) in [
            (true, true, Panel::Editor, ActiveResizer::Chat),
            (true, false, Panel::Editor, ActiveResizer::Chat),
            (false, true, Panel::Chat, ActiveResizer::Tree),
            (false, false, Panel::Chat, ActiveResizer::Tree),
        ] {
            layout.panels.set(PanelVisibility {
                sessions: false,
                files: true,
                editor,
                terminal,
                chat: true,
                ..PanelVisibility::default()
            });
            layout.fit(1400.0);
            settle().await;
            let dock = mounted.element(&format!("#panel-{}", panel.id()));
            let owner = if editor { Panel::Editor } else { Panel::Files };
            let separator = mounted.element(&format!("#panel-{} > [role=separator]", owner.id()));
            let before = dock.get_bounding_client_rect().width();
            let neighbor_before = layout.width(kind).get_untracked();

            let event = |name, x| {
                let init = web_sys::PointerEventInit::new();
                init.set_client_x(x);
                init.set_bubbles(true);
                web_sys::PointerEvent::new_with_event_init_dict(name, &init).unwrap()
            };
            separator
                .dispatch_event(&event("pointerdown", 500))
                .unwrap();
            web_sys::window()
                .unwrap()
                .dispatch_event(&event("pointermove", if editor { 530 } else { 470 }))
                .unwrap();
            web_sys::window()
                .unwrap()
                .dispatch_event(&event("pointerup", if editor { 530 } else { 470 }))
                .unwrap();
            settle().await;
            assert!((dock.get_bounding_client_rect().width() - before - 30.0).abs() < 1.0);
            assert!(
                (layout.width(kind).get_untracked() - neighbor_before + 30.0).abs() < f64::EPSILON
            );
            assert_eq!(
                mounted.state.fake.settings.borrow()[kind.setting_key()],
                (neighbor_before - 30.0).to_string()
            );
        }
        layout.panels.set(PanelVisibility {
            sessions: false,
            files: true,
            editor: true,
            chat: true,
            terminal: false,
            ..PanelVisibility::default()
        });
        actions.pin.run((
            Panel::Files,
            openwebide_frontend::state::responsive::PanelSide::Right,
        ));
        actions.move_panel.run((Panel::Files, false));
        settle().await;
        let grip = mounted.element("#panel-files > [role=separator]");
        let files_before = layout.tree_width.get_untracked();
        let chat_before = layout.chat_width.get_untracked();
        let editor_before = mounted
            .element("#panel-editor")
            .get_bounding_client_rect()
            .width();
        let seam_before = grip.get_bounding_client_rect().right();
        let event = |name, x| {
            let init = web_sys::PointerEventInit::new();
            init.set_client_x(x);
            init.set_bubbles(true);
            web_sys::PointerEvent::new_with_event_init_dict(name, &init).unwrap()
        };
        grip.dispatch_event(&event("pointerdown", 500)).unwrap();
        settle().await;
        assert_eq!(
            mounted
                .root
                .query_selector_all(".panel-resizer.is-active")
                .unwrap()
                .length(),
            1
        );
        web_sys::window()
            .unwrap()
            .dispatch_event(&event("pointermove", 530))
            .unwrap();
        web_sys::window()
            .unwrap()
            .dispatch_event(&event("pointerup", 530))
            .unwrap();
        settle().await;
        assert!((layout.tree_width.get_untracked() - files_before - 30.0).abs() < f64::EPSILON);
        assert!((layout.chat_width.get_untracked() - chat_before + 30.0).abs() < f64::EPSILON);
        assert!(
            (mounted
                .element("#panel-editor")
                .get_bounding_client_rect()
                .width()
                - editor_before)
                .abs()
                < 1.0
        );
        assert!((grip.get_bounding_client_rect().right() - seam_before - 30.0).abs() < 1.0);
    }
}

#[wasm_bindgen_test]
async fn bottom_terminal_spans_workspace_resizes_height_and_requires_project_in_both_modes() {
    use openwebide_frontend::{components::StatusBar, state::responsive::LayoutMode};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let read = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.auth.set_user(user(1));
            let layout = expect_context::<LayoutState>();
            let actions = LayoutActions::new(state.api, layout, state.auth, state.ui);
            provide_context(actions);
            read.set(Some((layout, actions)));
            let health = RwSignal::new(None);
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <div class="app" style="height:700px;width:1000px"><div class="app-body">
                    <div class="workspace-docks">
                        <PanelRail panels=vec![Panel::Sessions, Panel::Files, Panel::Editor, Panel::Chat] />
                        <ToolPanel panel=Panel::Editor><span>"Editor"</span></ToolPanel>
                        <ToolPanel panel=Panel::Chat><span>"Chat"</span></ToolPanel>
                    </div>
                    <ToolPanel panel=Panel::Terminal><div class="terminal-visibility"><div class="terminal-dock">"Retained shell"</div></div></ToolPanel>
                </div><StatusBar health=health.read_only() on_toggle_terminal=move || actions.toggle.run(Panel::Terminal) /></div>
            }
        });
        let (layout, actions) = slot.get().unwrap();
        actions.set_mode.run(LayoutMode::Desktop);
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".panel-rail [aria-controls=panel-terminal]")
                .unwrap()
                .is_none()
        );
        mounted.click(".statusbar [aria-label='Toggle Output']");
        settle().await;
        let terminal = mounted.element("#panel-terminal");
        let row = mounted.element(".workspace-docks");
        let grip = mounted.element("#panel-terminal > [role=separator]");
        assert_eq!(
            grip.get_attribute("aria-orientation").as_deref(),
            Some("horizontal")
        );
        assert!((terminal.get_bounding_client_rect().width() - 1000.0).abs() < 1.0);
        assert!(
            (row.get_bounding_client_rect().bottom() - terminal.get_bounding_client_rect().top())
                .abs()
                < 1.0
        );
        assert!(
            (grip.get_bounding_client_rect().top() - terminal.get_bounding_client_rect().top())
                .abs()
                < 2.0
        );
        let before = terminal.get_bounding_client_rect().height();
        let event = |name, y| {
            let init = web_sys::PointerEventInit::new();
            init.set_client_y(y);
            init.set_bubbles(true);
            web_sys::PointerEvent::new_with_event_init_dict(name, &init).unwrap()
        };
        grip.dispatch_event(&event("pointerdown", 500)).unwrap();
        web_sys::window()
            .unwrap()
            .dispatch_event(&event("pointermove", 470))
            .unwrap();
        web_sys::window()
            .unwrap()
            .dispatch_event(&event("pointerup", 470))
            .unwrap();
        settle().await;
        assert!((terminal.get_bounding_client_rect().height() - before - 30.0).abs() < 1.0);
        assert_eq!(
            mounted.state.fake.settings.borrow()["panel_terminal_height"],
            (before + 30.0).to_string()
        );
        mounted.click(".statusbar [aria-label='Toggle Output']");
        mounted.click(".statusbar [aria-label='Toggle Output']");
        settle().await;
        assert!(terminal.is_same_node(Some(mounted.element("#panel-terminal").as_ref())));
        assert!((terminal.get_bounding_client_rect().height() - before - 30.0).abs() < 1.0);
        grip.dispatch_event(&event("pointerdown", 500)).unwrap();
        web_sys::window()
            .unwrap()
            .dispatch_event(&event("pointermove", -1000))
            .unwrap();
        web_sys::window()
            .unwrap()
            .dispatch_event(&event("pointerup", -1000))
            .unwrap();
        settle().await;
        assert!(
            (layout.terminal_height.get_untracked() - terminal.get_bounding_client_rect().height())
                .abs()
                < 1.0
        );
        assert!(row.get_bounding_client_rect().height() >= 180.0);
        actions.set_mode.run(LayoutMode::Phone);
        settle().await;
        assert!(
            layout.visible_panels.get_untracked().chat
                && layout.visible_panels.get_untracked().terminal
        );
        mounted.state.projects.active_project.set(None);
        settle().await;
        assert!(
            mounted
                .element(".statusbar [aria-label='Toggle Output']")
                .has_attribute("disabled")
        );
        assert!(!layout.visible_panels.get_untracked().terminal);
    }
}

#[wasm_bindgen_test]
async fn app_and_account_menus_group_destinations_and_collapse_to_user_icon() {
    use openwebide_frontend::components::TopBar;
    use openwebide_frontend::state::{responsive::LayoutMode, settings::ConfigurationSection};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for phone in [false, true] {
            let opened = RwSignal::new(0);
            let logged_out = RwSignal::new(false);
            let mounted = mount_test(move |state| {
                state.auth.set_user(user(1));
                state.seed_project();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
                expect_context::<LayoutState>().preferences.update(|prefs| {
                    prefs.mode = if phone {
                        LayoutMode::Phone
                    } else {
                        LayoutMode::Desktop
                    };
                });
                view! {
                    <style>{include_str!("../../styles.css")}</style>
                    <div class=if phone { "app phone-layout" } else { "app" }><TopBar on_open_settings=Callback::new(move |()| opened.update(|count| *count += 1)) on_logout=Callback::new(move |()| logged_out.set(true)) /></div>
                }
            });
            settle().await;
            mounted.click("[aria-label='App menu']");
            settle().await;
            let app_menu = mounted.element(".app-menu-items").text_content().unwrap();
            for destination in [
                "Open local project",
                "Open remote project",
                "Open projects",
                "Recent projects",
                "Configuration",
                "Servers",
                "Help / Keyboard shortcuts",
                "About",
            ] {
                assert!(app_menu.contains(destination), "{destination}");
            }
            for destination in ["Settings", "System prompts", "Log out"] {
                assert!(!app_menu.contains(destination), "{destination}");
            }
            mounted.click("button[aria-label='About']");
            settle().await;
            assert!(mounted.state.ui.about_open.get_untracked());
            mounted.state.ui.about_open.set(false);
            mounted.click("[aria-label='Account menu']");
            settle().await;
            mounted.click("button[aria-label='Settings']");
            settle().await;
            assert_eq!(opened.get_untracked(), 1);
            for (destination, section) in [
                ("Servers", ConfigurationSection::Servers),
                ("System prompts", ConfigurationSection::SystemPrompts),
            ] {
                mounted.click(if section == ConfigurationSection::Servers {
                    "[aria-label='App menu']"
                } else {
                    "[aria-label='Account menu']"
                });
                settle().await;
                mounted.click_text(destination);
                settle().await;
                assert_eq!(
                    mounted.state.settings.configuration.get_untracked(),
                    Some(section)
                );
                assert!(
                    mounted
                        .root
                        .query_selector(".ui-dropdown-menu")
                        .unwrap()
                        .is_none()
                );
                mounted.state.settings.configuration.set(None);
            }
            mounted.click("[aria-label='Account menu']");
            settle().await;
            mounted.click("button[aria-label='Log out']");
            settle().await;
            assert!(logged_out.get_untracked());
            assert!(
                mounted
                    .root
                    .query_selector(".ui-dropdown-menu")
                    .unwrap()
                    .is_none()
            );
            if phone {
                assert_eq!(
                    web_sys::window()
                        .unwrap()
                        .get_computed_style(&mounted.element(".topbar-user"))
                        .unwrap()
                        .unwrap()
                        .get_property_value("display")
                        .unwrap(),
                    "none"
                );
                assert!(
                    mounted
                        .element("[aria-label='Account menu'] .ui-dropdown-label .ui-icon-glyph")
                        .get_bounding_client_rect()
                        .width()
                        > 0.0
                );
            }
        }
    }
}

#[wasm_bindgen_test]
async fn settings_appearance_uses_compact_shared_segmented_controls() {
    use openwebide_frontend::{
        components::Settings,
        state::{responsive::LayoutMode, settings::Theme},
    };
    let slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let read = slot.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.auth.set_user(user(1));
        let layout = expect_context::<LayoutState>();
        provide_context(LayoutActions::new(state.api, layout, state.auth, state.ui));
        read.set(Some(layout));
        view! {
            <style>{include_str!("../../styles.css")}</style>
            <Settings on_set_theme=Callback::new(move |theme| state.settings.theme.set(theme)) on_set_notifications=Callback::new(|_| ()) on_set_default_prompt=Callback::new(|_| ()) on_set_bridge_url=Callback::new(|_| ()) />
        }
    });
    settle().await;
    let controls = mounted
        .root
        .query_selector_all(".ui-form-grid .ui-segmented-control")
        .unwrap();
    assert_eq!(controls.length(), 2);
    for index in 0..2 {
        let control = controls
            .item(index)
            .unwrap()
            .dyn_into::<web_sys::HtmlElement>()
            .unwrap();
        assert!(
            control.get_bounding_client_rect().width()
                < control
                    .parent_element()
                    .unwrap()
                    .get_bounding_client_rect()
                    .width()
        );
        assert_eq!(
            control
                .query_selector_all("button.ui-seg-btn")
                .unwrap()
                .length(),
            3
        );
    }
    mounted.click_text("Light");
    settle().await;
    assert_eq!(mounted.state.settings.theme.get_untracked(), Theme::Light);
    mounted.click_text("Desktop");
    settle().await;
    assert_eq!(
        slot.get().unwrap().preferences.get_untracked().mode,
        LayoutMode::Desktop
    );
    assert!(
        mounted
            .state
            .fake
            .settings
            .borrow()
            .contains_key("workspace_layout")
    );
}

#[wasm_bindgen_test]
async fn phone_navigation_is_equal_and_output_overlays_retained_pane_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let read = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            let layout = expect_context::<LayoutState>();
            let actions = LayoutActions::new(state.api, layout, state.auth, state.ui);
            provide_context(actions);
            read.set(Some((layout, actions)));
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <div class="app phone-layout" style="width:320px;height:650px">
                    <div class="app-body"><div class="workspace-docks">
                        <PanelRail panels=vec![Panel::Sessions, Panel::Files, Panel::Editor, Panel::Chat] />
                        <ToolPanel panel=Panel::Sessions><span>"Sessions"</span></ToolPanel>
                        <ToolPanel panel=Panel::Files><span>"Files"</span></ToolPanel>
                        <ToolPanel panel=Panel::Editor><input value="retained draft" /></ToolPanel>
                        <ToolPanel panel=Panel::Chat><span>"Chat"</span></ToolPanel>
                    </div><ToolPanel panel=Panel::Terminal><span>"Output"</span></ToolPanel></div>
                </div>
            }
        });
        settle().await;
        let (_layout, actions) = slot.get().unwrap();
        actions
            .set_mode
            .run(openwebide_frontend::state::responsive::LayoutMode::Phone);
        actions.show.run(Panel::Editor);
        settle().await;
        let rail = mounted.element(".panel-rail");
        let editor = mounted.element("#panel-editor");
        let before = editor.get_bounding_client_rect();
        let nav = rail.get_bounding_client_rect();
        assert!(nav.top() >= before.bottom() - 1.0);
        assert!(rail.scroll_width() <= rail.client_width());
        for panel in ["sessions", "files", "editor", "chat"] {
            let button = mounted.element(&format!(".panel-tab[aria-controls=panel-{panel}]"));
            assert!((button.get_bounding_client_rect().width() - nav.width() / 4.0).abs() < 2.0);
        }
        mounted.click(".panel-tab[aria-controls=panel-editor]");
        settle().await;
        assert_eq!(
            mounted
                .element(".panel-tab[aria-controls=panel-editor]")
                .get_attribute("aria-current")
                .as_deref(),
            Some("page")
        );
        actions.show.run(Panel::Terminal);
        settle().await;
        assert!((editor.get_bounding_client_rect().height() - before.height()).abs() < 1.0);
        let output = mounted
            .element("#panel-terminal")
            .get_bounding_client_rect();
        assert!(output.top() < nav.top());
        assert!((output.width() - 320.0).abs() < 1.0);
        assert!((output.bottom() - nav.bottom()).abs() < 1.0);
        assert_eq!(
            mounted
                .element("#panel-editor input")
                .unchecked_into::<web_sys::HtmlInputElement>()
                .value(),
            "retained draft"
        );
    }
}

#[wasm_bindgen_test]
async fn window_title_tracks_file_project_session_and_logout_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_session();
            state.auth.set_user(user(1));
            state.projects.projects.update(|items| {
                items[0].mode = mode;
                items[0].name = "My project".into();
            });
            state
                .chat
                .sessions
                .update(|items| items[0].name = "My conversation".into());
            state.workspace.open_file.set(Some("src/main.rs".into()));
            openwebide_frontend::state_actions::lifecycle::install_window_title(
                state.auth,
                state.projects,
                state.workspace,
                state.chat,
            );
            view! { <div /> }
        });
        settle().await;
        assert_eq!(
            document().title(),
            "main.rs — My project — My conversation — Open WebIDE"
        );
        mounted
            .state
            .workspace
            .open_file
            .set(Some("README.md".into()));
        mounted
            .state
            .chat
            .sessions
            .update(|items| items[0].name = "Renamed".into());
        settle().await;
        assert_eq!(
            document().title(),
            "README.md — My project — Renamed — Open WebIDE"
        );
        mounted.state.workspace.open_file.set(None);
        mounted.state.workspace.active_session.set(None);
        settle().await;
        assert_eq!(document().title(), "My project — Open WebIDE");
        mounted.state.projects.active_project.set(None);
        settle().await;
        assert_eq!(document().title(), "Open WebIDE");
        mounted.state.auth.logout();
        settle().await;
        assert_eq!(document().title(), "Open WebIDE");
    }
}

#[wasm_bindgen_test]
async fn feature_headers_minimize_files_and_editor_and_retain_the_draft_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.auth.set_user(user(1));
            let layout = expect_context::<LayoutState>();
            provide_context(LayoutActions::new(state.api, layout, state.auth, state.ui));
            Effect::new(move |_| {
                layout.preferences.update(|prefs| {
                    prefs.mode = openwebide_frontend::state::responsive::LayoutMode::Desktop;
                });
            });
            state.workspace.open_file.set(Some("test.rs".into()));
            state.workspace.content.set("unsaved editor draft".into());
            state.workspace.dirty.set(true);
            let editor = super::support::editor_view(state);
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <div class="app" style="height:600px"><div class="app-body"><PanelRail panels=vec![Panel::Files, Panel::Editor] /><div class="workspace-docks">
                <ToolPanel panel=Panel::Files><openwebide_frontend::components::FilesPanel on_new_file=Callback::new(|()| ()) on_new_dir=Callback::new(|()| ())><p>"Files retained"</p></openwebide_frontend::components::FilesPanel></ToolPanel>
                <ToolPanel panel=Panel::Editor>{editor}</ToolPanel></div></div></div>
            }
        });
        settle().await;
        for panel in ["Files", "Editor"] {
            let button = mounted.element(&format!("button[aria-label='Minimize {panel}']"));
            assert!(
                button.offset_height() > 0,
                "{panel} minimize must be visible"
            );
            button.click();
            settle().await;
            let id = panel.to_lowercase();
            assert_eq!(
                mounted
                    .element(&format!("#panel-{id}"))
                    .style()
                    .get_property_value("display")
                    .unwrap(),
                "none"
            );
            assert_eq!(
                mounted.state.workspace.content.get_untracked(),
                "unsaved editor draft"
            );
            assert!(mounted.state.workspace.dirty.get_untracked());
            mounted.click(&format!("button[aria-controls='panel-{id}']"));
            settle().await;
            assert_eq!(
                mounted
                    .element(&format!("#panel-{id}"))
                    .style()
                    .get_property_value("display")
                    .unwrap(),
                "flex"
            );
            assert!(
                mounted
                    .element(&format!("button[aria-label='Minimize {panel}']"))
                    .offset_height()
                    > 0
            );
        }
    }
}

#[wasm_bindgen_test]
async fn rightmost_history_fills_remaining_dock_width_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let observed_layout = StoredValue::new(None);
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            let layout = expect_context::<LayoutState>();
            observed_layout.set_value(Some(layout));
            provide_context(LayoutActions::new(state.api, layout, state.auth, state.ui));
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <div class="app" style="width:1200px;height:600px;display:flex;flex-direction:row">
                    <ToolPanel panel=Panel::Editor><textarea>"editor"</textarea></ToolPanel>
                    <ToolPanel panel=Panel::Chat><textarea>"chat"</textarea></ToolPanel>
                    <ToolPanel panel=Panel::History><span>"history"</span></ToolPanel>
                </div>
            }
        });
        settle().await;
        let layout = observed_layout.get_value().unwrap();
        layout.panels.update(|panels| {
            panels.sessions = false;
            panels.files = false;
            panels.history = true;
        });
        layout.preferences.set(
            serde_json::from_str(
                r#"{"mode":"desktop","order":["editor","chat","history","files","sessions"]}"#,
            )
            .unwrap(),
        );
        settle().await;
        let history = mounted.element("#panel-history");
        assert!(history.class_list().contains("tool-panel-center"));
        let width = history.get_bounding_client_rect().width();
        assert!(width > 500.0, "history width {width}");
        assert!(
            mounted
                .element("#panel-editor")
                .get_bounding_client_rect()
                .width()
                >= 260.0
        );
        assert!(
            mounted
                .element("#panel-chat")
                .get_bounding_client_rect()
                .width()
                >= 200.0
        );
        mounted.click("#panel-chat button[aria-label='Minimize Chat']");
        settle().await;
        assert!(history.get_bounding_client_rect().width() > width);
        mounted
            .element(".app")
            .class_list()
            .add_1("editor-collapsed")
            .unwrap();
        layout.panels.update(|panels| {
            panels.editor = false;
            panels.chat = true;
        });
        settle().await;
        assert!(
            !mounted
                .element("#panel-chat")
                .class_list()
                .contains("tool-panel-center")
        );
        assert!(
            (history.get_bounding_client_rect().width()
                - (1200.0 - layout.chat_width.get_untracked()))
            .abs()
                < 1.0
        );
        mounted
            .element(".app")
            .class_list()
            .remove_1("editor-collapsed")
            .unwrap();
        layout.panels.update(|panels| {
            panels.editor = true;
            panels.chat = false;
        });
        settle().await;
        mounted.click("#panel-history button[aria-label='Panel actions']");
        settle().await;
        mounted.click("#panel-history button[aria-label='Move History left']");
        settle().await;
        assert!(!history.class_list().contains("tool-panel-center"));
        assert!(
            mounted
                .element("#panel-editor")
                .class_list()
                .contains("tool-panel-center")
        );
    }
}
