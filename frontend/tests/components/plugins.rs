use super::support::{mount_test_with_backend, settle};
use leptos::prelude::*;
use openwebide_core::{User, UserId, UserRole, WorkspaceMode, plugins::testing::receipt};
use openwebide_frontend::{
    project_host::ProjectHost, project_plugins::ProjectPluginActions, state::plugins::PluginsState,
    testing::fake_backend::FakeBackend,
};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen_test::*;

#[wasm_bindgen_test]
async fn plugins_explain_retired_tool_groups_and_prevent_reactivation_in_both_modes() {
    use openwebide_core::plugins::{
        PluginInstallation, PluginToolGroup, PluginUpdatePolicy, ProjectPlugin,
    };
    use wasm_bindgen::JsCast;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fake = Rc::new(FakeBackend::default());
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
            let plugins = PluginsState::default();
            let host = ProjectHost::new(state.api, state.projects, state.settings, state.auth);
            let actions = ProjectPluginActions::new(
                state.api,
                plugins,
                host,
                state.auth,
                state.projects,
                state.chat,
                state.settings,
            );
            slot.set(Some((plugins, actions)));
            provide_context(plugins);
            provide_context(actions);
            view! {<openwebide_frontend::components::Plugins/>}
        });
        settle().await;
        let (plugins, actions) = captured.get().unwrap();
        let mut prepared = receipt();
        prepared.manifest.compatibility.plugin_api = 2;
        prepared.manifest.contributions.tool_groups = vec![PluginToolGroup::Memory];
        plugins.installations.set(vec![PluginInstallation {
            prepared: prepared.clone(),
            revision: 1,
            hosts: vec![prepared.host_id.clone()],
            installed_at: 0,
            default_enabled: true,
            update_policy: PluginUpdatePolicy::Notify,
        }]);
        plugins.project_plugins.set(vec![ProjectPlugin {
            id: 1,
            revision: 1,
            prepared,
            enabled: false,
        }]);
        settle().await;
        let text = mounted.root.text_content().unwrap();
        assert!(text.contains("Update required"));
        assert!(text.contains("Your data and update preferences are retained"));
        let buttons = mounted
            .root
            .query_selector_all(".plugin-row button")
            .unwrap();
        let enable = (0..buttons.length())
            .filter_map(|i| buttons.item(i))
            .find(|button| {
                button
                    .text_content()
                    .is_some_and(|text| text.trim() == "Enable")
            })
            .expect("Enable control")
            .unchecked_into::<web_sys::HtmlButtonElement>();
        assert!(enable.disabled());
        mounted.click("button[aria-label='Installed plugin actions']");
        settle().await;
        let menu = mounted
            .root
            .query_selector_all("button[role='menuitem']")
            .unwrap();
        let enable = (0..menu.length())
            .filter_map(|i| menu.item(i))
            .find(|button| {
                button
                    .text_content()
                    .is_some_and(|text| text.trim() == "Enable for project")
            })
            .expect("Project activation menu item")
            .unchecked_into::<web_sys::HtmlButtonElement>();
        assert!(enable.disabled());
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Update preferences")
        );
        let installed = plugins.installations.get_untracked();
        actions.enable.run(installed[0].clone());
        settle().await;
        assert!(
            plugins
                .error
                .get_untracked()
                .is_some_and(|error| error.contains("Update required"))
        );
        assert_eq!(plugins.installations.get_untracked(), installed);
        assert!(fake.plugin_records.borrow().is_empty());
    }
}

#[wasm_bindgen_test]
async fn plugins_poll_host_preparation_and_cancel_pending_requests_without_recording_stale_receipts()
 {
    use super::support::wait_until;
    use openwebide_core::plugins::preparation::{
        PluginPreparation, PreparationCommand, PreparationState,
    };
    for boundary in ["cancel", "account", "success", "wrong-id"] {
        let fake = Rc::new(FakeBackend::default());
        let captured = Rc::new(Cell::new(None));
        let slot = captured.clone();
        let mounted = mount_test_with_backend(fake.clone(), move |state| {
            state.seed_project();
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "test".into(),
                role: UserRole::User,
                created_at: 0,
            });
            let plugins = PluginsState::default();
            let host = ProjectHost::new(state.api, state.projects, state.settings, state.auth);
            let actions = ProjectPluginActions::new(
                state.api,
                plugins,
                host,
                state.auth,
                state.projects,
                state.chat,
                state.settings,
            );
            slot.set(Some((plugins, actions)));
            provide_context(plugins);
            provide_context(actions);
            view! {<openwebide_frontend::components::Plugins/>}
        });
        settle().await;
        let (plugins, actions) = captured.get().unwrap();
        let prepared = receipt();
        let id = "a".repeat(32);
        for state in [PreparationState::Queued, PreparationState::Preparing] {
            let (send, receive) = futures::channel::oneshot::channel();
            send.send(Ok(PluginPreparation {
                id: id.clone(),
                state,
                prepared: None,
                error: None,
            }))
            .unwrap();
            fake.plugin_progress.borrow_mut().push_back(receive);
        }
        let (send, receive) = futures::channel::oneshot::channel();
        fake.plugin_progress.borrow_mut().push_back(receive);
        plugins.repository.set(prepared.source.repository.clone());
        plugins.commit.set(prepared.source.commit.clone());
        plugins.path.set(prepared.source.path.clone());
        actions.install.run(());
        wait_until("pending host preparation status", || {
            fake.plugin_preparation_commands
                .borrow()
                .iter()
                .filter(|(_, command)| matches!(command, PreparationCommand::Status { .. }))
                .count()
                == 2
        })
        .await;
        assert!(plugins.busy.get_untracked());
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Preparing plugin on host")
        );
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Cancel installation")
        );
        match boundary {
            "cancel" => mounted.click_text("Cancel installation"),
            "account" => mounted.state.auth.logout(),
            _ => {
                if boundary == "success" {
                    let (package_send, receive) = futures::channel::oneshot::channel();
                    package_send
                        .send(Ok(openwebide_core::plugins::testing::package()))
                        .unwrap();
                    fake.plugin_packages.borrow_mut().push_back(receive);
                }
                send.send(Ok(PluginPreparation {
                    id: if boundary == "wrong-id" {
                        "b".repeat(32)
                    } else {
                        id.clone()
                    },
                    state: PreparationState::Ready,
                    prepared: Some(prepared),
                    error: None,
                }))
                .unwrap();
                wait_until("finished host preparation", || {
                    !plugins.busy.get_untracked()
                })
                .await;
                if boundary == "wrong-id" {
                    assert!(
                        plugins
                            .error
                            .get_untracked()
                            .unwrap()
                            .contains("different preparation")
                    );
                    assert!(fake.plugin_records.borrow().is_empty());
                } else {
                    assert!(plugins.error.get_untracked().is_none());
                    assert_eq!(fake.plugin_records.borrow().len(), 1);
                    assert_eq!(plugins.installations.get_untracked().len(), 1);
                    assert!(
                        !fake.plugin_preparation_commands.borrow().iter().any(
                            |(_, command)| matches!(command, PreparationCommand::Cancel { .. })
                        )
                    );
                }
                continue;
            }
        }
        wait_until("host cancellation after request invalidation", || fake.plugin_preparation_commands.borrow()
            .iter().any(|(_,command)|matches!(command, PreparationCommand::Cancel { id: cancelled } if cancelled == &id))).await;
        assert!(fake.plugin_records.borrow().is_empty());
        assert!(
            send.send(Ok(PluginPreparation {
                id,
                state: PreparationState::Ready,
                prepared: Some(prepared),
                error: None
            }))
            .is_err(),
            "The pending status request must be dropped"
        );
        assert!(plugins.preparation.get_untracked().is_none());
    }
}

#[wasm_bindgen_test]
async fn plugins_review_new_capabilities_before_recording_updates_and_clear_stale_reviews() {
    use openwebide_core::plugins::{
        PluginPackage, PluginTool, PluginUpdatePolicy, RecordPlugin, RustPlugin,
        record_installation,
    };
    for boundary in [
        "approve",
        "dismiss",
        "account",
        "host",
        "automatic",
        "all",
        "account-race",
        "host-race",
    ] {
        let fake = Rc::new(FakeBackend::default());
        let mut original = receipt();
        original.manifest.compatibility.plugin_api = 3;
        original.manifest.contributions.skills.clear();
        original.manifest.contributions.tools = vec![PluginTool {
            name: "community_notes".into(),
            description: "Notes".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: false,
        }];
        original.manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "notes".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["records".into()],
        });
        let installed = record_installation(
            Vec::new(),
            &RecordPlugin {
                approved_capabilities: Vec::new(),
                update_policy: Some(PluginUpdatePolicy::Automatic),
                prepared: original.clone(),
                revision: None,
                package: Some(Box::new(PluginPackage {
                    prepared: original.clone(),
                    skills: Vec::new(),
                })),
            },
            1,
        )
        .unwrap();
        *fake.plugins.borrow_mut() = installed;
        let mut prepared = original;
        prepared.source.commit = "b".repeat(40);
        prepared.digest = "b".repeat(64);
        prepared.manifest.version = "0.1.1".into();
        prepared
            .manifest
            .executable
            .as_mut()
            .unwrap()
            .capabilities
            .push("http".into());
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(prepared.clone())).unwrap();
        fake.plugin_preparations.borrow_mut().push_back(receive);
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(PluginPackage {
            prepared: prepared.clone(),
            skills: Vec::new(),
        }))
        .unwrap();
        fake.plugin_packages.borrow_mut().push_back(receive);
        let captured = Rc::new(Cell::new(None));
        let slot = captured.clone();
        let mounted = mount_test_with_backend(fake.clone(), move |state| {
            state.seed_project();
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "test".into(),
                role: UserRole::User,
                created_at: 0,
            });
            let plugins = PluginsState::default();
            let host = ProjectHost::new(state.api, state.projects, state.settings, state.auth);
            let actions = ProjectPluginActions::new(
                state.api,
                plugins,
                host,
                state.auth,
                state.projects,
                state.chat,
                state.settings,
            );
            slot.set(Some((plugins, actions)));
            provide_context(plugins);
            provide_context(actions);
            view! {<openwebide_frontend::components::Plugins/>}
        });
        settle().await;
        let (plugins, actions) = captured.get().unwrap();
        plugins.repository.set(prepared.source.repository.clone());
        plugins.commit.set(prepared.source.commit.clone());
        plugins.path.set(prepared.source.path.clone());
        if matches!(boundary, "automatic" | "all") {
            let mut catalog = openwebide_core::plugins::testing::catalog();
            let mut release = catalog.catalog.plugins[0].releases[0].clone();
            release.version = prepared.manifest.version.clone();
            release.source.commit = prepared.source.commit.clone();
            catalog.catalog.plugins[0].releases.push(release);
            let markets = openwebide_core::plugins::marketplace::MarketplaceSettings {
                revision: 1,
                sources: vec![catalog.source.clone()],
                catalogs: vec![catalog],
            };
            *fake.marketplaces.borrow_mut() = markets.clone();
            if boundary == "automatic" {
                actions.refresh_catalogs.run(());
            } else {
                plugins.marketplaces.set(markets);
                actions.update_all.run(());
            }
        } else {
            actions.install.run(());
        }
        settle().await;
        assert!(fake.plugin_records.borrow().is_empty());
        assert_eq!(plugins.pending_updates.get_untracked().len(), 1);
        assert_eq!(fake.plugins.borrow()[0].prepared.manifest.version, "0.1.0");
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("additional access: http")
        );
        match boundary {
            "approve" | "automatic" | "all" => mounted.click_text("Approve and update"),
            "dismiss" => mounted.click_text("Keep current version"),
            "account" => mounted.state.auth.logout(),
            "account-race" => {
                mounted.state.auth.logout();
                actions.approve_update.run(prepared.source.clone());
            }
            "host-race" => {
                mounted
                    .state
                    .settings
                    .bridge_url
                    .set("ws://other-host:3001".into());
                actions.approve_update.run(prepared.source.clone());
            }
            "host" => mounted
                .state
                .settings
                .bridge_url
                .set("ws://other-host:3001".into()),
            _ => unreachable!(),
        }
        settle().await;
        assert!(plugins.pending_updates.get_untracked().is_empty());
        if matches!(boundary, "approve" | "automatic" | "all") {
            assert_eq!(fake.plugin_records.borrow().len(), 1);
            assert_eq!(
                fake.plugin_records.borrow()[0].approved_capabilities,
                vec!["http"]
            );
            assert_eq!(fake.plugins.borrow()[0].prepared.manifest.version, "0.1.1");
        } else {
            assert!(fake.plugin_records.borrow().is_empty());
            assert_eq!(fake.plugins.borrow()[0].prepared.manifest.version, "0.1.0");
        }
    }
}

#[wasm_bindgen_test]
async fn plugins_guard_preparation_before_recording_on_account_project_and_host_changes() {
    for boundary in [
        "account",
        "project",
        "host",
        "success",
        "unpaired",
        "projectless",
        "projectless-stale",
    ] {
        let fake = Rc::new(FakeBackend::default());
        let (send, receive) = futures::channel::oneshot::channel();
        fake.plugin_preparations.borrow_mut().push_back(receive);
        let (package_send, package_receive) = futures::channel::oneshot::channel();
        package_send
            .send(Ok(openwebide_core::plugins::testing::package()))
            .unwrap();
        fake.plugin_packages.borrow_mut().push_back(package_receive);
        let captured = Rc::new(Cell::new(None));
        let slot = captured.clone();
        let mounted = mount_test_with_backend(fake.clone(), move |state| {
            state.seed_project();
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "test".into(),
                role: UserRole::User,
                created_at: 0,
            });
            let plugins = PluginsState::default();
            let host = ProjectHost::new(state.api, state.projects, state.settings, state.auth);
            let actions = ProjectPluginActions::new(
                state.api,
                plugins,
                host,
                state.auth,
                state.projects,
                state.chat,
                state.settings,
            );
            slot.set(Some((plugins, actions)));
            provide_context(plugins);
            provide_context(actions);
            view! {<openwebide_frontend::components::Plugins/>}
        });
        settle().await;
        let (plugins, actions) = captured.get().unwrap();
        if boundary.starts_with("projectless") {
            mounted.state.projects.active_project.set(None);
            settle().await;
        }
        if boundary == "unpaired" {
            mounted
                .state
                .projects
                .projects
                .update(|projects| projects[0].mode = WorkspaceMode::Local);
            settle().await;
        }
        let prepared = receipt();
        plugins.repository.set(prepared.source.repository.clone());
        plugins.commit.set(prepared.source.commit.clone());
        plugins.path.set(prepared.source.path.clone());
        actions.install.run(());
        settle().await;
        if boundary == "unpaired" {
            super::support::wait_until("unpaired plugin host failure", || {
                !plugins.busy.get_untracked() && plugins.error.get_untracked().is_some()
            })
            .await;
            assert!(fake.plugin_requests.borrow().is_empty());
            assert!(plugins.error.get_untracked().is_some());
            continue;
        }
        assert_eq!(fake.plugin_requests.borrow().len(), 1);
        match boundary {
            "account" => mounted.state.auth.logout(),
            "project" => mounted.state.projects.active_project.set(None),
            "projectless-stale" => mounted.state.projects.active_project.set(Some(1)),
            "host" => mounted
                .state
                .settings
                .bridge_url
                .set("ws://other-host:3001".into()),
            _ => {}
        }
        // Exercise the synchronous guard before reactive invalidation also runs.
        send.send(Ok(prepared)).unwrap();
        settle().await;
        if boundary == "success" || boundary == "projectless" {
            assert_eq!(fake.plugin_records.borrow().len(), 1);
            if boundary == "projectless" {
                assert_eq!(fake.plugin_requests.borrow()[0].0, None);
            }
            assert_eq!(plugins.installations.get_untracked().len(), 1);
            assert!(mounted.root.text_content().unwrap().contains("PR Review"));
        } else {
            assert!(fake.plugin_records.borrow().is_empty());
            assert!(plugins.installations.get_untracked().is_empty());
        }
        assert!(!plugins.busy.get_untracked());
    }
}

#[wasm_bindgen_test]
async fn plugins_discard_an_old_accounts_installation_list() {
    let fake = Rc::new(FakeBackend::default());
    let (send, receive) = futures::channel::oneshot::channel();
    fake.plugin_loads.borrow_mut().push_back(receive);
    let captured = Rc::new(Cell::new(None));
    let slot = captured.clone();
    let mounted = mount_test_with_backend(fake, move |state| {
        state.auth.set_user(User {
            id: UserId::new(1),
            username: "test".into(),
            role: UserRole::User,
            created_at: 0,
        });
        let plugins = PluginsState::default();
        let host = ProjectHost::new(state.api, state.projects, state.settings, state.auth);
        ProjectPluginActions::new(
            state.api,
            plugins,
            host,
            state.auth,
            state.projects,
            state.chat,
            state.settings,
        );
        slot.set(Some(plugins));
        view! {<div/>}
    });
    settle().await;
    mounted.state.auth.logout();
    let entries = openwebide_core::plugins::record_installation(
        Vec::new(),
        &openwebide_core::plugins::RecordPlugin {
            approved_capabilities: Vec::new(),
            update_policy: None,
            package: None,
            prepared: receipt(),
            revision: None,
        },
        1,
    )
    .unwrap();
    send.send(Ok(entries)).unwrap();
    settle().await;
    let plugins = captured.get().unwrap();
    assert!(plugins.installations.get_untracked().is_empty());
    assert!(!plugins.busy.get_untracked());
}

#[wasm_bindgen_test]
async fn plugins_marketplace_install_enables_by_default_and_uninstall_restores_available() {
    use openwebide_core::plugins::testing::{catalog, package};
    let fake = Rc::new(FakeBackend::default());
    let mut catalog = catalog();
    let mut newer = catalog.catalog.plugins[0].releases[0].clone();
    newer.version = "0.2.0".into();
    newer.source.commit = "d".repeat(40);
    catalog.catalog.plugins[0].releases.push(newer);
    *fake.marketplaces.borrow_mut() = openwebide_core::plugins::marketplace::MarketplaceSettings {
        revision: 1,
        sources: vec![catalog.source.clone()],
        catalogs: vec![catalog.clone()],
    };
    let (send, receive) = futures::channel::oneshot::channel();
    send.send(Ok(package().prepared)).unwrap();
    fake.plugin_preparations.borrow_mut().push_back(receive);
    let (send, receive) = futures::channel::oneshot::channel();
    send.send(Ok(package())).unwrap();
    fake.plugin_packages.borrow_mut().push_back(receive);
    let captured = Rc::new(Cell::new(None));
    let slot = captured.clone();
    let mounted = mount_test_with_backend(fake.clone(), move |state| {
        state.seed_project();
        state.auth.set_user(User {
            id: UserId::new(1),
            username: "test".into(),
            role: UserRole::User,
            created_at: 0,
        });
        let plugins = PluginsState::default();
        let host = ProjectHost::new(state.api, state.projects, state.settings, state.auth);
        let actions = ProjectPluginActions::new(
            state.api,
            plugins,
            host,
            state.auth,
            state.projects,
            state.chat,
            state.settings,
        );
        slot.set(Some((plugins, actions)));
        provide_context(plugins);
        provide_context(actions);
        view! {<openwebide_frontend::components::Plugins/>}
    });
    settle().await;
    let (plugins, _actions) = captured.get().unwrap();
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Test marketplace")
    );
    assert!(
        mounted
            .root
            .query_selector("input[aria-label='Plugin commit']")
            .unwrap()
            .is_none()
    );
    assert!(
        mounted
            .root
            .query_selector("button[aria-label='Plugin release']")
            .unwrap()
            .is_none()
    );
    mounted.click(".plugin-row-actions > button");
    settle().await;
    assert_eq!(
        fake.plugin_requests.borrow()[0].1.repository,
        "https://git.example.org/plugins.git"
    );
    assert!(fake.plugin_commands.borrow().is_empty());
    assert!(
        mounted
            .root
            .query_selector("button[aria-label='Available plugin actions']")
            .unwrap()
            .is_none()
    );
    mounted.click("button[aria-label='Installed plugin actions']");
    settle().await;
    mounted.click_text("Choose release");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector("button[aria-label='Plugin release']")
            .unwrap()
            .is_some()
    );
    assert!(plugins.project_plugins.get_untracked()[0].enabled);
    assert!(plugins.installations.get_untracked()[0].default_enabled);
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Enabled in this project")
    );
    assert!(plugins.error.get_untracked().is_none());
    let mut disabled = plugins.project_plugins.get_untracked()[0].clone();
    disabled.enabled = false;
    disabled.revision += 1;
    let (send, receive) = futures::channel::oneshot::channel();
    send.send(Ok(vec![disabled])).unwrap();
    fake.project_plugin_results.borrow_mut().push_back(receive);
    mounted.click_text("Disable");
    settle().await;
    assert!(matches!(
        fake.plugin_commands.borrow()[0].1,
        openwebide_core::plugins::ProjectPluginCommand::Disable { .. }
    ));
    mounted.click("button[aria-label='Installed plugin actions']");
    settle().await;
    mounted.click_text("Uninstall");
    settle().await;
    assert_eq!(fake.plugins.borrow().len(), 1);
    mounted.click_text("Confirm uninstall");
    settle().await;
    assert!(fake.plugins.borrow().is_empty());
    assert!(
        mounted
            .root
            .query_selector("button[aria-label='Available plugin actions']")
            .unwrap()
            .is_some()
    );
}

#[wasm_bindgen_test]
async fn plugins_reject_mismatched_catalog_identity_and_stale_activation_packages() {
    use openwebide_core::plugins::testing::{catalog, package};
    use openwebide_frontend::project_plugins::CatalogSelection;
    for boundary in ["mismatch", "project", "account", "host"] {
        let fake = Rc::new(FakeBackend::default());
        let catalog = catalog();
        *fake.marketplaces.borrow_mut() =
            openwebide_core::plugins::marketplace::MarketplaceSettings {
                revision: 1,
                sources: vec![catalog.source.clone()],
                catalogs: vec![catalog.clone()],
            };
        let captured = Rc::new(Cell::new(None));
        let slot = captured.clone();
        let mounted = mount_test_with_backend(fake.clone(), move |state| {
            state.seed_project();
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "test".into(),
                role: UserRole::User,
                created_at: 0,
            });
            let plugins = PluginsState::default();
            let host = ProjectHost::new(state.api, state.projects, state.settings, state.auth);
            let actions = ProjectPluginActions::new(
                state.api,
                plugins,
                host,
                state.auth,
                state.projects,
                state.chat,
                state.settings,
            );
            slot.set(Some((plugins, actions)));
            view! {<div/>}
        });
        settle().await;
        let (plugins, actions) = captured.get().unwrap();
        if boundary == "mismatch" {
            let (send, receive) = futures::channel::oneshot::channel();
            let mut prepared = package().prepared;
            prepared.manifest.version = "0.2.0".into();
            send.send(Ok(prepared)).unwrap();
            fake.plugin_preparations.borrow_mut().push_back(receive);
            let listing = &catalog.catalog.plugins[0];
            actions.install_release.run(CatalogSelection {
                marketplace: catalog.source,
                publisher: listing.publisher.clone(),
                name: listing.name.clone(),
                version: listing.releases[0].version.clone(),
            });
            settle().await;
            assert!(fake.plugin_records.borrow().is_empty());
            assert!(
                plugins
                    .error
                    .get_untracked()
                    .unwrap()
                    .contains("does not match")
            );
            continue;
        }
        let entries = openwebide_core::plugins::record_installation(
            Vec::new(),
            &openwebide_core::plugins::RecordPlugin {
                approved_capabilities: Vec::new(),
                update_policy: None,
                package: None,
                prepared: package().prepared,
                revision: None,
            },
            1,
        )
        .unwrap();
        *fake.plugins.borrow_mut() = entries.clone();
        plugins.installations.set(entries.clone());
        let (send, receive) = futures::channel::oneshot::channel();
        fake.plugin_packages.borrow_mut().push_back(receive);
        actions.enable.run(entries[0].clone());
        settle().await;
        match boundary {
            "project" => mounted.state.projects.active_project.set(None),
            "account" => mounted.state.auth.logout(),
            _ => mounted
                .state
                .settings
                .bridge_url
                .set("ws://other-host:3001".into()),
        }
        send.send(Ok(package())).unwrap();
        settle().await;
        assert!(fake.plugin_records.borrow().is_empty());
        assert!(fake.plugin_commands.borrow().is_empty());
        assert!(plugins.project_plugins.get_untracked().is_empty());
        assert!(!plugins.busy.get_untracked());
    }
}

#[wasm_bindgen_test]
async fn plugins_custom_marketplace_cache_failure_and_account_guard() {
    use openwebide_core::plugins::{marketplace::*, testing::catalog};
    let fake = Rc::new(FakeBackend::default());
    let captured = Rc::new(Cell::new(None));
    let slot = captured.clone();
    let mounted = mount_test_with_backend(fake.clone(), move |state| {
        state.auth.set_user(User {
            id: UserId::new(1),
            username: "test".into(),
            role: UserRole::User,
            created_at: 0,
        });
        let plugins = PluginsState::default();
        let host = ProjectHost::new(state.api, state.projects, state.settings, state.auth);
        let actions = ProjectPluginActions::new(
            state.api,
            plugins,
            host,
            state.auth,
            state.projects,
            state.chat,
            state.settings,
        );
        slot.set(Some((plugins, actions)));
        view! {<div/>}
    });
    settle().await;
    let (plugins, actions) = captured.get().unwrap();
    let cache = catalog();
    actions
        .save_sources
        .run(vec![MarketplaceSource::official(), cache.source.clone()]);
    settle().await;
    assert_eq!(fake.marketplaces.borrow().sources.len(), 2);
    let settings =
        cache_catalogs(fake.marketplaces.borrow().clone(), 1, vec![cache.clone()]).unwrap();
    let (send, receive) = futures::channel::oneshot::channel();
    send.send(Ok(MarketplaceRefresh {
        settings: settings.clone(),
        failures: vec![MarketplaceFailure {
            source: MarketplaceSource::official(),
            message: "Offline".into(),
        }],
    }))
    .unwrap();
    fake.marketplace_results.borrow_mut().push_back(receive);
    actions.refresh_catalogs.run(());
    settle().await;
    assert_eq!(
        plugins.marketplaces.get_untracked().catalogs,
        vec![cache.clone()]
    );
    assert_eq!(plugins.failures.get_untracked().len(), 1);
    let (send, receive) = futures::channel::oneshot::channel();
    fake.marketplace_results.borrow_mut().push_back(receive);
    actions.refresh_catalogs.run(());
    settle().await;
    mounted.state.auth.logout();
    send.send(Ok(MarketplaceRefresh {
        settings,
        failures: Vec::new(),
    }))
    .unwrap();
    settle().await;
    assert!(plugins.marketplaces.get_untracked().catalogs.is_empty());
    assert!(plugins.failures.get_untracked().is_empty());
    assert!(!plugins.busy.get_untracked());
}

#[wasm_bindgen_test]
async fn plugins_only_custom_marketplaces_can_be_removed_in_both_modes() {
    use openwebide_core::plugins::marketplace::*;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fake = Rc::new(FakeBackend::default());
        let custom = openwebide_core::plugins::testing::catalog().source;
        fake.marketplaces.borrow_mut().sources.push(custom);
        let mounted = mount_test_with_backend(fake, move |state| {
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
            view! {<openwebide_frontend::components::PluginMarketplaceSources/>}
        });
        settle().await;
        let text = mounted.root.text_content().unwrap();
        assert!(text.contains("Official marketplace · always available"));
        assert_eq!(text.matches("Remove marketplace").count(), 1);
        assert!(!text.contains("Restore official marketplace"));
        assert!(text.contains(&MarketplaceSource::official().repository));
    }
}

#[wasm_bindgen_test]
async fn plugins_status_bar_discovery_and_source_settings_navigation_in_both_modes() {
    use openwebide_frontend::components::{PluginsDialog, Settings, StatusBar};
    for mode in [
        Some(WorkspaceMode::Local),
        Some(WorkspaceMode::Remote),
        None,
    ] {
        let fake = Rc::new(FakeBackend::default());
        let catalog = openwebide_core::plugins::testing::catalog();
        fake.marketplaces
            .borrow_mut()
            .sources
            .push(catalog.source.clone());
        fake.marketplaces
            .borrow_mut()
            .catalogs
            .push(catalog.clone());
        let mut official = catalog;
        official.source = openwebide_core::plugins::marketplace::MarketplaceSource::official();
        official.catalog.name = "OpenWebIDE Official".into();
        fake.marketplaces.borrow_mut().catalogs.push(official);
        let mounted = mount_test_with_backend(fake, move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode.unwrap_or(WorkspaceMode::Remote));
            if mode.is_none() {
                state.projects.active_project.set(None);
            }
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "test".into(),
                role: UserRole::User,
                created_at: 0,
            });
            let ui = state.ui;
            let settings = state.settings;
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <StatusBar health=RwSignal::new(None).read_only() on_toggle_terminal=|| {} />
                <Show when=move || ui.plugins_open.get()><PluginsDialog/></Show>
                <Show when=move || settings.show_settings.get()>
                    <Settings on_set_theme=Callback::new(|_| ()) on_set_notifications=Callback::new(|_| ())
                        on_set_default_prompt=Callback::new(|_| ()) on_set_bridge_url=Callback::new(|_| ()) />
                </Show>
            }
        });
        settle().await;
        mounted.click("button[title='Browse and manage plugins']");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector("input[aria-label='Search plugins']")
                .unwrap()
                .is_some()
        );
        assert!(
            !mounted
                .root
                .text_content()
                .unwrap()
                .contains("Add marketplace")
        );
        assert!(mounted.root.text_content().unwrap().contains("Installed"));
        assert!(
            mounted
                .root
                .query_selector("[role='menu']")
                .unwrap()
                .is_none()
        );
        let text = mounted.root.text_content().unwrap();
        assert!(text.contains("Source: Test marketplace"));
        assert!(text.contains("Source: Open WebIDE"));
        assert!(!text.contains("OpenWebIDE Official"));
        assert!(!text.contains("Installed packages"));
        assert_eq!(
            mounted
                .root
                .query_selector_all(".plugin-entry")
                .unwrap()
                .length(),
            2
        );
        assert!(
            mounted
                .root
                .query_selector(".plugin-catalog-name")
                .unwrap()
                .is_none()
        );
        use wasm_bindgen::JsCast;
        let search: web_sys::HtmlInputElement = mounted
            .element("input[aria-label='Search plugins']")
            .unchecked_into();
        let change_query = |query: &str| {
            search.set_value(query);
            let event = web_sys::EventInit::new();
            event.set_bubbles(true);
            search
                .dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &event).unwrap())
                .unwrap();
        };
        change_query("no-such-plugin");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".plugin-row")
                .unwrap()
                .is_none()
        );
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("No available plugins match your search.")
        );
        change_query("");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".plugin-row")
                .unwrap()
                .is_some()
        );
        let input = mounted.element("input[aria-label='Search plugins']");
        let magnifier = mounted.element(".panel-search-row > .ui-icon-glyph");
        let menu = mounted.element("button[aria-label='Plugin actions']");
        let input_box = input.get_bounding_client_rect();
        let icon_box = magnifier.get_bounding_client_rect();
        let menu_box = menu.get_bounding_client_rect();
        assert!(icon_box.right() <= input_box.left());
        assert!(input_box.right() <= menu_box.left());
        assert!(
            (input_box.y() + input_box.height() / 2.0 - menu_box.y() - menu_box.height() / 2.0)
                .abs()
                < 1.0
        );
        mounted.click("button[aria-label='Plugin actions']");
        settle().await;
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Refresh installations")
        );
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Refresh marketplaces")
        );
        mounted.click_text("Install a pinned plugin manually");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector("input[aria-label='Plugin commit']")
                .unwrap()
                .is_some()
        );
        mounted.click_text("Cancel");
        settle().await;
        if mode.is_none() {
            assert!(
                !mounted
                    .element(".plugin-row-actions > button")
                    .has_attribute("disabled")
            );
        }
        mounted.click(".plugin-name");
        settle().await;
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("https://git.example.org/plugins.git")
        );
        assert!(
            mounted
                .root
                .query_selector("button[aria-label='Plugin release']")
                .unwrap()
                .is_none()
        );
        mounted.click(".plugin-name");
        settle().await;
        mounted.click("button[aria-label='Available plugin actions']");
        settle().await;
        mounted.click_text("Choose release");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector("button[aria-label='Plugin release']")
                .unwrap()
                .is_some()
        );
        let description = mounted
            .element(".plugin-description")
            .get_bounding_client_rect();
        let row_action = mounted
            .element(".plugin-row-actions > button")
            .get_bounding_client_rect();
        assert!(description.right() <= row_action.left());
        mounted.click("button[aria-label='Plugin actions']");
        settle().await;
        mounted.click_text("Manage marketplace sources");
        settle().await;
        assert!(!mounted.state.ui.plugins_open.get_untracked());
        assert_eq!(
            mounted
                .element("#settings-tab-plugins")
                .get_attribute("aria-selected")
                .as_deref(),
            Some("true")
        );
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Add marketplace")
        );
        assert!(!mounted.root.text_content().unwrap().contains("Installed"));
        assert!(
            mounted
                .root
                .query_selector("input[aria-label='Search plugins']")
                .unwrap()
                .is_none()
        );
        let browse = mounted.element("#settings-panel-plugins .ui-inline-actions button.btn.ghost");
        assert!(
            browse.get_bounding_client_rect().width()
                < browse
                    .parent_element()
                    .unwrap()
                    .get_bounding_client_rect()
                    .width()
        );
        mounted.click_text("Browse plugins");
        settle().await;
        assert!(mounted.state.ui.plugins_open.get_untracked());
        assert!(!mounted.state.settings.show_settings.get_untracked());
        mounted.state.auth.logout();
        mounted.state.auth.reset_user_state(
            mounted.state.projects,
            mounted.state.workspace,
            mounted.state.git,
            mounted.state.chat,
            mounted.state.settings,
            mounted.state.ui,
        );
        settle().await;
        assert!(!mounted.state.ui.plugins_open.get_untracked());
    }
}

#[wasm_bindgen_test]
async fn plugins_notify_updates_in_status_bar_and_update_all_from_the_same_facade() {
    use openwebide_core::plugins::{
        PluginUpdatePolicy, RecordPlugin,
        testing::{catalog, package},
    };
    use openwebide_frontend::components::{PluginsDialog, StatusBar};
    for (projectless, policy) in [
        (false, PluginUpdatePolicy::Notify),
        (true, PluginUpdatePolicy::Notify),
        (false, PluginUpdatePolicy::Automatic),
        (true, PluginUpdatePolicy::Automatic),
        (false, PluginUpdatePolicy::Off),
        (true, PluginUpdatePolicy::Off),
    ] {
        let fake = Rc::new(FakeBackend::default());
        let original = package();
        let mut updated = original.clone();
        updated.prepared.source.commit = "b".repeat(40);
        updated.prepared.digest = "d".repeat(64);
        updated.prepared.manifest.version = "0.1.1".into();
        let mut catalog = catalog();
        catalog.catalog.plugins[0].releases.push(
            openwebide_core::plugins::marketplace::CatalogRelease {
                version: "0.1.1".into(),
                source: openwebide_core::plugins::marketplace::CatalogReleaseSource {
                    commit: updated.prepared.source.commit.clone(),
                    path: updated.prepared.source.path.clone(),
                },
            },
        );
        fake.marketplaces
            .borrow_mut()
            .sources
            .push(catalog.source.clone());
        fake.marketplaces.borrow_mut().catalogs.push(catalog);
        *fake.plugins.borrow_mut() = openwebide_core::plugins::record_installation(
            Vec::new(),
            &RecordPlugin {
                approved_capabilities: Vec::new(),
                prepared: original.prepared.clone(),
                revision: None,
                package: Some(Box::new(original)),
                update_policy: Some(policy),
            },
            0,
        )
        .unwrap();
        if policy == PluginUpdatePolicy::Automatic {
            let (send, receive) = futures::channel::oneshot::channel();
            send.send(Ok(updated.prepared.clone())).unwrap();
            fake.plugin_preparations.borrow_mut().push_back(receive);
            let (send, receive) = futures::channel::oneshot::channel();
            send.send(Ok(updated.clone())).unwrap();
            fake.plugin_packages.borrow_mut().push_back(receive);
        }
        let mounted = mount_test_with_backend(fake.clone(), move |state| {
            state.seed_project();
            if projectless {
                state.projects.active_project.set(None);
            }
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "owner".into(),
                role: UserRole::User,
                created_at: 0,
            });
            let ui = state.ui;
            view! {<StatusBar health=RwSignal::new(None).read_only() on_toggle_terminal=||{}/><Show when=move ||ui.plugins_open.get()><PluginsDialog/></Show>}
        });
        settle().await;
        assert_eq!(fake.plugins.borrow()[0].update_policy, policy);
        if policy != PluginUpdatePolicy::Notify {
            assert_eq!(
                fake.plugins.borrow()[0].prepared.manifest.version,
                if policy == PluginUpdatePolicy::Automatic {
                    "0.1.1"
                } else {
                    "0.1.0"
                }
            );
            assert!(
                mounted
                    .root
                    .query_selector("[aria-label='1 plugin updates available']")
                    .unwrap()
                    .is_none()
            );
            continue;
        }
        assert!(fake.plugin_requests.borrow().is_empty());
        assert!(
            mounted
                .root
                .query_selector("[aria-label='1 plugin updates available']")
                .unwrap()
                .is_some()
        );
        mounted.click("button[title='Browse and manage plugins']");
        settle().await;
        mounted.click(".plugin-section .ui-disclosure-header .ui-disclosure-toggle");
        settle().await;
        assert_eq!(
            mounted
                .root
                .query_selector(".plugin-section .ui-disclosure-header .ui-disclosure-toggle")
                .unwrap()
                .unwrap()
                .get_attribute("aria-expanded")
                .as_deref(),
            Some("false")
        );
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(updated.prepared.clone())).unwrap();
        fake.plugin_preparations.borrow_mut().push_back(receive);
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(updated)).unwrap();
        fake.plugin_packages.borrow_mut().push_back(receive);
        mounted.click_text("Update all (1)");
        settle().await;
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .find("Update all (")
                .is_none()
        );
        assert_eq!(
            mounted
                .root
                .query_selector(".plugin-section .ui-disclosure-header .ui-disclosure-toggle")
                .unwrap()
                .unwrap()
                .get_attribute("aria-expanded")
                .as_deref(),
            Some("false")
        );
        assert_eq!(fake.plugins.borrow()[0].prepared.manifest.version, "0.1.1");
        assert_eq!(
            fake.plugin_requests.borrow()[0].0,
            if projectless { None } else { Some(1) }
        );
        assert!(
            mounted
                .root
                .query_selector("[aria-label='1 plugin updates available']")
                .unwrap()
                .is_none()
        );
    }
}
