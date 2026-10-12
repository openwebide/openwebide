pub mod fake_backend;
/// Initialize the same Leptos executor used by mounted UI before an isolated
/// library fixture creates effects. The detached mount owns no application state.
pub fn initialize_runtime() {
    use wasm_bindgen::JsCast;
    let root = web_sys::window()
        .unwrap()
        .document()
        .unwrap()
        .create_element("div")
        .unwrap()
        .unchecked_into();
    drop(leptos::mount::mount_to(root, || leptos::view! { <span/> }));
}

pub mod fake_transport;

#[cfg(target_arch = "wasm32")]
pub use crate::mcp_bridge::McpBridge;
