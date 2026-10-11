//! App-visible history references. Plugins choose task/run associations and outcomes.
use super::*;
use openwebide_core::plugins::{PreparedPlugin, execution::PluginExecutionContext, records::*};
use serde::{Deserialize, Serialize};
use serde_json::json;

// Keep legacy IDs unchanged and new IDs disjoint, positive and JSON-number safe.
pub(super) const HISTORY_ID_BASE: i64 = 1_i64 << 52;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    status: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    session_id: Option<i64>,
    #[serde(default)]
    message_id: Option<i64>,
    #[serde(default)]
    permission_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryValue {
    key: String,
    task_id: i64,
    due_at: i64,
    #[serde(default)]
    run_id: Option<i64>,
    #[serde(default)]
    snapshot: Option<Snapshot>,
}
fn terminal(status: &str) -> bool {
    matches!(
        status,
        "complete" | "failed" | "cancelled" | "expired" | "interrupted"
    )
}
fn raw_status(state: &str) -> &str {
    match state {
        "pending" => "queued",
        "leased" => "claimed",
        "completed" => "complete",
        other => other,
    }
}
impl<D: Db> Store<D> {
    pub(super) async fn plugin_task_history_in_transaction(
        &self,
        user: UserId,
        plugin: &PreparedPlugin,
        context: &PluginExecutionContext,
        request: &RecordRequest,
        now: i64,
    ) -> Result<CollectionResult, StorageError> {
        let scope = [
            DbValue::Int(user.get()),
            context.project_id.map_or(DbValue::Null, DbValue::Int),
            DbValue::Text(plugin.storage_namespace()),
        ];
        let mut selected = None;
        match &request.operation {
            RecordOperation::Create { value } => {
                let value: HistoryValue = serde_json::from_value(value.clone())
                    .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
                if value.task_id <= 0
                    || value.due_at < 0
                    || value.run_id.is_some_and(|id| id <= 0)
                    || value.key.is_empty()
                    || value.key.len() > 128
                    || !value.key.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                    })
                {
                    return Err(StorageError::InvalidRequest(
                        "Invalid task history reference".into(),
                    ));
                }
                let mut owned = scope.to_vec();
                owned.push(DbValue::Int(value.task_id));
                if self.db.execute("SELECT 1 FROM scheduled_tasks t WHERE user_id=? AND project_id IS ? AND plugin_owner=? AND id=? AND NOT EXISTS(SELECT 1 FROM goal_workers WHERE task_id=t.id)",&owned).await?.rows.is_empty() {
                    return Err(StorageError::NotFound("Plugin-owned task".into()));
                }
                let existing=self.db.execute("SELECT id,due_at,run_id FROM plugin_task_runs WHERE task_id=? AND history_key=?",&[DbValue::Int(value.task_id),DbValue::Text(value.key.clone())]).await?;
                if let Some(row) = existing.rows.first() {
                    if row.get_int(1)? != value.due_at || row.get_int_opt(2) != value.run_id {
                        return Err(StorageError::Conflict(
                            "History key refers to another occurrence".into(),
                        ));
                    }
                    selected = Some(HISTORY_ID_BASE + row.get_int(0)?);
                } else {
                    let count=self.db.execute("SELECT count(*) FROM plugin_task_runs h JOIN scheduled_tasks t ON t.id=h.task_id WHERE t.user_id=? AND t.project_id IS ? AND t.plugin_owner=?",&scope).await?.rows[0].get_int(0)?;
                    if count >= 1000 {
                        return Err(StorageError::Conflict(
                            "Task history holds at most 1,000 entries in this scope".into(),
                        ));
                    }
                    let old = self
                        .db
                        .execute("SELECT max(id) FROM scheduled_runs", &[])
                        .await?;
                    if old.rows[0]
                        .get_int_opt(0)
                        .is_some_and(|id| id >= HISTORY_ID_BASE)
                    {
                        return Err(StorageError::Conflict(
                            "Legacy history exceeds the supported ID range".into(),
                        ));
                    }
                    let mut raw = vec![
                        DbValue::Int(value.run_id.unwrap_or(0)),
                        DbValue::Int(user.get()),
                        DbValue::Int(context.project_id.unwrap_or(0)),
                        DbValue::Text(plugin.storage_namespace()),
                    ];
                    let linked=self.db.execute("SELECT state,detail,session_id,message_id,permission_id FROM plugin_runs WHERE id=? AND user_id=? AND project_scope=? AND plugin=?",&raw).await?;
                    let live = linked.rows.first();
                    if live.is_none()
                        && let Some(id) = value.run_id
                        && !self
                            .db
                            .execute("SELECT 1 FROM plugin_runs WHERE id=?", &[DbValue::Int(id)])
                            .await?
                            .rows
                            .is_empty()
                    {
                        return Err(StorageError::NotFound("Raw run in this scope".into()));
                    }
                    let snapshot = if let Some(row) = live {
                        Snapshot {
                            status: raw_status(row.get_text(0)?).into(),
                            detail: row.get_text(1)?.into(),
                            session_id: row.get_int_opt(2),
                            message_id: row.get_int_opt(3),
                            permission_id: row.get_text_opt(4).map(str::to_owned),
                        }
                    } else {
                        value.snapshot.ok_or_else(|| {
                            StorageError::InvalidRequest(
                                "A missing raw run needs a terminal history snapshot".into(),
                            )
                        })?
                    };
                    if snapshot.detail.len() > 4096
                        || snapshot.permission_id.as_ref().is_some_and(|id| {
                            id.is_empty() || id.len() > 256 || id.chars().any(char::is_control)
                        })
                        || (live.is_none() && !terminal(&snapshot.status))
                    {
                        return Err(StorageError::InvalidRequest(
                            "Invalid history snapshot".into(),
                        ));
                    }
                    if let Some(session) = snapshot.session_id
                        && self.get_session(session, user).await?.project_id != context.project_id
                    {
                        return Err(StorageError::InvalidRequest(
                            "History conversation is outside this scope".into(),
                        ));
                    }
                    if let Some(message) = snapshot.message_id {
                        let row=self.db.execute("SELECT s.project_id FROM messages m JOIN sessions s ON s.id=m.session_id WHERE m.id=? AND s.user_id=? AND s.id IS ?",&[DbValue::Int(message),DbValue::Int(user.get()),snapshot.session_id.map_or(DbValue::Null,DbValue::Int)]).await?;
                        if row
                            .rows
                            .first()
                            .is_none_or(|row| row.get_int_opt(0) != context.project_id)
                        {
                            return Err(StorageError::InvalidRequest(
                                "History message is outside this scope".into(),
                            ));
                        }
                    }
                    raw = vec![
                        DbValue::Int(value.task_id),
                        DbValue::Text(value.key),
                        DbValue::Int(value.due_at),
                        value.run_id.map_or(DbValue::Null, DbValue::Int),
                        if live.is_some() {
                            value.run_id.map_or(DbValue::Null, DbValue::Int)
                        } else {
                            DbValue::Null
                        },
                        DbValue::Int(now),
                        DbValue::Text(snapshot.status),
                        DbValue::Text(snapshot.detail),
                        snapshot.session_id.map_or(DbValue::Null, DbValue::Int),
                        snapshot.message_id.map_or(DbValue::Null, DbValue::Int),
                        snapshot.permission_id.map_or(DbValue::Null, DbValue::Text),
                    ];
                    let id=self.db.execute("INSERT INTO plugin_task_runs(task_id,history_key,due_at,run_id,linked_run,updated_at,status,detail,session_id,message_id,permission_id) VALUES(?,?,?,?,?,?,?,?,?,?,?)",&raw).await?.last_insert_rowid;
                    if id >= HISTORY_ID_BASE {
                        return Err(StorageError::Conflict(
                            "Task history ID capacity exhausted".into(),
                        ));
                    }
                    selected = Some(id + HISTORY_ID_BASE);
                }
            }
            RecordOperation::Delete { id, revision } => {
                let mut params = scope.to_vec();
                params.push(DbValue::Int(id - HISTORY_ID_BASE));
                let rows=self.db.execute("SELECT h.revision,h.status FROM plugin_task_runs h JOIN scheduled_tasks t ON t.id=h.task_id WHERE t.user_id=? AND t.project_id IS ? AND t.plugin_owner=? AND h.id=?",&params).await?;
                let row = rows
                    .rows
                    .first()
                    .ok_or_else(|| StorageError::NotFound("Task history".into()))?;
                if row.get_int(0)? != *revision || !terminal(row.get_text(1)?) {
                    return Err(StorageError::Conflict(
                        "Only unchanged terminal plugin history can be deleted".into(),
                    ));
                }
                self.db
                    .execute(
                        "DELETE FROM plugin_task_runs WHERE id=?",
                        &[DbValue::Int(id - HISTORY_ID_BASE)],
                    )
                    .await?;
                return Ok(CollectionResult {
                    enabled: true,
                    records: vec![],
                    next: None,
                });
            }
            RecordOperation::Update { .. } => {
                return Err(StorageError::InvalidRequest(
                    "History bindings are immutable; raw progress updates their snapshots".into(),
                ));
            }
            RecordOperation::Read { id } => selected = Some(*id),
            RecordOperation::List { .. } => (),
        }
        let mut params = scope.to_vec();
        params.extend(scope.clone());
        let comparator = if selected.is_some() { "=" } else { ">" };
        let cursor = selected.unwrap_or({
            if let RecordOperation::List { after } = request.operation {
                after
            } else {
                0
            }
        });
        params.push(DbValue::Int(cursor));
        let rows=self.db.execute(&format!("SELECT * FROM (SELECT h.id+{HISTORY_ID_BASE} AS public_id,h.revision,h.updated_at,h.task_id,h.history_key,h.due_at,h.run_id,h.status,h.detail,h.session_id,h.message_id,h.permission_id,1 AS editable FROM plugin_task_runs h JOIN scheduled_tasks t ON t.id=h.task_id WHERE t.user_id=? AND t.project_id IS ? AND t.plugin_owner=? UNION ALL SELECT r.id,1,0,r.task_id,'legacy:'||r.id,r.due_at,NULL,r.status,r.detail,r.session_id,r.message_id,r.permission_id,0 FROM scheduled_runs r JOIN scheduled_tasks t ON t.id=r.task_id WHERE t.user_id=? AND t.project_id IS ? AND (t.plugin_owner IS NULL OR t.plugin_owner=?) AND NOT EXISTS(SELECT 1 FROM goal_workers WHERE task_id=t.id)) WHERE public_id{comparator}? ORDER BY public_id LIMIT 33"),&params).await?;
        if selected.is_some() && rows.rows.is_empty() {
            return Err(StorageError::NotFound("Task history".into()));
        }
        let mut records = Vec::new();
        let mut bytes = 128;
        let mut next = None;
        for row in rows.rows {
            let record = Record {
                id: row.get_int(0)?,
                revision: row.get_int(1)?,
                updated_at: row.get_int(2)?,
                value: json!({"task_id":row.get_int(3)?,"key":row.get_text(4)?,"due_at":row.get_int(5)?,"run_id":row.get_int_opt(6),"editable":row.get_int(12)?!=0,
                "snapshot":{"status":row.get_text(7)?,"detail":row.get_text(8)?,"session_id":row.get_int_opt(9),"message_id":row.get_int_opt(10),"permission_id":row.get_text_opt(11)}}),
            };
            let size = serde_json::to_vec(&record)
                .map_err(|error| StorageError::Db(error.to_string()))?
                .len()
                + 1;
            if records.len() == 32 || (!records.is_empty() && bytes + size > 1024 * 1024) {
                next = records.last().map(|record: &Record| record.id);
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
}
