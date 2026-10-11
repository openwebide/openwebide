use crate::state::auth::AuthState;
use crate::state::chat::ChatState;
use crate::state::git::GitState;
use crate::state::layout::{ActiveResizer, LayoutState, Panel};
use crate::state::projects::ProjectsState;
use crate::state::settings::SettingsState;
use crate::state::ui::UiState;
use crate::state::workspace::WorkspaceState;
use leptos::prelude::*;

use crate::components::{
    AuthGate, ChatPane, ConfirmDialog, Editor, FileBrowser, FileTree, GitPane, PanelRail,
    PromptDialog, SearchPane, Settings, Sidebar, StatusBar, TabBar, TerminalDock, ToolPanel,
    TopBar,
};
use crate::state_actions::{
    auth::{AuthActionContext, AuthActions},
    chat::{ChatActionContext, ChatActions},
    git::{GitActionContext, GitActions},
    lifecycle::{ProjectEffectContext, install_keyboard_shortcuts, install_project_effects},
    projects::{ProjectsActionContext, ProjectsActions, build_projects_actions},
    settings::{SettingsActionContext, SettingsActions, build_settings_actions},
    workspace::WorkspaceActions,
};
use crate::{api::HealthState, backend::Api};

#[component]
pub fn App() -> impl IntoView {
    crate::viewport::install_action_tooltips();
    crate::components::context_menu::disable_browser_menu();
    let api: Api =
        StoredValue::new_local(std::rc::Rc::new(crate::api::BackendApi::from_location()));
    provide_context(api);

    let ui = UiState::new();
    provide_context(ui);
    let auth = AuthState::new();
    provide_context(auth);
    let settings = SettingsState::new(
        crate::state::settings::Theme::from_root(),
        crate::bridge::default_bridge_url(),
    );
    provide_context(settings);
    let projects_state = ProjectsState::new();
    let layout = LayoutState::with_active_project(projects_state.active_project);
    provide_context(layout);
    let layout_actions = crate::state_actions::layout::LayoutActions::new(api, layout, auth, ui);
    provide_context(layout_actions);
    let active_project = projects_state.active_project;
    let workspace_state = WorkspaceState::with_active_project(active_project);
    let git_state = GitState::with_active_project(active_project);
    let active_session = workspace_state.active_session;
    let chat_state = ChatState::with_active_session_and_toast(active_session, ui.toast);
    provide_context(projects_state);
    provide_context(workspace_state);
    provide_context(git_state);
    provide_context(chat_state);
    provide_context(crate::state::questions::QuestionsState::new(
        api,
        auth,
        chat_state,
        projects_state,
    ));
    provide_context(crate::host_admin::HostState::new(
        api,
        auth,
        chat_state,
        projects_state,
    ));
    let skills = crate::state::skills::SkillsState::new();
    provide_context(skills);
    provide_context(crate::project_skills::ProjectSkillActions::new(
        api,
        skills,
        auth,
        projects_state,
        chat_state,
    ));
    let memories = crate::state::memories::MemoriesState::new();
    provide_context(memories);
    crate::state_actions::lifecycle::install_window_title(
        auth,
        projects_state,
        workspace_state,
        chat_state,
    );
    let sessions_state = crate::state::sessions::SessionsState::new(chat_state, active_project);
    provide_context(sessions_state);
    provide_context(crate::state_actions::sessions::SessionActions::new(
        api,
        auth,
        chat_state,
        projects_state,
        sessions_state,
        ui,
        std::rc::Rc::new(crate::state_actions::sessions::BrowserSessionDownload),
    ));

    // -- auth --------------------------------------------------------------
    // The signed-in account, once the cached token has been verified (or the
    // user has logged in). `None` while the gate is showing.
    let current_user = auth.user;
    // True once the initial token check has finished (regardless of outcome).
    let auth_checked = auth.checked;
    // The signed-in user's name, for the top bar.

    // -- global state ------------------------------------------------------
    let health = RwSignal::new(Option::<HealthState>::None);
    let bridge_url = settings.bridge_url;
    let bridge_connection = RwSignal::new_local(Option::<crate::bridge::BridgeConn>::None);
    let bridge_credentials = StoredValue::new(crate::bridge::BridgeCredentials::new(api));
    Effect::new(move |_| {
        let user = auth.user.get().map(|user| user.id);
        let url = bridge_url.get();
        auth.bridge.update_value(|connection| {
            if let Some(connection) = connection.take() {
                connection.close();
            }
            if user.is_some() {
                *connection = Some(crate::bridge::BridgeConn::new(
                    crate::bridge::BridgeConfig::new(&url),
                    bridge_credentials.get_value(),
                ));
            }
            bridge_connection.set(connection.clone());
        });
    });
    on_cleanup(move || {
        auth.bridge.with_value(|connection| {
            if let Some(connection) = connection {
                connection.close();
            }
        });
    });
    let active_resizer = layout.active_resizer;
    let error = ui.toast;

    // -- active project's workspace state ----------------------------------
    let ws_read_only = RwSignal::new(false);
    // "Include ignored folders" search toggle: per query, not persisted,
    // resets to off on reload.
    let include_ignored_search = RwSignal::new(false);

    // -- bottom dock terminal & TUI telemetry/context ----------------------
    let show_terminal = chat_state.show_terminal;
    let on_toggle_terminal = move || {
        layout_actions.toggle.run(Panel::Terminal);
    };

    let show_settings = settings.show_settings;

    // -- confirmation dialogs (Phase 10) -----------------------------------
    // A pending confirmation request; the ConfirmDialog renders it and runs
    // its action on confirm. Destructive actions set this instead of using
    // window.confirm.
    // -- remote file browser (Phase 10) ------------------------------------
    // Opens the host-folder browser so a remote project's folder can be picked
    // by navigating the mount tree. The chosen path is relative to the mount
    // root, which is exactly what NewProject.path wants.
    let show_browser = RwSignal::new(false);
    let on_open_remote = Callback::new(move |()| {
        show_browser.set(true);
    });
    let on_close_browser = Callback::new(move |()| {
        show_browser.set(false);
    });

    let project_git = crate::project_git::ProjectGit::new(api, projects_state, settings, auth);
    provide_context(project_git);
    provide_context(crate::project_memory::ProjectMemoryActions::new(
        api,
        memories,
        auth,
        projects_state,
        chat_state,
        expect_context::<crate::project_host::ProjectHost>(),
    ));
    let plugins = crate::state::plugins::PluginsState::default();
    provide_context(plugins);
    provide_context(crate::project_plugins::ProjectPluginActions::new(
        api,
        plugins,
        expect_context::<crate::project_host::ProjectHost>(),
        auth,
        projects_state,
        chat_state,
        settings,
    ));
    let monitors = crate::state::monitors::MonitorsState::new();
    provide_context(monitors);
    provide_context(crate::monitors::MonitorActions::new(
        api,
        monitors,
        auth,
        projects_state,
        chat_state,
    ));
    let scheduled = crate::state::scheduled::TasksState::new();
    provide_context(scheduled);
    provide_context(crate::scheduled::TaskActions::new(
        api,
        scheduled,
        auth,
        projects_state,
        chat_state,
    ));
    let refresh_git = GitActions::refresh(project_git, projects_state, git_state, auth);

    let auth_actions = AuthActions::new(AuthActionContext {
        api,
        auth,
        projects: projects_state,
        workspace: workspace_state,
        git: git_state,
        chat: chat_state,
        settings,
        ui,
    });
    let on_logout = auth_actions.on_logout;

    let workspace_actions = WorkspaceActions::new(
        api,
        projects_state,
        workspace_state,
        ui,
        ws_read_only,
        refresh_git,
    );
    let WorkspaceActions {
        workspace_for,
        on_grant_access,
        ensure_root,
        on_open_lossy,
        request_open,
        on_toggle,
        on_save,
        on_accept,
        on_reject,
        on_new_file,
        on_new_dir,
        on_search_input,
        on_cancel_search,
        on_search,
        on_clear_search,
        ..
    } = workspace_actions;

    let editor_recovery = crate::state::editor_recovery::EditorRecoveryState::new();
    provide_context(editor_recovery);
    let recovery_actions = crate::state_actions::editor_recovery::RecoveryActions::new(
        api,
        auth,
        projects_state,
        workspace_state,
        editor_recovery,
        ws_read_only,
        request_open,
    );
    provide_context(recovery_actions);

    provide_context(crate::components::editor_chrome::EditorFooterMount::new());
    let git_actions = GitActions::new(GitActionContext {
        project_git,
        projects: projects_state,
        workspace: workspace_state,
        git: git_state,
        chat: chat_state,
        ui,
        workspace_for,
        refresh: refresh_git,
    });
    provide_context(git_actions);
    let on_branch_click = git_actions.on_branch_click;
    let on_sync_click = git_actions.on_sync_click;
    let on_load_git_diff = git_actions.on_load_diff;
    let on_discard_git_diff = git_actions.on_discard_diff;

    let project_actions = build_projects_actions(ProjectsActionContext {
        api,
        projects: projects_state,
        workspace: workspace_state,
        git: git_state,
        chat: chat_state,
        ui,
        ensure_root,
        refresh_git,
    });
    let ProjectsActions {
        select_project,
        select_chat,
        close_project,
        tab_action,
        on_open_project,
        on_open_local,
        on_browser_select,
        on_delete_project,
    } = project_actions;

    let navigation = crate::state_actions::navigation::NavigationActions::new(
        projects_state,
        chat_state,
        ui,
        layout_actions,
        on_open_project,
        select_chat,
    );
    provide_context(navigation);
    let on_open_project = navigation.open_project;
    let notifications = crate::notifications::RunNotifications::new(
        std::rc::Rc::new(crate::notifications::BrowserNotificationHost::default()),
        auth,
        navigation.open_session,
    );
    provide_context(notifications);
    crate::web_push::install(
        api,
        auth,
        settings,
        chat_state,
        layout,
        notifications,
        navigation.open_session,
    );

    let settings_actions = build_settings_actions(SettingsActionContext { api, settings, ui });
    let SettingsActions {
        on_new_prompt,
        on_edit_prompt,
        on_cancel_prompt,
        on_save_prompt,
        on_delete_prompt,
        on_new_connection,
        on_edit_connection,
        on_cancel_connection,
        on_delete_connection,
        on_open_settings,
        on_set_theme,
        on_set_editor_preferences,
        on_set_notifications,
        on_set_default_connection: _,
        on_set_default_prompt,
        on_set_bridge_url,
    } = settings_actions;

    let request_open = Callback::new(move |path: String| {
        layout_actions.show.run(Panel::Editor);
        request_open.run(path);
    });
    Effect::new(move |_| {
        if show_terminal.get() {
            layout_actions.show.run(Panel::Terminal);
        }
    });
    let terminal_visible = RwSignal::new(false);
    Effect::new(move |_| {
        let visible = layout.visible_panels.get().terminal;
        terminal_visible.set(visible);
        show_terminal.set(visible);
    });
    let chat_actions = ChatActions::new(ChatActionContext {
        api,
        chat: chat_state,
        projects: projects_state,
        workspace: workspace_state,
        settings,
        ui,
        git: git_state,
        bridge: bridge_connection,
        request_open,
        refresh_git,
        on_sync_click,
        on_commit: git_actions.on_commit,
        on_checkout: git_actions.on_checkout,
    });
    expect_context::<crate::state_actions::file_tree::FileTreeActions>()
        .send_prompt
        .set(Some(chat_actions.send_prompt));
    let on_send = chat_actions.send;
    let on_stop = chat_actions.stop;
    let on_permission = chat_actions.permission;
    let on_permission_always = chat_actions.permission_always;
    let on_select_session = navigation.open_session;
    let on_new_session = Callback::new(move |()| {
        layout_actions.show.run(Panel::Chat);
        chat_actions.on_new_session.run(());
    });
    let on_rename_session = chat_actions.on_rename_session;
    let on_delete_session = chat_actions.on_delete_session;
    let on_slash_command = chat_actions.slash_command;
    let command_actions = crate::state_actions::commands::CommandActions::new(
        crate::state_actions::commands::CommandActionContext {
            workspace: workspace_state,
            chat: chat_state,
            settings,
            ui,
            layout: layout_actions,
            new_session: on_new_session,
            open_local: on_open_local,
            open_remote: on_open_remote,
            open_settings: on_open_settings,
            slash: on_slash_command,
        },
    );
    provide_context(command_actions);
    provide_context(crate::state_actions::omnibar::OmnibarActions::new(
        crate::state_actions::omnibar::OmnibarContext {
            api,
            projects: projects_state,
            chat: chat_state,
            ui,
            commands: command_actions,
            navigation,
            open_file: request_open,
        },
    ));
    install_keyboard_shortcuts(chat_state);

    install_project_effects(ProjectEffectContext {
        api,
        health,
        auth,
        settings,
        projects: projects_state,
        chat: chat_state,
        layout,
        select_project,
    });

    view! {
                    <>
                    <Show
                        when=move || auth_checked.get() && current_user.get().is_some()
                        fallback=move || {
                            view! {
                                <Show
                                    when=move || auth_checked.get()
                                    fallback=move || {
                                        view! {
                                            <div class="auth-gate">
                                                <div class="auth-loading">"Loading…"</div>
                                            </div>
                                        }
                                    }
                                >
                                    <AuthGate />
                                </Show>
                            }
                        }
                    >
                        <div class="app" class:phone-layout=move || layout.phone.get()>
                            <crate::components::CommandDialogs />
                            <TopBar on_select_chat=Callback::new(move |()| { select_chat.run(()); layout_actions.show.run(Panel::Chat); }) on_open_settings=on_open_settings on_logout=on_logout on_open_local=on_open_local on_open_remote=on_open_remote on_open_project=on_open_project on_delete_project=on_delete_project>
                                <TabBar show_chat=false on_select_chat=Callback::new(move |()| { select_chat.run(()); layout_actions.show.run(Panel::Chat); }) on_select=select_project on_tab_action=tab_action on_close=close_project />
                            </TopBar>
                            <crate::components::Configuration on_new_connection=on_new_connection on_edit_connection=on_edit_connection on_cancel_connection=on_cancel_connection on_delete_connection=on_delete_connection on_new_prompt=on_new_prompt on_edit_prompt=on_edit_prompt on_save_prompt=on_save_prompt on_cancel_prompt=on_cancel_prompt on_delete_prompt=on_delete_prompt />
                            <div class=move || format!("app-body{}{}", if active_resizer.get() != ActiveResizer::None { " is-resizing" } else { "" }, if layout.visible_panels.get().editor { "" } else { " editor-collapsed" })>
                            <div class="workspace-docks">
                            <PanelRail panels=vec![Panel::Sessions, Panel::Files, Panel::History, Panel::Editor, Panel::Chat] />
                            <ToolPanel panel=Panel::Sessions>
                            <Sidebar on_select_session=on_select_session on_new_session=on_new_session on_rename_session=on_rename_session on_delete_session=on_delete_session />
                            </ToolPanel>
                            <ToolPanel panel=Panel::Files>
    <crate::components::FilesPanel on_new_file=on_new_file on_new_dir=on_new_dir>
    <SearchPane on_open=request_open on_search_input=on_search_input on_cancel_search=on_cancel_search on_search=on_search on_clear_search=on_clear_search include_ignored=include_ignored_search.read_only() on_toggle_include_ignored=Callback::new(move |()| include_ignored_search.update(|v| *v = !*v))>
                                <div class="files-view" hidden=move || layout.preferences.with(|p| p.files_view == crate::state::responsive::FilesView::Changes)>
                            <FileTree
                                on_toggle=on_toggle
                                on_open=request_open
                                on_grant_access=on_grant_access
                            />
                                </div>
                                <div class="files-view" hidden=move || layout.preferences.with(|p| p.files_view != crate::state::responsive::FilesView::Changes)>
            <GitPane on_show_history=Callback::new(move |()| layout_actions.show.run(Panel::History)) on_commit=git_actions.on_commit on_sync=git_actions.on_sync on_load_branches=git_actions.on_load_branches on_select_branch=git_actions.on_select_branch on_new_branch=on_branch_click on_open=Callback::new(move |path| { request_open.run(path); }) on_load_git_diff=on_load_git_diff on_discard_git_diff=on_discard_git_diff />
                                </div>

    </SearchPane>
                            </crate::components::FilesPanel>
                            </ToolPanel>
                            {move || git_state.file_history.get().map(|path|view!{<crate::components::git_history::FileHistory path=path on_close=Callback::new(move |()|git_state.file_history.set(None)) />})}
                            <ToolPanel panel=Panel::History>
                                <crate::components::git_history::GitHistory on_open=Callback::new(move |path| { request_open.run(path); layout_actions.show.run(Panel::Editor); }) on_select_branch=git_actions.on_select_branch on_new_branch=on_branch_click />
                            </ToolPanel>
                            <ToolPanel panel=Panel::Editor>
                            <div class="center-pane">
                                <Editor
                                    read_only=Signal::derive(move || ws_read_only.get() || chat_state.rewinding.get())
                                    on_open_lossy=on_open_lossy
                                    on_load_git_diff=on_load_git_diff
                                    on_discard_git_diff=on_discard_git_diff
                                    on_save=on_save
                                    on_accept=on_accept
                                    on_reject=on_reject
                                />

                            </div>
                            </ToolPanel>
                            <ToolPanel panel=Panel::Chat>
                            <ChatPane
                                on_select_connection_model=chat_actions.select_connection_model
                                conversation_actions=chat_actions.conversation
                                queue_actions=chat_actions.queue
                                on_send=on_send
                                on_resume_run=chat_actions.resume_run
                                on_stop=on_stop
                                on_permission=on_permission
                                on_permission_always=on_permission_always
                                on_slash_command=on_slash_command
                                on_rewind=chat_actions.rewind
                            />
                            </ToolPanel>
                            <Show when=move || !layout.phone.get() && !layout.visible_panels.get().editor && !layout.visible_panels.get().chat>
                                <div class="panel-empty">"Choose a panel tab to expand it."</div>
                            </Show>
                            </div>
                            <ToolPanel panel=Panel::Terminal>
                                {move || bridge_connection.get().map(|bridge| view! { <TerminalDock bridge=bridge visible=terminal_visible on_close=Callback::new(move |()| layout_actions.toggle.run(Panel::Terminal)) /> })}
                            </ToolPanel>
                        </div>
                        <StatusBar
                            health=health.read_only()
                            on_toggle_terminal=on_toggle_terminal
                            on_branch_click=on_branch_click
                            on_load_branches=git_actions.on_load_branches
                            on_select_branch=git_actions.on_select_branch
                            on_sync_click=on_sync_click
                        />
                        <Show when=move || ui.plugins_open.get()>
                            <crate::components::PluginsDialog/>
                        </Show>
                        <Show when=move || show_settings.get() fallback=|| ()>
                            <Settings
                                on_set_theme=on_set_theme
                                on_set_editor_preferences=on_set_editor_preferences
                                on_set_notifications=on_set_notifications

                                on_set_default_prompt=on_set_default_prompt
                                on_set_bridge_url=on_set_bridge_url
                            />
                        </Show>
                        <ConfirmDialog />
                        <PromptDialog />
                        <Show when=move || show_browser.get() fallback=|| ()>
                            <FileBrowser
                                on_close=on_close_browser
                                on_select=on_browser_select
                            />
                        </Show>
                    </div>
                    </Show>
                    <Show when=move || chat_state.notice.get().is_some()>
                        <div class="toast toast-info" role="status">
                            <span class="toast-message">{move || chat_state.notice.get().unwrap_or_default()}</span>
                            <button class="icon-btn toast-close" title="Dismiss" on:click=move |_| chat_state.notice.set(None)>"×"</button>
                        </div>
                    </Show>
                    <Show when=move || error.get().is_some() fallback=|| ()>
                        <div class="toast" role="alert">
                            <span class="toast-message">{move || error.get().unwrap_or_default()}</span>
                            <button class="icon-btn toast-close" title="Dismiss" on:click=move |_| error.set(None)>
                                "✕"
                            </button>
                        </div>
                    </Show>
                    </>
                }
}
