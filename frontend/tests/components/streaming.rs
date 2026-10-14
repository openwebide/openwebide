use openwebide_core::{ChatMessage, Role, RunEvent};
use wasm_bindgen_test::*;

use super::support::{chat_view, mount_test, settle};

async fn settle_run(mounted: &super::support::Mounted) {
    use leptos::prelude::GetUntracked;
    for _ in 0..500 {
        settle().await;
        if !mounted.state.chat.streaming.get_untracked() {
            return;
        }
        openwebide_frontend::util::sleep_ms(10).await;
    }
    panic!("local run did not settle");
}

fn message(id: i64, role: Role, content: &str) -> ChatMessage {
    ChatMessage {
        id,
        session_id: 1,
        role,
        content: content.into(),
        created_at: 0,
        tool_calls: None,
        tool_call_id: None,
        usage: None,
    }
}

#[wasm_bindgen_test]
async fn reasoning_summary_counts_live_tokens_for_both_stream_formats_in_all_modes() {
    use leptos::prelude::*;
    use openwebide_core::WorkspaceMode;
    for mode in [
        Some(WorkspaceMode::Local),
        Some(WorkspaceMode::Remote),
        None,
    ] {
        for provider_reasoning in [false, true] {
            let mounted = mount_test(move |state| {
                if let Some(mode) = mode {
                    state.seed_project();
                    state
                        .projects
                        .projects
                        .update(|projects| projects[0].mode = mode);
                }
                state.seed_connection();
                state.seed_session();
                if mode.is_none() {
                    state
                        .chat
                        .sessions
                        .update(|sessions| sessions[0].project_id = None);
                }
                view! { <style>{include_str!("../../styles.css")}</style>{chat_view(state)} }
            });
            settle().await;
            let chat = mounted.state.chat;
            chat.apply_event(if provider_reasoning {
                RunEvent::ReasoningDelta {
                    content: "first".into(),
                }
            } else {
                RunEvent::Delta {
                    content: "<thi".into(),
                }
            });
            if !provider_reasoning {
                settle().await;
                chat.apply_event(RunEvent::Delta {
                    content: "nk>first".into(),
                });
            }
            settle().await;
            let header = mounted.element(".tui-thinking-summary");
            let style = web_sys::window()
                .unwrap()
                .get_computed_style(&header)
                .unwrap()
                .unwrap();
            assert_eq!(
                style.get_property_value("justify-content").unwrap(),
                "flex-start"
            );
            assert_eq!(style.get_property_value("text-align").unwrap(), "left");
            assert!(header.class_list().contains("ui-disclosure-toggle"));
            let initial = mounted.element(".tui-think-meta").text_content().unwrap();
            assert!(
                initial.contains("Thinking for 0.0s · ~2 tokens"),
                "{initial}"
            );
            openwebide_frontend::util::sleep_ms(220).await;
            chat.apply_event(if provider_reasoning {
                RunEvent::ReasoningDelta {
                    content: "x".repeat(5600),
                }
            } else {
                RunEvent::Delta {
                    content: "x".repeat(5600),
                }
            });
            settle().await;
            let live = mounted.element(".tui-think-meta").text_content().unwrap();
            assert!(live.contains("~1.4k tokens"), "{live}");
            assert!(!live.contains("for 0.0s"), "clock restarted: {live}");
            // Failure freezes partial traces just like cancellation and completion.
            chat.apply_event(RunEvent::Error {
                message: "provider disconnected".into(),
            });
            settle().await;
            let summary = mounted.element(".tui-think-meta").text_content().unwrap();
            assert!(summary.starts_with("Thought for "), "{summary}");
            assert!(
                mounted
                    .root
                    .query_selector(".tui-thinking-summary.active")
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                mounted
                    .element(".tui-thinking-summary")
                    .get_attribute("aria-expanded")
                    .as_deref(),
                Some("false")
            );
            openwebide_frontend::util::sleep_ms(220).await;
            assert_eq!(
                mounted.element(".tui-think-meta").text_content().unwrap(),
                summary
            );
            mounted.click(".tui-thinking-summary");
            settle().await;
            assert_eq!(
                mounted
                    .element(".tui-thinking-summary")
                    .get_attribute("aria-expanded")
                    .as_deref(),
                Some("true")
            );
            assert!(
                mounted
                    .element(".tui-thinking-pre")
                    .text_content()
                    .unwrap()
                    .starts_with("first")
            );
        }
    }
}

#[wasm_bindgen_test]
async fn interim_text_renders_above_tool_and_final_reply_below() {
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        state.fake.scripted_events.borrow_mut().push_back(vec![
            RunEvent::Message {
                message: message(7, Role::User, "check"),
            },
            RunEvent::Delta {
                content: "Checking the file".into(),
            },
            RunEvent::Interim {
                message: message(8, Role::Assistant, "Checking the file"),
            },
            RunEvent::ToolCall {
                id: "a7t1c0".into(),
                name: "read_file".into(),
                summary: "read src/main.rs".into(),
            },
            RunEvent::ToolResult {
                id: "a7t1c0".into(),
                name: "read_file".into(),
                ok: true,
                summary: "read src/main.rs".into(),
                diff: None,
            },
            RunEvent::Delta {
                content: "Everything is ready".into(),
            },
            RunEvent::Done {
                message: message(9, Role::Assistant, "Everything is ready"),
            },
        ]);
        chat_view(state)
    });
    settle().await;
    mounted.input("check");
    mounted.key("Enter", "Enter", false);
    settle().await;
    let text = mounted.root.text_content().unwrap();
    let interim = text.find("Checking the file").unwrap();
    let tool = text.find("read src/main.rs").unwrap();
    let final_reply = text.find("Everything is ready").unwrap();
    assert!(interim < tool && tool < final_reply, "{text}");
    assert_eq!(text.matches("Checking the file").count(), 1);
    assert_eq!(text.matches("Everything is ready").count(), 1);
}

#[wasm_bindgen_test]
async fn empty_interim_with_calls_is_hidden_but_text_interim_renders() {
    use openwebide_core::ConversationEntry;
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        let mut empty = message(8, Role::Assistant, "");
        empty.tool_calls = Some(vec![openwebide_core::ToolCall {
            id: "wire".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }]);
        let mut text = empty.clone();
        text.id = 9;
        text.content = "Checking the file".into();
        state.fake.messages.borrow_mut().insert(
            1,
            vec![
                ConversationEntry::Message(empty),
                ConversationEntry::Message(text),
            ],
        );
        chat_view(state)
    });
    settle().await;
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Checking the file")
    );
    assert!(
        mounted
            .root
            .query_selector(".tui-assistant")
            .unwrap()
            .is_some()
    );
    assert!(
        mounted
            .root
            .query_selector(".tui-assistant ~ .tui-assistant")
            .unwrap()
            .is_none()
    );
}

fn interrupted_local_view(state: super::support::TestState) -> impl leptos::prelude::IntoView {
    use leptos::prelude::*;
    use openwebide_core::{ConversationEntry, WorkspaceMode};
    use wasm_bindgen::JsCast;
    state.seed_project();
    state
        .projects
        .projects
        .update(|projects| projects[0].mode = WorkspaceMode::Local);
    state.seed_connection();
    state.seed_session();
    state.fake.messages.borrow_mut().insert(
        1,
        vec![ConversationEntry::Message(message(7, Role::User, "check"))],
    );
    state
        .fake
        .scripted_completions
        .borrow_mut()
        .push_back(openwebide_core::ChatCompletion {
            reasoning: String::new(),
            stop_reason: openwebide_core::StopReason::Complete,
            response: openwebide_core::ChatResponse::Text("Resumed reply".into()),
            preamble: String::new(),
            usage: None,
        });
    // Startup context checks instruction files even for a text-only completion.
    state.projects.local_handles.update(|handles| {
        handles.insert(1, super::local_bridge::empty_read_folder().unchecked_into());
    });
    chat_view(state)
}

#[wasm_bindgen_test]
async fn interrupted_local_resume_requests_model_without_persisting_user() {
    use openwebide_frontend::testing::fake_backend::Call;
    let mounted = mount_test(interrupted_local_view);
    settle().await;
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("This run was interrupted.")
    );
    mounted.state.fake.calls.borrow_mut().clear();
    mounted.click_text("Resume");
    settle_run(&mounted).await;
    let calls = mounted.state.fake.calls.borrow();
    assert!(calls.contains(&Call::Request {
        method: "chat_tools"
    }));
    assert_eq!(
        calls
            .iter()
            .filter(|call| **call
                == Call::Request {
                    method: "persist_message"
                })
            .count(),
        2
    );
    let messages = mounted.state.fake.messages.borrow();
    assert_eq!(messages[&1].len(), 3);
    assert_eq!(messages[&1].iter().filter(|entry| matches!(entry, openwebide_core::ConversationEntry::Message(message) if message.role == Role::User)).count(), 1);
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Resumed reply")
    );
    let requests = mounted.state.fake.completion_requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages, vec![message(7, Role::User, "check")]);
}

#[wasm_bindgen_test]
async fn dismiss_hides_interrupted_local_notice_for_page_lifetime() {
    use leptos::prelude::*;
    let mounted = mount_test(interrupted_local_view);
    settle().await;
    mounted.click_text("Dismiss");
    settle().await;
    assert!(
        !mounted
            .root
            .text_content()
            .unwrap()
            .contains("This run was interrupted.")
    );
    mounted.state.chat.active_session.set(None);
    settle().await;
    mounted.state.chat.active_session.set(Some(1));
    settle().await;
    assert!(
        !mounted
            .root
            .text_content()
            .unwrap()
            .contains("This run was interrupted.")
    );
}

#[wasm_bindgen_test]
async fn stale_resume_uses_fresh_history_turns() {
    use leptos::prelude::*;
    use openwebide_core::{ChatCompletion, ChatResponse, ConversationEntry, ToolCall, ToolStep};
    use openwebide_frontend::conversation::ConversationItem;
    let mounted = mount_test(interrupted_local_view);
    settle().await;
    let mut interim = message(8, Role::Assistant, "Checking old.txt");
    interim.tool_calls = Some(vec![ToolCall {
        id: "wire".into(),
        name: "read_file".into(),
        arguments: "{}".into(),
    }]);
    mounted
        .state
        .fake
        .messages
        .borrow_mut()
        .get_mut(&1)
        .unwrap()
        .extend([
            ConversationEntry::Message(interim),
            ConversationEntry::ToolStep(ToolStep {
                timing: None,
                tool_call_id: "a7t1c0".into(),
                name: "read_file".into(),
                summary: "read old.txt".into(),
                ok: Some(true),
                result_summary: Some("old contents".into()),
                diff: None,
                anchor_message_id: 8,
                checkpoint: None,
            }),
        ]);
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .push_front(ChatCompletion {
            reasoning: String::new(),
            stop_reason: openwebide_core::StopReason::Complete,
            response: ChatResponse::ToolCalls(vec![ToolCall {
                id: "wire".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }]),
            preamble: String::new(),
            usage: None,
        });
    mounted.click_text("Resume");
    settle_run(&mounted).await;
    let ids: Vec<_> = mounted
        .state
        .chat
        .messages
        .get_untracked()
        .into_iter()
        .filter_map(|item| match item {
            ConversationItem::ToolStep { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec!["a7t2c0"]);
    assert!(mounted.state.chat.interrupted_run.get_untracked().is_none());
}

#[wasm_bindgen_test]
async fn stale_resume_rejects_final_reply_or_different_user_anchor() {
    use leptos::prelude::*;
    use openwebide_core::ConversationEntry;
    for role in [Role::Assistant, Role::User] {
        let mounted = mount_test(interrupted_local_view);
        settle().await;
        mounted
            .state
            .fake
            .messages
            .borrow_mut()
            .get_mut(&1)
            .unwrap()
            .push(ConversationEntry::Message(message(8, role, "new message")));
        mounted.click_text("Resume");
        settle_run(&mounted).await;
        assert!(
            mounted
                .state
                .chat
                .error
                .get_untracked()
                .unwrap()
                .contains("conversation changed")
        );
        assert!(mounted.state.fake.completion_requests.borrow().is_empty());
        assert_eq!(mounted.state.fake.messages.borrow()[&1].len(), 2);
    }
}

#[wasm_bindgen_test]
async fn failed_resume_keeps_recovery_after_missing_handle() {
    use leptos::prelude::*;
    let mounted = mount_test(interrupted_local_view);
    settle().await;
    let handle = mounted.state.projects.local_handles.get_untracked()[&1].clone();
    mounted
        .state
        .projects
        .local_handles
        .update(std::collections::HashMap::clear);
    mounted.click_text("Resume");
    settle_run(&mounted).await;
    assert!(
        mounted
            .state
            .chat
            .error
            .get_untracked()
            .unwrap()
            .contains("local directory handle not available")
    );
    assert!(!mounted.state.chat.streaming.get_untracked());
    assert!(mounted.state.chat.interrupted_run.get_untracked().is_some());
    mounted.state.projects.local_handles.update(|handles| {
        handles.insert(1, handle);
    });
    mounted.click_text("Resume");
    settle_run(&mounted).await;
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Resumed reply")
    );
    assert!(mounted.state.chat.interrupted_run.get_untracked().is_none());
}

#[wasm_bindgen_test]
async fn failed_model_resume_keeps_recovery_available() {
    use leptos::prelude::*;
    use openwebide_core::{ChatCompletion, ChatResponse};
    let mounted = mount_test(interrupted_local_view);
    settle().await;
    mounted.state.fake.scripted_completions.borrow_mut().clear();
    mounted.click_text("Resume");
    settle_run(&mounted).await;
    assert!(mounted.state.chat.error.get_untracked().is_some());
    assert!(!mounted.state.chat.streaming.get_untracked());
    assert!(mounted.state.chat.interrupted_run.get_untracked().is_some());
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .push_back(ChatCompletion {
            reasoning: String::new(),
            stop_reason: openwebide_core::StopReason::Complete,
            response: ChatResponse::Text("Retry reply".into()),
            preamble: String::new(),
            usage: None,
        });
    mounted.click_text("Resume");
    settle_run(&mounted).await;
    assert!(mounted.root.text_content().unwrap().contains("Retry reply"));
    assert!(mounted.state.chat.interrupted_run.get_untracked().is_none());
}

#[wasm_bindgen_test]
async fn cancelled_reasoning_freezes_elapsed_time_and_collapses_trace() {
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        chat_view(state)
    });
    settle().await;
    let chat = mounted.state.chat;
    chat.apply_event(RunEvent::ReasoningDelta {
        content: "checking".into(),
    });
    settle().await;
    openwebide_frontend::util::sleep_ms(1_100).await;
    assert_ne!(
        mounted.element(".tui-think-meta").text_content().as_deref(),
        Some("0s")
    );
    chat.apply_event(RunEvent::ReasoningDelta {
        content: " again".into(),
    });
    settle().await;
    assert_ne!(
        mounted.element(".tui-think-meta").text_content().as_deref(),
        Some("0s")
    );
    chat.apply_event(RunEvent::Cancelled);
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".tui-thinking-summary.active")
            .unwrap()
            .is_none()
    );
    assert!(
        mounted
            .root
            .query_selector(".ui-disclosure-content[hidden] .tui-thinking-pre")
            .unwrap()
            .is_some()
    );
    let summary = mounted
        .element(".tui-thinking-summary")
        .text_content()
        .unwrap();
    assert!(summary.contains("Thought"));
    assert!(summary.contains(" · "));
    openwebide_frontend::util::sleep_ms(1_100).await;
    assert_eq!(
        mounted
            .element(".tui-thinking-summary")
            .text_content()
            .unwrap(),
        summary
    );
    mounted.click(".tui-thinking-summary");
    settle().await;
    assert_eq!(
        mounted
            .element(".tui-thinking-pre")
            .text_content()
            .as_deref(),
        Some("checking again")
    );
}

#[wasm_bindgen_test]
async fn resumed_reasoning_deltas_render_literal_text() {
    use leptos::prelude::*;
    use openwebide_frontend::conversation::merge_snapshot;

    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        chat_view(state)
    });
    settle().await;
    mounted.state.chat.reasoning_active.set(true);
    mounted.state.chat.messages.update(|items| {
        merge_snapshot(
            items,
            &openwebide_core::RunSnapshot {
                reasoning: "first <tag> & ".into(),
                ..Default::default()
            },
        );
    });
    mounted.state.chat.apply_event(RunEvent::ReasoningDelta {
        content: "next <tag> & </think>".into(),
    });
    settle().await;
    mounted.click(".tui-thinking-summary");
    settle().await;
    assert_eq!(
        mounted
            .root
            .query_selector(".tui-thinking-pre")
            .unwrap()
            .unwrap()
            .text_content()
            .as_deref(),
        Some("first <tag> & next <tag> & </think>")
    );
}

#[wasm_bindgen_test]
async fn reasoning_stream_stays_collapsed_until_expanded_and_shows_cutoff() {
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        chat_view(state)
    });
    settle().await;
    let chat = mounted.state.chat;
    for content in ["checking </thi", "nk> &lt; carefully"] {
        chat.apply_event(RunEvent::ReasoningDelta {
            content: content.into(),
        });
        settle().await;
    }
    assert!(
        mounted
            .root
            .query_selector(".ui-disclosure-content[hidden] .tui-thinking-pre")
            .unwrap()
            .is_some()
    );
    mounted.click(".tui-thinking-summary");
    settle().await;
    let trace = mounted
        .root
        .query_selector(".tui-thinking-pre")
        .unwrap()
        .unwrap();
    assert_eq!(
        trace.text_content().as_deref(),
        Some("checking </think> &lt; carefully")
    );
    assert!(
        mounted
            .root
            .query_selector(".tui-thinking-summary.active")
            .unwrap()
            .is_some()
    );
    mounted.click(".tui-thinking-summary");
    settle().await;
    chat.apply_event(RunEvent::Delta {
        content: "answer".into(),
    });
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".tui-thinking-summary.active")
            .unwrap()
            .is_none()
    );
    assert!(
        mounted
            .root
            .query_selector(".ui-disclosure-content[hidden] .tui-thinking-pre")
            .unwrap()
            .is_some()
    );
    let content = openwebide_core::with_reasoning(
        "checking </think> &lt; carefully",
        &format!("answer{}", openwebide_core::REPLY_CUT_OFF_MARKER),
    );
    chat.apply_event(RunEvent::Done {
        message: message(9, Role::Assistant, &content),
    });
    settle().await;
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("[reply cut off: output token limit reached]")
    );
    assert!(
        mounted
            .root
            .query_selector(".tui-thinking-summary")
            .unwrap()
            .is_some()
    );
}

#[wasm_bindgen_test]
async fn local_completion_persists_reasoning_and_omits_prior_reasoning() {
    use openwebide_core::{ConversationEntry, StopReason};
    let mounted = mount_test(|state| {
        let view = interrupted_local_view(state.clone());
        let mut completions = state.fake.scripted_completions.borrow_mut();
        let completion = completions.front_mut().unwrap();
        completion.reasoning = "new reasoning".into();
        completion.stop_reason = StopReason::Length;
        state
            .fake
            .messages
            .borrow_mut()
            .get_mut(&1)
            .unwrap()
            .insert(
                0,
                ConversationEntry::Message(message(
                    6,
                    Role::Assistant,
                    "<think>old reasoning</think>earlier",
                )),
            );
        view
    });
    settle().await;
    mounted.click_text("Resume");
    settle_run(&mounted).await;
    let expected = format!(
        "<think>new reasoning</think>Resumed reply{}",
        openwebide_core::REPLY_CUT_OFF_MARKER
    );
    assert!(
        matches!(mounted.state.fake.messages.borrow()[&1].last(), Some(ConversationEntry::Message(m)) if m.content == expected)
    );
    assert_eq!(
        mounted.state.fake.completion_requests.borrow()[0].messages[0].content,
        "earlier"
    );
    mounted.click(".tui-assistant:last-child .tui-thinking-summary");
    settle().await;
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("new reasoning")
    );
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("[reply cut off: output token limit reached]")
    );
}

#[wasm_bindgen_test]
async fn streaming_rows_keep_nodes_and_expanded_reasoning_through_finalization() {
    use openwebide_frontend::conversation::ConversationItem;
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_session();
        chat_view(state)
    });
    settle().await;
    let chat = mounted.state.chat;
    chat.messages
        .install_history(vec![ConversationItem::Message(message(
            5,
            Role::Assistant,
            "historical **answer**",
        ))]);
    chat.apply_event(RunEvent::ReasoningDelta {
        content: "reason".into(),
    });
    settle().await;
    let historical = mounted
        .root
        .query_selector(".tui-assistant")
        .unwrap()
        .unwrap();
    let historical_markdown = historical.query_selector(".markdown p").unwrap().unwrap();
    let live = mounted
        .root
        .query_selector(".tui-assistant:last-child")
        .unwrap()
        .unwrap();
    chat.apply_event(RunEvent::Delta {
        content: "first".into(),
    });
    settle().await;
    mounted.click(".tui-assistant:last-child .tui-thinking-summary");
    settle().await;
    for content in [" second", " third"] {
        chat.apply_event(RunEvent::Delta {
            content: content.into(),
        });
        settle().await;
        assert!(
            live.is_same_node(
                mounted
                    .root
                    .query_selector(".tui-assistant:last-child")
                    .unwrap()
                    .as_ref()
                    .map(AsRef::as_ref)
            )
        );
        assert!(live.query_selector(".tui-thinking-pre").unwrap().is_some());
    }
    chat.apply_event(RunEvent::Done {
        message: message(8, Role::Assistant, "<think>reason updated</think>final"),
    });
    settle().await;
    assert!(
        live.is_same_node(
            mounted
                .root
                .query_selector(".tui-assistant:last-child")
                .unwrap()
                .as_ref()
                .map(AsRef::as_ref)
        )
    );
    assert_eq!(
        live.query_selector(".tui-thinking-pre")
            .unwrap()
            .unwrap()
            .text_content()
            .as_deref(),
        Some("reason updated")
    );
    chat.apply_event(RunEvent::Message {
        message: message(8, Role::Assistant, "<think>reason updated</think>other"),
    });
    settle().await;
    assert_eq!(
        live.query_selector(".markdown")
            .unwrap()
            .unwrap()
            .text_content()
            .as_deref(),
        Some("other\n")
    );
    assert!(
        historical.is_same_node(
            mounted
                .root
                .query_selector(".tui-assistant")
                .unwrap()
                .as_ref()
                .map(AsRef::as_ref)
        )
    );
    assert!(
        historical_markdown.is_same_node(
            historical
                .query_selector(".markdown p")
                .unwrap()
                .as_ref()
                .map(AsRef::as_ref)
        )
    );
}

#[wasm_bindgen_test]
async fn hidden_tool_call_message_becomes_visible_and_user_text_updates() {
    use openwebide_frontend::conversation::ConversationItem;
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_session();
        chat_view(state)
    });
    settle().await;
    let mut hidden = message(8, Role::Assistant, "");
    hidden.tool_calls = Some(vec![]);
    let chat = mounted.state.chat;
    chat.messages.set(vec![
        ConversationItem::Message(message(7, Role::User, "first")),
        ConversationItem::Message(hidden.clone()),
    ]);
    settle().await;
    let user = mounted.root.query_selector(".tui-user").unwrap().unwrap();
    assert!(
        mounted
            .root
            .query_selector(".tui-assistant")
            .unwrap()
            .is_none()
    );
    hidden.content = "visible".into();
    chat.apply_event(RunEvent::Message { message: hidden });
    chat.apply_event(RunEvent::Message {
        message: message(7, Role::User, "other"),
    });
    settle().await;
    assert!(
        user.is_same_node(
            mounted
                .root
                .query_selector(".tui-user")
                .unwrap()
                .as_ref()
                .map(AsRef::as_ref)
        )
    );
    assert!(user.text_content().unwrap().contains("other"));
    assert!(
        mounted
            .root
            .query_selector(".tui-assistant")
            .unwrap()
            .unwrap()
            .text_content()
            .unwrap()
            .contains("visible")
    );
}

#[wasm_bindgen_test]
async fn saved_and_streamed_run_context_opens_from_prompt_menu_in_every_scope() {
    use leptos::prelude::*;
    use openwebide_core::{ConversationEntry, WorkspaceMode};
    use wasm_bindgen::JsCast;
    for mode in [
        Some(WorkspaceMode::Local),
        Some(WorkspaceMode::Remote),
        None,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_connection();
            state.seed_session();
            if let Some(mode) = mode {
                state.seed_project();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
            } else {
                state
                    .chat
                    .sessions
                    .update(|sessions| sessions[0].project_id = None);
            }
            state.fake.messages.borrow_mut().insert(
                1,
                vec![
                    ConversationEntry::Message(message(
                        8,
                        Role::System,
                        &format!("{}Root rule", openwebide_core::RUN_CONTEXT_PREFIX),
                    )),
                    ConversationEntry::Message(message(9, Role::User, "First prompt")),
                ],
            );
            view! { <style>{include_str!("../../styles.css")}</style>{chat_view(state)} }
        });
        super::support::wait_until("saved prompt context", || {
            mounted
                .state
                .chat
                .run_contexts
                .with_untracked(|contexts| contexts.contains_key(&9))
        })
        .await;
        assert!(
            mounted
                .root
                .query_selector(".ui-disclosure-panel.tui-thinking-box")
                .unwrap()
                .is_none()
        );
        assert!(
            !mounted
                .element(".messages")
                .text_content()
                .unwrap()
                .contains("Root rule")
        );
        mounted.click(".tui-user button[aria-label='Message actions']");
        settle().await;
        mounted.click("button[aria-label='Run context'][data-message-id='9']");
        let document = web_sys::window().unwrap().document().unwrap();
        super::support::wait_until("run context dialog", || {
            document
                .query_selector(".run-context-content")
                .unwrap()
                .is_some()
        })
        .await;
        let content = document
            .query_selector(".run-context-content")
            .unwrap()
            .unwrap();
        assert_eq!(content.text_content().as_deref(), Some("Root rule"));
        mounted.state.chat.apply_event(RunEvent::Message {
            message: message(
                10,
                Role::System,
                &format!(
                    "{}Nested rule <script>data</script>",
                    openwebide_core::RUN_CONTEXT_PREFIX
                ),
            ),
        });
        super::support::wait_until("updated context", || {
            content.text_content().unwrap().contains("Nested rule")
        })
        .await;
        assert!(content.query_selector("script").unwrap().is_none());
        assert!(
            !content
                .text_content()
                .unwrap()
                .contains(openwebide_core::RUN_CONTEXT_PREFIX)
        );
        document
            .query_selector("[role='dialog'] .modal-footer .btn")
            .unwrap()
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>()
            .click();
        super::support::wait_until("composer focus after context", || {
            document.active_element().is_some_and(|active| {
                active.is_same_node(Some(mounted.element(".composer-input").as_ref()))
            })
        })
        .await;
        mounted.state.chat.apply_event(RunEvent::Message {
            message: message(11, Role::User, "Second prompt"),
        });
        mounted.state.chat.apply_event(RunEvent::Message {
            message: message(
                12,
                Role::System,
                &format!("{}Second rule", openwebide_core::RUN_CONTEXT_PREFIX),
            ),
        });
        settle().await;
        mounted
            .root
            .query_selector_all(".tui-user button[aria-label='Message actions']")
            .unwrap()
            .item(1)
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>()
            .click();
        settle().await;
        mounted.click("button[aria-label='Run context'][data-message-id='11']");
        super::support::wait_until("second prompt context", || {
            document
                .query_selector(".run-context-content")
                .unwrap()
                .is_some_and(|content| content.text_content().unwrap() == "Second rule")
        })
        .await;
        mounted
            .state
            .auth
            .generation
            .update(|generation| *generation += 1);
        super::support::wait_until("account change closes context", || {
            document
                .query_selector("[role='dialog']")
                .unwrap()
                .is_none()
        })
        .await;
    }
}

#[wasm_bindgen_test]
async fn markdown_tables_render_in_history_and_streaming_in_both_modes() {
    use leptos::prelude::*;
    use openwebide_core::{ConversationEntry, WorkspaceMode};
    let table = "Summary with `code`.\n\n| Name | Count |\n| :--- | ---: |\n| `src` | 2 |\n";
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_connection();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.fake.messages.borrow_mut().insert(
                1,
                vec![ConversationEntry::Message(message(
                    8,
                    Role::Assistant,
                    table,
                ))],
            );
            chat_view(state)
        });
        settle().await;
        let assert_table = |selector: &str| {
            let body = mounted.element(selector);
            assert!(
                body.query_selector(".markdown-table table")
                    .unwrap()
                    .is_some()
            );
            let header = body.query_selector("th:last-child").unwrap().unwrap();
            assert_eq!(header.text_content().as_deref(), Some("Count"));
            assert_eq!(
                header.get_attribute("style").as_deref(),
                Some("text-align: right")
            );
            assert_eq!(
                body.query_selector("td code")
                    .unwrap()
                    .unwrap()
                    .text_content()
                    .as_deref(),
                Some("src")
            );
        };
        assert_table(".tui-assistant-body");
        mounted.state.chat.apply_event(RunEvent::Delta {
            content: table.into(),
        });
        settle().await;
        assert_table(".tui-assistant:last-child .tui-assistant-body");
    }
}

#[wasm_bindgen_test]
async fn run_context_menus_keep_narrow_history_clear_and_dialogs_outside_the_pane() {
    use leptos::prelude::*;
    use openwebide_core::{ConversationEntry, WorkspaceMode};
    use wasm_bindgen::JsCast;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.seed_connection();
            state.seed_session();
            state.fake.messages.borrow_mut().insert(
                1,
                (0..8)
                    .flat_map(|index| {
                        [
                            ConversationEntry::Message(message(
                                index * 3 + 1,
                                Role::User,
                                &format!("Prompt {index}"),
                            )),
                            ConversationEntry::Message(message(
                                index * 3 + 2,
                                Role::System,
                                &format!(
                                    "{}{}",
                                    openwebide_core::RUN_CONTEXT_PREFIX,
                                    "A long context rule that wraps in a narrow pane. ".repeat(20)
                                ),
                            )),
                            ConversationEntry::Message(message(
                                index * 3 + 3,
                                Role::Assistant,
                                &"An answer with enough content to overflow the history viewport. "
                                    .repeat(20),
                            )),
                        ]
                    })
                    .collect(),
            );
            view! { <style>{include_str!("../../styles.css")}</style><style>".context-layout-regression .chat-pane { width: 100% !important; } .context-layout-regression .messages { height: 180px; flex: none; width: 100%; box-sizing: border-box; }"</style><div class="context-layout-regression">{chat_view(state)}</div> }
        });
        super::support::wait_until("eight prompt contexts", || {
            mounted
                .state
                .chat
                .run_contexts
                .with_untracked(|contexts| contexts.len() == 8)
        })
        .await;
        for width in [220, 360, 900] {
            mounted
                .element(".context-layout-regression")
                .style()
                .set_property("width", &format!("{width}px"))
                .unwrap();
            settle().await;
            let stream = mounted.element(".messages.tui-stream");
            assert!(stream.scroll_height() > stream.client_height());
            assert!(
                stream
                    .query_selector(".ui-disclosure-panel.tui-thinking-box")
                    .unwrap()
                    .is_none()
            );
            assert!(stream.scroll_width() <= stream.client_width() + 1);
            assert_eq!(stream.query_selector_all(".tui-user").unwrap().length(), 8);
            mounted.click(".tui-user button[aria-label='Message actions']");
            settle().await;
            mounted.click("button[aria-label='Run context'][data-message-id='1']");
            let document = web_sys::window().unwrap().document().unwrap();
            super::support::wait_until("context modal escapes pane", || {
                document
                    .query_selector(".run-context-content")
                    .unwrap()
                    .is_some()
            })
            .await;
            let content = document
                .query_selector(".run-context-content")
                .unwrap()
                .unwrap()
                .unchecked_into::<web_sys::HtmlElement>();
            assert!(content.get_bounding_client_rect().height() > 20.0);
            assert!(!stream.contains(Some(content.as_ref())));
            document
                .query_selector("[role='dialog'] .modal-footer .btn")
                .unwrap()
                .unwrap()
                .unchecked_into::<web_sys::HtmlElement>()
                .click();
            super::support::wait_until("closed run context", || {
                document
                    .query_selector(".run-context-content")
                    .unwrap()
                    .is_none()
            })
            .await;
        }
    }
}

#[wasm_bindgen_test]
async fn reload_during_unfinished_local_tool_offers_resume_without_reexecuting_it() {
    use leptos::prelude::*;
    use openwebide_core::{ConversationEntry, ToolCall, ToolStep, UNRECORDED_TOOL_RESULT};
    for recorded_step in [false, true] {
        let mounted = mount_test(move |state| {
            let pane = interrupted_local_view(state.clone());
            let mut assistant = message(8, Role::Assistant, "checking");
            assistant.tool_calls = Some(vec![ToolCall {
                id: "wire-write".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"file.txt","content":"must not replay"}"#.into(),
            }]);
            state
                .fake
                .messages
                .borrow_mut()
                .get_mut(&1)
                .unwrap()
                .push(ConversationEntry::Message(assistant));
            if recorded_step {
                state.fake.messages.borrow_mut().get_mut(&1).unwrap().push(
                    ConversationEntry::ToolStep(ToolStep {
                        timing: None,
                        tool_call_id: "a7t1c0".into(),
                        name: "write_file".into(),
                        summary: "write file.txt".into(),
                        ok: None,
                        result_summary: None,
                        diff: None,
                        anchor_message_id: 8,
                        checkpoint: None,
                    }),
                );
            }
            pane
        });
        settle().await;
        assert_eq!(
            mounted
                .state
                .chat
                .interrupted_run
                .get_untracked()
                .unwrap()
                .first_turn,
            2
        );
        mounted.click_text("Resume");
        settle_run(&mounted).await;
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Resumed reply")
        );
        assert!(
            !mounted
                .root
                .text_content()
                .unwrap()
                .contains("Allow write_file")
        );
        let requests = mounted.state.fake.completion_requests.borrow();
        assert_eq!(requests.len(), 1);
        let tool = requests[0]
            .messages
            .iter()
            .find(|message| message.role == Role::Tool)
            .unwrap();
        assert_eq!(tool.tool_call_id.as_deref(), Some("wire-write"));
        assert_eq!(tool.content, UNRECORDED_TOOL_RESULT);
        assert_eq!(mounted.state.fake.messages.borrow()[&1].iter().filter(|entry| matches!(entry, ConversationEntry::Message(message) if message.role == Role::User)).count(), 1);
    }
}

#[wasm_bindgen_test]
async fn pending_thinking_indicator_survives_system_context_until_model_tokens_arrive() {
    use leptos::prelude::*;
    use openwebide_core::{Role, RunEvent, WorkspaceMode};

    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_connection();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            chat_view(state)
        });
        settle().await;
        let chat = mounted.state.chat;

        // Start a prompt send
        chat.streaming.set(true);
        chat.messages
            .push_optimistic_user(1, "Investigate this issue".into());
        settle().await;

        // The optimistic thinking box should be visible
        assert!(
            mounted.has_element(".tui-thinking-box"),
            "Pending thinking box should be visible immediately on send"
        );
        let text = mounted.root.text_content().unwrap();
        assert!(
            text.contains("Waiting for the model"),
            "Should show waiting for model prompt: {text}"
        );

        // System context arrives (e.g. startup context, run context, compaction)
        chat.apply_event(RunEvent::Message {
            message: message(
                2,
                Role::System,
                &format!("{}Context rules", openwebide_core::RUN_CONTEXT_PREFIX),
            ),
        });
        settle().await;

        // CRITICAL CHECK: The thinking box must NOT disappear when system context arrives!
        assert!(
            mounted.has_element(".tui-thinking-box"),
            "Pending thinking box must survive system context message"
        );
        let text = mounted.root.text_content().unwrap();
        assert!(
            text.contains("Waiting for the model"),
            "Should still show waiting for model: {text}"
        );

        // Server saves user message in place
        chat.apply_event(RunEvent::Message {
            message: message(1, Role::User, "Investigate this issue"),
        });
        settle().await;

        // The thinking box must STILL be visible while awaiting tokens
        assert!(
            mounted.has_element(".tui-thinking-box"),
            "Pending thinking box must survive server user message confirmation"
        );

        // First token arrives from the model
        chat.apply_event(RunEvent::Delta {
            content: "Here is what I found".into(),
        });
        settle().await;

        // Pending thinking box is replaced by the actual assistant message
        let text = mounted.root.text_content().unwrap();
        assert!(
            !text.contains("Waiting for the model"),
            "Waiting message should be replaced by real assistant message: {text}"
        );
        assert!(
            text.contains("Here is what I found"),
            "Assistant text should be rendered: {text}"
        );

        // Done finishes turn 1
        chat.apply_event(RunEvent::Done {
            message: message(3, Role::Assistant, "Here is what I found"),
        });
        chat.streaming.set(false);
        settle().await;

        // Turn 2 with a tool call
        chat.streaming.set(true);
        chat.apply_event(RunEvent::ToolCall {
            id: "t1".into(),
            name: "read_file".into(),
            summary: "read file.rs".into(),
        });
        settle().await;

        // While tool is running, pending thinking box should NOT show
        assert!(
            !mounted
                .root
                .text_content()
                .unwrap()
                .contains("Waiting for the model"),
            "Should not show pending thinking while tool is actively executing"
        );

        // Tool completes
        chat.apply_event(RunEvent::ToolResult {
            id: "t1".into(),
            name: "read_file".into(),
            ok: true,
            summary: "file contents".into(),
            diff: None,
        });
        settle().await;

        // Waiting for turn 2 model response: pending thinking box shows again
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Waiting for the model"),
            "Pending thinking box should show while awaiting turn 2 model response"
        );

        // Turn 2 tokens arrive
        chat.apply_event(RunEvent::Delta {
            content: "Got the file".into(),
        });
        settle().await;
        assert!(
            !mounted
                .root
                .text_content()
                .unwrap()
                .contains("Waiting for the model"),
            "Waiting message should disappear once turn 2 tokens arrive"
        );

        chat.streaming.set(false);
        settle().await;
    }
}
