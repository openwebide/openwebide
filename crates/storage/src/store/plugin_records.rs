//! Authenticated scope and transactional CRUD; plugins own record semantics.
use super::*;
use openwebide_core::plugins::records::{
    MAX_RECORDS, Record, RecordOperation, RecordRequest, RecordResult,
};

impl<D: Db> Store<D> {
    /// `plugin` is a verified namespace supplied by the invocation host.
    /// A session cannot select another project's records, and records outlive
    /// plugin version changes or removal so reinstalling can recover its state.
    pub async fn plugin_records(
        &self,
        user: UserId,
        session: i64,
        plugin: &str,
        request: &RecordRequest,
        now: i64,
    ) -> Result<RecordResult, StorageError> {
        self.db
            .transaction(|tx| async move {
                Store::new(tx)
                    .plugin_records_in_transaction(user, session, plugin, request, now)
                    .await
            })
            .await
    }
    pub(super) async fn plugin_records_in_transaction(
        &self,
        user: UserId,
        session: i64,
        plugin: &str,
        request: &RecordRequest,
        now: i64,
    ) -> Result<RecordResult, StorageError> {
        let project = self.get_session(session, user).await?.project_id;
        self.plugin_project_records_in_transaction(user, project, plugin, request, now)
            .await
    }
    pub(super) async fn plugin_project_records_in_transaction(
        &self,
        user: UserId,
        project: Option<i64>,
        plugin: &str,
        request: &RecordRequest,
        now: i64,
    ) -> Result<RecordResult, StorageError> {
        request.validate().map_err(StorageError::InvalidRequest)?;
        if plugin.is_empty()
            || plugin.len() > 128
            || !plugin.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'/')
            })
        {
            return Err(StorageError::InvalidRequest(
                "Invalid plugin namespace".into(),
            ));
        }
        let store = self;
        let project = project.unwrap_or(0);
        if project != 0 {
            store.get_project(project, user).await?;
        }
        let mut scope = vec![
            DbValue::Int(user.get()),
            DbValue::Int(project),
            DbValue::Text(plugin.into()),
            DbValue::Text(request.collection.clone()),
        ];
        let mut read_id = None;
        match &request.operation {
            RecordOperation::Create { value } => {
                let count = store.db.execute("SELECT COUNT(*) FROM plugin_records WHERE user_id=? AND project_scope=? AND plugin=?", &scope[..3]).await?;
                if count.rows[0].get_int(0)? >= i64::try_from(MAX_RECORDS).unwrap_or(i64::MAX) {
                    return Err(StorageError::InvalidRequest(
                        "Plugin record collection is full".into(),
                    ));
                }
                let mut values = scope.clone();
                values.extend([DbValue::Text(value.to_string()), DbValue::Int(now)]);
                read_id = Some(store.db.execute("INSERT INTO plugin_records(user_id,project_scope,plugin,collection,value,updated_at) VALUES(?,?,?,?,?,?)", &values).await?.last_insert_rowid);
            }
            RecordOperation::Update {
                id,
                revision,
                value,
            } => {
                let mut values = vec![DbValue::Text(value.to_string()), DbValue::Int(now)];
                values.extend(scope.clone());
                values.extend([DbValue::Int(*id), DbValue::Int(*revision)]);
                let changed = store.db.execute("UPDATE plugin_records SET value=?,updated_at=?,revision=revision+1 WHERE user_id=? AND project_scope=? AND plugin=? AND collection=? AND id=? AND revision=?", &values).await?;
                if changed.changes != 1 {
                    return Err(StorageError::Conflict(
                        "Record changed or is unavailable".into(),
                    ));
                }
                read_id = Some(*id);
            }
            RecordOperation::Delete { id, revision } => {
                scope.extend([DbValue::Int(*id), DbValue::Int(*revision)]);
                let changed = store.db.execute("DELETE FROM plugin_records WHERE user_id=? AND project_scope=? AND plugin=? AND collection=? AND id=? AND revision=?", &scope).await?;
                if changed.changes != 1 {
                    return Err(StorageError::Conflict(
                        "Record changed or is unavailable".into(),
                    ));
                }
                return Ok(RecordResult {
                    records: Vec::new(),
                    next: None,
                });
            }
            RecordOperation::Read { id } => read_id = Some(*id),
            RecordOperation::List { .. } => (),
        }
        let sql = if let Some(id) = read_id {
            scope.push(DbValue::Int(id));
            "SELECT id,revision,updated_at,value FROM plugin_records WHERE user_id=? AND project_scope=? AND plugin=? AND collection=? AND id=?"
        } else {
            let RecordOperation::List { after } = request.operation else {
                unreachable!("mutation selects its result")
            };
            scope.push(DbValue::Int(after));
            "SELECT id,revision,updated_at,value FROM plugin_records WHERE user_id=? AND project_scope=? AND plugin=? AND collection=? AND id>? ORDER BY id LIMIT 33"
        };
        let rows = store.db.execute(sql, &scope).await?;
        let mut records = rows
            .rows
            .iter()
            .map(|row| {
                Ok(Record {
                    id: row.get_int(0)?,
                    revision: row.get_int(1)?,
                    updated_at: row.get_int(2)?,
                    value: serde_json::from_str(row.get_text(3)?)
                        .map_err(|error| StorageError::Db(error.to_string()))?,
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        if read_id.is_some() && records.is_empty() {
            return Err(StorageError::NotFound("plugin record".into()));
        }
        let next = if records.len() > 32 {
            records.truncate(32);
            records.last().map(|record| record.id)
        } else {
            None
        };
        Ok(RecordResult { records, next })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use futures::executor::block_on;
    use serde_json::json;

    fn request(operation: RecordOperation) -> RecordRequest {
        RecordRequest {
            collection: "notes".into(),
            operation,
        }
    }
    #[test]
    fn records_are_owned_revision_safe_and_shared_between_project_sessions_in_both_modes() {
        block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                store.migrate().await.unwrap();
                store.migrate().await.unwrap();
                let owner = store
                    .insert_user("owner", "hash", UserRole::Admin, 0)
                    .await
                    .unwrap()
                    .id;
                let other = store
                    .insert_user("other", "hash", UserRole::User, 0)
                    .await
                    .unwrap()
                    .id;
                let project = store
                    .create_project(
                        &NewProject {
                            name: "p".into(),
                            mode,
                            path: Some("p".into()),
                        },
                        owner,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                let first = store
                    .create_session("first", None, None, Some(project), owner, 0)
                    .await
                    .unwrap()
                    .id;
                let second = store
                    .create_session("second", None, None, Some(project), owner, 0)
                    .await
                    .unwrap()
                    .id;
                let projectless = store
                    .create_session("other", None, None, None, owner, 0)
                    .await
                    .unwrap()
                    .id;
                let saved = store
                    .plugin_records(
                        owner,
                        first,
                        "example/notes",
                        &request(RecordOperation::Create {
                            value: json!({"body":"🦀"}),
                        }),
                        1,
                    )
                    .await
                    .unwrap();
                let entry = &saved.records[0];
                let read = request(RecordOperation::Read { id: entry.id });
                assert_eq!(
                    store
                        .plugin_records(owner, second, "example/notes", &read, 2)
                        .await
                        .unwrap(),
                    saved
                );
                for (user, session, plugin) in [
                    (other, first, "example/notes"),
                    (owner, projectless, "example/notes"),
                    (owner, first, "example/other"),
                ] {
                    assert!(
                        store
                            .plugin_records(user, session, plugin, &read, 2)
                            .await
                            .is_err()
                    );
                }
                let update = request(RecordOperation::Update {
                    id: entry.id,
                    revision: entry.revision,
                    value: json!({"body":"new"}),
                });
                let edited = store
                    .plugin_records(owner, second, "example/notes", &update, 3)
                    .await
                    .unwrap();
                assert_eq!(edited.records[0].revision, 2);
                assert!(matches!(
                    store
                        .plugin_records(owner, first, "example/notes", &update, 4)
                        .await,
                    Err(StorageError::Conflict(_))
                ));
                assert_eq!(
                    store
                        .plugin_records(owner, first, "example/notes", &read, 5)
                        .await
                        .unwrap(),
                    edited
                );
                let delete = request(RecordOperation::Delete {
                    id: entry.id,
                    revision: 2,
                });
                assert!(
                    store
                        .plugin_records(owner, second, "example/other", &delete, 6)
                        .await
                        .is_err()
                );
                store
                    .plugin_records(owner, second, "example/notes", &delete, 6)
                    .await
                    .unwrap();
                assert!(
                    store
                        .plugin_records(owner, first, "example/notes", &read, 7)
                        .await
                        .is_err()
                );
            }
        });
    }
    #[test]
    fn pages_are_bounded_and_collection_and_value_limits_apply_before_writes() {
        block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let owner = store
                .insert_user("owner", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("s", None, None, None, owner, 0)
                .await
                .unwrap()
                .id;
            let create = request(RecordOperation::Create {
                value: json!({"body":"value"}),
            });
            for time in 1..=35 {
                store
                    .plugin_records(owner, session, "example/notes", &create, time)
                    .await
                    .unwrap();
            }
            let first = store
                .plugin_records(
                    owner,
                    session,
                    "example/notes",
                    &request(RecordOperation::List { after: 0 }),
                    40,
                )
                .await
                .unwrap();
            assert_eq!(first.records.len(), 32);
            let second = store
                .plugin_records(
                    owner,
                    session,
                    "example/notes",
                    &request(RecordOperation::List {
                        after: first.next.unwrap(),
                    }),
                    40,
                )
                .await
                .unwrap();
            assert_eq!(second.records.len(), 3);
            assert!(second.next.is_none());
            assert!(
                first
                    .records
                    .iter()
                    .all(|a| second.records.iter().all(|b| a.id != b.id))
            );
            let mut invalid = create.clone();
            invalid.collection = "../other".into();
            assert!(
                store
                    .plugin_records(owner, session, "example/notes", &invalid, 41)
                    .await
                    .is_err()
            );
            invalid = request(RecordOperation::Create {
                value: json!("x".repeat(65536)),
            });
            assert!(
                store
                    .plugin_records(owner, session, "example/notes", &invalid, 41)
                    .await
                    .is_err()
            );
            let count = store
                .db
                .execute("SELECT COUNT(*) FROM plugin_records", &[])
                .await
                .unwrap();
            assert_eq!(count.rows[0].get_int(0).unwrap(), 35);
        });
    }
    #[test]
    fn quota_spans_collections_and_project_deletion_removes_project_records() {
        block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let owner = store
                .insert_user("owner", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let project = store
                .create_project(
                    &NewProject {
                        name: "p".into(),
                        mode: WorkspaceMode::Remote,
                        path: Some("p".into()),
                    },
                    owner,
                    0,
                )
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("s", None, None, Some(project), owner, 0)
                .await
                .unwrap()
                .id;
            let create = request(RecordOperation::Create {
                value: serde_json::json!(true),
            });
            for time in 0..MAX_RECORDS {
                store
                    .plugin_records(
                        owner,
                        session,
                        "example/notes",
                        &create,
                        i64::try_from(time).unwrap(),
                    )
                    .await
                    .unwrap();
            }
            let mut another = create.clone();
            another.collection = "another".into();
            assert!(matches!(
                store
                    .plugin_records(owner, session, "example/notes", &another, 2000)
                    .await,
                Err(StorageError::InvalidRequest(_))
            ));
            assert!(
                store
                    .plugin_records(owner, session, "example/other", &another, 2000)
                    .await
                    .is_ok()
            );
            store
                .db
                .execute("DELETE FROM projects WHERE id=?", &[DbValue::Int(project)])
                .await
                .unwrap();
            let count = store
                .db
                .execute("SELECT COUNT(*) FROM plugin_records", &[])
                .await
                .unwrap();
            assert_eq!(count.rows[0].get_int(0).unwrap(), 0);
        });
    }
}
