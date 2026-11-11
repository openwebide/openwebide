//! Worker admission is a durable CAS against the complete compact scope. Only
//! runtimes holding the physical folder gate may replace or release a worker.
//! Released IDs remain as tombstones, preventing delayed admission from reviving.
use super::*;
use openwebide_core::workspace_transaction::{
    WorkspaceTransactionScope, WorkspaceTransactionWorker,
};

fn conflict() -> StorageError {
    StorageError::Conflict("Workspace worker ownership changed; reload before continuing".into())
}
impl<D: Db> Store<D> {
    async fn worker_credentials(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        generation: &str,
    ) -> Result<(String, String), StorageError> {
        let result = self.db.execute(
            "SELECT worker_id,worker_digest FROM workspace_transactions WHERE user_id=? AND project_id=? AND id=? AND generation=?",
            &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into()),DbValue::Text(generation.into())],
        ).await?;
        let row = result.rows.first().ok_or_else(conflict)?;
        Ok((row.get_text(0)?.into(), row.get_text(1)?.into()))
    }
    pub(super) async fn authorize_workspace_transaction_worker(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        generation: &str,
        worker: Option<&WorkspaceTransactionWorker>,
    ) -> Result<(), StorageError> {
        let (id, digest) = self
            .worker_credentials(user, project, id, generation)
            .await?;
        match worker {
            None if id.is_empty() && digest.is_empty() => Ok(()),
            Some(worker) => {
                worker.validate().map_err(StorageError::InvalidRequest)?;
                if worker.id != id || worker.secret_digest() != digest {
                    return Err(conflict());
                }
                Ok(())
            }
            None => Err(conflict()),
        }
    }
    /// Call only after obtaining the entire native physical folder gate. This
    /// replaces an old worker regardless of age; OS exclusion, not a timer,
    /// establishes that its accepted filesystem work cannot still be running.
    pub async fn claim_workspace_transaction_worker(
        &self,
        user: UserId,
        project: i64,
        expected: &WorkspaceTransactionScope,
        worker: &WorkspaceTransactionWorker,
        now: i64,
    ) -> Result<WorkspaceTransactionScope, StorageError> {
        self.claim_workspace_transaction_worker_reviewed(user, project, expected, worker, None, now)
            .await
    }
    /// `review` is the digest acknowledged by the native verifier and fresh
    /// sessionless UI grant. Only trusted native transports call this method.
    pub async fn claim_workspace_transaction_worker_reviewed(
        &self,
        user: UserId,
        project: i64,
        expected: &WorkspaceTransactionScope,
        worker: &WorkspaceTransactionWorker,
        review: Option<(&str, &str)>,
        now: i64,
    ) -> Result<WorkspaceTransactionScope, StorageError> {
        self.db
            .transaction(|tx| async move {
                Store::new(tx)
                    .claim_workspace_transaction_worker_inner(
                        user, project, expected, worker, review, now,
                    )
                    .await
            })
            .await
    }
    pub(super) async fn claim_workspace_transaction_worker_inner(
        &self,
        user: UserId,
        project: i64,
        expected: &WorkspaceTransactionScope,
        worker: &WorkspaceTransactionWorker,
        review: Option<(&str, &str)>,
        now: i64,
    ) -> Result<WorkspaceTransactionScope, StorageError> {
        expected.validate().map_err(StorageError::InvalidRequest)?;
        worker.validate().map_err(StorageError::InvalidRequest)?;
        let mut scope = self
            .workspace_transaction_scope(user, project, &expected.id)
            .await?
            .ok_or_else(conflict)?;
        let (current, digest) = self
            .worker_credentials(user, project, &scope.id, &scope.generation)
            .await?;
        // A lost acknowledgement may repeat this exact active admission.
        // A released worker is never reactivated, even with its old secret.
        let mut claimed = expected.clone();
        claimed.worker = Some(worker.state());
        if let Some((digest, _)) = review {
            if !openwebide_core::workspace_transaction::companion::fingerprint(digest)
                || expected
                    .accepted_language_review_digest
                    .as_deref()
                    .is_some_and(|accepted| accepted != digest)
            {
                return Err(conflict());
            }
            claimed.accepted_language_review_digest = Some(digest.into());
        }
        if scope == claimed && current == worker.id && digest == worker.secret_digest() {
            self.accept_language_review(user, project, &scope, review, now)
                .await?;
            return Ok(scope);
        }
        if &scope != expected || current == worker.id {
            return Err(conflict());
        }
        let accepted = self
            .accept_language_review(user, project, &scope, review, now)
            .await?;
        let result = self.db.execute(
            "UPDATE workspace_transactions SET worker_id=?,worker_digest=?,accepted_language_review_digest=?,updated_at=? WHERE user_id=? AND project_id=? AND id=? AND generation=? AND revision=? AND worker_id=? AND worker_digest=?",
            &[DbValue::Text(worker.id.clone()),DbValue::Text(worker.secret_digest()),accepted.clone().map_or(DbValue::Null,DbValue::Text),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(scope.id.clone()),DbValue::Text(scope.generation.clone()),DbValue::Int(i64::try_from(scope.revision).map_err(|_| conflict())?),DbValue::Text(current),DbValue::Text(digest)],
        ).await?;
        if result.changes != 1 {
            return Err(conflict());
        }
        scope.worker = Some(worker.state());
        scope.accepted_language_review_digest = accepted;
        Ok(scope)
    }
    /// Release only after all accepted work has finished, while the same
    /// physical gate is still held. Keep its ID to fence delayed claim/release.
    pub async fn release_workspace_transaction_worker(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        generation: &str,
        worker: &WorkspaceTransactionWorker,
        now: i64,
    ) -> Result<(), StorageError> {
        worker.validate().map_err(StorageError::InvalidRequest)?;
        if !valid_id(id) || !valid_id(generation) {
            return Err(conflict());
        }
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            store.get_project(project,user).await?;
            let (current,digest) = store.worker_credentials(user,project,id,generation).await?;
            if current != worker.id || (!digest.is_empty() && digest != worker.secret_digest()) { return Err(conflict()); }
            if digest.is_empty() { return Ok(()); }
            let result = store.db.execute(
                "UPDATE workspace_transactions SET worker_digest='',updated_at=? WHERE user_id=? AND project_id=? AND id=? AND generation=? AND worker_id=? AND worker_digest=?",
                &[DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into()),DbValue::Text(generation.into()),DbValue::Text(worker.id.clone()),DbValue::Text(digest)],
            ).await?;
            if result.changes != 1 { return Err(conflict()); }
            Ok(())
        }).await
    }
}
