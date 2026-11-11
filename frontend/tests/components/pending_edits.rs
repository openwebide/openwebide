use leptos::prelude::*;
use openwebide_core::{EditDecision, FileDiff, PersistedEdit};
use openwebide_frontend::testing::fake_backend::Call;
use wasm_bindgen_test::*;

use super::support::{Mounted, editor_view, mount_test, settle, wait_until};

fn mount_diff(diff: FileDiff) -> Mounted {
    mount_test(move |state| {
        state.seed_project();
        state
            .fake
            .files
            .borrow_mut()
            .insert((1, diff.path.clone()), diff.new.clone());
        if let Some(backup) = &diff.backup_path {
            state
                .fake
                .files
                .borrow_mut()
                .insert((1, backup.clone()), "original bytes".into());
        }
        state.workspace.open_file.set(Some(diff.path.clone()));
        state.workspace.content.set(diff.new.clone().into());
        let edit = PersistedEdit {
            file: None,
            project_id: 1,
            path: diff.path.clone(),
            revision: 1,
            decision: EditDecision::Pending,
            diff,
        };
        state
            .fake
            .persisted_edits
            .borrow_mut()
            .insert((1, edit.path.clone()), edit.clone());
        state.workspace.set_persisted_edits(1, vec![edit]);
        editor_view(state)
    })
}

async fn wait_for_resolution_attempt(mounted: &Mounted, before: usize) {
    // A Web Lock is granted by a browser task, and fresh recovery reads may
    // involve timers/HTTP. Draining only Promise reactions cannot observe it.
    wait_until("edit resolution reached the backend or settled", || {
        mounted.state.fake.resolution_requests.borrow().len() != before
            || mounted
                .state
                .workspace
                .resolving_edits
                .with_untracked(std::collections::HashSet::is_empty)
    })
    .await;
    settle().await;
}
async fn accept(mounted: &Mounted) {
    let before = mounted.state.fake.resolution_requests.borrow().len();
    mounted.click_text("Accept");
    wait_for_resolution_attempt(mounted, before).await;
}

async fn reject(mounted: &Mounted) {
    let before = file_calls(mounted);
    let requests = mounted.state.fake.resolution_requests.borrow().len();
    settle().await;
    mounted.click_text("Reject");
    settle().await;
    assert_eq!(file_calls(mounted), before);
    mounted.click(".modal-footer .danger");
    wait_for_resolution_attempt(mounted, requests).await;
}

fn file_calls(mounted: &Mounted) -> Vec<Call> {
    mounted
        .state
        .fake
        .calls
        .borrow()
        .iter()
        .filter(|call| {
            matches!(
                call,
                Call::WriteFile { .. } | Call::CopyFile { .. } | Call::DeleteFile { .. }
            )
        })
        .cloned()
        .collect()
}

#[wasm_bindgen_test]
async fn split_diff_highlights_line_ending_changes_and_displays_notes() {
    for (old, new, note) in [
        ("a\r\n", "a\n", "⏎ CRLF → LF"),
        ("a", "a\n", "no newline at end of file"),
    ] {
        let mounted = mount_diff(FileDiff {
            path: "file.txt".into(),
            old: Some(old.into()),
            new: new.into(),
            old_unavailable: false,
            backup_path: None,
        });
        settle().await;
        mounted.click_text("Split");
        settle().await;
        for selector in [".sbs-del", ".sbs-add"] {
            let cell = mounted.element(selector);
            assert!(cell.text_content().unwrap().contains(note));
            assert!(cell.query_selector(".form-hint").unwrap().is_some());
        }
    }
}

#[wasm_bindgen_test]
async fn reject_restores_original_across_multiple_edits() {
    let mounted = mount_diff(FileDiff {
        path: "file.rs".into(),
        old: Some("v0".into()),
        new: "v1".into(),
        old_unavailable: false,
        backup_path: None,
    });
    let mut edit = mounted.state.fake.persisted_edits.borrow()[&(1, "file.rs".into())].clone();
    edit.revision = 2;
    edit.diff.new = "v2".into();
    mounted
        .state
        .fake
        .persisted_edits
        .borrow_mut()
        .insert((1, edit.path.clone()), edit.clone());
    mounted.state.workspace.set_persisted_edits(1, vec![edit]);
    reject(&mounted).await;
    assert_eq!(
        file_calls(&mounted),
        vec![Call::WriteFile {
            path: "file.rs".into(),
            content: "v0".into()
        }]
    );
    assert_eq!(
        mounted
            .state
            .fake
            .files
            .borrow()
            .get(&(1, "file.rs".into()))
            .map(String::as_str),
        Some("v0")
    );
    assert!(
        mounted
            .state
            .workspace
            .pending_edits
            .get_untracked()
            .is_empty()
    );
}

#[wasm_bindgen_test]
async fn reject_new_file_deletes_it() {
    let mounted = mount_diff(FileDiff {
        path: "new.rs".into(),
        old: None,
        new: "v2".into(),
        old_unavailable: false,
        backup_path: None,
    });
    reject(&mounted).await;
    assert_eq!(
        file_calls(&mounted),
        vec![Call::DeleteFile {
            path: "new.rs".into()
        }]
    );
    assert!(
        !mounted
            .state
            .fake
            .files
            .borrow()
            .contains_key(&(1, "new.rs".into()))
    );
}

#[wasm_bindgen_test]
async fn reject_unavailable_original_restores_backup_then_deletes_backup() {
    let mounted = mount_diff(FileDiff {
        path: "file.rs".into(),
        old: None,
        new: "v2".into(),
        old_unavailable: true,
        backup_path: Some("backup.rs".into()),
    });
    reject(&mounted).await;
    assert_eq!(
        file_calls(&mounted),
        vec![
            Call::CopyFile {
                from: "backup.rs".into(),
                to: "file.rs".into()
            },
            Call::DeleteFile {
                path: "backup.rs".into()
            }
        ]
    );
    assert_eq!(
        mounted
            .state
            .fake
            .files
            .borrow()
            .get(&(1, "file.rs".into()))
            .map(String::as_str),
        Some("original bytes")
    );
}

#[wasm_bindgen_test]
async fn accept_unavailable_original_only_deletes_backup() {
    let mounted = mount_diff(FileDiff {
        path: "file.rs".into(),
        old: None,
        new: "v2".into(),
        old_unavailable: true,
        backup_path: Some("backup.rs".into()),
    });
    settle().await;
    accept(&mounted).await;
    assert_eq!(
        file_calls(&mounted),
        vec![Call::DeleteFile {
            path: "backup.rs".into()
        }]
    );
    assert_eq!(
        mounted
            .state
            .fake
            .files
            .borrow()
            .get(&(1, "file.rs".into()))
            .map(String::as_str),
        Some("v2")
    );
    assert!(
        mounted
            .state
            .workspace
            .pending_edits
            .get_untracked()
            .is_empty()
    );
}

#[wasm_bindgen_test]
async fn persisted_edit_backend_contract_survives_replay_and_checks_revisions() {
    use openwebide_core::{EditDecision, ResolveEditRequest};
    use openwebide_frontend::backend::Backend;
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_session();
        view! { <div /> }
    });
    let fake = &mounted.state.fake;
    let diff = FileDiff {
        path: "a.txt".into(),
        old: Some("a".into()),
        new: "b".into(),
        old_unavailable: false,
        backup_path: None,
    };
    fake.upsert_tool_step(1, 1, "one", "write_file", "preview", Some(&diff))
        .await
        .unwrap();
    assert!(fake.list_pending_edits(1).await.unwrap().is_empty());
    fake.complete_tool_step(1, "one", true, "written", Some(&diff))
        .await
        .unwrap();
    assert_eq!(fake.list_pending_edits(1).await.unwrap()[0].diff, diff);
    assert!(fake.list_pending_edits(2).await.unwrap().is_empty());
    let mut request = ResolveEditRequest {
        path: "a.txt".into(),
        revision: 1,
        decision: EditDecision::Accepted,
    };
    *fake.resolution_error.borrow_mut() = Some("offline".into());
    assert!(fake.resolve_pending_edit(1, &request).await.is_err());
    assert_eq!(fake.list_pending_edits(1).await.unwrap().len(), 1);
    *fake.resolution_error.borrow_mut() = None;
    fake.resolve_pending_edit(1, &request).await.unwrap();
    fake.resolve_pending_edit(1, &request).await.unwrap();
    fake.complete_tool_step(1, "one", true, "replay", Some(&diff))
        .await
        .unwrap();
    assert!(fake.list_pending_edits(1).await.unwrap().is_empty());
    let newer = FileDiff {
        old: Some("b".into()),
        new: "c".into(),
        ..diff.clone()
    };
    fake.upsert_tool_step(1, 1, "two", "write_file", "write", None)
        .await
        .unwrap();
    fake.complete_tool_step(1, "two", true, "written", Some(&newer))
        .await
        .unwrap();
    assert!(fake.resolve_pending_edit(1, &request).await.is_err());
    assert_eq!(fake.list_pending_edits(1).await.unwrap()[0].revision, 2);
    request.revision = 2;
    request.decision = EditDecision::Rejected;
    fake.resolve_pending_edit(1, &request).await.unwrap();
    assert!(fake.list_pending_edits(1).await.unwrap().is_empty());
    fake.upsert_tool_step(1, 1, "failed", "write_file", "preview", Some(&diff))
        .await
        .unwrap();
    fake.complete_tool_step(1, "failed", false, "failed", Some(&diff))
        .await
        .unwrap();
    assert!(fake.list_pending_edits(1).await.unwrap().is_empty());
}

fn original_diff() -> FileDiff {
    FileDiff {
        path: "file.rs".into(),
        old: Some("original".into()),
        new: "changed".into(),
        old_unavailable: false,
        backup_path: None,
    }
}

#[wasm_bindgen_test]
async fn failed_restore_keeps_edit_actionable() {
    let mounted = mount_diff(original_diff());
    *mounted.state.fake.file_error.borrow_mut() = Some("permission denied".into());
    reject(&mounted).await;
    assert_eq!(
        mounted.state.workspace.pending_edits.get_untracked().len(),
        1
    );
    assert_eq!(
        mounted.state.ui.toast.get_untracked().as_deref(),
        Some("permission denied")
    );
    *mounted.state.fake.file_error.borrow_mut() = None;
    reject(&mounted).await;
    assert!(
        mounted
            .state
            .workspace
            .pending_edits
            .get_untracked()
            .is_empty()
    );
}

#[wasm_bindgen_test]
async fn failed_decision_retries_same_revision_and_preserves_backup() {
    let mounted = mount_diff(FileDiff {
        old: None,
        old_unavailable: true,
        backup_path: Some("backup.rs".into()),
        ..original_diff()
    });
    *mounted.state.fake.resolution_error.borrow_mut() = Some("offline".into());
    reject(&mounted).await;
    assert_eq!(
        mounted.state.workspace.pending_edits.get_untracked().len(),
        1
    );
    assert!(
        mounted
            .state
            .fake
            .files
            .borrow()
            .contains_key(&(1, "backup.rs".into()))
    );
    {
        let requests = mounted.state.fake.resolution_requests.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);
    }
    assert_eq!(
        mounted.state.ui.toast.get_untracked().as_deref(),
        Some("offline")
    );
    *mounted.state.fake.resolution_error.borrow_mut() = None;
    reject(&mounted).await;
    assert!(
        mounted
            .state
            .workspace
            .pending_edits
            .get_untracked()
            .is_empty()
    );
    assert!(
        !mounted
            .state
            .fake
            .files
            .borrow()
            .contains_key(&(1, "backup.rs".into()))
    );
}

#[wasm_bindgen_test]
async fn duplicate_accept_is_disabled_and_stale_resolution_keeps_newer_edit() {
    let mounted = mount_diff(original_diff());
    let (release, pending) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .resolution_results
        .borrow_mut()
        .push_back(pending);
    settle().await;
    accept(&mounted).await;
    assert!(mounted.element(".btn.approve").has_attribute("disabled"));
    mounted.click_text("Accept");
    assert_eq!(mounted.state.fake.resolution_requests.borrow().len(), 1);
    let mut edit = mounted.state.fake.persisted_edits.borrow()[&(1, "file.rs".into())].clone();
    edit.revision += 1;
    edit.diff.new = "newer".into();
    mounted
        .state
        .fake
        .persisted_edits
        .borrow_mut()
        .insert((1, edit.path.clone()), edit);
    release.send(Ok(())).unwrap();
    settle().await;
    assert_eq!(
        mounted.state.workspace.persisted_edits.get_untracked()["file.rs"].revision,
        2
    );
    assert!(!mounted.element(".btn.approve").has_attribute("disabled"));
}

#[wasm_bindgen_test]
async fn missing_backup_and_unavailable_original_remain_pending() {
    for backup_path in [Some("missing.rs".into()), None] {
        let mounted = mount_diff(FileDiff {
            old: None,
            old_unavailable: true,
            backup_path,
            ..original_diff()
        });
        mounted
            .state
            .fake
            .files
            .borrow_mut()
            .remove(&(1, "missing.rs".into()));
        settle().await;
        reject(&mounted).await;
        assert_eq!(
            mounted.state.workspace.pending_edits.get_untracked().len(),
            1
        );
        assert!(mounted.state.fake.resolution_requests.borrow().is_empty());
    }
}

#[wasm_bindgen_test]
async fn deleted_file_retry_after_failed_save_is_idempotent() {
    let mounted = mount_diff(FileDiff {
        old: None,
        ..original_diff()
    });
    *mounted.state.fake.resolution_error.borrow_mut() = Some("offline".into());
    reject(&mounted).await;
    assert!(
        !mounted
            .state
            .fake
            .files
            .borrow()
            .contains_key(&(1, "file.rs".into()))
    );
    assert_eq!(
        mounted.state.workspace.pending_edits.get_untracked().len(),
        1
    );
    *mounted.state.fake.resolution_error.borrow_mut() = None;
    // The first file action is expected; retry must not delete it a second time.
    mounted.state.fake.calls.borrow_mut().clear();
    reject(&mounted).await;
    assert!(file_calls(&mounted).is_empty());
    assert!(
        mounted
            .state
            .workspace
            .pending_edits
            .get_untracked()
            .is_empty()
    );
}

#[wasm_bindgen_test]
async fn local_folder_unavailable_keeps_rejection_pending_but_allows_acceptance() {
    let mounted = mount_diff(original_diff());
    mounted
        .state
        .projects
        .projects
        .update(|projects| projects[0].mode = openwebide_core::WorkspaceMode::Local);
    reject(&mounted).await;
    assert_eq!(
        mounted.state.workspace.pending_edits.get_untracked().len(),
        1
    );
    assert!(
        mounted
            .state
            .ui
            .toast
            .get_untracked()
            .unwrap()
            .contains("Grant folder access")
    );
    assert!(file_calls(&mounted).is_empty());
    accept(&mounted).await;
    assert!(
        mounted
            .state
            .workspace
            .pending_edits
            .get_untracked()
            .is_empty()
    );
}

#[wasm_bindgen_test]
async fn resolution_refresh_preserves_new_editor_input() {
    use wasm_bindgen::JsCast;
    for reject_edit in [false, true] {
        let mounted = mount_diff(original_diff());
        let (save, saving) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .resolution_results
            .borrow_mut()
            .push_back(saving);
        settle().await;
        if reject_edit {
            reject(&mounted).await;
        } else {
            accept(&mounted).await;
        }
        let (refresh, refreshing) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .pending_results
            .borrow_mut()
            .push_back(refreshing);
        save.send(Ok(())).unwrap();
        settle().await;
        assert_eq!(
            mounted.state.workspace.content.get_untracked(),
            if reject_edit { "original" } else { "changed" }
        );
        let textarea: web_sys::HtmlTextAreaElement =
            mounted.element(".editor-textarea").unchecked_into();
        textarea.set_value("new unsaved input");
        textarea
            .dispatch_event(&web_sys::Event::new("input").unwrap())
            .unwrap();
        refresh.send(Ok(vec![])).unwrap();
        settle().await;
        assert_eq!(
            mounted.state.workspace.content.get_untracked(),
            "new unsaved input"
        );
        assert!(mounted.state.workspace.dirty.get_untracked());
    }
}

#[wasm_bindgen_test]
async fn accept_finishes_editor_transition_after_early_authoritative_refresh() {
    use openwebide_frontend::{state::auth::AuthState, state_actions::workspace::refresh_pending};

    for background in [false, true] {
        for intervening_edit in [false, true] {
            let auth_slot = std::rc::Rc::new(std::cell::Cell::new(None));
            let slot = auth_slot.clone();
            let mounted = mount_test(move |state| {
                state.seed_project();
                slot.set(Some(expect_context::<AuthState>()));
                let edit = PersistedEdit {
                    file: None,
                    project_id: 1,
                    path: "file.rs".into(),
                    revision: 1,
                    decision: EditDecision::Pending,
                    diff: original_diff(),
                };
                state
                    .fake
                    .persisted_edits
                    .borrow_mut()
                    .insert((1, edit.path.clone()), edit.clone());
                state.workspace.set_persisted_edits(1, vec![edit]);
                state.workspace.open_file.set(Some("file.rs".into()));
                state.workspace.content.set("dirty draft".into());
                state.workspace.dirty.set(true);
                editor_view(state)
            });
            let (release, pending) = futures::channel::oneshot::channel();
            mounted
                .state
                .fake
                .resolution_results
                .borrow_mut()
                .push_back(pending);
            settle().await;
            accept(&mounted).await;
            assert_eq!(mounted.state.fake.resolution_requests.borrow().len(), 1);
            // Simulate a committed decision whose response has not arrived yet.
            mounted
                .state
                .fake
                .persisted_edits
                .borrow_mut()
                .get_mut(&(1, "file.rs".into()))
                .unwrap()
                .decision = EditDecision::Accepted;
            if intervening_edit {
                mounted.state.workspace.content.set("newer draft".into());
            }
            if background {
                mounted.state.workspace.switch_project(Some(1), 2);
                mounted.state.workspace.content.set("other project".into());
            }
            refresh_pending(
                mounted.state.api,
                mounted.state.projects,
                mounted.state.workspace,
                mounted.state.ui,
                auth_slot.get().unwrap(),
                1,
            )
            .await;
            if !background {
                assert!(
                    mounted
                        .state
                        .workspace
                        .persisted_edits
                        .get_untracked()
                        .is_empty()
                );
            }
            release.send(Ok(())).unwrap();
            settle().await;
            if background {
                assert_eq!(
                    mounted.state.workspace.content.get_untracked(),
                    "other project"
                );
                mounted.state.workspace.switch_project(Some(2), 1);
            }
            assert_eq!(
                mounted.state.workspace.content.get_untracked(),
                if intervening_edit {
                    "newer draft"
                } else {
                    "changed"
                }
            );
            assert_eq!(
                mounted.state.workspace.dirty.get_untracked(),
                intervening_edit
            );
            assert!(
                mounted
                    .state
                    .workspace
                    .pending_edits
                    .get_untracked()
                    .is_empty()
            );
        }
    }
}

#[wasm_bindgen_test]
async fn background_accept_updates_snapshot_and_preserves_intervening_changes() {
    for intervening_edit in [false, true] {
        let mounted = mount_diff(original_diff());
        mounted.state.workspace.content.set("dirty draft".into());
        mounted.state.workspace.dirty.set(true);
        let (release, pending) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .resolution_results
            .borrow_mut()
            .push_back(pending);
        settle().await;
        accept(&mounted).await;
        if intervening_edit {
            mounted.state.workspace.content.set("newer draft".into());
        }
        mounted.state.workspace.switch_project(Some(1), 2);
        mounted
            .state
            .workspace
            .open_file
            .set(Some("other.rs".into()));
        mounted.state.workspace.content.set("other project".into());
        release.send(Ok(())).unwrap();
        settle().await;
        assert_eq!(
            mounted.state.workspace.content.get_untracked(),
            "other project"
        );
        mounted.state.workspace.switch_project(Some(2), 1);
        settle().await;
        assert_eq!(
            mounted.state.workspace.content.get_untracked(),
            if intervening_edit {
                "newer draft"
            } else {
                "changed"
            }
        );
        assert_eq!(
            mounted.state.workspace.dirty.get_untracked(),
            intervening_edit
        );
        assert!(
            mounted
                .state
                .workspace
                .pending_edits
                .get_untracked()
                .is_empty()
        );
    }
}

#[wasm_bindgen_test]
async fn interrupted_delete_listing_does_not_mutate_files() {
    use openwebide_frontend::state::auth::AuthState;
    for delete_project in [false, true] {
        let auth_slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let slot = auth_slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            slot.set(Some(expect_context::<AuthState>()));
            let diff = FileDiff {
                old: None,
                ..original_diff()
            };
            let edit = PersistedEdit {
                file: None,
                project_id: 1,
                path: diff.path.clone(),
                revision: 1,
                decision: EditDecision::Pending,
                diff,
            };
            state
                .fake
                .files
                .borrow_mut()
                .insert((1, edit.path.clone()), edit.diff.new.clone());
            state
                .fake
                .persisted_edits
                .borrow_mut()
                .insert((1, edit.path.clone()), edit.clone());
            state.workspace.open_file.set(Some(edit.path.clone()));
            state.workspace.content.set(edit.diff.new.clone().into());
            state.workspace.set_persisted_edits(1, vec![edit]);
            editor_view(state)
        });
        settle().await;
        let (release, pending) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .file_list_results
            .borrow_mut()
            .push_back(pending);
        mounted.click_text("Reject");
        settle().await;
        mounted.click(".modal-footer .danger");
        wait_until("held delete listing", || {
            mounted.state.fake.file_list_results.borrow().is_empty()
        })
        .await;
        if delete_project {
            mounted.state.projects.projects.set(vec![]);
            mounted.state.workspace.clear_active();
        } else {
            auth_slot.get().unwrap().logout();
            mounted.state.workspace.reset();
            mounted.state.projects.projects.set(vec![]);
        }
        release
            .send(Ok(vec![openwebide_core::FileEntry {
                name: "file.rs".into(),
                path: "file.rs".into(),
                is_dir: false,
                size: 0,
            }]))
            .unwrap();
        settle().await;
        assert!(
            mounted
                .state
                .fake
                .files
                .borrow()
                .contains_key(&(1, "file.rs".into()))
        );
        assert!(file_calls(&mounted).is_empty());
        assert!(mounted.state.fake.resolution_requests.borrow().is_empty());
    }
}

async fn delayed_resolution_after_agent_write(dirty: bool, rejected: bool) {
    use openwebide_core::{ChatMessage, Role, RunEvent};
    use openwebide_frontend::{
        backend::Backend,
        components::{ConfirmDialog, Editor},
        state::auth::AuthState,
        state_actions::{
            chat::{ChatActionContext, ChatActions},
            workspace::{WorkspaceActions, refresh_pending},
        },
    };
    let clean_content = if rejected { "changed" } else { "original" };
    let auth_slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let send_slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let auth_copy = auth_slot.clone();
    let send_copy = send_slot.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.seed_session();
        state.seed_connection();
        auth_copy.set(Some(expect_context::<AuthState>()));
        let edit = PersistedEdit {
            file: None,
            project_id: 1,
            path: "file.rs".into(),
            revision: 1,
            decision: EditDecision::Pending,
            diff: original_diff(),
        };
        state
            .fake
            .persisted_edits
            .borrow_mut()
            .insert((1, edit.path.clone()), edit.clone());
        state
            .fake
            .files
            .borrow_mut()
            .insert((1, edit.path.clone()), edit.diff.new.clone());
        state.workspace.set_persisted_edits(1, vec![edit]);
        state.workspace.open_file.set(Some("file.rs".into()));
        state
            .workspace
            .content
            .set(if dirty { "dirty draft" } else { clean_content }.into());
        state.workspace.dirty.set(dirty);
        let read_only = RwSignal::new(false);
        let actions = WorkspaceActions::new(
            state.api,
            state.projects,
            state.workspace,
            state.ui,
            read_only,
            Callback::new(|()| ()),
        );
        let chat_actions = ChatActions::new(ChatActionContext {
            api: state.api,
            chat: state.chat,
            projects: state.projects,
            workspace: state.workspace,
            settings: state.settings,
            ui: state.ui,
            git: state.git,
            bridge: state.bridge,
            request_open: actions.request_open,
            refresh_git: Callback::new(|()| ()),
            on_sync_click: Callback::new(|()| ()),
            on_commit: Callback::new(|_: openwebide_core::GitCommitRequest| ()),
            on_checkout: Callback::new(|_: (String, bool)| ()),
        });
        send_copy.set(Some(chat_actions.send));
        view! {
            <Editor read_only=read_only.into() on_open_lossy=actions.on_open_lossy
                on_save=actions.on_save on_accept=actions.on_accept on_reject=actions.on_reject />
            <ConfirmDialog />
        }
    });
    // Hold the reply after the decision commits so a later write can precede delivery.
    let (old_response, old_response_pending) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .resolution_response_results
        .borrow_mut()
        .push_back(old_response_pending);
    settle().await;
    if rejected {
        reject(&mounted).await;
    } else {
        accept(&mounted).await;
    }
    assert_eq!(
        mounted.state.fake.persisted_edits.borrow()[&(1, "file.rs".into())].decision,
        if rejected {
            EditDecision::Rejected
        } else {
            EditDecision::Accepted
        }
    );
    refresh_pending(
        mounted.state.api,
        mounted.state.projects,
        mounted.state.workspace,
        mounted.state.ui,
        auth_slot.get().unwrap(),
        1,
    )
    .await;
    assert!(
        mounted
            .state
            .workspace
            .persisted_edits
            .get_untracked()
            .is_empty()
    );

    // Keep hydration behind the tool event to exercise the editor before the record loads.
    let newer = FileDiff {
        old: Some("changed".into()),
        new: if dirty {
            "newer agent content"
        } else {
            clean_content
        }
        .into(),
        ..original_diff()
    };
    mounted
        .state
        .fake
        .files
        .borrow_mut()
        .insert((1, "file.rs".into()), newer.new.clone());
    mounted
        .state
        .fake
        .upsert_tool_step(1, 1, "new-agent", "write_file", "written", None)
        .await
        .unwrap();
    mounted
        .state
        .fake
        .complete_tool_step(1, "new-agent", true, "written", Some(&newer))
        .await
        .unwrap();
    let (new_list, new_list_pending) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .pending_results
        .borrow_mut()
        .push_back(new_list_pending);
    mounted
        .state
        .fake
        .scripted_events
        .borrow_mut()
        .push_back(vec![
            RunEvent::ToolCall {
                id: "new-agent".into(),
                name: "write_file".into(),
                summary: "write".into(),
            },
            RunEvent::ToolResult {
                id: "new-agent".into(),
                name: "write_file".into(),
                ok: true,
                summary: "written".into(),
                diff: Some(newer),
            },
            RunEvent::Done {
                message: ChatMessage {
                    id: 3,
                    session_id: 1,
                    role: Role::Assistant,
                    content: "done".into(),
                    created_at: 0,
                    tool_calls: None,
                    tool_call_id: None,
                    usage: None,
                },
            },
        ]);
    mounted.state.chat.draft.set("edit again".into());
    send_slot.get().unwrap().run(());
    settle().await;
    assert!(
        mounted
            .state
            .workspace
            .persisted_edits
            .get_untracked()
            .is_empty()
    );
    assert_eq!(
        mounted.state.workspace.content.get_untracked(),
        if dirty { "dirty draft" } else { clean_content }
    );
    assert!(
        mounted
            .state
            .fake
            .calls
            .borrow()
            .iter()
            .any(|call| matches!(call, Call::SendMessage { .. }))
    );
    old_response.send(Ok(())).unwrap();
    settle().await;
    // The final resolution refresh can see revision 2; the transition has already run.
    assert_eq!(
        mounted.state.workspace.persisted_edits.get_untracked()["file.rs"].revision,
        2
    );
    assert_eq!(
        mounted.state.fake.files.borrow()[&(1, "file.rs".into())],
        if dirty {
            "newer agent content"
        } else {
            clean_content
        }
    );
    new_list.send(Ok(vec![])).unwrap();
    settle().await;
    assert_eq!(
        mounted.state.workspace.content.get_untracked(),
        if dirty { "dirty draft" } else { clean_content },
        "a delayed resolution must preserve the buffer after a newer agent write"
    );
    assert_eq!(mounted.state.workspace.dirty.get_untracked(), dirty);
}

#[wasm_bindgen_test]
async fn delayed_accept_preserves_dirty_buffer_after_agent_write() {
    delayed_resolution_after_agent_write(true, false).await;
}

#[wasm_bindgen_test]
async fn delayed_accept_preserves_clean_buffer_after_agent_write() {
    delayed_resolution_after_agent_write(false, false).await;
}

#[wasm_bindgen_test]
async fn delayed_reject_preserves_dirty_buffer_after_agent_write() {
    delayed_resolution_after_agent_write(true, true).await;
}

#[wasm_bindgen_test]
async fn delayed_reject_preserves_clean_buffer_after_agent_write() {
    delayed_resolution_after_agent_write(false, true).await;
}

#[wasm_bindgen_test]
async fn delayed_accept_preserves_editor_after_newer_revision_hydrates() {
    use openwebide_frontend::{state::auth::AuthState, state_actions::workspace::refresh_pending};

    let auth_slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let slot = auth_slot.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        slot.set(Some(expect_context::<AuthState>()));
        let edit = PersistedEdit {
            file: None,
            project_id: 1,
            path: "file.rs".into(),
            revision: 1,
            decision: EditDecision::Pending,
            diff: original_diff(),
        };
        state
            .fake
            .persisted_edits
            .borrow_mut()
            .insert((1, edit.path.clone()), edit.clone());
        state.workspace.set_persisted_edits(1, vec![edit]);
        state.workspace.open_file.set(Some("file.rs".into()));
        state.workspace.content.set("dirty draft".into());
        state.workspace.dirty.set(true);
        editor_view(state)
    });
    let (release, pending) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .resolution_response_results
        .borrow_mut()
        .push_back(pending);
    settle().await;
    accept(&mounted).await;
    assert_eq!(
        mounted.state.fake.persisted_edits.borrow()[&(1, "file.rs".into())].decision,
        EditDecision::Accepted
    );
    let mut newer = mounted.state.fake.persisted_edits.borrow()[&(1, "file.rs".into())].clone();
    newer.revision = 2;
    newer.decision = EditDecision::Pending;
    newer.diff.new = "newer agent content".into();
    mounted
        .state
        .fake
        .persisted_edits
        .borrow_mut()
        .insert((1, newer.path.clone()), newer.clone());
    refresh_pending(
        mounted.state.api,
        mounted.state.projects,
        mounted.state.workspace,
        mounted.state.ui,
        auth_slot.get().unwrap(),
        1,
    )
    .await;
    assert_eq!(
        mounted.state.workspace.persisted_edits.get_untracked()["file.rs"],
        newer
    );
    release.send(Ok(())).unwrap();
    settle().await;
    assert_eq!(
        mounted.state.workspace.content.get_untracked(),
        "dirty draft"
    );
    assert!(mounted.state.workspace.dirty.get_untracked());
    assert_eq!(
        mounted.state.workspace.persisted_edits.get_untracked()["file.rs"],
        newer
    );
}

#[wasm_bindgen_test]
async fn background_backup_rejection_restores_editor_without_overwriting_new_input() {
    for intervening_edit in [false, true] {
        let mounted = mount_diff(FileDiff {
            path: "file.rs".into(),
            old: None,
            new: "v2".into(),
            old_unavailable: true,
            backup_path: Some("backup.rs".into()),
        });
        let (release, pending) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .resolution_results
            .borrow_mut()
            .push_back(pending);
        settle().await;
        reject(&mounted).await;
        if intervening_edit {
            mounted.state.workspace.content.set("newer draft".into());
        }
        mounted.state.workspace.switch_project(Some(1), 2);
        mounted.state.workspace.content.set("other project".into());
        release.send(Ok(())).unwrap();
        settle().await;
        assert_eq!(
            mounted.state.workspace.content.get_untracked(),
            "other project"
        );
        mounted.state.workspace.switch_project(Some(2), 1);
        assert_eq!(
            mounted.state.workspace.content.get_untracked(),
            if intervening_edit {
                "newer draft"
            } else {
                "original bytes"
            }
        );
    }
}
