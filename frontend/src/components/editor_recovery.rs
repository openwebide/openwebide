use leptos::prelude::*;

use super::ui::{Button, ButtonSize, FormNotice, InlineActions, NoticeTone};
use crate::state::{
    editor_recovery::{EditorRecoveryState, RecoveryPhase},
    workspace::WorkspaceState,
};
use crate::state_actions::editor_recovery::RecoveryActions;

#[component]
pub fn RecoveryStatus() -> impl IntoView {
    let workspace = expect_context::<WorkspaceState>();
    let state = use_context::<EditorRecoveryState>();
    let actions = use_context::<RecoveryActions>();
    let projects = expect_context::<crate::state::projects::ProjectsState>();
    let workspace_actions = use_context::<crate::state_actions::workspace::WorkspaceActions>();
    let phase = Memo::new(move |_| {
        workspace.active_project.get().and_then(|id| {
            state.and_then(|state| {
                state
                    .projects
                    .with(|projects| projects.get(&id).map(|entry| entry.phase.clone()))
            })
        })
    });
    let issue = Memo::new(move |_| {
        workspace
            .active_project
            .get()
            .zip(workspace.open_file.get())
            .and_then(|key| {
                workspace
                    .editor_recovery_checks
                    .with(|checks| checks.get(&key).cloned())
            })
    });
    let busy = Memo::new(move |_| {
        matches!(phase.get(), Some(RecoveryPhase::Loading))
            || (issue.get() == Some(crate::state::workspace::RecoveredFileIssue::Pending)
                && !workspace
                    .active_project
                    .get()
                    .is_some_and(|id| projects.needs_grant.with(|ids| ids.contains(&id))))
    });
    view! {
        <WorkspaceRecoveryStatus />
        <Show when=move || phase.get().is_some_and(|phase| phase != RecoveryPhase::Ready) || issue.get().is_some()>
            <div class="editor-recovery" role="status" aria-busy=move || busy.get().to_string()>
                <FormNotice tone=NoticeTone::Warning>
                    {move || match phase.get() {
                        Some(RecoveryPhase::Loading) => "Loading saved editor files…".into(),
                        Some(RecoveryPhase::LoadFailed(error)) => format!("Could not load saved editor files: {error}"),
                        Some(RecoveryPhase::SaveFailed(error)) => format!("Drafts are open here but recovery could not save them: {error}"),
                        Some(RecoveryPhase::Conflict(error)) => format!("Recovery saves are paused: {error}"),
                        _ => issue.get().map(|issue| issue.message()).unwrap_or_default(),
                    }}
                </FormNotice>
                <InlineActions>
                    <Show when=move || workspace.active_project.get().is_some_and(|id| projects.needs_grant.with(|ids| ids.contains(&id)))>
                        <Button size=ButtonSize::Sm on_click=Callback::new(move |_| { if let Some(actions) = workspace_actions { actions.on_grant_access.run(()); } })>"Grant folder access"</Button>
                    </Show>
                    <Show when=move || matches!(phase.get(), Some(RecoveryPhase::LoadFailed(_) | RecoveryPhase::SaveFailed(_)))>
                        <Button size=ButtonSize::Sm on_click=Callback::new(move |_| { if let (Some(actions), Some(id)) = (actions, workspace.active_project.get_untracked()) { actions.retry.run(id); } })>"Retry recovery"</Button>
                    </Show>
                    <Show when=move || matches!(phase.get(), Some(RecoveryPhase::Conflict(_)))>
                        <Button size=ButtonSize::Sm on_click=Callback::new(move |_| { if let (Some(actions), Some(id)) = (actions, workspace.active_project.get_untracked()) { actions.restore.run(id); } })>"Restore saved files"</Button>
                        <Button size=ButtonSize::Sm on_click=Callback::new(move |_| { if let (Some(actions), Some(id)) = (actions, workspace.active_project.get_untracked()) { actions.keep_current.run(id); } })>"Keep this window"</Button>
                    </Show>
                    <Show when=move || issue.get().is_some_and(|issue| issue != crate::state::workspace::RecoveredFileIssue::Pending)>
                        <Button size=ButtonSize::Sm on_click=Callback::new(move |_| { if let (Some(actions), Some(id)) = (actions, workspace.active_project.get_untracked()) { actions.check_files.run(id); } })>"Check disk again"</Button>
                    </Show>
                    <Show when=move || matches!(issue.get(), Some(crate::state::workspace::RecoveredFileIssue::Conflict | crate::state::workspace::RecoveredFileIssue::Missing))>
                        <Button size=ButtonSize::Sm on_click=Callback::new(move |_| { if let Some(actions) = actions { actions.review_file.run(()); } })>"Review recovered file"</Button>
                    </Show>
                </InlineActions>
            </div>
        </Show>        {state.zip(actions).map(|(state, actions)| view! { <RecoveryFileDialog state=state actions=actions /> })}

    }
}

#[component]
fn WorkspaceRecoveryStatus() -> impl IntoView {
    use crate::state::workspace::WorkspaceRecoveryStatus as Status;
    use openwebide_core::plugins::languages::intelligence::actions::ActionPhase;
    let workspace = expect_context::<WorkspaceState>();
    let actions =
        use_context::<crate::state_actions::workspace_recovery::WorkspaceRecoveryActions>();
    let status = Memo::new(move |_| {
        workspace.active_project.get().and_then(|id| {
            workspace
                .workspace_recovery
                .with(|states| states.get(&id).cloned())
        })
    });
    view! {
        <Show when=move || status.get().is_some_and(|status| status.blocks_saves())>
            <div class="editor-recovery" role="status" aria-busy=move || matches!(status.get(), Some(Status::Checking | Status::Pending { recovering: true, .. } | Status::LanguageAction {recovering:true,..})).to_string()>
                <FormNotice tone=NoticeTone::Warning>{move || status.get().map(|status| status.message()).unwrap_or_default()}</FormNotice>
                <InlineActions>
                    <Show when=move || matches!(status.get(), Some(Status::Unavailable(_) | Status::Pending { recovering: false, .. }))>
                        <Button size=ButtonSize::Sm on_click=Callback::new(move |_| {
                            if let (Some(actions), Some(id)) = (actions, workspace.active_project.get_untracked()) { actions.check.run(id); }
                        })>"Check workspace recovery"</Button>
                    </Show>
                    <Show when=move || matches!(status.get(), Some(Status::Pending { recovering: false, .. }))>
                        <Show when=move || actions.is_some_and(|actions| workspace.active_project.get().is_some_and(|id| actions.missing_folders.with(|ids| ids.contains(&id))))>
                            <Button size=ButtonSize::Sm on_click=Callback::new(move |_| {
                                if let (Some(actions), Some(id)) = (actions, workspace.active_project.get_untracked()) { actions.choose_folder.run(id); }
                            })>"Choose original folder"</Button>
                        </Show>
                        <Show when=move || actions.is_some_and(|actions| workspace.active_project.get().is_some_and(|id| actions.folder_access.with(|ids| ids.contains(&id))))>
                            <Button size=ButtonSize::Sm on_click=Callback::new(move |_| {
                                if let (Some(actions), Some(id)) = (actions, workspace.active_project.get_untracked()) { actions.grant.run(id); }
                            })>"Grant original folder access"</Button>
                        </Show>
                        <Button size=ButtonSize::Sm on_click=Callback::new(move |_| {
                            if let (Some(actions), Some(id)) = (actions, workspace.active_project.get_untracked()) { actions.recover.run(id); }
                        })>{move ||status.get().map(|status|status.recovery_label()).unwrap_or("Restore interrupted changes")}</Button>
                    </Show>
                    <Show when=move || matches!(status.get(),Some(Status::LanguageAction {recovering:false,..}))>
                        <Button size=ButtonSize::Sm on_click=Callback::new(move |_| {
                            if let (Some(actions),Some(id))=(actions,workspace.active_project.get_untracked()) {actions.check_command.run(id);}
                        })>"Check command outcome"</Button>
                        <Show when=move || matches!(status.get(),Some(Status::LanguageAction {phase:ActionPhase::Running,..}))>
                            <Button size=ButtonSize::Sm on_click=Callback::new(move |_| {
                                if let (Some(actions),Some(id))=(actions,workspace.active_project.get_untracked()) {actions.stop_command.run(id);}
                            })>"Stop pending command"</Button>
                        </Show>
                        <Show when=move || matches!(status.get(),Some(Status::LanguageAction {phase:ActionPhase::Approved,..}))>
                            <Button size=ButtonSize::Sm on_click=Callback::new(move |_| {
                                if let (Some(actions),Some(id))=(actions,workspace.active_project.get_untracked()) {actions.discard_command.run(id);}
                            })>"Discard unstarted command"</Button>
                        </Show>
                        <Show when=move || matches!(status.get(),Some(Status::LanguageAction {phase:ActionPhase::Rejected|ActionPhase::Uncertain,..}))>
                            <Button size=ButtonSize::Sm on_click=Callback::new(move |_| {
                                if let (Some(actions),Some(id))=(actions,workspace.active_project.get_untracked()) {actions.acknowledge_command.run(id);}
                            })>{move ||if matches!(status.get(),Some(Status::LanguageAction {phase:ActionPhase::Uncertain,..})) {"Stop and acknowledge reviewed effects"} else {"Acknowledge reviewed outcome"}}</Button>
                        </Show>
                    </Show>
                </InlineActions>
            </div>
        </Show>
    }
}

#[component]
fn RecoveryFileDialog(state: EditorRecoveryState, actions: RecoveryActions) -> impl IntoView {
    let close = actions.close_file_review;
    view! {
        <Show when=move || state.file_review.get().is_some()>
            <super::modal::Modal title=Signal::derive(move || state.file_review.with(|review| review.as_ref().map(|review| format!("Recovered file: {}", review.path)).unwrap_or_default())) on_close=close size=super::ui::DialogSize::Wide>
                <super::ui::DialogBody>
                    <FormNotice tone=NoticeTone::Warning>"Reload disk discards this draft. Save draft replaces the disk file with this draft. Changes made after this review opened require another review."</FormNotice>
                    {move || state.file_review.get().map(|review| super::editor::render_inline_diff(openwebide_core::FileDiff { path: review.path, old: review.disk, new: review.draft, old_unavailable: false, backup_path: None }))}
                </super::ui::DialogBody>
                <super::ui::DialogActions>
                    <Button on_click=Callback::new(move |_| close.run(()))>"Cancel"</Button>
                    <Button disabled=Signal::derive(move || state.file_review.with(|review| review.as_ref().is_none_or(|review| review.disk.is_none()))) on_click=Callback::new(move |_| actions.reload_file.run(()))>"Reload disk"</Button>
                    <Button disabled=Signal::derive(move || state.file_review.with(|review| review.as_ref().is_none_or(|review| review.read_only))) variant=super::ui::ButtonVariant::Danger on_click=Callback::new(move |_| actions.overwrite_file.run(()))>"Save draft"</Button>
                </super::ui::DialogActions>
            </super::modal::Modal>
        </Show>
    }
}
