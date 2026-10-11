//! Existing app task records exposed as scoped CRUD, without scheduling policy.
use super::*;
use openwebide_core::{
    plugins::{
        PreparedPlugin,
        execution::PluginExecutionContext,
        records::{CollectionResult, Record, RecordOperation, RecordRequest},
    },
    scheduled::{SessionTarget, TaskDraft},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Monitor {
    session_id: i64,
    interval_seconds: i64,
    remaining: i64,
    expires_at: i64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskValue {
    draft: TaskDraft,
    next_run: Option<i64>,
    #[serde(default)]
    state: Value,
    #[serde(default)]
    monitor: Option<Monitor>,
}
fn encode(value: &impl Serialize) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| StorageError::Db(error.to_string()))
}
impl<D: Db> Store<D> {
    pub(super) async fn plugin_tasks_in_transaction(
        &self,
        user: UserId,
        plugin: &PreparedPlugin,
        context: &PluginExecutionContext,
        request: &RecordRequest,
        now: i64,
    ) -> Result<CollectionResult, StorageError> {
        let project = context.project_id;
        if let Some(project) = project {
            self.get_project(project, user).await?;
        }
        let scope = [
            DbValue::Int(user.get()),
            project.map_or(DbValue::Null, DbValue::Int),
        ];
        let owner = plugin.storage_namespace();
        let mut selected = None;
        match &request.operation {
            RecordOperation::Create { value } | RecordOperation::Update { value, .. } => {
                let value: TaskValue = serde_json::from_value(value.clone())
                    .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
                let previous =
                    if let RecordOperation::Update { id, revision, .. } = request.operation {
                        self.plugin_task_mutable(user, project, &owner, id, revision)
                            .await?;
                        let rows = self
                            .db
                            .execute(
                                "SELECT draft FROM scheduled_tasks WHERE id=?",
                                &[DbValue::Int(id)],
                            )
                            .await?;
                        let row = rows
                            .rows
                            .first()
                            .ok_or(StorageError::NotFound("Task".into()))?;
                        let draft: TaskDraft = serde_json::from_str(row.get_text(0)?)
                            .map_err(|error| StorageError::Db(error.to_string()))?;
                        Some(draft)
                    } else {
                        None
                    };
                // Data integrity only: cron, future times, recurrence and expiry decisions live in source.
                if value.draft.title.trim().is_empty()
                    || value.draft.title.chars().count() > 120
                    || value.draft.prompt.trim().is_empty()
                    || value.draft.prompt.len() > 32 * 1024
                    || value.next_run.is_some_and(|time| time < 0)
                    || !(value.state.is_object() || value.state.is_null())
                {
                    return Err(StorageError::InvalidRequest(
                        "Invalid task record schema".into(),
                    ));
                }
                let session = if value.draft.session_target == SessionTarget::Existing {
                    let session = self.get_session(value.draft.session_id, user).await?;
                    if session.project_id != project {
                        return Err(StorageError::InvalidRequest(
                            "Task conversation is outside this scope".into(),
                        ));
                    }
                    Some(session.id)
                } else {
                    None
                };
                if let Some(model) = &value.draft.model
                    && (model.model.trim().is_empty()
                        || model.model.len() > 256
                        || model.model.chars().any(char::is_control)
                        // Result bookkeeping can retain a previously owned model that
                        // disappeared; new model assignments must still be valid.
                        || (previous
                            .as_ref()
                            .is_none_or(|draft| draft.model != value.draft.model)
                            && !self.get_connection(model.server_id).await?.enabled))
                {
                    return Err(StorageError::InvalidRequest(
                        "Invalid task model selection".into(),
                    ));
                }
                if let Some(monitor) = &value.monitor
                    && (session != Some(monitor.session_id)
                        || !(1..=86400).contains(&monitor.interval_seconds)
                        || !(0..=24).contains(&monitor.remaining)
                        || monitor.expires_at < 0)
                {
                    return Err(StorageError::InvalidRequest(
                        "Invalid monitor record schema".into(),
                    ));
                }
                let count=self.db.execute("SELECT count(*) FROM scheduled_tasks t WHERE user_id=? AND project_id IS ? AND NOT EXISTS(SELECT 1 FROM goal_workers WHERE task_id=t.id)",&scope).await?.rows[0].get_int(0)?;
                if matches!(request.operation, RecordOperation::Create { .. }) && count >= 120 {
                    return Err(StorageError::Conflict(
                        "Task collection holds at most 120 records".into(),
                    ));
                }
                let mut params = vec![
                    DbValue::Text(encode(&value.draft)?),
                    DbValue::Int(i64::from(value.draft.enabled)),
                    value.next_run.map_or(DbValue::Null, DbValue::Int),
                    session.map_or(DbValue::Null, DbValue::Int),
                    DbValue::Text(plugin.host_id.clone()),
                    DbValue::Text(owner.clone()),
                    DbValue::Text(encode(&value.state)?),
                    DbValue::Int(now),
                ];
                let id = match request.operation {
                    RecordOperation::Update { id, revision, .. } => {
                        params.extend([DbValue::Int(id), DbValue::Int(revision)]);
                        self.db.execute("UPDATE scheduled_tasks SET draft=?,enabled=?,next_run=?,session_id=?,host_id=?,plugin_owner=?,plugin_state=?,plugin_updated_at=?,revision=revision+1,path=NULL WHERE id=? AND revision=?",&params).await?;
                        id
                    }
                    _ => {
                        params.extend(scope.clone());
                        self.db.execute("INSERT INTO scheduled_tasks(draft,enabled,next_run,session_id,host_id,plugin_owner,plugin_state,plugin_updated_at,user_id,project_id) VALUES(?,?,?,?,?,?,?,?,?,?)",&params).await?.last_insert_rowid
                    }
                };
                self.db
                    .execute("DELETE FROM monitors WHERE task_id=?", &[DbValue::Int(id)])
                    .await?;
                if let Some(monitor) = value.monitor {
                    self.db.execute("INSERT INTO monitors(task_id,session_id,interval_seconds,remaining,expires_at) VALUES(?,?,?,?,?)",&[DbValue::Int(id),DbValue::Int(monitor.session_id),DbValue::Int(monitor.interval_seconds),DbValue::Int(monitor.remaining),DbValue::Int(monitor.expires_at)]).await?;
                }
                selected = Some(id);
            }
            RecordOperation::Delete { id, revision } => {
                self.plugin_task_mutable(user, project, &owner, *id, *revision)
                    .await?;
                self.db
                    .execute(
                        "DELETE FROM scheduled_tasks WHERE id=?",
                        &[DbValue::Int(*id)],
                    )
                    .await?;
                return Ok(CollectionResult {
                    enabled: true,
                    records: vec![],
                    next: None,
                });
            }
            RecordOperation::Read { id } => selected = Some(*id),
            RecordOperation::List { .. } => (),
        }
        let mut params = scope.to_vec();
        let filter = match selected {
            Some(id) => {
                params.push(DbValue::Int(id));
                "t.id=?"
            }
            None => {
                let RecordOperation::List { after } = request.operation else {
                    unreachable!()
                };
                params.push(DbValue::Int(after));
                "t.id>?"
            }
        };
        let rows=self.db.execute(&format!("SELECT t.id,t.revision,t.plugin_updated_at,t.draft,t.next_run,t.plugin_state,t.plugin_owner,m.session_id,m.interval_seconds,m.remaining,m.expires_at FROM scheduled_tasks t LEFT JOIN monitors m ON m.task_id=t.id WHERE t.user_id=? AND t.project_id IS ? AND {filter} AND NOT EXISTS(SELECT 1 FROM goal_workers WHERE task_id=t.id) ORDER BY t.id LIMIT 33"),&params).await?;
        if selected.is_some() && rows.rows.is_empty() {
            return Err(StorageError::NotFound("Task".into()));
        }
        let mut records = Vec::new();
        let mut bytes = 128usize;
        let mut next = None;
        for row in rows.rows {
            if records.len() == 32 {
                next = records.last().map(|record: &Record| record.id);
                break;
            }
            let id = row.get_int(0)?;
            let monitor=row.get_int_opt(7).map(|session|json!({"session_id":session,"interval_seconds":row.get_int_opt(8),"remaining":row.get_int_opt(9),"expires_at":row.get_int_opt(10)}));
            let record = Record {
                id,
                revision: row.get_int(1)?,
                updated_at: row.get_int(2)?,
                value: json!({
                    "draft":serde_json::from_str::<Value>(row.get_text(3)?).map_err(|error|StorageError::Db(error.to_string()))?,
                    "next_run":row.get_int_opt(4),"state":serde_json::from_str::<Value>(row.get_text(5)?).map_err(|error|StorageError::Db(error.to_string()))?,
                    "monitor":monitor,"owner":row.get_text_opt(6),"editable":row.get_text_opt(6).is_none_or(|stored|stored==owner),
                }),
            };
            let size = serde_json::to_vec(&record)
                .map_err(|error| StorageError::Db(error.to_string()))?
                .len()
                + 1;
            if !records.is_empty() && bytes + size > 1024 * 1024 {
                next = records.last().map(|record| record.id);
                break;
            }
            bytes += size;
            records.push(record);
        }
        Ok(CollectionResult {
            enabled: true,
            records,
            next,
        })
    }
    async fn plugin_task_mutable(
        &self,
        user: UserId,
        project: Option<i64>,
        owner: &str,
        id: i64,
        revision: i64,
    ) -> Result<(), StorageError> {
        let rows=self.db.execute("SELECT revision,plugin_owner FROM scheduled_tasks t WHERE id=? AND user_id=? AND project_id IS ? AND NOT EXISTS(SELECT 1 FROM goal_workers WHERE task_id=t.id)",&[DbValue::Int(id),DbValue::Int(user.get()),project.map_or(DbValue::Null,DbValue::Int)]).await?;
        let row = rows
            .rows
            .first()
            .ok_or_else(|| StorageError::NotFound("Task".into()))?;
        if row.get_int(0)? != revision || row.get_text_opt(1).is_some_and(|stored| stored != owner)
        {
            return Err(StorageError::Conflict(
                "Task revision or plugin ownership changed".into(),
            ));
        }
        let active=self.db.execute("SELECT 1 FROM scheduled_runs WHERE task_id=? AND status IN ('queued','claimed','running','blocked')",&[DbValue::Int(id)]).await?;
        if !active.rows.is_empty() {
            return Err(StorageError::Conflict(
                "Task is still delivering prior host work".into(),
            ));
        }
        Ok(())
    }
}
