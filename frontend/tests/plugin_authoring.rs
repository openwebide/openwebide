#![cfg(target_arch = "wasm32")]
use openwebide_frontend::local_fs::BrowserFsaVfs;
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen(inline_js = r#"
export async function pluginAuthoringFolder() {
  const root = await navigator.storage.getDirectory();
  const name = 'plugin-authoring-' + crypto.randomUUID();
  const handle = await root.getDirectoryHandle(name, {create:true});
  return {handle, cleanup: () => root.removeEntry(name, {recursive:true})};
}
export function pluginAuthoringHandle(folder) { return folder.handle; }
export async function pluginAuthoringCleanup(folder) { await folder.cleanup(); }
"#)]
extern "C" {
    async fn pluginAuthoringFolder() -> JsValue;
    fn pluginAuthoringHandle(folder: &JsValue) -> JsValue;
    async fn pluginAuthoringCleanup(folder: &JsValue);
}

#[wasm_bindgen_test]
async fn browser_plugin_authoring_runs_the_shared_workspace_contract() {
    let folder = pluginAuthoringFolder().await;
    let handle: web_sys::FileSystemDirectoryHandle =
        pluginAuthoringHandle(&folder).unchecked_into();
    let vfs = BrowserFsaVfs::new(handle);
    openwebide_core::plugins::authoring::testing::contract(&vfs).await;
    pluginAuthoringCleanup(&folder).await;
}
