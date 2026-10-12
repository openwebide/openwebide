#![cfg(target_arch = "wasm32")]
use openwebide_frontend::{
    components::panel_playground::PanelPlayground, panel_playground::Playground,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);
async fn settle() {
    openwebide_frontend::util::sleep_ms(40).await;
}
fn edit(root: &web_sys::Element, selector: &str, value: &str) {
    let input = root.query_selector(selector).unwrap().unwrap();
    if let Some(area) = input.dyn_ref::<web_sys::HtmlTextAreaElement>() {
        area.set_value(value);
    } else if let Some(select) = input.dyn_ref::<web_sys::HtmlSelectElement>() {
        select.set_value(value);
    } else {
        input
            .dyn_ref::<web_sys::HtmlInputElement>()
            .unwrap()
            .set_value(value);
    }
    let init = web_sys::EventInit::new();
    init.set_bubbles(true);
    input
        .dispatch_event(
            &web_sys::Event::new_with_event_init_dict(
                if input.tag_name() == "SELECT" {
                    "change"
                } else {
                    "input"
                },
                &init,
            )
            .unwrap(),
        )
        .unwrap();
}
fn click(root: &web_sys::Element, selector: &str) {
    root.query_selector(selector)
        .unwrap()
        .unwrap()
        .dyn_into::<web_sys::HtmlElement>()
        .unwrap()
        .click();
}
#[wasm_bindgen_test]
async fn playground_uses_real_widgets_validates_live_and_simulates_approval_without_exposing_secrets()
 {
    let document = web_sys::window().unwrap().document().unwrap();
    let root = document.create_element("div").unwrap();
    document.body().unwrap().append_child(&root).unwrap();
    let handle = leptos::mount::mount_to(root.clone().dyn_into().unwrap(), PanelPlayground);
    settle().await;
    assert_eq!(
        root.query_selector_all(".plugin-panel-document table")
            .unwrap()
            .length(),
        1
    );
    edit(&root, "select[aria-label='Theme']", "light");
    edit(&root, "select[aria-label='Preview width']", "phone");
    settle().await;
    assert_eq!(
        document
            .document_element()
            .unwrap()
            .get_attribute("data-theme")
            .as_deref(),
        Some("light")
    );
    assert!(
        root.query_selector(".playground-preview-frame")
            .unwrap()
            .unwrap()
            .get_attribute("style")
            .unwrap()
            .contains("375px")
    );
    edit(&root, "select[aria-label='Component example']", "6");
    settle().await;
    edit(&root, "input[aria-label='Token']", "secret-test-value");
    let form = root
        .query_selector(".plugin-panel-document form")
        .unwrap()
        .unwrap();
    let init = web_sys::EventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    form.dispatch_event(&web_sys::Event::new_with_event_init_dict("submit", &init).unwrap())
        .unwrap();
    settle().await;
    let trace = root
        .query_selector(".playground-action-input")
        .unwrap()
        .unwrap()
        .text_content()
        .unwrap();
    assert!(trace.contains("<hidden>"));
    assert!(!trace.contains("secret-test-value"));
    let approve = root.query_selector_all("button").unwrap();
    let button = (0..approve.length())
        .filter_map(|index| approve.item(index))
        .find(|node| node.text_content().as_deref() == Some("Approve simulation"))
        .unwrap()
        .dyn_into::<web_sys::HtmlElement>()
        .unwrap();
    button.click();
    settle().await;
    let source = root
        .query_selector("textarea[aria-label='Panel JSON']")
        .unwrap()
        .unwrap()
        .dyn_into::<web_sys::HtmlTextAreaElement>()
        .unwrap()
        .value();
    assert_eq!(Playground::parse(&source).unwrap().document.revision, 2);
    // A stale replacement cannot alter the canonical draft.
    edit(&root, "select[aria-label='Component example']", "5");
    settle().await;
    let actions = openwebide_frontend::panel_playground::gallery()[5]
        .1
        .clone();
    edit(
        &root,
        "textarea[aria-label='Simulated response JSON']",
        &serde_json::to_string(&actions.document).unwrap(),
    );
    click(&root, ".plugin-panel-document button");
    settle().await;
    assert!(root.text_content().unwrap().contains("next view revision"));
    let source = root
        .query_selector("textarea[aria-label='Panel JSON']")
        .unwrap()
        .unwrap()
        .dyn_into::<web_sys::HtmlTextAreaElement>()
        .unwrap()
        .value();
    assert_eq!(Playground::parse(&source).unwrap().document.revision, 1);
    let buttons = root
        .query_selector_all(".plugin-panel-document button")
        .unwrap();
    (0..buttons.length())
        .filter_map(|index| buttons.item(index))
        .find(|node| node.text_content().as_deref() == Some("Save"))
        .unwrap()
        .dyn_into::<web_sys::HtmlElement>()
        .unwrap()
        .click();
    settle().await;
    assert!(root.text_content().unwrap().contains("Approve simulation"));
    edit(&root, "textarea[aria-label='Panel JSON']", "{broken");
    settle().await;
    assert!(
        root.query_selector(".plugin-panel-document")
            .unwrap()
            .is_none()
    );
    assert!(root.text_content().unwrap().contains("Invalid:"));
    assert!(!root.text_content().unwrap().contains("Approve simulation"));
    assert_eq!(
        root.query_selector_all("header button[disabled]")
            .unwrap()
            .length(),
        2
    );
    edit(&root, "select[aria-label='Component example']", "10");
    settle().await;
    assert!(root.query_selector(".tui-diff-lines").unwrap().is_some());
    click(&root, "button[aria-label='Format JSON']");
    settle().await;
    drop(handle);
    root.remove();
}
