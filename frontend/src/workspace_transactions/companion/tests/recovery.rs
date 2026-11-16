use super::*;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
export async function recoveryFolderFixture(mode) {
 const original=window.fetch;
 const state={mode,exec:0,actions:[],proofReads:0};
 state.parent=await navigator.storage.getDirectory();
 state.name='recovery-folder-'+crypto.randomUUID();
 state.handle=await state.parent.getDirectoryHandle(state.name,{create:true});
 if(mode==='revoked')state.handle.queryPermission=async()=> 'denied';
 const proofName='.openwebide-proof-'+'a'.repeat(32);
 const file=await state.handle.getFileHandle(proofName,{create:true}),writer=await file.createWritable();
 await writer.write('b'.repeat(64));await writer.close();
 const getFile=state.handle.getFileHandle;
 state.handle.getFileHandle=async function(name,options){
  if(name===proofName)state.proofReads++;
  return getFile.call(this,name,options);
 };
 window.fetch=async function(input,init) {
  const request=new Request(input,init);
  if(!request.url.startsWith('http://127.0.0.1:1/'))return original.call(window,input,init);
  if(new URL(request.url).pathname!=='/workspace/files') {
   state.exec++;
   return new Response(JSON.stringify({error:'Recover the original native workspace operation before changing this folder'}),{status:403});
  }
  const data=await request.json(),action=data.action;state.actions.push(action);
  let reply;
  if(action.operation==='open') reply={result:'opened',session:'a'.repeat(32),challenge:'.openwebide-proof-'+ 'a'.repeat(32),identity:(mode==='replacement'?'f':'c').repeat(64)};
  else if(action.operation==='prove') reply={result:'ready',identity:'c'.repeat(64),worker:{id:'d'.repeat(32),secret_digest:'e'.repeat(64)}};
  else if(action.operation==='close')reply={result:'closed'};
  else throw new Error('Unexpected recovery operation '+action.operation);
  return new Response(JSON.stringify(reply),{status:200,headers:{'Content-Type':'application/json'}});
 };
 state.restore=()=>{window.fetch=original;};return state;
}
export function recoveryFolderHandle(state){return state.handle;}
export function recoveryFolderCalls(state){return JSON.stringify({exec:state.exec,actions:state.actions,proofReads:state.proofReads});}
export function recoveryFolderRestore(state){state.restore();}
export async function recoveryFolderCleanup(state){await state.parent.removeEntry(state.name,{recursive:true});}
"#)]
extern "C" {
    async fn recoveryFolderFixture(mode: &str) -> JsValue;
    fn recoveryFolderHandle(state: &JsValue) -> JsValue;
    fn recoveryFolderCalls(state: &JsValue) -> String;
    fn recoveryFolderRestore(state: &JsValue);
    async fn recoveryFolderCleanup(state: &JsValue);
}
struct Fixture(JsValue);
impl Drop for Fixture {
    fn drop(&mut self) {
        recoveryFolderRestore(&self.0);
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn original_folder_recovery_proves_its_hint_without_using_fenced_ordinary_commands() {
    crate::testing::initialize_runtime();
    for case in ["original", "replacement", "superseded", "revoked"] {
        let previous = crate::idb::get_bridge_pairing_token().await.unwrap();
        crate::idb::set_bridge_pairing_token("recovery-folder-test")
            .await
            .unwrap();
        let fixture = Fixture(recoveryFolderFixture(case).await);
        let owner = Owner::new();
        let fake = Rc::new(FakeBackend::default());
        fake.settings
            .borrow_mut()
            .insert("local_bridge_cwd.1".into(), "original/project".into());
        let api: Api = owner.with(|| StoredValue::new_local(fake.clone() as Rc<dyn Backend>));
        let checks = Rc::new(Cell::new(0));
        let current: Rc<dyn Fn() -> bool> = {
            let checks = checks.clone();
            Rc::new(move || {
                let count = checks.get();
                checks.set(count + 1);
                case != "superseded" || count < 3
            })
        };
        let result = open(
            api,
            BridgeConfig::new("ws://127.0.0.1:1"),
            1,
            1,
            recoveryFolderHandle(&fixture.0).unchecked_into(),
            Some("c".repeat(64)),
            current,
        )
        .await;
        let (succeeded, error) = match result {
            Ok(files) => {
                files.close().await.unwrap();
                (true, None)
            }
            Err(error) => (false, Some(error)),
        };
        let calls: serde_json::Value =
            serde_json::from_str(&recoveryFolderCalls(&fixture.0)).unwrap();
        recoveryFolderCleanup(&fixture.0).await;
        drop(fixture);
        if let Some(previous) = previous {
            crate::idb::set_bridge_pairing_token(&previous)
                .await
                .unwrap();
        } else {
            crate::idb::delete_bridge_pairing_token().await.unwrap();
        }
        owner.cleanup();
        assert_eq!(
            calls["exec"], 0,
            "Recovery attempted an ordinary fenced command: {error:?}"
        );
        let actions = calls["actions"].as_array().unwrap();
        match case {
            "original" => {
                assert!(succeeded, "{error:?}");
                assert_eq!(actions[0]["path"], "original/project");
                assert_eq!(actions[0]["expected"], "c".repeat(64));
                assert_eq!(actions[1]["operation"], "prove");
                assert_eq!(actions[1]["proof"], "b".repeat(64));
                assert_eq!(calls["proofReads"], 1);
            }
            "replacement" => {
                assert!(!succeeded);
                assert!(
                    error
                        .unwrap()
                        .contains("does not match the original folder")
                );
                assert_eq!(actions.len(), 1);
                assert_eq!(calls["proofReads"], 0);
            }
            _ => {
                assert!(!succeeded);
                assert!(
                    actions.is_empty(),
                    "A superseded folder review dispatched native work"
                );
            }
        }
    }
}
