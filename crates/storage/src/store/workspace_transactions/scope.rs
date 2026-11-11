//! Owned recovery scope without materializing file or editor snapshots.
use super::*;
use openwebide_core::workspace_transaction::WorkspaceTransactionScope;

impl<D: Db> Store<D> {
    pub async fn workspace_transaction_scope(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<Option<WorkspaceTransactionScope>, StorageError> {
        if !valid_id(id) {
            return Err(StorageError::InvalidRequest(
                "Invalid workspace transaction identity".into(),
            ));
        }
        self.get_project(project, user).await?;
        let result = self.db.execute(
            "SELECT id,revision,generation,root_identity,editor_root,worker_id,worker_digest,accepted_language_review_digest FROM workspace_transactions WHERE user_id=? AND project_id=? AND id=?",
            &[DbValue::Int(user.get()), DbValue::Int(project), DbValue::Text(id.into())],
        ).await?;
        result
            .rows
            .first()
            .map(|row| {
                let scope = WorkspaceTransactionScope {
                    id: row.get_text(0)?.into(),
                    revision: u64::try_from(row.get_int(1)?).map_err(|_| {
                        StorageError::Db("Invalid workspace journal revision".into())
                    })?,
                    generation: row.get_text(2)?.into(),
                    root_identity: row.get_text(3)?.into(),
                    root: serde_json::from_str(row.get_text(4)?)
                        .map_err(|error| StorageError::Db(error.to_string()))?,
                    worker: if row.get_text(5)?.is_empty() {
                        None
                    } else {
                        Some(openwebide_core::workspace_transaction::WorkspaceTransactionWorkerState {
                            id: row.get_text(5)?.into(),
                            active: !row.get_text(6)?.is_empty(),
                        })
                    },
                    accepted_language_review_digest: row.get_text_opt(7).map(str::to_owned),
                };
                scope.validate().map_err(StorageError::InvalidValue)?;
                Ok(scope)
            })
            .transpose()
    }
}
