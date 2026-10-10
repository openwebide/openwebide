use super::support::{Mounted, mount_test, settle, wait_until};
use leptos::prelude::*;
use openwebide_core::{FileEntry, WorkspaceMode};
use openwebide_frontend::{
    components::{ConfirmDialog, FileTree, PromptDialog},
    state_actions::{file_tree::FileTreeActions, workspace::WorkspaceActions},
    workspace::Workspace,
};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_test::*;

#[wasm_bindgen(inline_js = r#"
export async function treeFolder() {
    const root = await navigator.storage.getDirectory();
    const name = 'tree-test-' + crypto.randomUUID();
    const handle = await root.getDirectoryHandle(name, {create:true});
    const file = await handle.getFileHandle('a.txt', {create:true});
    const writer = await file.createWritable(); await writer.write('Original'); await writer.close();
    return {root, name, handle};
}
export function treeHandle(folder) {return folder.handle;}
export async function treeCleanup(folder) {await folder.root.removeEntry(folder.name, {recursive:true});}
export function treeMiddleClick(row, button) {
    row.dispatchEvent(new MouseEvent('mousedown', {bubbles:true,cancelable:true,button}));
    row.dispatchEvent(new MouseEvent('auxclick', {bubbles:true,cancelable:true,button}));
}
export function treeContext(row) {
    const box = row.getBoundingClientRect();
    row.dispatchEvent(new MouseEvent('contextmenu', {bubbles:true,cancelable:true,clientX:box.left+20,clientY:box.top+5}));
}
export function treeLongPress(row, move) {
    row.dispatchEvent(new PointerEvent('pointerdown', {bubbles:true,pointerType:'touch',clientX:40,clientY:50}));
    if (move) row.dispatchEvent(new PointerEvent('pointermove', {bubbles:true,pointerType:'touch',clientX:60,clientY:50}));
}
export function treeClipboardFallback() {
    const clipboard = Object.getOwnPropertyDescriptor(navigator, 'clipboard');
    const command = document.execCommand;
    const state = {text:null};
    Object.defineProperty(navigator, 'clipboard', {configurable:true,value:{writeText:()=>Promise.reject(new Error('denied'))}});
    document.execCommand = name => {state.text=document.activeElement.value;return name==='copy';};
    state.restore = () => {
        if (clipboard) Object.defineProperty(navigator, 'clipboard', clipboard);
        else delete navigator.clipboard;
        document.execCommand=command;
    };
    return state;
}
export function treeClipboardText(state) {return state.text;}
export function treeClipboardRestore(state) {state.restore();}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    async fn treeFolder() -> Result<JsValue, JsValue>;
    fn treeHandle(folder: &JsValue) -> JsValue;
    #[wasm_bindgen(catch)]
    async fn treeCleanup(folder: &JsValue) -> Result<(), JsValue>;
    fn treeMiddleClick(row: &web_sys::HtmlElement, button: i16);
    fn treeContext(row: &web_sys::HtmlElement);
    fn treeLongPress(row: &web_sys::HtmlElement, move_pointer: bool);
    fn treeClipboardFallback() -> JsValue;
    fn treeClipboardText(state: &JsValue) -> String;
    fn treeClipboardRestore(state: &JsValue);
}
fn file(path: &str) -> FileEntry {
    FileEntry {
        name: path.into(),
        path: path.into(),
        is_dir: false,
        size: 8,
    }
}
fn fixture(
    handle: Option<JsValue>,
) -> (
    Mounted,
    std::rc::Rc<std::cell::Cell<Option<FileTreeActions>>>,
) {
    fixture_with_changes(handle, false)
}
fn fixture_with_changes(
    handle: Option<JsValue>,
    changes: bool,
) -> (
    Mounted,
    std::rc::Rc<std::cell::Cell<Option<FileTreeActions>>>,
) {
    let slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let captured = slot.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state
            .fake
            .files
            .borrow_mut()
            .insert((1, "a.txt".into()), "Original".into());
        if let Some(handle) = handle {
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = WorkspaceMode::Local);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
        }
        state.workspace.entries.update(|entries| {
            entries.insert("".into(), vec![file("a.txt")]);
        });
        let actions = WorkspaceActions::new(
            state.api,
            state.projects,
            state.workspace,
            state.ui,
            RwSignal::new(false),
            Callback::new(|()| ()),
        );
        let tree = expect_context::<FileTreeActions>();
        tree.send_prompt.set(Some(Callback::new(move |prompt| {
            state.chat.notice.set(Some(prompt));
        })));
        captured.set(Some(tree));
        view! {<FileTree on_toggle=actions.on_toggle on_open=actions.request_open /><PromptDialog /><ConfirmDialog />{changes.then(|| view! { <openwebide_frontend::components::GitPane on_open=actions.request_open on_load_git_diff=Callback::new(|()| ()) on_discard_git_diff=Callback::new(|()| ()) /> })}}
    });
    (mounted, slot)
}
fn choose(mounted: &Mounted, label: &str) {
    let buttons = mounted
        .root
        .query_selector_all(".ui-dropdown-menu button")
        .unwrap();
    let button = (0..buttons.length())
        .filter_map(|index| buttons.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        .find(|button| button.text_content().as_deref() == Some(label))
        .unwrap_or_else(|| panic!("Missing action {label}"));
    button.click();
}

#[wasm_bindgen_test]
async fn tree_menu_rename_dirty_guard_delete_and_stale_dialog_work_in_both_modes() {
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let (mounted, slot) = fixture(folder.as_ref().map(treeHandle));
        settle().await;
        let actions = slot.get().unwrap();
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        mounted.state.workspace.open_file.set(Some("a.txt".into()));
        mounted.state.workspace.content.set("Unsaved".into());
        mounted.state.workspace.dirty.set(true);
        actions.move_entry(&file("a.txt"), true);
        settle().await;
        assert!(mounted.state.ui.prompt.get_untracked().is_none());
        assert!(
            mounted
                .state
                .ui
                .toast
                .get_untracked()
                .unwrap()
                .contains("unsaved")
        );
        assert_eq!(files.read("a.txt").await.unwrap(), "Original");
        mounted.state.workspace.dirty.set(false);
        let row = mounted.element(".tree-item");
        assert_eq!(
            row.get_attribute("data-tree-path").as_deref(),
            Some("a.txt"),
            "{}",
            row.outer_html()
        );
        treeContext(&row);
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".ui-dropdown-menu")
                .unwrap()
                .is_some()
        );
        choose(&mounted, "Rename");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".ui-dropdown-menu")
                .unwrap()
                .is_none()
        );
        mounted
            .state
            .ui
            .prompt
            .get_untracked()
            .unwrap()
            .on_submit
            .run("renamed.txt".into());
        wait_until("rename completion", || !actions.busy.get_untracked()).await;
        let renamed = files.read("renamed.txt").await;
        assert!(
            renamed.is_ok(),
            "Rename failed: toast={:?}, entries={:?}, local={local}",
            mounted.state.ui.toast.get_untracked(),
            files.list("").await.unwrap()
        );
        assert_eq!(renamed.unwrap(), "Original");
        assert!(
            !files
                .list("")
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.path == "a.txt")
        );
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("renamed.txt")
        );
        // A dialog opened in the old scope must not mutate any project after a switch.
        actions.move_entry(&file("renamed.txt"), true);
        let pending = mounted.state.ui.prompt.get_untracked().unwrap();
        mounted.state.projects.active_project.set(None);
        settle().await;
        pending.on_submit.run("stale.txt".into());
        settle().await;
        assert!(
            !files
                .list("")
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.path == "stale.txt")
        );
        mounted.state.projects.active_project.set(Some(1));
        settle().await;
        mounted
            .state
            .workspace
            .open_file
            .set(Some("renamed.txt".into()));
        mounted.state.workspace.dirty.set(true);
        actions.delete(&file("renamed.txt"));
        settle().await;
        let confirm = mounted.state.ui.confirm.get_untracked().unwrap();
        assert!(confirm.message.contains("Unsaved"));
        assert_eq!(files.read("renamed.txt").await.unwrap(), "Original");
        confirm.action.run(());
        wait_until("delete completion", || !actions.busy.get_untracked()).await;
        assert!(files.list("").await.unwrap().is_empty());
        assert!(mounted.state.workspace.open_file.get_untracked().is_none());
        assert!(!mounted.state.workspace.dirty.get_untracked());
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
        drop(mounted);
    }
}

#[wasm_bindgen_test]
async fn tree_menu_keyboard_touch_dismissal_and_editable_chat_prompts_in_both_modes() {
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let (mounted, _) = fixture(folder.as_ref().map(treeHandle));
        settle().await;
        let row = mounted.element(".tree-item");
        row.focus().unwrap();
        let init = web_sys::KeyboardEventInit::new();
        init.set_key("F10");
        init.set_shift_key(true);
        init.set_bubbles(true);
        init.set_cancelable(true);
        row.dispatch_event(
            &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap(),
        )
        .unwrap();
        settle().await;
        let menu = mounted.element(".ui-dropdown-menu");
        assert!(
            menu.contains(
                web_sys::window()
                    .unwrap()
                    .document()
                    .unwrap()
                    .active_element()
                    .as_ref()
                    .map(AsRef::as_ref)
            )
        );
        mounted.state.chat.draft.set("Existing question".into());
        choose(&mounted, "Explain in chat");
        settle().await;
        assert_eq!(
            mounted.state.chat.draft.get_untracked(),
            "Existing question"
        );
        assert_eq!(
            mounted.state.chat.notice.get_untracked().as_deref(),
            Some("Explain how this works: @file:\"a.txt\"")
        );
        treeLongPress(&row, true);
        openwebide_frontend::util::sleep_ms(550).await;
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".ui-dropdown-menu")
                .unwrap()
                .is_none()
        );
        treeLongPress(&row, false);
        openwebide_frontend::util::sleep_ms(550).await;
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".ui-dropdown-menu")
                .unwrap()
                .is_some()
        );
        mounted.click(".ui-dropdown-backdrop");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".ui-dropdown-menu")
                .unwrap()
                .is_none()
        );
        assert!(mounted.root.query_selector(".tree-item").unwrap().is_some());
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
        drop(mounted);
    }
}

#[wasm_bindgen_test]
async fn tree_folder_create_move_ignore_and_collision_contract_in_both_modes() {
    use openwebide_core::vfs::VfsEntryKind;
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let (mounted, slot) = fixture(folder.as_ref().map(treeHandle));
        settle().await;
        let actions = slot.get().unwrap();
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        actions.create("", VfsEntryKind::Directory);
        mounted
            .state
            .ui
            .prompt
            .get_untracked()
            .unwrap()
            .on_submit
            .run("src/empty".into());
        wait_until("folder creation", || !actions.busy.get_untracked()).await;
        assert!(
            files
                .list("src")
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.path == "src/empty" && entry.is_dir)
        );
        actions.create("src", VfsEntryKind::File);
        mounted
            .state
            .ui
            .prompt
            .get_untracked()
            .unwrap()
            .on_submit
            .run("src/new.txt".into());
        wait_until("file creation", || !actions.busy.get_untracked()).await;
        files.write("src/new.txt", "Nested data").await.unwrap();
        files
            .write_bytes("src/bytes.bin", &[0, 255, 128, 10])
            .await
            .unwrap();
        // Move a whole folder, preserving empty children and the open file path.
        mounted
            .state
            .workspace
            .open_file
            .set(Some("src/new.txt".into()));
        mounted.state.workspace.dirty.set(false);
        let directory = FileEntry {
            name: "src".into(),
            path: "src".into(),
            is_dir: true,
            size: 0,
        };
        actions.move_entry(&directory, false);
        mounted
            .state
            .ui
            .prompt
            .get_untracked()
            .unwrap()
            .on_submit
            .run("lib".into());
        wait_until("folder move", || !actions.busy.get_untracked()).await;
        assert_eq!(files.read("lib/new.txt").await.unwrap(), "Nested data");
        assert!(files.list("lib/empty").await.unwrap().is_empty());
        assert_eq!(
            files.read_bytes("lib/bytes.bin").await.unwrap(),
            [0, 255, 128, 10]
        );
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("lib/new.txt")
        );
        // Existing destinations are never overwritten, including folders.
        actions.move_entry(&file("a.txt"), false);
        mounted
            .state
            .ui
            .prompt
            .get_untracked()
            .unwrap()
            .on_submit
            .run("lib/new.txt".into());
        wait_until("collision result", || !actions.busy.get_untracked()).await;
        assert!(
            mounted
                .state
                .ui
                .toast
                .get_untracked()
                .unwrap()
                .contains("already exists")
        );
        assert_eq!(files.read("a.txt").await.unwrap(), "Original");
        assert_eq!(files.read("lib/new.txt").await.unwrap(), "Nested data");
        files.write(".gitignore", "# Keep\r\n/cache").await.unwrap();
        mounted.state.workspace.pending_edits.update(|edits| {
            edits.insert(
                ".gitignore".into(),
                openwebide_core::FileDiff {
                    path: ".gitignore".into(),
                    old: Some("# Keep\r\n/cache".into()),
                    new: "pending review".into(),
                    old_unavailable: false,
                    backup_path: None,
                },
            );
        });
        actions.ignore(&file("lib/new.txt"));
        settle().await;
        assert_eq!(files.read(".gitignore").await.unwrap(), "# Keep\r\n/cache");
        assert!(
            mounted
                .state
                .ui
                .toast
                .get_untracked()
                .unwrap()
                .contains("pending edits")
        );
        mounted
            .state
            .workspace
            .pending_edits
            .set(Default::default());
        actions.ignore(&FileEntry {
            name: "lib".into(),
            path: "lib".into(),
            is_dir: true,
            size: 0,
        });
        wait_until("ignore rule", || !actions.busy.get_untracked()).await;
        assert_eq!(
            files.read(".gitignore").await.unwrap(),
            "# Keep\r\n/cache\r\n/lib/\r\n"
        );
        // Local operations must leave the fake remote filesystem alone.
        if local {
            assert_eq!(
                mounted
                    .state
                    .fake
                    .files
                    .borrow()
                    .get(&(1, "a.txt".into()))
                    .unwrap(),
                "Original"
            );
        }
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
        drop(mounted);
    }
}

#[wasm_bindgen_test]
async fn copy_path_falls_back_after_clipboard_denial_and_restores_focus() {
    let (mounted, slot) = fixture(None);
    settle().await;
    let row = mounted.element(".tree-item");
    row.focus().unwrap();
    let clipboard = treeClipboardFallback();
    slot.get().unwrap().copy_path("nested/café file.txt");
    settle().await;
    let copied = treeClipboardText(&clipboard);
    treeClipboardRestore(&clipboard);
    assert_eq!(copied, "nested/café file.txt");
    assert!(
        document()
            .active_element()
            .unwrap()
            .is_same_node(Some(row.as_ref()))
    );
    assert!(mounted.state.ui.toast.get_untracked().is_none());
}

#[wasm_bindgen_test]
async fn tree_dialogs_abort_after_account_folder_and_bridge_changes() {
    use openwebide_core::vfs::VfsEntryKind;
    let folder = treeFolder().await.unwrap();
    let (mounted, slot) = fixture(Some(treeHandle(&folder)));
    settle().await;
    let actions = slot.get().unwrap();
    let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
    actions.create("", VfsEntryKind::File);
    let pending = mounted.state.ui.prompt.get_untracked().unwrap();
    mounted
        .state
        .projects
        .projects
        .update(|projects| projects[0].path = Some("verified/bridge/path".into()));
    pending.on_submit.run("metadata-change.txt".into());
    wait_until("creation after local discovery metadata changes", || {
        !actions.busy.get_untracked()
    })
    .await;
    assert_eq!(files.read("metadata-change.txt").await.unwrap(), "");
    for change in 0..3 {
        actions.create("", VfsEntryKind::File);
        let pending = mounted.state.ui.prompt.get_untracked().unwrap();
        match change {
            0 => mounted
                .state
                .auth
                .generation
                .update(|generation| *generation += 1),
            1 => mounted.state.projects.local_handles.update(|handles| {
                handles.remove(&1);
            }),
            _ => mounted
                .state
                .settings
                .bridge_url
                .set("ws://different.test:3001".into()),
        }
        // Do not settle: guards must detect changes before any Effect runs.
        pending.on_submit.run(format!("stale-{change}"));
        settle().await;
        assert!(
            !files
                .list("")
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.path == format!("stale-{change}"))
        );
        mounted.state.projects.local_handles.update(|handles| {
            handles.insert(1, treeHandle(&folder).unchecked_into());
        });
        settle().await;
    }
    treeCleanup(&folder).await.unwrap();
    drop(mounted);
}

fn git_response(mounted: &Mounted, response: Result<openwebide_core::git::GitPathChanges, String>) {
    let (sender, receiver) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .git_path_results
        .borrow_mut()
        .push_back(receiver);
    sender.send(response).unwrap();
}
#[wasm_bindgen_test]
async fn tree_git_actions_share_both_adapters_and_report_failures() {
    use super::project_git::{Http, gitCalls, gitChange, gitHttp, restore_token, token};
    use openwebide_core::git::{GitPathAction, GitPathChanges};
    let old = token().await;
    let http = Http(gitHttp());
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let (mounted, slot) = fixture(folder.as_ref().map(treeHandle));
        if local {
            mounted
                .state
                .settings
                .bridge_url
                .set("ws://git.test:3001".into());
        }
        settle().await;
        let actions = slot.get().unwrap();
        let changed = GitPathChanges {
            has_head: true,
            staged: ["a.txt".into()].into(),
            unstaged: ["a.txt".into()].into(),
            ..Default::default()
        };
        if !local {
            git_response(&mounted, Ok(changed.clone()));
        }
        treeContext(&mounted.element(".tree-item"));
        wait_until("Git menu discovery", || {
            let buttons = mounted
                .root
                .query_selector_all(".ui-dropdown-menu button:not(:disabled)")
                .unwrap();
            (0..buttons.length())
                .filter_map(|index| buttons.item(index))
                .filter(|button| {
                    matches!(
                        button.text_content().as_deref(),
                        Some("Stage" | "Unstage" | "Revert changes")
                    )
                })
                .count()
                == 3
        })
        .await;
        if !local {
            git_response(&mounted, Ok(changed.clone()));
            git_response(&mounted, Ok(GitPathChanges::default()));
        }
        choose(&mounted, "Stage");
        wait_until("Git stage", || !actions.busy.get_untracked()).await;
        for action in [GitPathAction::Unstage, GitPathAction::Revert] {
            if !local {
                git_response(&mounted, Ok(changed.clone()));
                git_response(&mounted, Ok(GitPathChanges::default()));
            }
            actions.git_action("a.txt", action);
            if action == GitPathAction::Revert {
                let confirm = mounted.state.ui.confirm.get_untracked().unwrap();
                assert!(confirm.message.contains("Untracked files are preserved"));
                confirm.action.run(());
            }
            wait_until("Git action", || !actions.busy.get_untracked()).await;
        }
        if local {
            gitChange(&http.0, "pathError", true);
        } else {
            git_response(&mounted, Ok(changed.clone()));
            git_response(&mounted, Err("path action failed".into()));
        }
        actions.git_action("a.txt", GitPathAction::Stage);
        wait_until("Git action error", || !actions.busy.get_untracked()).await;
        assert!(
            mounted
                .state
                .ui
                .toast
                .get_untracked()
                .unwrap()
                .contains("path action failed")
        );
        if local {
            assert!(mounted.state.fake.git_path_requests.borrow().is_empty());
            let calls: Vec<serde_json::Value> = serde_json::from_str(&gitCalls(&http.0)).unwrap();
            let requests = calls
                .iter()
                .filter(|call| call["path"] == "/git/path")
                .collect::<Vec<_>>();
            assert_eq!(requests.len(), 4);
            for request in requests {
                assert_eq!(request["authorization"], "Bearer git-test-token");
                assert_eq!(request["body"]["cwd"], "repos/local");
                assert_eq!(request["body"]["path"], "a.txt");
            }
            gitChange(&http.0, "pathError", false);
        } else {
            let requests = mounted.state.fake.git_path_requests.borrow();
            assert_eq!(requests.len(), 4);
            assert!(
                requests
                    .iter()
                    .all(|(project, request)| *project == Some(1) && request.path == "a.txt")
            );
        }
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
        drop(mounted);
    }
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn changes_rows_share_git_and_chat_menus_and_review_uses_known_status() {
    use super::project_git::{Http, gitChange, gitHttp, restore_token, token};
    let previous = token().await;
    let http = Http(gitHttp());
    gitChange(&http.0, "invalid", true);
    use openwebide_core::{GitRepoStatus, git::GitFileStatus};
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let (mounted, _) = fixture_with_changes(folder.as_ref().map(treeHandle), true);
        mounted
            .state
            .settings
            .bridge_url
            .set("ws://git.test:3001".into());
        mounted.state.git.status.set(Some(GitRepoStatus {
            availability: openwebide_core::git::GitStatusAvailability::Complete,
            branch: "main".into(),
            commit_hash: "abc".into(),
            commit_message: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            is_clean: false,
            line_stats: Default::default(),
            file_line_stats: [(
                "a.txt".into(),
                openwebide_core::GitLineStats {
                    insertions: 3,
                    deletions: 1,
                },
            )]
            .into(),
            files: [("a.txt".into(), GitFileStatus::Modified)].into(),
        }));
        settle().await;
        assert_eq!(
            mounted
                .element(".tree-root .tree-line-stats")
                .get_attribute("aria-label")
                .as_deref(),
            Some("Modified: 3 added lines, 1 removed lines")
        );
        assert!(
            mounted
                .element(".tree-root .tree-icon")
                .class_list()
                .contains("git-badge-modified")
        );
        treeContext(&mounted.element(".tree-root .tree-item"));
        settle().await;
        let button = |label: &str| {
            let nodes = mounted
                .root
                .query_selector_all(".ui-dropdown-menu button")
                .unwrap();
            (0..nodes.length())
                .filter_map(|i| nodes.item(i))
                .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
                .find(|node| node.text_content().as_deref() == Some(label))
                .unwrap()
        };
        assert!(
            !mounted
                .element(".ui-dropdown-menu")
                .text_content()
                .unwrap()
                .contains("New folder")
        );
        assert!(!button("Review changes in chat").has_attribute("disabled"));
        mounted.click(".ui-dropdown-backdrop");
        treeContext(&mounted.element(".git-files .tree-item"));
        settle().await;
        wait_until("Git lookup completed", || {
            mounted
                .root
                .query_selector(".ui-dropdown-menu [role=status]")
                .unwrap()
                .is_some()
        })
        .await;
        let menu = mounted.element(".ui-dropdown-menu").text_content().unwrap();
        assert!(
            !menu.contains("New folder") && !menu.contains("Rename") && !menu.contains("Delete")
        );
        assert!(menu.contains("Stage") && menu.contains("Revert changes"));
        choose(&mounted, "Review changes in chat");
        assert_eq!(
            mounted.state.chat.notice.get_untracked().as_deref(),
            Some("Review changes for bugs and regressions in @diff:\"a.txt\"")
        );
        assert!(mounted.state.chat.draft.get_untracked().is_empty());
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
    }
    restore_token(previous).await;
}

#[wasm_bindgen_test]
async fn shortcut_send_uses_shared_run_pipeline_and_preserves_draft_in_both_modes() {
    use openwebide_core::{
        ChatCompletion, ChatResponse, ConversationEntry, PromptImage, Role, StopReason,
    };
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let handle = folder.as_ref().map(treeHandle);
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let capture = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_connection();
            state.seed_session();
            state
                .fake
                .files
                .borrow_mut()
                .insert((1, "a.txt".into()), "Original".into());
            if let Some(handle) = handle {
                state
                    .projects
                    .projects
                    .update(|items| items[0].mode = openwebide_core::WorkspaceMode::Local);
                state.projects.local_handles.update(|items| {
                    items.insert(1, handle.unchecked_into());
                });
            }
            state
                .fake
                .scripted_completions
                .borrow_mut()
                .push_back(ChatCompletion {
                    reasoning: String::new(),
                    response: ChatResponse::Text("Explained".into()),
                    stop_reason: StopReason::Complete,
                    preamble: String::new(),
                    usage: None,
                });
            capture.set(Some(super::support::chat_actions(state).send_prompt));
            view! {<div/>}
        });
        settle().await;
        let image = PromptImage {
            name: "draft.png".into(),
            mime: "image/png".into(),
            data: "unused-draft-image".into(),
        };
        mounted.state.chat.draft.set("Unsent draft".into());
        mounted.state.chat.prompt_images.set(vec![image.clone()]);
        slot.get().unwrap().run("Explain @file:\"a.txt\"".into());
        wait_until("shortcut sent", || {
            !mounted.state.chat.streaming.get_untracked()
        })
        .await;
        assert!(
            mounted.state.chat.error.get_untracked().is_none(),
            "{:?}",
            mounted.state.chat.error.get_untracked()
        );
        assert_eq!(mounted.state.chat.draft.get_untracked(), "Unsent draft");
        assert_eq!(
            mounted.state.chat.prompt_images.get_untracked(),
            vec![image]
        );
        if local {
            assert!(mounted.state.fake.messages.borrow()[&1].iter().any(|entry| matches!(entry,ConversationEntry::Message(message) if message.role==Role::User && message.content.contains("Explain") && message.content.contains("Original") && !message.content.contains("Unsent draft"))));
        } else {
            assert!(mounted.state.fake.calls.borrow().iter().any(|call| matches!(call,openwebide_frontend::testing::fake_backend::Call::SendMessage {content,..} if content.contains("Explain") && content.contains("Original") && !content.contains("Unsent draft"))));
        }
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
    }
}

#[wasm_bindgen_test]
async fn preview_availability_and_pdf_blob_contract_work_in_both_modes() {
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let handle = folder.as_ref().map(treeHandle);
        let open = std::rc::Rc::new(std::cell::Cell::new(None));
        let capture = open.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            if let Some(handle) = handle {
                state
                    .projects
                    .projects
                    .update(|items| items[0].mode = openwebide_core::WorkspaceMode::Local);
                state.projects.local_handles.update(|items| {
                    items.insert(1, handle.unchecked_into());
                });
            }
            let actions = WorkspaceActions::new(
                state.api,
                state.projects,
                state.workspace,
                state.ui,
                RwSignal::new(false),
                Callback::new(|()| ()),
            );
            capture.set(Some(actions.request_open));
            view! { <style>{include_str!("../../styles.css")}</style><openwebide_frontend::components::Editor read_only=Signal::derive(|| false) on_open_lossy=actions.on_open_lossy on_save=actions.on_save on_accept=actions.on_accept on_reject=actions.on_reject /> }
        });
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        for (path, content) in [
            ("source.rs", "fn code() {}"),
            ("README.md", "# Read me"),
            ("LICENSE", "Copyright <example>\n\n  All rights reserved."),
            ("notes.txt", "Plain *text* <example>"),
            ("data.json", "{\"key\":1}"),
            ("Makefile", "build:\n\tcargo build"),
            ("report.pdf", "%PDF-1.4\n%%EOF"),
            ("archive.zip", "binary placeholder"),
        ] {
            files.write(path, content).await.unwrap();
            open.get().unwrap().run(path.into());
            settle().await;
            let preview = || {
                let buttons = mounted.root.query_selector_all(".ui-seg-btn").unwrap();
                (0..buttons.length())
                    .filter_map(|i| buttons.item(i))
                    .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
                    .find(|node| node.text_content().as_deref() == Some("Preview"))
            };
            assert_eq!(
                preview().is_some(),
                openwebide_core::FileKind::supports_preview(path),
                "{path}"
            );
            if path == "LICENSE" || path == "notes.txt" {
                wait_until("Document loaded", || {
                    mounted.state.workspace.content.get_untracked() == content
                })
                .await;
                preview().unwrap().click();
                settle().await;
                let document = mounted.element(".editor-document-preview");
                assert_eq!(document.text_content().as_deref(), Some(content));
                assert_eq!(
                    document.children().length(),
                    0,
                    "Document text must stay literal"
                );
            }
            if path.ends_with("pdf") {
                wait_until("PDF loaded", || {
                    mounted.state.workspace.media_url.get_untracked().is_some()
                })
                .await;
                let url = mounted.state.workspace.media_url.get_untracked().unwrap();
                let response = gloo_net::http::Request::get(&url).send().await.unwrap();
                assert_eq!(
                    response.headers().get("content-type").as_deref(),
                    Some("application/pdf")
                );
                assert_eq!(
                    mounted
                        .element(".editor-pdf-preview")
                        .get_attribute("src")
                        .as_deref(),
                    Some(url.as_str())
                );
            }
            if path.ends_with("zip") {
                assert!(
                    mounted
                        .root
                        .query_selector(".editor-placeholder-view")
                        .unwrap()
                        .is_some()
                );
            }
        }
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
    }
}

#[wasm_bindgen_test]
async fn switching_files_preserves_independent_edits_history_and_positions_in_both_modes() {
    use openwebide_core::editor::{Indentation, Selection};
    use openwebide_frontend::state_actions::editor::{EditorActions, EditorCommand};
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let handle = folder.as_ref().map(treeHandle);
        let capture = std::rc::Rc::new(std::cell::Cell::new(None));
        let slot = capture.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            if let Some(handle) = handle {
                state
                    .projects
                    .projects
                    .update(|items| items[0].mode = WorkspaceMode::Local);
                state.projects.local_handles.update(|items| {
                    items.insert(1, handle.unchecked_into());
                });
            }
            let actions = WorkspaceActions::new(
                state.api,
                state.projects,
                state.workspace,
                state.ui,
                RwSignal::new(false),
                Callback::new(|()| ()),
            );
            slot.set(Some((
                actions,
                EditorActions::new(state.workspace),
                expect_context::<FileTreeActions>(),
            )));
            view! { <div /> }
        });
        let (actions, editor, tree) = capture.get().unwrap();
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        files.write("a.txt", "Original").await.unwrap();
        files.write("b.txt", "Second").await.unwrap();
        actions.request_open.run("a.txt".into());
        wait_until("File read completed", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        settle().await;
        editor
            .native_input("Original!".into(), Selection::caret(9), "insertText", 1.0)
            .unwrap();
        editor.record_scroll(1, "a.txt", 90.0, 12.0);
        actions.request_open.run("b.txt".into());
        wait_until("File read completed", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        settle().await;
        assert!(mounted.state.ui.confirm.get_untracked().is_none());
        assert_eq!(mounted.state.workspace.content.get_untracked(), "Second");
        editor
            .native_input("Second?".into(), Selection::caret(7), "insertText", 2.0)
            .unwrap();
        actions.request_open.run("a.txt".into());
        wait_until("File read completed", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        settle().await;
        assert_eq!(mounted.state.workspace.content.get_untracked(), "Original!");
        assert!(mounted.state.workspace.dirty.get_untracked());
        assert_eq!(editor.selection("Original!"), Some(Selection::caret(9)));
        assert!((editor.scroll().top - 90.0).abs() < f64::EPSILON);
        editor
            .command(
                EditorCommand::Undo,
                Selection::caret(9),
                Indentation::default(),
            )
            .unwrap();
        assert_eq!(mounted.state.workspace.content.get_untracked(), "Original");
        assert!(!mounted.state.workspace.dirty.get_untracked());
        actions.request_open.run("b.txt".into());
        wait_until("File read completed", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        settle().await;
        assert_eq!(mounted.state.workspace.content.get_untracked(), "Second?");
        assert_eq!(files.read("a.txt").await.unwrap(), "Original");
        assert_eq!(files.read("b.txt").await.unwrap(), "Second");
        // Both filesystem adapters publish the same complete source-owned index;
        // normal and lossy reads share cooperative document preparation.
        let large = "文😀\r\n".repeat(12_000);
        files.write("prepared.txt", &large).await.unwrap();
        actions.request_open.run("prepared.txt".into());
        wait_until("Prepared file read completed", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        mounted
            .state
            .workspace
            .editor_documents
            .with_untracked(|documents| {
                let document = &documents[&(1, "prepared.txt".into())];
                assert_eq!(document.text(), large);
                assert_eq!(document.line_count(), 12_001);
                assert!(std::sync::Arc::ptr_eq(
                    &document.shared_text(),
                    &mounted.state.workspace.content.get_untracked().shared()
                ));
                assert!(!document.is_dirty());
            });
        actions.on_open_lossy.run(());
        wait_until("Prepared lossy read completed", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        assert_eq!(mounted.state.workspace.content.get_untracked(), large);
        mounted
            .state
            .workspace
            .editor_documents
            .with_untracked(|documents| {
                assert_eq!(documents[&(1, "prepared.txt".into())].text(), large);
            });
        // Input arriving while a read is pending must win, even for facade callers.
        actions.request_open.run("third.txt".into());
        mounted.state.workspace.content.set("new input".into());
        mounted.state.workspace.dirty.set(true);
        wait_until("File read completed", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        settle().await;
        assert_eq!(mounted.state.workspace.content.get_untracked(), "new input");
        assert!(!mounted.state.workspace.editor_loading.get_untracked());
        tree.move_entry(&file("b.txt"), true);
        settle().await;
        assert!(
            mounted
                .state
                .ui
                .toast
                .get_untracked()
                .unwrap()
                .contains("unsaved")
        );
        assert!(mounted.state.ui.prompt.get_untracked().is_none());
        assert_eq!(files.read("b.txt").await.unwrap(), "Second");
        if local {
            mounted.state.projects.local_handles.update(|handles| {
                handles.remove(&1);
            });
        } else {
            mounted
                .state
                .projects
                .projects
                .update(|items| items[0].path = Some("/different-root".into()));
        }
        actions.request_open.run("a.txt".into());
        settle().await;
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("third.txt")
        );
        assert_eq!(mounted.state.workspace.content.get_untracked(), "new input");
        assert!(
            mounted
                .state
                .ui
                .toast
                .get_untracked()
                .unwrap()
                .contains("folder changed")
        );
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
    }
}

#[wasm_bindgen_test]
async fn editor_file_tabs_close_discard_and_keyboard_navigation_share_both_modes() {
    use openwebide_core::editor::Selection;
    use openwebide_frontend::state_actions::editor::EditorActions;
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let handle = folder.as_ref().map(treeHandle);
        let capture = std::rc::Rc::new(std::cell::Cell::new(None));
        let slot = capture.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            if let Some(handle) = handle {
                state
                    .projects
                    .projects
                    .update(|items| items[0].mode = WorkspaceMode::Local);
                state.projects.local_handles.update(|items| {
                    items.insert(1, handle.unchecked_into());
                });
            }
            let read_only = RwSignal::new(false);
            let actions = WorkspaceActions::new(
                state.api,
                state.projects,
                state.workspace,
                state.ui,
                read_only,
                Callback::new(|()| ()),
            );
            slot.set(Some((actions, EditorActions::new(state.workspace))));
            view! { <style>{include_str!("../../styles.css")}</style><openwebide_frontend::components::Editor read_only=read_only.into() on_open_lossy=actions.on_open_lossy on_save=actions.on_save on_accept=actions.on_accept on_reject=actions.on_reject /><ConfirmDialog /> }
        });
        let (actions, editor) = capture.get().unwrap();
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        for (path, content) in [("a.txt", "One"), ("b.txt", "Two")] {
            files.write(path, content).await.unwrap();
            actions.request_open.run(path.into());
            wait_until("Read completed", || {
                !mounted.state.workspace.editor_loading.get_untracked()
            })
            .await;
            settle().await;
        }
        assert_eq!(
            mounted
                .root
                .query_selector_all("[role=tab]")
                .unwrap()
                .length(),
            2
        );
        editor
            .native_input("Two!".into(), Selection::caret(4), "insertText", 1.0)
            .unwrap();
        mounted.click("[data-editor-tab='a.txt']");
        wait_until("Selected first file", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        settle().await;
        assert_eq!(
            mounted
                .element("[data-editor-tab='a.txt']")
                .get_attribute("aria-selected")
                .as_deref(),
            Some("true")
        );
        assert!(
            mounted
                .root
                .query_selector(".editor-tab-dirty.is-dirty")
                .unwrap()
                .is_some()
        );
        mounted.click("[aria-label='Close b.txt']");
        settle().await;
        assert!(mounted.state.ui.confirm.get_untracked().is_some());
        mounted.click_text("Cancel");
        settle().await;
        assert_eq!(
            mounted
                .root
                .query_selector_all("[role=tab]")
                .unwrap()
                .length(),
            2
        );
        mounted.click("[aria-label='Close b.txt']");
        settle().await;
        mounted.click_text("Discard and close");
        settle().await;
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("a.txt")
        );
        assert_eq!(
            mounted
                .root
                .query_selector_all("[role=tab]")
                .unwrap()
                .length(),
            1
        );
        assert!(
            !mounted
                .state
                .workspace
                .editor_buffers
                .get_untracked()
                .contains_key(&(1, "b.txt".into()))
        );
        assert_eq!(files.read("b.txt").await.unwrap(), "Two");
        actions.request_open.run("b.txt".into());
        wait_until("Reopened discarded file", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        settle().await;
        assert_eq!(mounted.state.workspace.content.get_untracked(), "Two");
        let init = web_sys::KeyboardEventInit::new();
        init.set_key("ArrowLeft");
        init.set_bubbles(true);
        init.set_cancelable(true);
        mounted
            .element("[data-editor-tab='b.txt']")
            .dispatch_event(
                &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
                    .unwrap(),
            )
            .unwrap();
        wait_until("Keyboard selected adjacent file", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        settle().await;
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("a.txt")
        );
        assert_eq!(
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .active_element()
                .unwrap()
                .get_attribute("data-editor-tab")
                .as_deref(),
            Some("a.txt")
        );
        // A confirmation cannot discard text entered after it was opened.
        editor
            .native_input("One!".into(), Selection::caret(4), "insertText", 2.0)
            .unwrap();
        actions.close_file.run("a.txt".into());
        settle().await;
        editor
            .native_input("One!!".into(), Selection::caret(5), "insertText", 3.0)
            .unwrap();
        mounted.click_text("Discard and close");
        settle().await;
        assert_eq!(mounted.state.workspace.content.get_untracked(), "One!!");
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("a.txt")
        );
        actions.close_file.run("a.txt".into());
        settle().await;
        let old_account_close = mounted.state.ui.confirm.get_untracked().unwrap().action;
        mounted
            .state
            .auth
            .generation
            .update(|generation| *generation += 1);
        old_account_close.run(());
        assert_eq!(mounted.state.workspace.content.get_untracked(), "One!!");
        assert!(
            mounted.state.workspace.editor_tabs.get_untracked()[&1].contains(&"a.txt".to_string())
        );
        mounted.state.ui.clear_confirm();
        actions.close_file.run("a.txt".into());
        settle().await;
        mounted.click_text("Discard and close");
        wait_until("Closed active file selected neighbor", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        settle().await;
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("b.txt")
        );
        actions.close_file.run("b.txt".into());
        settle().await;
        assert!(mounted.state.workspace.open_file.get_untracked().is_none());
        assert!(mounted.state.workspace.editor_tabs.get_untracked()[&1].is_empty());
        assert!(
            mounted
                .root
                .query_selector(".editor-file-tabs")
                .unwrap()
                .is_none()
        );
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
    }
}

#[wasm_bindgen_test]
async fn expand_and_collapse_discover_nested_folders_in_both_modes_and_cancel_late_results() {
    use openwebide_core::vfs::VfsEntryKind;
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let (mounted, slot) = fixture(folder.as_ref().map(treeHandle));
        settle().await;
        let actions = slot.get().unwrap();
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        files.create("src", VfsEntryKind::Directory).await.unwrap();
        files
            .create("src/deep", VfsEntryKind::Directory)
            .await
            .unwrap();
        files
            .create(".hidden", VfsEntryKind::Directory)
            .await
            .unwrap();
        files.write("src/deep/a.txt", "Nested").await.unwrap();
        actions.expand_all();
        wait_until("all folders expanded", || {
            !actions.expanding.get_untracked()
        })
        .await;
        assert!(
            mounted
                .state
                .workspace
                .expanded
                .with_untracked(|dirs| dirs.contains("src")
                    && dirs.contains("src/deep")
                    && !dirs.contains(".hidden"))
        );
        assert!(
            mounted
                .root
                .query_selector("[data-tree-path='src/deep/a.txt']")
                .unwrap()
                .is_some()
        );
        actions.collapse_all();
        settle().await;
        assert!(mounted.state.workspace.expanded.get_untracked().is_empty());
        assert!(
            mounted
                .root
                .query_selector("[data-tree-path='src/deep/a.txt']")
                .unwrap()
                .is_none()
        );
        if !local {
            let (send, receive) = futures::channel::oneshot::channel();
            mounted
                .state
                .fake
                .file_list_results
                .borrow_mut()
                .push_back(receive);
            actions.expand_all();
            settle().await;
            send.send(Err("folder unavailable".into())).unwrap();
            wait_until("expansion failure", || !actions.expanding.get_untracked()).await;
            assert!(
                mounted
                    .state
                    .ui
                    .toast
                    .get_untracked()
                    .unwrap()
                    .contains("Could not expand all folders")
            );
            mounted.state.ui.toast.set(None);
            for switch_project in [false, true] {
                let (send, receive) = futures::channel::oneshot::channel();
                mounted
                    .state
                    .fake
                    .file_list_results
                    .borrow_mut()
                    .push_back(receive);
                actions.expand_all();
                settle().await;
                if switch_project {
                    mounted.state.projects.active_project.set(None);
                } else {
                    actions.collapse_all();
                }
                settle().await;
                let _ = send.send(Ok(vec![FileEntry {
                    name: "late".into(),
                    path: "late".into(),
                    is_dir: true,
                    size: 0,
                }]));
                settle().await;
                assert!(
                    !mounted
                        .state
                        .workspace
                        .expanded
                        .with_untracked(|dirs| dirs.contains("late"))
                );
                assert!(!mounted.state.workspace.entries.with_untracked(|items| {
                    items
                        .get("")
                        .is_some_and(|entries| entries.iter().any(|entry| entry.path == "late"))
                }));
                assert!(!actions.expanding.get_untracked());
            }
        }
        drop(mounted);
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
    }
}

#[wasm_bindgen_test]
async fn cached_file_refresh_retains_identical_text_and_applies_external_changes_in_both_modes() {
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let (mounted, _) = fixture(folder.as_ref().map(treeHandle));
        settle().await;
        mounted.click("[data-tree-path='a.txt']");
        wait_until("initial file load", || {
            !mounted.state.workspace.editor_loading.get_untracked()
                && mounted.state.workspace.content.get_untracked().as_str() == "Original"
        })
        .await;
        let original = mounted.state.workspace.content.get_untracked().shared();
        mounted.click("[data-tree-path='a.txt']");
        wait_until("cached file refresh", || {
            !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        assert!(std::sync::Arc::ptr_eq(
            &original,
            &mounted.state.workspace.content.get_untracked().shared()
        ));
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        files.write("a.txt", "External change").await.unwrap();
        mounted.click("[data-tree-path='a.txt']");
        wait_until("external file change", || {
            !mounted.state.workspace.editor_loading.get_untracked()
                && mounted.state.workspace.content.get_untracked().as_str() == "External change"
        })
        .await;
        assert!(!std::sync::Arc::ptr_eq(
            &original,
            &mounted.state.workspace.content.get_untracked().shared()
        ));
        drop(mounted);
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
    }
}

#[wasm_bindgen_test]
async fn nested_file_groups_menus_reveal_and_refresh_work_in_both_modes() {
    use openwebide_core::vfs::VfsEntryKind;
    for local in [false, true] {
        let folder = if local {
            Some(treeFolder().await.unwrap())
        } else {
            None
        };
        let (mounted, slot) = fixture(folder.as_ref().map(treeHandle));
        settle().await;
        let actions = slot.get().unwrap();
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        for path in [
            "docker-compose.yml",
            "docker-compose.ssh.yml",
            "docker-compose.ssh.dev.yml",
            "orphan.ssh.yml",
        ] {
            files.write(path, "Contents").await.unwrap();
        }
        files
            .create("configs", VfsEntryKind::Directory)
            .await
            .unwrap();
        let refresh = || async {
            let entries = files.list("").await.unwrap();
            mounted.state.workspace.entries.update(|map| {
                map.insert("".into(), entries);
            });
            settle().await;
        };
        refresh().await;
        assert!(
            mounted
                .root
                .query_selector("[data-tree-path='docker-compose.ssh.yml']")
                .unwrap()
                .is_none()
        );
        assert!(
            mounted
                .root
                .query_selector("[data-tree-path='orphan.ssh.yml']")
                .unwrap()
                .is_some()
        );
        let base = mounted.element("[data-tree-path='docker-compose.yml']");
        treeContext(&base);
        settle().await;
        let text = mounted.element(".ui-dropdown-menu").text_content().unwrap();
        assert!(
            !text.contains("New file")
                && !text.contains("New folder")
                && !text.contains("Reveal in Files")
        );
        mounted.click(".ui-dropdown-backdrop");
        settle().await;
        treeContext(&mounted.element("[data-tree-path='configs']"));
        settle().await;
        let text = mounted.element(".ui-dropdown-menu").text_content().unwrap();
        assert!(text.contains("New file") && text.contains("New folder"));
        mounted.click(".ui-dropdown-backdrop");
        settle().await;
        mounted.click("[aria-label='Expand docker-compose.yml']");
        settle().await;
        let variant = mounted.element("[data-tree-path='docker-compose.ssh.yml']");
        assert_eq!(variant.get_attribute("aria-level").as_deref(), Some("2"));
        assert!(mounted.state.workspace.open_file.get_untracked().is_none());
        let key = web_sys::KeyboardEventInit::new();
        key.set_key("ArrowLeft");
        key.set_bubbles(true);
        variant
            .dispatch_event(
                &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &key)
                    .unwrap(),
            )
            .unwrap();
        assert!(
            base.is_same_node(
                mounted
                    .root
                    .owner_document()
                    .unwrap()
                    .active_element()
                    .as_ref()
                    .map(AsRef::as_ref)
            )
        );
        variant.click();
        wait_until("nested file opened", || {
            mounted.state.workspace.open_file.get_untracked().as_deref()
                == Some("docker-compose.ssh.yml")
                && !mounted.state.workspace.editor_loading.get_untracked()
        })
        .await;
        actions.collapse_all();
        settle().await;
        actions.reveal("docker-compose.ssh.dev.yml");
        wait_until("nested reveal focused", || {
            actions.reveal_target.get_untracked().is_none()
        })
        .await;
        assert_eq!(
            mounted
                .element("[data-tree-path='docker-compose.ssh.dev.yml']")
                .get_attribute("aria-level")
                .as_deref(),
            Some("3")
        );
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("docker-compose.ssh.yml")
        );
        actions.collapse_all();
        settle().await;
        actions.expand_all();
        wait_until("groups expanded", || !actions.expanding.get_untracked()).await;
        assert!(
            mounted
                .root
                .query_selector("[data-tree-path='docker-compose.ssh.dev.yml']")
                .unwrap()
                .is_some()
        );
        let retained_variant = mounted.element("[data-tree-path='docker-compose.ssh.yml']");
        // Removing the base must restore its variants to ordinary top-level rows.
        files.delete("docker-compose.yml").await.unwrap();
        refresh().await;
        assert!(retained_variant.is_same_node(Some(
            &mounted.element("[data-tree-path='docker-compose.ssh.yml']")
        )));
        assert_eq!(
            mounted
                .element("[data-tree-path='docker-compose.ssh.yml']")
                .get_attribute("aria-level")
                .as_deref(),
            Some("1")
        );
        if let Some(folder) = folder {
            treeCleanup(&folder).await.unwrap();
        }
    }
}

#[wasm_bindgen_test]
async fn close_menu_and_middle_click_keep_file_dirty_guards_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            for path in ["a.txt", "b.txt"] {
                state.workspace.register_editor_tab(1, path.into());
                state.workspace.open_file.set(Some(path.into()));
                state.workspace.content.set(path.into());
                state.workspace.retain_editor_buffer(false);
            }
            state.workspace.open_file.set(Some("a.txt".into()));
            state.workspace.content.set("Unsaved".into());
            state.workspace.dirty.set(true);
            view! { {super::support::editor_view(state)} <ConfirmDialog /> }
        });
        settle().await;
        let tab = mounted.element("[data-editor-tab='a.txt']");
        treeMiddleClick(&tab, 2);
        settle().await;
        assert!(mounted.state.ui.confirm.get_untracked().is_none());
        treeMiddleClick(&tab, 1);
        settle().await;
        assert!(mounted.state.ui.confirm.get_untracked().is_some());
        assert!(
            mounted.state.workspace.editor_tabs.get_untracked()[&1].contains(&"a.txt".to_string())
        );
        mounted.state.ui.clear_confirm();
        treeContext(&mounted.element("[data-editor-tab='b.txt']"));
        settle().await;
        choose(&mounted, "Close");
        settle().await;
        assert_eq!(
            mounted.state.workspace.editor_tabs.get_untracked()[&1],
            ["a.txt"]
        );
        assert_eq!(
            mounted.state.workspace.open_file.get_untracked().as_deref(),
            Some("a.txt")
        );
        treeContext(&tab);
        settle().await;
        choose(&mounted, "Close");
        settle().await;
        assert!(mounted.state.ui.confirm.get_untracked().is_some());
        mounted.click_text("Discard and close");
        settle().await;
        assert!(mounted.state.workspace.editor_tabs.get_untracked()[&1].is_empty());
    }
}

#[wasm_bindgen_test]
async fn git_tree_colors_counts_and_phone_disclosures_stay_visible_in_both_modes() {
    use openwebide_core::{GitLineStats, GitRepoStatus, git::GitFileStatus};
    use openwebide_frontend::state::layout::LayoutState;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let capture = slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.projects.projects.update(|items| items[0].mode = mode);
            state.workspace.entries.update(|entries| {
                entries.insert(
                    String::new(),
                    vec![
                        file("a.txt"),
                        FileEntry {
                            path: "src".into(),
                            name: "src".into(),
                            is_dir: true,
                            size: 0,
                        },
                    ],
                );
            });
            state.git.status.set(Some(GitRepoStatus {
                availability: openwebide_core::git::GitStatusAvailability::Complete,
                files: [
                    ("a.txt".into(), GitFileStatus::Modified),
                    ("src/b.txt".into(), GitFileStatus::Modified),
                ]
                .into(),
                file_line_stats: [
                    (
                        "a.txt".into(),
                        GitLineStats {
                            insertions: 3,
                            deletions: 1,
                        },
                    ),
                    (
                        "src/b.txt".into(),
                        GitLineStats {
                            insertions: 2,
                            deletions: 1,
                        },
                    ),
                ]
                .into(),
                ..Default::default()
            }));
            let actions = WorkspaceActions::new(
                state.api,
                state.projects,
                state.workspace,
                state.ui,
                RwSignal::new(false),
                Callback::new(|()| ()),
            );
            let layout = expect_context::<LayoutState>();
            capture.set(Some(layout));
            view! { <style>{include_str!("../../styles.css")}</style><div class="app" class:phone-layout=move || layout.phone.get() style="width:360px;height:600px"><span class="git-color-reference" style="color:var(--git-modified)"></span><FileTree on_toggle=actions.on_toggle on_open=actions.request_open /></div> }
        });
        settle().await;
        for viewport in [1000.0, 390.0] {
            slot.get().unwrap().viewport_width.set(viewport);
            settle().await;
            let folder = mounted.element("[data-tree-path='src']");
            let button = folder
                .query_selector(".tree-disclosure button")
                .unwrap()
                .unwrap();
            let icon = folder.query_selector(".tree-icon").unwrap().unwrap();
            assert!(
                button.get_bounding_client_rect().right() <= icon.get_bounding_client_rect().left(),
                "Disclosure touch target overlaps folder icon"
            );
            if viewport < 500.0 {
                assert!(button.get_bounding_client_rect().width() >= 44.0);
            }
            let expected = window()
                .get_computed_style(&mounted.element(".git-color-reference"))
                .unwrap()
                .unwrap()
                .get_property_value("color")
                .unwrap();
            for path in ["a.txt", "src"] {
                let icon = mounted.element(&format!("[data-tree-path='{path}'] .tree-icon"));
                assert_eq!(
                    window()
                        .get_computed_style(&icon)
                        .unwrap()
                        .unwrap()
                        .get_property_value("color")
                        .unwrap(),
                    expected
                );
                let counts = mounted
                    .element(&format!("[data-tree-path='{path}'] .tree-line-stats"))
                    .get_bounding_client_rect();
                let menu = mounted
                    .element(&format!("[data-tree-path='{path}'] .tree-entry-menu"))
                    .get_bounding_client_rect();
                assert!(
                    (menu.left() - counts.right() - 6.0).abs() < 0.1,
                    "Git counts must sit immediately before the menu button"
                );
            }
            assert_eq!(
                mounted
                    .element("[data-tree-path='a.txt'] .tree-line-stats")
                    .get_attribute("aria-label")
                    .as_deref(),
                Some("Modified: 3 added lines, 1 removed lines")
            );
            assert_eq!(
                mounted
                    .element("[data-tree-path='src'] .tree-line-stats")
                    .get_attribute("aria-label")
                    .as_deref(),
                Some("Contains changed files: 2 added lines, 1 removed lines")
            );
        }
    }
}
