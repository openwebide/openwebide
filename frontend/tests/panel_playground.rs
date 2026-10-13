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
fn click_text(root: &web_sys::Element, text: &str) {
    let buttons = root.query_selector_all("button").unwrap();
    (0..buttons.length())
        .filter_map(|index| buttons.item(index))
        .find(|button| button.text_content().as_deref() == Some(text))
        .unwrap()
        .dyn_into::<web_sys::HtmlElement>()
        .unwrap()
        .click();
}

fn source(root: &web_sys::Element) -> Playground {
    Playground::parse(
        &root
            .query_selector("textarea[aria-label='Panel JSON']")
            .unwrap()
            .unwrap()
            .dyn_into::<web_sys::HtmlTextAreaElement>()
            .unwrap()
            .value(),
    )
    .unwrap()
}

#[wasm_bindgen_test]
async fn visual_table_columns_keep_their_cells_aligned() {
    use openwebide_core::plugins::panels::Block;
    let document = web_sys::window().unwrap().document().unwrap();
    let root = document.create_element("div").unwrap();
    document.body().unwrap().append_child(&root).unwrap();
    let handle = leptos::mount::mount_to(root.clone().dyn_into().unwrap(), PanelPlayground);
    settle().await;
    edit(&root, "select[aria-label='Component example']", "8");
    settle().await;
    click(&root, "button[aria-label='Select or drag block 1']");
    settle().await;
    click_text(&root, "Add columns item");
    settle().await;
    edit(&root, "input[aria-label='columns · 4']", "New column");
    edit(&root, "input[aria-label='rows · 1 · 4']", "Aligned cell");
    let item = root
        .query_selector("input[aria-label='columns · 4']")
        .unwrap()
        .unwrap()
        .closest(".panel-property-item")
        .unwrap()
        .unwrap();
    click(&item, "button[aria-label='Move property item up']");
    settle().await;
    click_text(&root, "Apply properties");
    settle().await;
    let panel = source(&root);
    let Block::Table { columns, rows } = &panel.document.blocks[0] else {
        panic!("table")
    };
    assert_eq!(columns[2], "New column");
    assert_eq!(rows[0][2], "Aligned cell");
    assert!(rows.iter().all(|row| row.len() == columns.len()));
    assert!(
        root.query_selector(".plugin-panel-document table")
            .unwrap()
            .unwrap()
            .text_content()
            .unwrap()
            .contains("Aligned cell")
    );
    drop(handle);
    root.remove();
}

#[wasm_bindgen_test]
async fn visual_properties_keyboard_placement_and_history_use_the_same_document_as_json() {
    use openwebide_core::plugins::panels::{Block, FieldKind};
    let document = web_sys::window().unwrap().document().unwrap();
    let root = document.create_element("div").unwrap();
    document.body().unwrap().append_child(&root).unwrap();
    let handle = leptos::mount::mount_to(root.clone().dyn_into().unwrap(), PanelPlayground);
    settle().await;
    let initial = Playground::blank();
    click_text(&root, "New panel");
    settle().await;
    assert_eq!(source(&root), initial);
    click(&root, "button[aria-label='Add or drag Group']");
    settle().await;
    click(&root, "button[aria-label='Add or drag Text']");
    settle().await;
    edit(
        &root,
        ".panel-builder-properties textarea[aria-label='text']",
        "Visual text",
    );
    click_text(&root, "Apply properties");
    settle().await;
    assert!(
        matches!(&source(&root).document.blocks[0],Block::Group{blocks,..} if matches!(&blocks[0],Block::Text{text} if text=="Visual text"))
    );
    edit(&root, "select[aria-label='Destination group']", "[]");
    settle().await;
    edit(&root, "select[aria-label='Block position']", "0");
    click_text(&root, "Move block");
    settle().await;
    assert!(matches!(&source(&root).document.blocks[0],Block::Text{text} if text=="Visual text"));
    click(&root, "button[aria-label='Undo']");
    settle().await;
    assert_eq!(source(&root).document.blocks.len(), 1);
    click(&root, "button[aria-label='Redo']");
    settle().await;
    assert_eq!(source(&root).document.blocks.len(), 2);
    click(&root, "button[aria-label='Panel properties']");
    settle().await;
    edit(
        &root,
        ".panel-builder-properties input[aria-label='title']",
        "Visual panel",
    );
    click_text(&root, "Apply properties");
    settle().await;
    assert_eq!(source(&root).panel.title, "Visual panel");
    click(&root, "button[aria-label='Add or drag Form']");
    settle().await;
    click_text(&root, "Add fields item");
    settle().await;
    edit(&root, "select[aria-label='fields · 2 · kind']", "checkbox");
    settle().await;
    click_text(&root, "Apply properties");
    settle().await;
    let current = source(&root);
    let Block::Form { fields, .. } = current.document.blocks.last().unwrap() else {
        panic!("form")
    };
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[1].kind, FieldKind::Checkbox);
    let duplicate_id = fields[0].id.clone();
    edit(&root, "input[aria-label='fields · 2 · id']", &duplicate_id);
    settle().await;
    assert!(root.text_content().unwrap().contains("unique IDs"));
    click_text(&root, "Apply properties");
    settle().await;
    assert_eq!(
        source(&root),
        current,
        "invalid property drafts must not change the canonical document"
    );
    edit(&root, "textarea[aria-label='Panel JSON']", &initial.json());
    settle().await;
    assert!(
        root.query_selector(".panel-builder-properties")
            .unwrap()
            .is_none()
    );
    assert!(source(&root).document.blocks.is_empty());
    drop(handle);
    root.remove();
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
