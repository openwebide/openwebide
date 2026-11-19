use leptos::prelude::*;
use openwebide_core::ModelInfo;
use openwebide_core::RunEvent;
use openwebide_frontend::testing::fake_backend::Call;
use wasm_bindgen_test::*;

use super::support::{chat_view, mount_test, settle};

#[wasm_bindgen_test]
async fn model_dropdown_uses_default_and_sends_selected_model() {
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        *state.fake.models.borrow_mut() = vec![
            ModelInfo {
                name: "qwen3:8b".into(),
            },
            ModelInfo {
                name: "llama3".into(),
            },
        ];
        state
            .fake
            .scripted_events
            .borrow_mut()
            .push_back(vec![RunEvent::Delta {
                content: "answer".into(),
            }]);
        chat_view(state)
    });
    settle().await;
    assert_eq!(
        mounted
            .element(".tui-model-name")
            .text_content()
            .unwrap()
            .trim(),
        "qwen3:8b"
    );
    mounted.click(".tui-model-name");
    settle().await;
    let menu = mounted.element(".recent-menu").text_content().unwrap();
    assert!(!menu.contains("Default model"), "{menu}");
    for name in ["qwen3:8b", "llama3"] {
        assert!(menu.contains(name), "{menu}");
    }
    mounted.click_text("llama3");
    settle().await;
    assert_eq!(
        mounted
            .state
            .chat
            .session_model
            .get_untracked()
            .get(&1)
            .cloned()
            .flatten()
            .as_deref(),
        Some("llama3")
    );
    mounted.input("hello");
    mounted.key("Enter", "Enter", false);
    settle().await;
    assert!(
        mounted
            .state
            .fake
            .calls
            .borrow()
            .contains(&Call::SendMessage {
                session: 1,
                content: "hello".into(),
                model: Some("llama3".into())
            })
    );
    assert!(mounted.root.text_content().unwrap().contains("answer"));
}

#[wasm_bindgen_test]
async fn model_choice_carries_into_auto_created_session() {
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_connection();
        *state.fake.models.borrow_mut() = vec![ModelInfo {
            name: "llama3".into(),
        }];
        chat_view(state)
    });
    settle().await;
    mounted.click(".tui-model-name");
    settle().await;
    mounted.click_text("llama3");
    settle().await;
    mounted.input("new session");
    mounted.key("Enter", "Enter", false);
    settle().await;
    let session = mounted.state.chat.active_session.get_untracked().unwrap();
    assert_eq!(
        mounted
            .state
            .chat
            .session_model
            .get_untracked()
            .get(&session)
            .cloned()
            .flatten()
            .as_deref(),
        Some("llama3")
    );
    assert_eq!(mounted.state.fake.sessions.borrow().len(), 1);
    assert!(
        mounted
            .state
            .fake
            .calls
            .borrow()
            .contains(&Call::SendMessage {
                session,
                content: "new session".into(),
                model: Some("llama3".into())
            })
    );
}

#[wasm_bindgen_test]
async fn model_request_measurement() {
    let mounted = mount_test(|state| {
        state.seed_connection();
        state.seed_session();
        let mut second = state.chat.sessions.get_untracked()[0].clone();
        second.id = 2;
        state.chat.sessions.update(|sessions| sessions.push(second));
        chat_view(state)
    });
    settle().await;
    mounted.state.fake.calls.borrow_mut().clear();
    mounted.state.fake.model_requests.borrow_mut().clear();
    mounted.state.fake.context_requests.borrow_mut().clear();
    for i in 0..10 {
        mounted
            .state
            .chat
            .active_session
            .set(Some(if i % 2 == 0 { 2 } else { 1 }));
        settle().await;
        mounted
            .state
            .chat
            .sessions
            .update(|sessions| sessions[0].name = format!("rename {i}"));
        settle().await;
    }
    assert!(mounted.state.fake.model_requests.borrow().is_empty());
    assert!(mounted.state.fake.context_requests.borrow().is_empty());
    wasm_bindgen_test::console_log!(
        "ten switches + renames: models={} context={}",
        mounted.state.fake.model_requests.borrow().len(),
        mounted.state.fake.context_requests.borrow().len()
    );
}

#[wasm_bindgen_test]
async fn request_identity_ignores_names_but_tracks_connection_configuration() {
    let mounted = mount_test(|state| {
        state.seed_connection();
        state.seed_session();
        chat_view(state)
    });
    settle().await;
    mounted.state.fake.model_requests.borrow_mut().clear();
    mounted.state.fake.context_requests.borrow_mut().clear();
    mounted
        .state
        .settings
        .connections
        .update(|connections| connections[0].name = "renamed".into());
    settle().await;
    assert!(mounted.state.fake.model_requests.borrow().is_empty());
    assert!(mounted.state.fake.context_requests.borrow().is_empty());
    for change in 0..3 {
        mounted
            .state
            .settings
            .connections
            .update(|connections| match change {
                0 => connections[0].base_url = "http://changed".into(),
                1 => connections[0].kind = openwebide_core::ProviderKind::LlamaCpp,
                _ => connections[0].enabled = false,
            });
        settle().await;
    }
    assert_eq!(mounted.state.fake.model_requests.borrow().len(), 3);
    assert_eq!(mounted.state.fake.context_requests.borrow().len(), 3);
    mounted
        .state
        .settings
        .connections
        .update(|connections| connections[0].context_limit = Some(16384));
    settle().await;
    mounted
        .state
        .settings
        .connections
        .update(|connections| connections[0].model = Some("new default".into()));
    settle().await;
    mounted
        .state
        .chat
        .selected_model
        .set(Some("selected".into()));
    settle().await;
    assert_eq!(mounted.state.fake.model_requests.borrow().len(), 3);
    assert_eq!(mounted.state.fake.context_requests.borrow().len(), 6);
    assert_eq!(
        mounted
            .state
            .fake
            .context_requests
            .borrow()
            .last()
            .unwrap()
            .1
            .as_deref(),
        Some("selected")
    );
}

#[wasm_bindgen_test]
async fn stale_model_and_context_results_cannot_win_after_switching_back() {
    let (model_release, model_pending) = futures::channel::oneshot::channel();
    let (context_release, context_pending) = futures::channel::oneshot::channel();
    let mounted = mount_test(move |state| {
        state.seed_connection();
        state.seed_session();
        state
            .fake
            .model_results
            .borrow_mut()
            .push_back(model_pending);
        state
            .fake
            .context_results
            .borrow_mut()
            .push_back(context_pending);
        chat_view(state)
    });
    settle().await;
    mounted.state.settings.connections.update(|connections| {
        let mut second = connections[0].clone();
        second.id = 2;
        connections.push(second);
    });
    mounted
        .state
        .chat
        .sessions
        .update(|sessions| sessions[0].connection_id = Some(2));
    settle().await;
    *mounted.state.fake.models.borrow_mut() = vec![ModelInfo {
        name: "current".into(),
    }];
    mounted.state.fake.connections.borrow_mut()[0].context_limit = Some(8192);
    mounted
        .state
        .chat
        .sessions
        .update(|sessions| sessions[0].connection_id = Some(1));
    settle().await;
    assert_eq!(mounted.state.chat.models.get_untracked()[0].name, "current");
    assert_eq!(
        mounted
            .state
            .chat
            .session_telemetry
            .get_untracked()
            .context_limit,
        8192
    );
    model_release
        .send(Ok(vec![ModelInfo {
            name: "stale".into(),
        }]))
        .unwrap();
    context_release.send(Ok(Some(123))).unwrap();
    settle().await;
    assert_eq!(mounted.state.chat.models.get_untracked()[0].name, "current");
    assert_eq!(
        mounted
            .state
            .chat
            .session_telemetry
            .get_untracked()
            .context_limit,
        8192
    );
}

#[wasm_bindgen_test]
async fn logout_discards_model_and_context_results() {
    use openwebide_frontend::state::auth::AuthState;
    let auth_slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let slot = auth_slot.clone();
    let (model_release, model_pending) = futures::channel::oneshot::channel();
    let (context_release, context_pending) = futures::channel::oneshot::channel();
    let mounted = mount_test(move |state| {
        state.seed_connection();
        state.seed_session();
        state
            .fake
            .model_results
            .borrow_mut()
            .push_back(model_pending);
        state
            .fake
            .context_results
            .borrow_mut()
            .push_back(context_pending);
        slot.set(Some(expect_context::<AuthState>()));
        chat_view(state)
    });
    settle().await;
    auth_slot.get().unwrap().logout();
    settle().await;
    model_release
        .send(Ok(vec![ModelInfo {
            name: "stale".into(),
        }]))
        .unwrap();
    context_release.send(Ok(Some(123))).unwrap();
    settle().await;
    assert!(mounted.state.chat.models.get_untracked().is_empty());
    assert_ne!(
        mounted
            .state
            .chat
            .session_telemetry
            .get_untracked()
            .context_limit,
        123
    );
}

#[wasm_bindgen_test]
async fn connection_tree_switches_existing_session_in_both_modes() {
    for mode in [
        openwebide_core::WorkspaceMode::Remote,
        openwebide_core::WorkspaceMode::Local,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.seed_connection();
            state.seed_session();
            let mut second = state.settings.connections.get_untracked()[0].clone();
            second.id = 2;
            second.name = "llama.cpp".into();
            second.model = Some("second-model".into());
            state.fake.connections.borrow_mut().push(second.clone());
            state
                .settings
                .connections
                .update(|connections| connections.push(second));
            *state.fake.models.borrow_mut() = vec![ModelInfo {
                name: "qwen3:8b".into(),
            }];
            chat_view(state)
        });
        settle().await;
        mounted.click(".tui-model-name");
        settle().await;
        assert!(
            mounted
                .element(".recent-menu")
                .text_content()
                .unwrap()
                .contains("llama.cpp")
        );
        assert_eq!(&*mounted.state.fake.model_requests.borrow(), &[1]);
        *mounted.state.fake.models.borrow_mut() = vec![ModelInfo {
            name: "second-model".into(),
        }];
        mounted.click("[data-connection-id='2'] .tui-connection-heading");
        settle().await;
        assert_eq!(&*mounted.state.fake.model_requests.borrow(), &[1, 2]);
        mounted.click("[data-connection-id='2'] .tui-connection-model");
        settle().await;
        assert_eq!(
            mounted.state.fake.sessions.borrow()[0].connection_id,
            Some(2)
        );
        assert_eq!(
            mounted.state.chat.sessions.get_untracked()[0].connection_id,
            Some(2)
        );
        assert_eq!(mounted.state.chat.active_session.get_untracked(), Some(1));
        assert_eq!(
            mounted.state.chat.selected_model.get_untracked().as_deref(),
            Some("second-model")
        );
        assert_eq!(
            mounted.state.settings.default_connection.get_untracked(),
            Some(1)
        );
        assert!(
            mounted
                .root
                .query_selector(".recent-menu")
                .unwrap()
                .is_none()
        );
        // A fresh session goes back to the default connection and configured model.
        mounted.state.chat.active_session.set(None);
        settle().await;
        assert!(mounted.state.chat.selected_model.get_untracked().is_none());
        assert_eq!(
            mounted
                .element(".tui-model-name")
                .text_content()
                .unwrap()
                .trim(),
            "qwen3:8b"
        );
    }
}

#[wasm_bindgen_test]
async fn connection_tree_choice_carries_into_new_session() {
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_connection();
        let mut second = state.settings.connections.get_untracked()[0].clone();
        second.id = 2;
        second.name = "llama.cpp".into();
        second.model = Some("second-model".into());
        state.fake.connections.borrow_mut().push(second.clone());
        state
            .settings
            .connections
            .update(|connections| connections.push(second));
        *state.fake.models.borrow_mut() = vec![ModelInfo {
            name: "second-model".into(),
        }];
        chat_view(state)
    });
    settle().await;
    mounted.click(".tui-model-name");
    settle().await;
    mounted.click("[data-connection-id='2'] .tui-connection-heading");
    settle().await;
    mounted.click("[data-connection-id='2'] .tui-connection-model");
    settle().await;
    mounted.input("new session");
    mounted.key("Enter", "Enter", false);
    settle().await;
    assert_eq!(
        mounted.state.fake.sessions.borrow()[0].connection_id,
        Some(2)
    );
    assert!(
        mounted
            .state
            .fake
            .calls
            .borrow()
            .contains(&Call::SendMessage {
                session: 1,
                content: "new session".into(),
                model: Some("second-model".into()),
            })
    );
    assert_eq!(
        mounted.state.settings.default_connection.get_untracked(),
        Some(1)
    );
}

#[wasm_bindgen_test]
async fn user_model_defaults_exclude_shared_model_configuration() {
    use openwebide_frontend::components::model_setup::ModelSetupPanel;
    let mounted = mount_test(|state| {
        state.seed_connection();
        *state.fake.models.borrow_mut() = vec![ModelInfo {
            name: "qwen3:8b".into(),
        }];
        view! { <ModelSetupPanel defaults_only=true /> }
    });
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".model-settings-editor")
            .unwrap()
            .is_none()
    );
    assert!(
        mounted
            .root
            .query_selector(".model-setup input[type=number]")
            .unwrap()
            .is_none()
    );
    super::support::choose_dropdown(
        &mounted,
        ".model-setup .ui-dropdown-trigger",
        &serde_json::to_string(&openwebide_core::ModelSelection {
            server_id: 1,
            model: "qwen3:8b".into(),
        })
        .unwrap(),
    )
    .await;
    settle().await;
    let setup = mounted.state.fake.model_setup.borrow().clone();
    assert_eq!(setup.defaults.primary.as_ref().unwrap().model, "qwen3:8b");
    assert!(setup.defaults.fast.is_none());
    assert_eq!(setup.resolve(1, "qwen3:8b").fast, setup.defaults.primary);
    assert_eq!(
        setup.resolve(1, "qwen3:8b").auto_compact_threshold,
        Some(85)
    );
}

#[wasm_bindgen_test]
async fn servers_open_shared_configuration_while_preferences_keep_model_defaults() {
    use openwebide_frontend::components::{Configuration, Settings};
    use wasm_bindgen::JsCast;
    for mode in [
        openwebide_core::WorkspaceMode::Remote,
        openwebide_core::WorkspaceMode::Local,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_connection();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            *state.fake.models.borrow_mut() = vec![ModelInfo {
                name: "qwen3:8b".into(),
            }];
            state
                .fake
                .model_setup
                .borrow_mut()
                .profiles
                .push(openwebide_core::ModelProfile {
                    selection: openwebide_core::ModelSelection {
                        server_id: 1,
                        model: "qwen3:8b".into(),
                    },
                    settings: openwebide_core::ModelSettings {
                        context_limit: Some(8192),
                        ..Default::default()
                    },
                });
            let unit = Callback::new(|()| ());
            let id = Callback::new(|_: i64| ());
            let edit = Callback::new(move |id: i64| state.settings.begin_model_setup(Some(id)));
            state.settings.configuration.set(Some(
                openwebide_frontend::state::settings::ConfigurationSection::Servers,
            ));
            view! {
                <Configuration on_new_connection=unit on_edit_connection=edit on_cancel_connection=Callback::new(move |()| state.settings.show_conn_form.set(false)) on_delete_connection=id
                    on_new_prompt=unit on_edit_prompt=id on_save_prompt=unit on_cancel_prompt=unit on_delete_prompt=id />
                <Show when=move || state.settings.show_settings.get()><Settings on_set_notifications=Callback::new(|_| ()) on_set_theme=Callback::new(|_| ()) on_set_default_prompt=Callback::new(|_| ()) on_set_bridge_url=Callback::new(|_| ()) /></Show>
            }
        });
        settle().await;
        assert!(
            mounted
                .element("#configure-server-1")
                .text_content()
                .unwrap_or_default()
                .contains("Configure")
        );
        mounted.click("#configure-server-1");
        settle().await;
        assert_eq!(mounted.state.settings.conn_edit_id.get_untracked(), Some(1));
        mounted.click_text("Discover models");
        settle().await;
        mounted.click(".model-review-row .ui-disclosure-toggle");
        settle().await;
        let modal = mounted.element(".modal:has(.model-setup)");
        let text = modal.text_content().unwrap();
        assert!(text.contains("Detect settings") && text.contains("Context tokens"));
        assert!(!text.contains("Default model") && !text.contains("Assistance model"));
        let context: web_sys::HtmlInputElement = mounted
            .element(".model-settings-editor input[type=number]")
            .unchecked_into();
        assert_eq!(context.value(), "8192");

        context.set_value("16384");
        context
            .dispatch_event(&web_sys::Event::new("input").unwrap())
            .unwrap();
        settle().await;
        mounted.click_text("Save server");
        settle().await;
        assert_eq!(
            mounted.state.fake.model_setup.borrow().profiles[0]
                .settings
                .context_limit,
            Some(16384)
        );
        assert!(
            mounted
                .element(".server-row-saved")
                .text_content()
                .unwrap()
                .contains("Configure")
        );
        mounted.click_text("Open model defaults");
        settle().await;
        assert_eq!(
            mounted
                .element("#settings-tab-models")
                .get_attribute("aria-selected")
                .as_deref(),
            Some("true")
        );
        let text = mounted
            .element(".modal:has(.settings, .ui-form-grid)")
            .text_content()
            .unwrap();
        assert!(
            text.contains("Default model")
                && text.contains("Assistance model")
                && text.contains("Default system prompt")
        );
        assert!(
            !text.contains("Context tokens")
                && !text.contains("Auto-compact")
                && !text.contains("Discover local servers")
        );
    }
}

#[wasm_bindgen_test]
async fn setup_wizard_discovers_customizes_and_reruns_in_both_modes() {
    use openwebide_frontend::components::model_wizard::ModelSetupWizard;
    use wasm_bindgen::JsCast;
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.seed_connection();
            state.settings.conn_edit_id.set(Some(1));
            state
                .settings
                .conn_base_url
                .set("http://models.test".into());
            *state.fake.models.borrow_mut() = vec![ModelInfo {
                name: "qwen3:8b".into(),
            }];
            let open = RwSignal::new(true);
            view! { <Show when=move || open.get()><ModelSetupWizard on_close=Callback::new(move |()| open.set(false)) /></Show> }
        });
        settle().await;
        assert!(
            mounted
                .element("li[aria-current=step]")
                .text_content()
                .unwrap()
                .contains("Connect server")
        );
        let key: web_sys::HtmlInputElement =
            mounted.element("input[type=password]").unchecked_into();
        key.set_value("test-token");
        key.dispatch_event(&web_sys::Event::new("input").unwrap())
            .unwrap();
        mounted.click_text("Discover models");
        settle().await;
        assert!(
            mounted
                .element("li[aria-current=step]")
                .text_content()
                .unwrap()
                .contains("Review models")
        );
        assert!(mounted.state.fake.server_settings.borrow().is_empty());
        assert!(!mounted.has_element(".model-settings-editor"));
        mounted.click(".model-review-row .ui-disclosure-toggle");
        settle().await;
        let context: web_sys::HtmlInputElement = mounted
            .element(".model-settings-editor input[type=number]")
            .unchecked_into();
        assert_eq!(context.value(), "8192");
        context.set_value("16384");
        context
            .dispatch_event(&web_sys::Event::new("input").unwrap())
            .unwrap();
        settle().await;
        let (sender, receiver) = futures::channel::oneshot::channel();
        sender
            .send(Err("An unchanged connection should reuse discovery".into()))
            .unwrap();
        mounted
            .state
            .fake
            .model_results
            .borrow_mut()
            .push_back(receiver);
        mounted.click_text("Back");
        settle().await;
        assert_eq!(key.value(), "test-token");
        mounted.click_text("Discover models");
        settle().await;
        assert_eq!(context.value(), "16384");
        assert_eq!(mounted.state.fake.model_results.borrow().len(), 1);
        assert!(mounted.state.fake.model_setup.borrow().profiles.is_empty());
        assert!(
            !mounted
                .root
                .text_content()
                .unwrap()
                .contains("Save model settings")
        );
        assert!(
            !mounted
                .root
                .text_content()
                .unwrap()
                .contains("Use detected context")
        );
        assert!(
            !mounted
                .root
                .text_content()
                .unwrap()
                .contains("Re-run discovery")
        );
        mounted.click_text("Detect settings");
        settle().await;
        assert_eq!(context.value(), "8192");
        context.set_value("24576");
        context
            .dispatch_event(&web_sys::Event::new("input").unwrap())
            .unwrap();
        settle().await;
        mounted.click_text("Save server");
        settle().await;
        let setup = mounted.state.fake.model_setup.borrow();
        assert_eq!(setup.profiles[0].settings.context_limit, Some(24576));
        assert!(mounted.state.fake.server_settings.borrow()[&1].has_api_key);
        assert_eq!(setup.defaults.primary.as_ref().unwrap().model, "qwen3:8b");
        assert!(mounted.root.query_selector(".modal").unwrap().is_none());
    }
}

#[wasm_bindgen_test]
async fn setup_retry_keeps_one_server_and_closed_discovery_cannot_apply() {
    use openwebide_frontend::components::model_wizard::ModelSetupWizard;
    for close in [false, true] {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let mounted = mount_test(move |state| {
            state.fake.model_results.borrow_mut().push_back(receiver);
            *state.fake.models.borrow_mut() = vec![ModelInfo {
                name: "chat".into(),
            }];
            let open = RwSignal::new(true);
            view! { <Show when=move || open.get()><ModelSetupWizard on_close=Callback::new(move |()| open.set(false)) /></Show> }
        });
        settle().await;
        mounted.click_text("Discover models");
        settle().await;
        assert_eq!(mounted.state.fake.connections.borrow().len(), 0);
        if close {
            mounted.element(".modal button[title=Close]").click();
            settle().await;
            sender
                .send(Ok(vec![ModelInfo {
                    name: "stale".into(),
                }]))
                .unwrap();
            settle().await;
            assert!(mounted.state.fake.model_setup.borrow().profiles.is_empty());
            assert!(
                mounted
                    .state
                    .fake
                    .model_setup
                    .borrow()
                    .defaults
                    .primary
                    .is_none()
            );
            assert!(mounted.root.query_selector(".modal").unwrap().is_none());
        } else {
            sender
                .send(Err("401: Authentication required".into()))
                .unwrap();
            settle().await;
            assert!(
                mounted
                    .root
                    .text_content()
                    .unwrap()
                    .contains("Authentication required")
            );
            mounted.click_text("Discover models");
            settle().await;
            assert!(
                mounted
                    .element("li[aria-current=step]")
                    .text_content()
                    .unwrap()
                    .contains("Review models")
            );
            assert_eq!(mounted.state.fake.connections.borrow().len(), 0);
            mounted.click_text("Save server");
            settle().await;
            assert_eq!(mounted.state.fake.connections.borrow().len(), 1);
        }
    }
}

#[wasm_bindgen_test]
async fn detection_fills_all_reported_values_and_cancel_discards_them_in_both_modes() {
    use openwebide_core::{
        ModelDetection, ModelProfile, ModelSelection, ModelSettings, WorkspaceMode,
    };
    use openwebide_frontend::components::model_wizard::ModelSetupWizard;
    use wasm_bindgen::JsCast;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.seed_connection();
            state.settings.begin_model_setup(Some(1));
            *state.fake.models.borrow_mut() = vec![ModelInfo {
                name: "main".into(),
            }];
            state
                .fake
                .model_setup
                .borrow_mut()
                .profiles
                .push(ModelProfile {
                    selection: ModelSelection {
                        server_id: 1,
                        model: "main".into(),
                    },
                    settings: ModelSettings {
                        context_limit: Some(4096),
                        max_output_tokens: Some(256),
                        sampling: std::collections::BTreeMap::from([(
                            "temperature".into(),
                            serde_json::json!(0.2),
                        )]),
                        ..Default::default()
                    },
                });
            state.fake.detections.borrow_mut().insert(
                (1, "main".into()),
                ModelDetection {
                    context_limit: Some(8192),
                    max_output_tokens: Some(1024),
                    sampling: std::collections::BTreeMap::from([(
                        "temperature".into(),
                        serde_json::json!(0.8),
                    )]),
                    capabilities: vec!["completion".into(), "tools".into()],
                    ..Default::default()
                },
            );
            let open = RwSignal::new(true);
            view! { <Show when=move || open.get()><ModelSetupWizard on_close=Callback::new(move |()| open.set(false)) /></Show> }
        });
        settle().await;
        mounted.click_text("Discover models");
        settle().await;
        mounted.click(".model-review-row .ui-disclosure-toggle");
        settle().await;
        let context: web_sys::HtmlInputElement = mounted
            .element(".model-settings-editor section:first-of-type label:first-of-type input")
            .unchecked_into();
        let output: web_sys::HtmlInputElement = mounted
            .element(".model-settings-editor section:first-of-type label:nth-of-type(2) input")
            .unchecked_into();
        let temperature: web_sys::HtmlInputElement = mounted
            .element(".model-settings-editor section:nth-of-type(2) label:first-of-type input")
            .unchecked_into();
        assert_eq!(
            (context.value(), output.value(), temperature.value()),
            ("4096".into(), "256".into(), "0.2".into())
        );
        mounted.click_text("Detect settings");
        settle().await;
        assert_eq!(
            (context.value(), output.value(), temperature.value()),
            ("8192".into(), "1024".into(), "0.8".into())
        );
        assert_eq!(
            mounted.state.fake.model_setup.borrow().profiles[0]
                .settings
                .context_limit,
            Some(4096)
        );
        mounted.click_text("Cancel");
        settle().await;
        assert!(mounted.root.query_selector(".modal").unwrap().is_none());
        assert_eq!(
            mounted.state.fake.model_setup.borrow().profiles[0]
                .settings
                .context_limit,
            Some(4096)
        );
    }
}

#[wasm_bindgen_test]
async fn model_test_plain_chat_fallback_is_saved_only_for_the_tested_model() {
    use openwebide_frontend::components::model_wizard::ModelSetupWizard;
    let mounted = mount_test(|state| {
        state.seed_connection();
        state.settings.begin_model_setup(Some(1));
        *state.fake.models.borrow_mut() = vec![
            ModelInfo { name: "one".into() },
            ModelInfo { name: "two".into() },
        ];
        state
            .fake
            .test_results
            .borrow_mut()
            .push_back(Ok(openwebide_core::ModelTestResult {
                notice: Some("Plain chat after Save".into()),
                ..Default::default()
            }));
        let open = RwSignal::new(true);
        view! { <Show when=move || open.get()><ModelSetupWizard on_close=Callback::new(move |()| open.set(false)) /></Show> }
    });
    settle().await;
    mounted.click_text("Discover models");
    settle().await;
    mounted.click(".model-review-row .ui-disclosure-toggle");
    settle().await;
    mounted.click_text("Test model");
    settle().await;
    assert!(mounted.state.fake.model_setup.borrow().profiles.is_empty());
    mounted.click_text("Save server");
    settle().await;
    let setup = mounted.state.fake.model_setup.borrow();
    let one = setup
        .profiles
        .iter()
        .find(|profile| profile.selection.model == "one")
        .unwrap();
    let two = setup
        .profiles
        .iter()
        .find(|profile| profile.selection.model == "two")
        .unwrap();
    assert_eq!(one.settings.tools, Some(false));
    assert_eq!(one.settings.stream_tools, Some(false));
    assert_eq!(two.settings.tools, Some(true));
}

#[wasm_bindgen_test]
async fn model_defaults_autosave_serializes_changes_after_dialog_close_in_every_scope() {
    use openwebide_core::{ModelDefaults, ModelSelection, WorkspaceMode};
    use openwebide_frontend::state_actions::settings::set_model_defaults;
    for mode in [
        Some(WorkspaceMode::Local),
        Some(WorkspaceMode::Remote),
        None,
    ] {
        let (send, receive) = futures::channel::oneshot::channel();
        let mounted = mount_test(move |state| {
            if let Some(mode) = mode {
                state.seed_project();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
            }
            state
                .fake
                .model_default_results
                .borrow_mut()
                .push_back(receive);
            view! { <div /> }
        });
        let defaults = |name: &str| ModelDefaults {
            primary: Some(ModelSelection {
                server_id: 1,
                model: name.into(),
            }),
            ..Default::default()
        };
        let state = &mounted.state;
        set_model_defaults(
            state.api,
            state.settings,
            state.auth,
            state.ui,
            defaults("first"),
        );
        settle().await;
        assert_eq!(state.fake.model_default_requests.borrow().len(), 1);
        set_model_defaults(
            state.api,
            state.settings,
            state.auth,
            state.ui,
            defaults("second"),
        );
        set_model_defaults(
            state.api,
            state.settings,
            state.auth,
            state.ui,
            defaults("latest"),
        );
        state.settings.show_settings.set(false);
        assert_eq!(state.fake.model_default_requests.borrow().len(), 1);
        let mut old = state.fake.model_setup.borrow().clone();
        old.defaults = defaults("first");
        send.send(Ok(old)).unwrap();
        settle().await;
        assert_eq!(state.fake.model_default_requests.borrow().len(), 2);
        assert_eq!(
            state.fake.model_default_requests.borrow()[1],
            defaults("latest")
        );
        assert_eq!(
            state.settings.model_setup.get_untracked().defaults,
            defaults("latest")
        );
        assert_eq!(state.fake.model_setup.borrow().defaults, defaults("latest"));
        assert_eq!(state.settings.model_defaults_saving.get_untracked(), None);
    }
}

#[wasm_bindgen_test]
async fn model_defaults_autosave_shows_retry_and_ignores_previous_accounts() {
    use openwebide_core::{ModelDefaults, ModelSelection};
    use openwebide_frontend::{
        components::model_setup::ModelSetupPanel, state_actions::settings::set_model_defaults,
    };
    let (send, receive) = futures::channel::oneshot::channel();
    let mounted = mount_test(move |state| {
        state
            .fake
            .model_default_results
            .borrow_mut()
            .push_back(receive);
        view! { <ModelSetupPanel defaults_only=true /> }
    });
    settle().await;
    let state = &mounted.state;
    let defaults = ModelDefaults {
        primary: Some(ModelSelection {
            server_id: 1,
            model: "chosen".into(),
        }),
        ..Default::default()
    };
    set_model_defaults(
        state.api,
        state.settings,
        state.auth,
        state.ui,
        defaults.clone(),
    );
    settle().await;
    assert!(mounted.root.text_content().unwrap().contains("Saving…"));
    send.send(Err("Temporary save failure".into())).unwrap();
    settle().await;
    assert!(mounted.root.text_content().unwrap().contains("Not saved"));
    assert_eq!(
        state.settings.model_setup.get_untracked().defaults,
        defaults
    );
    mounted.click_text("Retry");
    settle().await;
    assert_eq!(state.fake.model_setup.borrow().defaults, defaults);
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Changes save automatically.")
    );
    let (send, receive) = futures::channel::oneshot::channel();
    state
        .fake
        .model_default_results
        .borrow_mut()
        .push_back(receive);
    set_model_defaults(state.api, state.settings, state.auth, state.ui, defaults);
    settle().await;
    *state.fake.model_setup.borrow_mut() = Default::default();
    state.auth.generation.update(|epoch| *epoch += 1);
    state.settings.model_setup.set(Default::default());
    state.settings.default_connection.set(None);
    state.settings.model_defaults_saving.set(None);
    send.send(Err("Old account failure".into())).unwrap();
    settle().await;
    assert_eq!(
        state.settings.model_setup.get_untracked().defaults,
        ModelDefaults::default()
    );
    assert_eq!(state.settings.default_connection.get_untracked(), None);
    assert_eq!(state.settings.model_defaults_error.get_untracked(), None);
    assert!(
        !mounted
            .root
            .text_content()
            .unwrap()
            .contains("Old account failure")
    );
}
