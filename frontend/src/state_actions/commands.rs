//! One command facade for palette selection and global keyboard shortcuts.
use crate::{
    commands::{Command, CommandContext},
    state::{
        auth::AuthState, chat::ChatState, settings::SettingsState, ui::UiState,
        workspace::WorkspaceState,
    },
    state_actions::layout::LayoutActions,
};
use leptos::prelude::*;
use openwebide_core::SlashCommand;
use wasm_bindgen::JsCast;

#[derive(Clone, Copy)]
pub struct CommandActions {
    pub context: Memo<CommandContext>,
    pub scope: Memo<(u64, Option<i64>, Option<i64>)>,
    pub run: Callback<Command>,
}
pub struct CommandActionContext {
    pub workspace: WorkspaceState,
    pub chat: ChatState,
    pub settings: SettingsState,
    pub ui: UiState,
    pub layout: LayoutActions,
    pub new_session: Callback<()>,
    pub open_local: Callback<()>,
    pub open_remote: Callback<()>,
    pub open_settings: Callback<()>,
    pub slash: Callback<SlashCommand>,
}
impl CommandActions {
    pub fn new(context: CommandActionContext) -> Self {
        let CommandActionContext {
            workspace,
            chat,
            settings,
            ui,
            layout,
            new_session,
            open_local,
            open_remote,
            open_settings,
            slash,
        } = context;
        let git = expect_context::<crate::state::git::GitState>();
        let git_actions = use_context::<super::git::GitActions>();
        let context = Memo::new(move |_| CommandContext {
            can_convert: ui.editor_can_convert.get(),
            can_discard: !chat.streaming.get()
                && !chat.rewinding.get()
                && workspace
                    .pending_edits
                    .with(std::collections::HashMap::is_empty)
                && git.head_content.get().is_some_and(|head| {
                    head.project_id == workspace.active_project.get()
                        && Some(head.path) == workspace.open_file.get()
                        && head.content.is_ok()
                })
                && git_actions.is_some(),
            project: workspace.active_project.get().is_some(),
            editor: workspace.open_file.get().is_some(),
            running: chat.streaming.get()
                && chat.streaming_session.get() == chat.active_session.get(),
        });
        let auth = expect_context::<AuthState>();
        let editor = super::editor::EditorActions::new(workspace);
        let scope = Memo::new(move |_| {
            (
                auth.generation.get(),
                workspace.active_project.get(),
                chat.active_session.get(),
            )
        });
        let run = Callback::new(move |command: Command| {
            if let Some(reason) = command.unavailable(context.get_untracked()) {
                ui.notify(reason);
                return;
            }
            ui.about_open.set(false);
            ui.palette_open.set(false);
            ui.shortcuts_open.set(false);
            match command {
                Command::ConvertIndentation => {
                    if let Some(action) = ui.editor_convert.get_untracked() {
                        after_close(auth, workspace, chat, move || action.run(()));
                    }
                }
                Command::DiscardChanges => {
                    if let Some(actions) = git_actions {
                        actions.on_discard_diff.run(());
                    }
                }
                Command::Palette => ui.palette_open.set(true),
                Command::About => ui.about_open.set(true),
                Command::Shortcuts => ui.shortcuts_open.set(true),
                Command::NewSession => new_session.run(()),
                Command::OpenLocal => open_local.run(()),
                Command::OpenRemote => open_remote.run(()),
                Command::Settings => open_settings.run(()),
                Command::ModelSetup => {
                    settings.begin_model_setup(settings.default_connection.get_untracked());
                }
                Command::TogglePanel(panel) => layout.toggle.run(panel),
                Command::ToggleTerminal => {
                    layout.toggle.run(crate::state::layout::Panel::Terminal);
                    chat.show_terminal.update(|visible| *visible = !*visible);
                }
                Command::Context => slash.run(SlashCommand::Context),
                Command::Stop => slash.run(SlashCommand::Stop),
                Command::CaptureEditor => {
                    if let Some(context) = super::lifecycle::capture_active_editor(
                        editor,
                        workspace.open_file.get_untracked(),
                        &workspace.content.get_untracked(),
                    ) {
                        chat.active_editor_context.set(Some(context));
                        layout.show.run(crate::state::layout::Panel::Chat);
                        after_close(auth, workspace, chat, focus_chat);
                    }
                }
                Command::FocusChat => {
                    layout.show.run(crate::state::layout::Panel::Chat);
                    after_close(auth, workspace, chat, focus_chat);
                }
                Command::CycleFocus => {
                    after_close(auth, workspace, chat, super::lifecycle::cycle_focus);
                }
            }
        });
        Self {
            context,
            scope,
            run,
        }
    }
}
fn after_close(
    auth: AuthState,
    workspace: WorkspaceState,
    chat: ChatState,
    action: impl FnOnce() + 'static,
) {
    let epoch = auth.generation.get_untracked();
    let project = workspace.active_project.get_untracked();
    let session = chat.active_session.get_untracked();
    request_animation_frame(move || {
        if auth.generation.try_get_untracked() == Some(epoch)
            && workspace.active_project.try_get_untracked() == Some(project)
            && chat.active_session.try_get_untracked() == Some(session)
        {
            action();
        }
    });
}
fn focus_chat() {
    if let Some(document) = web_sys::window().and_then(|window| window.document())
        && let Ok(Some(element)) = document.query_selector(".composer-input")
        && let Some(element) = element.dyn_ref::<web_sys::HtmlElement>()
    {
        let _ = element.focus();
    }
}
