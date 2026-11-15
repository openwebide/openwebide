//! One-time original bindings for readable pre-migration command records.
use super::*;
impl<D: Db> Store<D> {
    pub(super) async fn preserve_command_recovery(
        &self,
        user: UserId,
        project: i64,
        record: &ActionRecord,
    ) -> Result<(), StorageError> {
        let invocation = serde_json::to_string(
            &record
                .approval
                .command_invocation()
                .map_err(StorageError::InvalidValue)?,
        )
        .map_err(|error| StorageError::Db(error.to_string()))?;
        let approval = serde_json::to_string(&record.approval)
            .map_err(|error| StorageError::Db(error.to_string()))?;
        let digest = record
            .outcome
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|error| StorageError::Db(error.to_string()))?
            .map(|bytes| openwebide_core::plugins::content_digest(&bytes));
        self.db.execute("UPDATE language_actions SET invocation=?,outcome_digest=? WHERE user_id=? AND project_id=? AND id=? AND revision=? AND approval=? AND invocation IS NULL", &[
            DbValue::Text(invocation),digest.map_or(DbValue::Null,DbValue::Text),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(record.approval.id.clone()),DbValue::Int(i64::try_from(record.revision).map_err(|_|StorageError::InvalidValue("Invalid original command revision".into()))?),DbValue::Text(approval),
        ]).await?;
        Ok(())
    }
}

use openwebide_core::plugins::languages::intelligence::actions::{
    ActionRecovery, ActionRecoveryRequest, MAX_COMMAND_BYTES,
};
impl<D: Db> Store<D> {
    pub async fn language_action_recovery(
        &self,
        user: UserId,
        project: i64,
        request: &ActionRecoveryRequest,
        now: i64,
    ) -> Result<Option<ActionRecovery>, StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                match request {
                    ActionRecoveryRequest::Finish {
                        id,
                        revision,
                        outcome,
                    } => store
                        .finish_action_recovery(user, project, id, *revision, outcome, now)
                        .await
                        .map(Some),
                    ActionRecoveryRequest::Load { id } => {
                        store
                            .read_action_recovery(user, project, id.as_deref())
                            .await
                    }
                    ActionRecoveryRequest::Acknowledge { id, revision, note } => store
                        .acknowledge_action_recovery(user, project, id, *revision, note, now)
                        .await
                        .map(Some),
                }
            })
            .await
    }
    pub(super) async fn read_action_recovery(
        &self,
        user: UserId,
        project: i64,
        id: Option<&str>,
    ) -> Result<Option<ActionRecovery>, StorageError> {
        self.get_project(project, user).await?;
        if id.is_some_and(|id| !valid_id(id)) {
            return Err(StorageError::InvalidRequest(
                "Invalid language action identity".into(),
            ));
        }
        let mut params = vec![DbValue::Int(user.get()), DbValue::Int(project)];
        let suffix = if let Some(id) = id {
            params.push(DbValue::Text(id.into()));
            " AND id=?".into()
        } else {
            format!(
                " ORDER BY CASE WHEN {} THEN 0 ELSE 1 END,updated_at DESC,rowid DESC LIMIT 1",
                pending_predicate("")
            )
        };
        let rows=self.db.execute(&format!("SELECT id,invocation IS NULL FROM language_actions WHERE user_id=? AND project_id=?{suffix}"),&params).await?;
        let Some(row) = rows.rows.first() else {
            return Ok(None);
        };
        let id = row.get_text(0)?;
        if row.get_int(1)? != 0 {
            let record = self
                .language_action(user, project, Some(id))
                .await?
                .ok_or_else(|| {
                    StorageError::Conflict("Original command approval is unavailable".into())
                })?;
            self.preserve_command_recovery(user, project, &record)
                .await?;
        }
        let recovery = self
            .workspace_command_recovery(user, project, id)
            .await?
            .ok_or_else(|| {
                StorageError::Conflict("Original command recovery is unavailable".into())
            })?;
        let rows=self.db.execute("SELECT CASE WHEN outcome IS NULL OR length(CAST(outcome AS BLOB))<=? THEN outcome ELSE 'invalid' END,CASE WHEN acknowledgement IS NULL OR length(CAST(acknowledgement AS BLOB))<=4096 THEN acknowledgement ELSE '' END FROM language_actions WHERE user_id=? AND project_id=? AND id=?",&[DbValue::Int(i64::try_from(MAX_COMMAND_BYTES).unwrap()),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
        let row = rows.rows.first().ok_or_else(|| {
            StorageError::Conflict("Original command result is unavailable".into())
        })?;
        let value = ActionRecovery {
            command: recovery.command,
            outcome: row
                .get_text_opt(0)
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| {
                    StorageError::InvalidValue("Original command result is unreadable".into())
                })?,
            acknowledgement: row.get_text_opt(1).map(str::to_owned),
        };
        value.validate().map_err(StorageError::InvalidValue)?;
        Ok(Some(value))
    }
    pub(super) async fn finish_action_recovery(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        revision: u64,
        outcome: &CommandOutcome,
        now: i64,
    ) -> Result<ActionRecovery, StorageError> {
        outcome.validate().map_err(StorageError::InvalidRequest)?;
        let mut current = self
            .read_action_recovery(user, project, Some(id))
            .await?
            .ok_or_else(|| {
                StorageError::Conflict("Original command recovery is unavailable".into())
            })?;
        if current.outcome.as_ref() == Some(outcome) {
            return Ok(current);
        }
        if current.command.revision != revision {
            return Err(StorageError::Conflict(
                "Language action outcome changed".into(),
            ));
        }
        if current.command.native_settlement_pending {
            return Err(StorageError::Conflict(
                "The native command worker owns this outcome".into(),
            ));
        }
        current
            .command
            .phase
            .finish(outcome)
            .map_err(StorageError::Conflict)?;
        current.command.revision = current.command.revision.checked_add(1).ok_or_else(|| {
            StorageError::InvalidValue("Language action revision exhausted".into())
        })?;
        let encoded =
            serde_json::to_string(outcome).map_err(|e| StorageError::Db(e.to_string()))?;
        let digest = openwebide_core::plugins::content_digest(encoded.as_bytes());
        current.command.outcome_digest = Some(digest.clone());
        current.outcome = Some(outcome.clone());
        current.validate().map_err(StorageError::InvalidValue)?;
        let changed=self.db.execute("UPDATE language_actions SET phase=?,revision=?,outcome=?,outcome_digest=?,updated_at=? WHERE user_id=? AND project_id=? AND id=? AND revision=?",&[DbValue::Text(phase_text(current.command.phase)?),DbValue::Int(i64::try_from(current.command.revision).map_err(|_|StorageError::InvalidValue("Language action revision exhausted".into()))?),DbValue::Text(encoded),DbValue::Text(digest),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into()),DbValue::Int(i64::try_from(revision).map_err(|_|StorageError::InvalidValue("Language action revision exhausted".into()))?)]).await?;
        if changed.changes != 1 {
            return Err(StorageError::Conflict(
                "Language action outcome changed".into(),
            ));
        }
        Ok(current)
    }
    pub(super) async fn acknowledge_action_recovery(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        revision: u64,
        note: &str,
        now: i64,
    ) -> Result<ActionRecovery, StorageError> {
        let mut current = self
            .read_action_recovery(user, project, Some(id))
            .await?
            .ok_or_else(|| {
                StorageError::Conflict("Original command recovery is unavailable".into())
            })?;
        if current.command.revision != revision {
            return Err(StorageError::Conflict(
                "Language action changed while reviewing its outcome".into(),
            ));
        }
        let (_, activity) = self
            .workspace_transaction_activity(user, project, id)
            .await?
            .ok_or_else(|| StorageError::Conflict("Workspace recovery is missing".into()))?;
        if activity.file_recovery {
            return Err(StorageError::Conflict(
                "Recover the pending file operation before acknowledging its command".into(),
            ));
        }
        if current.command.native_settlement_pending && !current.command.command_cleanup_confirmed {
            return Err(StorageError::Conflict("Stop the original native command and confirm cleanup before acknowledging its effects".into()));
        }
        current
            .command
            .phase
            .acknowledge(note)
            .map_err(StorageError::InvalidRequest)?;
        current.command.revision = current.command.revision.checked_add(1).ok_or_else(|| {
            StorageError::InvalidValue("Language action revision exhausted".into())
        })?;
        current.acknowledgement = Some(note.into());
        current.validate().map_err(StorageError::InvalidValue)?;
        let changed=self.db.execute("UPDATE language_actions SET phase='acknowledged',revision=?,acknowledgement=?,updated_at=? WHERE user_id=? AND project_id=? AND id=? AND revision=?",&[DbValue::Int(i64::try_from(current.command.revision).map_err(|_|StorageError::InvalidValue("Language action revision exhausted".into()))?),DbValue::Text(note.into()),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into()),DbValue::Int(i64::try_from(revision).map_err(|_|StorageError::InvalidValue("Language action revision exhausted".into()))?)]).await?;
        if changed.changes != 1 {
            return Err(StorageError::Conflict(
                "Language action changed while acknowledging its outcome".into(),
            ));
        }
        Ok(current)
    }
}
