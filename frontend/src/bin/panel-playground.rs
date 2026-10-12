fn main() {
    #[cfg(target_arch = "wasm32")]
    leptos::mount::mount_to_body(
        openwebide_frontend::components::panel_playground::PanelPlayground,
    );
}
