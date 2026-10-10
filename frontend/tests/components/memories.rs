use super::support::{Mounted, chat_view, mount_test, settle, wait_until};
use leptos::prelude::*;
use openwebide_core::{
    ChatCompletion, ChatResponse, MemoryCommand, ProjectMemories, ProjectMemory, StopReason,
    ToolCall, WorkspaceMode,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;
fn fixture(mode: WorkspaceMode, chat: bool) -> Mounted {
    mount_test(move |state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        {
            let prepared = state.seed_memory_plugin();
            if mode == WorkspaceMode::Local {
                state
                    .settings
                    .bridge_url
                    .set("ws://bridge.test:3001".into());
                *state.fake.bridge_credential.borrow_mut() =
                    Some(("memory-test-token".into(), i64::MAX));
                let mock = super::local_bridge::fake_bridge_http();
                super::local_bridge::bridge_found(&mock, false);
                super::local_bridge::set_bridge_plugin(
                    &mock,
                    &serde_json::to_string(&prepared).unwrap(),
                );
                let mock = send_wrapper::SendWrapper::new(mock);
                on_cleanup(move || super::local_bridge::restore_bridge_http(&mock));
            }
        }
        state
            .projects
            .projects
            .update(|projects| projects[0].mode = mode);
        if mode == WorkspaceMode::Local {
            state.projects.local_handles.update(|handles| {
                handles.insert(1, super::local_bridge::probe_folder().unchecked_into());
            });
        }
        view! {<style>{include_str!("../../styles.css")}</style><openwebide_frontend::components::memories::Memories/>{chat.then(||chat_view(state))}}
    })
}
fn input(mounted: &Mounted, selector: &str, value: &str) {
    let element = mounted.element(selector);
    if let Some(input) = element.dyn_ref::<web_sys::HtmlInputElement>() {
        input.set_value(value);
    } else {
        element
            .dyn_ref::<web_sys::HtmlTextAreaElement>()
            .unwrap()
            .set_value(value);
    }
    element
        .dispatch_event(&web_sys::Event::new("input").unwrap())
        .unwrap();
}
#[wasm_bindgen_test]
async fn project_memory_ui_edits_toggles_preserves_conflicts_and_follows_project_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = fixture(mode, false);
        settle().await;
        assert!(
            mounted
                .element(".project-memories .ui-seg-btn")
                .get_attribute("aria-pressed")
                .as_deref()
                == Some("true")
        );
        mounted.click("[aria-label='New memory']");
        settle().await;
        input(&mounted, "[aria-label='Memory title']", "Build");
        input(
            &mounted,
            "[aria-label='Memory content']",
            "Run cargo test 🦀",
        );
        mounted.click_text("Save memory");
        wait_until("plugin action to finish", || {
            !mounted.state.memories.busy.get_untracked()
        })
        .await;
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Run cargo test 🦀"),
            "mode {mode:?}: {:?}",
            mounted.state.memories.error.get_untracked()
        );
        mounted.click(".memory-entry .ui-disclosure-toggle");
        mounted.click_text("Edit");
        settle().await;
        input(
            &mounted,
            "[aria-label='Memory content']",
            "Run cargo test --offline",
        );
        mounted
            .state
            .fake
            .memories
            .borrow_mut()
            .get_mut(&1)
            .unwrap()
            .entries[0]
            .revision += 1;
        mounted.click_text("Save memory");
        wait_until("plugin action to finish", || {
            !mounted.state.memories.busy.get_untracked()
        })
        .await;
        assert!(
            mounted
                .element("[role=alert]")
                .text_content()
                .unwrap()
                .contains("changed")
        );
        assert_eq!(
            mounted
                .element("[aria-label='Memory content']")
                .unchecked_into::<web_sys::HtmlTextAreaElement>()
                .value(),
            "Run cargo test --offline"
        );
        mounted.click_text("Cancel");
        mounted.state.chat.streaming.set(true);
        settle().await;
        mounted.state.chat.streaming.set(false);
        settle().await;
        let (sender, receiver) = futures::channel::oneshot::channel();
        sender.send(Err("Database unavailable".into())).unwrap();
        mounted
            .state
            .fake
            .memory_command_results
            .borrow_mut()
            .push_back(receiver);
        mounted.click_text("Disabled");
        settle().await;
        assert!(
            mounted
                .element(".project-memories .ui-seg-btn")
                .get_attribute("aria-pressed")
                .as_deref()
                == Some("true")
        );
        assert!(mounted.state.fake.memories.borrow()[&1].enabled);
        mounted.click_text("Disabled");
        settle().await;
        assert!(!mounted.state.fake.memories.borrow()[&1].enabled);
        assert_eq!(mounted.state.fake.memories.borrow()[&1].entries.len(), 1);
        mounted.click(".memory-entry .ui-disclosure-toggle");
        mounted.click_text("Delete");
        wait_until("plugin action to finish", || {
            !mounted.state.memories.busy.get_untracked()
        })
        .await;
        assert!(mounted.state.fake.memories.borrow()[&1].entries.is_empty());
        assert!(
            mounted
                .state
                .fake
                .memory_commands
                .borrow()
                .iter()
                .all(|(_, command, _)| matches!(command, MemoryCommand::SetEnabled { .. }))
        );
        mounted.state.projects.active_project.set(None);
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".project-memories")
                .unwrap()
                .is_none()
        );
    }
}
#[wasm_bindgen_test]
async fn memory_ui_requires_an_enabled_executable_plugin_without_builtin_fallback() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = fixture(mode, false);
        settle().await;
        mounted
            .state
            .fake
            .project_plugin_entries
            .borrow_mut()
            .get_mut(&1)
            .unwrap()[0]
            .enabled = false;
        let actions = mounted.state.memory_actions;
        actions.edit.run(None);
        mounted.state.memories.content.set("Keep this fact".into());
        actions.save.run(());
        wait_until("plugin action to finish", || {
            !mounted.state.memories.busy.get_untracked()
        })
        .await;
        assert!(
            mounted
                .state
                .memories
                .error
                .get_untracked()
                .unwrap()
                .contains("No enabled plugin")
        );
        assert!(mounted.state.fake.memory_commands.borrow().is_empty());
        assert!(
            mounted
                .state
                .fake
                .memories
                .borrow()
                .get(&1)
                .is_none_or(|data| data.entries.is_empty())
        );
    }
}
#[wasm_bindgen_test]
async fn project_memory_stale_project_and_account_responses_do_not_restore_old_entries() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = fixture(mode, false);
        settle().await;
        let actions = mounted.state.memory_actions;
        let state = mounted.state.memories;
        let (sender, receiver) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .memory_load_results
            .borrow_mut()
            .push_back(receiver);
        actions.refresh.run(());
        settle().await;
        mounted.state.projects.active_project.set(Some(2));
        settle().await;
        sender
            .send(Ok(ProjectMemories {
                enabled: true,
                entries: vec![ProjectMemory {
                    auto_title: false,
                    id: 1,
                    title: "Old project".into(),
                    content: "old".into(),
                    revision: 1,
                    updated_at: 0,
                }],
            }))
            .unwrap();
        settle().await;
        assert!(!mounted.root.text_content().unwrap().contains("Old project"));
        mounted.state.projects.active_project.set(Some(1));
        settle().await;
        actions.edit.run(None);
        state.title.set("Pending".into());
        state.content.set("must not return after logout".into());
        let (sender, receiver) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .memory_command_results
            .borrow_mut()
            .push_back(receiver);
        actions.save.run(());
        settle().await;
        mounted
            .state
            .auth
            .generation
            .update(|generation| *generation += 1);
        settle().await;
        sender
            .send(Ok(ProjectMemories {
                enabled: true,
                entries: vec![ProjectMemory {
                    auto_title: false,
                    id: 1,
                    title: "Stale account".into(),
                    content: "old".into(),
                    revision: 1,
                    updated_at: 0,
                }],
            }))
            .unwrap();
        settle().await;
        assert!(
            !mounted
                .root
                .text_content()
                .unwrap()
                .contains("Stale account")
        );
        assert!(!state.editing.get_untracked());
    }
}
#[wasm_bindgen_test]
async fn browser_memory_tools_persist_and_toggle_removes_context_and_tools_from_new_runs() {
    let mounted = fixture(WorkspaceMode::Local, true);
    settle().await;
    mounted
        .state
        .chat
        .set_approval_mode(1, openwebide_core::ApprovalMode::Yolo);
    mounted.state.fake.settings.borrow_mut().insert(
        openwebide_core::ApprovalMode::setting_key(1),
        "\"yolo\"".into(),
    );
    let reply = ChatCompletion {
        response: ChatResponse::Text("done".into()),
        preamble: String::new(),
        reasoning: String::new(),
        usage: None,
        stop_reason: StopReason::Complete,
    };
    let create = ChatCompletion {
        response: ChatResponse::ToolCalls(vec![ToolCall {
            id: "remember".into(),
            name: "memory_create".into(),
            arguments: r#"{"title":"Build","content":"Run cargo test"}"#.into(),
        }]),
        ..reply.clone()
    };
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .extend([create, reply.clone()]);
    mounted.input("remember our build command");
    mounted.key("Enter", "Enter", false);
    wait_until("memory run finished", || {
        !mounted.state.chat.streaming.get_untracked()
    })
    .await;
    settle().await;
    assert!(
        mounted.state.chat.error.get_untracked().is_none(),
        "{:?}",
        mounted.state.chat.error.get_untracked()
    );
    assert_eq!(mounted.state.fake.memories.borrow()[&1].entries.len(), 1);
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Run cargo test")
    );
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .push_back(reply.clone());
    mounted.input("what is our build command");
    mounted.key("Enter", "Enter", false);
    wait_until("memory context run finished", || {
        !mounted.state.chat.streaming.get_untracked()
    })
    .await;
    settle().await;
    let request = mounted
        .state
        .fake
        .completion_requests
        .borrow()
        .last()
        .unwrap()
        .clone();
    assert!(request.tools.iter().any(|tool| tool.name == "memory_read"));
    assert!(
        request
            .system_prompt
            .as_ref()
            .unwrap()
            .contains("Run cargo test")
    );
    mounted.click_text("Disabled");
    settle().await;
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .push_back(reply);
    mounted.input("try without memory");
    mounted.key("Enter", "Enter", false);
    wait_until("disabled memory run finished", || {
        !mounted.state.chat.streaming.get_untracked()
    })
    .await;
    settle().await;
    let requests = mounted.state.fake.completion_requests.borrow();
    let request = requests.last().unwrap();
    assert!(
        !request
            .tools
            .iter()
            .any(|tool| tool.name.starts_with("memory_"))
    );
    assert!(
        !request
            .system_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.contains("Project memories (stored"))
    );
    assert_eq!(
        mounted
            .state
            .fake
            .memory_commands
            .borrow()
            .iter()
            .filter(
                |(_, command, session)| *session && matches!(command, MemoryCommand::Create { .. })
            )
            .count(),
        0
    );
}
