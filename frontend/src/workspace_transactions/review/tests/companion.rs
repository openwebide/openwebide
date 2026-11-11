//! Browser transport/capability contract; the native HTTP/physical-folder
//! contracts independently verify the host side of these messages.
use super::*;
use crate::{
    project_host::ProjectHost,
    state::{
        auth::AuthState,
        projects::ProjectsState,
        settings::{SettingsState, Theme},
    },
};
use openwebide_core::{Project, User, UserId, UserRole};
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen(inline_js = r#"
export function nativeReviewCompanionFixture(handle) {
 const original=window.fetch,session='f'.repeat(32),identity='a'.repeat(64),proof='b'.repeat(64);
 const state={requests:[],sequence:0,closed:0};
 const response=body=>new Response(JSON.stringify(body),{headers:{'Content-Type':'application/json'}});
 window.fetch=async(input,init)=> {
   const request=new Request(input,init),url=new URL(request.url),body=await request.json();
   if(url.origin!=='http://127.0.0.1:1')throw new Error('Unexpected origin: '+url.origin);
   if(url.pathname==='/exec') {
     if(body.command.startsWith('test -f '))return response({exit_code:0,stdout:'',stderr:''});
     const marker=body.command.match(/\.openwebide-probe-[a-f0-9]{16}/)?.[0];
     if(!marker)throw new Error('Unexpected discovery command');
     return response({exit_code:0,stdout:'./fixture/'+marker+'\n',stderr:''});
   }
   if(url.pathname!=='/workspace/files')throw new Error('Unexpected path: '+url.pathname);
   state.requests.push(body);
   if(body.user!==1 || body.project!==1)throw new Error('Incorrect scope');
   const action=body.action;
   if(action.operation==='open') {
     if(action.path!=='fixture' || action.expected!==null)throw new Error('Incorrect discovery binding');
     const challenge='.openwebide-proof-'+session;
     const file=await handle.getFileHandle(challenge,{create:true}),writer=await file.createWritable();
     await writer.write(proof);await writer.close();
     return response({result:'opened',session,challenge,identity});
   }
   if(action.session!==session)throw new Error('Incorrect session');
   if(action.operation==='prove') {
     if(action.proof!==proof)throw new Error('Incorrect physical challenge');
     await handle.removeEntry('.openwebide-proof-'+session);
     return response({result:'ready',identity,worker:{id:'c'.repeat(32),secret_digest:'d'.repeat(64)}});
   }
   if(action.operation==='close') {state.closed++;return response({result:'closed'});}
   if(action.operation!=='file' || action.sequence!==++state.sequence)throw new Error('Incorrect cursor');
   const file=action.action;
   let value;
   if(file.operation==='canonicalize')value={kind:'path',value:file.path};
   else if(file.operation==='list') {
     if(file.path!=='')throw new Error('Unexpected directory');
     value={kind:'entries',value:['original.rs','unopened.rs'].map(path=>({path,name:path,is_dir:false,size:8}))};
   } else if(file.operation==='read') {
     if(!['original.rs','unopened.rs'].includes(file.path))throw new Error('Missing file');
     value={kind:'bytes',value:btoa(file.path==='original.rs'?'original':'unopened')};
   } else throw new Error('A review attempted a native mutation: '+file.operation);
   return response({result:'file',outcome:{Ok:value}});
 };
 state.restore=()=>window.fetch=original;
 return state;
}
export function nativeReviewCompanionRequests(state) {return JSON.stringify(state.requests);}
export function nativeReviewCompanionClosed(state) {return state.closed;}
export function nativeReviewCompanionRestore(state) {state.restore();}
"#)]
extern "C" {
    fn nativeReviewCompanionFixture(
        handle: &web_sys::FileSystemDirectoryHandle,
    ) -> wasm_bindgen::JsValue;
    fn nativeReviewCompanionRequests(state: &wasm_bindgen::JsValue) -> String;
    fn nativeReviewCompanionClosed(state: &wasm_bindgen::JsValue) -> u32;
    fn nativeReviewCompanionRestore(state: &wasm_bindgen::JsValue);
}
struct FetchFixture(wasm_bindgen::JsValue);
impl Drop for FetchFixture {
    fn drop(&mut self) {
        nativeReviewCompanionRestore(&self.0);
    }
}
#[wasm_bindgen_test::wasm_bindgen_test]
async fn normal_local_review_proves_and_retains_the_handle_and_uses_native_reads_only() {
    crate::testing::initialize_runtime();
    let (owner, fake, api, _) = setup();
    let folder = nativeReviewTestFolder().await;
    let handle: web_sys::FileSystemDirectoryHandle =
        nativeReviewTestHandle(&folder).unchecked_into();
    let previous = crate::idb::get_bridge_pairing_token().await.unwrap();
    crate::idb::set_bridge_pairing_token("native-review-test")
        .await
        .unwrap();
    let host = owner.with(|| {
        let auth = AuthState::new();
        auth.user.set(Some(User {
            id: UserId::new(1),
            username: "owner".into(),
            role: UserRole::Admin,
            created_at: 0,
        }));
        let projects = ProjectsState::new();
        projects.projects.set(vec![Project {
            id: 1,
            name: "fixture".into(),
            path: Some("fixture".into()),
            mode: WorkspaceMode::Local,
            user_id: Some(UserId::new(1)),
            created_at: 0,
        }]);
        projects.local_handles.update(|handles| {
            handles.insert(1, handle.clone());
        });
        ProjectHost::new(
            api,
            projects,
            SettingsState::new(Theme::Dark, "ws://127.0.0.1:1".into()),
            auth,
        )
    });
    let fixture = FetchFixture(nativeReviewCompanionFixture(&handle));
    let id = "f".repeat(32);
    let executor = WorkspaceTransactionExecutor::select(
        fake.clone(),
        Workspace::Local {
            handle: handle.clone(),
        },
        1,
        1,
    )
    .unwrap()
    .with_host(Some(host));
    let files = executor
        .review_files(id.clone(), Rc::new(|| true))
        .await
        .unwrap();
    assert_eq!(files.root_identity(), "a".repeat(64));
    assert_eq!(files.id(), id);
    assert!(
        crate::idb::load_transaction_root(1, 1, &id)
            .await
            .unwrap()
            .is_none()
    );
    files.retain_root(Rc::new(|| true)).await.unwrap();
    let retained = crate::idb::load_transaction_root(1, 1, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        JsFuture::from(retained.is_same_entry(&handle))
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let operation = FileOperation::Rename {
        from: "original.rs".into(),
        to: "moved.rs".into(),
        overwrite: false,
        ignore_if_exists: false,
    };
    let mut draft = files
        .prepare(
            vec![operation.clone()],
            BTreeMap::new(),
            vec![],
            None,
            || true,
        )
        .await
        .unwrap();
    draft.resource(operation).unwrap();
    assert_eq!(
        draft
            .document(&files, "unopened.rs", || true)
            .await
            .unwrap()
            .document
            .text(),
        "unopened"
    );
    draft.finish().unwrap();
    assert_eq!(draft.plan.version, 2);
    assert_eq!(
        draft.plan.after["moved.rs"].bytes(),
        Some(b"original".as_slice())
    );
    assert!(files.write_bytes("original.rs", b"changed").await.is_err());
    files.close().await;
    let requests: Vec<serde_json::Value> =
        serde_json::from_str(&nativeReviewCompanionRequests(&fixture.0)).unwrap();
    assert_eq!(requests[0]["action"]["operation"], "open");
    assert_eq!(requests[1]["action"]["operation"], "prove");
    assert!(nativeReviewCompanionClosed(&fixture.0) >= 1);
    assert!(fake.workspace_transactions.records.borrow().is_empty());
    // An acknowledged explicit close must suppress a redundant drop request.
    drop(files);
    crate::util::sleep_ms(20).await;
    assert_eq!(nativeReviewCompanionClosed(&fixture.0), 1);
    drop(fixture);
    crate::idb::delete_transaction_root(1, 1, &id)
        .await
        .unwrap();
    if let Some(previous) = previous {
        crate::idb::set_bridge_pairing_token(&previous)
            .await
            .unwrap();
    } else {
        crate::idb::delete_bridge_pairing_token().await.unwrap();
    }
    nativeReviewTestCleanup(&folder).await;
    owner.cleanup();
}
