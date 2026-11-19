use super::support::{mount_test, settle};
use leptos::prelude::*;
use openwebide_core::{ModelInfo, ToolSelection, WorkspaceMode};
use openwebide_frontend::components::{model_setup::ServerOptions, model_wizard::ModelSetupWizard};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;

#[wasm_bindgen_test]
async fn tool_budgets_wizard_saves_choices_and_context_costs_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for choice in ["all", "selected", "chat"] {
            let mounted = mount_test(move |state| {
                state.seed_project();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
                state.seed_connection();
                state.settings.begin_model_setup(Some(1));
                *state.fake.models.borrow_mut() = vec![ModelInfo {
                    name: "test".into(),
                }];
                view! { <ModelSetupWizard on_close=Callback::new(|()| ()) /> }
            });
            settle().await;
            mounted.click(".model-setup summary");
            settle().await;
            mounted.click("button[aria-label='Tool selection']");
            settle().await;
            mounted.click(&format!("button[data-value='{choice}']"));
            settle().await;
            if choice == "selected" {
                let checks = mounted
                    .root
                    .query_selector_all(".ui-check input[type=checkbox]")
                    .unwrap();
                for index in 0..checks.length() {
                    let input: web_sys::HtmlInputElement =
                        checks.item(index).unwrap().unchecked_into();
                    let label = input.parent_element().unwrap().text_content().unwrap();
                    if input.checked() && !label.starts_with("read_file") {
                        input.set_checked(false);
                        input
                            .dispatch_event(&web_sys::Event::new("change").unwrap())
                            .unwrap();
                    }
                }
                settle().await;
                assert!(
                    mounted
                        .element(".tool-budget")
                        .text_content()
                        .unwrap()
                        .starts_with("1 tool")
                );
            }
            assert!(mounted.state.fake.server_settings.borrow().is_empty());
            mounted.click_text("Discover models");
            settle().await;
            mounted.click(".model-review-row .ui-disclosure-toggle");
            settle().await;
            let budget = mounted
                .element(".model-settings-editor .tool-budget")
                .text_content()
                .unwrap();
            assert!(budget.contains("schema tokens"));
            assert!(budget.contains("% of 8192 context tokens"));
            if choice == "chat" {
                assert!(budget.starts_with("0 tools · ~0"));
            }
            mounted.click_text("Save server");
            settle().await;
            let expected = match choice {
                "selected" => ToolSelection::Selected(vec!["read_file".into()]),
                "chat" => ToolSelection::ChatOnly,
                _ => ToolSelection::All,
            };
            assert_eq!(
                mounted.state.settings.connections.get_untracked()[0].tool_selection,
                expected
            );
            assert_eq!(
                mounted.state.fake.connections.borrow()[0].tool_selection,
                expected
            );
            assert_eq!(
                mounted.state.fake.server_settings.borrow()[&1].tool_selection,
                expected
            );
        }
    }
}

#[wasm_bindgen_test]
async fn tool_budgets_late_server_settings_do_not_replace_edits_or_cross_accounts() {
    for account_change in [false, true] {
        let (done, pending) = futures::channel::oneshot::channel();
        let mounted = mount_test(move |state| {
            state.seed_connection();
            state
                .fake
                .server_settings_results
                .borrow_mut()
                .push_back(pending);
            view! { <ServerOptions id=Some(1) on_edit=Callback::new(|_| ()) /> }
        });
        settle().await;
        mounted.click("summary");
        settle().await;
        mounted.click("button[aria-label='Tool selection']");
        settle().await;
        mounted.click("button[data-value='chat']");
        settle().await;
        if account_change {
            mounted
                .state
                .auth
                .generation
                .update(|generation| *generation += 1);
        }
        done.send(Ok(openwebide_core::ServerSettings {
            timeout_seconds: 300,
            tool_selection: ToolSelection::Selected(vec!["write_file".into()]),
            ..Default::default()
        }))
        .unwrap();
        settle().await;
        assert_eq!(
            mounted
                .element("button[aria-label='Tool selection']")
                .text_content()
                .unwrap()
                .trim(),
            "Chat only"
        );
        assert!(
            mounted
                .element(".tool-budget")
                .text_content()
                .unwrap()
                .starts_with("0 tools · ~0")
        );
    }
}

#[wasm_bindgen_test]
async fn tool_budgets_browser_run_sends_only_selected_schemas_or_plain_chat() {
    use super::support::{chat_view, wait_until};
    use openwebide_core::{ChatCompletion, ChatResponse, StopReason};
    for selection in [
        ToolSelection::Selected(vec!["read_file".into()]),
        ToolSelection::Selected(vec![]),
        ToolSelection::ChatOnly,
    ] {
        let chosen = selection.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_connection();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = WorkspaceMode::Local);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, super::local_bridge::probe_folder().unchecked_into());
            });
            state.fake.connections.borrow_mut()[0].tool_selection = chosen.clone();
            state
                .settings
                .connections
                .update(|connections| connections[0].tool_selection = chosen);
            state
                .fake
                .scripted_completions
                .borrow_mut()
                .push_back(ChatCompletion {
                    response: ChatResponse::Text("done".into()),
                    preamble: String::new(),
                    reasoning: String::new(),
                    usage: None,
                    stop_reason: StopReason::Complete,
                });
            chat_view(state)
        });
        settle().await;
        mounted.input("hello");
        mounted.key("Enter", "Enter", false);
        wait_until("run finishes", || {
            !mounted.state.fake.completion_requests.borrow().is_empty()
                && !mounted.state.chat.streaming.get_untracked()
        })
        .await;
        let requests = mounted.state.fake.completion_requests.borrow();
        assert_eq!(requests.len(), 1);
        let names = requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        if selection == ToolSelection::Selected(vec!["read_file".into()]) {
            assert_eq!(names, vec!["read_file"]);
        } else {
            assert!(names.is_empty());
        }
    }
}
