//! Initial review authority and durable acceptance share the worker CAS commit.
//! Cleanup/recovery of accepted work never needs the original short-lived grant.
use super::*;
use openwebide_core::workspace_transaction::{LanguageReview, WorkspaceTransactionScope};

fn rejected() -> StorageError {
    StorageError::Conflict("The original native language review and current manual authority are required for admission".into())
}
impl<D: Db> Store<D> {
    pub(super) async fn accept_language_review(
        &self,
        user: UserId,
        project: i64,
        scope: &WorkspaceTransactionScope,
        admission: Option<(&str, &str)>,
        now: i64,
    ) -> Result<Option<String>, StorageError> {
        // This runs only inside the worker admission transaction. Do not load
        // the large resource/editor snapshots a second time for authority.
        let result = self.db.execute(
            "SELECT language_review,progress FROM workspace_transactions WHERE user_id=? AND project_id=? AND id=? AND generation=? AND revision=?",
            &[DbValue::Int(user.get()), DbValue::Int(project), DbValue::Text(scope.id.clone()), DbValue::Text(scope.generation.clone()), DbValue::Int(i64::try_from(scope.revision).map_err(|_| rejected())?)],
        ).await?;
        let row = result.rows.first().ok_or_else(rejected)?;
        let Some(encoded) = row.get_text_opt(0) else {
            if admission.is_some() || scope.accepted_language_review_digest.is_some() {
                return Err(rejected());
            }
            return Ok(None);
        };
        let digest = openwebide_core::plugins::content_digest(encoded.as_bytes());
        if admission.is_some_and(|(verified, _)| verified != digest) {
            return Err(rejected());
        }
        if let Some(accepted) = &scope.accepted_language_review_digest {
            if accepted != &digest {
                return Err(rejected());
            }
            return Ok(Some(digest));
        }
        let progress: TransactionProgress = serde_json::from_str(row.get_text(1)?)
            .map_err(|error| StorageError::InvalidValue(error.to_string()))?;
        // Account cancellation before any intent grants no new mutation
        // authority. Terminal cleanup must not resurrect its expired review.
        if progress == TransactionProgress::prepared_cancellation() && admission.is_none() {
            return Ok(None);
        }
        let (_, grant) = admission.ok_or_else(rejected)?;
        if !valid_id(grant) {
            return Err(rejected());
        }
        if progress != TransactionProgress::default() {
            return Err(rejected());
        }
        let review: LanguageReview = serde_json::from_str(encoded)
            .map_err(|error| StorageError::InvalidValue(error.to_string()))?;
        review.validate().map_err(StorageError::InvalidValue)?;
        let (prepared, context) = self
            .plugin_grant_in_transaction(user, None, grant, now)
            .await?;
        if prepared != review.prepared
            || !context.user_action
            || context.project_id != Some(project)
            || scope.root
                != EditorRecoveryRoot::for_project(&self.get_project(project, user).await?)
        {
            return Err(rejected());
        }
        self.validate_enabled_plugin(user, Some(project), &prepared)
            .await?;
        let current = self
            .project_language_context_in_transaction(user, project)
            .await?;
        review
            .validate_provider(
                &openwebide_core::plugins::languages::LanguageRegistry::from_bindings(
                    &current.bindings,
                ),
                &current.preferences,
            )
            .map_err(StorageError::Conflict)?;
        if matches!(
            review.input.query,
            openwebide_core::plugins::languages::intelligence::Query::ReviewAction { .. }
        ) {
            let complete: openwebide_core::plugins::languages::intelligence::actions::ActionReview =
                serde_json::from_value(review.result.clone()).map_err(|_| rejected())?;
            if complete.command.is_some() {
                let action = self
                    .language_action(user, project, Some(&scope.id))
                    .await?
                    .ok_or_else(rejected)?;
                if action.phase != openwebide_core::plugins::languages::intelligence::actions::ActionPhase::Approved
                    || action.approval.prepared != review.prepared
                    || action.approval.binding_revision != review.binding_revision
                    || action.approval.input != review.input
                    || action.approval.review != complete
                { return Err(rejected()); }
            }
        }
        Ok(Some(digest))
    }
}
