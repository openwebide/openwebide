//! One owned database observation; never materialize recovery snapshots or
//! command payloads merely to decide whether ordinary writes may proceed.
use super::*;
use openwebide_core::workspace_transaction::WorkspaceActivity;

impl<D: Db> Store<D> {
    pub async fn workspace_activity(
        &self,
        user: UserId,
        project: i64,
    ) -> Result<WorkspaceActivity, StorageError> {
        let result = self.db.execute(
            "SELECT EXISTS(SELECT 1 FROM workspace_transactions w WHERE w.user_id=p.user_id AND w.project_id=p.id AND w.terminal=0), EXISTS(SELECT 1 FROM language_actions a WHERE a.user_id=p.user_id AND a.project_id=p.id AND a.phase NOT IN ('completed','acknowledged','cancelled')) FROM projects p WHERE p.user_id=? AND p.id=?",
            &[DbValue::Int(user.get()), DbValue::Int(project)],
        ).await?;
        let row = result
            .rows
            .first()
            .ok_or_else(|| StorageError::NotFound("Project not found".into()))?;
        Ok(WorkspaceActivity {
            file_recovery: row.get_int(0)? != 0,
            language_command: row.get_int(1)? != 0,
        })
    }
}

#[cfg(test)]
mod tests;
