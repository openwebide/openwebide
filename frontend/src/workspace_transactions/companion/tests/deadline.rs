use super::*;
use futures::FutureExt;
use openwebide_core::{
    workspace_entries::WorkspaceEntries,
    workspace_transaction::{ResourceScope, TransactionFiles, companion::FileValue},
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
export function companionDeadlineFixture() {
    const originalFetch=window.fetch, originalSet=window.setTimeout, originalClear=window.clearTimeout;
    const state={calls:0,aborted:0,late:[],mode:'reply',reply:'',active:new Set(),scheduled:0,cleared:0,fired:0};
    window.setTimeout=function(callback,delay,...args) {
        if(delay!==32123 && delay!==9) return originalSet.call(window,callback,delay,...args);
        const id=originalSet.call(window,()=>{state.active.delete(id);state.fired++;callback(...args);},delay);
        state.active.add(id); state.scheduled++; return id;
    };
    window.clearTimeout=function(id) {
        if(state.active.delete(id)) state.cleared++;
        return originalClear.call(window,id);
    };
    window.fetch=function(input,init) {
        const request=new Request(input,init);
        if(request.url!=='http://127.0.0.1:1/workspace/files') return originalFetch.call(window,input,init);
        state.calls++;
        if(state.mode==='reply') return Promise.resolve(new Response(state.reply,{status:200,headers:{'Content-Type':'application/json'}}));
        const failure=new DOMException('Observer aborted','AbortError');
        if(state.mode==='body') {
            const stream=new ReadableStream({start(controller) {
                request.signal.addEventListener('abort',()=>{state.aborted++;controller.error(failure);},{once:true});
            }});
            return Promise.resolve(new Response(stream,{status:200,headers:{'Content-Type':'application/json'}}));
        }
        return new Promise((resolve,reject)=>{
            request.signal.addEventListener('abort',()=>{state.aborted++;reject(failure);},{once:true});
            state.late.push(()=>resolve(new Response(state.reply,{status:200,headers:{'Content-Type':'application/json'}})));
        });
    };
    state.restore=()=> {
        for(const id of state.active) originalClear.call(window,id);
        state.active.clear(); window.fetch=originalFetch;window.setTimeout=originalSet;window.clearTimeout=originalClear;
    };
    return state;
}
export function companionDeadlineMode(state,mode,reply) {state.mode=mode;state.reply=reply;}
export function companionDeadlineCount(state,name) {return name==='active'?state.active.size:state[name];}
export function companionDeadlineLate(state) {for(const resolve of state.late.splice(0))resolve();}
export function companionDeadlineRestore(state) {state.restore();}
"#)]
extern "C" {
    fn companionDeadlineFixture() -> JsValue;
    fn companionDeadlineMode(state: &JsValue, mode: &str, reply: &str);
    fn companionDeadlineCount(state: &JsValue, name: &str) -> u32;
    fn companionDeadlineLate(state: &JsValue);
    fn companionDeadlineRestore(state: &JsValue);
}
struct Fixture(JsValue);
impl Fixture {
    fn new() -> Self {
        Self(companionDeadlineFixture())
    }
    fn mode(&self, mode: &str, reply: &FolderReply) {
        companionDeadlineMode(&self.0, mode, &serde_json::to_string(reply).unwrap());
    }
    fn count(&self, name: &str) -> u32 {
        companionDeadlineCount(&self.0, name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        companionDeadlineRestore(&self.0);
    }
}
async fn client() -> (Owner, PluginBridgeClient, Option<String>) {
    let previous = crate::idb::get_bridge_pairing_token().await.unwrap();
    crate::idb::set_bridge_pairing_token("companion-deadline-test")
        .await
        .unwrap();
    let owner = Owner::new();
    let api: Api =
        owner.with(|| StoredValue::new_local(Rc::new(FakeBackend::default()) as Rc<dyn Backend>));
    (
        owner,
        PluginBridgeClient::new("http://127.0.0.1:1".into(), BridgeCredentials::new(api)),
        previous,
    )
}
async fn restore_token(previous: Option<String>) {
    if let Some(token) = previous {
        crate::idb::set_bridge_pairing_token(&token).await.unwrap();
    } else {
        crate::idb::delete_bridge_pairing_token().await.unwrap();
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn deadline_clears_timers_on_success_cancellation_and_expiry() {
    let fixture = Fixture::new();
    for _ in 0..64 {
        assert_eq!(
            crate::util::with_deadline(async { 42 }, 32123).await,
            Some(42)
        );
    }
    assert_eq!(fixture.count("scheduled"), 64);
    assert_eq!(fixture.count("cleared"), 64);
    assert_eq!(fixture.count("active"), 0);
    assert!(
        crate::util::with_deadline(futures::future::pending::<()>(), 32123)
            .now_or_never()
            .is_none()
    );
    assert_eq!(fixture.count("cleared"), 65);
    assert_eq!(fixture.count("active"), 0);
    let dropped = Rc::new(Cell::new(false));
    struct Dropped(Rc<Cell<bool>>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    let guard = Dropped(dropped.clone());
    assert!(
        crate::util::with_deadline(
            async move {
                let _guard = guard;
                futures::future::pending::<()>().await;
            },
            9
        )
        .await
        .is_none()
    );
    assert!(dropped.get());
    assert_eq!(fixture.count("fired"), 1);
    assert_eq!(fixture.count("active"), 0);
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn deadline_aborts_folder_and_journal_observers_including_response_bodies() {
    let fixture = Fixture::new();
    let (owner, client, previous) = client().await;
    let actions = [
        execution(),
        FolderAction::Open {
            path: String::new(),
            expected: None,
        },
        FolderAction::Prove {
            session: "a".repeat(32),
            proof: "b".repeat(64),
        },
        FolderAction::File {
            session: "a".repeat(32),
            sequence: 1,
            action: FileAction::Canonicalize {
                path: "file".into(),
            },
        },
        FolderAction::Close {
            session: "a".repeat(32),
        },
        FolderAction::Admit {
            session: "a".repeat(32),
            api_url: "http://127.0.0.1/api".into(),
            delegation: delegation(),
        },
        FolderAction::Advance {
            session: "a".repeat(32),
            request: openwebide_core::workspace_transaction::AdvanceWorkspaceTransaction {
                id: "c".repeat(32),
                generation: "d".repeat(32),
                revision: 1,
                progress: Default::default(),
            },
        },
    ];
    for (index, action) in actions.into_iter().enumerate() {
        fixture.mode(
            if index.is_multiple_of(2) {
                "fetch"
            } else {
                "body"
            },
            &FolderReply::Closed,
        );
        let request = FolderRequest {
            user: 1,
            project: 1,
            action,
        };
        let result = observe(send(&client, &request), 500).await;
        assert!(result.unwrap_err().contains("timed out"));
    }
    assert_eq!(fixture.count("calls"), 7);
    assert_eq!(fixture.count("aborted"), 7);
    restore_token(previous).await;
    owner.cleanup();
}

struct Timed(BrowserFolderTransport);
impl FolderTransport for Timed {
    async fn request(&self, request: &FolderRequest) -> Result<FolderReply, String> {
        self.0.request_with_deadline(request, 500).await
    }
}
#[wasm_bindgen_test::wasm_bindgen_test]
async fn ambiguous_complete_execution_blocks_replay_even_after_a_late_acknowledgement() {
    let fixture = Fixture::new();
    let (owner, client, previous) = client().await;
    fixture.mode(
        "reply",
        &FolderReply::File {
            outcome: Ok(FileValue::Unit),
        },
    );
    let mut files = CompanionFiles::new(
        Timed(BrowserFolderTransport {
            client,
            current: Rc::new(|| true),
            handle: handle("granted"),
            cleanup: None,
        }),
        1,
        1,
        "a".repeat(32),
        "b".repeat(64),
        openwebide_core::workspace_transaction::WorkspaceTransactionWorkerKey {
            id: "e".repeat(32),
            secret_digest: "f".repeat(64),
        },
    )
    .unwrap();
    files
        .bind(ResourceScope {
            transaction: "c".repeat(32),
            fingerprint: "d".repeat(64),
        })
        .await
        .unwrap();
    let FolderAction::Execute {
        api_url,
        delegation,
        request,
        ..
    } = execution()
    else {
        unreachable!()
    };
    fixture.mode(
        "fetch",
        &FolderReply::Executed {
            outcome: openwebide_core::workspace_transaction::WorkspaceTransactionOutcome {
                generation: request.generation.clone(),
                phase: Some(openwebide_core::workspace_transaction::TransactionPhase::Committed),
                error: None,
            },
        },
    );
    assert!(
        files
            .execute(api_url.clone(), delegation.clone(), request.clone())
            .await
            .unwrap_err()
            .contains("timed out")
    );
    companionDeadlineLate(&fixture.0);
    crate::util::sleep_ms(1).await;
    assert!(
        files
            .canonicalize("file")
            .await
            .unwrap_err()
            .contains("acknowledgement was lost")
    );
    assert!(
        files
            .execute(api_url, delegation, request)
            .await
            .unwrap_err()
            .contains("acknowledgement was lost")
    );
    assert_eq!(fixture.count("calls"), 2);
    assert_eq!(fixture.count("aborted"), 1);
    fixture.mode("reply", &FolderReply::Closed);
    files.close().await.unwrap();
    restore_token(previous).await;
    owner.cleanup();
}
#[wasm_bindgen_test::wasm_bindgen_test]
async fn ambiguous_admission_poisoning_blocks_later_journal_and_filesystem_dispatch() {
    let fixture = Fixture::new();
    let (owner, client, previous) = client().await;
    fixture.mode("fetch", &FolderReply::Admitted);
    let mut files = CompanionFiles::new(
        Timed(BrowserFolderTransport {
            client,
            current: Rc::new(|| true),
            handle: handle("granted"),
            cleanup: None,
        }),
        1,
        1,
        "a".repeat(32),
        "b".repeat(64),
        openwebide_core::workspace_transaction::WorkspaceTransactionWorkerKey {
            id: "e".repeat(32),
            secret_digest: "f".repeat(64),
        },
    )
    .unwrap();
    assert!(
        files
            .admit("http://127.0.0.1/api".into(), delegation())
            .await
            .unwrap_err()
            .contains("timed out")
    );
    let calls = fixture.count("calls");
    assert!(
        files
            .canonicalize("file")
            .await
            .unwrap_err()
            .contains("acknowledgement was lost")
    );
    assert!(
        files
            .admit("http://127.0.0.1/api".into(), delegation())
            .await
            .unwrap_err()
            .contains("acknowledgement was lost")
    );
    assert_eq!(fixture.count("calls"), calls);
    assert_eq!(fixture.count("aborted"), 1);
    restore_token(previous).await;
    owner.cleanup();
}
#[wasm_bindgen_test::wasm_bindgen_test]
async fn deadline_poisoning_prevents_more_file_calls_even_after_a_late_reply() {
    let fixture = Fixture::new();
    let (owner, client, previous) = client().await;
    fixture.mode(
        "reply",
        &FolderReply::File {
            outcome: Ok(FileValue::Unit),
        },
    );
    let mut files = CompanionFiles::new(
        Timed(BrowserFolderTransport {
            client,
            current: Rc::new(|| true),
            handle: handle("granted"),
            cleanup: None,
        }),
        1,
        1,
        "a".repeat(32),
        "b".repeat(64),
        openwebide_core::workspace_transaction::WorkspaceTransactionWorkerKey {
            id: "e".repeat(32),
            secret_digest: "f".repeat(64),
        },
    )
    .unwrap();
    let scope = ResourceScope {
        transaction: "c".repeat(32),
        fingerprint: "d".repeat(64),
    };
    files.bind(scope.clone()).await.unwrap();
    fixture.mode(
        "fetch",
        &FolderReply::File {
            outcome: Ok(FileValue::Unit),
        },
    );
    assert!(
        files
            .create_resource(
                &scope,
                0,
                "created",
                openwebide_core::vfs::VfsEntryKind::File
            )
            .await
            .unwrap_err()
            .contains("timed out")
    );
    assert_eq!(fixture.count("calls"), 2);
    assert_eq!(fixture.count("aborted"), 1);
    companionDeadlineLate(&fixture.0);
    crate::util::yield_task().await;
    assert!(
        files
            .read_bytes("created")
            .await
            .unwrap_err()
            .contains("acknowledgement was lost")
    );
    assert_eq!(fixture.count("calls"), 2);
    fixture.mode("reply", &FolderReply::Closed);
    files.close().await.unwrap();
    assert_eq!(fixture.count("calls"), 3);
    restore_token(previous).await;
    owner.cleanup();
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn deadline_also_bounds_a_stuck_browser_permission_query_before_dispatch() {
    let (owner, client, previous) = client().await;
    let fixture = Fixture::new();
    let handle = js_sys::Function::new_no_args("return {queryPermission:()=>new Promise(()=>{})};")
        .call0(&JsValue::NULL)
        .unwrap()
        .unchecked_into();
    let transport = BrowserFolderTransport {
        client,
        current: Rc::new(|| true),
        handle,
        cleanup: None,
    };
    let request = FolderRequest {
        user: 1,
        project: 1,
        action: FolderAction::File {
            session: "a".repeat(32),
            sequence: 1,
            action: FileAction::Canonicalize {
                path: "file".into(),
            },
        },
    };
    assert!(
        transport
            .request_with_deadline(&request, 9)
            .await
            .unwrap_err()
            .contains("timed out")
    );
    assert_eq!(fixture.count("calls"), 0);
    assert_eq!(fixture.count("active"), 0);
    restore_token(previous).await;
    owner.cleanup();
}
