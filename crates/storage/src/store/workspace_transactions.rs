//! Durable owned resource-operation journals. Filesystem policy stays in core;
//! this adapter supplies project ownership, quotas and revision-checked commits.
use super::*;
use openwebide_core::{
    editor::{EditorRecoveryRecord, EditorRecoveryRoot},
    workspace_transaction::{
        AdvanceWorkspaceTransaction, BeginWorkspaceTransaction, JournalRecord, PreparedJournal,
        TransactionPhase, TransactionProgress, WorkspaceTransactionAdvanced,
        WorkspaceTransactionPrepared, WorkspaceTransactionState, valid_id,
    },
};

const JOURNAL_QUOTA: usize = 256 * 1024 * 1024;
mod cancellation;
mod delegation;
mod review_admission;
mod scope;
mod worker;
const JOURNAL_SELECT: &str = "SELECT id, revision, plan, editor, progress, editor_revision, generation, COALESCE(language_review,'null'),accepted_language_review_digest FROM workspace_transactions WHERE user_id = ? AND project_id = ?";

fn decode(row: &crate::db::QueryRow) -> Result<WorkspaceTransactionState, StorageError> {
    let revision = u64::try_from(row.get_int(1)?)
        .map_err(|_| StorageError::Db("Invalid workspace journal revision".into()))?;
    let plan = serde_json::from_str(row.get_text(2)?)
        .map_err(|error| StorageError::Db(error.to_string()))?;
    let editor = serde_json::from_str(row.get_text(3)?)
        .map_err(|error| StorageError::Db(error.to_string()))?;
    let progress = serde_json::from_str(row.get_text(4)?)
        .map_err(|error| StorageError::Db(error.to_string()))?;
    let state = WorkspaceTransactionState {
        journal: JournalRecord {
            id: row.get_text(0)?.into(),
            generation: row.get_text(6)?.into(),
            revision,
            plan,
            progress,
        },
        editor,
        editor_revision: row.get_int(5)?,
        language_review: serde_json::from_str(row.get_text(7)?)
            .map_err(|error| StorageError::Db(error.to_string()))?,
    };
    state
        .journal
        .validate()
        .map_err(StorageError::InvalidValue)?;
    state
        .editor
        .validate()
        .map_err(StorageError::InvalidValue)?;
    if let Some(review) = &state.language_review {
        review
            .validate_transaction(&state.journal.plan, &state.editor)
            .map_err(StorageError::InvalidValue)?;
    }
    if let Some(accepted) = row.get_text_opt(8)
        && state
            .language_review
            .as_ref()
            .is_none_or(|review| !review.fingerprint().is_ok_and(|digest| digest == accepted))
    {
        return Err(StorageError::InvalidValue(
            "The accepted language review changed".into(),
        ));
    }
    Ok(state)
}
impl<D: Db> Store<D> {
    pub async fn workspace_transaction(
        &self,
        user: UserId,
        project: i64,
        id: Option<&str>,
    ) -> Result<Option<WorkspaceTransactionState>, StorageError> {
        self.get_project(project, user).await?;
        if id.is_some_and(|id| !valid_id(id)) {
            return Err(StorageError::InvalidRequest(
                "Invalid workspace transaction identity".into(),
            ));
        }
        let query = match id {
            Some(_) => format!("{JOURNAL_SELECT} AND id = ?"),
            None => {
                format!(
                    "{JOURNAL_SELECT} ORDER BY terminal ASC, updated_at DESC, rowid DESC LIMIT 1"
                )
            }
        };
        let mut params = vec![DbValue::Int(user.get()), DbValue::Int(project)];
        if let Some(id) = id {
            params.push(DbValue::Text(id.into()));
        }
        let result = self.db.execute(&query, &params).await?;
        result.rows.first().map(decode).transpose()
    }
    pub async fn begin_workspace_transaction(
        &self,
        user: UserId,
        project: i64,
        input: &BeginWorkspaceTransaction,
        now: i64,
    ) -> Result<WorkspaceTransactionPrepared, StorageError> {
        if !valid_id(&input.id) {
            return Err(StorageError::InvalidRequest(
                "Invalid workspace transaction identity".into(),
            ));
        }
        input
            .plan
            .validate()
            .map_err(StorageError::InvalidRequest)?;
        input
            .editor
            .validate()
            .map_err(StorageError::InvalidRequest)?;
        if let Some(review) = &input.language_review {
            review
                .validate_transaction(&input.plan, &input.editor)
                .map_err(StorageError::InvalidRequest)?;
        }
        let fingerprint = input
            .plan
            .fingerprint()
            .map_err(StorageError::InvalidRequest)?;
        let plan = serde_json::to_string(&input.plan)
            .map_err(|error| StorageError::Db(error.to_string()))?;
        let editor = serde_json::to_string(&input.editor)
            .map_err(|error| StorageError::Db(error.to_string()))?;
        let language_review = input
            .language_review
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| StorageError::Db(error.to_string()))?;
        let editor_root = serde_json::to_string(&input.editor.root)
            .map_err(|error| StorageError::Db(error.to_string()))?;
        let bytes = plan
            .len()
            .saturating_add(editor.len())
            .saturating_add(language_review.as_ref().map_or(0, String::len))
            .saturating_add(input.plan.root.len())
            .saturating_add(editor_root.len())
            .saturating_add(32);
        let bytes = bytes.saturating_add(96);
        // Reserve the native acceptance digest before any admission. Claiming
        // a worker must not discover that the account quota is already full.
        let bytes = bytes.saturating_add(if language_review.is_some() { 64 } else { 0 });
        if bytes > openwebide_core::workspace_transaction::JOURNAL_API_BODY_LIMIT {
            return Err(StorageError::InvalidRequest(
                "Workspace recovery exceeds its 192 MiB limit".into(),
            ));
        }
        self.db.transaction(|tx|async move {
            let store=Store::new(tx);
            let owned=store.get_project(project,user).await?;
            if input.editor.root!=EditorRecoveryRoot::for_project(&owned) {return Err(StorageError::Conflict("The project folder changed before this workspace operation".into()));}
            if let Some(existing)=store.workspace_transaction(user,project,Some(&input.id)).await? {
                if existing.journal.plan==input.plan && existing.editor==input.editor && existing.language_review==input.language_review && existing.journal.progress==TransactionProgress::default() {
                    return Ok(WorkspaceTransactionPrepared {journal:PreparedJournal {id:input.id.clone(),generation:existing.journal.generation,revision:existing.journal.revision,fingerprint},editor_revision:existing.editor_revision});
                }
                return Err(StorageError::Conflict("This operation already started; load its recovery state".into()));
            }
            if !store.db.execute("SELECT 1 FROM workspace_transactions WHERE user_id = ? AND project_id = ? AND terminal = 0",&[DbValue::Int(user.get()),DbValue::Int(project)]).await?.rows.is_empty() {
                return Err(StorageError::Conflict("Recover the pending workspace operation before starting another".into()));
            }
            if !store.db.execute("SELECT 1 FROM language_actions WHERE user_id=? AND project_id=? AND phase NOT IN ('completed','acknowledged','cancelled')",&[DbValue::Int(user.get()),DbValue::Int(project)]).await?.rows.is_empty() {
                return Err(StorageError::Conflict("Review the pending language command before starting another workspace operation".into()));
            }
            // Keep terminal records for the complete request/retry window. A
            // pending operation is never evicted to make room for another one.
            store.db.execute("DELETE FROM workspace_transactions WHERE user_id = ? AND terminal = 1 AND updated_at < ? AND NOT EXISTS (SELECT 1 FROM language_actions a WHERE a.user_id=workspace_transactions.user_id AND a.project_id=workspace_transactions.project_id AND a.id=workspace_transactions.id AND a.phase NOT IN ('completed','acknowledged','cancelled'))",&[DbValue::Int(user.get()),DbValue::Int(now.saturating_sub(300))]).await?;
            let quota=store.db.execute("SELECT COALESCE(SUM(bytes),0) FROM workspace_transactions WHERE user_id = ?",&[DbValue::Int(user.get())]).await?;
            let retained=usize::try_from(quota.rows[0].get_int(0)?).map_err(|_|StorageError::Db("Invalid workspace recovery quota".into()))?;
            if retained.saturating_add(bytes)>JOURNAL_QUOTA {return Err(StorageError::Conflict("Workspace recovery storage is full; recover pending operations or wait for completed records to expire".into()));}
            let recovery=store.editor_recovery(user,project).await?;
            if recovery.revision!=input.editor.revision {return Err(StorageError::Conflict("Editor recovery changed in another window; refresh before applying the operation".into()));}
            input.editor.apply_to(&recovery.state).map_err(StorageError::Conflict)?;
            let progress=serde_json::to_string(&TransactionProgress::default()).map_err(|error|StorageError::Db(error.to_string()))?;
            store.db.execute("INSERT INTO workspace_transactions (user_id,project_id,id,revision,plan,editor,progress,editor_revision,terminal,bytes,created_at,updated_at,root_identity,editor_root,generation,language_review) VALUES (?,?,?,1,?,?,?,?,0,?,?,?,?,?,lower(hex(randomblob(16))),?)",&[
                DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(input.id.clone()),DbValue::Text(plan),DbValue::Text(editor),DbValue::Text(progress),DbValue::Int(recovery.revision),DbValue::Int(i64::try_from(bytes).map_err(|_|StorageError::InvalidRequest("Workspace recovery is too large".into()))?),DbValue::Int(now),DbValue::Int(now),DbValue::Text(input.plan.root.clone()),DbValue::Text(editor_root),language_review.map_or(DbValue::Null,DbValue::Text),
            ]).await?;
            let scope=store.workspace_transaction_scope(user,project,&input.id).await?.ok_or_else(||StorageError::Db("Prepared workspace scope is missing".into()))?;
            Ok(WorkspaceTransactionPrepared {journal:PreparedJournal {id:input.id.clone(),generation:scope.generation,revision:1,fingerprint},editor_revision:recovery.revision})
        }).await
    }
    pub async fn advance_workspace_transaction(
        &self,
        user: UserId,
        project: i64,
        input: &AdvanceWorkspaceTransaction,
        now: i64,
    ) -> Result<WorkspaceTransactionAdvanced, StorageError> {
        if input.progress == TransactionProgress::prepared_cancellation()
            && let Some(response) = self
                .cancel_prepared_workspace_transaction(user, project, input, now)
                .await?
        {
            return Ok(response);
        }
        // A zero-step rollback can have the same target cursor. It remains on
        // the ordinary authorization path when its current phase is not Prepared.
        self.advance_workspace_transaction_for_worker(user, project, input, None, now)
            .await
    }
    pub async fn advance_workspace_transaction_for_worker(
        &self,
        user: UserId,
        project: i64,
        input: &AdvanceWorkspaceTransaction,
        worker: Option<&openwebide_core::workspace_transaction::WorkspaceTransactionWorker>,
        now: i64,
    ) -> Result<WorkspaceTransactionAdvanced, StorageError> {
        if !valid_id(&input.id) || !valid_id(&input.generation) || input.revision == 0 {
            return Err(StorageError::InvalidRequest(
                "Invalid workspace transaction revision".into(),
            ));
        }
        self.db.transaction(|tx|async move {
            let store=Store::new(tx);
            let owned=store.get_project(project,user).await?;
            let scope=store.workspace_transaction_scope(user,project,&input.id).await?.ok_or_else(||StorageError::Conflict("Workspace recovery record is missing".into()))?;
            if scope.generation!=input.generation || scope.revision!=input.revision {return Err(StorageError::Conflict("Workspace recovery identity or revision changed; reload before continuing".into()));}
            store.authorize_workspace_transaction_worker(user, project, &input.id, &input.generation, worker).await?;
            let current=store.workspace_transaction(user,project,Some(&input.id)).await?.ok_or_else(||StorageError::Conflict("Workspace recovery record is missing".into()))?;
            if current.language_review.is_some() && scope.accepted_language_review_digest.is_none() {return Err(StorageError::Conflict("Native language review admission is required before advancing this operation".into()));}
            if current.journal.revision!=input.revision {return Err(StorageError::Conflict("Workspace recovery changed in another window".into()));}
            current.journal.validate_transition(&input.progress).map_err(StorageError::InvalidRequest)?;
            let deciding=input.progress.commits_editor()&&!current.journal.progress.commits_editor();
            if deciding && current.editor.root!=EditorRecoveryRoot::for_project(&owned) {return Err(StorageError::Conflict("Project folder changed during the workspace operation".into()));}
            let editor=if deciding {
                let recovery=store.editor_recovery(user,project).await?;
                if recovery.revision!=current.editor_revision {return Err(StorageError::Conflict("A newer editor draft prevents committing this workspace operation".into()));}
                let state=current.editor.apply_to(&recovery.state).map_err(StorageError::Conflict)?;
                let revision=recovery.revision.checked_add(1).ok_or_else(||StorageError::Conflict("Editor recovery revision limit reached".into()))?;
                store.db.execute("UPDATE user_settings SET value = ?, revision = ? WHERE user_id = ? AND key = ? AND revision = ?",&[DbValue::Text(serde_json::to_string(&state).map_err(|error|StorageError::Db(error.to_string()))?),DbValue::Int(revision),DbValue::Int(user.get()),DbValue::Text(format!("editor_recovery_{project}")),DbValue::Int(recovery.revision)]).await?;
                // A previously empty recovery row is still a valid project.
                if recovery.revision==0 {
                    store.db.execute("INSERT OR IGNORE INTO user_settings (user_id,key,value,revision) VALUES (?,?,?,?)",&[DbValue::Int(user.get()),DbValue::Text(format!("editor_recovery_{project}")),DbValue::Text(serde_json::to_string(&state).map_err(|error|StorageError::Db(error.to_string()))?),DbValue::Int(revision)]).await?;
                }
                Some(EditorRecoveryRecord {revision,state})
            } else {None};
            let revision=input.revision.checked_add(1).and_then(|revision|i64::try_from(revision).ok()).ok_or_else(||StorageError::Conflict("Workspace journal revision limit reached".into()))?;
            let progress=serde_json::to_string(&input.progress).map_err(|error|StorageError::Db(error.to_string()))?;
            let terminal=matches!(input.progress.phase,TransactionPhase::Committed|TransactionPhase::Restored);
            let (worker_id, worker_digest)=worker.map_or_else(||(String::new(),String::new()),|worker|(worker.id.clone(),worker.secret_digest()));
            let updated=store.db.execute("UPDATE workspace_transactions SET revision = ?, progress = ?, terminal = ?, updated_at = ? WHERE user_id = ? AND project_id = ? AND id = ? AND revision = ? AND generation = ? AND worker_id = ? AND worker_digest = ?",&[DbValue::Int(revision),DbValue::Text(progress),DbValue::Int(i64::from(terminal)),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(input.id.clone()),DbValue::Int(i64::try_from(input.revision).map_err(|_|StorageError::InvalidRequest("Invalid workspace journal revision".into()))?),DbValue::Text(input.generation.clone()),DbValue::Text(worker_id),DbValue::Text(worker_digest)]).await?;
            if updated.changes!=1 {return Err(StorageError::Conflict("Workspace worker ownership changed before cursor commit".into()));}
            Ok(WorkspaceTransactionAdvanced {generation:input.generation.clone(),revision:u64::try_from(revision).unwrap(),editor})
        }).await
    }
}

#[cfg(test)]
mod tests;
