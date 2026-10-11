//! General plugin-owned records. Scope is attached by the authenticated host.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_RECORD_BYTES: usize = 64 * 1024;
pub const MAX_RECORDS: usize = 1000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordRequest {
    pub collection: String,
    pub operation: RecordOperation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn public_record_envelope_rejects_caller_supplied_scope_and_invalid_revisions() {
        let valid =
            json!({"collection":"notes","operation":{"action":"create","value":{"body":"text"}}});
        let request: RecordRequest = serde_json::from_value(valid.clone()).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), valid);
        let mut forged = valid.clone();
        forged["user_id"] = json!(42);
        assert!(serde_json::from_value::<RecordRequest>(forged).is_err());
        let mut forged = valid;
        forged["operation"]["project_id"] = json!(42);
        assert!(serde_json::from_value::<RecordRequest>(forged).is_err());
        for operation in [
            RecordOperation::Read { id: 0 },
            RecordOperation::List { after: -1 },
            RecordOperation::Delete { id: 1, revision: 0 },
        ] {
            assert!(
                RecordRequest {
                    collection: "notes".into(),
                    operation
                }
                .validate()
                .is_err()
            );
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecordOperation {
    List {
        #[serde(default)]
        after: i64,
    },
    Read {
        id: i64,
    },
    Create {
        value: Value,
    },
    Update {
        id: i64,
        revision: i64,
        value: Value,
    },
    Delete {
        id: i64,
        revision: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: i64,
    pub revision: i64,
    pub updated_at: i64,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordResult {
    pub records: Vec<Record>,
    pub next: Option<i64>,
}

/// Shared project collections can be disabled by the user independently of
/// plugin installation. Their values use the app's documented data schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollectionResult {
    pub enabled: bool,
    pub records: Vec<Record>,
    pub next: Option<i64>,
}

impl RecordRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.validate_with_limit(MAX_RECORD_BYTES)
    }

    /// Shared collections can admit their existing schema size without changing
    /// private record quotas. The persistence adapter chooses the limit.
    pub fn validate_with_limit(&self, max_value_bytes: usize) -> Result<(), String> {
        if self.collection.is_empty()
            || self.collection.len() > 64
            || !self.collection.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(
                "Record collections use 1–64 lowercase letters, digits, underscores or hyphens."
                    .into(),
            );
        }
        match &self.operation {
            RecordOperation::List { after } if *after < 0 => {
                return Err("Invalid record cursor.".into());
            }
            RecordOperation::Read { id }
            | RecordOperation::Update { id, .. }
            | RecordOperation::Delete { id, .. }
                if *id <= 0 =>
            {
                return Err("Invalid record ID.".into());
            }
            _ => (),
        }
        if matches!(&self.operation, RecordOperation::Update {revision, ..} | RecordOperation::Delete {revision, ..} if *revision <= 0)
        {
            return Err("Record mutations require a positive revision.".into());
        }
        if let RecordOperation::Create { value } | RecordOperation::Update { value, .. } =
            &self.operation
            && serde_json::to_vec(value)
                .map_err(|error| error.to_string())?
                .len()
                > max_value_bytes
        {
            return Err(format!("Record value exceeds {max_value_bytes} bytes."));
        }
        Ok(())
    }
}
