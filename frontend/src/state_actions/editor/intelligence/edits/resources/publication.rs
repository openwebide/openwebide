//! Publish the durable editor decision independently of private cleanup.
use super::*;
use crate::state_actions::editor_recovery::RecoveryPause;
use openwebide_core::workspace_transaction::WorkspaceTransactionState;

impl EditorIntelligence {
    pub(super) async fn publish_resource_decision(
        self,
        review: &WorkspaceResourceReview,
        state: &WorkspaceTransactionState,
        pause: &mut RecoveryPause,
        prepared: PreparedEditorRecovery,
        after: &EditorRecovery,
        current: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let context = self
            .resource_context
            .ok_or("Workspace recovery is unavailable")?;
        if state.journal.id != review.id || !state.journal.progress.commits_editor() {
            return Err("The workspace operation has no committed editor decision".into());
        }
        let fail = |message: &str| {
            if context.root_current(
                &review.project,
                &review.root,
                review.snapshot.presentation.account,
            ) {
                context
                    .recovery
                    .publication_failed(review.project.id, message.into());
            }
            message.to_string()
        };
        if !current() {
            return Err(fail(
                "Workspace changes committed after the editor context changed. Review the saved editor files before continuing.",
            ));
        }
        let record=context.api.with_value(Clone::clone).editor_recovery(review.project.id).await.map_err(|error|fail(&format!("Workspace changes committed, but saved editor files could not be loaded: {error}")))?;
        if !current()
            || record.state != *after
            || !self.editor.workspace.editor_recovery_guard_matches(
                &review.project,
                &review.guard,
                context.recovery.file_read_only(),
            )
        {
            return Err(fail(
                "Workspace changes committed, but the editor changed before publication. Review the saved editor files before continuing.",
            ));
        }
        let hydrated = batch(|| {
            pause.acknowledge(record.clone())?;
            Ok::<_, String>(self.editor.workspace.install_editor_recovery(
                &review.project,
                &record.state,
                prepared,
            ))
        })
        .map_err(|error| fail(&error))?;
        context
            .recovery
            .workspace_published(review.project.id, &review.id, &record, hydrated);
        Ok(())
    }
}
