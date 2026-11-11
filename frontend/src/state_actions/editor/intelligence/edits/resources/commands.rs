//! Editor integration above the shared action client and workspace transaction.
use super::*;
use crate::{
    plugin_actions::{ProjectLanguageActions, ProjectLanguageService},
    state::workspace::WorkspaceRecoveryStatus,
};
use openwebide_core::plugins::languages::intelligence::actions::{
    ActionJournalRequest, ActionRecord, CommandOperation, CommandOutcome, CommandProgress,
};

pub(super) async fn client(
    context: ResourceContext,
    review: &WorkspaceResourceReview,
) -> Result<ProjectLanguageActions, String> {
    let project = review.project.clone();
    let root = review.root.clone();
    let account = review.snapshot.presentation.account;
    crate::plugin_actions::project_language_actions(
        context.api,
        context
            .host
            .ok_or("The language execution host is unavailable")?,
        project.id,
        review.snapshot.service.preferences.clone(),
        move || context.root_current(&project, &root, account),
    )
    .await
}

async fn service(
    editor: EditorIntelligence,
    context: ResourceContext,
    review: &WorkspaceResourceReview,
    approved: &ActionRecord,
) -> Result<ProjectLanguageService, String> {
    let state = editor
        .editor
        .languages
        .ok_or("Language synchronization is unavailable")?;
    let namespace = format!(
        "plugin:{}/language:{}",
        approved.approval.prepared.storage_namespace(),
        approved.approval.input.language
    );
    let provider = review
        .snapshot
        .service
        .registry
        .find(&namespace)
        .ok_or("The approved language provider is unavailable")?
        .clone();
    let project = review.project.clone();
    let root = review.root.clone();
    let account = review.snapshot.presentation.account;
    Ok(ProjectLanguageService::new(
        context.api,
        context
            .host
            .ok_or("The language execution host is unavailable")?,
        project.id,
        provider,
        review.snapshot.service.preferences.clone(),
        move || context.root_current(&project, &root, account),
    )
    .await?
    .with_sequence(state.intelligence_sequence.get_value()))
}
pub(super) async fn prepare(
    editor: EditorIntelligence,
    context: ResourceContext,
    review: &WorkspaceResourceReview,
    approved: &ActionRecord,
) -> Result<(), String> {
    service(editor, context, review, approved)
        .await?
        .prepare_action_synchronization(
            &approved.approval,
            editor.editor.rules_untracked().indentation.tab_width(),
        )
        .await
}
pub(super) async fn complete(
    editor: EditorIntelligence,
    context: ResourceContext,
    review: &WorkspaceResourceReview,
    client: &ProjectLanguageActions,
    approved: &ActionRecord,
    after: &EditorRecovery,
    lock: &mut Option<crate::state::workspace::WorkspaceEditGuard>,
) -> Result<(), String> {
    let state = editor
        .editor
        .languages
        .ok_or("Language synchronization is unavailable")?;
    let service = service(editor, context, review, approved).await?;
    service
        .synchronize_action(
            review.project.id,
            &approved.approval,
            after,
            editor.source_revision.get_untracked(),
            editor.editor.rules_untracked().indentation.tab_width(),
            &state.intelligence_documents.get_value(),
        )
        .await?;
    let claimed = client.claim(approved).await?;
    editor
        .editor
        .workspace
        .workspace_recovery
        .try_update(|states| {
            if let Some(WorkspaceRecoveryStatus::LanguageAction {
                id,
                phase,
                recovering,
                error,
                ..
            }) = states.get_mut(&review.project.id)
                && id == &claimed.approval.id
            {
                *phase = claimed.phase;
                *recovering = false;
                *error = None;
            }
        });
    // The durable command fence now owns disk-write exclusion. Release editor
    // I/O ownership so drafts persist and the same window can stop the command.
    drop(lock.take());
    if editor.editor.workspace.active_project.get_untracked() == Some(review.project.id)
        && editor.resources.try_with_untracked(|current| {
            current
                .as_ref()
                .is_some_and(|current| std::ptr::eq(current.as_ref(), review))
        }) == Some(true)
    {
        editor.result.try_set(None);
    }
    // A lost start acknowledgement is recovery work. Do not issue Start again.
    let mut progress = client.command(&claimed, CommandOperation::Start).await?;
    let deadline = js_sys::Date::now() + 90_000.0;
    loop {
        match progress {
            CommandProgress::Finished {outcome}=> {
                client.request(&ActionJournalRequest::Finish {id:claimed.approval.id.clone(),revision:claimed.revision,outcome:outcome.clone()}).await?.ok_or("The language command outcome was not acknowledged")?;
                return match outcome {
                    CommandOutcome::Completed {..}=>Ok(()),
                    CommandOutcome::Rejected {message}|CommandOutcome::Uncertain {message}=>Err(message),
                };
            }
            CommandProgress::Ready=>return Err("The approved command was not dispatched. Review its pending outcome before continuing.".into()),
            CommandProgress::Running=>{},
        }
        if js_sys::Date::now() > deadline {
            return Err(
                "The language command is still pending. Check its outcome before continuing."
                    .into(),
            );
        }
        crate::util::sleep_ms(100).await;
        progress = client.command(&claimed, CommandOperation::Status).await?;
    }
}

pub(super) async fn refresh(
    context: ResourceContext,
    editor: EditorIntelligence,
    review: &WorkspaceResourceReview,
    error: Option<String>,
) {
    if editor
        .editor
        .workspace
        .editor_operation_active_untracked(review.project.id)
    {
        return;
    }
    if !context.root_current(
        &review.project,
        &review.root,
        review.snapshot.presentation.account,
    ) {
        return;
    }
    let backend = context.api.with_value(Clone::clone);
    let result = async {
        let workspace = backend
            .workspace_transaction(review.project.id, Some(&review.id))
            .await
            .map_err(|e| e.to_string())?;
        let action = backend
            .language_action_request(
                review.project.id,
                &ActionJournalRequest::Load {
                    id: Some(review.id.clone()),
                },
            )
            .await?;
        Ok::<_, String>(WorkspaceRecoveryStatus::from_records(
            workspace.as_ref(),
            action.as_ref(),
        ))
    }
    .await;
    if !context.root_current(
        &review.project,
        &review.root,
        review.snapshot.presentation.account,
    ) {
        return;
    }
    let mut status = result.unwrap_or_else(WorkspaceRecoveryStatus::Unavailable);
    if let WorkspaceRecoveryStatus::LanguageAction { error: issue, .. } = &mut status
        && error.is_some()
    {
        *issue = error;
    }
    if editor
        .editor
        .workspace
        .editor_operation_active_untracked(review.project.id)
    {
        return;
    }
    editor
        .editor
        .workspace
        .workspace_recovery
        .try_update(|states| {
            let replaced = match states.get(&review.project.id) {
                Some(
                    WorkspaceRecoveryStatus::EditorConflict(_)
                    | WorkspaceRecoveryStatus::Unavailable(_),
                ) => true,
                Some(
                    WorkspaceRecoveryStatus::Pending { id, .. }
                    | WorkspaceRecoveryStatus::LanguageAction { id, .. },
                ) => id != &review.id,
                _ => false,
            };
            if !replaced {
                states.insert(review.project.id, status);
            }
        });
}
