use super::support::{mount_test, settle};
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
    settle().await;
    assert_eq!(field.value(), "");
    assert_eq!(
        mounted.state.fake.host_inputs.borrow().as_slice(),
        &[(1, 1, "secret-test-reply\n".into())]
    );
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
    assert!(mounted.state.fake.calls.borrow().is_empty());
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
    let host = mounted
        .element("[aria-label='Administration SSH host']")
        .unchecked_into::<web_sys::HtmlInputElement>();
    assert_eq!(host.value(), "admin@homelab");
    assert!(host.get_bounding_client_rect().width() <= 336.0);
    mounted.click_text("Save host connection");
    settle().await;
    assert_eq!(mounted.state.fake.host_connection.borrow().revision, 5);
}

#[wasm_bindgen_test]
async fn host_inspection_is_closed_by_default_and_only_polls_while_open() {
    let mounted = mount_test(move |state| {
        state.chat.active_session.set(Some(1));
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
async fn phone_host_flyout_retains_a_visible_touch_target_and_icon() {
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
    let icon = mounted
        .element(".tui-host-status .ui-icon-glyph")
        .get_bounding_client_rect();
    assert!(button.height() >= 44.0);
    assert!(icon.width() > 0.0 && icon.height() > 0.0);
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
