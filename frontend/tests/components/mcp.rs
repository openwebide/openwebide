use super::support::{mount_test, settle};
use leptos::prelude::*;
use openwebide_agent::mcp::Transport;
use openwebide_core::ToolCall;
use openwebide_frontend::{
    bridge::BridgeCredentials, plugin_bridge::PluginBridgeClient, testing::McpBridge,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::*;

#[wasm_bindgen(inline_js = r#"
export function installMcpHttp() {
 const original = window.fetch;
 const mock = {calls: [], fail: false, restore: () => { window.fetch = original; }};
 window.fetch = async request => {
  if (!request.url.startsWith('http://mcp.test/')) return original(request);
  const path = new URL(request.url).pathname;
  const body = JSON.parse(await request.text());
  mock.calls.push({path, body, auth: request.headers.get('Authorization')});
  if (mock.fail) return new Response(JSON.stringify({error:'MCP catalog closed'}), {status:400});
  if (path === '/mcp/open') return new Response(JSON.stringify({id:'fixture', tools:[{server:'docs',name:'echo',definition:{name:'mcp__docs__echo',description:'Echo',parameters:{type:'object'}}}],failures:[]}));
  if (path === '/mcp/call') return new Response(JSON.stringify({ok:false,content:'tool error',summary:'MCP docs: echo'}));
  return new Response('{}');
 };
 return mock;
}
export function mcpCalls(mock) { return JSON.stringify(mock.calls); }
export function failMcp(mock) { mock.fail = true; }
export function restoreMcp(mock) { mock.restore(); }
"#)]
extern "C" {
    #[wasm_bindgen(js_name=installMcpHttp)]
    fn install_mcp_http() -> JsValue;
    #[wasm_bindgen(js_name=mcpCalls)]
    fn calls(mock: &JsValue) -> String;
    #[wasm_bindgen(js_name=failMcp)]
    fn fail(mock: &JsValue);
    #[wasm_bindgen(js_name=restoreMcp)]
    fn restore(mock: &JsValue);
}
struct Mock(JsValue);
impl Drop for Mock {
    fn drop(&mut self) {
        restore(&self.0);
    }
}

#[wasm_bindgen_test]
async fn browser_mcp_adapter_pins_catalog_authenticates_and_closes_last_lease() {
    let old_token = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .ok()
        .flatten();
    openwebide_frontend::idb::delete_bridge_pairing_token()
        .await
        .ok();
    let mock = Mock(install_mcp_http());
    let mounted = mount_test(|state| {
        *state.fake.bridge_credential.borrow_mut() = Some(("mcp-token".into(), i64::MAX));
        view! { <div>"MCP fixture"</div> }
    });
    let client = PluginBridgeClient::new(
        "http://mcp.test".into(),
        BridgeCredentials::new(mounted.state.api),
    );
    let (transport, catalog) = McpBridge::open(Some(client), Some("repos/project".into())).await;
    assert_eq!(catalog.tools[0].definition.name, "mcp__docs__echo");
    let call = ToolCall {
        id: "call".into(),
        name: catalog.tools[0].definition.name.clone(),
        arguments: "{}".into(),
    };
    let result = transport.invoke(&catalog, &call).await.unwrap();
    assert!(!result.ok);
    assert_eq!(result.content, "tool error");
    let clone = transport.clone();
    drop(transport);
    settle().await;
    let requests: Vec<serde_json::Value> = serde_json::from_str(&calls(&mock.0)).unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["body"]["cwd"], "repos/project");
    assert_eq!(requests[1]["body"]["catalog"]["id"], "fixture");
    assert!(
        requests
            .iter()
            .all(|request| request["auth"] == "Bearer mcp-token")
    );
    fail(&mock.0);
    assert_eq!(
        clone.invoke(&catalog, &call).await.unwrap_err(),
        "MCP catalog closed"
    );
    drop(clone);
    for _ in 0..100 {
        let requests: Vec<serde_json::Value> = serde_json::from_str(&calls(&mock.0)).unwrap();
        if requests
            .last()
            .is_some_and(|request| request["path"] == "/mcp/close")
        {
            break;
        }
        openwebide_frontend::util::sleep_ms(10).await;
    }
    let requests: Vec<serde_json::Value> = serde_json::from_str(&calls(&mock.0)).unwrap();
    assert_eq!(requests.last().unwrap()["path"], "/mcp/close");
    assert_eq!(requests.last().unwrap()["body"]["id"], "fixture");
    let (transport, catalog) = McpBridge::open(None, None).await;
    assert!(catalog.tools.is_empty());
    assert!(transport.invoke(&catalog, &call).await.is_err());
    if let Some(token) = old_token {
        openwebide_frontend::idb::set_bridge_pairing_token(&token)
            .await
            .ok();
    } else {
        openwebide_frontend::idb::delete_bridge_pairing_token()
            .await
            .ok();
    }
}
