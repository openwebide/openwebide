use crate::state::{git::GitState, workspace::WorkspaceState};
use leptos::prelude::*;

/// Git status and diffs use the same shared actions and diff renderer as the editor.
#[component]
pub fn GitPane(
    on_open: Callback<String>,
    #[prop(default = Callback::new(|()| ()))] on_show_history: Callback<()>,
    #[prop(default = Callback::new(|()| ()))] on_load_branches: Callback<()>,
    #[prop(default = Callback::new(|_: String| ()))] on_select_branch: Callback<String>,
    #[prop(default = Callback::new(|()| ()))] on_new_branch: Callback<()>,
    #[prop(default = Callback::new(|_: String| ()))] on_sync: Callback<String>,
    #[prop(default = Callback::new(|_: openwebide_core::GitCommitRequest| ()))] on_commit: Callback<
        openwebide_core::GitCommitRequest,
    >,
    on_load_git_diff: Callback<()>,
    on_discard_git_diff: Callback<()>,
) -> impl IntoView {
    let git = expect_context::<GitState>();
    let workspace = expect_context::<WorkspaceState>();
    let changes = crate::state_actions::git_changes::GitChangesActions::new();
    let chat = expect_context::<crate::state::chat::ChatState>();
    let show_result = RwSignal::new(false);
    let layout = expect_context::<crate::state::layout::LayoutState>();
    let assistance = crate::state_actions::git_assistance::GitAssistance::new(
        expect_context(),
        expect_context(),
        expect_context(),
        expect_context(),
        expect_context(),
        expect_context(),
    );
    assistance.auto_commit(Signal::derive(move || {
        layout.visible_panels.get().files
            && layout
                .preferences
                .with(|prefs| prefs.files_view == crate::state::responsive::FilesView::Changes)
    }));
    Effect::new(move |_| {
        workspace.open_file.track();
        if layout.visible_panels.get().files
            && layout
                .preferences
                .with(|prefs| prefs.files_view == crate::state::responsive::FilesView::Changes)
            && git.status.get().is_some()
            && workspace.open_file.get_untracked().is_some()
        {
            on_load_git_diff.run(());
        }
    });
    view! { <div class="git-pane" class:compact-tree=move || !layout.phone.get()>
            <super::ui::PanelToolbar class="file-tree-header git-pane-toolbar">
                <super::BranchPicker on_load=on_load_branches on_select=on_select_branch on_new=on_new_branch />
                <super::ui::IconButton label="Show History" on_click=Callback::new(move |_| on_show_history.run(()))><super::ui::Icon name=super::ui::IconName::FileClock /></super::ui::IconButton>
                <div class="ui-actions">
                <Show when=move || changes.loading.get() || assistance.busy.get() ||git.status_loading.get() ||git.sync_busy.get().is_some() || git.commit_busy.get()><span class="ui-progress-slot"><super::ui::LoadingStatus label=Signal::derive(move ||if assistance.busy.get(){"Drafting Git descriptions…".to_owned()}else if git.sync_busy.get().is_some(){"Running Git operation and updating changes…".to_owned()}else if git.commit_busy.get(){"Committing and updating changes…".to_owned()}else{"Refreshing Git status and changes…".to_owned()}) compact=true /></span></Show>
                <super::dropdown::ActionMenu aria_label="Git actions">
                    { ["sync", "fetch", "pull", "push"].into_iter().map(|action| view! {
                        <button role="menuitem" class="ui-dropdown-item recent-item btn" disabled=move || git.sync_busy.get().is_some() || git.branch_busy.get() || git.commit_busy.get() on:click=move |_| on_sync.run(action.into())><super::ui::Icon name=match action {"sync"=>super::ui::IconName::ArrowDownUp,"fetch"=>super::ui::IconName::RefreshCw,"pull"=>super::ui::IconName::ArrowDown,_=>super::ui::IconName::ArrowUp} /><span>{match action {"sync"=>"Sync","fetch"=>"Fetch", "pull"=>"Pull", _=>"Push"}}</span></button>
                    }).collect_view() }
                    <button type="button" role="menuitem" class="ui-dropdown-item recent-item btn" disabled=move ||git.sync_notice.get().is_none() on:click=move |_|show_result.set(true)><super::ui::Icon name=super::ui::IconName::Info /><span>"Last Git operation details"</span></button>
                </super::dropdown::ActionMenu>
                </div>
            </super::ui::PanelToolbar>
            <super::ui::FeedbackOverlay message=git.status_error.read_only() on_dismiss=Callback::new(|()|()) />
            <super::ui::FeedbackOverlay message=git.sync_error.read_only() on_dismiss=Callback::new(|()|()) />
            <Show when=move ||show_result.get() &&git.sync_notice.get().is_some()><div class="git-sync-result ui-feedback-overlay"><pre>{move ||git.sync_notice.get().unwrap_or_default()}</pre><super::ui::IconButton label="Dismiss Git operation details" on_click=Callback::new(move |_|show_result.set(false))><super::ui::Icon name=super::ui::IconName::X /></super::ui::IconButton></div></Show>
            <div class="git-working-tree">
            <form class="git-commit-form" on:submit=move |event| {event.prevent_default(); on_commit.run(openwebide_core::GitCommitRequest {message:git.commit_message.get_untracked(),paths:None,include_untracked:false,staged_only:true});}>
                <div class="git-commit-heading"><span class="form-label">"Commit message"</span><div class="ui-actions"><span class="ui-disabled-help" tabindex="0" aria-label=move ||if git.path_changes.with(|changes|changes.as_ref().is_none_or(|changes|changes.staged.is_empty())) {"Stage changes to draft a commit message"}else{"Draft staged commit message"} title=move ||if git.path_changes.with(|changes|changes.as_ref().is_none_or(|changes|changes.staged.is_empty())) {"Stage changes to draft a commit message"}else{"Draft staged commit message"}><super::ui::IconButton label="Draft staged commit message" disabled=Signal::derive(move ||assistance.busy.get() ||chat.streaming.get() ||git.commit_busy.get() ||git.branch_busy.get() ||git.sync_busy.get().is_some() ||git.path_changes.with(|changes|changes.as_ref().is_none_or(|changes|changes.staged.is_empty()))) on_click=Callback::new(move |_|assistance.generate_staged.run(()))><super::ui::Icon name=super::ui::IconName::Sparkles /></super::ui::IconButton></span><super::git_assistance::GitDrafts actions=assistance /></div></div>
                <super::ui::TextArea label="Commit message" value=git.commit_message.read_only() on_change=Callback::new(move |message|git.commit_message.set(message)) rows=3 maxlength=65536 disabled=Signal::derive(move ||git.commit_busy.get()) />
                <button class="btn send git-commit-submit" type="submit" disabled=move ||git.status_error.get().is_some() ||git.commit_busy.get() ||git.branch_busy.get() ||git.sync_busy.get().is_some() ||git.commit_message.with(|message|message.trim().is_empty()) ||git.path_changes.with(|changes|changes.as_ref().is_none_or(|changes|changes.staged.is_empty()))><super::ui::Icon name=super::ui::IconName::GitCommitHorizontal />"Commit staged changes"</button>
                <super::ui::FeedbackOverlay message=git.commit_error.read_only() on_dismiss=Callback::new(|()|()) />
                <p class="form-hint">"Commits only the staged changes. Save editor changes before staging."</p>
            </form>
            <super::git_changes::GitChanges on_open=on_open actions=changes />
            <div class="git-diff-actions" data-context-menu="">
                <span class="git-diff-path">{move || workspace.open_file.get().unwrap_or_else(|| "Select a changed file".into())}</span>
    <super::dropdown::ActionMenu aria_label="Diff actions">            <button role="menuitem" class="ui-dropdown-item recent-item btn" disabled=move || workspace.open_file.get().is_none() || git.status.get().is_none() on:click=move |_| on_load_git_diff.run(())><super::ui::Icon name=super::ui::IconName::RefreshCw /><span>"Refresh diff"</span></button>
                <button role="menuitem" class="ui-dropdown-item recent-item btn" disabled=move || !git.can_revert(workspace.active_project.get(), workspace.open_file.get().as_deref()) on:click=move |_| on_discard_git_diff.run(())><super::ui::Icon name=super::ui::IconName::Undo2 /><span>"Revert file"</span></button></super::dropdown::ActionMenu>
            </div>
            {move || git.head_diff(workspace.active_project.get(), workspace.open_file.get(), workspace.content.get().into()).map(super::editor::render_inline_diff)}
            </div>
        </div> }
}
