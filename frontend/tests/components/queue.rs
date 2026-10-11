use super::support::{Mounted, chat_view, mount_test, settle};
use leptos::prelude::*;
use openwebide_core::{BridgeClientMessage, BridgeServerMessage, ChatMessage, Role, RunEvent};
use openwebide_frontend::{
    bridge::{BridgeConfig, BridgeConn},
    testing::fake_transport::FakeTransport,
    util::sleep_ms,
};
use std::rc::Rc;
use wasm_bindgen_test::*;

fn fixture(projectless: bool) -> (Mounted, Rc<FakeTransport>) {
    let fake = Rc::new(FakeTransport::default());
    let transport = fake.clone();
    let mounted = mount_test(move |state| {
        if !projectless {
            state.seed_project();
        }
        state.seed_connection();
        state.seed_session();
        if projectless {
            state
                .chat
                .sessions
                .update(|sessions| sessions[0].project_id = None);
            state.fake.sessions.borrow_mut()[0].project_id = None;
        }
        state.bridge.set(Some(BridgeConn::with_transport(
            BridgeConfig::new("ws://test"),
            transport,
            Rc::new(|| Box::pin(async { Ok("token".into()) })),
        )));
        chat_view(state)
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
    fake.reply(BridgeServerMessage::Runs {
        session_id: 1,
        runs: vec![],
    });
}
fn starts(fake: &FakeTransport) -> Vec<(String, String, Option<openwebide_core::QueuedPromptKey>)> {
    fake.sent()
        .into_iter()
        .filter_map(|message| match message {
            BridgeClientMessage::RunStart {
                run_id,
                content,
                queued_prompt,
                ..
            } => Some((run_id, content, queued_prompt)),
            _ => None,
        })
        .collect()
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
fn event(fake: &FakeTransport, run: &str, seq: u64, event: RunEvent) {
    fake.reply(BridgeServerMessage::RunEvent {
        run_id: run.into(),
        seq,
        event,
    });
}

#[wasm_bindgen_test]
async fn queue_keeps_the_live_draft_and_delivers_fifo_after_the_run_in_project_and_projectless_chat()
 {
    for projectless in [false, true] {
        let (mounted, fake) = fixture(projectless);
        ready(&fake).await;
        mounted.input("first");
        mounted.key("Enter", "Enter", false);
        settle().await;
        let first = starts(&fake)[0].0.clone();
        event(
            &fake,
            &first,
            1,
            RunEvent::Message {
                message: message(7, Role::User, "first"),
            },
        );
        settle().await;
        mounted.input("second");
        mounted.key("Enter", "Enter", false);
        settle().await;
        assert_eq!(starts(&fake).len(), 1);
        assert_eq!(
            mounted.state.fake.queued_prompts.borrow()[&1][0].content,
            "second"
        );
        assert_eq!(mounted.state.chat.draft.get_untracked(), "");
        mounted.input("keep typing");
        event(
            &fake,
            &first,
            2,
            RunEvent::Done {
                message: message(8, Role::Assistant, "done"),
            },
        );
        settle().await;
        let all = starts(&fake);
        assert_eq!(all.len(), 2);
        let (second, content, key) = &all[1];
        assert_eq!(content, "second");
        assert!(key.is_some());
        assert_eq!(mounted.state.chat.draft.get_untracked(), "keep typing");
        // The bridge/backend commits and removes the item before emitting Message.
        mounted
            .state
            .fake
            .queued_prompts
            .borrow_mut()
            .get_mut(&1)
            .unwrap()
            .clear();
        event(
            &fake,
            second,
            1,
            RunEvent::Message {
                message: message(9, Role::User, "second"),
            },
        );
        settle().await;
        assert!(mounted.state.chat.queued_prompts.get_untracked().is_empty());
        event(
            &fake,
            second,
            2,
            RunEvent::Done {
                message: message(10, Role::Assistant, "done"),
            },
        );
        settle().await;
        assert!(!mounted.state.chat.streaming.get_untracked());
        assert_eq!(starts(&fake).len(), 2);
        assert_eq!(mounted.state.chat.draft.get_untracked(), "keep typing");
        mounted.state.bridge.get_untracked().unwrap().close();
    }
}

#[wasm_bindgen_test]
async fn steering_saves_guidance_before_cancelling_and_delivers_it_before_followups() {
    let (mounted, fake) = fixture(true);
    ready(&fake).await;
    mounted.input("first");
    mounted.key("Enter", "Enter", false);
    settle().await;
    let first = starts(&fake)[0].0.clone();
    event(
        &fake,
        &first,
        1,
        RunEvent::Message {
            message: message(7, Role::User, "first"),
        },
    );
    settle().await;
    mounted.input("later");
    mounted.key("Enter", "Enter", false);
    settle().await;
    mounted.input("use the other approach");
    settle().await;
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Enter");
    init.set_ctrl_key(true);
    init.set_bubbles(true);
    init.set_cancelable(true);
    mounted
        .element(".composer-input")
        .dispatch_event(
            &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap(),
        )
        .unwrap();
    settle().await;
    let pending = mounted.state.fake.queued_prompts.borrow()[&1].clone();
    assert_eq!(pending[0].content, "use the other approach");
    assert!(pending[0].guidance);
    assert_eq!(pending[1].content, "later");
    assert!(fake.sent().iter().any(
        |message| matches!(message, BridgeClientMessage::RunCancel { run_id } if *run_id == first)
    ));
    assert_eq!(starts(&fake).len(), 1);
    event(&fake, &first, 2, RunEvent::Cancelled);
    settle().await;
    assert_eq!(starts(&fake).len(), 2);
    assert_eq!(starts(&fake)[1].1, "use the other approach");
    mounted.state.bridge.get_untracked().unwrap().close();
}

#[wasm_bindgen_test]
async fn restored_queue_waits_for_run_queue_and_can_be_edited_or_removed() {
    let mounted = mount_test(|state| {
        state.seed_connection();
        state.seed_session();
        state.fake.queued_prompts.borrow_mut().insert(
            1,
            vec![openwebide_core::QueuedPrompt {
                scheduled_task: None,
                plugin_run: None,
                id: 1,
                session_id: 1,
                revision: 1,
                content: "saved followup".into(),
                created_at: 0,
                guidance: false,
            }],
        );
        chat_view(state)
    });
    settle().await;
    assert!(
        mounted
            .state
            .fake
            .calls
            .borrow()
            .iter()
            .all(|call| !matches!(
                call,
                openwebide_frontend::testing::fake_backend::Call::SendMessage { .. }
            ))
    );
    mounted.click(".tui-queued-prompt .btn");
    settle().await;
    assert_eq!(mounted.state.chat.draft.get_untracked(), "saved followup");
    mounted.input("edited followup");
    mounted.key("Enter", "Enter", false);
    settle().await;
    assert_eq!(
        mounted.state.fake.queued_prompts.borrow()[&1][0].content,
        "edited followup"
    );
    assert_eq!(
        mounted.state.fake.queued_prompts.borrow()[&1][0].revision,
        2
    );
    assert!(mounted.state.chat.queue_edit.get_untracked().is_none());
    mounted.click(".tui-queued-prompt .btn:last-child");
    settle().await;
    assert!(mounted.state.fake.queued_prompts.borrow()[&1].is_empty());
    assert!(mounted.state.chat.queued_prompts.get_untracked().is_empty());
}

#[wasm_bindgen_test]
async fn failed_delivery_keeps_the_queued_prompt_pauses_the_queue_and_preserves_new_input() {
    let mounted = mount_test(|state| {
        state.seed_connection();
        state.seed_session();
        state.fake.queued_prompts.borrow_mut().insert(
            1,
            vec![openwebide_core::QueuedPrompt {
                scheduled_task: None,
                plugin_run: None,
                id: 1,
                session_id: 1,
                revision: 1,
                content: "pending task".into(),
                created_at: 0,
                guidance: false,
            }],
        );
        state
            .fake
            .message_save_results
            .borrow_mut()
            .push_back(Err("offline".into()));
        chat_view(state)
    });
    settle().await;
    mounted.input("unfinished draft");
    settle().await;
    mounted.click(".tui-queue-toggle");
    settle().await;
    assert_eq!(
        mounted.state.fake.queued_prompts.borrow()[&1][0].content,
        "pending task"
    );
    assert_eq!(mounted.state.chat.draft.get_untracked(), "unfinished draft");
    assert!(
        !mounted
            .state
            .chat
            .queue_running
            .get_untracked()
            .contains(&1)
    );
    assert!(!mounted.state.chat.streaming.get_untracked());
    assert!(mounted.state.chat.error.get_untracked().is_some());
    assert!(
        mounted
            .state
            .fake
            .messages
            .borrow()
            .get(&1)
            .is_none_or(Vec::is_empty)
    );
}

#[wasm_bindgen_test]
async fn failed_steering_save_does_not_cancel_the_current_run_or_discard_guidance() {
    let (mounted, fake) = fixture(true);
    ready(&fake).await;
    mounted.input("first");
    mounted.key("Enter", "Enter", false);
    settle().await;
    mounted
        .state
        .fake
        .queue_errors
        .borrow_mut()
        .push_back("offline".into());
    mounted.input("new guidance");
    settle().await;
    mounted.click(".tui-btn-steer");
    settle().await;
    assert!(
        fake.sent()
            .iter()
            .all(|message| !matches!(message, BridgeClientMessage::RunCancel { .. }))
    );
    assert_eq!(mounted.state.chat.draft.get_untracked(), "new guidance");
    assert!(mounted.state.chat.streaming.get_untracked());
    assert_eq!(starts(&fake).len(), 1);
    mounted.state.bridge.get_untracked().unwrap().close();
}

#[wasm_bindgen_test]
async fn stale_queue_load_cannot_update_a_new_session_or_account() {
    for account_change in [false, true] {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let mounted = mount_test(move |state| {
            state.seed_connection();
            state.seed_session();
            state
                .fake
                .queue_load_results
                .borrow_mut()
                .push_back(receiver);
            chat_view(state)
        });
        settle().await;
        if account_change {
            mounted
                .state
                .auth
                .generation
                .update(|generation| *generation += 1);
        } else {
            mounted.state.chat.active_session.set(Some(2));
        }
        settle().await;
        mounted.state.chat.draft.set("new draft".into());
        sender
            .send(Ok(vec![openwebide_core::QueuedPrompt {
                scheduled_task: None,
                plugin_run: None,
                id: 1,
                session_id: 1,
                revision: 1,
                content: "old account/session".into(),
                created_at: 0,
                guidance: false,
            }]))
            .unwrap();
        settle().await;
        assert!(mounted.state.chat.queued_prompts.get_untracked().is_empty());
        assert_eq!(mounted.state.chat.draft.get_untracked(), "new draft");
    }
}

#[wasm_bindgen_test]
async fn plugin_prompt_queue_waits_for_its_host_in_both_project_modes() {
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_connection();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.fake.projects.borrow_mut()[0].mode = mode;
            state.fake.queued_prompts.borrow_mut().insert(
                1,
                vec![openwebide_core::QueuedPrompt {
                    scheduled_task: None,
                    plugin_run: Some(7),
                    id: 1,
                    session_id: 1,
                    revision: 1,
                    content: "Plugin-owned prompt".into(),
                    created_at: 0,
                    guidance: false,
                }],
            );
            chat_view(state)
        });
        settle().await;
        mounted.click(".tui-queue-toggle");
        settle().await;
        assert!(
            mounted
                .state
                .fake
                .calls
                .borrow()
                .iter()
                .all(|call| !matches!(
                    call,
                    openwebide_frontend::testing::fake_backend::Call::SendMessage { .. }
                ))
        );
        assert_eq!(mounted.state.fake.queued_prompts.borrow()[&1].len(), 1);
        let edit = mounted
            .root
            .query_selector(".tui-queued-prompt .btn")
            .unwrap()
            .unwrap();
        assert!(edit.has_attribute("disabled"));
        assert_eq!(
            mounted
                .root
                .query_selector(".tui-queue-kind")
                .unwrap()
                .unwrap()
                .text_content()
                .unwrap(),
            "Plugin"
        );
        mounted.click(".tui-queued-prompt .btn:last-child");
        settle().await;
        assert!(mounted.state.fake.queued_prompts.borrow()[&1].is_empty());
    }
}
