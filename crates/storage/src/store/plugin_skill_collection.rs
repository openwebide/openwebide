//! Raw access to the shared skill schema; authoring and selection stay in plugins.
use super::*;
use openwebide_core::plugins::records::{CollectionResult, Record, RecordOperation, RecordRequest};
use openwebide_core::{SkillDraft, plugins::PreparedPlugin};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillValue {
    draft: SkillDraft,
}

impl<D: Db> Store<D> {
    pub(super) async fn plugin_skills_in_transaction(
        &self,
        user: UserId,
        project: Option<i64>,
        user_action: bool,
        request: &RecordRequest,
        now: i64,
    ) -> Result<CollectionResult, StorageError> {
        let enabled = if let Some(project) = project {
            self.get_project(project, user).await?;
            user_action
                || self
                    .get_user_setting(user, &format!("project_skills_{project}"))
                    .await?
                    .as_deref()
                    != Some("false")
        } else {
            false
        };
        if !enabled {
            if matches!(
                request.operation,
                RecordOperation::List { .. } | RecordOperation::Read { .. }
            ) {
                return Ok(CollectionResult {
                    enabled,
                    records: Vec::new(),
                    next: None,
                });
            }
            return Err(StorageError::InvalidRequest(
                "Project collection is disabled or requires a project".into(),
            ));
        }
        let project = project.expect("enabled collection has a project");
        let scope = [DbValue::Int(user.get()), DbValue::Int(project)];
        let mutation = match request.operation {
            RecordOperation::Update { id, .. } | RecordOperation::Delete { id, .. } => Some(id),
            _ => None,
        };
        if let Some(id) = mutation {
            let current = self.db.execute("SELECT s.draft,m.skill_id FROM project_skills s LEFT JOIN project_plugin_skills m ON m.skill_id=s.id WHERE s.user_id=? AND s.project_id=? AND s.id=?", &[
                scope[0].clone(), scope[1].clone(), DbValue::Int(id),
            ]).await?;
            let row = current
                .rows
                .first()
                .ok_or_else(|| StorageError::NotFound("Record".into()))?;
            let draft: SkillDraft = serde_json::from_str(row.get_text(0)?)
                .map_err(|error| StorageError::Db(error.to_string()))?;
            if row.get_int_opt(1).is_some() || !draft.enabled {
                return Err(StorageError::InvalidRequest(
                    "This record is managed by its plugin or disabled".into(),
                ));
            }
        }
        let mut selected = None;
        match &request.operation {
            RecordOperation::Create { value } | RecordOperation::Update { value, .. } => {
                let value: SkillValue = serde_json::from_value(value.clone())
                    .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
                value
                    .draft
                    .validate()
                    .map_err(StorageError::InvalidRequest)?;
                let exclude = mutation.unwrap_or(0);
                let duplicate = self.db.execute("SELECT id FROM project_skills WHERE user_id=? AND project_id=? AND name=? AND id<>?", &[
                    scope[0].clone(), scope[1].clone(), DbValue::Text(value.draft.name.clone()), DbValue::Int(exclude),
                ]).await?;
                if !duplicate.rows.is_empty() {
                    return Err(StorageError::Conflict(
                        "A record with this name already exists".into(),
                    ));
                }
                let json = serde_json::to_string(&value.draft)
                    .map_err(|error| StorageError::Db(error.to_string()))?;
                if let RecordOperation::Update { id, revision, .. } = request.operation {
                    let changed = self.db.execute("UPDATE project_skills SET name=?,draft=?,updated_at=?,revision=revision+1 WHERE user_id=? AND project_id=? AND id=? AND revision=?", &[
                        DbValue::Text(value.draft.name), DbValue::Text(json), DbValue::Int(now), scope[0].clone(), scope[1].clone(), DbValue::Int(id), DbValue::Int(revision),
                    ]).await?;
                    if changed.changes != 1 {
                        return Err(StorageError::Conflict(
                            "Record changed or is unavailable".into(),
                        ));
                    }
                    selected = Some(id);
                } else {
                    let count = self
                        .db
                        .execute(
                            "SELECT COUNT(*) FROM project_skills WHERE user_id=? AND project_id=?",
                            &scope,
                        )
                        .await?;
                    if count.rows[0].get_int(0)?
                        >= i64::try_from(openwebide_core::skills::MAX_SKILLS).unwrap_or(i64::MAX)
                    {
                        return Err(StorageError::InvalidRequest(
                            "Project collection is full".into(),
                        ));
                    }
                    let result = self.db.execute("INSERT INTO project_skills(user_id,project_id,name,draft,updated_at) VALUES(?,?,?,?,?)", &[
                        scope[0].clone(), scope[1].clone(), DbValue::Text(value.draft.name), DbValue::Text(json), DbValue::Int(now),
                    ]).await?;
                    selected = Some(result.last_insert_rowid);
                }
            }
            RecordOperation::Delete { id, revision } => {
                let changed = self.db.execute("DELETE FROM project_skills WHERE user_id=? AND project_id=? AND id=? AND revision=?", &[
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
        // Mutation results include the newly disabled record; normal reads honor
        // per-skill disablement. Eight records keep full resources below the RPC limit.
        let sql = if let Some(id) = selected {
            values.push(DbValue::Int(id));
            "SELECT s.id,s.revision,s.updated_at,s.draft,p.prepared FROM project_skills s LEFT JOIN project_plugin_skills m ON m.skill_id=s.id LEFT JOIN project_plugins p ON p.id=m.plugin_id WHERE s.user_id=? AND s.project_id=? AND s.id=?"
        } else {
            let RecordOperation::List { after } = request.operation else {
                unreachable!("mutation selects its result")
            };
            values.push(DbValue::Int(after));
            "SELECT s.id,s.revision,s.updated_at,s.draft,p.prepared FROM project_skills s LEFT JOIN project_plugin_skills m ON m.skill_id=s.id LEFT JOIN project_plugins p ON p.id=m.plugin_id WHERE s.user_id=? AND s.project_id=? AND s.id>? AND COALESCE(json_extract(s.draft,'$.enabled'),1)<>0 ORDER BY s.id LIMIT 9"
        };
        let result = self.db.execute(sql, &values).await?;
        let records = result
            .rows
            .iter()
            .map(|row| {
                let draft: SkillDraft = serde_json::from_str(row.get_text(3)?)
                    .map_err(|error| StorageError::Db(error.to_string()))?;
                if matches!(request.operation, RecordOperation::Read { .. }) && !draft.enabled {
                    return Err(StorageError::NotFound("Record".into()));
                }
                let origin = row
                    .get_text_opt(4)
                    .map(|json| {
                        let prepared: PreparedPlugin = serde_json::from_str(json)
                            .map_err(|error| StorageError::Db(error.to_string()))?;
                        Ok::<_, StorageError>(openwebide_core::plugins::PluginSkillOrigin {
                            publisher: prepared.manifest.publisher,
                            name: prepared.manifest.name,
                            version: prepared.manifest.version,
                            commit: prepared.source.commit,
                        })
                    })
                    .transpose()?;
                Ok(Record {
                    id: row.get_int(0)?,
                    revision: row.get_int(1)?,
                    updated_at: row.get_int(2)?,
                    value: serde_json::json!({"draft":draft,"origin":origin}),
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        if selected.is_some() && records.is_empty() {
            return Err(StorageError::NotFound("Record".into()));
        }
        let mut records = records;
        let total = records.len();
        let mut bytes = 0;
        let mut count = 0;
        for record in records.iter().take(8) {
            let size = serde_json::to_vec(record)
                .map_err(|error| StorageError::Db(error.to_string()))?
                .len();
            if bytes + size > 1024 * 1024 && count > 0 {
                break;
            }
            bytes += size;
            count += 1;
        }
        let next = if count < total {
            records.truncate(count);
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
