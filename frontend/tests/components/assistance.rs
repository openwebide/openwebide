use super::support::{mount_test_with_backend, settle, wait_until};
use leptos::prelude::*;
use openwebide_core::{ChatMessage, Role, WorkspaceMode};
use openwebide_frontend::{
    conversation::ConversationItem, state_actions::assistance::ChatAssistance,
    testing::fake_backend::FakeBackend,
};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen_test::*;

#[wasm_bindgen_test]
async fn context_assistance_rejects_invented_ids_and_attaches_only_after_selection() {
    use openwebide_frontend::state_actions::context_assistance::{
        ContextAssistance, ContextCandidate,
    };
    for mode in [
        Some(WorkspaceMode::Local),
        Some(WorkspaceMode::Remote),
        None,
    ] {
        let fake = Rc::new(FakeBackend::default());
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(Some("session:2\nsession:999\nfile:invented.rs".into())))
            .unwrap();
        fake.assistance_results.borrow_mut().push_back(receive);
        let captured = Rc::new(Cell::new(None));
        let slot = captured.clone();
        let mounted = mount_test_with_backend(fake, move |state| {
            if let Some(mode) = mode {
                state.seed_project();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
            }
            state.seed_session();
            let project = state.projects.active_project.get_untracked();
            state.chat.sessions.update(|sessions| {
                sessions[0].project_id = project;
                let mut previous = sessions[0].clone();
                previous.id = 2;
                previous.name = "Menu icons".into();
                sessions.push(previous);
            });
            state.fake.messages.borrow_mut().insert(
                2,
                vec![openwebide_core::ConversationEntry::Message(ChatMessage {
                    id: 2,
                    session_id: 2,
                    role: Role::Assistant,
                    content: "<think>private</think>Fixed menu icons".into(),
                    created_at: 0,
                    tool_calls: None,
                    tool_call_id: None,
                    usage: None,
                })],
            );
            state.chat.draft.set("Explain the menu icon changes".into());
            slot.set(Some(ContextAssistance::from_context()));
            view! { <div/> }
        });
        wait_until("context suggestions", || {
            !captured
                .get()
                .unwrap()
                .suggestions
                .get_untracked()
                .is_empty()
        })
        .await;
        let actions = captured.get().unwrap();
        assert_eq!(
            actions.suggestions.get_untracked(),
            [ContextCandidate::Session(2, "Menu icons".into())]
        );
        assert_eq!(
            mounted.state.chat.draft.get_untracked(),
            "Explain the menu icon changes"
        );
        actions
            .attach
            .run(actions.suggestions.get_untracked()[0].clone());
        wait_until("selected chat attached", || {
            mounted
                .state
                .chat
                .draft
                .get_untracked()
                .contains("Fixed menu icons")
        })
        .await;
        assert!(!mounted.state.chat.draft.get_untracked().contains("private"));
        let old = ContextCandidate::Session(2, "Menu icons".into());
        mounted.state.auth.generation.update(|epoch| *epoch += 1);
        settle().await;
        let original = mounted.state.chat.draft.get_untracked();
        actions.attach.run(old);
        settle().await;
        assert_eq!(mounted.state.chat.draft.get_untracked(), original);
    }
}

#[wasm_bindgen_test]
async fn assistance_shows_grounded_recaps_and_three_distinct_ranked_suggestions() {
    let fake = Rc::new(FakeBackend::default());
    for result in [
        "Fixed menu icons; compilation failed.",
        " Review changes \nDraft a commit\nReview changes\nRun tests\nIgnored fourth suggestion",
        "Fixed menu icons; compilation failed.",
    ] {
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(Some(result.into()))).unwrap();
        fake.assistance_results.borrow_mut().push_back(receive);
    }
    let captured = Rc::new(Cell::new(None));
    let slot = captured.clone();
    let mounted = mount_test_with_backend(fake, move |state| {
        state.seed_project();
        state.seed_session();
        state.chat.messages.set(vec![
            ConversationItem::ToolStep {
                timing: None,
                key: 1,
                id: "call".into(),
                name: "run_command".into(),
                summary: "cargo test".into(),
                result: Some(openwebide_frontend::conversation::ToolStepResult {
                    ok: false,
                    summary: "Compilation failed".into(),
                    diff: None,
                }),
                diff: None,
                note: None,
                awaiting_permission: false,
            },
            ConversationItem::Message(ChatMessage {
                id: 1,
                session_id: 1,
                role: Role::Assistant,
                content: "<think>private reasoning</think>Fixed icons; compilation failed".into(),
                created_at: 0,
                tool_calls: None,
                tool_call_id: None,
                usage: None,
            }),
        ]);
        slot.set(Some(ChatAssistance::new(
            state.api,
            state.auth,
            state.chat,
            state.projects,
        )));
        view! { <div/> }
    });
    wait_until("recap and suggestions", || {
        captured.get().unwrap().next_actions.get_untracked().len() == 3
            && captured.get().unwrap().completion.get_untracked().is_some()
    })
    .await;
    let assistance = captured.get().unwrap();
    assert_eq!(
        assistance.recap.get_untracked().as_deref(),
        Some("Fixed menu icons; compilation failed.")
    );
    assert_eq!(
        assistance.next_actions.get_untracked(),
        ["Review changes", "Draft a commit", "Run tests"]
    );
    assert!(
        mounted
            .state
            .fake
            .assistance_requests
            .borrow()
            .iter()
            .all(|request| !request.input.contains("private reasoning"))
    );
    assert!(
        mounted
            .state
            .fake
            .assistance_requests
            .borrow()
            .iter()
            .all(|request| request.input.contains("success=false: Compilation failed"))
    );
    mounted.state.chat.streaming.set(true);
    settle().await;
    assert!(assistance.recap.get_untracked().is_none());
    assert!(assistance.completion.get_untracked().is_none());
    assert!(assistance.next_actions.get_untracked().is_empty());
}

#[wasm_bindgen_test]
async fn assistance_discards_stale_account_project_and_session_results_in_every_mode() {
    for mode in [
        Some(WorkspaceMode::Local),
        Some(WorkspaceMode::Remote),
        None,
    ] {
        for change in ["account", "project", "session", "streaming"] {
            let fake = Rc::new(FakeBackend::default());
            let (send, receive) = futures::channel::oneshot::channel();
            fake.assistance_results.borrow_mut().push_back(receive);
            let captured = Rc::new(Cell::new(None));
            let slot = captured.clone();
            let mounted = mount_test_with_backend(fake, move |state| {
                if let Some(mode) = mode {
                    state.seed_project();
                    state
                        .projects
                        .projects
                        .update(|projects| projects[0].mode = mode);
                }
                state.seed_session();
                let project = state.projects.active_project.get_untracked();
                state
                    .chat
                    .sessions
                    .update(|sessions| sessions[0].project_id = project);
                state
                    .chat
                    .messages
                    .set(vec![ConversationItem::Message(ChatMessage {
                        id: 1,
                        session_id: 1,
                        role: Role::Assistant,
                        content: "Fixed icons; tests passed".into(),
                        created_at: 0,
                        tool_calls: None,
                        tool_call_id: None,
                        usage: None,
                    })]);
                slot.set(Some(ChatAssistance::new(
                    state.api,
                    state.auth,
                    state.chat,
                    state.projects,
                )));
                view! { <div/> }
            });
            wait_until("idle assistance request", || {
                !mounted.state.fake.assistance_requests.borrow().is_empty()
            })
            .await;
            let request = mounted.state.fake.assistance_requests.borrow()[0].clone();
            assert_eq!(
                request.project_id,
                mounted.state.projects.active_project.get_untracked()
            );
            match change {
                "account" => mounted
                    .state
                    .auth
                    .generation
                    .update(|generation| *generation += 1),
                "project" => mounted.state.projects.active_project.set(Some(99)),
                "session" => mounted.state.chat.active_session.set(Some(99)),
                _ => mounted.state.chat.streaming.set(true),
            }
            settle().await;
            send.send(Ok(Some("Old account result".into()))).unwrap();
            settle().await;
            let assistance = captured.get().unwrap();
            assert!(assistance.recap.get_untracked().is_none());
            assert!(assistance.next_actions.get_untracked().is_empty());
        }
    }
}

#[wasm_bindgen_test]
async fn assistance_git_drafts_keep_edits_and_reject_changed_sources_in_both_modes() {
    use super::project_git::{Http, gitDiff, gitHttp, restore_token, token};
    use openwebide_core::{AssistanceKind, GitRepoStatus};
    use openwebide_frontend::{
        project_git::ProjectGit, state_actions::git_assistance::GitAssistance,
    };
    use wasm_bindgen::JsCast;
    let old = token().await;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for outcome in ["stable", "edit", "diff", "account", "foreground"] {
            let http = Http(gitHttp());
            let folder = super::local_bridge::probe_folder();
            let captured = Rc::new(Cell::new(None));
            let slot = captured.clone();
            let fake = Rc::new(FakeBackend::default());
            let (send, receive) = futures::channel::oneshot::channel();
            fake.assistance_results.borrow_mut().push_back(receive);
            if mode == WorkspaceMode::Remote {
                for _ in 0..2 {
                    let (send, receive) = futures::channel::oneshot::channel();
                    send.send(Ok(GitRepoStatus {
                        branch: "main".into(),
                        ..Default::default()
                    }))
                    .unwrap();
                    fake.git_statuses.borrow_mut().push_back(receive);
                }
                fake.git_diffs.borrow_mut().extend([
                    Ok("actual remote diff".into()),
                    Ok(if outcome == "diff" {
                        "changed diff"
                    } else {
                        "actual remote diff"
                    }
                    .into()),
                ]);
            }
            let mounted = mount_test_with_backend(fake, move |state| {
                state.seed_project();
                state.seed_connection();
                state.seed_session();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
                if mode == WorkspaceMode::Local {
                    state.projects.local_handles.update(|handles| {
                        handles.insert(1, folder.unchecked_into());
                    });
                    state.settings.bridge_url.set("ws://git.test:3001".into());
                }
                slot.set(Some(GitAssistance::new(
                    state.api,
                    expect_context::<ProjectGit>(),
                    state.projects,
                    state.auth,
                    state.chat,
                    state.settings,
                )));
                view! { <div/> }
            });
            settle().await;
            let actions = captured.get().unwrap();
            actions.generate.run(AssistanceKind::Commit);
            wait_until("diff grounded draft request", || {
                !mounted.state.fake.assistance_requests.borrow().is_empty()
            })
            .await;
            assert!(
                mounted.state.fake.assistance_requests.borrow()[0]
                    .input
                    .contains(if mode == WorkspaceMode::Local {
                        "local diff"
                    } else {
                        "actual remote diff"
                    })
            );
            match outcome {
                "edit" => actions.draft.set("My edited message".into()),
                "diff" if mode == WorkspaceMode::Local => gitDiff(&http.0, "changed diff"),
                "account" => mounted.state.auth.generation.update(|epoch| *epoch += 1),
                "foreground" => mounted.state.chat.streaming.set(true),
                _ => (),
            }
            settle().await;
            send.send(Ok(Some("Fix menu icons".into()))).unwrap();
            wait_until("draft resolved", || {
                !actions.busy.get_untracked() || outcome == "foreground"
            })
            .await;
            settle().await;
            assert_eq!(
                actions.draft.get_untracked(),
                match outcome {
                    "stable" => "Fix menu icons",
                    "edit" => "My edited message",
                    _ => "",
                }
            );
            if outcome == "diff" {
                assert!(actions.error.get_untracked().unwrap().contains("Try again"));
            }
            assert!(
                !mounted
                    .state
                    .fake
                    .calls
                    .borrow()
                    .iter()
                    .any(|call| matches!(
                        call,
                        openwebide_frontend::testing::fake_backend::Call::Request {
                            method: "git_commit"
                        }
                    ))
            );
            drop(mounted);
            drop(http);
        }
    }
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn assistance_context_files_and_memories_require_selection_and_reject_changed_memories() {
    use openwebide_core::{FileEntry, ProjectMemories, ProjectMemory};
    use openwebide_frontend::state_actions::context_assistance::{
        ContextAssistance, ContextCandidate,
    };
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fake = Rc::new(FakeBackend::default());
        for _ in 0..2 {
            let (send, receive) = futures::channel::oneshot::channel();
            send.send(Ok(Some("file:main.rs\nmemory:1\nfile:main.rs".into())))
                .unwrap();
            fake.assistance_results.borrow_mut().push_back(receive);
        }
        let captured = Rc::new(Cell::new(None));
        let slot = captured.clone();
        let entry = ProjectMemory {
            id: 1,
            title: "Build instructions".into(),
            content: "cargo test".into(),
            revision: 1,
            updated_at: 0,
            auto_title: false,
        };
        let setup_entry = entry.clone();
        let mounted = mount_test_with_backend(fake, move |state| {
            state.seed_project();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.workspace.entries.update(|entries| {
                entries.insert(
                    String::new(),
                    vec![FileEntry {
                        name: "main.rs".into(),
                        path: "main.rs".into(),
                        is_dir: false,
                        size: 5,
                    }],
                );
            });
            state.fake.memories.borrow_mut().insert(
                1,
                ProjectMemories {
                    enabled: true,
                    entries: vec![setup_entry.clone()],
                },
            );
            state.memories.data.set(Some(ProjectMemories {
                enabled: true,
                entries: vec![setup_entry],
            }));
            state.chat.draft.set("Explain the build for main.rs".into());
            slot.set(Some(ContextAssistance::from_context()));
            view! { <div/> }
        });
        let actions = captured.get().unwrap();
        wait_until("file and memory candidates", || {
            actions.suggestions.get_untracked().len() == 2
        })
        .await;
        assert_eq!(
            mounted.state.chat.draft.get_untracked(),
            "Explain the build for main.rs"
        );
        let request = mounted
            .state
            .fake
            .assistance_requests
            .borrow()
            .last()
            .unwrap()
            .clone();
        let input: serde_json::Value = serde_json::from_str(&request.input).unwrap();
        assert_eq!(input["draft"], "Explain the build for main.rs");
        let candidates = input["candidates"].as_array().unwrap();
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate["id"] == "file:main.rs")
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate["id"] == "memory:1")
        );

        actions.attach.run(ContextCandidate::File("main.rs".into()));
        wait_until("selected file mention", || {
            mounted.state.chat.draft.get_untracked().contains("@file:")
        })
        .await;
        settle().await;
        wait_until("refreshed memory candidate", || {
            actions
                .suggestions
                .get_untracked()
                .iter()
                .any(|candidate| matches!(candidate, ContextCandidate::Memory(_)))
        })
        .await;
        actions.attach.run(ContextCandidate::Memory(entry.clone()));
        wait_until("selected memory contents", || {
            mounted
                .state
                .chat
                .draft
                .get_untracked()
                .contains("cargo test")
        })
        .await;
        let original = mounted.state.chat.draft.get_untracked();
        mounted.state.memories.data.update(|data| {
            data.as_mut().unwrap().entries[0].revision += 1;
        });
        actions.attach.run(ContextCandidate::Memory(entry));
        settle().await;
        assert_eq!(mounted.state.chat.draft.get_untracked(), original);
    }
}

#[wasm_bindgen_test]
async fn inline_chat_hints_accept_only_at_the_caret_end_in_every_scope() {
    use super::support::chat_view;
    use wasm_bindgen::JsCast;
    for mode in [
        None,
        Some(WorkspaceMode::Local),
        Some(WorkspaceMode::Remote),
    ] {
        let fake = Rc::new(FakeBackend::default());
        for result in [
            "Recent work",
            "😀 Review changes\nDraft a commit\nRun tests",
            "Finished",
        ] {
            let (send, receive) = futures::channel::oneshot::channel();
            send.send(Ok(Some(result.into()))).unwrap();
            fake.assistance_results.borrow_mut().push_back(receive);
        }
        let mounted = mount_test_with_backend(fake, move |state| {
            state.seed_connection();
            state.seed_project();
            state.seed_session();
            if let Some(mode) = mode {
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
            } else {
                state.projects.active_project.set(None);
                state
                    .chat
                    .sessions
                    .update(|sessions| sessions[0].project_id = None);
            }
            state.fake.messages.borrow_mut().insert(
                1,
                vec![openwebide_core::ConversationEntry::Message(ChatMessage {
                    id: 1,
                    session_id: 1,
                    role: Role::Assistant,
                    content: "Finished the work".into(),
                    created_at: 0,
                    tool_calls: None,
                    tool_call_id: None,
                    usage: None,
                })],
            );
            view! { <style>{include_str!("../../styles.css")}".chat-pane .messages { flex: none; height: 40px; }"</style>{chat_view(state)} }
        });
        wait_until("inline prompt hint", || {
            mounted
                .root
                .query_selector(".tui-inline-hint")
                .unwrap()
                .is_some()
        })
        .await;
        assert!(
            mounted
                .root
                .query_selector(".messages [aria-label='Suggested next actions']")
                .unwrap()
                .is_some()
        );
        let history = mounted
            .element(".messages")
            .unchecked_into::<web_sys::HtmlElement>();
        wait_until("history suggestions visible after arrival", || {
            f64::from(history.scroll_height() - history.client_height()) - history.scroll_top()
                <= 1.0
        })
        .await;
        let buttons = mounted
            .root
            .query_selector_all(".chat-followups button")
            .unwrap();
        assert_eq!(buttons.length(), 2);
        assert_eq!(
            buttons.item(0).unwrap().text_content().as_deref(),
            Some("❯Draft a commit")
        );
        assert_eq!(
            buttons.item(1).unwrap().text_content().as_deref(),
            Some("❯Run tests")
        );
        assert!(
            mounted
                .root
                .query_selector(".chat-followups button[aria-label='😀 Review changes']")
                .unwrap()
                .is_none()
        );
        let input = mounted
            .element(".composer-input")
            .unchecked_into::<web_sys::HtmlTextAreaElement>();
        assert!(
            input.value().is_empty(),
            "Hints must not become input until accepted"
        );
        assert!(
            mounted
                .element(".tui-inline-hint")
                .text_content()
                .unwrap()
                .contains("😀 Review changes")
        );
        let key = |shift, ctrl, alt, meta, composing| {
            let init = web_sys::KeyboardEventInit::new();
            init.set_key("ArrowRight");
            init.set_code("ArrowRight");
            init.set_shift_key(shift);
            init.set_ctrl_key(ctrl);
            init.set_alt_key(alt);
            init.set_meta_key(meta);
            init.set_is_composing(composing);
            init.set_bubbles(true);
            init.set_cancelable(true);
            input
                .dispatch_event(
                    &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
                        .unwrap(),
                )
                .unwrap()
        };
        for (shift, ctrl, alt, meta, composing) in [
            (true, false, false, false, false),
            (false, true, false, false, false),
            (false, false, true, false, false),
            (false, false, false, true, false),
            (false, false, false, false, true),
        ] {
            assert!(key(shift, ctrl, alt, meta, composing));
            assert!(mounted.state.chat.draft.get_untracked().is_empty());
        }
        assert!(!key(false, false, false, false, false));
        settle().await;
        assert_eq!(
            mounted.state.chat.draft.get_untracked(),
            "😀 Review changes"
        );
        assert_eq!(input.selection_start().unwrap(), Some(17));
        assert_eq!(input.selection_end().unwrap(), Some(17));
        assert!(
            mounted
                .root
                .query_selector(".tui-inline-hint")
                .unwrap()
                .is_none()
        );
        mounted.input("😀 Rev");
        settle().await;
        input.set_selection_range(2, 2).unwrap();
        assert!(
            key(false, false, false, false, false),
            "Right moves normally inside the input"
        );
        input.set_selection_range(2, 6).unwrap();
        assert!(
            key(false, false, false, false, false),
            "Right must not overwrite selected text"
        );
        input.set_selection_range(6, 6).unwrap();
        assert!(!key(false, false, false, false, false));
        settle().await;
        assert_eq!(
            mounted.state.chat.draft.get_untracked(),
            "😀 Review changes"
        );
        mounted.input("Draft");
        settle().await;
        input.set_selection_range(5, 5).unwrap();
        assert!(key(false, false, false, false, false));
        settle().await;
        assert_eq!(mounted.state.chat.draft.get_untracked(), "Draft");
        assert!(
            mounted
                .root
                .query_selector(".tui-inline-hint")
                .unwrap()
                .is_none()
        );
        mounted.input("Different question");
        settle().await;
        assert!(key(false, false, false, false, false));
        assert_eq!(
            mounted.state.chat.draft.get_untracked(),
            "Different question"
        );
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(Some("session:2".into()))).unwrap();
        mounted
            .state
            .fake
            .assistance_results
            .borrow_mut()
            .push_back(receive);
        mounted.state.chat.sessions.update(|sessions| {
            let mut other = sessions[0].clone();
            other.id = 2;
            other.name = "Related chat".into();
            sessions.push(other);
        });
        wait_until("context hints in history", || {
            mounted
                .root
                .query_selector(".messages [aria-label='Suggested context']")
                .unwrap()
                .is_some()
        })
        .await;
        assert!(
            mounted
                .root
                .query_selector(".chat-pane > .chat-context-suggestions")
                .unwrap()
                .is_none()
        );
        mounted.input("");
        settle().await;
        mounted.state.chat.streaming.set(true);
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".tui-inline-hint")
                .unwrap()
                .is_none()
        );
    }
}

#[wasm_bindgen_test]
async fn chat_actions_send_followups_immediately_and_restore_composer_focus() {
    use openwebide_frontend::components::ChatPane;
    use openwebide_frontend::state_actions::prompt_queue::PromptQueueActions;
    use wasm_bindgen::JsCast;
    for mode in [
        None,
        Some(WorkspaceMode::Local),
        Some(WorkspaceMode::Remote),
    ] {
        let fake = Rc::new(FakeBackend::default());
        for result in [
            "Recent work",
            "Review changes\nDraft a commit\nRun tests",
            "Finished",
        ] {
            let (send, receive) = futures::channel::oneshot::channel();
            send.send(Ok(Some(result.into()))).unwrap();
            fake.assistance_results.borrow_mut().push_back(receive);
        }
        let sent = RwSignal::new(Vec::<String>::new());
        let queued = RwSignal::new(Vec::<String>::new());
        let steered = RwSignal::new(Vec::<String>::new());
        let stopped = RwSignal::new(0);
        let mounted = mount_test_with_backend(fake, move |state| {
            state.seed_connection();
            state.seed_project();
            state.seed_session();
            if let Some(mode) = mode {
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
            } else {
                state.projects.active_project.set(None);
                state
                    .chat
                    .sessions
                    .update(|sessions| sessions[0].project_id = None);
            }
            let chat = state.chat;
            chat.messages.update(|messages| {
                messages.push(ConversationItem::Message(ChatMessage {
                    id: 1,
                    session_id: 1,
                    role: Role::Assistant,
                    content: "Finished".into(),
                    created_at: 0,
                    tool_calls: None,
                    tool_call_id: None,
                    usage: None,
                }));
            });
            let queue = PromptQueueActions {
                enqueue: Callback::new(move |()| {
                    queued.update(|values| values.push(chat.draft.get_untracked()));
                    chat.draft.set(String::new());
                }),
                steer: Callback::new(move |()| {
                    steered.update(|values| values.push(chat.draft.get_untracked()));
                    chat.draft.set(String::new());
                    chat.streaming.set(false);
                }),
                edit: Callback::new(|_| ()),
                remove: Callback::new(|_| ()),
                toggle: Callback::new(|()| ()),
                cancel_edit: Callback::new(|()| ()),
            };
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <ChatPane on_select_connection_model=Callback::new(|_|()) on_send=Callback::new(move |()| {
                    sent.update(|values| values.push(chat.draft.get_untracked())); chat.draft.set(String::new()); chat.streaming.set(true);
                }) on_resume_run=Callback::new(|()|()) on_stop=Callback::new(move |()| { stopped.update(|count| *count += 1); chat.streaming.set(false); })
                    on_permission=Callback::new(|_|()) on_permission_always=Callback::new(|_|()) on_slash_command=Callback::new(|_|()) queue_actions=queue/>
            }
        });
        wait_until("clickable history followups", || {
            mounted
                .root
                .query_selector(".messages .chat-followups button")
                .unwrap()
                .is_some()
        })
        .await;
        let input = mounted.element(".composer-input");
        let focused = || {
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .active_element()
                .is_some_and(|active| active.is_same_node(Some(input.as_ref())))
        };
        let click = |selector: &str| {
            mounted
                .element(selector)
                .unchecked_into::<web_sys::HtmlElement>()
                .focus()
                .unwrap();
            mounted.click(selector);
        };
        input
            .clone()
            .unchecked_into::<web_sys::HtmlTextAreaElement>()
            .blur()
            .unwrap();
        assert!(!focused());
        click(".chat-followups button[aria-label='Draft a commit']");
        settle().await;
        assert_eq!(sent.get_untracked(), ["Draft a commit"]);
        wait_until("focus after suggested prompt", focused).await;
        assert!(mounted.state.chat.draft.get_untracked().is_empty());
        mounted.input("Change direction");
        settle().await;
        click("button[aria-label='Steer']");
        settle().await;
        assert_eq!(steered.get_untracked(), ["Change direction"]);
        wait_until("focus after steer", focused).await;
        mounted.state.chat.streaming.set(true);
        mounted.input("Follow up");
        settle().await;
        click("button[aria-label='Queue follow-up']");
        settle().await;
        assert_eq!(queued.get_untracked(), ["Follow up"]);
        wait_until("focus after queue", focused).await;
        click("button[aria-label='Stop']");
        settle().await;
        assert_eq!(stopped.get_untracked(), 1);
        wait_until("focus after stop", focused).await;
        mounted
            .state
            .chat
            .active_editor_context
            .set(Some(openwebide_core::EditorContext {
                file_path: "src/main.rs".into(),
                cursor_line: 1,
                cursor_col: 1,
                selection: None,
            }));
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".tui-active-context-pill")
                .unwrap()
                .is_none()
        );
        click(".composer button[aria-label='Prompt attachments']");
        settle().await;
        click("button[aria-label='Detach editor context']");
        settle().await;
        assert!(
            mounted
                .state
                .chat
                .active_editor_context
                .get_untracked()
                .is_none()
        );
        wait_until("focus after detaching context", focused).await;
        mounted.input("Another prompt");
        settle().await;
        click("button[aria-label='Send']");
        settle().await;
        assert_eq!(sent.get_untracked(), ["Draft a commit", "Another prompt"]);
        wait_until("focus after send", focused).await;
        // IME and keyboard tests elsewhere retain the native textarea as the typing target.
        let textarea = input.unchecked_into::<web_sys::HtmlTextAreaElement>();
        assert_eq!(textarea.value(), "");
    }
}
