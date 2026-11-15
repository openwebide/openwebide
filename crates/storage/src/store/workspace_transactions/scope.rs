//! Owned recovery scope without materializing file or editor snapshots.
use super::*;
use openwebide_core::workspace_transaction::{
    WorkspaceActivity, WorkspaceCommandRecovery, WorkspaceTransactionScope,
};

fn flag(row: &crate::db::QueryRow, index: usize) -> Result<bool, StorageError> {
    match row.get_int(index)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(StorageError::InvalidValue(
            "Original command worker state is invalid".into(),
        )),
    }
}
fn decode_scope(
    row: &crate::db::QueryRow,
) -> Result<(WorkspaceTransactionScope, WorkspaceActivity), StorageError> {
    let scope = WorkspaceTransactionScope {
        id: row.get_text(0)?.into(),
        revision: u64::try_from(row.get_int(1)?)
            .map_err(|_| StorageError::Db("Invalid workspace journal revision".into()))?,
        generation: row.get_text(2)?.into(),
        root_identity: row.get_text(3)?.into(),
        root: serde_json::from_str(row.get_text(4)?)
            .map_err(|error| StorageError::Db(error.to_string()))?,
        worker: if row.get_text(5)?.is_empty() {
            None
        } else {
            Some(
                openwebide_core::workspace_transaction::WorkspaceTransactionWorkerState {
                    id: row.get_text(5)?.into(),
                    active: !row.get_text(6)?.is_empty(),
                },
            )
        },
        accepted_language_review_digest: row.get_text_opt(7).map(str::to_owned),
    };
    scope.validate().map_err(StorageError::InvalidValue)?;
    Ok((
        scope,
        WorkspaceActivity {
            file_recovery: row.get_int(8)? != 0,
            language_command: row.get_int(9)? != 0,
        },
    ))
}
impl<D: Db> Store<D> {
    pub async fn workspace_transaction_scope(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<Option<WorkspaceTransactionScope>, StorageError> {
        Ok(self
            .workspace_transaction_activity(user, project, id)
            .await?
            .map(|(scope, _)| scope))
    }
    /// Read the exact original journal and both recovery phases in one database
    /// statement, without loading file snapshots or command/provider payloads.
    pub async fn workspace_transaction_activity(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<Option<(WorkspaceTransactionScope, WorkspaceActivity)>, StorageError> {
        self.recovery_metadata_row(user, project, id)
            .await?
            .as_ref()
            .map(decode_scope)
            .transpose()
    }
    /// Compact command identity/outcome fingerprint and compound recovery phase
    /// come from the same original row observation, including frozen payloads.
    pub async fn workspace_command_recovery(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<Option<WorkspaceCommandRecovery>, StorageError> {
        let Some(row) = self.recovery_metadata_row(user, project, id).await? else {
            return Ok(None);
        };
        let (scope, activity) = decode_scope(&row)?;
        let invocation = row.get_text_opt(11).ok_or_else(|| {
            StorageError::Conflict("Original command recovery metadata is unavailable".into())
        })?;
        if invocation.len() > 4096 {
            return Err(StorageError::InvalidValue(
                "Original command recovery metadata exceeds its bound".into(),
            ));
        }
        let command =
            openwebide_core::plugins::languages::intelligence::actions::CommandRecoveryScope {
                invocation: serde_json::from_str(invocation).map_err(|_| {
                    StorageError::InvalidValue(
                        "Original command recovery identity is invalid".into(),
                    )
                })?,
                revision: u64::try_from(row.get_int(12)?).map_err(|_| {
                    StorageError::InvalidValue(
                        "Original command recovery revision is invalid".into(),
                    )
                })?,
                phase: serde_json::from_value(serde_json::Value::String(row.get_text(13)?.into()))
                    .map_err(|_| {
                        StorageError::InvalidValue(
                            "Original command recovery phase is invalid".into(),
                        )
                    })?,
                outcome_digest: row.get_text_opt(14).map(str::to_owned),
                worker: if row.get_text(15)?.is_empty() {
                    None
                } else {
                    Some(
                        openwebide_core::workspace_transaction::WorkspaceTransactionWorkerState {
                            id: row.get_text(15)?.into(),
                            active: flag(&row, 16)?,
                        },
                    )
                },
                native_settlement_pending: flag(&row, 17)?,
                command_cleanup_confirmed: flag(&row, 18)?,
            };
        let recovery = WorkspaceCommandRecovery {
            scope,
            activity,
            command,
        };
        recovery.validate().map_err(StorageError::InvalidValue)?;
        Ok(Some(recovery))
    }
    async fn recovery_metadata_row(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<Option<crate::db::QueryRow>, StorageError> {
        if !valid_id(id) {
            return Err(StorageError::InvalidRequest(
                "Invalid workspace transaction identity".into(),
            ));
        }
        self.get_project(project, user).await?;
        let query = format!(
            "SELECT w.id,w.revision,w.generation,w.root_identity,w.editor_root,w.worker_id,w.worker_digest,w.accepted_language_review_digest,w.terminal=0,COALESCE({},0),a.id,CASE WHEN length(CAST(a.invocation AS BLOB))<=4096 THEN a.invocation END,a.revision,CASE WHEN length(CAST(a.phase AS BLOB))<=32 THEN a.phase ELSE 'invalid' END,CASE WHEN a.outcome_digest IS NULL OR length(CAST(a.outcome_digest AS BLOB))=64 THEN a.outcome_digest ELSE 'invalid' END,CASE WHEN length(a.command_worker_id)<=32 THEN a.command_worker_id ELSE 'invalid' END,CASE WHEN length(a.command_worker_digest) IN (0,64) THEN a.command_worker_digest!='' END,a.native_settlement_pending,a.command_cleanup_confirmed FROM workspace_transactions w LEFT JOIN language_actions a ON a.user_id=w.user_id AND a.project_id=w.project_id AND a.id=w.id WHERE w.user_id=? AND w.project_id=? AND w.id=?",
            super::super::language_actions::pending_predicate("a")
        );
        let result = self
            .db
            .execute(
                &query,
                &[
                    DbValue::Int(user.get()),
                    DbValue::Int(project),
                    DbValue::Text(id.into()),
                ],
            )
            .await?;
        Ok(result.rows.into_iter().next())
    }
}
