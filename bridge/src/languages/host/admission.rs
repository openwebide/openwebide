//! Verify the original result under its folder lease without a new query,
//! source synchronization, document watermark or replacement service.
use super::*;
use crate::folder_coordination::{Folder, Lease};
use openwebide_core::workspace_transaction::{ResourceScope, WorkspaceTransactionState};

pub(crate) struct ReviewGuard<'lease> {
    service: ServiceUse,
    affected: analysis::affected::Snapshot,
    _documents: tokio::sync::OwnedMutexGuard<documents::Documents>,
    folder: Folder,
    lease: &'lease Lease,
}
impl std::fmt::Debug for ReviewGuard<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ReviewGuard")
    }
}
impl ReviewGuard<'_> {
    pub(crate) fn ensure_current(&self) -> Result<(), String> {
        if *self.service.0.stopped.borrow()
            || self
                .service
                .0
                .session
                .get()
                .is_none_or(|session| session.client.is_closed())
            || !self.lease.covers(&self.folder)
        {
            return Err(
                "The original language service or physical folder changed during admission".into(),
            );
        }
        self.affected.ensure_current(&self.folder.path)
    }
}
impl Host {
    pub(crate) async fn validate_workspace_review<'lease>(
        &self,
        scope: Scope,
        state: &WorkspaceTransactionState,
        lease: &'lease Lease,
    ) -> Result<ReviewGuard<'lease>, String> {
        tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.verify_workspace_review(scope, state, lease),
        )
        .await
        .map_err(|_| "Native language review verification timed out".to_string())?
    }
    async fn verify_workspace_review<'lease>(
        &self,
        scope: Scope,
        state: &WorkspaceTransactionState,
        lease: &'lease Lease,
    ) -> Result<ReviewGuard<'lease>, String> {
        let review = state
            .language_review
            .as_ref()
            .ok_or("The original language review is unavailable")?;
        review.validate_transaction(&state.journal.plan, &state.editor)?;
        let folder = Folder::capture(&scope.root)?;
        if !lease.covers(&folder) {
            return Err("Language review verification requires its original folder lease".into());
        }
        if scope.owner.is_empty()
            || scope.project <= 0
            || scope.provider != review.prepared.storage_namespace()
            || scope.digest != review.prepared.digest
            || &scope.server != protocol::declared_server(&review.prepared, &review.input.language)?
        {
            return Err("The retained language provider differs from its native scope".into());
        }
        let mut server = scope.server.clone();
        server.language_id.take();
        let server = serde_json::to_string(&server).map_err(|error| error.to_string())?;
        let service = {
            let entries = self.inner.entries.lock().unwrap();
            let matching = |key: &Key| {
                key.owner == scope.owner
                    && key.project == scope.project
                    && key.root == folder.path
                    && key.provider == scope.provider
                    && key.digest == scope.digest
                    && key.server == server
                    && key.client == review.input.client
            };
            let (key, entry) = entries.iter().find(|(key, entry)| matching(key)
                && !*entry.stopped.borrow()
                && entry.session.get().is_some_and(|session| session.id == review.context.server))
                .ok_or("The original language service is unavailable; native admission never starts a replacement")?;
            // Another retained workspace cannot loan this review an exception
            // to the ordinary pending-command fence.
            if entries.iter().any(|(key, other)| {
                matching(key)
                    && !Arc::ptr_eq(other, entry)
                    && !*other.stopped.borrow()
                    && other
                        .session
                        .get()
                        .is_some_and(commands::has_pending_action)
            }) {
                return Err(
                    "Another reviewed command is being applied in this language scope".into(),
                );
            }
            prefetch::admit(&self.inner, &entries, key, &review.input)?;
            entry
                .active_uses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            ServiceUse(entry.clone())
        };
        let entry = &service.0;
        let session = entry
            .session
            .get()
            .ok_or("The original language service is unavailable")?;
        let documents = session.documents.clone().lock_owned().await;
        entry.check_context(session).await?;
        let path = documents::source_path(&folder.path, &review.input.path).await?;
        if !documents.has_source(&review.input, &path) {
            return Err("The original analysis document changed or closed".into());
        }
        let inputs = session.inputs.observe(&session.root).await?;
        if !session.inputs.is_baseline(&inputs) {
            return Err("The original language review's declared project inputs changed".into());
        }
        commands::validate_workspace_review(
            session,
            &review.input,
            &review.result,
            inputs.revision,
        )?;
        let files = ProjectFiles::for_server(&review.prepared, &review.input.language)?;
        let sources = session
            .sources
            .get()
            .filter(|sources| sources.matches(&files))
            .ok_or("The original analysis has no matching complete project-source observation")?;
        let before = sources.observe(&session.root).await?;
        let affected = session.analysis.verify_original(
            &review.context,
            &session.id,
            &review.input,
            before,
            documents.overlay_revision(),
            &session.root,
        )?;
        let files = crate::runs::native_vfs::resources::ResourceFiles::reviewed(
            folder.handle.as_file(),
            &state.journal.plan.root,
            ResourceScope {
                transaction: state.journal.id.clone(),
                fingerprint: state.journal.plan.fingerprint()?,
            },
            (state.journal.plan.version == 2).then_some(review.as_ref()),
            &state.journal.plan,
        )?;
        review
            .reproduce(&files, &state.journal.plan, &state.editor, || {
                lease.covers(&folder) && !*entry.stopped.borrow()
            })
            .await?;
        let after = sources.observe(&session.root).await?;
        let after_inputs = session.inputs.observe(&session.root).await?;
        if after != before || !session.inputs.is_baseline(&after_inputs) {
            return Err("Project files changed during native language review verification".into());
        }
        entry.check_context(session).await?;
        commands::validate_workspace_review(
            session,
            &review.input,
            &review.result,
            after_inputs.revision,
        )?;
        let guard = ReviewGuard {
            affected,
            service,
            _documents: documents,
            folder,
            lease,
        };
        guard.ensure_current()?;
        Ok(guard)
    }
}

#[cfg(test)]
mod tests;
