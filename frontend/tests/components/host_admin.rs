use super::support::{mount_test, settle, wait_until};
use leptos::prelude::*;
use openwebide_core::{WorkspaceMode, host_admin::*};
use openwebide_frontend::{
    components::host_admin::{HostPanel, HostSettings},
    host_admin::HostState,
};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;

fn operation() -> HostOperation {
    HostOperation {
        id: 1,
        user_id: 1,
        session_id: 1,
        request_id: "operation".into(),
        plan: HostPlan {
            target: "actual-host".into(),
            title: "Update".into(),
            steps: vec![HostCommand {
                program: "true".into(),
                args: vec![],
                cwd: "/tmp".into(),
                elevated: false,
                interactive: true,
                timeout_seconds: 300,
            }],
            checks: vec![],
            expects_reboot: false,
        },
        boot_id: "boot".into(),
        connection_revision: 1,
        steps_started: 1,
        input_token: Some("private-terminal".into()),
        live_output: "Password: ".into(),
        state: OperationState::Running,
        outputs: vec![],
        detail: "Running".into(),
        created_at: 1,
        updated_at: 1,
    }
}

#[wasm_bindgen_test]
async fn host_replies_survive_output_polling_and_never_enter_chat() {
    let slot = Rc::new(Cell::new(None));
    let capture = slot.clone();
    let mounted = mount_test(move |state| {
        state.chat.active_session.set(Some(1));
        state.fake.host_connection.borrow_mut().username = "admin".into();
        state.auth.set_user(openwebide_core::User {
            id: openwebide_core::UserId::new(1),
            username: "admin".into(),
            role: openwebide_core::UserRole::Admin,
            created_at: 0,
        });
        let host = HostState::new(state.api, state.auth, state.chat, state.projects);
        provide_context(host);
        capture.set(Some(host));
        view! {<style>{include_str!("../../styles.css")}</style><HostPanel/>}
    });
    settle().await;
    let host = slot.get().unwrap();
    *mounted.state.fake.host_operations.borrow_mut() = vec![operation()];
    host.operations.set(vec![operation()]);
    host.open.set(true);
    settle().await;
    let field = mounted
        .element("[aria-label='Private host prompt reply']")
        .unchecked_into::<web_sys::HtmlInputElement>();
    field.set_value("secret-test-reply");
    field
        .dispatch_event(&web_sys::Event::new("input").unwrap())
        .unwrap();
    host.operations
        .update(|operations| operations[0].live_output.push_str("still waiting"));
    settle().await;
    assert_eq!(field.value(), "secret-test-reply");
    mounted.click_text("Send reply");
    wait_until("encrypted private reply delivered", || {
        !mounted.state.fake.host_inputs.borrow().is_empty()
    })
    .await;
    settle().await;
    assert_eq!(field.value(), "");
    assert_eq!(
        mounted.state.fake.host_inputs.borrow().as_slice(),
        &[(1, 1, "secret-test-reply\n".into())]
    );
    field.set_value("secret-test-reply");
    field
        .dispatch_event(&web_sys::Event::new("input").unwrap())
        .unwrap();
    mounted.click_text("Send reply");
    wait_until("second encrypted private reply delivered", || {
        mounted.state.fake.host_inputs.borrow().len() == 2
    })
    .await;
    {
        let replies = mounted.state.fake.host_encrypted_inputs.borrow();
        assert_ne!(replies[0].request_id, replies[1].request_id);
        assert_ne!(replies[0].encapsulated_key, replies[1].encapsulated_key);
        assert_ne!(replies[0].ciphertext, replies[1].ciphertext);
    }
    assert!(
        !mounted
            .state
            .chat
            .draft
            .get_untracked()
            .contains("secret-test-reply")
    );
    assert!(
        !mounted
            .root
            .text_content()
            .unwrap()
            .contains("secret-test-reply")
    );
    assert!(
        mounted
            .state
            .fake
            .host_encrypted_inputs
            .borrow()
            .iter()
            .all(|input| !serde_json::to_string(input)
                .unwrap()
                .contains("secret-test-reply"))
    );
    host.operations
        .update(|operations| operations[0].input_token = None);
    settle().await;
    assert!(
        mounted
            .root
            .query_selector("[aria-label='Private host prompt reply']")
            .unwrap()
            .is_none()
    );
}

#[wasm_bindgen_test]
async fn host_results_cannot_cross_project_session_or_account_transitions() {
    for transition in 0..4 {
        let slot = Rc::new(Cell::new(None));
        let capture = slot.clone();
        let mounted = mount_test(move |state| {
            state.chat.active_session.set(Some(1));
            let host = HostState::new(state.api, state.auth, state.chat, state.projects);
            capture.set(Some(host));
            view! {<div>"Host scope fixture"</div>}
        });
        settle().await;
        let host = slot.get().unwrap();
        let (sender, receiver) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .host_results
            .borrow_mut()
            .push_back(receiver);
        host.connection.set(Some(HostConnection {
            username: "admin".into(),
            ..Default::default()
        }));
        host.refresh(false);
        settle().await;
        assert!(host.busy.get_untracked());
        match transition {
            0 | 1 => {
                mounted.state.seed_project();
                mounted.state.projects.projects.update(|projects| {
                    projects[0].mode = if transition == 0 {
                        WorkspaceMode::Local
                    } else {
                        WorkspaceMode::Remote
                    }
                });
                mounted.state.projects.active_project.set(Some(1));
            }
            2 => mounted.state.chat.active_session.set(Some(2)),
            _ => mounted.state.auth.logout(),
        }
        settle().await;
        sender
            .send(Ok(HostResponse::Operations(vec![operation()])))
            .unwrap();
        settle().await;
        assert!(host.operations.get_untracked().is_empty());
        assert!(host.environment.get_untracked().is_none());
        assert!(!host.busy.get_untracked());
        if transition < 2 {
            let requests = mounted.state.fake.host_requests.borrow().len();
            host.refresh(true);
            settle().await;
            assert_eq!(mounted.state.fake.host_requests.borrow().len(), requests);
        }
    }
}

#[wasm_bindgen_test]
async fn host_settings_load_on_first_mount_and_fit_a_phone_width() {
    let mounted = mount_test(move |state| {
        state.fake.host_connection.borrow_mut().destination = "admin@homelab".into();
        state.fake.host_connection.borrow_mut().revision = 4;
        let host = HostState::new(state.api, state.auth, state.chat, state.projects);
        provide_context(host);
        view! {<style>{include_str!("../../styles.css")}</style><div style="width: 360px; max-width: 100%; padding: 12px; box-sizing: border-box;"><HostSettings/></div>}
    });
    settle().await;
    mounted.click_text("Advanced connection settings");
    settle().await;
    let host = mounted
        .element("[aria-label='Administration SSH host']")
        .unchecked_into::<web_sys::HtmlInputElement>();
    assert_eq!(host.value(), "admin@homelab");
    assert!(host.get_bounding_client_rect().width() <= 336.0);
    mounted.click_text("Save and test connection");
    settle().await;
    assert_eq!(mounted.state.fake.host_connection.borrow().revision, 5);
}

#[wasm_bindgen_test]
async fn host_save_and_test_reports_success_and_rejects_stale_environment() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for transition in 0..3 {
            let slot = Rc::new(Cell::new(None));
            let capture = slot.clone();
            let mounted = mount_test(move |state| {
                state.seed_project();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
                state.chat.active_session.set(Some(1));
                let host = HostState::new(state.api, state.auth, state.chat, state.projects);
                capture.set(Some(host));
                provide_context(host);
                view! { <HostSettings/> }
            });
            settle().await;
            let host = slot.get().unwrap();
            let (sender, receiver) = futures::channel::oneshot::channel();
            mounted
                .state
                .fake
                .host_probe_results
                .borrow_mut()
                .push_back(receiver);
            let connection = HostConnection {
                username: "admin".into(),
                ..host.connection.get_untracked().unwrap()
            };
            host.save_and_test_connection(connection);
            settle().await;
            assert_eq!(
                mounted.state.fake.host_connection.borrow().username,
                "admin"
            );
            assert!(host.connection_busy.get_untracked());
            if transition == 1 {
                mounted.state.chat.active_session.set(Some(2));
            }
            if transition == 2 {
                mounted.state.auth.generation.update(|value| *value += 1);
            }
            settle().await;
            sender
                .send(Ok(HostEnvironment {
                    target: "host".into(),
                    boot_id: "boot".into(),
                    info: openwebide_core::HostInfo {
                        os: Some("Linux".into()),
                        ..Default::default()
                    },
                    resources: Vec::new(),
                    relationships: Vec::new(),
                    notes: Vec::new(),
                }))
                .unwrap();
            settle().await;
            assert!(!host.connection_busy.get_untracked());
            if transition == 0 {
                assert!(host.environment.get_untracked().is_some());
                assert!(
                    mounted
                        .root
                        .text_content()
                        .unwrap()
                        .contains("Connected to host (Linux)")
                );
            } else {
                assert!(host.environment.get_untracked().is_none());
            }
        }
    }
}

#[wasm_bindgen_test]
async fn host_disable_is_saved_immediately_and_background_polling_does_not_disable_setup() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = Rc::new(Cell::new(None));
        let capture = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.fake.host_connection.borrow_mut().username = "admin".into();
            let host = HostState::new(state.api, state.auth, state.chat, state.projects);
            capture.set(Some(host));
            provide_context(host);
            view! { <HostSettings/> }
        });
        settle().await;
        let host = slot.get().unwrap();
        host.busy.set(true);
        settle().await;
        let save = mounted
            .root
            .query_selector(".btn")
            .unwrap()
            .unwrap()
            .unchecked_into::<web_sys::HtmlButtonElement>();
        assert!(!save.disabled());
        let toggle = mounted
            .element("input[type=checkbox]")
            .unchecked_into::<web_sys::HtmlInputElement>();
        toggle.set_checked(false);
        toggle
            .dispatch_event(&web_sys::Event::new("change").unwrap())
            .unwrap();
        settle().await;
        assert!(!mounted.state.fake.host_connection.borrow().enabled);
        assert!(!host.connection.get_untracked().unwrap().enabled);
        host.load_connection();
        settle().await;
        assert!(!toggle.checked());
    }
}

#[wasm_bindgen_test]
async fn host_inspection_is_closed_by_default_and_only_polls_while_open() {
    let mounted = mount_test(move |state| {
        state.chat.active_session.set(Some(1));
        state.fake.host_connection.borrow_mut().username = "admin".into();
        provide_context(HostState::new(
            state.api,
            state.auth,
            state.chat,
            state.projects,
        ));
        view! { <style>{include_str!("../../styles.css")}</style><HostPanel /> }
    });
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".host-panel")
            .unwrap()
            .is_none()
    );
    openwebide_frontend::util::sleep_ms(2100).await;
    assert!(mounted.state.fake.host_requests.borrow().is_empty());
    mounted.click("[aria-label='Host environment']");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".host-panel")
            .unwrap()
            .is_some()
    );
    assert_eq!(mounted.state.fake.host_requests.borrow().len(), 2);
    openwebide_frontend::util::sleep_ms(2100).await;
    assert!(mounted.state.fake.host_requests.borrow().len() > 2);
    mounted.click("[aria-label='Host environment']");
    settle().await;
    let requests = mounted.state.fake.host_requests.borrow().len();
    openwebide_frontend::util::sleep_ms(2100).await;
    assert_eq!(mounted.state.fake.host_requests.borrow().len(), requests);
    assert!(
        mounted
            .root
            .query_selector(".host-panel")
            .unwrap()
            .is_none()
    );
}

#[wasm_bindgen_test]
async fn phone_host_flyout_retains_a_visible_text_touch_target() {
    let mounted = mount_test(move |state| {
        provide_context(HostState::new(
            state.api,
            state.auth,
            state.chat,
            state.projects,
        ));
        view! {
            <style>{include_str!("../../styles.css")}</style>
            <div class="phone-layout" style="width: 360px;"><div class="tui-statusline"><HostPanel /></div></div>
        }
    });
    settle().await;
    let button = mounted
        .element("button[aria-label='Host environment']")
        .get_bounding_client_rect();
    assert!(button.height() >= 44.0);
    assert!(
        mounted
            .root
            .query_selector(".tui-host-status .ui-icon-glyph")
            .unwrap()
            .is_none()
    );
    assert!(
        mounted
            .element(".tui-host-status .ui-dropdown-label")
            .text_content()
            .unwrap()
            .contains("Host")
    );
    mounted.click("button[aria-label='Host environment']");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".host-panel")
            .unwrap()
            .is_some()
    );
}

#[wasm_bindgen_test]
async fn host_settings_default_address_and_advanced_overrides_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            provide_context(HostState::new(
                state.api,
                state.auth,
                state.chat,
                state.projects,
            ));
            view! { <style>{include_str!("../../styles.css")}</style><HostSettings/> }
        });
        settle().await;
        let toggle = mounted.element(".ui-disclosure-toggle");
        assert_eq!(
            toggle.get_attribute("aria-expanded").as_deref(),
            Some("false")
        );
        let username = mounted
            .element("[aria-label='Administration SSH username']")
            .unchecked_into::<web_sys::HtmlInputElement>();
        username.set_value("admin");
        username
            .dispatch_event(&web_sys::Event::new("input").unwrap())
            .unwrap();
        settle().await;
        mounted.click_text("Save and test connection");
        settle().await;
        {
            let saved = mounted.state.fake.host_connection.borrow();
            assert_eq!(saved.username, "admin");
            assert!(saved.destination.is_empty());
            assert_eq!(saved.port, 22);
            assert!(saved.is_configured());
        }
        mounted.click_text("Advanced connection settings");
        settle().await;
        assert_eq!(
            toggle.get_attribute("aria-expanded").as_deref(),
            Some("true")
        );
        let address = mounted
            .element("[aria-label='Administration SSH host']")
            .unchecked_into::<web_sys::HtmlInputElement>();
        address.set_value("other-host");
        address
            .dispatch_event(&web_sys::Event::new("input").unwrap())
            .unwrap();
        settle().await;
        mounted.click_text("Save and test connection");
        settle().await;
        assert_eq!(
            mounted.state.fake.host_connection.borrow().destination,
            "other-host"
        );
        mounted.click_text("Advanced connection settings");
        settle().await;
        assert_eq!(
            toggle.get_attribute("aria-expanded").as_deref(),
            Some("false")
        );
    }
}

#[wasm_bindgen_test]
async fn unconfigured_host_offers_settings_without_polling() {
    let mounted = mount_test(move |state| {
        state.chat.active_session.set(Some(1));
        provide_context(HostState::new(
            state.api,
            state.auth,
            state.chat,
            state.projects,
        ));
        view! { <style>{include_str!("../../styles.css")}</style><HostPanel/> }
    });
    settle().await;
    mounted.click("[aria-label='Host environment']");
    settle().await;
    assert!(mounted.state.fake.host_requests.borrow().is_empty());
    assert!(
        mounted
            .root
            .query_selector("[aria-label='Filter host resources']")
            .unwrap()
            .is_none()
    );
    mounted.click_text("Open host settings");
    settle().await;
    assert!(mounted.state.settings.show_settings.get_untracked());
    assert_eq!(mounted.state.settings.requested_tab.get_untracked(), 4);
}

#[wasm_bindgen_test]
async fn private_reply_key_failures_and_scope_changes_never_send_plaintext() {
    for change in 0..6 {
        let slot = Rc::new(Cell::new(None));
        let capture = slot.clone();
        let mounted = mount_test(move |state| {
            state.chat.active_session.set(Some(1));
            state.auth.set_user(openwebide_core::User {
                id: openwebide_core::UserId::new(1),
                username: "admin".into(),
                role: openwebide_core::UserRole::Admin,
                created_at: 0,
            });
            let host = HostState::new(state.api, state.auth, state.chat, state.projects);
            host.operations.set(vec![operation()]);
            capture.set(Some(host));
            view! { <div>"private reply fixture"</div> }
        });
        settle().await;
        let host = slot.get().unwrap();
        let (sender, receiver) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .host_input_key_results
            .borrow_mut()
            .push_back(receiver);
        host.reply(1, "private-terminal".into(), "never-forward-this".into());
        settle().await;
        match change {
            0 => mounted.state.auth.logout(),
            1 => mounted.state.chat.active_session.set(Some(2)),
            2 => {
                mounted.state.seed_project();
                mounted.state.projects.active_project.set(Some(1));
            }
            3 => host
                .operations
                .update(|operations| operations[0].input_token = Some("replacement".into())),
            _ => {}
        }
        settle().await;
        let prompt = HostInputKeyRequest {
            operation: 1,
            token: "private-terminal".into(),
        };
        let mut key = private_input::PrivateInputRecipient::new(1, 1, prompt, [7; 32]).public_key();
        if change == 5 {
            key.session = 2;
        }
        sender
            .send(if change == 4 {
                Err("Encryption unavailable".into())
            } else {
                Ok(key)
            })
            .unwrap();
        settle().await;
        assert!(mounted.state.fake.host_encrypted_inputs.borrow().is_empty());
        assert!(mounted.state.fake.host_inputs.borrow().is_empty());
    }
}

#[wasm_bindgen_test]
fn private_reply_crypto_contract_in_browser() {
    private_input::testing::assert_private_reply_contract();
}

#[wasm_bindgen_test]
async fn host_resource_clicks_expand_details_and_ask_reveals_the_draft_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = Rc::new(Cell::new(None));
        let capture = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.active_project.set(None);
            state.chat.active_session.set(Some(1));
            state.fake.host_connection.borrow_mut().username = "admin".into();
            let host = HostState::new(state.api, state.auth, state.chat, state.projects);
            provide_context(host);
            capture.set(Some(host));
            view! { <style>{include_str!("../../styles.css")}</style><HostPanel/> }
        });
        settle().await;
        let host = slot.get().unwrap();
        host.environment.set(Some(HostEnvironment {
            target: "host".into(),
            boot_id: "boot".into(),
            info: openwebide_core::HostInfo::default(),
            resources: vec![
                HostResource {
                    id: "process:123".into(),
                    kind: ResourceKind::Process,
                    name: "webide".into(),
                    status: "running".into(),
                    detail: "PID 123; memory 1024 bytes".into(),
                },
                HostResource {
                    id: "port:3000".into(),
                    kind: ResourceKind::Port,
                    name: "TCP 3000".into(),
                    status: "listening".into(),
                    detail: "port".into(),
                },
            ],
            relationships: vec![HostRelationship {
                from: "port:3000".into(),
                to: "process:123".into(),
                description: "owned by".into(),
            }],
            notes: Vec::new(),
        }));
        host.open.set(true);
        settle().await;
        let menu = mounted.element(".tui-host-menu");
        let viewport = f64::from(
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .document_element()
                .unwrap()
                .client_width(),
        );
        assert!(
            (menu.get_bounding_client_rect().width() - 640_f64.min(viewport - 16.0)).abs() < 1.0
        );
        assert!(menu.get_bounding_client_rect().height() > 300.0);
        assert_eq!(
            web_sys::window()
                .unwrap()
                .get_computed_style(&mounted.element(".host-resources"))
                .unwrap()
                .unwrap()
                .get_property_value("overflow-y")
                .unwrap(),
            "visible"
        );
        let resource = mounted.element(".host-resource");
        assert!(!resource.has_attribute("open"));
        mounted.click(".host-resource > summary");
        settle().await;
        assert!(resource.has_attribute("open"));
        let text = resource.text_content().unwrap();
        assert!(text.contains("PID 123"));
        assert!(text.contains("TCP 3000 (port:3000) → webide (process:123)"));
        mounted.click(".host-resource > summary");
        settle().await;
        assert!(!resource.has_attribute("open"));
        mounted.click(".host-resource > summary");
        settle().await;
        mounted.click_text("Ask about this");
        settle().await;
        assert!(!host.open.get_untracked());
        assert!(
            mounted
                .state
                .chat
                .draft
                .get_untracked()
                .contains("process:123")
        );
        assert!(
            mounted
                .state
                .fake
                .host_requests
                .borrow()
                .iter()
                .all(|(_, request)| matches!(
                    request,
                    HostRequest::Inspect | HostRequest::Operations
                ))
        );
    }
}

#[wasm_bindgen_test]
async fn configured_host_refresh_retains_geometry_nodes_and_expanded_resources() {
    let slot = Rc::new(Cell::new(None));
    let capture = slot.clone();
    let mounted = mount_test(move |state| {
        state.chat.active_session.set(Some(1));
        state.fake.host_connection.borrow_mut().username = "admin".into();
        let host = HostState::new(state.api, state.auth, state.chat, state.projects);
        provide_context(host);
        capture.set(Some(host));
        view! { <style>{include_str!("../../styles.css")}</style><HostPanel/> }
    });
    settle().await;
    let host = slot.get().unwrap();
    let mut environment = HostEnvironment {
        target: "host".into(),
        boot_id: "boot".into(),
        info: openwebide_core::HostInfo::default(),
        resources: vec![HostResource {
            id: "service".into(),
            kind: ResourceKind::Service,
            name: "Service".into(),
            status: "running".into(),
            detail: "details".into(),
        }],
        relationships: vec![],
        notes: vec![],
    };
    host.environment.set(Some(environment.clone()));
    host.open.set(true);
    settle().await;
    let panel = mounted.element(".host-panel");
    let resource = mounted.element(".host-resource");
    resource.set_attribute("open", "").unwrap();
    let summary = mounted.element(".host-summary");
    let height = panel.get_bounding_client_rect().height();
    let refresh = mounted.element("[aria-label='Refresh host environment']");
    assert_eq!(refresh.text_content().unwrap().trim(), "");
    for outcome in [true, false] {
        let (sender, receiver) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .host_results
            .borrow_mut()
            .push_back(receiver);
        host.refresh(true);
        settle().await;
        assert!((panel.get_bounding_client_rect().height() - height).abs() < 0.01);
        environment.info.ram_available_bytes += 1024;
        environment.notes.push("Collector note".into());
        sender
            .send(if outcome {
                Ok(HostResponse::Environment(environment.clone()))
            } else {
                Err("Refresh unavailable".into())
            })
            .unwrap();
        settle().await;
        assert!((panel.get_bounding_client_rect().height() - height).abs() < 0.01);
        assert!(summary.is_same_node(Some(&mounted.element(".host-summary"))));
        assert!(resource.is_same_node(Some(&mounted.element(".host-resource"))));
        assert!(resource.has_attribute("open"));
        assert!(host.environment.get_untracked().is_some());
    }
}

#[wasm_bindgen_test]
async fn host_updates_use_the_same_settings_and_actions_in_both_project_modes() {
    use openwebide_core::host_admin::updates::*;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = Rc::new(Cell::new(None));
        let capture = slot.clone();
        let settings = UpdateSettings {
            deployment: Deployment::Quadlet {
                image_file: "/home/test/.config/containers/systemd/openwebide.image".into(),
                service: "openwebide.service".into(),
            },
            revision: 1,
            ..UpdateSettings::default()
        };
        let running = Build {
            version: "1.0.0-alpha.6".into(),
            commit: "a".repeat(40),
            schema: 58,
        };
        let candidate = Candidate {
            digest: format!("sha256:{}", "b".repeat(64)),
            architecture: "amd64".into(),
            build: Build {
                commit: "c".repeat(40),
                ..running.clone()
            },
        };
        let initial = UpdateResponse::Settings {
            settings: settings.clone(),
            running: running.clone(),
            job: None,
        };
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            let (sender, receiver) = futures::channel::oneshot::channel();
            sender.send(Ok(initial)).unwrap();
            state
                .fake
                .host_update_results
                .borrow_mut()
                .push_back(receiver);
            let host = HostState::new(state.api, state.auth, state.chat, state.projects);
            capture.set(Some(host));
            provide_context(host);
            view! { <openwebide_frontend::components::host_updates::HostUpdates/> }
        });
        settle().await;
        let host = slot.get().unwrap();
        assert_eq!(host.updates.settings.get_untracked(), settings);
        let text = mounted.root.text_content().unwrap();
        assert!(text.contains("Nightly") && !text.contains("Alpha"));
        let check = UpdateCheck {
            connection_revision: 0,
            running,
            settings: settings.clone(),
            candidate: candidate.clone(),
            available: true,
        };
        let (sender, receiver) = futures::channel::oneshot::channel();
        sender.send(Ok(UpdateResponse::Check { check })).unwrap();
        mounted
            .state
            .fake
            .host_update_results
            .borrow_mut()
            .push_back(receiver);
        host.updates.request(UpdateRequest::Check);
        settle().await;
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Available: 1.0.0-alpha.6")
        );
        let button = mounted.root.query_selector_all("button").unwrap();
        let install = (0..button.length())
            .filter_map(|index| button.item(index))
            .find(|button| {
                button
                    .text_content()
                    .is_some_and(|text| text.contains("Install release"))
            })
            .unwrap();
        install.unchecked_ref::<web_sys::HtmlElement>().click();
        settle().await;
        assert!(mounted.state.fake.host_update_requests.borrow().iter().any(|request|matches!(request,
            UpdateRequest::Start{candidate: requested,settings_revision:1,..} if requested==&candidate)));
        mounted.state.chat.active_session.set(Some(99));
        settle().await;
        assert_eq!(host.updates.settings.get_untracked(), settings);
    }
}

#[wasm_bindgen_test]
async fn host_installation_discovery_populates_the_same_draft_in_both_modes_and_retains_manual_fallback()
 {
    use openwebide_core::host_admin::updates::*;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let deployment = Deployment::Compose {
            directory: "/srv/ide".into(),
            files: vec!["/srv/ide/app.yml".into(), "/srv/ide/ssh.yml".into()],
            service: "app".into(),
        };
        let detected = deployment.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            for result in [
                Ok(UpdateResponse::Settings {
                    settings: UpdateSettings::default(),
                    running: Build {
                        version: "test".into(),
                        commit: "test".into(),
                        schema: 1,
                    },
                    job: None,
                }),
                Ok(UpdateResponse::Discovered {
                    deployment: detected.clone(),
                }),
            ] {
                let (sender, receiver) = futures::channel::oneshot::channel();
                sender.send(result).unwrap();
                state
                    .fake
                    .host_update_results
                    .borrow_mut()
                    .push_back(receiver);
            }
            provide_context(HostState::new(
                state.api,
                state.auth,
                state.chat,
                state.projects,
            ));
            view! { <openwebide_frontend::components::host_updates::HostUpdates/> }
        });
        settle().await;
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Docker Compose installation: /srv/ide")
        );
        assert!(
            mounted
                .state
                .fake
                .host_update_requests
                .borrow()
                .iter()
                .any(|request| matches!(request, UpdateRequest::Discover))
        );
        mounted.click_text("Save update settings");
        settle().await;
        assert!(mounted.state.fake.host_update_requests.borrow().iter().any(|request| matches!(request, UpdateRequest::Save { settings } if settings.deployment == deployment)));
        mounted.click_text("Detect installation");
        settle().await;
        // A failed retry retains the discovered draft and manual override.
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Docker Compose installation: /srv/ide")
        );
        mounted.click_text("Advanced deployment settings");
        settle().await;
        let directory = mounted
            .element("[aria-label='Update installation directory']")
            .unchecked_into::<web_sys::HtmlInputElement>();
        assert_eq!(directory.value(), "/srv/ide");
    }
}

#[wasm_bindgen_test]
async fn host_update_results_are_discarded_after_account_changes() {
    use openwebide_core::host_admin::updates::*;
    let slot = Rc::new(Cell::new(None));
    let capture = slot.clone();
    let mounted = mount_test(move |state| {
        let host = HostState::new(state.api, state.auth, state.chat, state.projects);
        capture.set(Some(host));
        view! {<div>"Host update scope"</div>}
    });
    settle().await;
    let host = slot.get().unwrap();
    let (sender, receiver) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .host_update_results
        .borrow_mut()
        .push_back(receiver);
    host.updates.request(UpdateRequest::Settings);
    settle().await;
    mounted.state.auth.generation.update(|epoch| *epoch += 1);
    settle().await;
    sender
        .send(Ok(UpdateResponse::Settings {
            settings: UpdateSettings {
                revision: 44,
                ..UpdateSettings::default()
            },
            running: Build {
                version: "1.0.0".into(),
                commit: "a".repeat(40),
                schema: 58,
            },
            job: None,
        }))
        .unwrap();
    settle().await;
    assert_eq!(host.updates.settings.get_untracked().revision, 0);
    assert!(host.updates.running.get_untracked().is_none());
    assert!(!host.updates.busy.get_untracked());
}

#[wasm_bindgen_test]
async fn lost_host_update_acknowledgement_keeps_tracking_the_durable_claim() {
    use openwebide_core::host_admin::updates::*;
    let slot = Rc::new(Cell::new(None));
    let capture = slot.clone();
    let mounted = mount_test(move |state| {
        let host = HostState::new(state.api, state.auth, state.chat, state.projects);
        capture.set(Some(host));
        view! {<div>"Update reconnect fixture"</div>}
    });
    settle().await;
    let updates = slot.get().unwrap().updates;
    let candidate = Candidate {
        digest: format!("sha256:{}", "b".repeat(64)),
        architecture: "amd64".into(),
        build: Build {
            version: "1.0.0-alpha.6".into(),
            commit: "c".repeat(40),
            schema: 58,
        },
    };
    let (sender, receiver) = futures::channel::oneshot::channel();
    sender
        .send(Err("Connection closed during restart".into()))
        .unwrap();
    mounted
        .state
        .fake
        .host_update_results
        .borrow_mut()
        .push_back(receiver);
    updates.request(UpdateRequest::Start {
        connection_revision: 0,
        candidate: candidate.clone(),
        settings_revision: 0,
    });
    settle().await;
    assert!(updates.tracking.get_untracked());
    let job = UpdateJob {
        id: "a".repeat(32),
        candidate: candidate.clone(),
        phase: UpdatePhase::Succeeded,
        detail: "The selected build is running.".into(),
        settings_revision: 0,
        connection_revision: 0,
        created_at: 1,
    };
    let (sender, receiver) = futures::channel::oneshot::channel();
    sender
        .send(Ok(UpdateResponse::Status {
            job: Some(job),
            running: candidate.build,
        }))
        .unwrap();
    mounted
        .state
        .fake
        .host_update_results
        .borrow_mut()
        .push_back(receiver);
    updates.request(UpdateRequest::Status);
    settle().await;
    assert!(!updates.tracking.get_untracked());
    assert!(updates.error.get_untracked().is_none());
    assert_eq!(
        updates.job.get_untracked().unwrap().phase,
        UpdatePhase::Succeeded
    );
}
