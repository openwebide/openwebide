use crate::state::{
    auth::AuthState,
    layout::LayoutState,
    projects::ProjectsState,
    settings::{ConfigurationSection, SettingsState},
    ui::UiState,
};
use leptos::prelude::*;

/// Branding, projects and universal search share one app row.
#[component]
pub fn TopBar(
    on_open_settings: Callback<()>,
    on_logout: Callback<()>,
    #[prop(default = Callback::new(|()| ()))] on_select_chat: Callback<()>,
    #[prop(default = Callback::new(|()| ()))] on_open_local: Callback<()>,
    #[prop(default = Callback::new(|()| ()))] on_open_remote: Callback<()>,
    #[prop(default = Callback::new(|_: i64| ()))] on_open_project: Callback<i64>,
    #[prop(default = Callback::new(|_: i64| ()))] on_delete_project: Callback<i64>,
    #[prop(optional)] children: Option<Children>,
    #[prop(optional, into)] status: Option<ViewFn>,
) -> impl IntoView {
    let auth = expect_context::<AuthState>();
    let layout = expect_context::<LayoutState>();
    let ui = expect_context::<UiState>();
    let projects = expect_context::<ProjectsState>();
    let settings = expect_context::<SettingsState>();
    let menu_open = RwSignal::new(false);
    let account_open = RwSignal::new(false);
    let status = StoredValue::new(status);
    let close = Callback::new(move |()| menu_open.set(false));
    Effect::new(move |previous: Option<u64>| {
        let epoch = auth.generation.get();
        if previous.is_some_and(|previous| previous != epoch) {
            menu_open.set(false);
            account_open.set(false);
        }
        epoch
    });
    Effect::new(move |_| {
        if ui.palette_open.get() {
            menu_open.set(false);
            account_open.set(false);
        }
    });
    view! {
        <header class="topbar app-navigation">
            <Show when=move || layout.phone.get() fallback=move || view! {
                <super::dropdown::Dropdown aria_label="App menu" class="app-menu" trigger_class="btn ghost logo" open=menu_open label=|| view! { <super::ui::LogoMark /><span class="brand-name">"Open WebIDE"</span> }>
                    <AppMenuItems on_close=close on_open_local=on_open_local on_open_remote=on_open_remote on_open_project=on_open_project on_delete_project=on_delete_project />
                </super::dropdown::Dropdown>
            }>
                <button type="button" class="btn ghost logo app-drawer-trigger" aria-label="App menu" aria-controls="app-drawer" aria-expanded=move || menu_open.get().to_string() on:click=move |_| menu_open.set(true)><super::ui::LogoMark /><span class="brand-name">"Open WebIDE"</span><super::ui::Icon name=super::ui::IconName::ChevronDown /></button>
            </Show>
            {children.map(|children| children())}
            {if use_context::<crate::state_actions::omnibar::OmnibarActions>().is_some() { view! { <super::Omnibar /> }.into_any() } else { view! { <input class="form-input omnibar-trigger" aria-label="Search commands, files, projects and sessions" placeholder="Search…" disabled /> }.into_any() }}
            <button class="btn ghost chat-tab" class:active=move || projects.active_project.get().is_none() title="Chat without a project" aria-label="Chat without a project" aria-pressed={move || projects.active_project.get().is_none().to_string()} on:click=move |_| on_select_chat.run(())><super::ui::Icon name=super::ui::IconName::MessageCircle /></button>
            <Show when=move || auth.username.get().is_some()>
                <super::dropdown::Dropdown aria_label="Account menu" class="topbar-account" open=account_open label=move || view! { <super::ui::Icon name=super::ui::IconName::User /><span class="topbar-user">{move || auth.username.get().unwrap_or_default()}</span> }>
                    <button type="button" role="menuitem" class="ui-dropdown-item recent-item" aria-label="Settings" on:click=move |_| { account_open.set(false); on_open_settings.run(()); }><super::ui::Icon name=super::ui::IconName::Settings /><span>"Settings"</span></button>
                    <button type="button" role="menuitem" class="ui-dropdown-item recent-item" on:click=move |_| { account_open.set(false); settings.configuration.set(Some(ConfigurationSection::SystemPrompts)); }><super::ui::Icon name=super::ui::IconName::MessageCircle /><span>"System prompts"</span></button>
                    <button role="menuitem" class="ui-dropdown-item recent-item" aria-label="Log out" on:click=move |_| { account_open.set(false); on_logout.run(()); }><super::ui::Icon name=super::ui::IconName::LogOut /><span>"Log out"</span></button>
                </super::dropdown::Dropdown>
            </Show>
        </header>
        <Show when=move || layout.phone.get() && menu_open.get()>
            <super::modal::Modal title=Signal::derive(|| "Open WebIDE".to_string()) class="modal app-drawer" on_close=close describedby="app-drawer-help">
                <super::ui::DialogBody>
                    <p class="form-hint" id="app-drawer-help">"Projects, configuration and help"</p>
                    <nav id="app-drawer" aria-label="App navigation"><AppMenuItems drawer=true on_close=close on_open_local=on_open_local on_open_remote=on_open_remote on_open_project=on_open_project on_delete_project=on_delete_project /></nav>
                    {status.get_value().map(|status| view! { <DrawerStatus status=status on_close=close /> })}
                </super::ui::DialogBody>
            </super::modal::Modal>
        </Show>
    }
}

#[component]
fn DrawerStatus(status: ViewFn, on_close: Callback<()>) -> impl IntoView {
    provide_context(super::status_bar::DrawerStatusActions(on_close));
    view! { <section class="app-drawer-status" aria-label="Workspace status">{status.run()}</section> }
}

/// The same destinations and actions serve desktop dropdown and phone drawer.
#[component]
fn AppMenuItems(
    on_close: Callback<()>,
    on_open_local: Callback<()>,
    on_open_remote: Callback<()>,
    on_open_project: Callback<i64>,
    on_delete_project: Callback<i64>,
    #[prop(optional)] drawer: bool,
) -> impl IntoView {
    let settings = expect_context::<SettingsState>();
    let ui = expect_context::<UiState>();
    let projects = expect_context::<ProjectsState>();
    view! {
        <div class="app-menu-items">
            <button type="button" role=(!drawer).then_some("menuitem") class="ui-dropdown-item recent-item" on:click=move |_| { on_close.run(()); on_open_local.run(()); }><super::ui::Icon name=super::ui::IconName::Folder /><span>"Open local project"</span></button>
            <button type="button" role=(!drawer).then_some("menuitem") class="ui-dropdown-item recent-item" on:click=move |_| { on_close.run(()); on_open_remote.run(()); }><super::ui::Icon name=super::ui::IconName::Folder /><span>"Open remote project"</span></button>
            <h3 class="app-menu-heading">"Open projects"</h3>
            <For each=move || projects.open_tabs() key=|project| project.id children=move |project| {
                let id=project.id;
                view! { <button type="button" class="ui-dropdown-item recent-item" role=(!drawer).then_some("menuitem") aria-current=move || (projects.active_project.get() == Some(id)).then_some("page") on:click=move |_| { on_close.run(()); on_open_project.run(id); }><span class="recent-name">{project.name}</span><span class="tab-mode">{project.mode.as_str()}</span></button> }
            } />
            <h3 class="app-menu-heading">"Recent projects"</h3>
            <For each=move || projects.recent_projects.get() key=|project| project.id children=move |project| {
                let id=project.id;
                let delete_label = format!("Delete project {}", project.name);
                view! { <div class="recent-project-row"><button type="button" class="ui-dropdown-item recent-item" role=(!drawer).then_some("menuitem") on:click=move |_| { on_close.run(()); on_open_project.run(id); }><span class="recent-name">{project.name}</span><span class="tab-mode">{project.mode.as_str()}</span></button><button type="button" class="icon-btn ui-icon" role=(!drawer).then_some("menuitem") aria-label=delete_label title="Delete project" on:click=move |_| { on_close.run(()); on_delete_project.run(id); }><super::ui::Icon name=super::ui::IconName::X /></button></div> }
            } />
            <Show when=move || projects.recent_projects.with(Vec::is_empty)><p class="form-hint">"No other saved projects."</p></Show>
            <h3 class="app-menu-heading">"Configuration"</h3>
            <button type="button" role=(!drawer).then_some("menuitem") class="ui-dropdown-item recent-item" on:click=move |_| { on_close.run(()); settings.configuration.set(Some(ConfigurationSection::Servers)); }><super::ui::Icon name=super::ui::IconName::SlidersHorizontal /><span>"Servers"</span></button>
            <h3 class="app-menu-heading">"Help"</h3>
            <button type="button" role=(!drawer).then_some("menuitem") class="ui-dropdown-item recent-item" on:click=move |_| { on_close.run(()); ui.shortcuts_open.set(true); }><super::ui::Icon name=super::ui::IconName::Info /><span>"Help / Keyboard shortcuts"</span></button>
            <button type="button" role=(!drawer).then_some("menuitem") class="ui-dropdown-item recent-item" aria-label="About" on:click=move |_| { on_close.run(()); ui.about_open.set(true); }><super::ui::Icon name=super::ui::IconName::Info /><span>"About"</span></button>
        </div>
    }
}
