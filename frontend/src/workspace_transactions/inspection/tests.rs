use super::*;
use crate::api::BackendApi;
use futures::FutureExt;
use leptos::prelude::*;
use openwebide_core::{FileEntry, workspace_entries::WorkspaceEntries};
use std::cell::Cell;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen(inline_js = r#"
export function folderInspectionFixture() {
    const original=window.fetch;
    const state={requests:[],reply:'',hold:false,resolve:null,signal:null};
    state.started=new Promise(resolve=>state.start=resolve);
    window.fetch=async(input,init)=> {
        const request=new Request(input,init),path=new URL(request.url).pathname;
        if(path==='/api/projects/7/workspace-transactions/root')
            return new Response(JSON.stringify({identity:'a'.repeat(64)}),{headers:{'Content-Type':'application/json'}});
        if(path!=='/api/projects/7/workspace-transactions/inspect')throw new Error('Unexpected workspace request: '+path);
        if(request.method!=='POST')throw new Error('Inspection must use POST');
        state.requests.push(await request.json()); state.signal=request.signal;state.start();
        if(state.hold)return new Promise((resolve,reject)=> {
            state.resolve=()=>resolve(new Response(state.reply,{headers:{'Content-Type':'application/json'}}));
            request.signal.addEventListener('abort',()=>reject(new DOMException('Aborted','AbortError')),{once:true});
        });
        return new Response(state.reply,{headers:{'Content-Type':'application/json'}});
    };
    state.restore=()=>window.fetch=original;
    return state;
}
export function folderInspectionMode(state,hold,reply) {state.hold=hold;state.reply=reply;state.started=new Promise(resolve=>state.start=resolve);}
export function folderInspectionStarted(state) {return state.started;}
export function folderInspectionRelease(state) {state.resolve();}
export function folderInspectionAborted(state) {return state.signal?.aborted===true;}
export function folderInspectionRequests(state) {return JSON.stringify(state.requests);}
export function folderInspectionRestore(state) {state.restore();}
"#)]
extern "C" {
    fn folderInspectionFixture() -> JsValue;
    fn folderInspectionMode(state: &JsValue, hold: bool, reply: &str);
    fn folderInspectionStarted(state: &JsValue) -> js_sys::Promise;
    fn folderInspectionRelease(state: &JsValue);
    fn folderInspectionAborted(state: &JsValue) -> bool;
    fn folderInspectionRequests(state: &JsValue) -> String;
    fn folderInspectionRestore(state: &JsValue);
}
struct Fixture(JsValue);
impl Fixture {
    fn new() -> Self {
        Self(folderInspectionFixture())
    }
    fn mode(&self, hold: bool, reply: FileValue) {
        folderInspectionMode(&self.0, hold, &serde_json::to_string(&reply).unwrap());
    }
    fn requests(&self) -> Vec<serde_json::Value> {
        serde_json::from_str(&folderInspectionRequests(&self.0)).unwrap()
    }
    async fn started(&self) {
        JsFuture::from(folderInspectionStarted(&self.0))
            .await
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        folderInspectionRestore(&self.0);
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn remote_inspection_sends_only_the_bound_root_and_read_action_without_file_tree_fallback() {
    let owner = Owner::new();
    let api = owner.with(BackendApi::from_location);
    let fixture = Fixture::new();
    let files = remote_files(Rc::new(api), 7, Rc::new(|| true))
        .await
        .unwrap();
    assert_eq!(files.root_identity(), "a".repeat(64));
    fixture.mode(false, FileValue::Path("source".into()));
    assert_eq!(files.canonicalize("source").await.unwrap(), "source");
    let private = format!(".openwebide-proof-{}", "b".repeat(32));
    fixture.mode(
        false,
        FileValue::Entries(vec![
            FileEntry {
                name: "source".into(),
                path: "source".into(),
                is_dir: false,
                size: 7,
            },
            FileEntry {
                name: private.clone(),
                path: private.clone(),
                is_dir: false,
                size: 64,
            },
        ]),
    );
    let entries = files.list("").await.unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry.name == private));
    fixture.mode(false, FileValue::Bytes(b"original".to_vec()));
    assert_eq!(files.read_bytes("source").await.unwrap(), b"original");
    assert!(files.write_bytes("source", b"changed").await.is_err());
    assert!(files.delete("source").await.is_err());
    assert!(
        files
            .create("new", openwebide_core::vfs::VfsEntryKind::File)
            .await
            .is_err()
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 3);
    for request in requests {
        assert_eq!(request["root"], "a".repeat(64));
        assert_eq!(request.as_object().unwrap().len(), 2);
        assert!(request.get("path").is_none());
        assert!(request.get("user").is_none());
    }
    owner.cleanup();
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn remote_inspection_rejects_late_results_after_context_changes_and_drops_abort_observers() {
    let owner = Owner::new();
    let api = owner.with(BackendApi::from_location);
    let fixture = Fixture::new();
    let current = Rc::new(Cell::new(true));
    let check = {
        let current = current.clone();
        Rc::new(move || current.get())
    };
    let files = remote_files(Rc::new(api), 7, check).await.unwrap();
    fixture.mode(true, FileValue::Bytes(b"late".to_vec()));
    let mut read = Box::pin(files.read_bytes("source"));
    assert!(read.as_mut().now_or_never().is_none());
    fixture.started().await;
    current.set(false);
    folderInspectionRelease(&fixture.0);
    assert!(read.await.unwrap_err().contains("changed during its read"));
    assert!(
        files
            .read_bytes("source")
            .await
            .unwrap_err()
            .contains("workspace changed")
    );
    assert_eq!(fixture.requests().len(), 1);
    current.set(true);
    fixture.mode(true, FileValue::Bytes(b"cancelled".to_vec()));
    let mut read = Box::pin(files.read_bytes("source"));
    assert!(read.as_mut().now_or_never().is_none());
    fixture.started().await;
    assert!(!folderInspectionAborted(&fixture.0));
    drop(read);
    assert!(folderInspectionAborted(&fixture.0));
    fixture.mode(false, FileValue::Bytes(b"current".to_vec()));
    assert_eq!(files.read_bytes("source").await.unwrap(), b"current");
    assert_eq!(fixture.requests().len(), 3);
    owner.cleanup();
}
