use super::{
    Modal,
    ui::{DialogActions, DialogBody, LogoMark},
};
use crate::state::ui::UiState;
use leptos::prelude::*;

// The shared build generator escapes all metadata and notice text.
const OVERVIEW: &str = include_str!(concat!(env!("OUT_DIR"), "/about-overview.html"));
const SOFTWARE_URL: &str = env!("OPENWEBIDE_SOFTWARE_URL");

#[component]
pub fn About() -> impl IntoView {
    let ui = expect_context::<UiState>();
    let close = Callback::new(move |()| ui.about_open.set(false));
    let software = RwSignal::new(false);
    let notices = RwSignal::new(None::<Result<String, String>>);
    let pending = RwSignal::new(false);
    let load = move || {
        if pending.get_untracked()
            || notices.with_untracked(|result| result.as_ref().is_some_and(Result::is_ok))
        {
            return;
        }
        pending.set(true);
        notices.set(None);
        leptos::task::spawn_local(async move {
            let result = async {
                let response = gloo_net::http::Request::get(SOFTWARE_URL)
                    .send()
                    .await
                    .map_err(|_| ())?;
                if !response.ok() {
                    return Err(());
                }
                response.text().await.map_err(|_| ())
            }
            .await
            .map_err(|()| "Could not load open-source software.".to_string());
            notices.try_set(Some(result));
            pending.try_set(false);
        });
    };
    Effect::new(move || {
        if software.get() {
            load();
        }
    });
    let tab = RwSignal::new(if ui.about_release_notes.get_untracked() {
        2_usize
    } else {
        0
    });
    ui.about_release_notes.set(false);
    Effect::new(move || software.set(tab.get() == 1));
    view! {
        <Modal title="About Open WebIDE".to_string().into() on_close=close class="modal about-modal">
            <super::ui::DialogTabs label="About pages" options=vec![
                super::ui::DialogTab::new("About", "about-tab-overview", "about-panel"),
                super::ui::DialogTab::new("Open-source software", "about-tab-software", "about-panel"),
                super::ui::DialogTab::new("Release notes", "about-tab-release-notes", "about-panel"),
            ] selected=tab.read_only().into() on_change=Callback::new(move |index| tab.set(index)) />
            <DialogBody class="about-body"><div class="about-content" class:about-overview=move || tab.get() == 0 class:about-software=move || software.get() id="about-panel" role="tabpanel" tabindex="0" aria-labelledby=move || match tab.get() { 1 => "about-tab-software", 2 => "about-tab-release-notes", _ => "about-tab-overview" }>
                {move || if tab.get() == 2 {
                    view! { <ReleaseNotes /> }.into_any()
                } else if software.get() {
                    match notices.get() {
                        Some(Ok(html)) => view! { <div inner_html=html /> }.into_any(),
                        Some(Err(error)) => view! { <p role="alert">{error}</p><button class="btn" on:click=move |_| load()>"Retry"</button> }.into_any(),
                        None => view! { <p role="status">"Loading open-source software…"</p> }.into_any(),
                    }
                } else {
                    view! {
                        <div>
                            <LogoMark class="about-logo" />
                            <div inner_html=OVERVIEW />
                            <a href="https://openwebide.com/" target="_blank" rel="noopener noreferrer">"openwebide.com"</a>
                        </div>
                    }.into_any()
                }}
            </div></DialogBody>
            <DialogActions><button class="btn" on:click=move |_| close.run(())>"Close"</button></DialogActions>
        </Modal>
    }
}

#[component]
fn ReleaseNotes() -> impl IntoView {
    view! {
        <div class="release-notes" inner_html=crate::markdown::render(include_str!(concat!(env!("OUT_DIR"), "/release-notes.md"))) />
        <a href=format!("https://github.com/openwebide/openwebide/releases/tag/v{}", env!("CARGO_PKG_VERSION")) target="_blank" rel="noopener noreferrer">"View release on GitHub"</a>
    }
}

#[component]
pub fn WhatsNew() -> impl IntoView {
    let ui = expect_context::<UiState>();
    let api = expect_context::<crate::backend::Api>();
    let auth = expect_context::<crate::state::auth::AuthState>();
    let pending = RwSignal::new(false);
    let acknowledge = Callback::new(move |show_notes: bool| {
        if pending.get_untracked() {
            return;
        }
        pending.set(true);
        let generation = auth.generation.get_untracked();
        let backend = api.get_value();
        leptos::task::spawn_local(async move {
            let result = backend
                .set_setting("last_seen_release_version", env!("CARGO_PKG_VERSION"))
                .await;
            if auth.generation.try_get_untracked() != Some(generation) {
                return;
            }
            pending.try_set(false);
            match result {
                Ok(()) => {
                    ui.whats_new_open.set(false);
                    if show_notes {
                        ui.about_release_notes.set(true);
                        ui.about_open.set(true);
                    }
                }
                Err(error) => ui.notify(error),
            }
        });
    });
    let close = Callback::new(move |()| acknowledge.run(false));
    view! {
        <Modal title=format!("What’s new in Open WebIDE {}", env!("CARGO_PKG_VERSION")).into() on_close=close class="modal">
            <DialogBody><p>"Open WebIDE has been updated. Read the release notes to see what’s changed."</p></DialogBody>
            <DialogActions>
                <button class="btn ghost" disabled=move || pending.get() on:click=move |_| close.run(())>"Dismiss"</button>
                <button class="btn" disabled=move || pending.get() on:click=move |_| acknowledge.run(true)><super::ui::Icon name=super::ui::IconName::FileClock />"Release notes"</button>
            </DialogActions>
        </Modal>
    }
}
