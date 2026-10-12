#![cfg(target_arch = "wasm32")]
use leptos::prelude::*;
use openwebide_core::plugins::panels::*;
use openwebide_frontend::components::plugin_panel_view::{PanelSubmission, PluginPanelDocument};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn panel_renderer_escapes_text_and_submits_typed_values_with_routing_data() {
    let document = web_sys::window().unwrap().document().unwrap();
    let root: web_sys::HtmlElement = document.create_element("div").unwrap().unchecked_into();
    document.body().unwrap().append_child(&root).unwrap();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<PanelSubmission>));
    let result = captured.clone();
    let edits = std::sync::Arc::new(std::sync::Mutex::new(0u32));
    let edit_counter = edits.clone();
    let handle = leptos::mount::mount_to(root.clone(), move || {
        let panel: PanelDefinition = serde_json::from_value(serde_json::json!({
            "id":"reviews", "title":"Reviews", "icon":"git_pull_request", "view":"render",
            "actions":[{"id":"publish","handler":"publish","title":"Publish review","requires_approval":true}]
        })).unwrap();
        let doc: PanelDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"revision":1,"blocks":[
                {"type":"text","text":"<img src=x onerror=alert(1)>"},
                {"type":"form","id":"review","fields":[
                    {"id":"body","label":"Review body","kind":{"type":"text","multiline":true,"max_length":100},"required":true},
                    {"id":"approved","label":"Approve","kind":{"type":"checkbox"},"required":true}
                ],"submit":{"action":"publish","label":"Publish","values":{"number":42}}}
            ]
        })).unwrap();
        view! { <PluginPanelDocument panel document=doc busy=Signal::derive(|| false)
        on_action=Callback::new(move |submission| *result.lock().unwrap()=Some(submission)) on_edit=Callback::new(move |()|*edit_counter.lock().unwrap()+=1)/> }
    });
    assert!(root.query_selector("img").unwrap().is_none());
    assert!(root.text_content().unwrap().contains("<img src=x"));
    let textarea: web_sys::HtmlTextAreaElement = root
        .query_selector("textarea")
        .unwrap()
        .unwrap()
        .unchecked_into();
    assert!(textarea.required());
    assert_eq!(textarea.max_length(), 100);
    textarea.set_value("Looks good");
    textarea
        .dispatch_event(&web_sys::Event::new("input").unwrap())
        .unwrap();
    let checkbox: web_sys::HtmlInputElement = root
        .query_selector("input[type=checkbox]")
        .unwrap()
        .unwrap()
        .unchecked_into();
    assert!(checkbox.required());
    checkbox.set_checked(true);
    checkbox
        .dispatch_event(&web_sys::Event::new("change").unwrap())
        .unwrap();
    root.query_selector("form")
        .unwrap()
        .unwrap()
        .dispatch_event(&web_sys::Event::new("submit").unwrap())
        .unwrap();
    assert_eq!(*edits.lock().unwrap(), 2);
    let actual = captured.lock().unwrap().take().unwrap();
    assert_eq!(actual.action, "publish");
    assert_eq!(actual.values["number"], serde_json::json!(42));
    assert_eq!(actual.values["body"], serde_json::json!("Looks good"));
    assert_eq!(actual.values["approved"], serde_json::json!(true));
    drop(handle);
    root.remove();
}

#[wasm_bindgen_test]
fn panel_renderer_rejects_unsafe_document_before_mounting_controls() {
    let document = web_sys::window().unwrap().document().unwrap();
    let root: web_sys::HtmlElement = document.create_element("div").unwrap().unchecked_into();
    document.body().unwrap().append_child(&root).unwrap();
    let handle = leptos::mount::mount_to(root.clone(), || {
        let panel: PanelDefinition = serde_json::from_value(serde_json::json!({
            "id":"reviews","title":"Reviews","icon":"git_pull_request","view":"render"
        }))
        .unwrap();
        let doc: PanelDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"revision":1,"blocks":[{"type":"link","text":"Unsafe","url":"javascript:alert(1)"}]
        })).unwrap();
        view! { <PluginPanelDocument panel document=doc busy=Signal::derive(|| false) on_action=Callback::new(|_| panic!("Invalid document invoked an action"))/> }
    });
    assert!(root.query_selector("a").unwrap().is_none());
    assert!(root.query_selector("[role=alert]").unwrap().is_some());
    drop(handle);
    root.remove();
}

#[wasm_bindgen_test]
fn panel_secret_control_starts_empty_and_uses_native_password_input() {
    let document = web_sys::window().unwrap().document().unwrap();
    let root: web_sys::HtmlElement = document.create_element("div").unwrap().unchecked_into();
    document.body().unwrap().append_child(&root).unwrap();
    let handle = leptos::mount::mount_to(root.clone(), || {
        let panel: PanelDefinition=serde_json::from_value(serde_json::json!({
            "id":"reviews","title":"Reviews","icon":"git_pull_request","view":"render",
            "actions":[{"id":"configure","handler":"configure","title":"Save configuration","requires_approval":false}]
        })).unwrap();
        let view: PanelDocument=serde_json::from_value(serde_json::json!({
            "schema_version":1,"revision":1,"blocks":[{"type":"form","id":"configuration",
            "fields":[{"id":"token","label":"GitHub token","kind":{"type":"secret","max_length":1024},"required":true}],
            "submit":{"action":"configure","label":"Save"}}]
        })).unwrap();
        view! { <PluginPanelDocument panel document=view busy=Signal::derive(||false) on_action=Callback::new(|_|())/> }
    });
    let input: web_sys::HtmlInputElement = root
        .query_selector("input")
        .unwrap()
        .unwrap()
        .unchecked_into();
    assert_eq!(input.get_attribute("type").as_deref(), Some("password"));
    assert_eq!(
        input.get_attribute("autocomplete").as_deref(),
        Some("new-password")
    );
    assert!(input.value().is_empty());
    assert!(input.required());
    assert_eq!(input.max_length(), 1024);
    drop(handle);
    root.remove();
}
