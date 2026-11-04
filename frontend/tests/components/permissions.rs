use leptos::prelude::*;
use openwebide_frontend::{
    conversation::{ConversationItem, next_item_nonce},
    testing::fake_backend::Call,
};
use wasm_bindgen_test::*;

use super::support::{chat_view, mount_test, settle};

fn awaiting(id: &str) -> ConversationItem {
    ConversationItem::ToolStep {
        timing: None,
        key: next_item_nonce(),
        id: id.into(),
        name: "write_file".into(),
        summary: "write file".into(),
        result: None,
        awaiting_permission: true,
        diff: None,
        note: None,
    }
}

#[wasm_bindgen_test]
async fn permission_shortcuts_target_current_run_and_stop_cancels_prompts() {
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_session();
        chat_view(state)
    });
    settle().await;
    let chat = mounted.state.chat;
    chat.streaming.set(true);
    chat.streaming_session.set(Some(1));
    chat.current_run_anchor.set(Some(7));
    chat.messages
        .set(vec![awaiting("a3t1c0"), awaiting("a7t1c0")]);
    settle().await;
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("no longer pending")
    );
    mounted.input("y");
    mounted.key("Enter", "Enter", false);
    settle().await;
    assert!(
        !mounted
            .state
            .fake
            .calls
            .borrow()
            .iter()
            .any(|call| matches!(call, Call::SetPermission { .. }))
    );
    mounted.key("y", "KeyY", true);
    settle().await;
    assert!(
        mounted
            .state
            .fake
            .calls
            .borrow()
            .contains(&Call::SetPermission {
                session: 1,
                id: "a7t1c0".into(),
                approved: true
            })
    );
    chat.messages.update(|items| items.push(awaiting("a7t1c1")));
    settle().await;
    mounted.key("n", "KeyN", true);
    settle().await;
    assert!(
        mounted
            .state
            .fake
            .calls
            .borrow()
            .contains(&Call::SetPermission {
                session: 1,
                id: "a7t1c1".into(),
                approved: false
            })
    );
    mounted.key("y", "KeyY", true);
    settle().await;
    let permissions: Vec<_> = mounted
        .state
        .fake
        .calls
        .borrow()
        .iter()
        .filter(|call| matches!(call, Call::SetPermission { .. }))
        .cloned()
        .collect();
    assert_eq!(permissions.len(), 2);
    chat.messages.update(|items| items.push(awaiting("a7t1c2")));
    settle().await;
    mounted.click(".tui-btn-stop");
    settle().await;
    assert!(chat.messages.get_untracked().iter().any(|item| matches!(item, ConversationItem::ToolStep { id, awaiting_permission: false, result: Some(result), .. } if id == "a7t1c2" && result.summary == "cancelled")));
    assert!(
        mounted
            .state
            .fake
            .calls
            .borrow()
            .contains(&Call::CancelSession { session: 1 })
    );
}

#[wasm_bindgen_test]
async fn approval_preview_collapses_expands_and_preserves_alt_y() {
    use openwebide_core::{FileDiff, RunEvent};
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_session();
        chat_view(state)
    });
    settle().await;
    let chat = mounted.state.chat;
    chat.streaming.set(true);
    chat.streaming_session.set(Some(1));
    chat.current_run_anchor.set(Some(7));
    chat.apply_event(RunEvent::PermissionRequest {
        id: "a7t1c0".into(),
        name: "write_file".into(),
        summary: "write large file".into(),
        diff: Some(FileDiff {
            path: "large".into(),
            old: None,
            new: (0..100).map(|i| format!("line {i}\n")).collect(),
            old_unavailable: false,
            backup_path: None,
        }),
        note: None,
    });
    settle().await;
    assert_eq!(
        mounted
            .root
            .query_selector(".tui-diff-lines")
            .unwrap()
            .unwrap()
            .child_element_count(),
        40
    );
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Show full diff")
    );
    mounted.click(".tui-diff-box .btn");
    settle().await;
    assert_eq!(
        mounted
            .root
            .query_selector(".tui-diff-lines")
            .unwrap()
            .unwrap()
            .child_element_count(),
        100
    );
    assert!(
        !mounted
            .root
            .text_content()
            .unwrap()
            .contains("Show full diff")
    );
    let row = mounted
        .root
        .query_selector(".tui-tool-box")
        .unwrap()
        .unwrap();
    chat.messages.reconcile(|items| {
        if let ConversationItem::ToolStep {
            summary,
            diff,
            note,
            ..
        } = &mut items[0]
        {
            *summary = "updated preview".into();
            *note = Some("preview refreshed".into());
            diff.as_mut().unwrap().new.push_str("last line\n");
        }
    });
    settle().await;
    assert!(
        row.is_same_node(
            mounted
                .root
                .query_selector(".tui-tool-box")
                .unwrap()
                .as_ref()
                .map(AsRef::as_ref)
        )
    );
    assert_eq!(
        row.query_selector(".tui-diff-lines")
            .unwrap()
            .unwrap()
            .child_element_count(),
        101
    );
    assert!(row.text_content().unwrap().contains("updated preview"));
    assert!(row.text_content().unwrap().contains("preview refreshed"));
    mounted.key("y", "KeyY", true);
    settle().await;
    assert!(
        mounted
            .state
            .fake
            .calls
            .borrow()
            .contains(&Call::SetPermission {
                session: 1,
                id: "a7t1c0".into(),
                approved: true
            })
    );
    chat.apply_event(RunEvent::ToolResult {
        id: "a7t1c0".into(),
        name: "write_file".into(),
        ok: true,
        summary: "written".into(),
        diff: None,
    });
    settle().await;
    assert!(
        row.is_same_node(
            mounted
                .root
                .query_selector(".tui-tool-box")
                .unwrap()
                .as_ref()
                .map(AsRef::as_ref)
        )
    );
    assert!(
        row.query_selector(".tui-permission-prompt")
            .unwrap()
            .is_none()
    );
    assert!(row.text_content().unwrap().contains("written"));
}

#[wasm_bindgen_test]
async fn unreadable_approval_preview_shows_note_without_a_diff() {
    use openwebide_core::{FileDiff, RunEvent};
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_session();
        chat_view(state)
    });
    settle().await;
    let chat = mounted.state.chat;
    chat.streaming.set(true);
    chat.streaming_session.set(Some(1));
    chat.current_run_anchor.set(Some(7));
    let note = "The existing file isn't readable as text (binary or too large); approving overwrites it — a backup is kept and Reject restores it.";
    chat.apply_event(RunEvent::PermissionRequest {
        id: "a7t1c0".into(),
        name: "write_file".into(),
        summary: "write binary".into(),
        diff: Some(FileDiff {
            path: "binary".into(),
            old: None,
            new: "replacement".into(),
            old_unavailable: true,
            backup_path: None,
        }),
        note: Some(note.into()),
    });
    settle().await;
    let text = mounted.root.text_content().unwrap();
    assert!(text.contains(note));
    assert!(text.contains("diff unavailable"));
    assert!(
        mounted
            .root
            .query_selector(".tui-diff-line")
            .unwrap()
            .is_none()
    );
}

#[wasm_bindgen_test]
async fn approval_picker_and_shift_tab_share_database_state_in_both_modes() {
    use openwebide_core::{ApprovalMode, WorkspaceMode};
    use openwebide_frontend::state_actions::lifecycle::install_keyboard_shortcuts;
    fn shift_tab(mounted: &super::support::Mounted) {
        let init = web_sys::KeyboardEventInit::new();
        init.set_key("Tab");
        init.set_shift_key(true);
        init.set_bubbles(true);
        init.set_cancelable(true);
        mounted
            .element(".composer-input")
            .dispatch_event(
                &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
                    .unwrap(),
            )
            .unwrap();
    }
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            super::support::command_actions(state.clone());
            install_keyboard_shortcuts(state.chat);
            chat_view(state)
        });
        settle().await;
        assert_eq!(
            mounted.element(".tui-mode-badge").text_content().unwrap(),
            "[MANUAL]"
        );
        shift_tab(&mounted);
        super::support::wait_until("saved auto-accept mode", || {
            mounted.state.chat.approval_mode.get_untracked().get(&1)
                == Some(&ApprovalMode::AutoAcceptEdits)
        })
        .await;
        assert_eq!(
            mounted.state.chat.approval_mode.get_untracked().get(&1),
            Some(&ApprovalMode::AutoAcceptEdits)
        );
        assert_eq!(
            mounted.state.fake.settings.borrow()[&ApprovalMode::setting_key(1)],
            "\"auto_accept_edits\""
        );
        mounted.click(".tui-mode-badge");
        settle().await;
        for (index, choice) in ApprovalMode::CHOICES.into_iter().enumerate() {
            assert_eq!(
                mounted
                    .element(&format!(
                        ".approval-mode-menu button:nth-child({}) .ui-dropdown-item-description",
                        index + 1
                    ))
                    .text_content()
                    .unwrap(),
                choice.description()
            );
        }
        assert!(
            mounted
                .root
                .query_selector(".approval-mode-menu .form-hint")
                .unwrap()
                .is_none()
        );
        mounted.click(".approval-mode-menu button:nth-child(3)");
        super::support::wait_until("saved auto mode", || {
            mounted.state.chat.approval_mode.get_untracked().get(&1) == Some(&ApprovalMode::Auto)
        })
        .await;
        assert_eq!(
            mounted.state.chat.approval_mode.get_untracked().get(&1),
            Some(&ApprovalMode::Auto)
        );
        shift_tab(&mounted);
        super::support::wait_until("saved YOLO mode", || {
            mounted.state.chat.approval_mode.get_untracked().get(&1) == Some(&ApprovalMode::Yolo)
        })
        .await;
        assert_eq!(
            mounted.element(".tui-mode-badge").text_content().unwrap(),
            "[YOLO]"
        );
        shift_tab(&mounted);
        super::support::wait_until("saved manual mode", || {
            mounted.state.chat.approval_mode.get_untracked().get(&1) == Some(&ApprovalMode::Default)
        })
        .await;
        assert_eq!(
            mounted.state.chat.approval_mode.get_untracked().get(&1),
            Some(&ApprovalMode::Default)
        );
        assert_eq!(
            mounted.state.fake.settings.borrow()[&ApprovalMode::setting_key(1)],
            "\"default\""
        );
    }
}

#[wasm_bindgen_test]
async fn approval_diff_paint_uses_complete_source_context_in_both_modes() {
    use openwebide_core::{FileDiff, RunEvent, WorkspaceMode};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let host = super::language_support::LanguageHost::new().await;
        let mounted = host.mount(move |state| {
            state.seed_project();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            chat_view(state)
        });
        settle().await;
        let chat = mounted.state.chat;
        chat.streaming.set(true);
        chat.streaming_session.set(Some(1));
        chat.current_run_anchor.set(Some(7));
        chat.apply_event(RunEvent::PermissionRequest {
            id: "a7t1c0".into(),
            name: "write_file".into(),
            summary: "update comment".into(),
            diff: Some(FileDiff {
                path: "fixture.rs".into(),
                old: Some("/*\r\nold café\r\n*/\r\n".into()),
                new: "/*\r\nnew 😀\r\n*/\r\n".into(),
                old_unavailable: false,
                backup_path: None,
            }),
            note: None,
        });
        super::support::wait_until("approval diff host syntax", || {
            mounted
                .root
                .query_selector(".add .tok-comment.diff-word-add")
                .unwrap()
                .is_some()
                && mounted
                    .root
                    .query_selector(".del .tok-comment.diff-word-del")
                    .unwrap()
                    .is_some()
        })
        .await;
        let lines = mounted.element(".tui-diff-lines");
        assert_eq!(lines.child_element_count(), 2);
        assert!(
            lines
                .query_selector(".add .tok-comment.diff-word-add")
                .unwrap()
                .is_some()
        );
        assert!(
            lines
                .query_selector(".del .tok-comment.diff-word-del")
                .unwrap()
                .is_some()
        );
        assert!(lines.text_content().unwrap().contains("new 😀"));
        drop(mounted);
        host.close().await;
    }
}
