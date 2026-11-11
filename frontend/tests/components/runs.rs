use std::rc::Rc;

use leptos::prelude::*;
use openwebide_core::{
    BridgeClientMessage, BridgeServerMessage, ChatMessage, Role, RunEvent, RunInfo, RunItem,
    RunRejectCode, RunSnapshot, RunStep, ToolStreamChunk,
};
use openwebide_frontend::{
    bridge::{BridgeConfig, BridgeConn},
    conversation::ConversationItem,
    local_agent::BrowserLlmProvider,
    testing::{fake_backend::Call, fake_transport::FakeTransport},
    util::sleep_ms,
};
use wasm_bindgen_test::*;

use super::support::{Mounted, chat_view, mount_test, settle, wait_until};

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

fn fixture() -> (Mounted, Rc<FakeTransport>) {
    let fake = Rc::new(FakeTransport::default());
    let transport = fake.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        state.bridge.set(Some(BridgeConn::with_transport(
            BridgeConfig::new("ws://test"),
            transport,
            Rc::new(|| Box::pin(async { Ok("token".into()) })),
        )));
        let chat = state.chat;
        let pane = chat_view(state);
        view! {
            {pane}
            <Show when=move || chat.notice.get().is_some()><div class="toast toast-info">{move || chat.notice.get().unwrap_or_default()}</div></Show>
            <Show when=move || chat.error.get().is_some()><div class="toast">{move || chat.error.get().unwrap_or_default()}</div></Show>
        }
    });
    (mounted, fake)
}

async fn ready(fake: &FakeTransport) {
    settle().await;
    fake.reply(BridgeServerMessage::HelloOk {
        user_id: Some(1),
        protocol: 1,
        runs: true,
    });
    sleep_ms(25).await;
    settle().await;
}

fn no_runs(fake: &FakeTransport) {
    fake.reply(BridgeServerMessage::Runs {
        session_id: 1,
        runs: vec![],
    });
}

async fn send(mounted: &Mounted, fake: &FakeTransport) -> String {
    mounted.input("hello");
    mounted.key("Enter", "Enter", false);
    settle().await;
    fake.sent()
        .into_iter()
        .find_map(|message| match message {
            BridgeClientMessage::RunStart { run_id, .. } => Some(run_id),
            _ => None,
        })
        .unwrap()
}

fn event(fake: &FakeTransport, run_id: &str, seq: u64, event: RunEvent) {
    fake.reply(BridgeServerMessage::RunEvent {
        run_id: run_id.into(),
        seq,
        event,
    });
}

fn sse_calls(mounted: &Mounted) -> usize {
    mounted
        .state
        .fake
        .calls
        .borrow()
        .iter()
        .filter(|call| matches!(call, Call::SendMessage { .. }))
        .count()
}

fn close(mounted: &Mounted) {
    mounted.state.bridge.get_untracked().unwrap().close();
}

#[wasm_bindgen_test]
async fn ws_prompt_renders_reply_and_done_clears_streaming() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    assert_eq!(sse_calls(&mounted), 0);
    let preferences = fake.sent().into_iter().find_map(|message| match message {
        BridgeClientMessage::RunStart {
            browser_preferences,
            ..
        } => browser_preferences,
        _ => None,
    });
    assert_eq!(
        preferences,
        openwebide_frontend::browser_preferences::capture()
    );
    assert!(preferences.is_some());
    event(
        &fake,
        &run_id,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "hello"),
        },
    );
    event(
        &fake,
        &run_id,
        2,
        RunEvent::Delta {
            content: "reply".into(),
        },
    );
    settle().await;
    assert!(mounted.root.text_content().unwrap().contains("reply"));
    event(
        &fake,
        &run_id,
        3,
        RunEvent::Done {
            message: message(8, Role::Assistant, "reply"),
        },
    );
    settle().await;
    assert!(!mounted.state.chat.streaming.get_untracked());
    assert!(mounted.state.chat.active_run.get_untracked().is_none());
    assert_eq!(
        mounted
            .root
            .text_content()
            .unwrap()
            .matches("reply")
            .count(),
        1
    );
    close(&mounted);
}

#[wasm_bindgen_test]
async fn rejection_falls_back_or_shows_error_as_specified() {
    for code in [
        RunRejectCode::ProjectUnavailable,
        RunRejectCode::Unauthorized,
        RunRejectCode::Unavailable,
        RunRejectCode::Busy,
        RunRejectCode::PlanFailed,
    ] {
        let (mounted, fake) = fixture();
        ready(&fake).await;
        no_runs(&fake);
        let run_id = send(&mounted, &fake).await;
        fake.reply(BridgeServerMessage::RunRejected {
            run_id,
            code: code.clone(),
            message: "missing".into(),
        });
        settle().await;
        match code {
            RunRejectCode::ProjectUnavailable => {
                assert_eq!(sse_calls(&mounted), 1);
                assert_eq!(
                    mounted.element(".toast-info").text_content().unwrap(),
                    "Bridge can't see this project (missing); running through the server instead."
                );
            }
            RunRejectCode::Unauthorized | RunRejectCode::Unavailable => {
                assert_eq!(sse_calls(&mounted), 1);
                assert!(mounted.state.chat.notice.get_untracked().is_none());
            }
            _ => {
                assert_eq!(sse_calls(&mounted), 0);
                assert_eq!(mounted.element(".toast").text_content().unwrap(), "missing");
            }
        }
        close(&mounted);
    }
}

#[wasm_bindgen_test]
async fn connecting_times_out_then_uses_sse() {
    let (mounted, fake) = fixture();
    settle().await;
    mounted.input("hello");
    mounted.key("Enter", "Enter", false);
    settle().await;
    assert_eq!(sse_calls(&mounted), 0);
    sleep_ms(60).await;
    settle().await;
    assert_eq!(sse_calls(&mounted), 1);
    assert!(
        !fake
            .sent()
            .iter()
            .any(|message| matches!(message, BridgeClientMessage::RunStart { .. }))
    );
    close(&mounted);
}

#[wasm_bindgen_test]
async fn stop_and_permission_use_the_current_ws_run() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    event(
        &fake,
        &run_id,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "hello"),
        },
    );
    event(
        &fake,
        &run_id,
        2,
        RunEvent::PermissionRequest {
            id: "a7t0c0".into(),
            name: "write_file".into(),
            summary: "file".into(),
            diff: None,
            note: None,
        },
    );
    settle().await;
    mounted.key("y", "KeyY", true);
    settle().await;
    assert!(fake.sent().contains(&BridgeClientMessage::RunPermission {
        run_id: run_id.clone(),
        tool_call_id: "a7t0c0".into(),
        approved: true
    }));
    mounted.click(".btn.stop");
    settle().await;
    assert!(fake.sent().contains(&BridgeClientMessage::RunCancel {
        run_id: run_id.clone()
    }));
    assert!(
        !mounted
            .state
            .chat
            .messages
            .get_untracked()
            .iter()
            .any(|item| matches!(item, ConversationItem::Stopped { .. }))
    );
    event(&fake, &run_id, 3, RunEvent::Cancelled);
    event(&fake, &run_id, 3, RunEvent::Cancelled);
    settle().await;
    assert_eq!(
        mounted
            .state
            .chat
            .messages
            .get_untracked()
            .iter()
            .filter(|item| matches!(item, ConversationItem::Stopped { .. }))
            .count(),
        1
    );
    close(&mounted);
}

#[wasm_bindgen_test]
async fn history_discovers_run_and_merges_awaiting_snapshot_twice() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    assert!(
        fake.sent()
            .contains(&BridgeClientMessage::RunList { session_id: 1 })
    );
    fake.reply(BridgeServerMessage::Runs {
        session_id: 1,
        runs: vec![RunInfo {
            run_id: "running".into(),
            session_id: 1,
            running: true,
            seq: 4,
            started_at: 0,
        }],
    });
    settle().await;
    assert!(fake.sent().contains(&BridgeClientMessage::RunAttach {
        run_id: "running".into(),
        last_seq: None
    }));
    let snapshot = RunSnapshot {
        items: vec![
            RunItem::Message(message(7, Role::User, "hello")),
            RunItem::Step(RunStep {
                timing: None,
                id: "a7t0c0".into(),
                name: "write_file".into(),
                summary: "file".into(),
                awaiting_permission: true,
                result: None,
                diff: None,
                note: None,
            }),
        ],
        ..Default::default()
    };
    let mut previous_row: Option<web_sys::Element> = None;
    for _ in 0..2 {
        fake.reply(BridgeServerMessage::RunSnapshot {
            run_id: "running".into(),
            session_id: 1,
            seq: 4,
            snapshot: snapshot.clone(),
        });
        settle().await;
        let row = mounted
            .root
            .query_selector(".tui-tool-box")
            .unwrap()
            .unwrap();
        if let Some(previous) = previous_row.replace(row.clone()) {
            assert!(previous.is_same_node(Some(&row)));
        }
        assert_eq!(mounted.state.chat.messages.get_untracked().len(), 2);
        assert_eq!(
            mounted
                .state
                .chat
                .awaiting_step_id
                .get_untracked()
                .as_deref(),
            Some("a7t0c0")
        );
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("? Allow write_file")
        );
        mounted.element(".btn-y");
    }
    close(&mounted);
}

#[wasm_bindgen_test]
async fn missing_attach_routes_to_run_and_recovers_history() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    fake.reply(BridgeServerMessage::Runs {
        session_id: 1,
        runs: vec![RunInfo {
            run_id: "missing".into(),
            session_id: 1,
            running: true,
            seq: 1,
            started_at: 0,
        }],
    });
    settle().await;
    let terminal_errors = Rc::new(std::cell::Cell::new(0));
    let errors = terminal_errors.clone();
    mounted
        .state
        .bridge
        .get_untracked()
        .unwrap()
        .register_terminal(Rc::new(move |_| errors.set(errors.get() + 1)));
    fake.reply(BridgeServerMessage::Error {
        id: "missing".into(),
        message: "run not found".into(),
    });
    settle().await;
    assert!(!mounted.state.chat.streaming.get_untracked());
    assert!(mounted.state.chat.active_run.get_untracked().is_none());
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("The run was interrupted.")
    );
    assert_eq!(terminal_errors.get(), 0);
    fake.reply(BridgeServerMessage::Error {
        id: "shell".into(),
        message: "terminal error".into(),
    });
    assert_eq!(terminal_errors.get(), 1);
    close(&mounted);
}

#[wasm_bindgen_test]
async fn local_completions_stream_cancel_and_report_disconnect() {
    use futures::StreamExt;
    use openwebide_llm::LlmProvider;
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let provider = BrowserLlmProvider::new(
        mounted.state.api,
        openwebide_core::ProviderKind::Ollama,
        mounted.state.bridge.get_untracked(),
    );
    let request = openwebide_core::ChatRequest {
        model_settings: Default::default(),
        connection_id: 1,
        model: None,
        system_prompt: None,
        messages: vec![],
        tools: vec![],
    };
    let unpolled = provider.chat_tools_stream(&request);
    let unpolled_id = fake
        .sent()
        .into_iter()
        .find_map(|message| match message {
            BridgeClientMessage::CompletionStart { id, .. } => Some(id),
            _ => None,
        })
        .unwrap();
    drop(unpolled);
    assert!(
        fake.sent()
            .contains(&BridgeClientMessage::CompletionCancel { id: unpolled_id })
    );
    let mut stream = provider.chat_tools_stream(&request);
    let id = fake
        .sent()
        .into_iter()
        .filter_map(|message| match message {
            BridgeClientMessage::CompletionStart { id, .. } => Some(id),
            _ => None,
        })
        .next_back()
        .unwrap();
    fake.reply(BridgeServerMessage::CompletionChunk {
        id: id.clone(),
        chunk: ToolStreamChunk::Delta("token".into()),
    });
    assert!(
        matches!(stream.next().await, Some(Ok(ToolStreamChunk::Delta(text))) if text == "token")
    );
    let response = openwebide_core::ChatResponse::ToolCalls(vec![openwebide_core::ToolCall {
        id: "read-1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({ "path": "note.txt" }).to_string(),
    }]);
    fake.reply(BridgeServerMessage::CompletionChunk {
        id: id.clone(),
        chunk: ToolStreamChunk::Response(response.clone()),
    });
    assert_eq!(
        stream.next().await.unwrap().unwrap(),
        ToolStreamChunk::Response(response)
    );
    drop(stream);
    assert!(
        fake.sent()
            .contains(&BridgeClientMessage::CompletionCancel { id })
    );
    let mut stream = provider.chat_tools_stream(&request);
    let id = fake
        .sent()
        .into_iter()
        .filter_map(|message| match message {
            BridgeClientMessage::CompletionStart { id, .. } => Some(id),
            _ => None,
        })
        .next_back()
        .unwrap();
    fake.reply(BridgeServerMessage::CompletionEnd {
        id,
        error: Some("provider failed".into()),
    });
    assert!(
        matches!(stream.next().await, Some(Err(openwebide_llm::ProviderError::Http(error))) if error == "provider failed")
    );
    assert!(stream.next().await.is_none());
    drop(stream);
    let mut stream = provider.chat_tools_stream(&request);
    fake.disconnect();
    settle().await;
    assert!(
        matches!(stream.next().await, Some(Err(openwebide_llm::ProviderError::Http(error))) if error == "bridge disconnected")
    );
    drop(stream);
    close(&mounted);
}

#[wasm_bindgen_test]
async fn reconnect_attaches_last_sequence_and_ignores_replayed_events() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    event(
        &fake,
        &run_id,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "hello"),
        },
    );
    event(
        &fake,
        &run_id,
        2,
        RunEvent::Delta {
            content: "partial".into(),
        },
    );
    fake.disconnect();
    settle().await;
    assert!(mounted.state.chat.streaming.get_untracked());
    sleep_ms(1100).await;
    ready(&fake).await;
    assert!(fake.sent().contains(&BridgeClientMessage::RunAttach {
        run_id: run_id.clone(),
        last_seq: Some(2)
    }));
    event(
        &fake,
        &run_id,
        2,
        RunEvent::Delta {
            content: "partial".into(),
        },
    );
    event(
        &fake,
        &run_id,
        3,
        RunEvent::Delta {
            content: " reply".into(),
        },
    );
    settle().await;
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("partial reply")
    );
    assert!(
        !mounted
            .root
            .text_content()
            .unwrap()
            .contains("partialpartial")
    );
    event(
        &fake,
        &run_id,
        4,
        RunEvent::Done {
            message: message(8, Role::Assistant, "partial reply"),
        },
    );
    settle().await;
    assert!(!mounted.state.chat.streaming.get_untracked());
    close(&mounted);
}

#[wasm_bindgen_test]
async fn adversarial_permission_during_disconnect_remains_actionable() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    event(
        &fake,
        &run_id,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "hello"),
        },
    );
    event(
        &fake,
        &run_id,
        2,
        RunEvent::PermissionRequest {
            id: "a7t0c0".into(),
            name: "write_file".into(),
            summary: "file".into(),
            diff: None,
            note: None,
        },
    );
    settle().await;
    fake.disconnect();
    settle().await;
    mounted.key("y", "KeyY", true);
    settle().await;
    assert_eq!(
        mounted
            .state
            .chat
            .awaiting_step_id
            .get_untracked()
            .as_deref(),
        Some("a7t0c0")
    );
    sleep_ms(1100).await;
    ready(&fake).await;
    let decision_sent = fake.sent().iter().any(|m| {
        matches!(m,
        BridgeClientMessage::RunPermission { tool_call_id, .. } if tool_call_id == "a7t0c0")
    });
    let actionable = mounted
        .state
        .chat
        .awaiting_step_id
        .get_untracked()
        .is_some();
    close(&mounted);
    assert!(
        decision_sent || actionable,
        "approval was not sent and the approval UI disappeared after reconnect"
    );
}

#[wasm_bindgen_test]
async fn adversarial_connection_replacement_retains_run_callback() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    event(
        &fake,
        &run_id,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "hello"),
        },
    );
    close(&mounted);
    let replacement = Rc::new(FakeTransport::default());
    mounted.state.bridge.set(Some(BridgeConn::with_transport(
        BridgeConfig::new("ws://test/"),
        replacement.clone(),
        Rc::new(|| Box::pin(async { Ok("token".into()) })),
    )));
    ready(&replacement).await;
    assert!(
        replacement.sent().iter().any(
            |m| matches!(m, BridgeClientMessage::RunAttach { run_id: id, .. } if id == &run_id)
        )
    );
    event(
        &replacement,
        &run_id,
        2,
        RunEvent::Done {
            message: message(8, Role::Assistant, "replacement reply"),
        },
    );
    settle().await;
    let rendered = mounted
        .root
        .text_content()
        .unwrap()
        .contains("replacement reply");
    close(&mounted);
    assert!(
        rendered,
        "new connection sent attach but discarded the reply"
    );
}

#[wasm_bindgen_test]
async fn adversarial_snapshot_autoapproval_is_idempotent() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    mounted.state.chat.approval_mode.update(|m| {
        m.insert(1, openwebide_agent::policy::ApprovalMode::AlwaysForSession);
    });
    fake.reply(BridgeServerMessage::Runs {
        session_id: 1,
        runs: vec![RunInfo {
            run_id: "running".into(),
            session_id: 1,
            running: true,
            seq: 2,
            started_at: 0,
        }],
    });
    settle().await;
    let snapshot = RunSnapshot {
        items: vec![
            RunItem::Message(message(7, Role::User, "hello")),
            RunItem::Step(RunStep {
                timing: None,
                id: "a7t0c0".into(),
                name: "write_file".into(),
                summary: "file".into(),
                awaiting_permission: true,
                result: None,
                diff: None,
                note: None,
            }),
        ],
        ..Default::default()
    };
    for _ in 0..2 {
        fake.reply(BridgeServerMessage::RunSnapshot {
            run_id: "running".into(),
            session_id: 1,
            seq: 2,
            snapshot: snapshot.clone(),
        });
        settle().await;
    }
    let approvals = fake
        .sent()
        .iter()
        .filter(|m| {
            matches!(m,
        BridgeClientMessage::RunPermission { tool_call_id, .. } if tool_call_id == "a7t0c0")
        })
        .count();
    fake.reply(BridgeServerMessage::Error {
        id: "running".into(),
        message: "no pending permission for a7t0c0".into(),
    });
    settle().await;
    let still_running = mounted.state.chat.active_run.get_untracked().is_some();
    assert!(still_running);
    event(
        &fake,
        "running",
        3,
        RunEvent::Done {
            message: message(8, Role::Assistant, "finished after command error"),
        },
    );
    settle().await;
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("finished after command error")
    );
    assert!(!mounted.state.chat.streaming.get_untracked());
    close(&mounted);
    assert_eq!(
        approvals, 1,
        "same snapshot approved the tool twice; run still tracked: {still_running}"
    );
}

#[wasm_bindgen_test]
async fn adversarial_snapshot_counts_distinct_turns_with_equal_usage() {
    use openwebide_core::TurnTelemetry;
    let (mounted, fake) = fixture();
    ready(&fake).await;
    fake.reply(BridgeServerMessage::Runs {
        session_id: 1,
        runs: vec![RunInfo {
            run_id: "running".into(),
            session_id: 1,
            running: true,
            seq: 5,
            started_at: 0,
        }],
    });
    settle().await;
    let usage = TurnTelemetry {
        context: None,
        prompt_tokens: 10,
        completion_tokens: 3,
        eval_duration_ms: 100,
        estimated: false,
    };
    let mut interim = message(8, Role::Assistant, "interim");
    interim.usage = Some(usage);
    let mut snapshot = RunSnapshot::default();
    snapshot.apply(&RunEvent::Message {
        message: message(7, Role::User, "hello"),
    });
    snapshot.apply(&RunEvent::Telemetry { usage });
    snapshot.apply(&RunEvent::Interim { message: interim });
    snapshot.apply(&RunEvent::Delta {
        content: "second turn".into(),
    });
    snapshot.apply(&RunEvent::Telemetry { usage });
    for _ in 0..2 {
        fake.reply(BridgeServerMessage::RunSnapshot {
            run_id: "running".into(),
            session_id: 1,
            seq: 5,
            snapshot: snapshot.clone(),
        });
        settle().await;
        let telemetry = mounted.state.chat.session_telemetry.get_untracked();
        assert_eq!(telemetry.total_prompt_tokens, 20);
        assert_eq!(telemetry.total_completion_tokens, 6);
    }
    let mut second = message(9, Role::Assistant, "second turn");
    second.usage = Some(usage);
    snapshot.apply(&RunEvent::Interim { message: second });
    fake.reply(BridgeServerMessage::RunSnapshot {
        run_id: "running".into(),
        session_id: 1,
        seq: 6,
        snapshot,
    });
    settle().await;
    let total = mounted
        .state
        .chat
        .session_telemetry
        .get_untracked()
        .total_prompt_tokens;
    close(&mounted);
    assert_eq!(
        total, 20,
        "identical usage values are from two distinct turns"
    );
}

#[wasm_bindgen_test]
async fn adversarial_finished_background_run_discovers_selected_session() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    event(
        &fake,
        &run_id,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "hello"),
        },
    );
    let mut second = mounted.state.chat.sessions.get_untracked()[0].clone();
    second.id = 2;
    mounted
        .state
        .fake
        .sessions
        .borrow_mut()
        .push(second.clone());
    mounted.state.chat.sessions.update(|s| s.push(second));
    mounted.state.chat.active_session.set(Some(2));
    settle().await;
    event(
        &fake,
        &run_id,
        2,
        RunEvent::Done {
            message: message(8, Role::Assistant, "reply"),
        },
    );
    settle().await;
    let listed = fake
        .sent()
        .contains(&BridgeClientMessage::RunList { session_id: 2 });
    close(&mounted);
    assert!(
        listed,
        "selected session never gets its running runs discovered after the tracked background run finishes"
    );
}

#[wasm_bindgen_test]
async fn adversarial_stop_during_disconnect_is_sent_on_reconnect() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    event(
        &fake,
        &run_id,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "hello"),
        },
    );
    fake.disconnect();
    settle().await;
    mounted.click(".btn.stop");
    settle().await;
    sleep_ms(1100).await;
    ready(&fake).await;
    let cancelled = fake
        .sent()
        .contains(&BridgeClientMessage::RunCancel { run_id });
    close(&mounted);
    assert!(cancelled, "stop request vanished across reconnect");
}

#[wasm_bindgen_test]
async fn lost_queued_permission_snapshot_restores_retry() {
    permission_snapshot_restores_retry(false).await;
}

#[wasm_bindgen_test]
async fn lost_queued_autoapproval_snapshot_restores_retry() {
    permission_snapshot_restores_retry(true).await;
}

async fn permission_snapshot_restores_retry(auto_approve: bool) {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    event(
        &fake,
        &run_id,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "hello"),
        },
    );
    event(
        &fake,
        &run_id,
        2,
        RunEvent::PermissionRequest {
            id: "a7t0c0".into(),
            name: "write_file".into(),
            summary: "file".into(),
            diff: None,
            note: None,
        },
    );
    settle().await;
    fake.lose_next_permission();
    mounted.key("y", "KeyY", true);
    settle().await;
    assert!(
        !fake
            .sent()
            .iter()
            .any(|m| matches!(m, BridgeClientMessage::RunPermission { .. }))
    );
    fake.disconnect();
    settle().await;
    sleep_ms(1100).await;
    ready(&fake).await;
    mounted.state.chat.active_session.set(None);
    settle().await;
    mounted.state.chat.active_session.set(Some(1));
    settle().await;
    if auto_approve {
        mounted.state.chat.approval_mode.update(|modes| {
            modes.insert(1, openwebide_agent::policy::ApprovalMode::AlwaysForSession);
        });
    }
    fake.reply(BridgeServerMessage::RunSnapshot {
        run_id: run_id.clone(),
        session_id: 1,
        seq: 2,
        snapshot: RunSnapshot {
            items: vec![
                RunItem::Message(message(7, Role::User, "hello")),
                RunItem::Step(RunStep {
                    timing: None,
                    id: "a7t0c0".into(),
                    name: "write_file".into(),
                    summary: "file".into(),
                    awaiting_permission: true,
                    result: None,
                    diff: None,
                    note: None,
                }),
            ],
            ..Default::default()
        },
    });
    settle().await;
    assert_eq!(
        mounted
            .state
            .chat
            .awaiting_step_id
            .get_untracked()
            .as_deref(),
        Some("a7t0c0")
    );
    assert!(mounted.state.chat.active_run.get_untracked().is_some());
    mounted.key("y", "KeyY", true);
    settle().await;
    assert!(fake.sent().contains(&BridgeClientMessage::RunPermission {
        run_id: run_id.clone(),
        tool_call_id: "a7t0c0".into(),
        approved: true,
    }));
    event(
        &fake,
        &run_id,
        3,
        RunEvent::ToolCall {
            id: "a7t0c0".into(),
            name: "write_file".into(),
            summary: "file".into(),
        },
    );
    settle().await;
    assert!(
        mounted
            .state
            .chat
            .awaiting_step_id
            .get_untracked()
            .is_none()
    );
    close(&mounted);
}

#[wasm_bindgen_test]
async fn stop_running_tool_renders_cancelled_and_one_marker() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    event(
        &fake,
        &run_id,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "hello"),
        },
    );
    event(
        &fake,
        &run_id,
        2,
        RunEvent::ToolCall {
            id: "a7t1c0".into(),
            name: "run_command".into(),
            summary: "sleep 30".into(),
        },
    );
    settle().await;
    mounted.click(".btn.stop");
    settle().await;
    assert!(fake.sent().contains(&BridgeClientMessage::RunCancel {
        run_id: run_id.clone()
    }));
    event(
        &fake,
        &run_id,
        3,
        RunEvent::ToolResult {
            id: "a7t1c0".into(),
            name: "run_command".into(),
            ok: false,
            summary: "cancelled".into(),
            diff: None,
        },
    );
    event(&fake, &run_id, 4, RunEvent::Cancelled);
    event(&fake, &run_id, 4, RunEvent::Cancelled);
    settle().await;
    assert!(mounted.root.text_content().unwrap().contains("cancelled"));
    assert_eq!(
        mounted
            .state
            .chat
            .messages
            .get_untracked()
            .iter()
            .filter(|item| matches!(item, ConversationItem::Stopped { .. }))
            .count(),
        1
    );
    close(&mounted);
}

#[wasm_bindgen_test]
async fn completed_snapshots_refresh_db_state_without_resurrecting_resolved_edits() {
    use openwebide_core::{EditDecision, FileDiff, PersistedEdit};
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    let diff = FileDiff {
        path: "file.rs".into(),
        old: Some("old".into()),
        new: "new".into(),
        old_unavailable: false,
        backup_path: None,
    };
    let result = RunEvent::ToolResult {
        id: "a7t0c0".into(),
        name: "write_file".into(),
        ok: true,
        summary: "written".into(),
        diff: Some(diff.clone()),
    };
    let mut snapshot = RunSnapshot::default();
    snapshot.apply(&RunEvent::Message {
        message: message(7, Role::User, "hello"),
    });
    snapshot.apply(&RunEvent::ToolCall {
        id: "a7t0c0".into(),
        name: "write_file".into(),
        summary: "file".into(),
    });
    snapshot.apply(&result);
    for decision in [
        EditDecision::Accepted,
        EditDecision::Rejected,
        EditDecision::Pending,
    ] {
        mounted.state.fake.persisted_edits.borrow_mut().insert(
            (1, "file.rs".into()),
            PersistedEdit {
                file: None,
                project_id: 1,
                path: "file.rs".into(),
                revision: 2,
                decision,
                diff: diff.clone(),
            },
        );
        for _ in 0..2 {
            fake.reply(BridgeServerMessage::RunSnapshot {
                run_id: run_id.clone(),
                session_id: 1,
                seq: 4,
                snapshot: snapshot.clone(),
            });
            settle().await;
            assert_eq!(
                mounted.state.workspace.pending_edits.get_untracked().len(),
                usize::from(decision == EditDecision::Pending)
            );
        }
    }
    mounted.state.workspace.switch_project(Some(1), 2);
    event(&fake, &run_id, 5, result);
    settle().await;
    assert!(
        mounted
            .state
            .workspace
            .pending_edits
            .get_untracked()
            .is_empty()
    );
    assert_eq!(
        mounted.state.workspace.snapshots.get_untracked()[&1].persisted_edits["file.rs"].revision,
        2
    );
    close(&mounted);
}

async fn replay_during_resolution(
    rejected: bool,
    seen_live: bool,
) -> (
    openwebide_frontend::state::workspace::EditorText,
    bool,
    bool,
) {
    use openwebide_core::{EditDecision, FileDiff, PersistedEdit};
    let fake = Rc::new(FakeTransport::default());
    let transport = fake.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        state.bridge.set(Some(BridgeConn::with_transport(
            BridgeConfig::new("ws://test"),
            transport,
            Rc::new(|| Box::pin(async { Ok("token".into()) })),
        )));
        let pane = chat_view(state.clone());
        let editor = super::support::editor_view(state);
        view! { {pane} {editor} }
    });
    ready(&fake).await;
    no_runs(&fake);
    let run_id = send(&mounted, &fake).await;
    let diff = FileDiff {
        path: "file.rs".into(),
        old: Some("original".into()),
        new: "changed".into(),
        old_unavailable: false,
        backup_path: None,
    };
    let result = RunEvent::ToolResult {
        id: "a7t0c0".into(),
        name: "write_file".into(),
        ok: true,
        summary: "written".into(),
        diff: Some(diff.clone()),
    };
    if seen_live {
        event(
            &fake,
            &run_id,
            1,
            RunEvent::ToolCall {
                id: "a7t0c0".into(),
                name: "write_file".into(),
                summary: "file".into(),
            },
        );
        event(&fake, &run_id, 2, result.clone());
        settle().await;
    }
    assert_eq!(
        mounted
            .state
            .workspace
            .agent_writes
            .get_untracked()
            .get(&(1, "file.rs".into()))
            .copied()
            .unwrap_or_default(),
        u64::from(seen_live)
    );
    let edit = PersistedEdit {
        file: None,
        project_id: 1,
        path: "file.rs".into(),
        revision: 1,
        decision: EditDecision::Pending,
        diff: diff.clone(),
    };
    mounted
        .state
        .fake
        .persisted_edits
        .borrow_mut()
        .insert((1, "file.rs".into()), edit.clone());
    mounted
        .state
        .fake
        .files
        .borrow_mut()
        .insert((1, "file.rs".into()), "changed".into());
    mounted.state.workspace.set_persisted_edits(1, vec![edit]);
    mounted
        .state
        .workspace
        .open_file
        .set(Some("file.rs".into()));
    if rejected {
        mounted.state.workspace.content.set("changed".into());
        mounted.state.workspace.dirty.set(false);
    } else {
        mounted.state.workspace.content.set("dirty draft".into());
        mounted.state.workspace.dirty.set(true);
    }
    let (release, pending) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .resolution_response_results
        .borrow_mut()
        .push_back(pending);
    settle().await;
    if rejected {
        mounted.click_text("Reject");
        settle().await;
        mounted.click(".modal-footer .danger");
    } else {
        mounted.click_text("Accept");
    }
    wait_until("edit decision committed while its response is held", || {
        mounted
            .state
            .fake
            .persisted_edits
            .borrow()
            .get(&(1, "file.rs".into()))
            .is_some_and(|edit| {
                edit.decision
                    == if rejected {
                        EditDecision::Rejected
                    } else {
                        EditDecision::Accepted
                    }
            })
    })
    .await;
    if !seen_live {
        mounted
            .state
            .fake
            .files
            .borrow_mut()
            .insert((1, "file.rs".into()), "changed".into());
    }
    let mut snapshot = RunSnapshot::default();
    snapshot.apply(&RunEvent::Message {
        message: message(7, Role::User, "hello"),
    });
    snapshot.apply(&RunEvent::ToolCall {
        id: "a7t0c0".into(),
        name: "write_file".into(),
        summary: "file".into(),
    });
    snapshot.apply(&result);
    fake.reply(BridgeServerMessage::RunSnapshot {
        run_id: run_id.clone(),
        session_id: 1,
        seq: 2,
        snapshot,
    });
    settle().await;
    if !seen_live {
        assert_eq!(
            mounted.state.workspace.agent_writes.get_untracked()[&(1, "file.rs".into())],
            1
        );
    }
    release.send(Ok(())).unwrap();
    wait_until("snapshot replay resolution finished", || {
        mounted
            .state
            .workspace
            .persisted_edits
            .get_untracked()
            .is_empty()
            && !mounted.state.workspace.is_resolving()
    })
    .await;
    let out = (
        mounted.state.workspace.content.get_untracked(),
        mounted.state.workspace.dirty.get_untracked(),
        mounted
            .state
            .workspace
            .persisted_edits
            .get_untracked()
            .is_empty(),
    );
    close(&mounted);
    out
}

#[wasm_bindgen_test]
async fn snapshot_replay_does_not_block_accept() {
    let (content, dirty, cleared) = replay_during_resolution(false, true).await;
    assert!(cleared);
    assert_eq!((content.as_str(), dirty), ("changed", false));
}

#[wasm_bindgen_test]
async fn snapshot_replay_does_not_block_reject() {
    let (content, dirty, cleared) = replay_during_resolution(true, true).await;
    assert!(cleared);
    assert_eq!((content.as_str(), dirty), ("original", false));
}

#[wasm_bindgen_test]
async fn snapshot_with_unseen_write_blocks_stale_accept_update() {
    let (content, dirty, cleared) = replay_during_resolution(false, false).await;
    assert!(cleared);
    assert_eq!((content.as_str(), dirty), ("dirty draft", true));
}

#[wasm_bindgen_test]
async fn host_goal_discovery_attaches_finished_turns_once_without_browser_run_start() {
    let (mounted, fake) = fixture();
    ready(&fake).await;
    no_runs(&fake);
    let mut goal = openwebide_core::Goal::transition(
        None,
        1,
        openwebide_core::GoalCommand::Start {
            objective: "Verify tests".into(),
        },
        1,
    )
    .unwrap();
    goal.worker = true;
    mounted
        .state
        .fake
        .goals
        .borrow_mut()
        .insert(1, goal.clone());
    mounted.state.chat.goal.set(Some(goal));
    settle().await;
    let info = RunInfo {
        run_id: "scheduled-42".into(),
        session_id: 1,
        running: false,
        seq: 2,
        started_at: 1,
    };
    fake.reply(BridgeServerMessage::Runs {
        session_id: 1,
        runs: vec![info.clone()],
    });
    settle().await;
    assert!(fake.sent().iter().any(|message|matches!(message,BridgeClientMessage::RunAttach{run_id,..} if run_id=="scheduled-42")));
    let final_message = message(8, Role::Assistant, "Goal tests verified");
    fake.reply(BridgeServerMessage::RunSnapshot {
        run_id: "scheduled-42".into(),
        session_id: 1,
        seq: 2,
        snapshot: RunSnapshot {
            items: vec![
                RunItem::Message(message(7, Role::User, "Continue the goal")),
                RunItem::Message(final_message.clone()),
            ],
            finished: Some(RunEvent::Done {
                message: final_message,
            }),
            ..Default::default()
        },
    });
    settle().await;
    assert!(!mounted.state.chat.streaming.get_untracked());
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Goal tests verified")
    );
    let attaches = fake
        .sent()
        .iter()
        .filter(|message| matches!(message, BridgeClientMessage::RunAttach { .. }))
        .count();
    mounted
        .state
        .chat
        .goal
        .update(|goal| goal.as_mut().unwrap().note = Some("Verified".into()));
    settle().await;
    fake.reply(BridgeServerMessage::Runs {
        session_id: 1,
        runs: vec![info],
    });
    settle().await;
    assert_eq!(
        attaches,
        fake.sent()
            .iter()
            .filter(|message| matches!(message, BridgeClientMessage::RunAttach { .. }))
            .count()
    );
    assert!(
        !fake
            .sent()
            .iter()
            .any(|message| matches!(message, BridgeClientMessage::RunStart { .. }))
    );
    close(&mounted);
}
