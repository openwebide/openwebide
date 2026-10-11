use super::support::{mount_test, settle};
use leptos::prelude::*;
use openwebide_frontend::components::{CommandDialogs, StatusBar, TopBar};
use wasm_bindgen_test::*;

#[wasm_bindgen_test]
async fn release_notice_acknowledges_the_account_and_opens_bundled_notes() {
    let mounted = mount_test(move |state| {
        state.ui.whats_new_open.set(true);
        view! { <style>{include_str!("../../styles.css")}</style><CommandDialogs /> }
    });
    settle().await;
    mounted.click_text("Release notes");
    settle().await;
    assert!(!mounted.state.ui.whats_new_open.get_untracked());
    assert_eq!(
        mounted
            .state
            .fake
            .settings
            .borrow()
            .get("last_seen_release_version")
            .map(String::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert!(mounted.state.ui.about_open.get_untracked());
    assert!(
        mounted
            .element(".release-notes")
            .text_content()
            .unwrap()
            .contains(env!("CARGO_PKG_VERSION"))
    );
    mounted.click_text("Close");
    settle().await;
    assert!(!mounted.state.ui.about_open.get_untracked());
}

#[wasm_bindgen_test]
async fn phone_workspace_status_lives_in_drawer_and_plugin_action_closes_it() {
    let mounted = mount_test(move |_state| {
        let layout = expect_context::<openwebide_frontend::state::layout::LayoutState>();
        layout
            .preferences
            .update(|prefs| prefs.mode = openwebide_frontend::state::responsive::LayoutMode::Phone);
        let (health, _) = signal(None);
        view! {
            <style>{include_str!("../../styles.css")}</style>
            <div class="app phone-layout"><TopBar on_open_settings=Callback::new(|()| ()) on_logout=Callback::new(|()| ()) status=move || view! { <StatusBar health=health on_toggle_terminal=|| () /> } /></div>
        }
    });
    settle().await;
    assert!(mounted.root.query_selector(".statusbar").unwrap().is_none());
    mounted.click("[aria-label='App menu']");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector(".app-drawer-status .statusbar")
            .unwrap()
            .is_some()
    );
    mounted.click_text("Plugins");
    settle().await;
    assert!(mounted.state.ui.plugins_open.get_untracked());
    assert!(
        mounted
            .root
            .query_selector(".app-drawer")
            .unwrap()
            .is_none()
    );
}
