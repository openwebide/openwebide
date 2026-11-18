use super::{
    local_bridge::probe_folder,
    project_git::{Http, gitCalls, gitHistoryFixture, gitHttp, restore_token, token},
    support::{mount_test, settle, wait_until},
};
use leptos::prelude::*;
use openwebide_core::{WorkspaceMode, git::*};
use openwebide_frontend::{
    components::git_history::GitHistory, state_actions::git_history::GitHistoryActions,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;

fn fixture() -> GitHistoryPage {
    GitHistoryPage {
        commits: vec![GitHistoryCommit {
            history_path: None,
            hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            parents: vec![
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                "cccccccccccccccccccccccccccccccccccccccc".into(),
            ],
            author: "History Author".into(),
            author_email: "author@example.com".into(),
            authored_at: "2026-10-08T10:00:00Z".into(),
            committer: "Committer".into(),
            committer_email: "committer@example.com".into(),
            committed_at: "2026-10-08T11:00:00Z".into(),
            subject: "Merge history".into(),
            message: "Merge history\n\nComplete commit message".into(),
            refs: vec!["branch: main".into()],
        }],
        refs: vec![GitHistoryRef {
            name: "main".into(),
            hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            kind: "branch".into(),
        }],
        has_more: false,
    }
}
fn changes() -> GitCommitDiff {
    GitCommitDiff {
        truncated: false,
        files: vec![GitCommitFile {
            path: "src/main.rs".into(),
            status: "M".into(),
            previous_path: None,
        }],
        diff: "@@ -1 +1 @@\n-old\n+new\n".into(),
    }
}

#[wasm_bindgen_test]
async fn history_inline_tree_readonly_diff_and_branch_dropdown_in_both_modes() {
    let old = token().await;
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let http = Http(gitHttp());
        gitHistoryFixture(
            &http.0,
            &serde_json::to_string(&fixture()).unwrap(),
            &serde_json::to_string(&changes()).unwrap(),
        );
        let handle = probe_folder();
        let opened = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let open_result = opened.clone();
        let checkout = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let checkout_result = checkout.clone();
        let mounted = mount_test(move |state| {
            let layout = expect_context::<openwebide_frontend::state::layout::LayoutState>();
            provide_context(
                openwebide_frontend::state_actions::layout::LayoutActions::new(
                    state.api, layout, state.auth, state.ui,
                ),
            );
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            state.settings.bridge_url.set("ws://git.test:3001".into());
            *state.fake.git_history.borrow_mut() = fixture();
            *state.fake.git_commit_diff.borrow_mut() = Some(changes());
            view! {<style>{include_str!("../../styles.css")}</style><div style="height:700px;width:360px;display:flex"><GitHistory on_open=Callback::new(move |path|*opened.lock().unwrap()=Some(path)) on_select_branch=Callback::new(move |branch|*checkout.lock().unwrap()=Some(branch)) on_new_branch=Callback::new(|()|()) /></div>}
        });
        wait_until("history rows", || {
            mounted
                .root
                .query_selector(".git-history-row")
                .unwrap()
                .is_some()
        })
        .await;
        mounted.click(".git-history-row");
        wait_until("commit changes", || {
            mounted
                .root
                .query_selector(".git-commit-file")
                .unwrap()
                .is_some()
        })
        .await;
        assert!(
            mounted
                .element(".git-commit-details")
                .text_content()
                .unwrap()
                .contains("Complete commit message")
        );
        assert!(
            mounted
                .element(".git-commit-details")
                .text_content()
                .unwrap()
                .contains("committer@example.com")
        );
        mounted.click("[aria-label=\"Compare with parent\"]");
        settle().await;
        mounted.click("[data-value=\"cccccccccccccccccccccccccccccccccccccccc\"]");
        wait_until("parent diff", || {
            if mode == WorkspaceMode::Remote {
                mounted.state.fake.git_commit_diff_requests.borrow().len() >= 2
            } else {
                serde_json::from_str::<Vec<serde_json::Value>>(&gitCalls(&http.0))
                    .unwrap()
                    .iter()
                    .filter(|call| call["path"] == "/git/commit-diff")
                    .count()
                    >= 2
            }
        })
        .await;
        settle().await;
        wait_until("parent files rendered", || {
            mounted
                .root
                .query_selector(".git-commit-file .btn:first-child")
                .unwrap()
                .is_some()
        })
        .await;
        mounted.click(".git-commit-file .btn:first-child");
        wait_until("file diff request", || {
            if mode == WorkspaceMode::Remote {
                mounted.state.fake.git_commit_diff_requests.borrow().len() >= 3
            } else {
                serde_json::from_str::<Vec<serde_json::Value>>(&gitCalls(&http.0))
                    .unwrap()
                    .iter()
                    .filter(|call| call["path"] == "/git/commit-diff")
                    .count()
                    >= 3
            }
        })
        .await;
        wait_until("file diff rendered", || {
            mounted
                .root
                .query_selector(".git-commit-file")
                .unwrap()
                .is_some()
        })
        .await;
        let requests = if mode == WorkspaceMode::Remote {
            mounted.state.fake.git_commit_diff_requests.borrow().clone()
        } else {
            serde_json::from_str::<Vec<serde_json::Value>>(&gitCalls(&http.0))
                .unwrap()
                .into_iter()
                .filter(|call| call["path"] == "/git/commit-diff")
                .map(|call| serde_json::from_value(call["body"].clone()).unwrap())
                .collect()
        };
        assert_eq!(
            requests.last().unwrap().parent.as_deref(),
            Some("cccccccccccccccccccccccccccccccccccccccc")
        );
        assert_eq!(
            requests.last().unwrap().path.as_deref(),
            Some("src/main.rs")
        );
        assert!(
            mounted
                .root
                .query_selector("[role=dialog]")
                .unwrap()
                .is_none()
        );
        assert!(
            mounted
                .root
                .query_selector(".git-readonly-diff textarea")
                .unwrap()
                .is_none()
        );
        assert!(
            mounted
                .element(".git-readonly-diff")
                .text_content()
                .unwrap()
                .contains("new")
        );
        assert!(
            mounted
                .root
                .query_selector(".git-history-folder")
                .unwrap()
                .is_some()
        );
        mounted.click("[aria-label=\"Open current file in editor\"]");
        settle().await;
        assert_eq!(open_result.lock().unwrap().as_deref(), Some("src/main.rs"));
        assert!(
            mounted
                .root
                .query_selector("[role=dialog]")
                .unwrap()
                .is_none()
        );
        mounted.click("[aria-label=\"History branch\"]");
        settle().await;
        mounted.click("[data-value=\"refs/heads/main\"]");
        settle().await;
        mounted.click("[aria-label=\"History actions\"]");
        settle().await;
        mounted.click(".git-history-toolbar [role=menuitem]");
        assert_eq!(checkout_result.lock().unwrap().as_deref(), Some("main"));
    }
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn history_and_diff_results_ignore_replaced_projects_and_accounts() {
    let (send, receive) = futures::channel::oneshot::channel();
    let slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let actions_slot = slot.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state
            .fake
            .git_history_results
            .borrow_mut()
            .push_back(receive);
        let actions = GitHistoryActions::new();
        actions_slot.set(Some(actions));
        view! {<div/>}
    });
    wait_until("pending history", || {
        !mounted.state.fake.git_history_requests.borrow().is_empty()
    })
    .await;
    mounted.state.projects.active_project.set(None);
    settle().await;
    send.send(Ok(fixture())).unwrap();
    settle().await;
    let actions = slot.get().unwrap();
    assert!(actions.commits.get_untracked().is_empty());
    mounted.state.projects.active_project.set(Some(1));
    settle().await;
    let (send, receive) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .git_commit_diff_results
        .borrow_mut()
        .push_back(receive);
    actions
        .select
        .run((fixture().commits.remove(0), None, None));
    wait_until("pending diff", || {
        !mounted
            .state
            .fake
            .git_commit_diff_requests
            .borrow()
            .is_empty()
    })
    .await;
    mounted.state.auth.logout();
    settle().await;
    send.send(Ok(changes())).unwrap();
    settle().await;
    assert!(actions.diff.get_untracked().is_none());
    assert!(actions.selected.get_untracked().is_none());
}

#[wasm_bindgen_test]
async fn history_paging_and_query_failures_have_the_same_recovery_in_both_modes() {
    use super::project_git::gitChange;
    let old = token().await;
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let http = Http(gitHttp());
        let mut first = fixture();
        first.has_more = true;
        gitHistoryFixture(
            &http.0,
            &serde_json::to_string(&first).unwrap(),
            &serde_json::to_string(&changes()).unwrap(),
        );
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let actions_slot = slot.clone();
        let handle = probe_folder();
        let mounted = mount_test(move |state| {
            let layout = expect_context::<openwebide_frontend::state::layout::LayoutState>();
            provide_context(
                openwebide_frontend::state_actions::layout::LayoutActions::new(
                    state.api, layout, state.auth, state.ui,
                ),
            );
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            state.settings.bridge_url.set("ws://git.test:3001".into());
            *state.fake.git_history.borrow_mut() = first;
            *state.fake.git_commit_diff.borrow_mut() = Some(changes());
            let actions = GitHistoryActions::new();
            actions_slot.set(Some(actions));
            view! {<div/>}
        });
        let actions = slot.get().unwrap();
        wait_until("first history page", || {
            actions.commits.with_untracked(Vec::len) == 1 && !actions.loading.get_untracked()
        })
        .await;
        let mut older = fixture();
        older.commits[0].hash = "dddddddddddddddddddddddddddddddddddddddd".into();
        *mounted.state.fake.git_history.borrow_mut() = older.clone();
        gitHistoryFixture(
            &http.0,
            &serde_json::to_string(&older).unwrap(),
            &serde_json::to_string(&changes()).unwrap(),
        );
        actions.search.set("draft".into());
        settle().await;
        actions.search.set("final query".into());
        actions.load.run(true);
        wait_until("debounced search", || {
            actions.commits.with_untracked(Vec::len) == 1
                && !actions.loading.get_untracked()
                && if mode == WorkspaceMode::Remote {
                    mounted
                        .state
                        .fake
                        .git_history_requests
                        .borrow()
                        .last()
                        .is_some_and(|request| request.search == "final query")
                } else {
                    serde_json::from_str::<Vec<serde_json::Value>>(&gitCalls(&http.0))
                        .unwrap()
                        .iter()
                        .any(|call| {
                            call["path"] == "/git/history"
                                && call["body"]["search"] == "final query"
                        })
                }
        })
        .await;
        let requests = if mode == WorkspaceMode::Remote {
            mounted.state.fake.git_history_requests.borrow().clone()
        } else {
            serde_json::from_str::<Vec<serde_json::Value>>(&gitCalls(&http.0))
                .unwrap()
                .into_iter()
                .filter(|call| call["path"] == "/git/history")
                .map(|call| serde_json::from_value(call["body"].clone()).unwrap())
                .collect()
        };
        assert_eq!(requests.last().unwrap().offset, 0);
        assert_eq!(requests.last().unwrap().search, "final query");
        assert!(requests.iter().all(|request| request.search != "draft"));
        assert!(!actions.has_more.get_untracked());
        let (send, receive) = futures::channel::oneshot::channel();
        if mode == WorkspaceMode::Remote {
            mounted
                .state
                .fake
                .git_history_results
                .borrow_mut()
                .push_back(receive);
            send.send(Err("history unavailable".into())).unwrap();
        }
        gitChange(&http.0, "historyError", true);
        actions.load.run(false);
        wait_until("history error", || actions.error.get_untracked().is_some()).await;
        assert!(!actions.loading.get_untracked());
        assert_eq!(actions.commits.with_untracked(Vec::len), 1);
        gitChange(&http.0, "historyError", false);
        actions.search.set(String::new());
        actions.load.run(false);
        wait_until("history retry", || {
            actions.error.get_untracked().is_none() && actions.commits.with_untracked(Vec::len) == 1
        })
        .await;
        let (send, receive) = futures::channel::oneshot::channel();
        if mode == WorkspaceMode::Remote {
            mounted
                .state
                .fake
                .git_commit_diff_results
                .borrow_mut()
                .push_back(receive);
            send.send(Err("diff unavailable".into())).unwrap();
        }
        gitChange(&http.0, "diffError", true);
        actions
            .select
            .run((fixture().commits.remove(0), None, None));
        wait_until("diff error", || {
            actions.diff_error.get_untracked().is_some()
        })
        .await;
        assert!(!actions.diff_loading.get_untracked());
        actions.load.run(false);
        assert!(actions.diff_error.get_untracked().is_none());
        assert!(!actions.diff_loading.get_untracked());
        wait_until("history after diff error", || {
            !actions.loading.get_untracked()
        })
        .await;
        gitChange(&http.0, "diffError", false);
        actions
            .select
            .run((fixture().commits.remove(0), None, None));
        wait_until("diff retry", || actions.diff.get_untracked().is_some()).await;
        assert!(actions.diff_error.get_untracked().is_none());
    }
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn git_actions_refresh_history_and_publish_failures_in_both_modes() {
    use super::project_git::gitChange;
    use openwebide_core::{GitCommitRequest, GitCommitResult, GitSyncResult};
    use openwebide_frontend::{
        project_git::ProjectGit,
        state_actions::git::{GitActionContext, GitActions},
    };
    let old = token().await;
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let http = Http(gitHttp());
        let handle = probe_folder();
        let slot = std::rc::Rc::new(std::cell::Cell::new(None));
        let actions_slot = slot.clone();
        let mounted = mount_test(move |state| {
            let layout = expect_context::<openwebide_frontend::state::layout::LayoutState>();
            provide_context(
                openwebide_frontend::state_actions::layout::LayoutActions::new(
                    state.api, layout, state.auth, state.ui,
                ),
            );
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            state.settings.bridge_url.set("ws://git.test:3001".into());
            let actions = GitActions::new(GitActionContext {
                project_git: expect_context::<ProjectGit>(),
                projects: state.projects,
                workspace: state.workspace,
                git: state.git,
                chat: state.chat,
                ui: state.ui,
                workspace_for: Callback::new(|_| None),
                refresh: Callback::new(|()| ()),
            });
            actions_slot.set(Some(actions));
            view! {<div/>}
        });
        settle().await;
        let actions = slot.get().unwrap();
        let revision = mounted.state.git.history_revision.get_untracked();
        let (send, receive) = futures::channel::oneshot::channel();
        if mode == WorkspaceMode::Remote {
            mounted
                .state
                .fake
                .git_commit_results
                .borrow_mut()
                .push_back(receive);
            send.send(Ok(GitCommitResult {
                commit_hash: "abc".into(),
                summary: "Save changes".into(),
                pre_commit_output: None,
                is_signed: false,
            }))
            .unwrap();
        }
        mounted.state.git.commit_message.set("Save changes".into());
        actions.on_commit.run(GitCommitRequest {
            message: "Save changes".into(),
            paths: None,
            include_untracked: false,
            staged_only: false,
        });
        wait_until("commit refresh", || {
            mounted.state.git.history_revision.get_untracked() > revision
        })
        .await;
        assert!(!mounted.state.git.commit_busy.get_untracked());
        assert!(mounted.state.git.commit_message.get_untracked().is_empty());
        for action in ["sync", "fetch", "pull", "push"] {
            let revision = mounted.state.git.history_revision.get_untracked();
            let (send, receive) = futures::channel::oneshot::channel();
            if mode == WorkspaceMode::Remote {
                mounted
                    .state
                    .fake
                    .git_sync_results
                    .borrow_mut()
                    .push_back(receive);
                send.send(Ok(GitSyncResult {
                    remote: "origin".into(),
                    branch: "main".into(),
                    pulled_commits: 0,
                    pushed_commits: 0,
                    output: String::new(),
                }))
                .unwrap();
            }
            actions.on_sync.run(action.into());
            wait_until("sync refresh", || {
                mounted.state.git.history_revision.get_untracked() > revision
            })
            .await;
            assert!(mounted.state.git.sync_busy.get_untracked().is_none());
            assert!(mounted.state.git.sync_notice.get_untracked().is_some());
        }
        let (send, receive) = futures::channel::oneshot::channel();
        if mode == WorkspaceMode::Remote {
            mounted
                .state
                .fake
                .git_sync_results
                .borrow_mut()
                .push_back(receive);
            send.send(Err("sync failed".into())).unwrap();
        }
        gitChange(&http.0, "syncError", true);
        let failed_revision = mounted.state.git.history_revision.get_untracked();
        actions.on_sync.run("sync".into());
        wait_until("sync failure", || {
            mounted.state.git.sync_error.get_untracked().is_some()
        })
        .await;
        assert!(mounted.state.git.sync_busy.get_untracked().is_none());
        assert!(mounted.state.ui.toast.get_untracked().is_some());
        assert!(mounted.state.git.history_revision.get_untracked() > failed_revision);
    }
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn local_history_and_diff_results_are_rejected_after_project_or_account_changes() {
    use super::project_git::{gitChange, gitRelease};
    let old = token().await;
    let http = Http(gitHttp());
    let handle = probe_folder();
    gitHistoryFixture(
        &http.0,
        &serde_json::to_string(&fixture()).unwrap(),
        &serde_json::to_string(&changes()).unwrap(),
    );
    gitChange(&http.0, "deferHistory", true);
    let slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let actions_slot = slot.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state
            .projects
            .projects
            .update(|projects| projects[0].mode = WorkspaceMode::Local);
        state.projects.local_handles.update(|handles| {
            handles.insert(1, handle.unchecked_into());
        });
        state.settings.bridge_url.set("ws://git.test:3001".into());
        let actions = GitHistoryActions::new();
        actions_slot.set(Some(actions));
        view! {<div/>}
    });
    let actions = slot.get().unwrap();
    wait_until("local pending history", || {
        gitCalls(&http.0).contains("/git/history")
    })
    .await;
    mounted.state.projects.active_project.set(None);
    settle().await;
    gitRelease(&http.0, "deferHistory");
    settle().await;
    assert!(actions.commits.get_untracked().is_empty());
    mounted.state.projects.active_project.set(Some(1));
    wait_until("local fresh history", || {
        actions.commits.with_untracked(Vec::len) == 1
    })
    .await;
    gitChange(&http.0, "deferDiff", true);
    actions
        .select
        .run((fixture().commits.remove(0), None, None));
    wait_until("local pending diff", || {
        gitCalls(&http.0).contains("/git/commit-diff")
    })
    .await;
    mounted.state.auth.logout();
    settle().await;
    gitRelease(&http.0, "deferDiff");
    settle().await;
    assert!(actions.diff.get_untracked().is_none());
    assert!(actions.selected.get_untracked().is_none());
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn history_details_remain_visible_in_narrow_panes_in_both_modes() {
    let old = token().await;
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let http = Http(gitHttp());
        let mut page = fixture();
        let commit = page.commits.remove(0);
        page.commits = (0..25)
            .map(|index| {
                let mut commit = commit.clone();
                commit.hash = format!("{index:040x}");
                commit.parents.clear();
                commit.message = "Detailed commit description\n".repeat(100);
                commit
            })
            .collect();
        let mut diff = changes();
        diff.diff = "+A changed line\n".repeat(100);
        gitHistoryFixture(
            &http.0,
            &serde_json::to_string(&page).unwrap(),
            &serde_json::to_string(&diff).unwrap(),
        );
        let handle = probe_folder();
        let mounted = mount_test(move |state| {
            let layout = expect_context::<openwebide_frontend::state::layout::LayoutState>();
            provide_context(
                openwebide_frontend::state_actions::layout::LayoutActions::new(
                    state.api, layout, state.auth, state.ui,
                ),
            );
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            state.settings.bridge_url.set("ws://git.test:3001".into());
            *state.fake.git_history.borrow_mut() = page;
            *state.fake.git_commit_diff.borrow_mut() = Some(diff);
            view! {<style>{include_str!("../../styles.css")}</style><div style="height:700px;width:360px;display:flex"><GitHistory on_open=Callback::new(|_|()) on_select_branch=Callback::new(|_|()) on_new_branch=Callback::new(|()|()) /></div>}
        });
        wait_until("history rows", || {
            mounted
                .root
                .query_selector(".git-history-row")
                .unwrap()
                .is_some()
        })
        .await;
        mounted.click(".git-history-row");
        wait_until("commit diff", || {
            mounted
                .root
                .query_selector(".git-commit-file")
                .unwrap()
                .is_some()
        })
        .await;
        settle().await;
        let pane = mounted.element(".git-history").get_bounding_client_rect();
        let list = mounted
            .element(".git-history-list")
            .get_bounding_client_rect();
        let details = mounted
            .element(".git-commit-details")
            .get_bounding_client_rect();
        assert!(
            details.height() >= 120.0,
            "commit details must retain usable space"
        );
        assert!(list.bottom() <= details.top() + 1.0);
        assert!(
            details.bottom() <= pane.bottom() + 1.0,
            "long history and commit messages must not push inspection below the pane"
        );
    }
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn file_history_modal_uses_branch_path_and_revision_timeline_in_both_modes() {
    let old = token().await;
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let http = Http(gitHttp());
        let handle = probe_folder();
        let mut history = fixture();
        history.commits[0].history_path = Some("src/old.rs".into());
        gitHistoryFixture(
            &http.0,
            &serde_json::to_string(&history).unwrap(),
            &serde_json::to_string(&changes()).unwrap(),
        );
        let mounted = mount_test(move |state| {
            let layout = expect_context::<openwebide_frontend::state::layout::LayoutState>();
            provide_context(
                openwebide_frontend::state_actions::layout::LayoutActions::new(
                    state.api, layout, state.auth, state.ui,
                ),
            );
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            state.settings.bridge_url.set("ws://git.test:3001".into());
            *state.fake.git_history.borrow_mut() = history;
            *state.fake.git_commit_diff.borrow_mut() = Some(changes());
            openwebide_frontend::state_actions::workspace::WorkspaceActions::new(
                state.api,
                state.projects,
                state.workspace,
                state.ui,
                RwSignal::new(false),
                Callback::new(|()| ()),
            );
            view! {<openwebide_frontend::components::git_history::FileHistory path="src/main.rs".to_owned() on_close=Callback::new(|()|()) />}
        });
        wait_until("file timeline", || {
            mounted
                .root
                .query_selector(".git-file-timeline button")
                .unwrap()
                .is_some()
        })
        .await;
        mounted.click(".git-file-timeline button");
        wait_until("historical path diff", || {
            mounted
                .root
                .query_selector(".editor-diff")
                .unwrap()
                .is_some()
        })
        .await;
        let requests: Vec<GitHistoryRequest> = if mode == WorkspaceMode::Remote {
            mounted.state.fake.git_history_requests.borrow().clone()
        } else {
            serde_json::from_str::<Vec<serde_json::Value>>(&gitCalls(&http.0))
                .unwrap()
                .into_iter()
                .filter(|call| call["path"] == "/git/history")
                .map(|call| serde_json::from_value(call["body"].clone()).unwrap())
                .collect()
        };
        assert_eq!(requests[0].path.as_deref(), Some("src/main.rs"));
        assert_eq!(requests[0].reference.as_deref(), Some("HEAD"));
        assert!(
            mounted
                .element(".git-readonly-diff")
                .text_content()
                .unwrap()
                .contains("src/old.rs")
        );
        assert!(
            mounted
                .root
                .query_selector("[role=dialog]")
                .unwrap()
                .is_some()
        );
        assert!(
            mounted
                .root
                .query_selector(".git-readonly-diff textarea")
                .unwrap()
                .is_none()
        );
        drop(mounted);
        drop(http);
    }
    restore_token(old).await;
}

#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function historyWorkingFolder(present) {
    const files = new Map(present ? [['current.rs','working source']] : []);
    const handle = name => {
        const handle = Object.create(FileSystemFileHandle.prototype);
        Object.defineProperties(handle, {name:{value:name}, kind:{value:'file'}, getFile:{value:async()=>new File([files.get(name)], name)}, createWritable:{value:async()=>Object.assign(Object.create(FileSystemWritableFileStream.prototype),{write:async value=>files.set(name,value),close:async()=>{}})}});
        return handle;
    };
    return {name:'project', queryPermission:async()=> 'granted', values:async function*(){for(const name of files.keys()) yield handle(name);}, getFileHandle:async(name,options)=>{if(!files.has(name) && !options?.create) throw new DOMException('missing','NotFoundError'); if(!files.has(name)) files.set(name,'');return handle(name);}, removeEntry:async name=>files.delete(name), getDirectoryHandle:async()=>{throw new DOMException('missing','NotFoundError');}};
}
"#)]
extern "C" {
    fn historyWorkingFolder(present: bool) -> wasm_bindgen::JsValue;
}

#[wasm_bindgen_test]
async fn file_history_open_current_preserves_edits_and_missing_files_in_both_modes() {
    let old = token().await;
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        for (present, dirty) in [(false, false), (true, false), (true, true)] {
            let http = Http(gitHttp());
            gitHistoryFixture(
                &http.0,
                &serde_json::to_string(&fixture()).unwrap(),
                &serde_json::to_string(&changes()).unwrap(),
            );
            let handle = historyWorkingFolder(present);
            let mounted = mount_test(move |state| {
                state.seed_project();
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
                state.projects.local_handles.update(|handles| {
                    handles.insert(1, handle.unchecked_into());
                });
                state.settings.bridge_url.set("ws://git.test:3001".into());
                *state.fake.git_history.borrow_mut() = fixture();
                *state.fake.git_commit_diff.borrow_mut() = Some(changes());
                if present {
                    state
                        .fake
                        .files
                        .borrow_mut()
                        .insert((1, "current.rs".into()), "working source".into());
                }
                if dirty {
                    state.workspace.open_file.set(Some("current.rs".into()));
                    state.workspace.content.set("unsaved edits".into());
                    state.workspace.dirty.set(true);
                }
                let layout = expect_context::<openwebide_frontend::state::layout::LayoutState>();
                provide_context(
                    openwebide_frontend::state_actions::layout::LayoutActions::new(
                        state.api, layout, state.auth, state.ui,
                    ),
                );
                openwebide_frontend::state_actions::workspace::WorkspaceActions::new(
                    state.api,
                    state.projects,
                    state.workspace,
                    state.ui,
                    RwSignal::new(false),
                    Callback::new(|()| ()),
                );
                let git = state.git;
                git.file_history.set(Some("current.rs".into()));
                view! {
                    <openwebide_frontend::components::ToolPanel panel=openwebide_frontend::state::layout::Panel::Editor><textarea>"editor"</textarea></openwebide_frontend::components::ToolPanel>
                    <Show when=move ||git.file_history.get().is_some()>
                        <openwebide_frontend::components::git_history::FileHistory path="current.rs".to_owned() on_close=Callback::new(move |()|git.file_history.set(None)) />
                    </Show>
                }
            });
            wait_until("timeline", || {
                mounted
                    .root
                    .query_selector(".git-file-timeline button")
                    .unwrap()
                    .is_some()
            })
            .await;
            mounted.click(".git-file-timeline button");
            wait_until("file action", || {
                mounted
                    .root
                    .query_selector("button[aria-label='Open current file in editor']")
                    .unwrap()
                    .is_some()
            })
            .await;
            mounted.click("button[aria-label='Open current file in editor']");
            if present {
                wait_until("modal closes and editor focuses", || {
                    mounted
                        .root
                        .query_selector("[role=dialog]")
                        .unwrap()
                        .is_none()
                        && web_sys::window()
                            .unwrap()
                            .document()
                            .unwrap()
                            .active_element()
                            .is_some_and(|element| element.tag_name() == "TEXTAREA")
                })
                .await;
                assert_eq!(
                    mounted.state.workspace.open_file.get_untracked().as_deref(),
                    Some("current.rs")
                );
                assert_eq!(
                    mounted.state.workspace.content.get_untracked().as_str(),
                    if dirty {
                        "unsaved edits"
                    } else {
                        "working source"
                    }
                );
                assert_eq!(mounted.state.workspace.dirty.get_untracked(), dirty);
            } else {
                wait_until("missing file alert", || {
                    mounted
                        .root
                        .query_selector("[role=alert]")
                        .unwrap()
                        .is_some()
                })
                .await;
                assert!(
                    mounted
                        .root
                        .query_selector("[role=dialog]")
                        .unwrap()
                        .is_some()
                );
                assert!(
                    mounted
                        .state
                        .ui
                        .toast
                        .get_untracked()
                        .unwrap()
                        .contains("no longer exists")
                );
            }
            drop(mounted);
            drop(http);
            openwebide_frontend::util::sleep_ms(0).await;
            settle().await;
        }
    }
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn history_status_rows_insets_and_internal_seam_share_geometry_in_both_modes() {
    let old = token().await;
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let http = Http(gitHttp());
        let mut payload = changes();
        payload.files = ["A", "D", "M", "R100", "C100", "T", "U", "???", ""]
            .into_iter()
            .enumerate()
            .map(|(index, status)| GitCommitFile {
                path: format!(
                    "planning-tms-integration/{index}-{}.rs",
                    if index == 0 {
                        "a"
                    } else {
                        "a-very-long-file-name-that-must-never-wrap"
                    }
                ),
                status: status.into(),
                previous_path: (status == "R100").then(|| "old-name.rs".into()),
            })
            .collect();
        gitHistoryFixture(
            &http.0,
            &serde_json::to_string(&fixture()).unwrap(),
            &serde_json::to_string(&payload).unwrap(),
        );
        let handle = probe_folder();
        let width = RwSignal::new(900.0);
        let mounted = mount_test(move |state| {
            state.auth.set_user(openwebide_core::User {
                id: openwebide_core::UserId::new(1),
                username: "owner".into(),
                role: openwebide_core::UserRole::User,
                created_at: 0,
            });
            let layout = expect_context::<openwebide_frontend::state::layout::LayoutState>();
            provide_context(
                openwebide_frontend::state_actions::layout::LayoutActions::new(
                    state.api, layout, state.auth, state.ui,
                ),
            );
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            state.settings.bridge_url.set("ws://git.test:3001".into());
            *state.fake.git_history.borrow_mut() = fixture();
            *state.fake.git_commit_diff.borrow_mut() = Some(payload);
            view! {<style>{include_str!("../../styles.css")}</style><div style=move ||format!("width:{}px;height:700px;display:flex",width.get())><GitHistory on_open=Callback::new(|_:String|()) on_select_branch=Callback::new(|_:String|()) on_new_branch=Callback::new(|()|()) /></div>}
        });
        wait_until("history", || {
            mounted
                .root
                .query_selector(".git-history-row")
                .unwrap()
                .is_some()
        })
        .await;
        mounted.click(".git-history-row");
        wait_until("files", || {
            mounted
                .root
                .query_selector_all(".git-commit-file")
                .unwrap()
                .length()
                == 9
        })
        .await;
        for theme in ["dark", "light"] {
            web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .document_element()
                .unwrap()
                .set_attribute("data-theme", theme)
                .unwrap();
            for tree_width in [260.0, 160.0] {
                let separator = mounted.element("[aria-label='Resize History tree']");
                while separator
                    .get_attribute("aria-valuenow")
                    .unwrap()
                    .parse::<f64>()
                    .unwrap()
                    > tree_width
                {
                    let init = web_sys::KeyboardEventInit::new();
                    init.set_key("ArrowLeft");
                    separator
                        .dispatch_event(
                            &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict(
                                "keydown", &init,
                            )
                            .unwrap(),
                        )
                        .unwrap();
                    settle().await;
                }
                let tree = mounted.element(".git-commit-files");
                let seam = separator.get_bounding_client_rect();
                let maximum = separator
                    .get_attribute("aria-valuemax")
                    .unwrap()
                    .parse::<f64>()
                    .unwrap();
                assert!(
                    maximum <= 650.0
                        && maximum + 266.0
                            <= separator
                                .parent_element()
                                .unwrap()
                                .get_bounding_client_rect()
                                .width()
                                + 1.0
                );
                assert!((seam.left() - tree.get_bounding_client_rect().right()).abs() < 1.0);
                assert!(seam.width() >= 6.0);
                let rows = mounted
                    .root
                    .query_selector_all(".git-commit-file > .tree-item")
                    .unwrap();
                let mut origin: Option<(f64, f64)> = None;
                for index in 0..rows.length() {
                    let row = rows
                        .item(index)
                        .unwrap()
                        .dyn_into::<web_sys::Element>()
                        .unwrap();
                    let name = row
                        .query_selector(".tree-name")
                        .unwrap()
                        .unwrap()
                        .get_bounding_client_rect();
                    let position = (name.left(), row.get_bounding_client_rect().height());
                    if let Some((x, height)) = origin {
                        assert!((position.0 - x).abs() < 1.0);
                        assert!((position.1 - height).abs() < 1.0);
                    } else {
                        origin = Some(position);
                    }
                    super::support::assert_start_aligned(&row);
                    assert!(!row.get_attribute("title").unwrap().is_empty());
                    assert!(
                        row.get_attribute("aria-label")
                            .unwrap()
                            .contains("planning-tms-integration/")
                    );
                    assert!(
                        (row.query_selector(".tree-icon")
                            .unwrap()
                            .unwrap()
                            .get_bounding_client_rect()
                            .width()
                            - 20.0)
                            .abs()
                            < 1.0
                    );
                }
                assert!(
                    mounted
                        .element(".git-history-folder > summary")
                        .get_attribute("title")
                        .unwrap()
                        .contains("Conflict")
                );
                assert!(
                    mounted
                        .root
                        .query_selector(".git-commit-file .git-status")
                        .unwrap()
                        .is_none()
                );
                let toolbar = mounted.element(".git-history-reference");
                let glyph = toolbar
                    .query_selector(".ui-icon-glyph")
                    .unwrap()
                    .unwrap()
                    .get_bounding_client_rect();
                assert!(
                    (glyph.left() - toolbar.get_bounding_client_rect().left() - 12.0).abs() < 1.0
                );
            }
        }
        settle().await;
        assert_eq!(
            mounted.state.fake.settings.borrow()["panel_history_tree_width"],
            "160"
        );
        assert!(
            mounted
                .element(".git-readonly-diff")
                .get_bounding_client_rect()
                .width()
                >= 260.0
        );
        width.set(400.0);
        settle().await;
        assert!(
            mounted
                .element("[aria-label='Resize History tree']")
                .get_bounding_client_rect()
                .width()
                < 1.0
        );
        drop(mounted);
        drop(http);
    }
    web_sys::window()
        .unwrap()
        .document()
        .unwrap()
        .document_element()
        .unwrap()
        .set_attribute("data-theme", "dark")
        .unwrap();
    restore_token(old).await;
}
