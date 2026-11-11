//! Ownership, durable claims and CAS persistence for approved language actions.
//! Core owns the state machine; native execution retains the opaque command.
use super::*;
use openwebide_core::{
    editor::EditorRecoveryRoot,
    plugins::languages::intelligence::{
        Call,
        actions::{ActionApproval, ActionPhase, ActionRecord, CommandOutcome},
    },
    workspace_transaction::{TransactionPhase, WorkspaceTransactionState, valid_id},
};

const SELECT: &str = "SELECT approval,revision,phase,outcome,acknowledgement FROM language_actions WHERE user_id=? AND project_id=?";
const QUOTA: usize = 64 * 1024 * 1024;
mod requests;
const OUTCOME_RESERVE: usize =
    openwebide_core::plugins::languages::intelligence::actions::MAX_COMMAND_BYTES + 4096;

fn decode_action(row: &crate::db::QueryRow) -> Result<ActionRecord, StorageError> {
    let record = ActionRecord {
        approval: serde_json::from_str(row.get_text(0)?)
            .map_err(|e| StorageError::Db(e.to_string()))?,
        revision: u64::try_from(row.get_int(1)?).map_err(|e| StorageError::Db(e.to_string()))?,
        phase: serde_json::from_value(serde_json::Value::String(row.get_text(2)?.into()))
            .map_err(|e| StorageError::Db(e.to_string()))?,
        outcome: row
            .get_text_opt(3)
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| StorageError::Db(e.to_string()))?,
        acknowledgement: row.get_text_opt(4).map(str::to_owned),
    };
    record.validate().map_err(StorageError::Db)?;
    Ok(record)
}
fn phase_text(phase: ActionPhase) -> Result<String, StorageError> {
    serde_json::to_value(phase)
        .map_err(|e| StorageError::Db(e.to_string()))?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| StorageError::Db("Invalid action phase".into()))
}

impl<D: Db> Store<D> {
    pub async fn language_action(
        &self,
        user: UserId,
        project: i64,
        id: Option<&str>,
    ) -> Result<Option<ActionRecord>, StorageError> {
        self.get_project(project, user).await?;
        if id.is_some_and(|id| !valid_id(id)) {
            return Err(StorageError::InvalidRequest(
                "Invalid language action identity".into(),
            ));
        }
        let mut params = vec![DbValue::Int(user.get()), DbValue::Int(project)];
        let query = if let Some(id) = id {
            params.push(DbValue::Text(id.into()));
            format!("{SELECT} AND id=?")
        } else {
            format!(
                "{SELECT} ORDER BY CASE WHEN phase IN ('completed','acknowledged','cancelled') THEN 1 ELSE 0 END, updated_at DESC, rowid DESC LIMIT 1"
            )
        };
        let result = self.db.execute(&query, &params).await?;
        result.rows.first().map(decode_action).transpose()
    }

    async fn action_authority(
        &self,
        user: UserId,
        project: i64,
        call: &Call,
        approval: &ActionApproval,
        now: i64,
    ) -> Result<(), StorageError> {
        let (plugin, context) = self
            .plugin_grant_in_transaction(user, call.session_id, &call.grant, now)
            .await?;
        if !context.user_action
            || context.project_id != Some(project)
            || call.input != approval.input
            || call.prepared.source != approval.prepared.source
            || call.prepared.manifest != approval.prepared.manifest
            || call.prepared.digest != approval.prepared.digest
            || call.prepared.host_id != approval.prepared.host_id
            || plugin.source != approval.prepared.source
            || plugin.manifest != approval.prepared.manifest
            || plugin.digest != approval.prepared.digest
            || plugin.host_id != approval.prepared.host_id
        {
            return Err(StorageError::Conflict("Language command approval requires the original project, installed provider and explicit user-interface authority".into()));
        }
        self.validate_enabled_plugin(user, Some(project), &plugin)
            .await?;
        let current = self
            .project_language_context_in_transaction(user, project)
            .await?;
        approval
            .validate_provider(
                &openwebide_core::plugins::languages::LanguageRegistry::from_bindings(
                    &current.bindings,
                ),
                &current.preferences,
            )
            .map_err(StorageError::Conflict)?;
        Ok(())
    }

    async fn action_workspace(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<WorkspaceTransactionState, StorageError> {
        let owned = self.get_project(project, user).await?;
        let workspace = self
            .workspace_transaction(user, project, Some(id))
            .await?
            .ok_or_else(|| {
                StorageError::Conflict(
                    "The language action's workspace recovery record is missing".into(),
                )
            })?;
        if workspace.editor.root != EditorRecoveryRoot::for_project(&owned) {
            return Err(StorageError::Conflict(
                "The approved language action belongs to a different project folder".into(),
            ));
        }
        Ok(workspace)
    }

    pub async fn approve_language_action(
        &self,
        user: UserId,
        project: i64,
        call: &Call,
        approval: &ActionApproval,
        now: i64,
    ) -> Result<ActionRecord, StorageError> {
        approval.validate().map_err(StorageError::InvalidRequest)?;
        let encoded =
            serde_json::to_string(approval).map_err(|e| StorageError::Db(e.to_string()))?;
        let reserved = encoded.len().saturating_add(OUTCOME_RESERVE);
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            store.action_authority(user, project, call, approval, now).await?;
            if let Some(existing) = store.language_action(user, project, Some(&approval.id)).await? {
                if existing.approval.fingerprint().map_err(StorageError::Db)? == approval.fingerprint().map_err(StorageError::InvalidRequest)? {
                    return Ok(existing);
                }
                return Err(StorageError::Conflict("This language action already has a different approval".into()));
            }
            let workspace = store.action_workspace(user, project, &approval.id).await?;
            if workspace.journal.progress.phase != TransactionPhase::Prepared {
                return Err(StorageError::Conflict("Approve the complete language action before applying any edits".into()));
            }
            let existing = store.language_action(user, project, None).await?;
            if existing.is_some_and(|record| !record.phase.terminal()) {
                return Err(StorageError::Conflict("Review the pending language command before approving another".into()));
            }
            let command = approval.review.command.as_ref().ok_or_else(|| StorageError::InvalidRequest("This action has no server command".into()))?;
            if !store.db.execute("SELECT 1 FROM language_actions WHERE user_id=? AND project_id=? AND server=? AND command_id=?",&[
                DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(command.server.clone()),DbValue::Text(command.id.clone()),
            ]).await?.rows.is_empty() {
                return Err(StorageError::Conflict("This reviewed server command already has an approval; load its outcome".into()));
            }
            store.db.execute("DELETE FROM language_actions WHERE user_id=? AND phase IN ('completed','acknowledged','cancelled') AND updated_at<?",&[DbValue::Int(user.get()),DbValue::Int(now.saturating_sub(300))]).await?;
            let quota = store.db.execute("SELECT COALESCE(SUM(bytes),0),COUNT(*) FROM language_actions WHERE user_id=?",&[DbValue::Int(user.get())]).await?;
            let retained = usize::try_from(quota.rows[0].get_int(0)?).map_err(|e| StorageError::Db(e.to_string()))?;
            if retained.saturating_add(reserved) > QUOTA || quota.rows[0].get_int(1)? >= 256 {
                return Err(StorageError::Conflict("Language action recovery storage is full; review pending outcomes first".into()));
            }
            store.db.execute("INSERT INTO language_actions (user_id,project_id,id,revision,server,command_id,approval,phase,bytes,created_at,updated_at) VALUES (?,?,?,1,?,?,?,'approved',?,?,?)",&[
                DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(approval.id.clone()),DbValue::Text(command.server.clone()),DbValue::Text(command.id.clone()),DbValue::Text(encoded.clone()),DbValue::Int(i64::try_from(reserved).map_err(|e| StorageError::InvalidRequest(e.to_string()))?),DbValue::Int(now),DbValue::Int(now),
            ]).await?;
            Ok(ActionRecord {approval:approval.clone(),revision:1,phase:ActionPhase::Approved,outcome:None,acknowledgement:None})
        }).await
    }

    pub async fn claim_language_action(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        revision: u64,
        call: &Call,
        now: i64,
    ) -> Result<ActionRecord, StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let current = store
                    .language_action(user, project, Some(id))
                    .await?
                    .ok_or_else(|| {
                        StorageError::Conflict("Language action approval is missing".into())
                    })?;
                store
                    .action_authority(user, project, call, &current.approval, now)
                    .await?;
                if current.revision != revision {
                    return Err(StorageError::Conflict(
                        "Language action changed; load its outcome instead of replaying it".into(),
                    ));
                }
                let workspace = store.action_workspace(user, project, id).await?;
                if workspace.journal.progress.phase != TransactionPhase::Committed {
                    return Err(StorageError::Conflict(
                        "Apply the entire approved workspace edit before running its command"
                            .into(),
                    ));
                }
                store
                    .validate_action_editors(user, project, &workspace)
                    .await?;
                let next = current.claim().map_err(StorageError::Conflict)?;
                store
                    .persist_language_action(user, project, revision, &next, now)
                    .await?;
                Ok(next)
            })
            .await
    }

    async fn validate_action_editors(
        &self,
        user: UserId,
        project: i64,
        workspace: &WorkspaceTransactionState,
    ) -> Result<(), StorageError> {
        let recovery = self.editor_recovery(user, project).await?;
        let committed_revision = workspace
            .editor_revision
            .checked_add(1)
            .ok_or_else(|| StorageError::Conflict("Editor recovery revision exhausted".into()))?;
        if recovery.revision < committed_revision
            || workspace.editor.after.iter().any(|after| {
                !recovery.state.files.iter().any(|current| {
                    current.path == after.path
                        && current.read_only == after.read_only
                        && current.document.as_ref().map(|doc| &doc.text)
                            == after.document.as_ref().map(|doc| &doc.text)
                })
            })
            || workspace.editor.before.iter().any(|before| {
                !workspace
                    .editor
                    .after
                    .iter()
                    .any(|after| after.path == before.path)
                    && recovery
                        .state
                        .files
                        .iter()
                        .any(|current| current.path == before.path)
            })
        {
            return Err(StorageError::Conflict(
                "Editor drafts changed after approval; the server command cannot start".into(),
            ));
        }
        Ok(())
    }

    async fn persist_language_action(
        &self,
        user: UserId,
        project: i64,
        revision: u64,
        next: &ActionRecord,
        now: i64,
    ) -> Result<(), StorageError> {
        next.validate().map_err(StorageError::InvalidRequest)?;
        let outcome = next
            .outcome
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StorageError::Db(e.to_string()))?;
        let bytes = serde_json::to_vec(&next.approval)
            .map_err(|e| StorageError::Db(e.to_string()))?
            .len()
            .saturating_add(OUTCOME_RESERVE);
        self.db.execute("UPDATE language_actions SET revision=?,phase=?,outcome=?,acknowledgement=?,bytes=?,updated_at=? WHERE user_id=? AND project_id=? AND id=? AND revision=?",&[
            DbValue::Int(i64::try_from(next.revision).map_err(|e| StorageError::InvalidRequest(e.to_string()))?),DbValue::Text(phase_text(next.phase)?),outcome.map_or(DbValue::Null,DbValue::Text),next.acknowledgement.clone().map_or(DbValue::Null,DbValue::Text),DbValue::Int(i64::try_from(bytes).map_err(|e| StorageError::InvalidRequest(e.to_string()))?),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(next.approval.id.clone()),DbValue::Int(i64::try_from(revision).map_err(|e| StorageError::InvalidRequest(e.to_string()))?),
        ]).await?;
        Ok(())
    }

    pub async fn finish_language_action(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        revision: u64,
        outcome: &CommandOutcome,
        now: i64,
    ) -> Result<ActionRecord, StorageError> {
        outcome.validate().map_err(StorageError::InvalidRequest)?;
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let current = store
                    .language_action(user, project, Some(id))
                    .await?
                    .ok_or_else(|| {
                        StorageError::Conflict("Language action approval is missing".into())
                    })?;
                if current.outcome.as_ref() == Some(outcome) {
                    return Ok(current); // Lost outcome acknowledgements never rerun work.
                }
                if current.revision != revision {
                    return Err(StorageError::Conflict(
                        "Language action outcome changed".into(),
                    ));
                }
                let next = current
                    .finish(outcome.clone())
                    .map_err(StorageError::Conflict)?;
                store
                    .persist_language_action(user, project, revision, &next, now)
                    .await?;
                Ok(next)
            })
            .await
    }

    pub async fn reject_unstarted_language_action(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        revision: u64,
        message: &str,
        now: i64,
    ) -> Result<ActionRecord, StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let current = store
                    .language_action(user, project, Some(id))
                    .await?
                    .ok_or_else(|| {
                        StorageError::Conflict("Language action approval is missing".into())
                    })?;
                if current.revision != revision {
                    return Err(StorageError::Conflict(
                        "Language action changed before its unstarted command was rejected".into(),
                    ));
                }
                let next = current
                    .reject_unstarted(message.into())
                    .map_err(StorageError::Conflict)?;
                store
                    .persist_language_action(user, project, revision, &next, now)
                    .await?;
                Ok(next)
            })
            .await
    }

    pub async fn acknowledge_language_action(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        revision: u64,
        note: &str,
        now: i64,
    ) -> Result<ActionRecord, StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let current = store
                    .language_action(user, project, Some(id))
                    .await?
                    .ok_or_else(|| {
                        StorageError::Conflict("Language action approval is missing".into())
                    })?;
                if current.revision != revision {
                    return Err(StorageError::Conflict(
                        "Language action changed while reviewing its outcome".into(),
                    ));
                }
                let workspace = store
                    .workspace_transaction(user, project, Some(id))
                    .await?
                    .ok_or_else(|| {
                        StorageError::Conflict("Workspace recovery is missing".into())
                    })?;
                if !matches!(
                    workspace.journal.progress.phase,
                    TransactionPhase::Committed | TransactionPhase::Restored
                ) {
                    return Err(StorageError::Conflict(
                        "Recover the pending file operation before acknowledging its command"
                            .into(),
                    ));
                }
                let next = current
                    .acknowledge(note.into())
                    .map_err(StorageError::InvalidRequest)?;
                store
                    .persist_language_action(user, project, revision, &next, now)
                    .await?;
                Ok(next)
            })
            .await
    }

    pub async fn cancel_language_action(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        revision: u64,
        now: i64,
    ) -> Result<ActionRecord, StorageError> {
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            let current = store.language_action(user, project, Some(id)).await?
                .ok_or_else(|| StorageError::Conflict("Language action approval is missing".into()))?;
            if current.revision != revision {
                return Err(StorageError::Conflict("Language action changed before cancellation".into()));
            }
            let workspace = store.workspace_transaction(user, project, Some(id)).await?
                .ok_or_else(|| StorageError::Conflict("Workspace recovery is missing".into()))?;
            if workspace.journal.progress.phase != TransactionPhase::Restored {
                return Err(StorageError::Conflict("Restore or cancel the workspace edits before cancelling their unstarted command".into()));
            }
            let next = current.cancel().map_err(StorageError::Conflict)?;
            store.persist_language_action(user, project, revision, &next, now).await?;
            Ok(next)
        }).await
    }
}

#[cfg(test)]
mod tests;
