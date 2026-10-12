use super::support::{mount_test_with_backend, settle};
use leptos::prelude::*;
use openwebide_core::{
    User, UserId, UserRole, WorkspaceMode,
    plugins::{ProjectPlugin, RustPlugin, panels::*, testing::receipt},
};
use openwebide_frontend::{
    components::{ConfirmDialog, PanelRail, ToolPanel, plugin_panels::PluginPanels},
    state::{
        layout::{LayoutState, Panel},
        plugin_panels::PluginPanelsState,
        plugins::PluginsState,
        responsive::{LAYOUT_PREFERENCES_KEY, LayoutMode},
    },
    state_actions::layout::LayoutActions,
    testing::fake_backend::FakeBackend,
};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen_test::*;

fn binding() -> ProjectPlugin {
    let mut prepared = receipt();
    prepared.manifest.compatibility.plugin_api = 4;
    prepared.manifest.contributions.skills.clear();
    prepared.manifest.contributions.panels = vec![PanelDefinition {
        id: "reviews".into(),
        title: "Reviews".into(),
        icon: PanelIcon::GitPullRequest,
        view: "render".into(),
        actions: Vec::new(),
        refresh_seconds: None,
    }];
    prepared.manifest.executable = Some(RustPlugin {
        manifest: "Cargo.toml".into(),
        library: "reviews".into(),
        sdk_version: "0.1.0".into(),
        capabilities: vec!["http".into()],
    });
    ProjectPlugin {
        id: 1,
        revision: 1,
        enabled: true,
        prepared,
    }
}

#[wasm_bindgen_test]
async fn contributed_panel_navigation_persists_and_recovers_when_disabled_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fake = Rc::new(FakeBackend::default());
        fake.project_plugin_entries
            .borrow_mut()
            .insert(1, vec![binding()]);
        let captured = Rc::new(Cell::new(None));
        let slot = captured.clone();
        let mounted = mount_test_with_backend(fake.clone(), move |state| {
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
            let layout = expect_context::<LayoutState>();
            let plugins = expect_context::<PluginsState>();
            let actions = LayoutActions::new(state.api, layout, state.auth, state.ui);
            provide_context(actions);
            provide_context(PluginPanelsState::new(plugins, state.projects, layout));
            slot.set(Some((layout, plugins, actions)));
            view! {
                <PanelRail panels=vec![Panel::Editor,Panel::Chat]/>
                <ToolPanel panel=Panel::Editor><span>"Editor stays available"</span></ToolPanel>
                <ToolPanel panel=Panel::Chat><span>"Chat stays available"</span></ToolPanel>
                <PluginPanels/>
            }
        });
        settle().await;
        let (layout, plugins, actions) = captured.get().unwrap();
        plugins.project_plugins.set(vec![binding()]);
        actions.set_mode.run(LayoutMode::Desktop);
        settle().await;
        mounted.click("button[title='Show or collapse Reviews']");
        settle().await;
        assert_eq!(
            mounted
                .element("button[title='Show or collapse Reviews']")
                .get_attribute("aria-expanded")
                .as_deref(),
            Some("true")
        );
        assert!(
            mounted
                .element("section[aria-label='Reviews']")
                .get_attribute("style")
                .unwrap()
                .contains("display: flex")
        );
        assert!(
            mounted
                .root
                .query_selector("section[aria-label='Reviews'] [role=alert]")
                .unwrap()
                .is_some()
        );
        let persisted: serde_json::Value =
            serde_json::from_str(&fake.settings.borrow()[LAYOUT_PREFERENCES_KEY]).unwrap();
        assert!(
            persisted["plugin_panels"]
                .as_object()
                .unwrap()
                .values()
                .any(|panel| panel["visible"] == true)
        );
        mounted.click("button[aria-label='Minimize Reviews']");
        settle().await;
        assert!(
            mounted
                .element("section[aria-label='Reviews']")
                .get_attribute("style")
                .unwrap()
                .contains("display: none")
        );
        actions.set_mode.run(LayoutMode::Phone);
        settle().await;
        mounted.click("button[title='Show or collapse Reviews']");
        settle().await;
        assert!(layout.plugin_sheet.get_untracked().is_some());
        assert!(!layout.visible_panels.get_untracked().chat);
        plugins
            .project_plugins
            .update(|entries| entries[0].enabled = false);
        settle().await;
        assert!(layout.plugin_sheet.get_untracked().is_none());
        assert!(layout.visible_panels.get_untracked().chat);
        assert!(
            mounted
                .root
                .query_selector("section[aria-label='Reviews']")
                .unwrap()
                .is_none()
        );
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Chat stays available")
        );
    }
}

fn action_binding() -> ProjectPlugin {
    let mut plugin = binding();
    plugin.prepared.manifest.contributions.panels[0].actions = vec![PanelAction {
        id: "publish".into(),
        handler: "publish".into(),
        title: "Publish review".into(),
        requires_approval: true,
    }];
    plugin
}
fn action_document(revision: u64) -> PanelDocument {
    PanelDocument {
        schema_version: UI_VERSION,
        revision,
        state: serde_json::json!({"repository":"owner/repo"}),
        blocks: vec![Block::Actions {
            buttons: vec![PanelButton {
                action: "publish".into(),
                label: "Publish review".into(),
                values: [("number".into(), serde_json::json!(42))].into(),
                disabled: false,
            }],
        }],
    }
}

#[wasm_bindgen_test]
async fn panel_actions_use_both_host_adapters_and_reject_obsolete_approvals() {
    use super::local_bridge::{
        HttpGuard, bridge_calls, fake_bridge_http, set_bridge_panel_documents, set_bridge_plugin,
    };
    use super::support::wait_until;
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("panel-contract-token")
        .await
        .unwrap();
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for display in [LayoutMode::Desktop, LayoutMode::Phone] {
            for change in [
                "publish",
                "close",
                "disable",
                "project",
                "account",
                "update",
                "backend_update",
                "initial_backend_update",
            ] {
                let http = HttpGuard(fake_bridge_http());
                let plugin = action_binding();
                set_bridge_plugin(&http.0, &serde_json::to_string(&plugin.prepared).unwrap());
                set_bridge_panel_documents(
                    &http.0,
                    &serde_json::to_string(&vec![action_document(1), action_document(2)]).unwrap(),
                );
                let fake = Rc::new(FakeBackend::default());
                fake.panel_coordination.set(true);
                fake.panel_documents
                    .borrow_mut()
                    .extend([action_document(1), action_document(2)]);
                fake.project_plugin_entries
                    .borrow_mut()
                    .insert(1, vec![plugin.clone()]);
                let captured = Rc::new(Cell::new(None));
                let slot = captured.clone();
                let mounted = mount_test_with_backend(fake.clone(), move |state| {
                    state.seed_project();
                    state
                        .projects
                        .projects
                        .update(|projects| projects[0].mode = mode);
                    state
                        .settings
                        .bridge_url
                        .set("ws://bridge.test:3001".into());
                    state.auth.set_user(User {
                        id: UserId::new(1),
                        username: "owner".into(),
                        role: UserRole::User,
                        created_at: 0,
                    });
                    let layout = expect_context::<LayoutState>();
                    let plugins = expect_context::<PluginsState>();
                    let actions = LayoutActions::new(state.api, layout, state.auth, state.ui);
                    provide_context(actions);
                    provide_context(PluginPanelsState::new(plugins, state.projects, layout));
                    slot.set(Some((layout, plugins, actions)));
                    view! { <PanelRail panels=vec![Panel::Chat]/><PluginPanels/><ConfirmDialog/> }
                });
                settle().await;
                let (_, plugins, actions) = captured.get().unwrap();
                plugins.project_plugins.set(vec![plugin]);
                actions.set_mode.run(display);
                settle().await;
                if change == "initial_backend_update" {
                    fake.project_plugin_entries
                        .borrow_mut()
                        .get_mut(&1)
                        .unwrap()[0]
                        .prepared
                        .digest = "d".repeat(64);
                }
                mounted.click("button[title='Show or collapse Reviews']");
                if change == "initial_backend_update" {
                    wait_until("stale panel metadata rejection", || {
                        mounted
                            .root
                            .query_selector("section[aria-label='Reviews'] [role=alert]")
                            .unwrap()
                            .is_some()
                    })
                    .await;
                    assert!(fake.panel_begins.borrow().is_empty());
                    assert!(fake.plugin_action_calls.borrow().is_empty());
                    let calls: Vec<serde_json::Value> =
                        serde_json::from_str(&bridge_calls(&http.0)).unwrap();
                    assert!(!calls.iter().any(|call| call["path"] == "/plugins/invoke"));
                    drop(mounted);
                    settle().await;
                    continue;
                }
                wait_until("initial panel document", || {
                    !fake.panel_finishes.borrow().is_empty()
                })
                .await;
                assert!(
                    fake.panel_finishes.borrow()[0].outcome.is_ok(),
                    "{mode:?} {change}: {:?}",
                    fake.panel_finishes.borrow()
                );
                let button = mounted
                    .root
                    .query_selector("section[aria-label='Reviews'] .plugin-panel-document button")
                    .unwrap();
                assert!(
                    button.is_some(),
                    "initial panel action control: {mode:?} {change}"
                );
                mounted.click("section[aria-label='Reviews'] .plugin-panel-document button");
                settle().await;
                let approval = mounted
                    .state
                    .ui
                    .confirm
                    .get_untracked()
                    .expect("declared action requires approval");
                assert!(approval.message.contains("42"));
                assert!(
                    !fake
                        .panel_begins
                        .borrow()
                        .iter()
                        .any(|begin| begin.input.action.is_some())
                );
                match change {
                    "close" => mounted.click("button[aria-label='Minimize Reviews']"),
                    "disable" => plugins
                        .project_plugins
                        .update(|bindings| bindings[0].enabled = false),
                    "project" => mounted.state.projects.active_project.set(Some(2)),
                    "account" => mounted.state.auth.set_user(User {
                        id: UserId::new(2),
                        username: "other".into(),
                        role: UserRole::User,
                        created_at: 0,
                    }),
                    "update" => plugins
                        .project_plugins
                        .update(|bindings| bindings[0].prepared.digest = "d".repeat(64)),
                    "backend_update" => {
                        fake.project_plugin_entries
                            .borrow_mut()
                            .get_mut(&1)
                            .unwrap()[0]
                            .prepared
                            .digest = "d".repeat(64);
                    }
                    "publish" => (),
                    _ => unreachable!(),
                }
                settle().await;
                mounted.click("button.btn.danger");
                settle().await;
                if change == "publish" {
                    wait_until("approved panel action", || {
                        fake.panel_finishes.borrow().len() == 2
                    })
                    .await;
                    let begins = fake.panel_begins.borrow();
                    assert_eq!(begins.len(), 2);
                    assert!(begins[1].approved);
                    assert_eq!(begins[1].input.revision, 1);
                    assert_eq!(begins[1].input.instance, begins[0].input.instance);
                    assert_eq!(begins[1].input.values["number"], serde_json::json!(42));
                    assert_eq!(
                        fake.panel_finishes.borrow()[1]
                            .outcome
                            .as_ref()
                            .unwrap()
                            .revision,
                        2
                    );
                } else {
                    assert!(
                        !fake
                            .panel_begins
                            .borrow()
                            .iter()
                            .any(|begin| begin.input.action.is_some()),
                        "stale approval executed: {mode:?} {change}"
                    );
                }
                let calls: Vec<serde_json::Value> =
                    serde_json::from_str(&bridge_calls(&http.0)).unwrap();
                let invokes: Vec<_> = calls
                    .iter()
                    .filter(|call| call["path"] == "/plugins/invoke")
                    .collect();
                if mode == WorkspaceMode::Local {
                    assert!(!invokes.is_empty());
                    assert_eq!(invokes[0]["authorization"], "Bearer panel-contract-token");
                    assert_eq!(invokes[0]["body"]["call"]["operation"], "panel");
                    assert!(fake.plugin_action_calls.borrow().is_empty());
                } else {
                    assert!(invokes.is_empty());
                    assert!(!fake.plugin_action_calls.borrow().is_empty());
                }
                drop(mounted);
                settle().await;
            }
        }
    }
    openwebide_frontend::idb::set_bridge_pairing_token(previous.as_deref().unwrap_or(""))
        .await
        .unwrap();
}

#[wasm_bindgen_test]
async fn delayed_view_reply_cannot_replace_a_closed_panel_in_either_mode() {
    use super::local_bridge::{
        HttpGuard, bridge_calls, defer_bridge_panel, fake_bridge_http, release_bridge_panel,
        set_bridge_panel_documents, set_bridge_plugin,
    };
    use super::support::wait_until;
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("panel-contract-token")
        .await
        .unwrap();
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let http = HttpGuard(fake_bridge_http());
        let plugin = action_binding();
        set_bridge_plugin(&http.0, &serde_json::to_string(&plugin.prepared).unwrap());
        set_bridge_panel_documents(
            &http.0,
            &serde_json::to_string(&vec![action_document(1)]).unwrap(),
        );
        let fake = Rc::new(FakeBackend::default());
        fake.panel_coordination.set(true);
        fake.panel_documents
            .borrow_mut()
            .push_back(action_document(1));
        fake.project_plugin_entries
            .borrow_mut()
            .insert(1, vec![plugin.clone()]);
        let captured = Rc::new(Cell::new(None));
        let slot = captured.clone();
        let mounted = mount_test_with_backend(fake.clone(), move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state
                .settings
                .bridge_url
                .set("ws://bridge.test:3001".into());
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "owner".into(),
                role: UserRole::User,
                created_at: 0,
            });
            let layout = expect_context::<LayoutState>();
            let plugins = expect_context::<PluginsState>();
            let actions = LayoutActions::new(state.api, layout, state.auth, state.ui);
            provide_context(actions);
            provide_context(PluginPanelsState::new(plugins, state.projects, layout));
            slot.set(Some((plugins, actions)));
            view! {<PanelRail panels=vec![Panel::Chat]/><PluginPanels/>}
        });
        settle().await;
        let (plugins, actions) = captured.get().unwrap();
        plugins.project_plugins.set(vec![plugin]);
        actions.set_mode.run(LayoutMode::Desktop);
        settle().await;
        mounted.click("button[title='Show or collapse Reviews']");
        wait_until("initial view before delayed refresh", || {
            fake.panel_finishes.borrow().len() == 1
        })
        .await;
        let (sender, receiver) = futures::channel::oneshot::channel();
        if mode == WorkspaceMode::Remote {
            fake.panel_document_results.borrow_mut().push_back(receiver);
        } else {
            defer_bridge_panel(&http.0);
        }
        mounted.click("button[aria-label='Refresh panel']");
        wait_until("delayed refresh claim", || {
            fake.panel_begins.borrow().len() == 2
        })
        .await;
        wait_until("host transport awaiting delayed reply", || {
            if mode == WorkspaceMode::Remote {
                fake.panel_document_results.borrow().is_empty()
            } else {
                let calls: Vec<serde_json::Value> =
                    serde_json::from_str(&bridge_calls(&http.0)).unwrap();
                calls
                    .iter()
                    .filter(|call| call["path"] == "/plugins/continue")
                    .count()
                    == 2
            }
        })
        .await;
        mounted.click("button[aria-label='Minimize Reviews']");
        settle().await;
        if mode == WorkspaceMode::Remote {
            sender.send(Ok(action_document(2))).unwrap();
        } else {
            release_bridge_panel(
                &http.0,
                &serde_json::to_string(&action_document(2)).unwrap(),
            );
        }
        settle().await;
        wait_until("closed-instance cleanup", || {
            !fake.panel_closes.borrow().is_empty()
        })
        .await;
        settle().await;
        assert_eq!(
            fake.panel_finishes.borrow().len(),
            1,
            "late view reached publication: {mode:?}"
        );
        assert!(
            mounted
                .element("section[aria-label='Reviews']")
                .get_attribute("style")
                .unwrap()
                .contains("display: none")
        );
        drop(mounted);
        settle().await;
    }
    openwebide_frontend::idb::set_bridge_pairing_token(previous.as_deref().unwrap_or(""))
        .await
        .unwrap();
}
