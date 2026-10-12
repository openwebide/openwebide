use leptos::prelude::*;
use std::sync::{Arc, Mutex};

use openwebide_agent::{BridgeClient, policy::BRIDGE_TOOLS};
use openwebide_core::{CommandOutcome, Vfs};
use openwebide_frontend::{
    bridge::{BridgeConfig, BridgeCredentials},
    local_agent::{
        BRIDGE_FOLDER_NOTICE, BrowserBridgeClient, local_tools, resolve_bridge_cwd_with,
    },
    local_fs::BrowserFsaVfs,
    util::sleep_ms,
};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_test::*;

use super::support::{chat_view, mount_test, settle};

#[wasm_bindgen(inline_js = r#"
export function probeFolder() {
    const files = new Map();
    const deleted = [];
    return {
        files, deleted, name: 'project',
        values: async function*() {},
        getDirectoryHandle: async () => { throw new DOMException('missing', 'NotFoundError'); },
        queryPermission: async () => 'granted',
        getFileHandle: async (name, options) => {
            if (!files.has(name) && !options?.create) throw new DOMException('missing', 'NotFoundError');
            files.set(name, files.get(name) || '');
            return Object.assign(Object.create(FileSystemFileHandle.prototype), { getFile: async () => new File([files.get(name)], name), createWritable: async () => Object.assign(Object.create(FileSystemWritableFileStream.prototype), {
                write: async value => { files.set(name, value); },
                close: async () => {}
            }) });
        },
        removeEntry: async name => { deleted.push(name); files.delete(name); }
    };
}
export function disappearingEntryFolder(errorName) {
    return { name: 'project', queryPermission: async () => 'granted', values: async function*() {
        for (const name of ['temporary.crswap', 'kept.txt']) {
            const handle = Object.create(FileSystemFileHandle.prototype);
            Object.defineProperties(handle, { name: {value:name}, kind: {value:'file'}, getFile: {value:async () => {
                if (name === 'temporary.crswap') throw new DOMException('entry disappeared', errorName);
                return new File(['kept'], name);
            }}});
            yield handle;
        }
    }};
}
export function emptyReadFolder() {
    return { name: 'project', queryPermission: async () => 'granted', values: async function*() {},
        removeEntry: async () => { throw new DOMException('missing', 'NotFoundError'); },
        getDirectoryHandle: async () => { throw new DOMException('missing', 'NotFoundError'); },
        getFileHandle: async () => { throw new DOMException('missing', 'NotFoundError'); } };
}
export function fakeBridgeHttp() {
    const original = window.fetch;
    const mock = { found: true, invalid: false, hanging: false, aborted: 0, calls: [], restore: () => { window.fetch = original; } };
    window.fetch = async request => {
        if (!request.url.startsWith('http://bridge.test:3001/')) return original(request);
        const body = JSON.parse((await request.text()) || '{}');
        const path = new URL(request.url).pathname;
        mock.calls.push({ path, body, authorization: request.headers.get('Authorization') });
        if (path === '/mcp/open') return new Response(JSON.stringify({id:'', tools:[], failures:[]}));
        if (path.startsWith('/plugins/prepare/')) {
            if (mock.invalid) return new Response(JSON.stringify({error:'preparation failed'}), {status:400});
            const id = body.id || 'a'.repeat(32);
            const plugin = JSON.parse(mock.plugin);
            if (path.endsWith('/start')) return new Response(JSON.stringify({id, state:'queued', prepared:null, error:null}));
            if (path.endsWith('/cancel')) return new Response(JSON.stringify({id, state:'cancelled', prepared:null, error:null}));
            return new Response(JSON.stringify({id, state:'ready', prepared:plugin.prepared || plugin, error:null}));
        }
        if (path === '/plugins/prepare' || path === '/plugins/package') { const plugin = JSON.parse(mock.plugin); return mock.invalid ? new Response(JSON.stringify({error:'preparation failed'}), {status:400}) : new Response(JSON.stringify(path === '/plugins/prepare' ? (plugin.prepared || plugin) : plugin)); }
        if (path === '/plugins/invoke') {
            mock.actor = body.call;
            const plugin = JSON.parse(mock.plugin);
            mock.collection = (plugin.prepared || plugin).manifest.executable.library === 'skills_fixture' ? 'skills' : 'memories';
            return new Response(JSON.stringify({id:'plugin-ui', step:{status:'ready'}}));
        }
        if (path === '/plugins/continue') {
            const continuation = body.continuation;
            if (continuation.sequence === 0) {
                if (mock.actor.operation === 'context') {
                    return new Response(JSON.stringify({id:'plugin-ui', step:{status:'host_call', sequence:1, capability:'collections', payload:JSON.stringify({collection:mock.collection, operation:{action:'list', after:0}})}}));
                }
                const args = JSON.parse(mock.actor.arguments);
                const action = mock.actor.name.replace(/^(memory|skill)_/, '');
                const value = mock.collection === 'skills' ? {draft:args.draft} : {title:args.title, content:args.content, auto_title:args.auto_title};
                const operation = action === 'create' ? {action, value} : action === 'update' ? {action, id:args.id, revision:args.revision, value} : {action, id:args.id, revision:args.revision};
                return new Response(JSON.stringify({id:'plugin-ui', step:{status:'host_call', sequence:1, capability:'collections', payload:JSON.stringify({collection:mock.collection, operation})}}));
            }
            const ok = continuation.response.Ok !== undefined;
            if (mock.actor.operation === 'context' && ok) {
                const page = JSON.parse(continuation.response.Ok);
                const plugin = JSON.parse(mock.plugin);
                const tools = (plugin.prepared || plugin).manifest.contributions.tools.map(tool => tool.name);
                const prompt = page.records.map(record => mock.collection === 'skills' ? record.value.draft.name : record.value.content).join('\n');
                const content = JSON.stringify({prompt:page.enabled ? prompt || null : null, disabled_tools:page.enabled ? [] : tools});
                return new Response(JSON.stringify({id:'plugin-ui', step:{status:'complete', ok:true, content, summary:'Fixture context'}}));
            }
            return new Response(JSON.stringify({id:'plugin-ui', step:{status:'complete', ok, content:ok ? continuation.response.Ok : continuation.response.Err, summary:ok ? 'Stored' : 'Failed'}}));
        }
        if (path === '/plugins/cancel') { mock.actor = null; return new Response('{}'); }
        if (path === '/scheduler/host') return new Response(JSON.stringify({id:'paired-host', name:'Test host', last_seen:0}));
        if (path === '/host/info' && !mock.hanging) return new Response(JSON.stringify({host_name:'bridge-host', os:'linux', scope:'bridge host', cpu:'Test CPU', logical_cores:8, ram_total_bytes:32000000000, ram_available_bytes:16000000000, disks:[], temperatures:[], gpus:[], fans:[], notes:[]}));
        if (path === '/environment' && !mock.hanging) return new Response(JSON.stringify({os: 'linux', shell: 'sh'}));
        if (mock.hanging) return new Promise((resolve, reject) => {
            const abort = () => {
                mock.aborted++;
                reject(new DOMException('aborted', 'AbortError'));
            };
            if (request.signal.aborted) abort();
            else request.signal.addEventListener('abort', abort, { once: true });
        });
        if (path === '/git/diff' && !mock.invalid) return new Response(JSON.stringify({diff:mock.diff || ''}));
        if (mock.invalid) return new Response(JSON.stringify({ error: 'cwd does not exist: repos/x' }), { status: 400 });
        if (body.branch === 'fix-cwd..mapping') return new Response(JSON.stringify({ error: `invalid branch name '${body.branch}': invalid ref` }), { status: 400 });
        const probe = body.command?.match(/\.openwebide-probe-[0-9a-f]+/)?.[0];
        const stdout = body.command?.startsWith('find ') && mock.found ? `./repos/x/${probe}\n` : '';
        return new Response(JSON.stringify({ exit_code: mock.found ? 0 : 1, stdout, stderr: '' }));
    };
    return mock;
}
export function setBridgePlugin(mock, plugin) { mock.plugin = plugin; }
export function setBridgeDiff(mock, diff) { mock.diff = diff; }
export function restoreBridgeHttp(mock) { mock.restore(); }
export function bridgeCalls(mock) { return JSON.stringify(mock.calls); }
export function bridgeFound(mock, found) { mock.found = found; }
export function bridgeHanging(mock, hanging) { mock.hanging = hanging; }
export function bridgeAborted(mock) { return mock.aborted; }
export function bridgeInvalid(mock) { mock.invalid = true; }
export function denyFolder(folder) {
    folder.getFileHandle = async () => { throw new DOMException('denied', 'NotAllowedError'); };
}
export function folderEmpty(folder) { return folder.files.size === 0 && folder.deleted.length > 0; }
export function probeDeleted(folder, name) {
    return folder.deleted.includes(name) && !folder.files.has(name);
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = fakeBridgeHttp)]
    pub(crate) fn fake_bridge_http() -> JsValue;
    #[wasm_bindgen(js_name = setBridgePlugin)]
    pub(crate) fn set_bridge_plugin(mock: &JsValue, plugin: &str);
    #[wasm_bindgen(js_name = setBridgeDiff)]
    fn set_bridge_diff(mock: &JsValue, diff: &str);
    #[wasm_bindgen(js_name = restoreBridgeHttp)]
    pub(crate) fn restore_bridge_http(mock: &JsValue);
    #[wasm_bindgen(js_name = bridgeCalls)]
    fn bridge_calls(mock: &JsValue) -> String;
    #[wasm_bindgen(js_name = bridgeFound)]
    pub(crate) fn bridge_found(mock: &JsValue, found: bool);
    #[wasm_bindgen(js_name = bridgeHanging)]
    fn bridge_hanging(mock: &JsValue, hanging: bool);
    #[wasm_bindgen(js_name = bridgeAborted)]
    fn bridge_aborted(mock: &JsValue) -> u32;
    #[wasm_bindgen(js_name = bridgeInvalid)]
    fn bridge_invalid(mock: &JsValue);
    #[wasm_bindgen(js_name = denyFolder)]
    fn deny_folder(folder: &JsValue);
    #[wasm_bindgen(js_name = folderEmpty)]
    fn folder_empty(folder: &JsValue) -> bool;
    #[wasm_bindgen(js_name = probeFolder)]
    pub(crate) fn probe_folder() -> JsValue;
    fn disappearingEntryFolder(error_name: &str) -> JsValue;
    #[wasm_bindgen(js_name = emptyReadFolder)]
    pub(crate) fn empty_read_folder() -> JsValue;
    #[wasm_bindgen(js_name = probeDeleted)]
    fn probe_deleted(folder: &JsValue, name: &str) -> bool;
}

#[wasm_bindgen_test]
async fn local_vfs_clones_read_write_and_drop_unpolled_operations() {
    let folder = probe_folder();
    let vfs = BrowserFsaVfs::new(folder.unchecked_into());
    let clone = vfs.clone();
    vfs.write("note.txt", "first λ").await.unwrap();
    assert_eq!(clone.read("note.txt").await.unwrap(), "first λ");
    drop(clone.write("note.txt", "must not run"));
    assert_eq!(vfs.read("note.txt").await.unwrap(), "first λ");
    clone.write("note.txt", "latest").await.unwrap();
    drop(clone);
    assert_eq!(vfs.read("note.txt").await.unwrap(), "latest");
    vfs.delete("note.txt").await.unwrap();
    assert!(matches!(
        vfs.read("note.txt").await,
        Err(openwebide_core::VfsError::NotFound(_))
    ));
}

#[derive(Clone)]
struct FakeBridgeHttp {
    cwd: String,
    replies: Arc<Mutex<std::collections::VecDeque<Result<CommandOutcome, String>>>>,
    calls: Arc<Mutex<Vec<(String, String, u64)>>>,
}

impl BridgeClient for FakeBridgeHttp {
    async fn execute_command(&self, command: &str, timeout: u64) -> Result<CommandOutcome, String> {
        self.calls
            .lock()
            .unwrap()
            .push((self.cwd.clone(), command.into(), timeout));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted probe response")
    }
}

fn bridge(replies: Vec<Result<CommandOutcome, String>>) -> FakeBridgeHttp {
    FakeBridgeHttp {
        cwd: String::new(),
        replies: Arc::new(Mutex::new(replies.into())),
        calls: Arc::new(Mutex::new(Vec::new())),
    }
}

fn outcome(stdout: &str, exit_code: i32) -> CommandOutcome {
    CommandOutcome {
        exit_code: Some(exit_code),
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

#[wasm_bindgen_test]
async fn discovery_with_traversal_error_is_saved_and_enables_commands() {
    let mounted = mount_test(|state| {
        state.seed_session();
        chat_view(state)
    });
    let folder = probe_folder();
    let vfs = BrowserFsaVfs::new(folder.clone().unchecked_into());
    let http = bridge(vec![Ok(outcome("./repos/x/.openwebide-probe-ab12\n", 1))]);
    let cwd = resolve_bridge_cwd_with(mounted.state.api, &vfs, 12, "ab12", |cwd| FakeBridgeHttp {
        cwd,
        ..http.clone()
    })
    .await;
    assert_eq!(cwd.as_deref(), Some("repos/x"));
    assert_eq!(
        mounted
            .state
            .fake
            .settings
            .borrow()
            .get("local_bridge_cwd.12")
            .map(String::as_str),
        Some("repos/x")
    );
    assert!(
        local_tools(cwd.as_deref())
            .iter()
            .any(|tool| tool.name == "run_command")
    );
    assert!(probe_deleted(&folder, ".openwebide-probe-ab12"));
    let calls = http.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "");
    assert!(calls[0].1.starts_with("find . -maxdepth 5"));
    assert_eq!(calls[0].2, 10);
}

#[wasm_bindgen_test]
async fn missing_folder_hides_bridge_tools_and_notifies_once() {
    let mounted = mount_test(|state| {
        state.seed_session();
        chat_view(state)
    });
    for reply in [Ok(outcome("", 0)), Err("bridge unreachable".into())] {
        let folder = probe_folder();
        let vfs = BrowserFsaVfs::new(folder.clone().unchecked_into());
        let http = bridge(vec![reply]);
        let cwd =
            resolve_bridge_cwd_with(mounted.state.api, &vfs, 12, "ab12", |cwd| FakeBridgeHttp {
                cwd,
                ..http.clone()
            })
            .await;
        assert_eq!(cwd, None);
        assert!(
            local_tools(cwd.as_deref())
                .iter()
                .all(|tool| !BRIDGE_TOOLS.contains(&tool.name.as_str()))
        );
        assert!(
            local_tools(cwd.as_deref())
                .iter()
                .any(|tool| tool.name == "read_file")
        );
        mounted.state.chat.notify_bridge_folder_once(1);
        assert!(probe_deleted(&folder, ".openwebide-probe-ab12"));
    }
    settle().await;
    let text = mounted.root.text_content().unwrap();
    assert_eq!(text.matches("Command and git tools are off").count(), 1);
    assert!(text.contains("bridge can't see this folder"));
    assert_eq!(mounted.state.chat.messages.get_untracked().len(), 1);
    assert!(BRIDGE_FOLDER_NOTICE.contains("--workspace"));
}

#[wasm_bindgen_test]
async fn candidate_is_verified_each_run_and_rediscovered_when_stale() {
    let mounted = mount_test(|state| {
        state.seed_session();
        chat_view(state)
    });
    mounted
        .state
        .fake
        .settings
        .borrow_mut()
        .insert("local_bridge_cwd.12".into(), "repos/x".into());
    let folder = probe_folder();
    let vfs = BrowserFsaVfs::new(folder.clone().unchecked_into());
    let http = bridge(vec![
        Ok(outcome("", 0)),
        Ok(outcome("", 1)),
        Ok(outcome("./.openwebide-probe-ab12", 0)),
    ]);
    let first =
        resolve_bridge_cwd_with(mounted.state.api, &vfs, 12, "ab12", |cwd| FakeBridgeHttp {
            cwd,
            ..http.clone()
        })
        .await;
    assert_eq!(first.as_deref(), Some("repos/x"));
    let second =
        resolve_bridge_cwd_with(mounted.state.api, &vfs, 12, "ab12", |cwd| FakeBridgeHttp {
            cwd,
            ..http.clone()
        })
        .await;
    assert_eq!(second.as_deref(), Some(""));
    assert_eq!(
        mounted
            .state
            .fake
            .settings
            .borrow()
            .get("local_bridge_cwd.12")
            .map(String::as_str),
        Some("")
    );
    let calls = http.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[0],
        ("repos/x".into(), "test -f .openwebide-probe-ab12".into(), 5)
    );
    assert_eq!(calls[1], calls[0]);
    assert_eq!(calls[2].0, "");
    assert!(probe_deleted(&folder, ".openwebide-probe-ab12"));
}

pub(crate) struct HttpGuard(pub(crate) JsValue);
impl Drop for HttpGuard {
    fn drop(&mut self) {
        restore_bridge_http(&self.0);
    }
}

#[wasm_bindgen_test]
async fn local_runs_use_discovered_tools_and_hide_them_when_bridge_cannot_see_folder() {
    let http = HttpGuard(fake_bridge_http());
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("probe-token")
        .await
        .unwrap();
    let folder = probe_folder();
    let handle = folder.clone();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        // This contract varies bridge discovery, not the model's tool-schema budget.
        state.fake.connections.borrow_mut()[0].context_limit = Some(32768);
        state
            .settings
            .connections
            .update(|connections| connections[0].context_limit = Some(32768));
        state
            .projects
            .projects
            .update(|projects| projects[0].mode = openwebide_core::WorkspaceMode::Local);
        state.projects.local_handles.update(|handles| {
            handles.insert(1, handle.unchecked_into());
        });
        state
            .settings
            .bridge_url
            .set("ws://bridge.test:3001".into());
        chat_view(state)
    });
    settle().await;
    for (found, hanging) in [(true, false), (false, false), (false, true), (true, false)] {
        bridge_hanging(&http.0, hanging);
        bridge_found(&http.0, found);
        mounted
            .state
            .fake
            .scripted_completions
            .borrow_mut()
            .push_back(openwebide_core::ChatCompletion {
                reasoning: String::new(),
                stop_reason: openwebide_core::StopReason::Complete,
                response: openwebide_core::ChatResponse::Text("Reply".into()),
                preamble: String::new(),
                usage: None,
            });
        let before = mounted.state.fake.completion_requests.borrow().len();
        mounted.input("check this folder");
        mounted.key("Enter", "Enter", false);
        for _ in 0..4000 {
            sleep_ms(5).await;
            settle().await;
            if mounted.state.fake.completion_requests.borrow().len() > before
                && !mounted.state.chat.streaming.get_untracked()
            {
                break;
            }
        }
        assert!(
            mounted.state.chat.error.get_untracked().is_none(),
            "{:?}",
            mounted.state.chat.error.get_untracked()
        );
        let requests = mounted.state.fake.completion_requests.borrow();
        assert_eq!(requests.len(), before + 1);
        let tools = &requests.last().unwrap().tools;
        assert_eq!(tools.iter().any(|tool| tool.name == "run_command"), found);
        if !found {
            assert!(
                tools
                    .iter()
                    .all(|tool| !BRIDGE_TOOLS.contains(&tool.name.as_str()))
            );
        }
        assert!(folder_empty(&folder));
        if hanging {
            assert_eq!(bridge_aborted(&http.0), 2);
        }
    }
    assert_eq!(
        mounted
            .state
            .fake
            .settings
            .borrow()
            .get("local_bridge_cwd.1")
            .map(String::as_str),
        Some("repos/x")
    );
    assert_eq!(
        mounted
            .root
            .text_content()
            .unwrap()
            .matches("Command and git tools are off")
            .count(),
        1
    );
    restore_bridge_http(&http.0);
    let http = HttpGuard(fake_bridge_http());
    let credentials = BridgeCredentials::new(mounted.state.api);
    for cwd in ["repos/x", ""] {
        let client = BrowserBridgeClient::for_project(
            BridgeConfig::new("ws://bridge.test:3001").http_url,
            cwd.into(),
            credentials.clone(),
        );
        client.execute_command("pwd", 5).await.unwrap();
        let _ = client.git_status().await;
        let _ = client.git_diff(Some("file.rs")).await;
        let _ = client
            .git_commit(&openwebide_core::GitCommitRequest {
                message: "commit".into(),
                paths: None,
                include_untracked: false,
                staged_only: false,
            })
            .await;
        let _ = client
            .git_checkout(&openwebide_core::GitCheckoutRequest {
                branch: "main".into(),
                create_if_missing: false,
            })
            .await;
        let calls: Vec<serde_json::Value> = serde_json::from_str(&bridge_calls(&http.0)).unwrap();
        for call in &calls[calls.len() - 5..] {
            let expected = if call["path"] == "/exec" || !cwd.is_empty() {
                cwd
            } else {
                "."
            };
            assert_eq!(call["body"]["cwd"], expected);
            assert_eq!(call["authorization"], "Bearer probe-token");
        }
    }
    let client = BrowserBridgeClient::for_project(
        "http://bridge.test:3001".into(),
        "repos/x".into(),
        credentials,
    );
    let error = client
        .git_checkout(&openwebide_core::GitCheckoutRequest {
            branch: "fix-cwd..mapping".into(),
            create_if_missing: false,
        })
        .await
        .unwrap_err();
    assert!(error.contains("invalid branch name"));
    client.execute_command("pwd", 5).await.unwrap();
    bridge_invalid(&http.0);
    assert!(
        client
            .execute_command("pwd", 5)
            .await
            .unwrap_err()
            .contains("HTTP 400")
    );
    let before = bridge_calls(&http.0);
    assert!(
        client
            .execute_command("pwd", 5)
            .await
            .unwrap_err()
            .contains("no longer verified")
    );
    assert_eq!(bridge_calls(&http.0), before);
    if let Some(token) = previous {
        openwebide_frontend::idb::set_bridge_pairing_token(&token)
            .await
            .unwrap();
    } else {
        openwebide_frontend::idb::delete_bridge_pairing_token()
            .await
            .unwrap();
    }
}

#[wasm_bindgen_test]
async fn terminal_local_spawns_resolve_cwd_and_refuse_stale_probes() {
    use openwebide_core::{BridgeClientMessage, BridgeServerMessage};
    use openwebide_frontend::{
        bridge::BridgeConn, components::TerminalPane, testing::fake_transport::FakeTransport,
    };
    use std::{cell::RefCell, rc::Rc};
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("probe-token")
        .await
        .unwrap();
    for scenario in [
        "shell",
        "parallel",
        "empty",
        "typed",
        "test",
        "restart",
        "denied",
        "missing",
        "unresolved",
        "switch",
        "deleted",
        "logout",
        "url",
        "unmount",
    ] {
        let http = HttpGuard(fake_bridge_http());
        let folder = probe_folder();
        if scenario == "denied" {
            deny_folder(&folder);
        }
        let handle = folder.clone();
        let fake = Rc::new(FakeTransport::default());
        let transport = fake.clone();
        let slot = Rc::new(RefCell::new(None));
        let connection_slot = slot.clone();
        let auth_slot = Rc::new(RefCell::new(None));
        let auth_context = auth_slot.clone();
        let command_slot = Rc::new(RefCell::new(None));
        let command_context = command_slot.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.projects.projects.update(|projects| {
                projects[0].mode = if scenario == "restart" {
                    openwebide_core::WorkspaceMode::Remote
                } else {
                    openwebide_core::WorkspaceMode::Local
                };
            });
            if scenario != "missing" {
                state.projects.local_handles.update(|handles| {
                    handles.insert(1, handle.unchecked_into());
                });
            }
            state
                .settings
                .bridge_url
                .set("ws://bridge.test:3001".into());
            *command_context.borrow_mut() = Some(
                expect_context::<openwebide_frontend::state::layout::LayoutState>().terminal_cmd,
            );
            *auth_context.borrow_mut() =
                Some(expect_context::<openwebide_frontend::state::auth::AuthState>());
            let bridge = BridgeConn::with_transport(
                BridgeConfig::new("ws://bridge.test:3001"),
                transport,
                Rc::new(|| Box::pin(async { Ok("token".into()) })),
            );
            *connection_slot.borrow_mut() = Some(bridge.clone());
            view! { <TerminalPane bridge=bridge on_close=|| () /> }
        });
        settle().await;
        fake.reply(BridgeServerMessage::HelloOk {
            user_id: Some(1),
            protocol: 1,
            runs: false,
        });
        settle().await;
        if scenario == "unresolved" {
            bridge_found(&http.0, false);
        }
        let deferred = matches!(
            scenario,
            "switch" | "deleted" | "logout" | "url" | "unmount"
        );
        let (tx, rx) = futures::channel::oneshot::channel();
        if deferred {
            mounted
                .state
                .fake
                .settings_load_results
                .borrow_mut()
                .push_back(rx);
        }
        match scenario {
            "empty" | "typed" => {
                let input = mounted
                    .root
                    .query_selector(".terminal-input")
                    .unwrap()
                    .unwrap()
                    .unchecked_into::<web_sys::HtmlInputElement>();
                input.set_value(if scenario == "typed" {
                    "echo local"
                } else {
                    ""
                });
                input
                    .dispatch_event(&web_sys::Event::new("input").unwrap())
                    .unwrap();
                mounted.click_text("Send");
            }
            "restart" => {
                super::support::click_action(&mounted, r#"button[title="New interactive shell"]"#)
                    .await;
                let id = fake
                    .sent()
                    .into_iter()
                    .find_map(|message| match message {
                        BridgeClientMessage::Spawn { id, .. } => Some(id),
                        _ => None,
                    })
                    .unwrap();
                fake.reply(BridgeServerMessage::Spawned {
                    id: id.clone(),
                    pid: 1,
                    pty: true,
                });
                mounted
                    .state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = openwebide_core::WorkspaceMode::Local);
                fake.disconnect();
                sleep_ms(1100).await;
                fake.reply(BridgeServerMessage::HelloOk {
                    user_id: Some(1),
                    protocol: 1,
                    runs: false,
                });
                settle().await;
                fake.reply(BridgeServerMessage::Error {
                    id,
                    message: "session not found".into(),
                });
            }
            "test" => command_slot
                .borrow()
                .unwrap()
                .set(Some("cargo test".into())),
            "parallel" => {
                super::support::click_action(&mounted, r#"button[title="New interactive shell"]"#)
                    .await;
                super::support::click_action(&mounted, r#"button[title="New interactive shell"]"#)
                    .await;
            }
            _ => {
                super::support::click_action(&mounted, r#"button[title="New interactive shell"]"#)
                    .await;
            }
        }
        settle().await;
        match scenario {
            "switch" => mounted.state.projects.active_project.set(None),
            "deleted" => mounted.state.projects.projects.set(Vec::new()),
            "logout" => auth_slot.borrow().unwrap().logout(),
            "url" => mounted.state.settings.bridge_url.set("ws://other".into()),
            "unmount" => {
                drop(mounted);
                tx.send(Ok(Default::default())).unwrap();
                for _ in 0..200 {
                    sleep_ms(5).await;
                    settle().await;
                    if folder_empty(&folder) {
                        break;
                    }
                }
                assert!(
                    !fake
                        .sent()
                        .iter()
                        .any(|message| matches!(message, BridgeClientMessage::Spawn { .. }))
                );
                slot.borrow_mut().take().unwrap().close();
                continue;
            }
            _ => {}
        }
        if deferred {
            tx.send(Ok(Default::default())).unwrap();
        }
        for _ in 0..200 {
            sleep_ms(5).await;
            settle().await;
            if fake
                .sent()
                .iter()
                .filter(|message| matches!(message, BridgeClientMessage::Spawn { .. }))
                .count()
                > usize::from(matches!(scenario, "restart" | "parallel"))
                || (matches!(scenario, "logout" | "url") && folder_empty(&folder))
            {
                break;
            }
        }
        let frame = js_sys::Promise::new(&mut |resolve, _| {
            web_sys::window()
                .unwrap()
                .request_animation_frame(&resolve)
                .unwrap();
        });
        wasm_bindgen_futures::JsFuture::from(frame).await.unwrap();
        settle().await;
        let spawns = fake
            .sent()
            .into_iter()
            .filter_map(|message| match message {
                BridgeClientMessage::Spawn { cwd, .. } => Some(cwd),
                _ => None,
            })
            .skip(usize::from(scenario == "restart"))
            .collect::<Vec<_>>();
        if matches!(scenario, "logout" | "url") {
            assert!(spawns.is_empty(), "{scenario}");
        } else {
            let fallback = matches!(scenario, "missing" | "unresolved" | "deleted" | "denied");
            assert_eq!(
                spawns,
                vec![
                    if fallback {
                        None
                    } else {
                        Some("repos/x".into())
                    };
                    if scenario == "parallel" { 2 } else { 1 }
                ],
                "{scenario}"
            );
            assert_eq!(
                mounted
                    .root
                    .text_content()
                    .unwrap()
                    .contains("starting in the bridge workspace root"),
                fallback,
                "{scenario}"
            );
        }
        if scenario != "missing" {
            assert!(folder_empty(&folder), "{scenario}");
        }
        drop(mounted);
        slot.borrow_mut().take().unwrap().close();
    }
    if let Some(previous) = previous {
        openwebide_frontend::idb::set_bridge_pairing_token(&previous)
            .await
            .unwrap();
    } else {
        openwebide_frontend::idb::delete_bridge_pairing_token()
            .await
            .unwrap();
    }
}

#[wasm_bindgen_test]
async fn resume_after_failed_history_persistence_reuses_write_id() {
    use super::support::editor_view;
    use openwebide_core::{
        ChatCompletion, ChatResponse, EditDecision, FileDiff, PersistedEdit, StopReason, ToolCall,
        WorkspaceMode,
    };

    let http = HttpGuard(fake_bridge_http());
    bridge_found(&http.0, false);
    let folder = probe_folder();
    let vfs = BrowserFsaVfs::new(folder.clone().unchecked_into());
    vfs.write("file.rs", "original").await.unwrap();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        state
            .projects
            .projects
            .update(|projects| projects[0].mode = WorkspaceMode::Local);
        state.projects.local_handles.update(|handles| {
            handles.insert(1, folder.unchecked_into());
        });
        state
            .settings
            .bridge_url
            .set("ws://bridge.test:3001".into());
        let mut second = state.chat.sessions.get_untracked()[0].clone();
        second.id = 2;
        state.fake.sessions.borrow_mut().push(second.clone());
        state.chat.sessions.update(|sessions| sessions.push(second));
        state
            .chat
            .set_approval_mode(1, openwebide_core::ApprovalMode::AutoAcceptEdits);
        state.fake.settings.borrow_mut().insert(
            openwebide_core::ApprovalMode::setting_key(1),
            serde_json::to_string(&openwebide_core::ApprovalMode::AutoAcceptEdits).unwrap(),
        );
        state.workspace.open_file.set(Some("file.rs".into()));
        state.workspace.content.set("dirty draft".into());
        state.workspace.dirty.set(true);
        let edit = PersistedEdit {
            file: None,
            project_id: 1,
            path: "file.rs".into(),
            revision: 1,
            decision: EditDecision::Pending,
            diff: FileDiff {
                path: "file.rs".into(),
                old: Some("original".into()),
                new: "changed".into(),
                old_unavailable: false,
                backup_path: None,
            },
        };
        state
            .fake
            .persisted_edits
            .borrow_mut()
            .insert((1, edit.path.clone()), edit.clone());
        state.workspace.set_persisted_edits(1, vec![edit]);
        view! { {chat_view(state.clone())} {editor_view(state)} }
    });
    settle().await;
    mounted
        .state
        .fake
        .message_save_results
        .borrow_mut()
        .extend([Ok(()), Ok(()), Err("offline".into())]);
    *mounted.state.fake.step_save_error.borrow_mut() = Some("offline".into());
    for content in ["first write", "resumed write"] {
        mounted
            .state
            .fake
            .scripted_completions
            .borrow_mut()
            .extend([
                ChatCompletion {
                    reasoning: String::new(),
                    stop_reason: StopReason::Complete,
                    response: ChatResponse::ToolCalls(vec![ToolCall {
                        id: "wire".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({"path": "file.rs", "content": content})
                            .to_string(),
                    }]),
                    preamble: String::new(),
                    usage: None,
                },
                ChatCompletion {
                    reasoning: String::new(),
                    stop_reason: StopReason::Complete,
                    response: ChatResponse::Text("done".into()),
                    preamble: String::new(),
                    usage: None,
                },
            ]);
    }
    mounted.input("edit file");
    mounted.key("Enter", "Enter", false);
    for _ in 0..1000 {
        sleep_ms(5).await;
        settle().await;
        if !mounted.state.chat.streaming.get_untracked() {
            break;
        }
    }
    assert!(!mounted.state.chat.streaming.get_untracked());
    // Failed checkpoint persistence must stop before changing the file.
    assert_eq!(vfs.read("file.rs").await.unwrap(), "original");
    assert!(
        mounted
            .state
            .workspace
            .agent_writes
            .get_untracked()
            .is_empty()
    );
    assert!(
        mounted
            .state
            .workspace
            .counted_agent_writes
            .get_untracked()
            .is_empty()
    );
    assert_eq!(mounted.state.fake.messages.borrow()[&1].len(), 2);
    assert!(mounted.state.fake.tool_sources.borrow().is_empty());
    assert!(mounted.state.fake.message_save_results.borrow().is_empty());
    // A failed tool-turn save ends the run before executing or requesting the next completion.
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .pop_front();
    mounted.state.chat.active_session.set(Some(2));
    settle().await;
    mounted.state.chat.active_session.set(Some(1));
    settle().await;
    let resume = mounted.state.chat.interrupted_run.get_untracked().unwrap();
    assert_eq!((resume.anchor_id, resume.first_turn), (1, 1));
    let (release, response) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .resolution_response_results
        .borrow_mut()
        .push_back(response);
    mounted.click_text("Accept");
    settle().await;
    assert_eq!(
        mounted.state.fake.persisted_edits.borrow()[&(1, "file.rs".into())].decision,
        EditDecision::Accepted
    );
    *mounted.state.fake.step_save_error.borrow_mut() = None;
    mounted.click_text("Resume");
    for _ in 0..1000 {
        sleep_ms(5).await;
        settle().await;
        if !mounted.state.chat.streaming.get_untracked() {
            break;
        }
    }
    assert!(!mounted.state.chat.streaming.get_untracked());
    assert_eq!(vfs.read("file.rs").await.unwrap(), "resumed write");
    assert_eq!(
        mounted
            .state
            .fake
            .tool_sources
            .borrow()
            .get(&(1, "a1t1c0".into())),
        Some(&true)
    );
    assert_eq!(
        mounted.state.fake.persisted_edits.borrow()[&(1, "file.rs".into())].revision,
        2
    );
    release.send(Ok(())).unwrap();
    settle().await;
    assert_eq!(
        mounted.state.workspace.agent_writes.get_untracked()[&(1, "file.rs".into())],
        1
    );
    assert_eq!(
        mounted.state.workspace.content.get_untracked(),
        "dirty draft"
    );
    assert!(mounted.state.workspace.dirty.get_untracked());
}

#[wasm_bindgen_test]
async fn startup_instructions_use_browser_filesystem_and_actual_local_tools() {
    let folder = probe_folder();
    let files = js_sys::Reflect::get(&folder, &"files".into())
        .unwrap()
        .unchecked_into::<js_sys::Map>();
    files.set(
        &"AGENTS.md".into(),
        &"Browser root instruction @shared.md".into(),
    );
    files.set(&"CLAUDE.md".into(), &"@AGENTS.md".into());
    files.set(&"shared.md".into(), &"Imported instruction".into());
    let vfs = BrowserFsaVfs::new(folder.unchecked_into());
    let mut context = openwebide_agent::context::RunContext::new(openwebide_core::RunEnvironment {
        browser_preferences: openwebide_frontend::browser_preferences::capture(),
        project_name: Some("Browser project".into()),
        mode: Some(openwebide_core::WorkspaceMode::Local),
        ..Default::default()
    });
    let text = context
        .startup(
            &vfs,
            &openwebide_agent::NoopBridgeClient,
            &local_tools(None),
        )
        .await;
    assert_eq!(text.matches("Browser root instruction").count(), 1);
    assert!(text.contains("Imported instruction"));
    assert!(text.contains("Workspace mode: local"));
    assert!(text.contains("User timezone (browser):"));
    assert!(text.contains("User locale preference (browser):"));
    assert!(!text.contains("- run_command:"));
}

#[wasm_bindgen_test]
async fn startup_environment_comes_from_authenticated_execution_bridge() {
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("context-token")
        .await
        .unwrap();
    let mounted = mount_test(|_| view! { <div /> });
    let mock = HttpGuard(fake_bridge_http());
    let credentials = BridgeCredentials::new(mounted.state.api);
    let bridge = BrowserBridgeClient::for_project(
        "http://bridge.test:3001".into(),
        "repos/x".into(),
        credentials,
    );
    let host = bridge.environment().await.unwrap();
    assert_eq!(host.os, "linux");
    assert_eq!(host.shell, "sh");
    let hardware = bridge.host_info().await.unwrap();
    assert_eq!(hardware.host_name.as_deref(), Some("bridge-host"));
    assert_eq!(hardware.logical_cores, 8);
    assert!(bridge_calls(&mock.0).contains("context-token"));
    if let Some(token) = previous {
        openwebide_frontend::idb::set_bridge_pairing_token(&token)
            .await
            .unwrap();
    } else {
        openwebide_frontend::idb::delete_bridge_pairing_token()
            .await
            .unwrap();
    }
}

#[wasm_bindgen_test]
async fn new_local_chat_uses_browser_instructions_and_selects_session() {
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("warm-token")
        .await
        .unwrap();
    let http = fake_bridge_http();
    let folder = probe_folder();
    let files = js_sys::Reflect::get(&folder, &"files".into())
        .unwrap()
        .unchecked_into::<js_sys::Map>();
    files.set(&"CLAUDE.md".into(), &"Local startup rule".into());
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.seed_connection();
        state
            .projects
            .projects
            .update(|items| items[0].mode = openwebide_core::WorkspaceMode::Local);
        state.projects.local_handles.update(|handles| {
            handles.insert(1, folder.unchecked_into());
        });
        state
            .settings
            .bridge_url
            .set("ws://bridge.test:3001".into());
        let actions = super::support::chat_actions(state);
        view! { <button on:click=move |_| actions.on_new_session.run(())>"New chat"</button> }
    });
    settle().await;
    mounted.click_text("New chat");
    for _ in 0..100 {
        sleep_ms(5).await;
        settle().await;
        if !mounted.state.chat.creating_session.get_untracked() {
            break;
        }
    }
    let session = mounted.state.chat.active_session.get_untracked().unwrap();
    assert!(
        mounted.state.chat.error.get_untracked().is_none(),
        "{:?}",
        mounted.state.chat.error.get_untracked()
    );
    assert!(
        matches!(&mounted.state.fake.messages.borrow()[&session][0], openwebide_core::ConversationEntry::Message(message) if message.content.contains("Local startup rule") && message.content.contains("Workspace mode: local"))
    );
    drop(mounted);
    restore_bridge_http(&http);
    if let Some(token) = previous {
        openwebide_frontend::idb::set_bridge_pairing_token(&token)
            .await
            .unwrap();
    } else {
        openwebide_frontend::idb::delete_bridge_pairing_token()
            .await
            .unwrap();
    }
}

#[wasm_bindgen(inline_js = r#"
export async function contractFolder() {
    const root = await navigator.storage.getDirectory();
    const name = 'vfs-contract-' + crypto.randomUUID();
    const handle = await root.getDirectoryHandle(name, {create:true});
    return {handle, cleanup: async () => root.removeEntry(name, {recursive:true})};
}
export function contractHandle(fixture) { return fixture.handle; }
export async function contractCleanup(fixture) { await fixture.cleanup(); }
"#)]
extern "C" {
    async fn contractFolder() -> JsValue;
    fn contractHandle(fixture: &JsValue) -> JsValue;
    async fn contractCleanup(fixture: &JsValue);
}

#[wasm_bindgen_test]
async fn browser_vfs_uses_shared_paths_read_limits_and_search_contract() {
    let fixture = contractFolder().await;
    let vfs = BrowserFsaVfs::new(contractHandle(&fixture).unchecked_into());
    vfs.write("src\\a.txt", "Needle\nsecond needle")
        .await
        .unwrap();
    assert_eq!(
        vfs.read("/src/./a.txt").await.unwrap(),
        "Needle\nsecond needle"
    );
    assert!(matches!(
        vfs.create("src/a.txt", openwebide_core::vfs::VfsEntryKind::File)
            .await,
        Err(openwebide_core::VfsError::AlreadyExists(_))
    ));
    for path in ["../outside", ".spin/db", "a/.spin/db"] {
        assert!(matches!(
            vfs.write(path, "x").await,
            Err(openwebide_core::VfsError::PathEscape(_))
        ));
    }
    vfs.write("node_modules/ignored", "needle").await.unwrap();
    let mut large = "x".repeat(2 * 1024 * 1024);
    large.push_str("\nneedle");
    vfs.write("large", &large).await.unwrap();
    let hits = vfs
        .search_content("NEEDLE", "", Default::default())
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().any(|hit| hit.path == "large"));
    assert!(!hits.iter().any(|hit| hit.path.starts_with("node_modules")));
    vfs.write("oversized", &"x".repeat(10 * 1024 * 1024 + 1))
        .await
        .unwrap();
    assert!(vfs.read("oversized").await.is_err());
    vfs.write(
        "src/a.txt",
        &format!("needle{}\n", "é".repeat(500)).repeat(501),
    )
    .await
    .unwrap();
    let hits = vfs
        .search_content("needle", "src", Default::default())
        .await
        .unwrap();
    assert_eq!(hits.len(), 500);
    assert!(hits.iter().all(|hit| hit.text.chars().count() == 400));
    contractCleanup(&fixture).await;
}

#[wasm_bindgen_test]
async fn browser_vfs_directory_listing_tolerates_disappearing_entries_only() {
    let vfs = BrowserFsaVfs::new(disappearingEntryFolder("NotFoundError").unchecked_into());
    let entries = vfs.list("").await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "kept.txt");
    assert_eq!(entries[0].size, 4);
    let denied = BrowserFsaVfs::new(disappearingEntryFolder("NotAllowedError").unchecked_into());
    assert!(matches!(
        denied.list("").await,
        Err(openwebide_core::VfsError::PermissionDenied(_))
    ));
}

#[wasm_bindgen_test]
async fn browser_vfs_creation_contract() {
    let fixture = contractFolder().await;
    let vfs = BrowserFsaVfs::new(contractHandle(&fixture).unchecked_into());
    openwebide_core::testing::vfs_creation_contract(&vfs).await;
    contractCleanup(&fixture).await;
}

#[wasm_bindgen_test]
async fn browser_chat_only_run_uses_shared_reply_lifecycle() {
    let mounted =
        mount_test(|state| {
            state.seed_project();
            state.seed_connection();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = openwebide_core::WorkspaceMode::Local);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, empty_read_folder().unchecked_into());
            });
            state
                .fake
                .model_setup
                .borrow_mut()
                .profiles
                .push(openwebide_core::ModelProfile {
                    selection: openwebide_core::ModelSelection {
                        server_id: 1,
                        model: "qwen3:8b".into(),
                    },
                    settings: openwebide_core::ModelSettings {
                        tools: Some(false),
                        ..Default::default()
                    },
                });
            state.fake.scripted_completions.borrow_mut().push_back(
                openwebide_core::ChatCompletion {
                    reasoning: "thinking".into(),
                    preamble: String::new(),
                    response: openwebide_core::ChatResponse::Text("reply".into()),
                    stop_reason: openwebide_core::StopReason::Length,
                    usage: Some(openwebide_core::TurnTelemetry {
                        context: None,
                        prompt_tokens: 10,
                        ..Default::default()
                    }),
                },
            );
            chat_view(state)
        });
    settle().await;
    mounted.input("hello");
    mounted.key("Enter", "Enter", false);
    for _ in 0..1000 {
        sleep_ms(5).await;
        settle().await;
        if !mounted.state.chat.streaming.get_untracked() {
            break;
        }
    }
    assert!(
        mounted.state.chat.error.get_untracked().is_none(),
        "{:?}",
        mounted.state.chat.error.get_untracked()
    );
    assert!(!mounted.state.chat.streaming.get_untracked());
    let requests = mounted.state.fake.completion_requests.borrow();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].tools.is_empty());
    assert!(
        requests[0]
            .system_prompt
            .as_ref()
            .unwrap()
            .contains("chat-only run")
    );
    let entries = mounted.state.fake.messages.borrow();
    let reply = entries[&1]
        .iter()
        .rev()
        .find_map(|entry| match entry {
            openwebide_core::ConversationEntry::Message(message)
                if message.role == openwebide_core::Role::Assistant =>
            {
                Some(message)
            }
            _ => None,
        })
        .unwrap();
    assert!(reply.content.contains("thinking") && reply.content.contains("reply"));
    assert!(
        reply
            .content
            .ends_with(openwebide_core::REPLY_CUT_OFF_MARKER)
    );
    assert_eq!(reply.usage.unwrap().prompt_tokens, 10);
    let context = reply.usage.unwrap().context.unwrap();
    assert_eq!(context.total(), 10);
    assert!(context.system > 0);
}

#[wasm_bindgen_test]
async fn browser_chat_and_agent_compact_before_reply_and_keep_history() {
    use openwebide_core::{
        ChatCompletion, ChatMessage, ChatResponse, ConversationEntry, ModelProfile, ModelSelection,
        ModelSettings, Role, StopReason,
    };
    for tools in [false, true] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_connection();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = openwebide_core::WorkspaceMode::Local);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, empty_read_folder().unchecked_into());
            });
            state
                .fake
                .model_setup
                .borrow_mut()
                .profiles
                .push(ModelProfile {
                    selection: ModelSelection {
                        server_id: 1,
                        model: "qwen3:8b".into(),
                    },
                    settings: ModelSettings {
                        context_limit: Some(32768),
                        tools: Some(tools),
                        ..Default::default()
                    },
                });
            state.fake.messages.borrow_mut().insert(
                1,
                vec![ConversationEntry::Message(ChatMessage {
                    id: 1,
                    session_id: 1,
                    role: Role::Assistant,
                    content: "original history ".repeat(16000),
                    created_at: 0,
                    usage: None,
                    tool_calls: None,
                    tool_call_id: None,
                })],
            );
            *state.fake.background_completion.borrow_mut() = Some(Ok(ChatCompletion {
                reasoning: String::new(),
                preamble: String::new(),
                response: ChatResponse::Text(
                    "Previous work: checked the file. Continue the task.".into(),
                ),
                stop_reason: StopReason::Complete,
                usage: None,
            }));
            state
                .fake
                .scripted_completions
                .borrow_mut()
                .push_back(ChatCompletion {
                    reasoning: String::new(),
                    preamble: String::new(),
                    response: ChatResponse::Text("finished".into()),
                    stop_reason: StopReason::Complete,
                    usage: None,
                });
            chat_view(state)
        });
        settle().await;
        mounted.input("continue fixing the test");
        mounted.key("Enter", "Enter", false);
        for _ in 0..1000 {
            sleep_ms(5).await;
            settle().await;
            if !mounted.state.chat.streaming.get_untracked() {
                break;
            }
        }
        assert!(
            mounted.state.chat.error.get_untracked().is_none(),
            "{:?}",
            mounted.state.chat.error.get_untracked()
        );
        assert!(!mounted.state.chat.streaming.get_untracked());
        let entries = mounted.state.fake.messages.borrow();
        let messages = entries[&1]
            .iter()
            .filter_map(|entry| match entry {
                ConversationEntry::Message(message) => Some(message),
                ConversationEntry::ToolStep(_) | ConversationEntry::Task(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(messages[0].content, "original history ".repeat(16000));
        let saved = messages
            .iter()
            .find(|message| openwebide_core::Compaction::parse(&message.content).is_some())
            .unwrap();
        let summary = openwebide_core::Compaction::parse(&saved.content).unwrap();
        assert_eq!(summary.retained[0].content, "continue fixing the test");
        assert!(
            messages
                .iter()
                .any(|message| message.id > saved.id && message.content == "finished")
        );
        assert!(mounted.state.fake.background_requests.borrow().len() > 1);
        let requests = mounted.state.fake.completion_requests.borrow();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].messages[0]
                .content
                .contains("Previous conversation summary")
        );
        assert_eq!(requests[0].messages[1].content, "continue fixing the test");
    }
}

#[wasm_bindgen_test]
async fn rewind_uses_the_same_workspace_contract_in_both_modes() {
    use openwebide_frontend::workspace::Workspace;
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let fixture = contractFolder().await;
        let handle = contractHandle(&fixture);
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            view! { <div/> }
        });
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        openwebide_core::testing::rewind_contract(&files).await;
        openwebide_core::testing::review_contract(&files).await;
        drop(mounted);
        contractCleanup(&fixture).await;
    }
}

#[wasm_bindgen_test]
async fn rewind_restores_files_conversation_and_prompt_in_both_modes() {
    use openwebide_core::{ChatMessage, ConversationEntry, FileDiff, Role, ToolStep};
    use openwebide_frontend::{components::ConfirmDialog, workspace::Workspace};
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let fixture = contractFolder().await;
        let handle = contractHandle(&fixture);
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            let message = |id, role, content: &str| {
                ConversationEntry::Message(ChatMessage {
                    id,
                    session_id: 1,
                    role,
                    content: content.into(),
                    created_at: 0,
                    tool_calls: None,
                    tool_call_id: None,
                    usage: None,
                })
            };
            state.fake.messages.borrow_mut().insert(
                1,
                vec![
                    message(1, Role::User, "old prompt"),
                    message(2, Role::Assistant, "old reply"),
                    message(3, Role::User, "change a file"),
                    ConversationEntry::ToolStep(ToolStep {
                        timing: None,
                        tool_call_id: "write".into(),
                        name: "write_file".into(),
                        summary: "file.txt".into(),
                        ok: Some(true),
                        result_summary: Some("done".into()),
                        diff: Some(FileDiff {
                            path: "file.txt".into(),
                            old: Some("before".into()),
                            new: "after".into(),
                            old_unavailable: false,
                            backup_path: None,
                        }),
                        anchor_message_id: 3,
                        checkpoint: Some(openwebide_core::rewind::ProjectCheckpoint {
                            before: [("file.txt".into(), "YmVmb3Jl".into())].into(),
                            after: Some([("file.txt".into(), "YWZ0ZXI=".into())].into()),
                            skipped: [("large.bin".into(), "file exceeds 10 MiB".into())].into(),
                        }),
                    }),
                    message(4, Role::Assistant, "edited"),
                ],
            );
            view! { {chat_view(state)} <ConfirmDialog/> }
        });
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        files.write("file.txt", "after").await.unwrap();
        files
            .write("large.bin", "keep current contents")
            .await
            .unwrap();
        settle().await;
        super::support::click_action(&mounted, ".tui-rewind[data-message-id='3']").await;
        settle().await;
        assert_eq!(files.read("file.txt").await.unwrap(), "after");
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("large.bin: file exceeds 10 MiB")
        );
        mounted.click(".modal-footer .danger");
        for _ in 0..100 {
            sleep_ms(5).await;
            if !mounted.state.chat.rewinding.get_untracked() {
                break;
            }
        }
        settle().await;
        assert_eq!(files.read("file.txt").await.unwrap(), "before");
        assert_eq!(
            files.read("large.bin").await.unwrap(),
            "keep current contents"
        );
        assert_eq!(mounted.state.chat.draft.get_untracked(), "change a file");
        assert!(!mounted.state.chat.rewinding.get_untracked());
        assert_eq!(mounted.state.fake.messages.borrow()[&1].len(), 2);
        assert!(!mounted.root.text_content().unwrap().contains("edited"));
        drop(mounted);
        contractCleanup(&fixture).await;
    }
}

#[wasm_bindgen_test]
async fn rewind_confirmation_cannot_change_another_session_project_or_account() {
    use openwebide_core::{ChatMessage, ConversationEntry, Role};
    use openwebide_frontend::components::ConfirmDialog;
    for change in ["session", "project", "account", "dirty"] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_session();
            state.fake.messages.borrow_mut().insert(
                1,
                vec![ConversationEntry::Message(ChatMessage {
                    id: 1,
                    session_id: 1,
                    role: Role::User,
                    content: "original prompt".into(),
                    created_at: 0,
                    tool_calls: None,
                    tool_call_id: None,
                    usage: None,
                })],
            );
            view! { {chat_view(state)} <ConfirmDialog/> }
        });
        settle().await;
        super::support::click_action(&mounted, ".tui-rewind").await;
        settle().await;
        match change {
            "session" => mounted.state.chat.active_session.set(Some(2)),
            "project" => mounted.state.projects.active_project.set(Some(2)),
            "account" => mounted.state.auth.generation.update(|value| *value += 1),
            "dirty" => mounted.state.workspace.dirty.set(true),
            _ => unreachable!(),
        }
        mounted.click(".modal-footer .danger");
        settle().await;
        assert!(mounted.state.fake.rewinds.borrow().is_empty());
        assert_eq!(mounted.state.fake.messages.borrow()[&1].len(), 1);
        assert!(!mounted.state.chat.rewinding.get_untracked());
    }
}

#[wasm_bindgen_test]
async fn browser_project_checkpoint_contract() {
    let fixture = contractFolder().await;
    let vfs = BrowserFsaVfs::new(contractHandle(&fixture).unchecked_into());
    openwebide_core::testing::project_checkpoint_contract(&vfs).await;
    openwebide_core::testing::checkpoint_coverage_contract(&vfs).await;
    contractCleanup(&fixture).await;
}

#[wasm_bindgen_test]
async fn run_changes_review_hunks_and_editor_markers_in_both_modes() {
    use openwebide_core::{EditDecision, PersistedEdit, RewindFile, RunChange};
    use openwebide_frontend::{components::RunChangesPanel, workspace::Workspace};
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let fixture = contractFolder().await;
        let handle = contractHandle(&fixture);
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            let record = RunChange::new(
                1,
                3,
                RewindFile::from_bytes(
                    "review.txt".into(),
                    Some(b"one\r\nkeep\nthree".to_vec()),
                    Some(b"ONE\r\nkeep\nTHREE\n".to_vec()),
                ),
            )
            .unwrap();
            let file = record.pending_file().unwrap();
            let edit = PersistedEdit {
                project_id: 1,
                path: file.path.clone(),
                revision: 1,
                decision: EditDecision::Pending,
                diff: file.preview(),
                file: Some(file),
            };
            state
                .fake
                .persisted_edits
                .borrow_mut()
                .insert((1, edit.path.clone()), edit.clone());
            state.fake.run_changes.borrow_mut().insert(1, vec![record]);
            state.workspace.set_persisted_edits(1, vec![edit]);
            state.workspace.open_file.set(Some("review.txt".into()));
            state.workspace.content.set("ONE\r\nkeep\nTHREE\n".into());
            let editor = super::support::editor_view(state);
            view! { {editor} <RunChangesPanel message=3/> }
        });
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        files
            .write("review.txt", "ONE\r\nkeep\nTHREE\n")
            .await
            .unwrap();
        settle().await;
        mounted.click_text("Inline");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".editor-diff-inline .diff-line.add[data-line='1']")
                .unwrap()
                .is_some()
        );
        assert!(
            mounted
                .root
                .query_selector(".editor-diff-inline .diff-line.add[data-line='3']")
                .unwrap()
                .is_some()
        );
        mounted.click(".run-change-hunk[data-hunk='0'] .approve");
        for _ in 0..50 {
            sleep_ms(5).await;
            if mounted.state.fake.run_changes.borrow()[&1][0].revision == 2 {
                break;
            }
        }
        settle().await;
        assert_eq!(
            files.read("review.txt").await.unwrap(),
            "ONE\r\nkeep\nTHREE\n"
        );
        assert!(
            mounted
                .root
                .query_selector(".editor-diff-inline .diff-line.add[data-line='1']")
                .unwrap()
                .is_none()
        );
        assert!(
            mounted
                .root
                .query_selector(".editor-diff-inline .diff-line.add[data-line='3']")
                .unwrap()
                .is_some()
        );
        mounted.click(".run-change-hunk[data-hunk='1'] .deny");
        settle().await;
        assert_eq!(
            files.read("review.txt").await.unwrap(),
            "ONE\r\nkeep\nTHREE\n"
        );
        mounted.click(".modal-footer .danger");
        for _ in 0..50 {
            sleep_ms(5).await;
            if mounted.state.fake.run_changes.borrow()[&1][0].revision == 3 {
                break;
            }
        }
        settle().await;
        assert_eq!(
            files.read("review.txt").await.unwrap(),
            "ONE\r\nkeep\nthree"
        );
        assert_eq!(
            mounted.state.workspace.content.get_untracked(),
            "ONE\r\nkeep\nthree"
        );
        assert!(
            !mounted
                .state
                .workspace
                .pending_edits
                .get_untracked()
                .contains_key("review.txt")
        );
        assert!(mounted.root.text_content().unwrap().contains("Reviewed"));
        files.delete("review.txt").await.unwrap();
        drop(mounted);
        contractCleanup(&fixture).await;
    }
}

#[wasm_bindgen_test]
async fn run_review_preserves_manual_edits_and_ignores_a_previous_account() {
    use openwebide_core::{EditDecision, ReviewRequest, RewindFile, RunChange};
    use openwebide_frontend::{components::RunChangesPanel, workspace::Workspace};
    let record = RunChange::new(
        1,
        3,
        RewindFile::from_bytes(
            "review.txt".into(),
            Some(b"before".to_vec()),
            Some(b"after".to_vec()),
        ),
    )
    .unwrap();
    let request = ReviewRequest {
        session_id: 1,
        message_id: 3,
        path: "review.txt".into(),
        revision: 1,
        decision: EditDecision::Accepted,
        hunk: None,
    };
    let plan = record.prepare(request).unwrap();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.seed_session();
        state.fake.run_changes.borrow_mut().insert(1, vec![record]);
        let editor = super::support::editor_view(state);
        view! { {editor} <RunChangesPanel message=3/> }
    });
    let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
    files.write("review.txt", "manual").await.unwrap();
    settle().await;
    mounted.click(".run-change-header .deny");
    settle().await;
    mounted.click(".modal-footer .danger");
    settle().await;
    assert_eq!(files.read("review.txt").await.unwrap(), "manual");
    assert!(mounted.state.fake.reviews.borrow().is_empty());
    files.write("review.txt", "after").await.unwrap();
    let (sender, receiver) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .review_results
        .borrow_mut()
        .push_back(receiver);
    mounted.click(".run-change-header .approve");
    settle().await;
    mounted.state.auth.generation.update(|value| *value += 1);
    settle().await;
    sender.send(Ok(plan)).unwrap();
    settle().await;
    assert!(mounted.state.fake.reviews.borrow().is_empty());
    assert_eq!(mounted.state.fake.run_changes.borrow()[&1][0].revision, 1);
    assert_eq!(files.read("review.txt").await.unwrap(), "after");
}

#[wasm_bindgen_test]
async fn prompt_mentions_capture_files_folders_and_git_diff_in_both_modes() {
    use openwebide_core::{
        PromptContent,
        prompt::{attach, complete},
    };
    use openwebide_frontend::{
        project_git::ProjectGit, prompt::ProjectPromptSource, workspace::Workspace,
    };
    let previous_token = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("prompt-test-token")
        .await
        .unwrap();
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let http = HttpGuard(fake_bridge_http());
        set_bridge_diff(&http.0, "diff --git a/source.rs b/source.rs\n+new");
        let fixture = contractFolder().await;
        let handle = contractHandle(&fixture);
        let git = std::rc::Rc::new(std::cell::Cell::new(None));
        let saved_git = git.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            state
                .settings
                .bridge_url
                .set("ws://bridge.test:3001".into());
            state
                .fake
                .git_diffs
                .borrow_mut()
                .push_back(Ok("diff --git a/source.rs b/source.rs\n+new".into()));
            saved_git.set(Some(expect_context::<ProjectGit>()));
            view! { <div/> }
        });
        let files = Workspace::for_project(mounted.state.api, mounted.state.projects, 1).unwrap();
        files
            .write("src/space name.rs", "original source")
            .await
            .unwrap();
        let source = ProjectPromptSource::new(
            mounted.state.api,
            mounted.state.projects,
            git.get().unwrap(),
            Some(1),
        )
        .unwrap();
        let choices = complete(&source, "file:src/").await.unwrap();
        assert_eq!(choices[0].insertion, "@file:\"src/space name.rs\" ");
        let captured = attach(
            &source,
            PromptContent {
                text: "@file:\"src/space name.rs\" @folder:src @diff".into(),
                ..Default::default()
            },
            || true,
        )
        .await
        .unwrap();
        assert_eq!(captured.references[0].content, "original source");
        assert!(captured.references[1].content.contains("src/space name.rs"));
        assert!(captured.references[2].content.contains("+new"));
        files
            .write("src/space name.rs", "changed later")
            .await
            .unwrap();
        assert_eq!(
            PromptContent::parse(&captured.encode().unwrap())
                .unwrap()
                .references[0]
                .content,
            "original source"
        );
        assert!(
            attach(
                &source,
                PromptContent {
                    text: "@file:missing".into(),
                    ..Default::default()
                },
                || true
            )
            .await
            .is_err()
        );
        assert!(
            attach(
                &source,
                PromptContent {
                    text: "@file:src/space".into(),
                    ..Default::default()
                },
                || false
            )
            .await
            .is_err()
        );
        drop(mounted);
        contractCleanup(&fixture).await;
    }
    openwebide_frontend::idb::set_bridge_pairing_token(previous_token.as_deref().unwrap_or(""))
        .await
        .unwrap();
}

#[wasm_bindgen_test]
async fn queued_local_prompt_is_consumed_once_with_its_captured_images_and_references() {
    let http = HttpGuard(fake_bridge_http());
    bridge_found(&http.0, true);
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("queue-test-token")
        .await
        .unwrap();
    let folder = probe_folder();
    let handle = folder.clone();
    let prompt = openwebide_core::PromptContent {
        text: "queued work".into(),
        references: vec![openwebide_core::prompt::PromptReference {
            mention: openwebide_core::prompt::Mention {
                kind: openwebide_core::prompt::MentionKind::File,
                path: "a.txt".into(),
            },
            content: "immutable capture".into(),
        }],
        images: vec![
            openwebide_core::PromptImage::from_bytes("test.png".into(), b"\x89PNG\r\n\x1a\n")
                .unwrap(),
        ],
    }
    .encode()
    .unwrap();
    let content = prompt.clone();
    let mounted =
        mount_test(move |state| {
            state.seed_project();
            state.seed_connection();
            state.fake.connections.borrow_mut()[0].context_limit = Some(32768);
            state
                .settings
                .connections
                .update(|connections| connections[0].context_limit = Some(32768));

            state.seed_session();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = openwebide_core::WorkspaceMode::Local);
            state.projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
            state
                .settings
                .bridge_url
                .set("ws://bridge.test:3001".into());
            state.fake.queued_prompts.borrow_mut().insert(
                1,
                vec![openwebide_core::QueuedPrompt {
                    scheduled_task: None,
                    plugin_run: None,
                    id: 1,
                    session_id: 1,
                    revision: 1,
                    content,
                    created_at: 0,
                    guidance: false,
                }],
            );
            state.fake.scripted_completions.borrow_mut().push_back(
                openwebide_core::ChatCompletion {
                    response: openwebide_core::ChatResponse::Text("Reply".into()),
                    preamble: String::new(),
                    reasoning: String::new(),
                    stop_reason: openwebide_core::StopReason::Complete,
                    usage: None,
                },
            );
            chat_view(state)
        });
    settle().await;
    mounted.input("next draft");
    settle().await;
    mounted.click(".tui-queue-toggle");
    for _ in 0..400 {
        sleep_ms(5).await;
        settle().await;
        if !mounted.state.fake.completion_requests.borrow().is_empty()
            && !mounted.state.chat.streaming.get_untracked()
        {
            break;
        }
    }
    assert!(mounted.state.fake.queued_prompts.borrow()[&1].is_empty());
    {
        let entries = mounted.state.fake.messages.borrow();
        let users = entries[&1]
            .iter()
            .filter_map(|entry| match entry {
                openwebide_core::ConversationEntry::Message(message)
                    if message.role == openwebide_core::Role::User =>
                {
                    Some(message)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].content, prompt);
    }
    assert_eq!(mounted.state.chat.draft.get_untracked(), "next draft");
    assert!(folder_empty(&folder));
    assert_eq!(
        mounted.state.fake.completion_requests.borrow().len(),
        1,
        "queued run error: {:?}",
        mounted.state.chat.error.get_untracked()
    );
    openwebide_frontend::idb::set_bridge_pairing_token(previous.as_deref().unwrap_or(""))
        .await
        .unwrap();
}

#[wasm_bindgen_test]
async fn browser_child_task_uses_inherited_files_manual_approval_and_durable_nested_history() {
    use openwebide_core::{ChatCompletion, ChatResponse, StopReason, TaskStatus, ToolCall};
    let fixture = contractFolder().await;
    let handle: web_sys::FileSystemDirectoryHandle = contractHandle(&fixture).unchecked_into();
    let vfs = BrowserFsaVfs::new(handle.clone());
    vfs.write("child.txt", "before").await.unwrap();
    let mounted = mount_test(move |state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        state
            .projects
            .projects
            .update(|projects| projects[0].mode = openwebide_core::WorkspaceMode::Local);
        state.projects.local_handles.update(|handles| {
            handles.insert(1, handle.clone());
        });
        state
            .chat
            .set_approval_mode(1, openwebide_core::ApprovalMode::Default);
        state.fake.settings.borrow_mut().insert(
            openwebide_core::ApprovalMode::setting_key(1),
            serde_json::to_string(&openwebide_core::ApprovalMode::Default).unwrap(),
        );
        let completion = |response| ChatCompletion {
            reasoning: String::new(),
            preamble: String::new(),
            response,
            stop_reason: StopReason::Complete,
            usage: None,
        };
        // Task naming uses a separate model request; do not consume the child edit.
        *state.fake.background_completion.borrow_mut() =
            Some(Ok(completion(ChatResponse::Text("Delegated edit".into()))));
        state.fake.scripted_completions.borrow_mut().extend([
            completion(ChatResponse::ToolCalls(vec![ToolCall { id: "discover-task".into(), name: "tool_search".into(), arguments: serde_json::json!({"query":"select: task"}).to_string() }])),
            completion(ChatResponse::ToolCalls(vec![ToolCall { id: "parent-wire".into(), name: "task".into(), arguments: serde_json::json!({"tasks":[{"description":"Delegated edit","prompt":"Write child.txt with after"}]}).to_string() }])),
            completion(ChatResponse::ToolCalls(vec![ToolCall { id: "child-wire".into(), name: "write_file".into(), arguments: serde_json::json!({"path":"child.txt","content":"after"}).to_string() }])),
            completion(ChatResponse::Text("Child finished".into())),
            completion(ChatResponse::Text("Parent finished".into())),
        ]);
        chat_view(state)
    });
    settle().await;
    mounted.input("Delegate this edit");
    mounted.key("Enter", "Enter", false);
    for _ in 0..400 {
        settle().await;
        if mounted
            .root
            .query_selector(".tui-task-content .btn.approve")
            .unwrap()
            .is_some()
        {
            break;
        }
        sleep_ms(5).await;
    }
    assert_eq!(vfs.read("child.txt").await.unwrap(), "before");
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Waiting for approval"),
        "child run error: {:?}; streaming: {}; completions: {}",
        mounted.state.chat.error.get_untracked(),
        mounted.state.chat.streaming.get_untracked(),
        mounted.state.fake.completion_requests.borrow().len()
    );
    let child = mounted
        .state
        .chat
        .messages
        .get_untracked()
        .into_iter()
        .find_map(|item| match item {
            openwebide_frontend::conversation::ConversationItem::Task(task) => Some(task),
            _ => None,
        })
        .unwrap();
    assert_eq!(child.task.status, TaskStatus::WaitingForApproval);
    let (permission, _) = child.pending_permission().unwrap();
    assert!(permission.contains(".task1."));
    mounted.click(".tui-task-content .btn.approve");
    for _ in 0..1000 {
        sleep_ms(5).await;
        settle().await;
        if !mounted.state.chat.streaming.get_untracked() {
            break;
        }
    }
    assert!(
        mounted.state.chat.error.get_untracked().is_none(),
        "{:?}",
        mounted.state.chat.error.get_untracked()
    );
    assert!(!mounted.state.chat.streaming.get_untracked());
    assert_eq!(vfs.read("child.txt").await.unwrap(), "after");
    let entries = mounted.state.fake.messages.borrow()[&1].clone();
    let saved = entries
        .iter()
        .find_map(|entry| match entry {
            openwebide_core::ConversationEntry::Task(task) => Some(task),
            _ => None,
        })
        .unwrap();
    assert_eq!(saved.snapshot.task.status, TaskStatus::Completed);
    assert!(saved.snapshot.pending_permission().is_none());
    {
        let requests = mounted.state.fake.completion_requests.borrow();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[2].messages.len(), 1);
        assert_eq!(
            requests[2].messages[0].content,
            "Write child.txt with after"
        );
        assert!(
            !requests[2]
                .tools
                .iter()
                .any(|tool| tool.name == "todo_write")
        );
        assert!(
            requests[4]
                .messages
                .iter()
                .all(|message| message.content != "Write child.txt with after")
        );
    }
    assert!(
        mounted
            .state
            .fake
            .tool_sources
            .borrow()
            .get(&(1, permission))
            .copied()
            .unwrap()
    );
    drop(mounted);
    contractCleanup(&fixture).await;
}

#[wasm_bindgen_test]
async fn local_plugin_transport_uses_paired_credentials_and_propagates_host_failures() {
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("plugin-test-token")
        .await
        .unwrap();
    let http = HttpGuard(fake_bridge_http());
    let prepared = openwebide_core::plugins::testing::receipt();
    set_bridge_plugin(&http.0, &serde_json::to_string(&prepared).unwrap());
    let mounted = mount_test(|_| view! {<div/>});
    let client = openwebide_frontend::plugin_bridge::PluginBridgeClient::new(
        "http://bridge.test:3001".into(),
        BridgeCredentials::new(mounted.state.api),
    );
    assert_eq!(
        client.prepare_plugin(&prepared.source).await.unwrap(),
        prepared
    );
    let calls: Vec<serde_json::Value> = serde_json::from_str(&bridge_calls(&http.0)).unwrap();
    assert_eq!(calls[0]["path"], "/plugins/prepare");
    assert_eq!(calls[0]["authorization"], "Bearer plugin-test-token");
    assert_eq!(
        calls[0]["body"]["source"],
        serde_json::to_value(&prepared.source).unwrap()
    );
    let package = openwebide_core::plugins::testing::package();
    set_bridge_plugin(&http.0, &serde_json::to_string(&package).unwrap());
    assert_eq!(client.plugin_package(&prepared).await.unwrap(), package);
    let calls: Vec<serde_json::Value> = serde_json::from_str(&bridge_calls(&http.0)).unwrap();
    assert_eq!(calls[1]["path"], "/plugins/package");
    assert_eq!(calls[1]["authorization"], "Bearer plugin-test-token");
    assert_eq!(
        calls[1]["body"]["prepared"],
        serde_json::to_value(&prepared).unwrap()
    );
    bridge_invalid(&http.0);
    assert_eq!(
        client.prepare_plugin(&prepared.source).await.unwrap_err(),
        "preparation failed"
    );
    assert_eq!(
        client.plugin_package(&prepared).await.unwrap_err(),
        "preparation failed"
    );
    openwebide_frontend::idb::set_bridge_pairing_token(previous.as_deref().unwrap_or(""))
        .await
        .unwrap();
}

#[wasm_bindgen_test]
async fn plugins_install_and_enable_use_the_same_workflow_on_paired_and_remote_hosts() {
    use openwebide_core::{
        User, UserId, UserRole, WorkspaceMode,
        plugins::{ProjectPlugin, testing::package},
    };
    use openwebide_frontend::{
        project_plugins::ProjectPluginActions, state::plugins::PluginsState,
    };
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("plugin-contract-token")
        .await
        .unwrap();
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let http = HttpGuard(fake_bridge_http());
        set_bridge_plugin(&http.0, &serde_json::to_string(&package()).unwrap());
        let captured = std::rc::Rc::new(std::cell::Cell::new(None));
        let slot = captured.clone();
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "owner".into(),
                role: UserRole::User,
                created_at: 0,
            });
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state
                .settings
                .bridge_url
                .set("ws://bridge.test:3001".into());
            slot.set(Some((
                expect_context::<PluginsState>(),
                expect_context::<ProjectPluginActions>(),
            )));
            view! {<div/>}
        });
        settle().await;
        let (plugins, actions) = captured.get().unwrap();
        let package = package();
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(package.prepared.clone())).unwrap();
        mounted
            .state
            .fake
            .plugin_preparations
            .borrow_mut()
            .push_back(receive);
        let (package_send, package_receive) = futures::channel::oneshot::channel();
        package_send.send(Ok(package.clone())).unwrap();
        mounted
            .state
            .fake
            .plugin_packages
            .borrow_mut()
            .push_back(package_receive);
        plugins
            .repository
            .set(package.prepared.source.repository.clone());
        plugins.commit.set(package.prepared.source.commit.clone());
        plugins.path.set(package.prepared.source.path.clone());
        actions.install.run(());
        super::support::wait_until("plugin host installation", move || {
            !plugins.busy.get_untracked()
        })
        .await;
        assert!(
            plugins.error.get_untracked().is_none(),
            "{:?}",
            plugins.error.get_untracked()
        );
        assert_eq!(plugins.installations.get_untracked().len(), 1);
        assert!(plugins.installations.get_untracked()[0].default_enabled);
        assert!(plugins.project_plugins.get_untracked()[0].enabled);
        assert!(mounted.state.fake.plugin_commands.borrow().is_empty());
        set_bridge_plugin(&http.0, &serde_json::to_string(&package).unwrap());
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(package.clone())).unwrap();
        mounted
            .state
            .fake
            .plugin_packages
            .borrow_mut()
            .push_back(receive);
        let (send, receive) = futures::channel::oneshot::channel();
        send.send(Ok(vec![ProjectPlugin {
            id: 1,
            revision: 1,
            enabled: true,
            prepared: package.prepared.clone(),
        }]))
        .unwrap();
        mounted
            .state
            .fake
            .project_plugin_results
            .borrow_mut()
            .push_back(receive);
        actions
            .enable
            .run(plugins.installations.get_untracked()[0].clone());
        super::support::wait_until("plugin activation", move || !plugins.busy.get_untracked())
            .await;
        assert!(
            plugins.error.get_untracked().is_none(),
            "{:?}",
            plugins.error.get_untracked()
        );
        assert!(plugins.project_plugins.get_untracked()[0].enabled);
        let commands = mounted.state.fake.plugin_commands.borrow().clone();
        let openwebide_core::plugins::ProjectPluginCommand::Enable {
            package: enabled, ..
        } = &commands[0].1
        else {
            panic!("Expected activation")
        };
        assert_eq!(**enabled, package);
        if mode == WorkspaceMode::Local {
            assert!(mounted.state.fake.plugin_requests.borrow().is_empty());
            let calls: Vec<serde_json::Value> =
                serde_json::from_str(&bridge_calls(&http.0)).unwrap();
            assert!(calls.iter().all(|call| call["path"] != "/fs/resolve"));
            assert!(
                calls
                    .iter()
                    .filter(|c| c["path"]
                        .as_str()
                        .is_some_and(|p| p.starts_with("/plugins/")))
                    .all(|c| c["authorization"] == "Bearer plugin-contract-token")
            );
        } else {
            assert_eq!(mounted.state.fake.plugin_requests.borrow().len(), 1);
        }
        drop(commands);
        drop(mounted);
    }
    openwebide_frontend::idb::set_bridge_pairing_token(previous.as_deref().unwrap_or(""))
        .await
        .unwrap();
}
