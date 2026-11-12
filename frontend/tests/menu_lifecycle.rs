#![cfg(target_arch = "wasm32")]

use leptos::prelude::*;
use openwebide_frontend::components::{Modal, dropdown::ActionMenu};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

async fn settle() {
    for _ in 0..50 {
        JsFuture::from(js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED))
            .await
            .unwrap();
    }
}

fn root() -> web_sys::HtmlElement {
    let document = web_sys::window().unwrap().document().unwrap();
    let root: web_sys::HtmlElement = document.create_element("div").unwrap().unchecked_into();
    document.body().unwrap().append_child(&root).unwrap();
    root
}

async fn toggle(shown: RwSignal<bool>) {
    shown.set(true);
    settle().await;
    shown.set(false);
    settle().await;
}

#[wasm_bindgen_test]
async fn conditional_content_releases_browser_references_after_repeated_dismissal() {
    let shown = RwSignal::new(false);
    let root = root();
    let mounted = leptos::mount::mount_to(root.clone(), move || {
        view! {
            <Show when=move || shown.get()><p>"Conditional content"</p></Show>
        }
    });
    settle().await;
    for _ in 0..8 {
        toggle(shown).await;
    }
    let before = wasm_bindgen::externref_heap_live_count();
    for _ in 0..16 {
        toggle(shown).await;
    }
    let after = wasm_bindgen::externref_heap_live_count();
    console_log!("conditional references before={before} after={after}");
    drop(mounted);
    root.remove();
    settle().await;
    assert_eq!(
        after, before,
        "Dismissed conditional views must release browser references"
    );
}

#[wasm_bindgen_test]
async fn result_dialogs_release_browser_references_after_repeated_dismissal() {
    let shown = RwSignal::new(false);
    let root = root();
    let mounted = leptos::mount::mount_to(root.clone(), move || {
        view! {
            <Show when=move || shown.get()>
                <Modal title=Signal::derive(|| "Symbol information".to_string()) on_close=Callback::new(move |()| shown.set(false))>
                    <div>"function answer(): string"</div>
                </Modal>
            </Show>
        }
    });
    settle().await;
    for _ in 0..8 {
        toggle(shown).await;
    }
    let before = wasm_bindgen::externref_heap_live_count();
    for _ in 0..16 {
        toggle(shown).await;
    }
    let after = wasm_bindgen::externref_heap_live_count();
    console_log!("dialog references before={before} after={after}");
    drop(mounted);
    root.remove();
    settle().await;
    assert_eq!(
        after, before,
        "Dismissed language result dialogs must release browser references"
    );
}

#[wasm_bindgen_test]
async fn action_menu_controls_keep_browser_references_bounded_across_reopening() {
    let root = root();
    let mounted = leptos::mount::mount_to(root.clone(), || {
        view! {
            <ActionMenu aria_label="Language actions"><button role="menuitem">"Symbol information"</button></ActionMenu>
        }
    });
    settle().await;
    for _ in 0..8 {
        root.query_selector(".ui-dropdown-trigger")
            .unwrap()
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>()
            .click();
        settle().await;
        root.query_selector(".ui-dropdown-backdrop")
            .unwrap()
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>()
            .click();
        settle().await;
    }
    let before = wasm_bindgen::externref_heap_live_count();
    for _ in 0..16 {
        root.query_selector(".ui-dropdown-trigger")
            .unwrap()
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>()
            .click();
        settle().await;
        root.query_selector(".ui-dropdown-backdrop")
            .unwrap()
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>()
            .click();
        settle().await;
    }
    let after = wasm_bindgen::externref_heap_live_count();
    console_log!("menu references before={before} after={after}");
    drop(mounted);
    root.remove();
    settle().await;
    assert_eq!(
        after, before,
        "Reopening language actions must keep browser references bounded"
    );
}

#[wasm_bindgen_test]
async fn reactive_language_result_body_releases_browser_references_on_dismissal() {
    use openwebide_frontend::components::ui::DialogBody;
    let result = RwSignal::new(None::<String>);
    let error = RwSignal::new(None::<String>);
    let root = root();
    let close = Callback::new(move |()| result.set(None));
    let mounted = leptos::mount::mount_to(root.clone(), move || {
        view! {
            <Show when=move || result.with(Option::is_some)>
                <Modal title=Signal::derive(move || result.with(|value| value.as_ref().map_or("Language service", |_| "Symbol information")).to_string()) on_close=close>
                    <DialogBody>
                        {move || result.get().map(|text| view! { <div class="markdown" inner_html=openwebide_frontend::markdown::render(&text) /> }.into_any())}
                        <Show when=move || error.with(Option::is_some)><p role="alert">{move || error.get()}</p></Show>
                    </DialogBody>
                </Modal>
            </Show>
        }
    });
    settle().await;
    for _ in 0..8 {
        result.set(Some("```typescript\nfunction answer(): string\n```".into()));
        settle().await;
        close.run(());
        settle().await;
    }
    let before = wasm_bindgen::externref_heap_live_count();
    for _ in 0..16 {
        result.set(Some("```typescript\nfunction answer(): string\n```".into()));
        settle().await;
        close.run(());
        settle().await;
    }
    let after = wasm_bindgen::externref_heap_live_count();
    console_log!("reactive dialog references before={before} after={after}");
    drop(mounted);
    root.remove();
    settle().await;
    assert_eq!(
        after, before,
        "Dismissed reactive language result bodies must release browser references"
    );
}
