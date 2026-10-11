//! Schema adapters for app-visible project data. Feature behavior stays in plugins.
use super::*;
use openwebide_core::plugins::records::{CollectionResult, Record, RecordOperation, RecordRequest};
use openwebide_core::plugins::{PreparedPlugin, execution::PluginExecutionContext};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MemoryValue {
    title: String,
    content: String,
    #[serde(default)]
    auto_title: bool,
}

impl MemoryValue {
    fn parse(value: &serde_json::Value) -> Result<Self, StorageError> {
        let value: Self = serde_json::from_value(value.clone())
            .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
        // Persistence enforces the existing UI schema. Search, title generation,
        // trimming, ranking and agent output formatting belong to plugin code.
        if value.title.trim().is_empty()
            || value.title.chars().count() > openwebide_core::memory::MAX_MEMORY_TITLE
            || value.content.trim().is_empty()
            || value.content.chars().count() > openwebide_core::memory::MAX_MEMORY_CONTENT
        {
            return Err(StorageError::InvalidRequest(
                "Invalid memory record value".into(),
            ));
        }
        Ok(value)
    }
}

impl<D: Db> Store<D> {
    pub(super) async fn plugin_collections_in_transaction(
        &self,
        user: UserId,
        plugin: &PreparedPlugin,
        context: &PluginExecutionContext,
        request: &RecordRequest,
        now: i64,
    ) -> Result<CollectionResult, StorageError> {
        let limit = if request.collection == "skills" {
            // The existing skill schema admits 128 KiB of data; JSON escapes
            // can expand resource text sixfold, plus the draft envelope.
            1024 * 1024
        } else {
            openwebide_core::plugins::records::MAX_RECORD_BYTES
        };
        request
            .validate_with_limit(limit)
            .map_err(StorageError::InvalidRequest)?;
        if request.collection == "configuration" {
            return self
                .plugin_configuration_in_transaction(user, request)
                .await;
        }
        if request.collection == "conversations" {
            return self
                .plugin_conversations_in_transaction(user, context, request)
                .await;
        }
        if request.collection == "tasks" {
            return self
                .plugin_tasks_in_transaction(user, plugin, context, request, now)
                .await;
        }
        if request.collection == "task_runs" {
            return self
                .plugin_task_history_in_transaction(user, plugin, context, request, now)
                .await;
        }
        let project = context.project_id;
        let user_action = context.user_action;
        if request.collection == "skills" {
            return self
                .plugin_skills_in_transaction(user, project, user_action, request, now)
                .await;
        }
        if request.collection != "memories" {
            return Err(StorageError::InvalidRequest(
                "Unknown project collection".into(),
            ));
        }
        let Some(project) = project else {
            if matches!(
                request.operation,
                RecordOperation::List { .. } | RecordOperation::Read { .. }
            ) {
                return Ok(CollectionResult {
                    enabled: false,
                    records: Vec::new(),
                    next: None,
                });
            }
            return Err(StorageError::InvalidRequest(
                "Collection requires a project".into(),
            ));
        };
        self.get_project(project, user).await?;
        let enabled = user_action
            || self
                .get_user_setting(user, &format!("project_memory_{project}"))
                .await?
                .as_deref()
                != Some("false");
        if !enabled {
            if !matches!(
                request.operation,
                RecordOperation::List { .. } | RecordOperation::Read { .. }
            ) {
                return Err(StorageError::InvalidRequest(
                    "Project collection is disabled".into(),
                ));
            }
            return Ok(CollectionResult {
                enabled,
                records: Vec::new(),
                next: None,
            });
        }
        let scope = [DbValue::Int(user.get()), DbValue::Int(project)];
        let mut selected = None;
        match &request.operation {
            RecordOperation::Create { value } => {
                let value = MemoryValue::parse(value)?;
                let count = self
                    .db
                    .execute(
                        "SELECT COUNT(*) FROM project_memories WHERE user_id=? AND project_id=?",
                        &scope,
                    )
                    .await?;
                if count.rows[0].get_int(0)?
                    >= i64::try_from(openwebide_core::memory::MAX_MEMORIES).unwrap_or(i64::MAX)
                {
                    return Err(StorageError::InvalidRequest(
                        "Project collection is full".into(),
                    ));
                }
                let result = self.db.execute("INSERT INTO project_memories(user_id,project_id,title,content,auto_title,updated_at) VALUES(?,?,?,?,?,?)", &[
                    scope[0].clone(), scope[1].clone(), DbValue::Text(value.title), DbValue::Text(value.content),
                    DbValue::Int(i64::from(value.auto_title)), DbValue::Int(now),
                ]).await?;
                selected = Some(result.last_insert_rowid);
            }
            RecordOperation::Update {
                id,
                revision,
                value,
            } => {
                let value = MemoryValue::parse(value)?;
                let changed = self.db.execute("UPDATE project_memories SET title=?,content=?,auto_title=?,updated_at=?,revision=revision+1 WHERE user_id=? AND project_id=? AND id=? AND revision=?", &[
                    DbValue::Text(value.title), DbValue::Text(value.content), DbValue::Int(i64::from(value.auto_title)), DbValue::Int(now),
                    scope[0].clone(), scope[1].clone(), DbValue::Int(*id), DbValue::Int(*revision),
                ]).await?;
                if changed.changes != 1 {
                    return Err(StorageError::Conflict(
                        "Record changed or is unavailable".into(),
                    ));
                }
                selected = Some(*id);
            }
            RecordOperation::Delete { id, revision } => {
                let changed = self.db.execute("DELETE FROM project_memories WHERE user_id=? AND project_id=? AND id=? AND revision=?", &[
                    scope[0].clone(), scope[1].clone(), DbValue::Int(*id), DbValue::Int(*revision),
                ]).await?;
                if changed.changes != 1 {
                    return Err(StorageError::Conflict(
                        "Record changed or is unavailable".into(),
                    ));
                }
                return Ok(CollectionResult {
                    enabled,
                    records: Vec::new(),
                    next: None,
                });
            }
            RecordOperation::Read { id } => selected = Some(*id),
            RecordOperation::List { .. } => (),
        }
        let mut values = scope.to_vec();
        let sql = if let Some(id) = selected {
            values.push(DbValue::Int(id));
            "SELECT id,revision,updated_at,title,content,auto_title FROM project_memories WHERE user_id=? AND project_id=? AND id=?"
        } else {
            let RecordOperation::List { after } = request.operation else {
                unreachable!("mutation selects its result")
            };
            values.push(DbValue::Int(after));
            "SELECT id,revision,updated_at,title,content,auto_title FROM project_memories WHERE user_id=? AND project_id=? AND id>? ORDER BY id LIMIT 33"
        };
        let result = self.db.execute(sql, &values).await?;
        let mut records = result.rows.iter().map(|row| {
            Ok(Record {
                id: row.get_int(0)?, revision: row.get_int(1)?, updated_at: row.get_int(2)?,
                value: serde_json::json!({"title":row.get_text(3)?,"content":row.get_text(4)?,"auto_title":row.get_int(5)? != 0}),
            })
        }).collect::<Result<Vec<_>, StorageError>>()?;
        if selected.is_some() && records.is_empty() {
            return Err(StorageError::NotFound("Record".into()));
        }
        let next = if records.len() > 32 {
            records.truncate(32);
            records.last().map(|record| record.id)
        } else {
            None
        };
        Ok(CollectionResult {
            enabled,
            records,
            next,
        })
    }
}
