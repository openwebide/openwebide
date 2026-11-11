use leptos::prelude::*;
use openwebide_core::{FileEntry, GitRepoStatus, WorkspaceMode};
use openwebide_frontend::{
    components::FileTree, project_git::ProjectGit, state::auth::AuthState,
    state_actions::git::GitActions, util::sleep_ms,
};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_test::*;

use super::{
    local_bridge::probe_folder,
    support::{mount_test, settle},
};

#[wasm_bindgen(inline_js = r#"
export function gitHttp() {
    const original = window.fetch;
    const mock = { calls: [], found: true, clean: false, invalid: false, branches: [], currentBranch: "local-branch", checkoutError: false, branchesError: false };
    window.fetch = async request => {
        if (!request.url.startsWith('http://git.test:3001/')) return original(request);
        const body = JSON.parse(await request.text());
        const path = new URL(request.url).pathname;
        mock.calls.push({path, body, authorization: request.headers.get('Authorization')});
        const json = (value, status = 200) => new Response(JSON.stringify(value), {status});
        if (path === '/exec') {
            const probe = body.command.match(/\.openwebide-probe-[0-9a-f]+/)?.[0];
            return json({exit_code: mock.found ? 0 : 1, stdout: body.command.startsWith('find ') && mock.found ? `./repos/local/${probe}\n` : '', stderr: ''});
        }
        if (mock.invalid) return json({error:'cwd does not exist: repos/local'}, 400);
        if (path === '/git/history' && mock.deferHistory) await new Promise(resolve => { mock.historyResolve = resolve; });
        if (path === '/git/path-status' && mock.deferPaths) await new Promise(resolve => { mock.pathsResolve = resolve; });
        if (path === '/git/path-status' && mock.pathsError) return json({error:'index unavailable'}, 500);
        if (path === '/git/commit-diff' && mock.deferDiff) await new Promise(resolve => { mock.diffResolve = resolve; });
        if (path === '/git/history') return mock.historyError ? json({error:'history unavailable'}, 500) : json(mock.history || {commits:[],refs:[],has_more:false});
        if (path === '/git/commit-diff') return mock.diffError ? json({error:'diff unavailable'}, 500) : json(mock.commitDiff || {files:[],diff:''});
        if (path === '/git/status' && mock.statusError) return json({error:'status unavailable'}, 500);
        if (path === '/git/status') return json({branch:mock.currentBranch,commit_hash:'abc',commit_message:null,upstream:null,ahead:0,behind:0,is_clean:mock.clean,line_stats:{insertions:0,deletions:0},files:mock.clean?{}:{'main.rs':'modified'}});
        if (path === '/git/path-status' || path === '/git/path') return mock.pathError && path === '/git/path' ? json({error:'path action failed'}, 400) : json({has_head:true,staged:['a.txt'],unstaged:['a.txt'],untracked:[],renamed_from:{}});
        if (path === '/git/show') return json({content:'committed'});
        if (path === '/git/stash') return json({stashes:mock.stashes || [],output:''});
        if (path === '/git/index-diff') return json({diff:mock.indexDiff || 'staged diff'});
        if (path === '/git/diff') return json({diff:mock.diff || 'local diff'});
        if (path === '/git/branches') return mock.branchesError ? json({error:'cannot list branches'}, 400) : json(mock.branches);
        if (path === '/git/checkout') { if (mock.checkoutError) return json({error:'uncommitted changes'}, 400); const previous = mock.currentBranch; mock.currentBranch = body.branch; return json({branch:body.branch,previous_branch:previous,switched:true}); }
        if (path === '/git/commit') return mock.commitError ? json({error:'commit failed'},400) : json({commit_hash:'abc',summary:'saved',is_signed:false});
        if (path === '/git/sync' && mock.deferSync) await new Promise(resolve => { mock.syncResolve = resolve; });
        if (path === '/git/sync') return mock.syncError ? json({error:'sync failed'},400) : json({remote:'origin',branch:'main',pulled_commits:0,pushed_commits:0,output:''});
        throw new Error(path);
    };
    mock.restore = () => { window.fetch = original; };
    return mock;
}
export function gitHistoryFixture(mock, history, diff) { mock.history = JSON.parse(history); mock.commitDiff = JSON.parse(diff); }
export function gitDiff(mock, diff) { mock.diff = diff; }
export function gitBranches(mock, branches) { mock.branches = JSON.parse(branches); mock.currentBranch = "main"; }
export function gitCalls(mock) { return JSON.stringify(mock.calls); }
export function gitRelease(mock, field) { mock[field] = false; const resolve = field === "deferSync" ? mock.syncResolve : field === "deferHistory" ? mock.historyResolve : field === "deferPaths" ? mock.pathsResolve : mock.diffResolve; if (resolve) resolve(); }
export function gitChange(mock, field, value) { mock[field] = value; }
export function gitRestore(mock) { mock.restore(); }
"#)]
extern "C" {
    pub(super) fn gitHttp() -> JsValue;
    pub(super) fn gitDiff(mock: &JsValue, diff: &str);
    pub(super) fn gitHistoryFixture(mock: &JsValue, history: &str, diff: &str);
    fn gitBranches(mock: &JsValue, branches: &str);
    pub(super) fn gitCalls(mock: &JsValue) -> String;
    pub(super) fn gitRelease(mock: &JsValue, field: &str);
    pub(super) fn gitChange(mock: &JsValue, field: &str, value: bool);
    fn gitRestore(mock: &JsValue);
}

pub(super) struct Http(pub(super) JsValue);
impl Drop for Http {
    fn drop(&mut self) {
        gitRestore(&self.0);
    }
}

pub(super) async fn token() -> Option<String> {
    let old = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("git-test-token")
        .await
        .unwrap();
    old
}
pub(super) async fn restore_token(old: Option<String>) {
    if let Some(old) = old {
        openwebide_frontend::idb::set_bridge_pairing_token(&old)
            .await
            .unwrap();
    } else {
        openwebide_frontend::idb::delete_bridge_pairing_token()
            .await
            .unwrap();
    }
}

#[wasm_bindgen_test]
async fn local_git_badges_and_all_actions_share_verified_bridge_repository() {
    let old = token().await;
    let http = Http(gitHttp());
    let folder = probe_folder();
    let handle = folder.clone();
    let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let facade_slot = slot.clone();
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
        state.workspace.entries.update(|entries| {
            entries.insert(
                String::new(),
                vec![FileEntry {
                    name: "main.rs".into(),
                    path: "main.rs".into(),
                    is_dir: false,
                    size: 1,
                }],
            );
        });
        let project_git = expect_context::<ProjectGit>();
        *facade_slot.borrow_mut() = Some(project_git);
        let refresh = GitActions::refresh(
            project_git,
            state.projects,
            state.git,
            expect_context::<AuthState>(),
        );
        view! {
                   <button id="refresh" on:click=move |_| refresh.run(())>"Refresh"</button>
                   <FileTree on_toggle=Callback::new(|_| ()) on_open=Callback::new(|_| ())

        />
               }
    });
    settle().await;
    mounted.click("#refresh");
    for _ in 0..100 {
        sleep_ms(5).await;
        settle().await;
        if mounted.state.git.status.get_untracked().is_some() {
            break;
        }
    }
    assert_eq!(
        mounted.state.git.status.get_untracked().unwrap().branch,
        "local-branch"
    );
    assert!(
        mounted
            .element("[data-tree-path='main.rs']")
            .get_attribute("title")
            .unwrap()
            .contains("Modified")
    );
    assert_eq!(
        mounted
            .element(".git-badge-modified")
            .get_attribute("aria-hidden")
            .as_deref(),
        Some("true")
    );
    assert!(
        mounted
            .element(".git-badge-modified")
            .query_selector("svg")
            .unwrap()
            .is_some()
    );
    let git = slot.borrow().unwrap();
    let repo = git.repository(Some(1)).await.unwrap();
    assert_eq!(repo.file_head("main.rs").await.unwrap(), "committed");
    assert_eq!(repo.diff(Some("main.rs")).await.unwrap(), "local diff");
    assert!(repo.branches().await.unwrap().is_empty());
    repo.commit(&openwebide_core::GitCommitRequest {
        message: "save".into(),
        paths: None,
        include_untracked: false,
        staged_only: false,
    })
    .await
    .unwrap();
    repo.checkout(&openwebide_core::GitCheckoutRequest {
        branch: "other".into(),
        create_if_missing: false,
    })
    .await
    .unwrap();
    repo.sync(&openwebide_core::GitSyncRequest {
        action: "sync".into(),
        remote: None,
        branch: None,
    })
    .await
    .unwrap();
    // Resolved access is shared by UI refreshes and all Git actions.
    let before: Vec<serde_json::Value> = serde_json::from_str(&gitCalls(&http.0)).unwrap();
    assert_eq!(
        before.iter().filter(|call| call["path"] == "/exec").count(),
        1
    );
    gitChange(&http.0, "clean", true);
    mounted.click("#refresh");
    for _ in 0..100 {
        sleep_ms(5).await;
        settle().await;
        if mounted
            .state
            .git
            .status
            .get_untracked()
            .is_some_and(|s| s.is_clean)
        {
            break;
        }
    }
    assert!(
        mounted
            .root
            .query_selector(".git-badge-modified")
            .unwrap()
            .is_none()
    );
    assert!(mounted.state.fake.git_status_requests.borrow().is_empty());
    let calls: Vec<serde_json::Value> = serde_json::from_str(&gitCalls(&http.0)).unwrap();
    for call in calls
        .iter()
        .filter(|call| call["path"].as_str().unwrap().starts_with("/git/"))
    {
        assert_eq!(call["body"]["cwd"], "repos/local");
        assert_eq!(call["authorization"], "Bearer git-test-token");
    }
    assert_eq!(
        js_sys::Reflect::get(&folder, &"files".into())
            .unwrap()
            .unchecked_into::<js_sys::Map>()
            .size(),
        0
    );
    // A vanished cwd invalidates the adapter; the next access re-probes instead of
    // falling back to the backend's unrelated workspace.
    gitChange(&http.0, "invalid", true);
    assert!(repo.status().await.is_err());
    assert!(git.repository(Some(1)).await.is_err());
    gitChange(&http.0, "invalid", false);
    let recovered = git.repository(Some(1)).await.unwrap();
    assert_eq!(recovered.status().await.unwrap().branch, "other");
    let after: Vec<serde_json::Value> = serde_json::from_str(&gitCalls(&http.0)).unwrap();
    assert_eq!(
        after.iter().filter(|call| call["path"] == "/exec").count(),
        2
    );

    // Replacing a browser handle also invalidates cached host access.
    mounted.state.projects.local_handles.update(|handles| {
        handles.insert(1, probe_folder().unchecked_into());
    });
    settle().await;
    git.repository(Some(1)).await.unwrap();
    let after: Vec<serde_json::Value> = serde_json::from_str(&gitCalls(&http.0)).unwrap();
    assert_eq!(
        after.iter().filter(|call| call["path"] == "/exec").count(),
        3
    );
    drop(mounted);
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn unresolved_local_git_never_falls_back_to_remote_workspace() {
    let old = token().await;
    let http = Http(gitHttp());
    gitChange(&http.0, "found", false);
    let folder = probe_folder();
    let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let facade_slot = slot.clone();
    let mounted = mount_test(move |state| {
        *facade_slot.borrow_mut() = Some(expect_context::<ProjectGit>());
        state.seed_project();
        state
            .projects
            .projects
            .update(|projects| projects[0].mode = WorkspaceMode::Local);
        state.projects.local_handles.update(|handles| {
            handles.insert(1, folder.unchecked_into());
        });
        state.settings.bridge_url.set("ws://git.test:3001".into());
        view! { <div /> }
    });
    let git = slot.borrow().unwrap();
    settle().await;
    assert!(git.repository(Some(1)).await.is_err());
    assert!(mounted.state.fake.git_status_requests.borrow().is_empty());
    let calls: Vec<serde_json::Value> = serde_json::from_str(&gitCalls(&http.0)).unwrap();
    assert!(calls.iter().all(|call| call["path"] == "/exec"));
    drop(mounted);
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn remote_git_refresh_ignores_results_after_project_switch_or_logout() {
    for logout in [false, true] {
        let (tx, rx) = futures::channel::oneshot::channel();
        let auth = std::rc::Rc::new(std::cell::RefCell::new(None));
        let auth_slot = auth.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.fake.git_statuses.borrow_mut().push_back(rx);
            let session_auth = expect_context::<AuthState>();
            *auth_slot.borrow_mut() = Some(session_auth);
            let refresh = GitActions::refresh(
                expect_context::<ProjectGit>(),
                state.projects,
                state.git,
                session_auth,
            );
            view! { <button on:click=move |_| refresh.run(())>"Refresh"</button> }
        });
        settle().await;
        mounted.click("button");
        settle().await;
        assert_eq!(
            *mounted.state.fake.git_status_requests.borrow(),
            vec![Some(1)]
        );
        if logout {
            auth.borrow().unwrap().logout();
        } else {
            mounted.state.projects.active_project.set(Some(2));
        }
        tx.send(Ok(GitRepoStatus {
            branch: "stale".into(),
            ..Default::default()
        }))
        .unwrap();
        settle().await;
        assert!(mounted.state.git.status.get_untracked().is_none());
        drop(mounted);
    }
}

#[wasm_bindgen_test]
async fn remote_git_refresh_deduplicates_and_publishes_current_status() {
    let (tx, rx) = futures::channel::oneshot::channel();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.fake.git_statuses.borrow_mut().push_back(rx);
        let refresh = GitActions::refresh(
            expect_context::<ProjectGit>(),
            state.projects,
            state.git,
            expect_context::<AuthState>(),
        );
        view! { <button on:click=move |_| refresh.run(())>"Refresh"</button> }
    });
    settle().await;
    mounted.click("button");
    mounted.click("button");
    settle().await;
    assert_eq!(
        *mounted.state.fake.git_status_requests.borrow(),
        vec![Some(1)]
    );
    tx.send(Ok(GitRepoStatus {
        branch: "remote-branch".into(),
        ..Default::default()
    }))
    .unwrap();
    settle().await;
    assert_eq!(
        mounted.state.git.status.get_untracked().unwrap().branch,
        "remote-branch"
    );
}

#[wasm_bindgen_test]
async fn branch_selectors_share_checkout_creation_and_failures_in_both_modes() {
    use super::support::wait_until;
    use openwebide_core::{GitBranchInfo, GitCheckoutResult};
    use openwebide_frontend::{
        components::{GitPane, StatusBar},
        state_actions::git::GitActionContext,
    };
    let old = token().await;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let http = Http(gitHttp());
        let branches = vec![
            GitBranchInfo {
                name: "feature".into(),
                is_current: false,
                is_remote: false,
                upstream: None,
            },
            GitBranchInfo {
                name: "main".into(),
                is_current: true,
                is_remote: false,
                upstream: None,
            },
        ];
        gitBranches(&http.0, &serde_json::to_string(&branches).unwrap());
        let handle = probe_folder();
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
            *state.fake.git_branches.borrow_mut() = branches;
            state.git.status.set(Some(GitRepoStatus {
                branch: "main".into(),
                ..GitRepoStatus::default()
            }));
            let actions = GitActions::new(GitActionContext {
                project_git: expect_context::<ProjectGit>(),
                projects: state.projects,
                workspace: state.workspace,
                git: state.git,
                chat: state.chat,
                ui: state.ui,
                workspace_for: Callback::new(|_: i64| None),
                refresh: Callback::new(|()| ()),
            });
            view! {
                <div id="changes-branch"><GitPane on_open=Callback::new(|_: String| ()) on_load_git_diff=Callback::new(|()| ()) on_discard_git_diff=Callback::new(|()| ()) on_load_branches=actions.on_load_branches on_select_branch=actions.on_select_branch on_new_branch=actions.on_branch_click /></div>
                <div id="footer-branch"><StatusBar health=RwSignal::new(None).read_only() on_toggle_terminal=|| () on_load_branches=actions.on_load_branches on_select_branch=actions.on_select_branch on_branch_click=actions.on_branch_click /></div>
            }
        });
        wait_until("branch options", || {
            mounted
                .state
                .git
                .branches
                .with_untracked(|branches| branches.len() == 2)
        })
        .await;
        let discovery_count = || {
            if mode == WorkspaceMode::Remote {
                mounted
                    .state
                    .fake
                    .calls
                    .borrow()
                    .iter()
                    .filter(|call| {
                        matches!(
                            call,
                            openwebide_frontend::testing::fake_backend::Call::Request {
                                method: "git_branches"
                            }
                        )
                    })
                    .count()
            } else {
                let calls: Vec<serde_json::Value> =
                    serde_json::from_str(&gitCalls(&http.0)).unwrap();
                calls
                    .iter()
                    .filter(|call| call["path"] == "/git/branches")
                    .count()
            }
        };
        let before = discovery_count();
        for ahead in [1, 2, 0] {
            mounted
                .state
                .git
                .status
                .update(|status| status.as_mut().unwrap().ahead = ahead);
            settle().await;
        }
        assert_eq!(
            discovery_count(),
            before,
            "Working-tree refresh should not rediscover branches in {mode:?}"
        );
        let choose = async |selector: &str, value: &str| {
            super::support::choose_dropdown(&mounted, selector, value).await;
        };
        let (send, receive) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .git_checkout_results
            .borrow_mut()
            .push_back(receive);
        choose("#changes-branch .ui-dropdown-trigger", "branch:feature").await;
        if mode == WorkspaceMode::Remote {
            wait_until("checkout request", || {
                !mounted.state.fake.git_checkout_requests.borrow().is_empty()
            })
            .await;
            assert_eq!(
                mounted.state.git.status.get_untracked().unwrap().branch,
                "main"
            );
            send.send(Ok(GitCheckoutResult {
                branch: "feature".into(),
                previous_branch: Some("main".into()),
                switched: true,
            }))
            .unwrap();
        }
        wait_until("shared branch selection", || {
            mounted.state.git.status.with_untracked(|status| {
                status
                    .as_ref()
                    .is_some_and(|status| status.branch == "feature")
            })
        })
        .await;
        for selector in [
            "#changes-branch .ui-dropdown-trigger",
            "#footer-branch .ui-dropdown-trigger",
        ] {
            assert_eq!(mounted.element(selector).text_content().unwrap(), "feature");
        }
        if mode == WorkspaceMode::Remote {
            assert!(
                !mounted.state.fake.git_checkout_requests.borrow()[0]
                    .1
                    .create_if_missing
            );
        } else {
            assert!(gitCalls(&http.0).contains("checkout"));
        }
        choose("#footer-branch .ui-dropdown-trigger", "new").await;
        settle().await;
        let prompt = mounted.state.ui.prompt.get_untracked().unwrap();
        assert_eq!(prompt.title, "New Git Branch");
        let (send, receive) = futures::channel::oneshot::channel();
        if mode == WorkspaceMode::Remote {
            mounted
                .state
                .fake
                .git_checkout_results
                .borrow_mut()
                .push_back(receive);
        }
        prompt.on_submit.run("new-feature".into());
        if mode == WorkspaceMode::Remote {
            send.send(Ok(GitCheckoutResult {
                branch: "new-feature".into(),
                previous_branch: Some("feature".into()),
                switched: true,
            }))
            .unwrap();
        }
        wait_until("new branch selected", || {
            mounted.state.git.status.with_untracked(|status| {
                status
                    .as_ref()
                    .is_some_and(|status| status.branch == "new-feature")
            })
        })
        .await;
        if mode == WorkspaceMode::Remote {
            assert!(
                mounted
                    .state
                    .fake
                    .git_checkout_requests
                    .borrow()
                    .last()
                    .unwrap()
                    .1
                    .create_if_missing
            );
        }
        gitChange(&http.0, "checkoutError", true);
        let (send, receive) = futures::channel::oneshot::channel();
        if mode == WorkspaceMode::Remote {
            mounted
                .state
                .fake
                .git_checkout_results
                .borrow_mut()
                .push_back(receive);
        }
        choose("#changes-branch .ui-dropdown-trigger", "branch:main").await;
        if mode == WorkspaceMode::Remote {
            send.send(Err("uncommitted changes".into())).unwrap();
        }
        wait_until("checkout failure", || {
            mounted.state.ui.toast.with_untracked(|toast| {
                toast
                    .as_ref()
                    .is_some_and(|toast| toast.contains("Git checkout failed"))
            })
        })
        .await;
        assert_eq!(
            mounted.state.git.status.get_untracked().unwrap().branch,
            "new-feature"
        );
        assert!(!mounted.state.git.branch_busy.get_untracked());
        gitChange(&http.0, "branchesError", true);
        let (send, receive) = futures::channel::oneshot::channel();
        if mode == WorkspaceMode::Remote {
            mounted
                .state
                .fake
                .git_branches_results
                .borrow_mut()
                .push_back(receive);
        }
        mounted
            .element("#changes-branch .ui-dropdown-trigger")
            .unchecked_into::<web_sys::HtmlElement>()
            .click();
        settle().await;
        if mode == WorkspaceMode::Remote {
            send.send(Err("cannot list branches".into())).unwrap();
        }
        wait_until("branch discovery failure", || {
            mounted.state.git.branches_error.get_untracked().is_some()
        })
        .await;
        assert_eq!(
            mounted.state.git.status.get_untracked().unwrap().branch,
            "new-feature"
        );
        mounted.click("#changes-branch .recent-backdrop");
        settle().await;
        gitChange(&http.0, "branchesError", false);
        mounted
            .element("#footer-branch .ui-dropdown-trigger")
            .unchecked_into::<web_sys::HtmlElement>()
            .click();
        settle().await;
        wait_until("branch discovery retry", || {
            !mounted.state.git.branches_loading.get_untracked()
                && mounted.state.git.branches_error.get_untracked().is_none()
        })
        .await;
    }
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn branch_results_and_new_branch_dialog_ignore_project_and_account_changes() {
    use super::support::wait_until;
    use openwebide_core::{GitBranchInfo, GitCheckoutResult, User, UserId, UserRole};
    use openwebide_frontend::{components::BranchPicker, state_actions::git::GitActionContext};
    let (load_send, load_receive) = futures::channel::oneshot::channel();
    let slot = std::rc::Rc::new(std::cell::Cell::new(None));
    let read = slot.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state
            .fake
            .git_branches_results
            .borrow_mut()
            .push_back(load_receive);
        state.git.status.set(Some(GitRepoStatus {
            branch: "main".into(),
            ..GitRepoStatus::default()
        }));
        let actions = GitActions::new(GitActionContext {
            project_git: expect_context::<ProjectGit>(),
            projects: state.projects,
            workspace: state.workspace,
            git: state.git,
            chat: state.chat,
            ui: state.ui,
            workspace_for: Callback::new(|_: i64| None),
            refresh: Callback::new(|()| ()),
        });
        read.set(Some(actions));
        view! { <BranchPicker on_load=actions.on_load_branches on_select=actions.on_select_branch on_new=actions.on_branch_click /> }
    });
    wait_until("pending branch listing", || {
        mounted.state.git.branches_loading.get_untracked()
    })
    .await;
    let actions = slot.get().unwrap();
    actions.on_branch_click.run(());
    let prompt = mounted.state.ui.prompt.get_untracked().unwrap();
    mounted.state.projects.active_project.set(None);
    settle().await;
    load_send
        .send(Ok(vec![GitBranchInfo {
            name: "stale".into(),
            is_current: false,
            is_remote: false,
            upstream: None,
        }]))
        .unwrap();
    prompt.on_submit.run("wrong-project".into());
    settle().await;
    assert!(mounted.state.git.branches.get_untracked().is_empty());
    assert!(mounted.state.fake.git_checkout_requests.borrow().is_empty());
    mounted.state.projects.active_project.set(Some(1));
    settle().await;
    let (checkout_send, checkout_receive) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .git_checkout_results
        .borrow_mut()
        .push_back(checkout_receive);
    actions.on_select_branch.run("feature".into());
    wait_until("pending branch checkout", || {
        !mounted.state.fake.git_checkout_requests.borrow().is_empty()
    })
    .await;
    mounted.state.auth.set_user(User {
        id: UserId::new(2),
        username: "other".into(),
        role: UserRole::User,
        created_at: 0,
    });
    settle().await;
    checkout_send
        .send(Ok(GitCheckoutResult {
            branch: "feature".into(),
            previous_branch: Some("main".into()),
            switched: true,
        }))
        .unwrap();
    settle().await;
    assert_eq!(
        mounted.state.git.status.get_untracked().unwrap().branch,
        "main"
    );
    assert!(!mounted.state.git.branch_busy.get_untracked());
    assert!(mounted.state.ui.toast.get_untracked().is_none());
}

#[wasm_bindgen_test]
async fn branch_menus_stay_mounted_during_background_status_and_option_refreshes() {
    use openwebide_core::GitRepoStatus;
    use openwebide_frontend::components::{GitPane, StatusBar};
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.git.status.set(Some(GitRepoStatus {
                branch: "main".into(),
                ..GitRepoStatus::default()
            }));
            view! {
                <style>{include_str!("../../styles.css")}</style>
                <div id="changes-branch"><GitPane on_open=Callback::new(|_: String| ()) on_load_git_diff=Callback::new(|()| ()) on_discard_git_diff=Callback::new(|()| ()) /></div>
                <div id="footer-branch"><StatusBar health=RwSignal::new(None).read_only() on_toggle_terminal=|| () /></div>
            }
        });
        settle().await;
        for root in ["#changes-branch", "#footer-branch"] {
            let selector = format!("{root} .ui-dropdown-trigger");
            let trigger = mounted.element(&selector);
            trigger.click();
            settle().await;
            let menu = mounted.element(&format!("{root} .ui-dropdown-menu"));
            for ahead in [1, 2, 0] {
                mounted
                    .state
                    .git
                    .status
                    .update(|status| status.as_mut().unwrap().ahead = ahead);
                mounted.state.git.branches_loading.set(true);
                settle().await;
                mounted
                    .state
                    .git
                    .branches
                    .set(vec![openwebide_core::GitBranchInfo {
                        name: "feature".into(),
                        is_current: false,
                        is_remote: false,
                        upstream: None,
                    }]);
                mounted.state.git.branches_loading.set(false);
                settle().await;
                assert!(
                    trigger.is_same_node(Some(mounted.element(&selector).as_ref())),
                    "Branch picker remounted during status refresh in {mode:?}"
                );
                assert_eq!(
                    trigger.get_attribute("aria-expanded").as_deref(),
                    Some("true")
                );
                assert!(
                    menu.is_same_node(Some(
                        mounted
                            .element(&format!("{root} .ui-dropdown-menu"))
                            .as_ref()
                    ))
                );
                assert!(menu.text_content().unwrap().contains("feature"));
            }
            mounted.click(&format!("{root} .ui-dropdown-backdrop"));
            settle().await;
        }
    }
}

#[wasm_bindgen_test]
async fn degraded_git_refresh_retains_rows_and_complete_empty_status_clears_them_in_both_modes() {
    use super::support::wait_until;
    use openwebide_core::git::{GitFileStatus, GitStatusAvailability};
    let old = token().await;
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let http = Http(gitHttp());
        let handle = probe_folder();
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
            state.workspace.entries.update(|entries| {
                entries.insert(
                    String::new(),
                    vec![FileEntry {
                        name: "main.rs".into(),
                        path: "main.rs".into(),
                        is_dir: false,
                        size: 1,
                    }],
                );
            });
            if mode == WorkspaceMode::Remote {
                for result in [
                    GitRepoStatus {
                        branch: "main".into(),
                        files: [("main.rs".into(), GitFileStatus::Modified)].into(),
                        ..Default::default()
                    },
                    GitRepoStatus {
                        branch: "passive branch".into(),
                        availability: GitStatusAvailability::Passive,
                        ..Default::default()
                    },
                    GitRepoStatus {
                        branch: "main".into(),
                        is_clean: true,
                        ..Default::default()
                    },
                ] {
                    let (send, receive) = futures::channel::oneshot::channel();
                    send.send(Ok(result)).unwrap();
                    state.fake.git_statuses.borrow_mut().push_back(receive);
                }
            }
            let refresh = GitActions::refresh(
                expect_context::<ProjectGit>(),
                state.projects,
                state.git,
                state.auth,
            );
            let git = state.git;
            view! {<button id="refresh-degraded" on:click=move |_|refresh.run(())>"Refresh"</button><openwebide_frontend::components::ui::FeedbackOverlay message=git.status_error.read_only() on_dismiss=Callback::new(|()|()) /><FileTree on_toggle=Callback::new(|_:String|()) on_open=Callback::new(|_:String|()) />}
        });
        settle().await;
        mounted.click("#refresh-degraded");
        wait_until("complete status", || {
            mounted.state.git.status.get_untracked().is_some()
                && !mounted.state.git.status_loading.get_untracked()
        })
        .await;
        let row = mounted.element("[data-tree-path='main.rs']");
        assert!(
            row.query_selector(".tree-icon.git-badge-modified")
                .unwrap()
                .is_some()
        );
        if mode == WorkspaceMode::Local {
            gitChange(&http.0, "statusError", true);
        }
        mounted.click("#refresh-degraded");
        wait_until("degraded status", || {
            mounted.state.git.status_error.get_untracked().is_some()
        })
        .await;
        let retained = mounted.state.git.status.get_untracked().unwrap();
        assert_eq!(
            retained.files.get("main.rs"),
            Some(&GitFileStatus::Modified)
        );
        assert_eq!(
            retained.availability,
            if mode == WorkspaceMode::Remote {
                GitStatusAvailability::Passive
            } else {
                GitStatusAvailability::Unavailable
            }
        );
        if mode == WorkspaceMode::Remote {
            assert_eq!(retained.branch, "passive branch");
        }
        assert!(row.is_same_node(Some(mounted.element("[data-tree-path='main.rs']").as_ref())));
        assert!(
            row.query_selector(".tree-icon.git-badge-modified")
                .unwrap()
                .is_some()
        );
        assert!(
            mounted
                .root
                .query_selector("[role=alert]")
                .unwrap()
                .is_some()
        );
        gitChange(&http.0, "statusError", false);
        gitChange(&http.0, "clean", true);
        mounted.click("#refresh-degraded");
        wait_until(&format!("empty complete status in {mode:?}"), || {
            mounted
                .state
                .git
                .status
                .get_untracked()
                .is_some_and(|status| {
                    status.availability == GitStatusAvailability::Complete
                        && status.files.is_empty()
                })
                && !mounted.state.git.status_loading.get_untracked()
        })
        .await;
        assert!(mounted.state.git.status_error.get_untracked().is_none());
        assert!(row.is_same_node(Some(mounted.element("[data-tree-path='main.rs']").as_ref())));
        assert!(
            row.query_selector(".tree-icon.git-badge-modified")
                .unwrap()
                .is_none()
        );
        assert!(
            row.get_attribute("aria-label")
                .unwrap()
                .contains("Unchanged")
        );
        drop(mounted);
        drop(http);
    }
    restore_token(old).await;
}

#[wasm_bindgen_test]
async fn all_git_writers_recheck_durable_recovery_even_when_the_ui_still_says_ready_in_both_modes()
{
    use openwebide_core::{
        GitCheckoutRequest, GitCommitRequest, GitSyncRequest,
        git::{GitPathAction, GitPathRequest, GitStashAction, GitStashRequest},
        plugins::languages::intelligence::actions::ActionPhase,
        workspace_transaction::TransactionPhase,
    };
    use openwebide_frontend::state::workspace::WorkspaceRecoveryStatus;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let old = token().await;
        let http = Http(gitHttp());
        let folder = probe_folder();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            if mode == WorkspaceMode::Local {
                state.projects.local_handles.update(|handles| {
                    handles.insert(1, folder.unchecked_into());
                });
            }
            state.settings.bridge_url.set("ws://git.test:3001".into());
            view! {<span/>}
        });
        settle().await;
        let facade = mounted.state.owner.with(expect_context::<ProjectGit>);
        let repo = facade.repository(Some(1)).await.unwrap();
        let commit = GitCommitRequest {
            message: "save".into(),
            paths: None,
            include_untracked: false,
            staged_only: false,
        };
        let checkout = GitCheckoutRequest {
            branch: "other".into(),
            create_if_missing: false,
        };
        let sync = GitSyncRequest {
            action: "sync".into(),
            remote: None,
            branch: None,
        };
        let path = GitPathRequest {
            path: String::new(),
            action: GitPathAction::StageAll,
        };
        for transaction in [true, false] {
            for phase in 0..4 {
                let harness = &mounted.state.fake.workspace_transactions;
                harness.records.borrow_mut().clear();
                harness.commands.borrow_mut().clear();
                if transaction {
                    harness.pending(
                        1,
                        mode,
                        [
                            TransactionPhase::Prepared,
                            TransactionPhase::Applying,
                            TransactionPhase::Reverting,
                            TransactionPhase::Finalizing,
                        ][phase],
                    );
                } else {
                    harness.pending(1, mode, TransactionPhase::Committed);
                    harness.command(
                        1,
                        [
                            ActionPhase::Approved,
                            ActionPhase::Running,
                            ActionPhase::Rejected,
                            ActionPhase::Uncertain,
                        ][phase],
                    );
                }
                mounted.state.workspace.workspace_recovery.update(|states| {
                    states.insert(1, WorkspaceRecoveryStatus::Ready);
                });
                assert!(repo.commit(&commit).await.is_err());
                assert!(repo.checkout(&checkout).await.is_err());
                assert!(repo.sync(&sync).await.is_err());
                assert!(repo.path_action(&path).await.is_err());
                for action in [
                    GitStashAction::Save,
                    GitStashAction::Apply,
                    GitStashAction::Drop,
                ] {
                    assert!(
                        repo.stash(&GitStashRequest {
                            action,
                            hash: Some("a".repeat(40)),
                            message: None
                        })
                        .await
                        .is_err()
                    );
                }
                // Read-only recovery inspection remains available.
                repo.diff(None).await.unwrap();
                repo.stash(&GitStashRequest {
                    action: GitStashAction::List,
                    hash: None,
                    message: None,
                })
                .await
                .unwrap();
                assert!(matches!(
                    mounted
                        .state
                        .workspace
                        .workspace_recovery
                        .get_untracked()
                        .get(&1),
                    Some(WorkspaceRecoveryStatus::Ready)
                ));
            }
        }
        let calls: Vec<serde_json::Value> = serde_json::from_str(&gitCalls(&http.0)).unwrap();
        assert!(!calls.iter().any(|call| matches!(
            call["path"].as_str(),
            Some("/git/commit" | "/git/checkout" | "/git/sync" | "/git/path")
        )));
        assert!(
            !calls
                .iter()
                .any(|call| call["path"] == "/git/stash" && call["body"]["action"] != "list")
        );
        assert!(mounted.state.fake.git_commit_requests.borrow().is_empty());
        assert!(mounted.state.fake.git_checkout_requests.borrow().is_empty());
        assert!(mounted.state.fake.git_sync_requests.borrow().is_empty());
        assert!(mounted.state.fake.git_path_requests.borrow().is_empty());
        assert!(
            mounted
                .state
                .fake
                .git_stash_requests
                .borrow()
                .iter()
                .all(|request| request.action == GitStashAction::List)
        );
        let harness = &mounted.state.fake.workspace_transactions;
        harness.records.borrow_mut().clear();
        harness.commands.borrow_mut().clear();
        for phase in [
            ActionPhase::Completed,
            ActionPhase::Acknowledged,
            ActionPhase::Cancelled,
        ] {
            harness.pending(
                1,
                mode,
                if phase == ActionPhase::Cancelled {
                    TransactionPhase::Restored
                } else {
                    TransactionPhase::Committed
                },
            );
            harness.command(1, phase);
            if mode == WorkspaceMode::Remote {
                let (send, receive) = futures::channel::oneshot::channel();
                send.send(Ok(openwebide_core::GitCommitResult {
                    commit_hash: "abc".into(),
                    summary: "saved".into(),
                    is_signed: false,
                    pre_commit_output: None,
                }))
                .unwrap();
                mounted
                    .state
                    .fake
                    .git_commit_results
                    .borrow_mut()
                    .push_back(receive);
            }
            repo.commit(&commit).await.unwrap();
        }
        drop(repo);
        drop(mounted);
        drop(http);
        restore_token(old).await;
    }
}
