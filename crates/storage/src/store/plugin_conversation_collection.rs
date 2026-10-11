//! Read-only owned conversation metadata; target-selection policy stays in source.
use super::*;
use openwebide_core::plugins::{
    execution::PluginExecutionContext,
    records::{CollectionResult, Record, RecordOperation, RecordRequest},
};
use serde_json::{Value, json};

impl<D: Db> Store<D> {
    /// Raw model preferences and public server metadata, without selection policy.
    pub(super) async fn plugin_configuration_in_transaction(
        &self,
        user: UserId,
        request: &RecordRequest,
    ) -> Result<CollectionResult, StorageError> {
        match request.operation {
            RecordOperation::List { after: 0 } | RecordOperation::Read { id: 1 } => (),
            RecordOperation::List { .. } => {
                return Ok(CollectionResult {
                    enabled: true,
                    records: vec![],
                    next: None,
                });
            }
            RecordOperation::Read { .. } => {
                return Err(StorageError::NotFound("Configuration metadata".into()));
            }
            _ => {
                return Err(StorageError::InvalidRequest(
                    "Configuration metadata is read-only".into(),
                ));
            }
        }
        let defaults = self
            .get_user_setting(user, "model_defaults")
            .await?
            .and_then(|value| serde_json::from_str::<openwebide_core::ModelDefaults>(&value).ok())
            .unwrap_or_default();
        let default_connection = self
            .get_user_setting(user, "default_connection")
            .await?
            .and_then(|value| value.parse::<i64>().ok());
        // These are shared server facts, not per-user copies. No endpoint or credential
        // is exported. The account's preference merely names one of these servers.
        let rows = self
            .db
            .execute(
                "SELECT id,model,enabled FROM connections ORDER BY id LIMIT 257",
                &[],
            )
            .await?;
        if rows.rows.len() > 256 {
            return Err(StorageError::InvalidValue(
                "Configuration metadata holds at most 256 servers".into(),
            ));
        }
        let servers=rows.rows.iter().map(|row|Ok(json!({"id":row.get_int(0)?,"model":row.get_text_opt(1),"enabled":row.get_int(2)?!=0})))
            .collect::<Result<Vec<_>,StorageError>>()?;
        let value = json!({"primary":defaults.primary,"fast":defaults.fast,
            "default_connection":default_connection,"servers":servers});
        let encoded =
            serde_json::to_vec(&value).map_err(|error| StorageError::Db(error.to_string()))?;
        if encoded.len() > 64 * 1024 {
            return Err(StorageError::InvalidValue(
                "Configuration metadata exceeds 64 KiB".into(),
            ));
        }
        let digest = openwebide_core::plugins::content_digest(&encoded);
        let revision = i64::from_str_radix(&digest[..13], 16)
            .map_err(|error| StorageError::Db(error.to_string()))?
            + 1;
        Ok(CollectionResult {
            enabled: true,
            records: vec![Record {
                id: 1,
                revision,
                updated_at: 0,
                value,
            }],
            next: None,
        })
    }
    pub(super) async fn plugin_conversations_in_transaction(
        &self,
        user: UserId,
        context: &PluginExecutionContext,
        request: &RecordRequest,
    ) -> Result<CollectionResult, StorageError> {
        let (filter, selected) = match request.operation {
            RecordOperation::List { after } => ("s.id>?", after),
            RecordOperation::Read { id } => ("s.id=?", id),
            _ => {
                return Err(StorageError::InvalidRequest(
                    "Conversation metadata is read-only; submit prompts through runs".into(),
                ));
            }
        };
        if let Some(project) = context.project_id {
            self.get_project(project, user).await?;
        }
        let rows = self.db.execute(&format!(
            "SELECT s.id,s.name,s.created_at,s.pinned,s.archived,s.auto_title,s.connection_id,coalesce((SELECT max(created_at) FROM messages WHERE session_id=s.id),s.created_at),u.value,c.model,c.enabled FROM sessions s LEFT JOIN user_settings u ON u.user_id=s.user_id AND u.key=('session_model_'||s.id) LEFT JOIN connections c ON c.id=s.connection_id WHERE s.user_id=? AND s.project_id IS ? AND {filter} ORDER BY s.id LIMIT 33"
        ), &[
            DbValue::Int(user.get()), context.project_id.map_or(DbValue::Null,DbValue::Int), DbValue::Int(selected),
        ]).await?;
        if matches!(request.operation, RecordOperation::Read { .. }) && rows.rows.is_empty() {
            return Err(StorageError::NotFound("Conversation".into()));
        }
        let mut records = Vec::new();
        let mut bytes = 128usize;
        let mut next = None;
        for row in &rows.rows {
            if records.len() == 32 {
                next = records.last().map(|record: &Record| record.id);
                break;
            }
            let id = row.get_int(0)?;
            let connection_id = row.get_int_opt(6);
            let value = json!({
                "name":row.get_text(1)?, "created_at":row.get_int(2)?,
                "pinned":row.get_int(3)?!=0, "archived":row.get_int(4)?!=0,
                "auto_title":row.get_int(5)?!=0,
                "connection_id":connection_id,
                "last_activity":row.get_int(7)?,
                "origin":context.session_id==Some(id),
                "model_override":row.get_text_opt(8).map(|value|serde_json::from_str::<Value>(value).unwrap_or_else(|_|json!(value))),
                "connection":connection_id.map(|id|json!({"id":id,"model":row.get_text_opt(9),"enabled":row.get_int_opt(10).is_some_and(|enabled|enabled!=0)})),
            });
            let encoded =
                serde_json::to_vec(&value).map_err(|error| StorageError::Db(error.to_string()))?;
            if encoded.len() > 64 * 1024 {
                return Err(StorageError::InvalidValue(
                    "Conversation metadata exceeds 64 KiB".into(),
                ));
            }
            // Read-only snapshots use an opaque positive content token, not a mutable counter.
            // 52 bits remain exactly representable by JSON consumers using JavaScript numbers.
            let digest = openwebide_core::plugins::content_digest(&encoded);
            let revision = i64::from_str_radix(&digest[..13], 16)
                .map_err(|error| StorageError::Db(error.to_string()))?
                + 1;
            let record = Record {
                id,
                revision,
                updated_at: row.get_int(7)?,
                value,
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
}
