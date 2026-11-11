//! Account cancellation before the first durable filesystem intent.
//! Revoking credentials does not release any native physical gate.
use super::*;

impl<D: Db> Store<D> {
    pub(super) async fn cancel_prepared_workspace_transaction(
        &self,
        user: UserId,
        project: i64,
        input: &AdvanceWorkspaceTransaction,
        now: i64,
    ) -> Result<Option<WorkspaceTransactionAdvanced>, StorageError> {
        if !valid_id(&input.id)
            || !valid_id(&input.generation)
            || input.revision == 0
            || input.progress != TransactionProgress::prepared_cancellation()
        {
            return Err(StorageError::InvalidRequest(
                "Invalid prepared cancellation".into(),
            ));
        }
        let revision = input
            .revision
            .checked_add(1)
            .and_then(|revision| i64::try_from(revision).ok())
            .ok_or_else(|| {
                StorageError::Conflict("Workspace journal revision limit reached".into())
            })?;
        self.db.transaction(|tx|async move {
            let store=Store::new(tx);
            store.get_project(project,user).await?;
            let result=store.db.execute("SELECT generation,revision,progress,worker_id,worker_digest FROM workspace_transactions WHERE user_id=? AND project_id=? AND id=?",
                &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(input.id.clone())]).await?;
            let conflict=||StorageError::Conflict("Workspace recovery changed or already started; reload before cancelling".into());
            let row=result.rows.first().ok_or_else(conflict)?;
            let progress=row.get_text(2)?;
            if row.get_text(0)?!=input.generation || u64::try_from(row.get_int(1)?).ok()!=Some(input.revision)
                || progress.len()>openwebide_core::workspace_transaction::RECOVERY_SCOPE_BODY_LIMIT {return Err(conflict());}
            let current:TransactionProgress=serde_json::from_str(progress).map_err(|_|conflict())?;
            if current!=TransactionProgress::default() {return Ok(None);}
            let worker=row.get_text(3)?;let digest=row.get_text(4)?;
            if (worker.is_empty() && !digest.is_empty()) || (!worker.is_empty() && !valid_id(worker))
                || (!digest.is_empty() && !openwebide_core::workspace_transaction::companion::fingerprint(digest)) {return Err(conflict());}
            let cancelled=serde_json::to_string(&input.progress).map_err(|error|StorageError::Db(error.to_string()))?;
            // The captured identity and cursor fence a concurrent admission or
            // first intent. Retain the worker ID as a tombstone, revoke its
            // credential, and leave the actual native gate with its task.
            let updated=store.db.execute("UPDATE workspace_transactions SET revision=?,progress=?,terminal=1,worker_digest='',updated_at=? WHERE user_id=? AND project_id=? AND id=? AND generation=? AND revision=? AND progress=? AND worker_id=? AND worker_digest=?",&[
                DbValue::Int(revision),DbValue::Text(cancelled),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),
                DbValue::Text(input.id.clone()),DbValue::Text(input.generation.clone()),DbValue::Int(row.get_int(1)?),
                DbValue::Text(progress.into()),DbValue::Text(worker.into()),DbValue::Text(digest.into()),
            ]).await?;
            if updated.changes!=1 {return Err(conflict());}
            Ok(Some(WorkspaceTransactionAdvanced {generation:input.generation.clone(),revision:u64::try_from(revision).unwrap(),editor:None}))
        }).await
    }
}
