//! Typed repositories over a [`Db`].

mod assistance;
mod branches;
mod chat_queue;
mod editor_recovery;
mod goals;
pub use goals::GoalTurnAssessment;
mod host_admin;
mod memories;
mod model_setup;
mod plugin_background;
mod plugin_collections;
mod plugin_conversation_collection;
mod plugin_grants;
mod plugin_jobs;
mod plugin_records;
mod plugin_runs;
mod plugin_skill_collection;
mod plugin_task_collection;
mod plugin_task_history;
mod plugins;
mod push;
mod questions;
mod reviews;
mod rewind;
mod rows;
mod skills;
pub use push::PushDelivery;
#[cfg(test)]
mod monitor_tests;
mod scheduled;
mod sessions;
mod tasks;
mod todos;
mod tool_timing;

use rows::*;

use std::collections::BTreeMap;

use openwebide_core::{
    ChatMessage, ChatSession, Connection, ConversationEntry, EditDecision, FileDiff, NewConnection,
    NewProject, NewSession, PersistedEdit, Project, ProviderKind, ResolveEditRequest, Role,
    SystemPrompt, ToolCall, ToolStep, TurnTelemetry, User, UserId, UserRole, WorkspaceMode,
};

use crate::db::{Db, DbValue};
use crate::{StorageError, migrations};

pub struct Store<D: Db> {
    db: D,
}

/// A user row including the password hash. The hash is internal to storage;
/// the API never returns it (it maps to [`User`], which omits it).
#[derive(Debug, Clone)]
pub struct UserRecord {
    pub id: UserId,
    pub username: String,
    pub password_hash: String,
    pub role: UserRole,
    pub created_at: i64,
    pub token_epoch: i64,
}

impl UserRecord {
    /// The public form of this account, without the password hash.
    pub fn public(&self) -> User {
        User {
            id: self.id,
            username: self.username.clone(),
            role: self.role,
            created_at: self.created_at,
        }
    }
}

impl<D: Db> Store<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }

    /// Apply idempotent schema migrations.
    pub async fn migrate(&self) -> Result<(), StorageError> {
        self.migrate_with(&|_| false).await
    }

    /// Apply schema migrations, letting `probe` ("does this mount-relative
    /// directory exist?") answer the filesystem questions the steps ask
    /// (the Docker path rewrite).
    pub async fn migrate_with(
        &self,
        probe: &(dyn Fn(&str) -> bool + Send + Sync),
    ) -> Result<(), StorageError> {
        migrations::apply(&self.db, probe).await
    }

    // -- users -------------------------------------------------------------

    const USER_COLUMNS: &'static str = "id, username, password_hash, role, created_at, token_epoch";

    pub async fn insert_first_admin(
        &self,
        username: &str,
        password_hash: &str,
        created_at: i64,
    ) -> Result<Option<UserRecord>, StorageError> {
        let res = self.db.transaction(|tx| async move {
            let res = tx.execute(
                "INSERT INTO users (username, password_hash, role, created_at) SELECT ?, ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM users)",
                &[
                    DbValue::Text(username.into()),
                    DbValue::Text(password_hash.into()),
                    DbValue::Text(UserRole::Admin.as_str().into()),
                    DbValue::Int(created_at),
                ],
            )
            .await?;

            if res.changes == 0 {
                return Ok(None);
            }

            let rows = tx.execute(
                "SELECT id, username, password_hash, role, created_at, token_epoch FROM users WHERE username = ?",
                &[DbValue::Text(username.into())]
            ).await?;

            rows.rows.first().map(user_from_row).transpose()?.map(Some)
                .ok_or_else(|| StorageError::NotFound("User not found after insert".into()))
        }).await?;
        Ok(res)
    }

    pub async fn insert_user(
        &self,
        username: &str,
        password_hash: &str,
        role: UserRole,
        created_at: i64,
    ) -> Result<UserRecord, StorageError> {
        self.db
            .execute(
                "INSERT INTO users (username, password_hash, role, created_at)
                 VALUES (?, ?, ?, ?)",
                &[
                    DbValue::Text(username.into()),
                    DbValue::Text(password_hash.into()),
                    DbValue::Text(role.as_str().into()),
                    DbValue::Int(created_at),
                ],
            )
            .await?;
        self.get_user_by_username(username).await?.ok_or_else(|| {
            StorageError::NotFound(format!("user {username} not found after insert"))
        })
    }

    pub async fn get_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<UserRecord>, StorageError> {
        let res = self
            .db
            .execute(
                &format!(
                    "SELECT {} FROM users WHERE username = ?",
                    Self::USER_COLUMNS
                ),
                &[DbValue::Text(username.into())],
            )
            .await?;
        res.rows.first().map(user_from_row).transpose()
    }

    pub async fn get_user(&self, id: UserId) -> Result<Option<UserRecord>, StorageError> {
        let res = self
            .db
            .execute(
                &format!("SELECT {} FROM users WHERE id = ?", Self::USER_COLUMNS),
                &[DbValue::Int(id.get())],
            )
            .await?;
        res.rows.first().map(user_from_row).transpose()
    }

    pub async fn count_users(&self) -> Result<i64, StorageError> {
        let res = self.db.execute("SELECT COUNT(*) FROM users", &[]).await?;
        Ok(res
            .rows
            .first()
            .and_then(|r| r.values.first())
            .and_then(|v| match v {
                DbValue::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(0))
    }

    pub async fn list_users(&self) -> Result<Vec<UserRecord>, StorageError> {
        let res = self
            .db
            .execute(
                &format!("SELECT {} FROM users ORDER BY id", Self::USER_COLUMNS),
                &[],
            )
            .await?;
        res.rows.iter().map(user_from_row).collect()
    }

    /// Give the first registered user ownership of any projects created
    /// before accounts existed.
    pub async fn reassign_orphaned_projects(&self, user_id: UserId) -> Result<u64, StorageError> {
        let res = self
            .db
            .execute(
                "UPDATE projects SET user_id = ? WHERE user_id IS NULL",
                &[DbValue::Int(user_id.get())],
            )
            .await?;
        Ok(res.changes)
    }

    /// Give the first registered user ownership of any sessions created before
    /// accounts existed, so pre-auth chat history isn't lost to scoping.
    pub async fn reassign_orphaned_sessions(&self, user_id: UserId) -> Result<u64, StorageError> {
        let res = self
            .db
            .execute(
                "UPDATE sessions SET user_id = ? WHERE user_id IS NULL",
                &[DbValue::Int(user_id.get())],
            )
            .await?;
        Ok(res.changes)
    }

    // -- settings ----------------------------------------------------------

    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT value FROM settings WHERE key = ?",
                &[DbValue::Text(key.into())],
            )
            .await?;
        Ok(res
            .rows
            .first()
            .and_then(|r| r.get_text(0).ok().map(str::to_string)))
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<(), StorageError> {
        self.db
            .execute(
                "INSERT INTO settings (key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                &[DbValue::Text(key.into()), DbValue::Text(value.into())],
            )
            .await?;
        Ok(())
    }

    pub async fn delete_setting(&self, key: &str) -> Result<(), StorageError> {
        self.db
            .execute(
                "DELETE FROM settings WHERE key = ?",
                &[DbValue::Text(key.into())],
            )
            .await?;
        Ok(())
    }

    /// Insert a setting only if the key is absent, so concurrent first
    /// writers cannot clobber each other. Returns whether the row was
    /// inserted.
    pub async fn insert_setting_if_absent(
        &self,
        key: &str,
        value: &str,
    ) -> Result<bool, StorageError> {
        let res = self
            .db
            .execute(
                "INSERT INTO settings (key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO NOTHING",
                &[DbValue::Text(key.into()), DbValue::Text(value.into())],
            )
            .await?;
        Ok(res.changes > 0)
    }

    pub async fn all_settings(&self) -> Result<BTreeMap<String, String>, StorageError> {
        let res = self
            .db
            .execute("SELECT key, value FROM settings ORDER BY key", &[])
            .await?;
        let mut map = BTreeMap::new();
        for row in &res.rows {
            map.insert(row.get_text(0)?.to_string(), row.get_text(1)?.to_string());
        }
        Ok(map)
    }

    // -- user settings -----------------------------------------------------

    pub async fn get_user_setting(
        &self,
        user_id: UserId,
        key: &str,
    ) -> Result<Option<String>, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT value FROM user_settings WHERE user_id = ? AND key = ?",
                &[DbValue::Int(user_id.get()), DbValue::Text(key.into())],
            )
            .await?;
        Ok(res
            .rows
            .first()
            .and_then(|r| r.get_text(0).ok().map(str::to_string)))
    }

    pub async fn set_user_setting(
        &self,
        user_id: UserId,
        key: &str,
        value: &str,
    ) -> Result<(), StorageError> {
        if key.starts_with("editor_recovery_") {
            return Err(StorageError::InvalidValue(
                "Use the versioned editor recovery API".into(),
            ));
        }
        self.db
            .execute(
                "INSERT INTO user_settings (user_id, key, value) VALUES (?, ?, ?)
                 ON CONFLICT(user_id, key) DO UPDATE SET value = excluded.value",
                &[
                    DbValue::Int(user_id.get()),
                    DbValue::Text(key.into()),
                    DbValue::Text(value.into()),
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn all_user_settings(
        &self,
        user_id: UserId,
    ) -> Result<BTreeMap<String, String>, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT key, value FROM user_settings WHERE user_id = ? AND key NOT GLOB 'editor_recovery_*' ORDER BY key",
                &[DbValue::Int(user_id.get())],
            )
            .await?;
        let mut map = BTreeMap::new();
        for row in &res.rows {
            map.insert(row.get_text(0)?.to_string(), row.get_text(1)?.to_string());
        }
        Ok(map)
    }

    // -- connections -------------------------------------------------------

    pub async fn list_connections(&self) -> Result<Vec<Connection>, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT id, name, kind, base_url, model, enabled, context_limit, tool_stream_unsupported, tool_stream_revision, tool_selection
                 FROM connections ORDER BY id",
                &[],
            )
            .await?;
        res.rows.iter().map(connection_from_row).collect()
    }

    pub async fn get_connection(&self, id: i64) -> Result<Connection, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT id, name, kind, base_url, model, enabled, context_limit, tool_stream_unsupported, tool_stream_revision, tool_selection
                 FROM connections WHERE id = ?",
                &[DbValue::Int(id)],
            )
            .await?;
        res.rows
            .first()
            .map(connection_from_row)
            .transpose()?
            .ok_or_else(|| StorageError::NotFound(format!("connection {id}")))
    }

    pub async fn insert_connection(&self, new: &NewConnection) -> Result<Connection, StorageError> {
        let res = self
            .db
            .execute(
                "INSERT INTO connections (name, kind, base_url, model, context_limit)
                 VALUES (?, ?, ?, ?, ?)",
                &[
                    DbValue::Text(new.name.clone()),
                    DbValue::Text(new.kind.as_str().into()),
                    DbValue::Text(new.base_url.clone()),
                    new.model
                        .as_ref()
                        .map(|m| DbValue::Text(m.clone()))
                        .unwrap_or(DbValue::Null),
                    new.context_limit
                        .map(|n| DbValue::Int(i64::try_from(n).unwrap_or(i64::MAX)))
                        .unwrap_or(DbValue::Null),
                ],
            )
            .await?;
        self.get_connection(res.last_insert_rowid).await
    }

    pub async fn update_connection(&self, conn: &Connection) -> Result<(), StorageError> {
        conn.tool_selection
            .validate()
            .map_err(StorageError::InvalidValue)?;
        let res = self
            .db
            .execute(
                "UPDATE connections
                 SET tool_stream_unsupported = CASE WHEN base_url != ? OR kind != ? THEN 0 ELSE tool_stream_unsupported END,
                     tool_stream_revision = CASE WHEN base_url != ? OR kind != ? THEN tool_stream_revision + 1 ELSE tool_stream_revision END,
                     name = ?, kind = ?, base_url = ?, model = ?, enabled = ?, context_limit = ?, tool_selection = ?
                 WHERE id = ?",
                &[
                    DbValue::Text(conn.base_url.clone()),
                    DbValue::Text(conn.kind.as_str().into()),
                    DbValue::Text(conn.base_url.clone()),
                    DbValue::Text(conn.kind.as_str().into()),
                    DbValue::Text(conn.name.clone()),
                    DbValue::Text(conn.kind.as_str().into()),
                    DbValue::Text(conn.base_url.clone()),
                    conn.model
                        .as_ref()
                        .map(|m| DbValue::Text(m.clone()))
                        .unwrap_or(DbValue::Null),
                    DbValue::Int(i64::from(conn.enabled)),
                    conn.context_limit
                        .map(|n| DbValue::Int(i64::try_from(n).unwrap_or(i64::MAX)))
                        .unwrap_or(DbValue::Null),
                    DbValue::Text(serde_json::to_string(&conn.tool_selection).map_err(|error| StorageError::InvalidValue(error.to_string()))?),
                    DbValue::Int(conn.id),
                ],
            )
            .await?;
        if res.changes == 0 {
            return Err(StorageError::NotFound(format!("connection {}", conn.id)));
        }
        Ok(())
    }

    pub async fn set_tool_stream_unsupported(
        &self,
        connection_id: i64,
        tool_stream_revision: i64,
    ) -> Result<(), StorageError> {
        self.db
            .execute(
                "UPDATE connections SET tool_stream_unsupported = 1 WHERE id = ? AND tool_stream_revision = ?",
                &[DbValue::Int(connection_id), DbValue::Int(tool_stream_revision)],
            )
            .await?;
        Ok(())
    }

    pub async fn delete_connection(&self, id: i64) -> Result<(), StorageError> {
        let res = self
            .db
            .execute("DELETE FROM connections WHERE id = ?", &[DbValue::Int(id)])
            .await?;
        if res.changes == 0 {
            return Err(StorageError::NotFound(format!("connection {id}")));
        }
        Ok(())
    }

    // -- system prompts ------------------------------------------------------

    pub async fn list_system_prompts(
        &self,
        user_id: UserId,
    ) -> Result<Vec<SystemPrompt>, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT id, name, content FROM system_prompts WHERE user_id = ? ORDER BY id",
                &[DbValue::Int(user_id.get())],
            )
            .await?;
        res.rows.iter().map(prompt_from_row).collect()
    }

    pub async fn insert_system_prompt(
        &self,
        user_id: UserId,
        name: &str,
        content: &str,
    ) -> Result<SystemPrompt, StorageError> {
        let res = self
            .db
            .execute(
                "INSERT INTO system_prompts (name, content, user_id) VALUES (?, ?, ?)",
                &[
                    DbValue::Text(name.into()),
                    DbValue::Text(content.into()),
                    DbValue::Int(user_id.get()),
                ],
            )
            .await?;
        self.get_system_prompt(res.last_insert_rowid, user_id).await
    }

    pub async fn get_system_prompt(
        &self,
        id: i64,
        user_id: UserId,
    ) -> Result<SystemPrompt, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT id, name, content FROM system_prompts WHERE id = ? AND user_id = ?",
                &[DbValue::Int(id), DbValue::Int(user_id.get())],
            )
            .await?;
        res.rows
            .first()
            .map(prompt_from_row)
            .transpose()?
            .ok_or_else(|| StorageError::NotFound(format!("system prompt {id}")))
    }

    pub async fn update_system_prompt(
        &self,
        id: i64,
        user_id: UserId,
        name: &str,
        content: &str,
    ) -> Result<SystemPrompt, StorageError> {
        let res = self
            .db
            .execute(
                "UPDATE system_prompts SET name = ?, content = ? WHERE id = ? AND user_id = ?",
                &[
                    DbValue::Text(name.into()),
                    DbValue::Text(content.into()),
                    DbValue::Int(id),
                    DbValue::Int(user_id.get()),
                ],
            )
            .await?;
        if res.changes == 0 {
            return Err(StorageError::NotFound(format!("system prompt {id}")));
        }
        self.get_system_prompt(id, user_id).await
    }

    pub async fn delete_system_prompt(&self, id: i64, user_id: UserId) -> Result<(), StorageError> {
        self.db.transaction(|tx| async move {
            let res = tx.execute("DELETE FROM system_prompts WHERE id = ? AND user_id = ?", &[DbValue::Int(id), DbValue::Int(user_id.get())]).await?;
            if res.changes == 0 {
                return Err(StorageError::NotFound(format!("system prompt {id}")));
            }
            tx.execute("DELETE FROM user_settings WHERE user_id = ? AND key = 'default_prompt' AND value = ?", &[DbValue::Int(user_id.get()), DbValue::Text(id.to_string())]).await?;
            Ok(())
        }).await
    }

    pub async fn reassign_orphaned_system_prompts(
        &self,
        user_id: UserId,
    ) -> Result<(), StorageError> {
        self.db
            .execute(
                "UPDATE system_prompts SET user_id = ? WHERE user_id IS NULL",
                &[DbValue::Int(user_id.get())],
            )
            .await?;
        Ok(())
    }

    // -- projects ------------------------------------------------------------
    //
    // Every project is owned by a user; these methods always filter by
    // `user_id` so one account never sees another's projects.

    const PROJECT_COLUMNS: &'static str = "id, name, mode, path, user_id, created_at";

    pub async fn list_projects(&self, user_id: UserId) -> Result<Vec<Project>, StorageError> {
        let res = self
            .db
            .execute(
                &format!(
                    "SELECT {} FROM projects WHERE user_id = ? ORDER BY id",
                    Self::PROJECT_COLUMNS
                ),
                &[DbValue::Int(user_id.get())],
            )
            .await?;
        res.rows.iter().map(project_from_row).collect()
    }

    pub async fn get_project(&self, id: i64, user_id: UserId) -> Result<Project, StorageError> {
        let res = self
            .db
            .execute(
                &format!(
                    "SELECT {} FROM projects WHERE id = ? AND user_id = ?",
                    Self::PROJECT_COLUMNS
                ),
                &[DbValue::Int(id), DbValue::Int(user_id.get())],
            )
            .await?;
        res.rows
            .first()
            .map(project_from_row)
            .transpose()?
            .ok_or_else(|| StorageError::NotFound(format!("project {id}")))
    }

    pub async fn create_project(
        &self,
        new: &NewProject,
        user_id: UserId,
        created_at: i64,
    ) -> Result<Project, StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let project = store
                    .create_project_in_transaction(new, user_id, created_at)
                    .await?;
                store
                    .inherit_plugin_defaults(user_id, project.id, created_at)
                    .await?;
                Ok(project)
            })
            .await
    }

    async fn create_project_in_transaction(
        &self,
        new: &NewProject,
        user_id: UserId,
        created_at: i64,
    ) -> Result<Project, StorageError> {
        // Re-opening a folder that already has a project returns that
        // project instead of creating a duplicate: closing a tab only hides
        // it, so the same folder can be opened again later. The unique
        // index on (user_id, mode, path) makes the insert race-free.
        if let Some(path) = &new.path {
            self.db
                .execute(
                    "INSERT INTO projects (name, mode, path, user_id, created_at)
                     VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT DO NOTHING",
                    &[
                        DbValue::Text(new.name.clone()),
                        DbValue::Text(new.mode.as_str().into()),
                        DbValue::Text(path.clone()),
                        DbValue::Int(user_id.get()),
                        DbValue::Int(created_at),
                    ],
                )
                .await?;
            let res = self
                .db
                .execute(
                    "SELECT id FROM projects
                     WHERE user_id = ? AND mode = ? AND path = ?",
                    &[
                        DbValue::Int(user_id.get()),
                        DbValue::Text(new.mode.as_str().into()),
                        DbValue::Text(path.clone()),
                    ],
                )
                .await?;
            let id = res
                .rows
                .first()
                .ok_or_else(|| StorageError::NotFound("project not found after insert".into()))?
                .get_int(0)?;
            return self.get_project(id, user_id).await;
        }
        let res = self
            .db
            .execute(
                "INSERT INTO projects (name, mode, path, user_id, created_at)
                 VALUES (?, ?, ?, ?, ?)",
                &[
                    DbValue::Text(new.name.clone()),
                    DbValue::Text(new.mode.as_str().into()),
                    DbValue::Null,
                    DbValue::Int(user_id.get()),
                    DbValue::Int(created_at),
                ],
            )
            .await?;
        self.get_project(res.last_insert_rowid, user_id).await
    }

    pub async fn rename_project(
        &self,
        id: i64,
        name: &str,
        user_id: UserId,
    ) -> Result<Project, StorageError> {
        let res = self
            .db
            .execute(
                "UPDATE projects SET name = ? WHERE id = ? AND user_id = ?",
                &[
                    DbValue::Text(name.into()),
                    DbValue::Int(id),
                    DbValue::Int(user_id.get()),
                ],
            )
            .await?;
        if res.changes == 0 {
            return Err(StorageError::NotFound(format!("project {id}")));
        }
        self.get_project(id, user_id).await
    }

    pub async fn delete_project(&self, id: i64, user_id: UserId) -> Result<(), StorageError> {
        self.db
            .transaction(|tx| async move {
                let check = tx
                    .execute(
                        "SELECT 1 FROM projects WHERE id = ? AND user_id = ?",
                        &[DbValue::Int(id), DbValue::Int(user_id.get())],
                    )
                    .await?;
                if check.rows.is_empty() {
                    return Err(StorageError::NotFound(format!("project {id}")));
                }

                tx.execute(
                    "DELETE FROM user_settings WHERE user_id = ? AND key = ?",
                    &[
                        DbValue::Int(user_id.get()),
                        DbValue::Text(format!("editor_recovery_{id}")),
                    ],
                )
                .await?;
                tx.execute(
                    "DELETE FROM projects WHERE id = ? AND user_id = ?",
                    &[DbValue::Int(id), DbValue::Int(user_id.get())],
                )
                .await?;

                Ok(())
            })
            .await
    }

    // -- sessions ------------------------------------------------------------
    //
    // Sessions are owned by a user (set at creation); every lookup filters by
    // `user_id` so one account never reads another's conversations.

    const SESSION_COLUMNS: &'static str = "id, name, connection_id, system_prompt_id, project_id, user_id, created_at, pinned, archived, auto_title, title_revision";

    pub async fn list_sessions(&self, user_id: UserId) -> Result<Vec<ChatSession>, StorageError> {
        let res = self
            .db
            .execute(
                &format!(
                    "SELECT {} FROM sessions WHERE user_id = ? ORDER BY id",
                    Self::SESSION_COLUMNS
                ),
                &[DbValue::Int(user_id.get())],
            )
            .await?;
        res.rows.iter().map(session_from_row).collect()
    }

    pub async fn list_sessions_for_project(
        &self,
        project_id: i64,
        user_id: UserId,
    ) -> Result<Vec<ChatSession>, StorageError> {
        let res = self
            .db
            .execute(
                &format!(
                    "SELECT {} FROM sessions WHERE project_id = ? AND user_id = ? ORDER BY id",
                    Self::SESSION_COLUMNS
                ),
                &[DbValue::Int(project_id), DbValue::Int(user_id.get())],
            )
            .await?;
        res.rows.iter().map(session_from_row).collect()
    }

    pub async fn get_session(&self, id: i64, user_id: UserId) -> Result<ChatSession, StorageError> {
        let res = self
            .db
            .execute(
                &format!(
                    "SELECT {} FROM sessions WHERE id = ? AND user_id = ?",
                    Self::SESSION_COLUMNS
                ),
                &[DbValue::Int(id), DbValue::Int(user_id.get())],
            )
            .await?;
        res.rows
            .first()
            .map(session_from_row)
            .transpose()?
            .ok_or_else(|| StorageError::NotFound(format!("session {id}")))
    }

    pub async fn create_session(
        &self,
        name: &str,
        connection_id: Option<i64>,
        system_prompt_id: Option<i64>,
        project_id: Option<i64>,
        user_id: UserId,
        created_at: i64,
    ) -> Result<ChatSession, StorageError> {
        self.create_session_request(
            &NewSession {
                auto_title: false,
                name: name.into(),
                connection_id,
                system_prompt_id,
                project_id,
            },
            user_id,
            created_at,
        )
        .await
    }

    pub async fn create_session_request(
        &self,
        request: &NewSession,
        user_id: UserId,
        created_at: i64,
    ) -> Result<ChatSession, StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let session = store
                    .create_session_unlocked(
                        &request.name,
                        request.connection_id,
                        request.system_prompt_id,
                        request.project_id,
                        user_id,
                        created_at,
                    )
                    .await?;
                if request.auto_title {
                    store
                        .db
                        .execute(
                            "UPDATE sessions SET auto_title = 1 WHERE id = ? AND user_id = ?",
                            &[DbValue::Int(session.id), DbValue::Int(user_id.get())],
                        )
                        .await?;
                }
                store.get_session(session.id, user_id).await
            })
            .await
    }

    async fn create_session_unlocked(
        &self,
        name: &str,
        connection_id: Option<i64>,
        system_prompt_id: Option<i64>,
        project_id: Option<i64>,
        user_id: UserId,
        created_at: i64,
    ) -> Result<ChatSession, StorageError> {
        if let Some(project) = project_id {
            self.get_project(project, user_id).await?;
        }
        if let Some(prompt) = system_prompt_id {
            self.get_system_prompt(prompt, user_id).await?;
        }
        let res = self.db.execute(
                "INSERT INTO sessions (name, connection_id, system_prompt_id, project_id, user_id, created_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
                &[
                    DbValue::Text(name.into()),
                    connection_id.map(DbValue::Int).unwrap_or(DbValue::Null),
                    system_prompt_id.map(DbValue::Int).unwrap_or(DbValue::Null),
                    project_id.map(DbValue::Int).unwrap_or(DbValue::Null),
                    DbValue::Int(user_id.get()),
                    DbValue::Int(created_at),
                ],
            )
            .await?;
        self.db
            .execute(
                "INSERT INTO user_settings (user_id, key, value) VALUES (?, ?, ?)",
                &[
                    DbValue::Int(user_id.get()),
                    DbValue::Text(openwebide_core::ApprovalMode::setting_key(
                        res.last_insert_rowid,
                    )),
                    DbValue::Text(
                        serde_json::to_string(&openwebide_core::ApprovalMode::NEW_SESSION)
                            .expect("approval mode serializes"),
                    ),
                ],
            )
            .await?;
        self.get_session(res.last_insert_rowid, user_id).await
    }

    pub async fn set_session_connection(
        &self,
        id: i64,
        connection_id: i64,
        user_id: UserId,
    ) -> Result<ChatSession, StorageError> {
        self.get_session(id, user_id).await?;
        self.get_connection(connection_id).await?;
        self.db
            .execute(
                "UPDATE sessions SET connection_id = ? WHERE id = ? AND user_id = ?",
                &[
                    DbValue::Int(connection_id),
                    DbValue::Int(id),
                    DbValue::Int(user_id.get()),
                ],
            )
            .await?;
        self.get_session(id, user_id).await
    }

    pub async fn rename_session(
        &self,
        id: i64,
        name: &str,
        user_id: UserId,
    ) -> Result<ChatSession, StorageError> {
        let res = self
            .db
            .execute(
                "UPDATE sessions SET name = ?, auto_title = 0, title_revision = title_revision + 1 WHERE id = ? AND user_id = ?",
                &[
                    DbValue::Text(name.into()),
                    DbValue::Int(id),
                    DbValue::Int(user_id.get()),
                ],
            )
            .await?;
        if res.changes == 0 {
            return Err(StorageError::NotFound(format!("session {id}")));
        }
        self.get_session(id, user_id).await
    }

    pub async fn delete_session(&self, id: i64, user_id: UserId) -> Result<(), StorageError> {
        let res = self
            .db
            .execute(
                "DELETE FROM sessions WHERE id = ? AND user_id = ? AND NOT EXISTS (SELECT 1 FROM host_operations WHERE session_id=sessions.id AND state IN ('running','awaiting_reconnect'))",
                &[DbValue::Int(id), DbValue::Int(user_id.get())],
            )
            .await?;
        if res.changes == 0 {
            self.get_session(id, user_id).await?;
            return Err(StorageError::Conflict(
                "Host maintenance is still active; finish it before deleting this session.".into(),
            ));
        }
        Ok(())
    }

    // -- messages --------------------------------------------------------------

    pub async fn list_messages(&self, session_id: i64) -> Result<Vec<ChatMessage>, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT id, session_id, role, content, created_at,
                        prompt_tokens, completion_tokens, eval_duration_ms, usage_estimated, tool_calls, context_breakdown
                 FROM messages WHERE session_id = ? ORDER BY id",
                &[DbValue::Int(session_id)],
            )
            .await?;
        res.rows.iter().map(message_from_row).collect()
    }

    pub async fn insert_message(
        &self,
        session_id: i64,
        role: Role,
        content: &str,
        created_at: i64,
    ) -> Result<ChatMessage, StorageError> {
        self.insert_message_with_usage(session_id, role, content, created_at, None)
            .await
    }

    /// Insert a message, persisting the model call's usage alongside it (or
    /// NULLs when `usage` is `None`).
    pub async fn insert_message_with_usage(
        &self,
        session_id: i64,
        role: Role,
        content: &str,
        created_at: i64,
        usage: Option<&TurnTelemetry>,
    ) -> Result<ChatMessage, StorageError> {
        self.insert_interim_message(session_id, role, content, created_at, usage, None)
            .await
    }

    pub fn insert_interim_message<'a>(
        &'a self,
        session_id: i64,
        role: Role,
        content: &'a str,
        created_at: i64,
        usage: Option<&'a TurnTelemetry>,
        tool_calls: Option<&'a [ToolCall]>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ChatMessage, StorageError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.db
                .transaction(|tx| async move {
                    let store = Store::new(tx);
                    store.ensure_not_rewinding(session_id).await?;
                    store
                        .insert_interim_message_unlocked(
                            session_id, role, content, created_at, usage, tool_calls,
                        )
                        .await
                })
                .await
        })
    }

    async fn insert_interim_message_unlocked(
        &self,
        session_id: i64,
        role: Role,
        content: &str,
        created_at: i64,
        usage: Option<&TurnTelemetry>,
        tool_calls: Option<&[ToolCall]>,
    ) -> Result<ChatMessage, StorageError> {
        if role == Role::User {
            openwebide_core::PromptContent::attachments(content)
                .map_err(StorageError::InvalidValue)?;
        }
        let calls_json = tool_calls
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StorageError::InvalidValue(e.to_string()))?;
        let res = self
            .db
            .execute(
                "INSERT INTO messages
                     (session_id, role, content, created_at,
                      prompt_tokens, completion_tokens, eval_duration_ms, usage_estimated, tool_calls, context_breakdown)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                &[
                    DbValue::Int(session_id),
                    DbValue::Text(role.as_str().into()),
                    DbValue::Text(content.into()),
                    DbValue::Int(created_at),
                    usage
                        .map(|u| DbValue::Int(i64::try_from(u.prompt_tokens).unwrap_or(i64::MAX)))
                        .unwrap_or(DbValue::Null),
                    usage
                        .map(|u| DbValue::Int(i64::try_from(u.completion_tokens).unwrap_or(i64::MAX)))
                        .unwrap_or(DbValue::Null),
                    usage
                        .map(|u| DbValue::Int(i64::try_from(u.eval_duration_ms).unwrap_or(i64::MAX)))
                        .unwrap_or(DbValue::Null),
                    usage
                        .map(|u| DbValue::Int(i64::from(u.estimated)))
                        .unwrap_or(DbValue::Null),
                    calls_json.map(DbValue::Text).unwrap_or(DbValue::Null),
                    usage.and_then(|usage| usage.context).map(|context| serde_json::to_string(&context)).transpose().map_err(|error| StorageError::InvalidValue(error.to_string()))?.map(DbValue::Text).unwrap_or(DbValue::Null),
                ],
            )
            .await?;
        Ok(ChatMessage {
            id: res.last_insert_rowid,
            session_id,
            role,
            content: content.into(),
            created_at,
            tool_calls: tool_calls.map(<[ToolCall]>::to_vec),
            tool_call_id: None,
            usage: usage.copied(),
        })
    }

    // -- tool steps ------------------------------------------------------------
    //
    // Agent tool steps are persisted separately from chat messages so a
    // session's steps survive a tab switch without polluting the LLM context.
    // `anchor_message_id` is the user message that started the turn; a step
    // renders right after it.

    /// Record (or refresh) a tool step as it is requested. Called for both a
    /// `tool_call` and a `permission_request` (which share an id), so a gated
    /// write is stored once.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_tool_step<'a>(
        &'a self,
        session_id: i64,
        anchor_message_id: i64,
        tool_call_id: &'a str,
        name: &'a str,
        summary: &'a str,
        created_at: i64,
        diff: Option<&'a FileDiff>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StorageError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.db
                .transaction(|tx| async move {
                    let store = Store::new(tx);
                    store.ensure_not_rewinding(session_id).await?;
                    store
                        .upsert_tool_step_unlocked(
                            session_id,
                            anchor_message_id,
                            tool_call_id,
                            name,
                            summary,
                            created_at,
                            diff,
                        )
                        .await
                })
                .await
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn upsert_tool_step_unlocked(
        &self,
        session_id: i64,
        anchor_message_id: i64,
        tool_call_id: &str,
        name: &str,
        summary: &str,
        created_at: i64,
        diff: Option<&FileDiff>,
    ) -> Result<(), StorageError> {
        let diff_json = diff
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StorageError::Db(e.to_string()))?;
        self.db
            .execute(
                "INSERT INTO tool_steps
                     (session_id, anchor_message_id, tool_call_id, name, summary, created_at, diff)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (session_id, tool_call_id)
                 DO UPDATE SET name = excluded.name, summary = excluded.summary, diff = excluded.diff
                 WHERE tool_steps.completion_applied = 0",
                &[
                    DbValue::Int(session_id),
                    DbValue::Int(anchor_message_id),
                    DbValue::Text(tool_call_id.into()),
                    DbValue::Text(name.into()),
                    DbValue::Text(summary.into()),
                    DbValue::Int(created_at),
                    diff_json.map(DbValue::Text).unwrap_or(DbValue::Null),
                ],
            )
            .await?;
        Ok(())
    }

    /// Complete and apply a source exactly once, in the same transaction.
    /// Boxing keeps the transaction future usable in Send event streams.
    pub fn complete_tool_step<'a>(
        &'a self,
        user_id: UserId,
        session_id: i64,
        tool_call_id: &'a str,
        ok: bool,
        result_summary: &'a str,
        diff: Option<&'a FileDiff>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StorageError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.db
                .transaction(|tx| async move {
                    let store = Store::new(tx);
                    store.ensure_not_rewinding(session_id).await?;
                    let session = store.get_session(session_id, user_id).await?;
                    if let Some(project) = session.project_id {
                        store.get_project(project, user_id).await?;
                    }
                    let source = store.db.execute(
                        "SELECT completion_applied, checkpoint, anchor_message_id, id FROM tool_steps WHERE session_id = ? AND tool_call_id = ?",
                        &[DbValue::Int(session_id), DbValue::Text(tool_call_id.into())],
                    ).await?;
                    let row = source.rows.first().ok_or_else(|| StorageError::NotFound("tool step".into()))?;
                    if row.get_int(0)? != 0 {
                        return Ok(());
                    }
                    let diff_json = diff.map(serde_json::to_string).transpose()
                        .map_err(|e| StorageError::Db(e.to_string()))?;
                    store.db.execute(
                        "UPDATE tool_steps SET ok = ?, result_summary = ?, diff = ?, completion_applied = 1
                         WHERE session_id = ? AND tool_call_id = ?",
                        &[DbValue::Int(i64::from(ok)), DbValue::Text(result_summary.into()),
                          diff_json.map(DbValue::Text).unwrap_or(DbValue::Null),
                          DbValue::Int(session_id), DbValue::Text(tool_call_id.into())],
                    ).await?;
                    store.db.execute("UPDATE agent_questions SET reply=? WHERE session_id=? AND tool_call_id=? AND reply IS NULL", &[DbValue::Text(serde_json::to_string(&openwebide_core::questions::QuestionReply::Cancel).expect("reply serializes")),DbValue::Int(session_id),DbValue::Text(tool_call_id.into())]).await?;
                    if let Some(project_id) = session.project_id {
                        if let Some(checkpoint) = row.get_text_opt(1) {
                            let checkpoint: openwebide_core::rewind::ProjectCheckpoint = serde_json::from_str(checkpoint).map_err(|e| StorageError::Db(e.to_string()))?;
                            if checkpoint.after.is_some() {
                                store.record_run_changes(user_id, project_id, session_id, row.get_int(2)?, row.get_int(3)?, &checkpoint).await?;
                            }
                        } else if let (true, Some(diff)) = (ok, diff) {
                            store.merge_completed_edit(user_id, project_id, diff).await?;
                        }
                    }
                    Ok(())
                })
                .await
        })
    }

    async fn merge_completed_edit(
        &self,
        user_id: UserId,
        project_id: i64,
        diff: &FileDiff,
    ) -> Result<(), StorageError> {
        let previous = self.db.execute(
            "SELECT project_id, path, revision, decision, diff, file FROM pending_edits WHERE project_id = ? AND path = ? AND user_id = ?",
            &[DbValue::Int(project_id), DbValue::Text(diff.path.clone()), DbValue::Int(user_id.get())],
        ).await?;
        let previous = previous
            .rows
            .first()
            .map(persisted_edit_from_row)
            .transpose()?;
        let revision = previous.as_ref().map_or(1, |edit| edit.revision + 1);
        let mut merged = diff.clone();
        let mut decision = EditDecision::Pending;
        if let Some(previous) = previous.filter(|edit| edit.decision == EditDecision::Pending) {
            merged = previous.diff;
            merged.new.clone_from(&diff.new);
            if !merged.old_unavailable && merged.old.as_deref() == Some(merged.new.as_str()) {
                decision = EditDecision::Accepted;
            }
        }
        let json = serde_json::to_string(&merged).map_err(|e| StorageError::Db(e.to_string()))?;
        self.db.execute(
            "INSERT INTO pending_edits (user_id, project_id, path, diff, revision, decision) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (project_id, path) DO UPDATE SET diff = excluded.diff, revision = excluded.revision, decision = excluded.decision",
            &[DbValue::Int(user_id.get()), DbValue::Int(project_id), DbValue::Text(diff.path.clone()),
              DbValue::Text(json), DbValue::Int(revision), DbValue::Text(edit_decision_text(decision).into())],
        ).await?;
        Ok(())
    }

    pub async fn list_pending_edits(
        &self,
        user_id: UserId,
        project_id: i64,
    ) -> Result<Vec<PersistedEdit>, StorageError> {
        self.get_project(project_id, user_id).await?;
        let rows = self.db.execute(
            "SELECT project_id, path, revision, decision, diff, file FROM pending_edits WHERE project_id = ? AND user_id = ? AND decision = 'pending' ORDER BY path",
            &[DbValue::Int(project_id), DbValue::Int(user_id.get())],
        ).await?;
        rows.rows.iter().map(persisted_edit_from_row).collect()
    }

    pub async fn resolve_pending_edit(
        &self,
        user_id: UserId,
        project_id: i64,
        request: &ResolveEditRequest,
    ) -> Result<PersistedEdit, StorageError> {
        if request.decision == EditDecision::Pending || request.revision <= 0 {
            return Err(StorageError::InvalidValue("invalid edit resolution".into()));
        }
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            store.get_project(project_id, user_id).await?;
            let rows = store.db.execute(
                "SELECT project_id, path, revision, decision, diff, file FROM pending_edits WHERE project_id = ? AND path = ? AND user_id = ?",
                &[DbValue::Int(project_id), DbValue::Text(request.path.clone()), DbValue::Int(user_id.get())],
            ).await?;
            let mut edit = rows.rows.first().map(persisted_edit_from_row).transpose()?
                .ok_or_else(|| StorageError::NotFound("pending edit".into()))?;
            if edit.file.is_some() { return Err(StorageError::Conflict("Use the run changes review for this file".into())); }
            if edit.revision != request.revision || (edit.decision != EditDecision::Pending && edit.decision != request.decision) {
                return Err(StorageError::Conflict("edit revision or decision changed".into()));
            }
            store.db.execute(
                "UPDATE pending_edits SET decision = ? WHERE project_id = ? AND path = ? AND user_id = ? AND revision = ?",
                &[DbValue::Text(edit_decision_text(request.decision).into()), DbValue::Int(project_id),
                  DbValue::Text(request.path.clone()), DbValue::Int(user_id.get()), DbValue::Int(request.revision)],
            ).await?;
            edit.decision = request.decision;
            Ok(edit)
        }).await
    }

    /// The session's tool steps in the order they were recorded.
    pub async fn list_tool_steps(&self, session_id: i64) -> Result<Vec<ToolStep>, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT tool_call_id, name, summary, ok, result_summary, diff, anchor_message_id, checkpoint, timing
                 FROM tool_steps WHERE session_id = ? ORDER BY anchor_message_id, COALESCE(execution_order, id), id",
                &[DbValue::Int(session_id)],
            )
            .await?;
        res.rows.iter().map(tool_step_from_row).collect()
    }

    /// The session's conversation as a single ordered list of messages and
    /// tool steps: each message, then the steps anchored to it. This is what
    /// the message-list endpoint returns so a reloaded session shows its steps.
    pub async fn list_conversation(
        &self,
        session_id: i64,
    ) -> Result<Vec<ConversationEntry>, StorageError> {
        let messages = self.list_messages(session_id).await?;
        let steps = self.list_tool_steps(session_id).await?;
        let mut by_anchor: BTreeMap<i64, Vec<ToolStep>> = BTreeMap::new();
        for step in steps {
            by_anchor
                .entry(step.anchor_message_id)
                .or_default()
                .push(step);
        }
        let mut task_groups: BTreeMap<String, Vec<openwebide_core::TaskHistory>> = BTreeMap::new();
        for task in self.list_tasks(session_id).await? {
            task_groups
                .entry(task.snapshot.task.parent_tool_call_id.clone())
                .or_default()
                .push(task);
        }
        fn append_steps(
            out: &mut Vec<ConversationEntry>,
            groups: &mut BTreeMap<String, Vec<openwebide_core::TaskHistory>>,
            steps: Vec<ToolStep>,
        ) {
            for step in steps {
                let tasks = groups.remove(&step.tool_call_id).unwrap_or_default();
                out.push(ConversationEntry::ToolStep(step));
                out.extend(
                    tasks
                        .into_iter()
                        .map(|task| ConversationEntry::Task(Box::new(task))),
                );
            }
        }
        let mut out: Vec<ConversationEntry> = Vec::new();
        for message in messages {
            let anchor = message.id;
            out.push(ConversationEntry::Message(message));
            if let Some(steps) = by_anchor.remove(&anchor) {
                append_steps(&mut out, &mut task_groups, steps);
            }
        }
        // Steps whose anchor message is gone (shouldn't happen) go at the end.
        for steps in by_anchor.into_values() {
            append_steps(&mut out, &mut task_groups, steps);
        }
        out.extend(
            task_groups
                .into_values()
                .flatten()
                .map(|task| ConversationEntry::Task(Box::new(task))),
        );
        Ok(out)
    }

    // -- run cancellation ----------------------------------------------------

    /// Record when cancellation was requested; runs started later ignore it.
    pub async fn request_cancel(&self, session_id: i64, at_ms: i64) -> Result<(), StorageError> {
        self.db
            .execute(
                "INSERT INTO run_cancels (session_id, requested_at_ms) VALUES (?, ?) \
             ON CONFLICT(session_id) DO UPDATE SET requested_at_ms = excluded.requested_at_ms",
                &[DbValue::Int(session_id), DbValue::Int(at_ms)],
            )
            .await?;
        Ok(())
    }

    /// Whether cancellation was requested after this run started.
    pub async fn cancel_requested_since(
        &self,
        session_id: i64,
        started_ms: i64,
    ) -> Result<bool, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT 1 FROM run_cancels WHERE session_id = ? AND requested_at_ms > ?",
                &[DbValue::Int(session_id), DbValue::Int(started_ms)],
            )
            .await?;
        Ok(!res.rows.is_empty())
    }

    // -- tool permissions ----------------------------------------------------

    /// Record the user's decision on a gated tool call. The in-flight run
    /// polls [`Self::take_tool_permission`] until it arrives, consuming it;
    /// Spin requests are stateless, so the decision lives in the database.
    pub async fn set_tool_permission(
        &self,
        session_id: i64,
        tool_call_id: &str,
        approved: bool,
    ) -> Result<(), StorageError> {
        self.db
            .execute(
                "INSERT OR REPLACE INTO tool_permissions \
                 (session_id, tool_call_id, decision) VALUES (?, ?, ?)",
                &[
                    DbValue::Int(session_id),
                    DbValue::Text(tool_call_id.to_string()),
                    DbValue::Int(i64::from(approved)),
                ],
            )
            .await?;
        Ok(())
    }

    /// Take the user's decision on a gated tool call, if one has been
    /// recorded, deleting it so it can answer only one call. Two statements
    /// rather than `DELETE … RETURNING`: the session's single poller is the
    /// only reader.
    pub async fn take_tool_permission(
        &self,
        session_id: i64,
        tool_call_id: &str,
    ) -> Result<Option<bool>, StorageError> {
        let params = [
            DbValue::Int(session_id),
            DbValue::Text(tool_call_id.to_string()),
        ];
        let res = self
            .db
            .execute(
                "SELECT decision FROM tool_permissions \
                 WHERE session_id = ? AND tool_call_id = ?",
                &params,
            )
            .await?;
        let Some(row) = res.rows.first() else {
            return Ok(None);
        };
        let decision = row.get_int(0).map(|d| d != 0).unwrap_or(false);
        self.db
            .execute(
                "DELETE FROM tool_permissions WHERE session_id = ? AND tool_call_id = ?",
                &params,
            )
            .await?;
        Ok(Some(decision))
    }

    /// Clear recorded decisions belonging to this run's original anchor.
    pub async fn clear_tool_permissions_for_run(
        &self,
        session_id: i64,
        anchor_id: i64,
    ) -> Result<(), StorageError> {
        self.db
            .execute(
                "DELETE FROM tool_permissions WHERE session_id = ? AND tool_call_id LIKE ?",
                &[
                    DbValue::Int(session_id),
                    DbValue::Text(format!("{}%", openwebide_core::step_id_prefix(anchor_id))),
                ],
            )
            .await?;
        Ok(())
    }
    // -- auth security -----------------------------------------------------

    pub async fn login_failures(&self, username: &str) -> Result<Option<(i64, i64)>, StorageError> {
        let res = self
            .db
            .execute(
                "SELECT failures, last_failed_at FROM login_failures WHERE username = ?",
                &[DbValue::Text(username.into())],
            )
            .await?;
        if let Some(row) = res.rows.first() {
            Ok(Some((row.get_int(0)?, row.get_int(1)?)))
        } else {
            Ok(None)
        }
    }

    pub async fn record_login_failure(&self, username: &str, now: i64) -> Result<(), StorageError> {
        self.db
            .transaction(|tx| async move {
                tx.execute(
                    "DELETE FROM login_failures WHERE last_failed_at < ?",
                    &[DbValue::Int(now - 86400)],
                )
                .await?;

                tx.execute(
                    "INSERT INTO login_failures (username, failures, last_failed_at)
                 VALUES (?, 1, ?)
                 ON CONFLICT(username) DO UPDATE SET
                 failures = login_failures.failures + 1,
                 last_failed_at = excluded.last_failed_at",
                    &[DbValue::Text(username.into()), DbValue::Int(now)],
                )
                .await?;
                Ok(())
            })
            .await
    }

    pub async fn clear_login_failures(&self, username: &str) -> Result<(), StorageError> {
        self.db
            .execute(
                "DELETE FROM login_failures WHERE username = ?",
                &[DbValue::Text(username.into())],
            )
            .await?;
        Ok(())
    }

    pub async fn bump_token_epoch(&self, user_id: UserId) -> Result<(), StorageError> {
        self.db
            .execute(
                "UPDATE users SET token_epoch = token_epoch + 1 WHERE id = ?",
                &[DbValue::Int(user_id.get())],
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::QueryRow;
    use crate::rusqlite_db::RusqliteDb;
    use futures::executor::block_on;

    fn test_store() -> Store<RusqliteDb> {
        let db = RusqliteDb::open_in_memory().unwrap();
        let store = Store::new(db);
        block_on(store.migrate()).unwrap();
        store
    }

    /// Create a user and return its id, for tests that need scoped data.
    /// Call this *before* the test's `block_on` block (it runs its own
    /// executor, so it must not be nested inside one).
    fn test_user(store: &Store<RusqliteDb>, username: &str, role: UserRole) -> UserId {
        block_on(async {
            store
                .insert_user(username, "hash", role, 1)
                .await
                .unwrap()
                .id
        })
    }

    fn edit(old: Option<&str>, new: &str) -> FileDiff {
        FileDiff {
            path: "a.txt".into(),
            old: old.map(str::to_string),
            new: new.into(),
            old_unavailable: false,
            backup_path: None,
        }
    }

    async fn edit_project(store: &Store<RusqliteDb>, user: UserId) -> Project {
        store
            .create_project(
                &NewProject {
                    name: "p".into(),
                    mode: WorkspaceMode::Remote,
                    path: Some("p".into()),
                },
                user,
                1,
            )
            .await
            .unwrap()
    }

    async fn complete_edit(
        store: &Store<RusqliteDb>,
        user: UserId,
        session: i64,
        source: &str,
        diff: &FileDiff,
    ) {
        store
            .upsert_tool_step(session, 1, source, "write_file", "write", 1, Some(diff))
            .await
            .unwrap();
        store
            .complete_tool_step(user, session, source, true, "written", Some(diff))
            .await
            .unwrap();
    }

    #[test]
    fn persisted_compaction_keeps_original_messages_and_is_session_scoped() {
        let store = test_store();
        let user = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let session = store
                .create_session("s", None, None, None, user, 1)
                .await
                .unwrap();
            let other = store
                .create_session("other", None, None, None, user, 1)
                .await
                .unwrap();
            let original = store
                .insert_message(session.id, Role::User, "exact user task", 1)
                .await
                .unwrap();
            let compaction = openwebide_core::Compaction {
                summary: "prior work".into(),
                retained: vec![original.clone()],
                through_message_id: original.id,
            };
            store
                .insert_message(
                    session.id,
                    Role::System,
                    &compaction.stored_content().unwrap(),
                    2,
                )
                .await
                .unwrap();
            let messages = store.list_messages(session.id).await.unwrap();
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0], original);
            assert_eq!(
                openwebide_core::Compaction::parse(&messages[1].content).unwrap(),
                compaction
            );
            assert!(store.list_messages(other.id).await.unwrap().is_empty());
        });
    }

    #[test]
    fn model_profiles_are_shared_but_defaults_are_user_scoped_and_secrets_write_only() {
        use openwebide_core::{
            ModelDefaults, ModelProfile, ModelSelection, ModelSettings, ServerSettingsUpdate,
        };
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::Admin);
        let bob = test_user(&store, "bob", UserRole::User);
        block_on(async {
            let server = store
                .insert_connection(&openwebide_core::NewConnection {
                    name: "server".into(),
                    base_url: "http://localhost:11434".into(),
                    kind: openwebide_core::ProviderKind::Ollama,
                    model: None,
                    context_limit: None,
                })
                .await
                .unwrap();
            let selection = ModelSelection {
                server_id: server.id,
                model: "main".into(),
            };
            store
                .save_model_defaults(
                    alice,
                    &ModelDefaults {
                        primary: Some(selection.clone()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            store
                .save_model_profile(&ModelProfile {
                    selection,
                    settings: ModelSettings {
                        context_limit: Some(8192),
                        ..Default::default()
                    },
                })
                .await
                .unwrap();
            assert_eq!(
                store
                    .model_setup(alice)
                    .await
                    .unwrap()
                    .resolve(server.id, "main")
                    .context_limit,
                Some(8192)
            );
            assert_eq!(
                store
                    .model_setup(alice)
                    .await
                    .unwrap()
                    .resolve(server.id, "other")
                    .context_limit,
                None
            );
            let bob_setup = store.model_setup(bob).await.unwrap();
            assert_eq!(bob_setup.defaults, ModelDefaults::default());
            assert_eq!(
                bob_setup.resolve(server.id, "main").context_limit,
                Some(8192)
            );
            store
                .save_server_settings(
                    server.id,
                    &ServerSettingsUpdate {
                        api_key: Some("secret-value".into()),
                        headers: Some(std::collections::BTreeMap::from([(
                            "X-Proxy-Key".into(),
                            "secret-header".into(),
                        )])),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let browser =
                serde_json::to_string(&store.server_settings(server.id).await.unwrap()).unwrap();
            assert!(!browser.contains("secret-value"));
            assert!(!browser.contains("secret-header"));
            store
                .save_server_settings(
                    server.id,
                    &ServerSettingsUpdate {
                        timeout_seconds: Some(60),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                store
                    .server_transport(server.id)
                    .await
                    .unwrap()
                    .api_key
                    .as_deref(),
                Some("secret-value")
            );
            store
                .save_server_settings(
                    server.id,
                    &ServerSettingsUpdate {
                        clear_api_key: true,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert!(!store.server_settings(server.id).await.unwrap().has_api_key);
            assert!(
                store
                    .save_server_settings(
                        server.id,
                        &ServerSettingsUpdate {
                            headers: Some(std::collections::BTreeMap::from([(
                                "Host".into(),
                                "evil".into()
                            )])),
                            ..Default::default()
                        }
                    )
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn shared_profile_migration_preserves_preferences_thresholds_and_is_idempotent() {
        block_on(async {
            let db = RusqliteDb::open_in_memory().unwrap();
            migrations::apply_through(&db, 19).await.unwrap();
            let store = Store::new(db);
            let alice = store
                .insert_user("alice", "hash", UserRole::Admin, 1)
                .await
                .unwrap()
                .id;
            let bob = store
                .insert_user("bob", "hash", UserRole::User, 1)
                .await
                .unwrap()
                .id;
            // Populate the historical schema without using today's row decoder.
            let server = store.db.execute("INSERT INTO connections (name, kind, base_url, model) VALUES ('host', 'ollama', 'http://host', 'main')", &[]).await.unwrap().last_insert_rowid;
            for (user, model, threshold) in [(alice, "main", 80), (bob, "other", 0)] {
                store.set_user_setting(user, "model_defaults", &serde_json::json!({"primary":{"server_id":server,"model":model},"auto_compact_threshold":threshold}).to_string()).await.unwrap();
            }
            for (user, context, fast) in [
                (
                    alice,
                    4096,
                    serde_json::json!({"server_id":server,"model":"quick"}),
                ),
                (bob, 8192, serde_json::Value::Null),
            ] {
                store.db.execute("INSERT INTO model_settings (user_id, server_id, model, settings) VALUES (?, ?, 'main', ?)", &[DbValue::Int(user.get()),DbValue::Int(server),DbValue::Text(serde_json::json!({"context_limit":context,"fast":fast}).to_string())]).await.unwrap();
            }
            store
                .set_user_setting(alice, &format!("model_detection_{server}_main"), "cached")
                .await
                .unwrap();
            store
                .db
                .execute("PRAGMA user_version = 19", &[])
                .await
                .unwrap();
            store.migrate_with(&|_| true).await.unwrap();
            let first = store.model_setup(alice).await.unwrap();
            let second = store.model_setup(bob).await.unwrap();
            assert_eq!(first.profiles, second.profiles);
            assert_eq!(first.resolve(server, "main").context_limit, Some(4096));
            assert_eq!(
                first.resolve(server, "main").auto_compact_threshold,
                Some(80)
            );
            assert_eq!(
                second.resolve(server, "other").auto_compact_threshold,
                Some(0)
            );
            assert_eq!(first.defaults.fast.as_ref().unwrap().model, "quick");
            assert!(second.defaults.fast.is_none());
            assert_eq!(second.defaults.primary.as_ref().unwrap().model, "other");
            assert!(
                first
                    .profiles
                    .iter()
                    .all(|profile| profile.settings.fast.is_none())
            );
            assert_eq!(
                store
                    .get_setting(&format!("model_detection_{server}_main"))
                    .await
                    .unwrap()
                    .as_deref(),
                Some("cached")
            );
            migrations::apply_through(&store.db, 20).await.unwrap();
            assert_eq!(store.model_setup(alice).await.unwrap(), first);
            assert_eq!(store.model_setup(bob).await.unwrap(), second);
        });
    }

    #[test]
    fn pending_edits_merge_once_across_sessions_and_keep_resolutions() {
        let store = test_store();
        let user = test_user(&store, "u", UserRole::User);
        block_on(async {
            let project = edit_project(&store, user).await;
            let first = store
                .create_session("s1", None, None, Some(project.id), user, 1)
                .await
                .unwrap();
            let second = store
                .create_session("s2", None, None, Some(project.id), user, 1)
                .await
                .unwrap();
            let original = edit(Some("a"), "b");
            complete_edit(&store, user, first.id, "one", &original).await;
            complete_edit(&store, user, second.id, "two", &edit(Some("b"), "c")).await;
            let edits = store.list_pending_edits(user, project.id).await.unwrap();
            assert_eq!(edits.len(), 1);
            assert_eq!(edits[0].diff, edit(Some("a"), "c"));
            assert_eq!(edits[0].revision, 2);
            store
                .complete_tool_step(user, first.id, "one", true, "replay", Some(&original))
                .await
                .unwrap();
            assert_eq!(
                store.list_pending_edits(user, project.id).await.unwrap(),
                edits
            );
            let mut request = ResolveEditRequest {
                path: "a.txt".into(),
                revision: 1,
                decision: EditDecision::Accepted,
            };
            assert!(matches!(
                store.resolve_pending_edit(user, project.id, &request).await,
                Err(StorageError::Conflict(_))
            ));
            request.revision = 2;
            let resolved = store
                .resolve_pending_edit(user, project.id, &request)
                .await
                .unwrap();
            assert_eq!(
                store
                    .resolve_pending_edit(user, project.id, &request)
                    .await
                    .unwrap(),
                resolved
            );
            assert!(
                store
                    .list_pending_edits(user, project.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            // Neither completion nor a late preview can resurrect a resolved source.
            complete_edit(&store, user, first.id, "one", &original).await;
            assert!(
                store
                    .list_pending_edits(user, project.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                store.list_tool_steps(first.id).await.unwrap()[0].diff,
                Some(original)
            );
            complete_edit(&store, user, first.id, "three", &edit(Some("c"), "d")).await;
            let fresh = store.list_pending_edits(user, project.id).await.unwrap();
            assert_eq!(fresh[0].revision, 3);
            assert_eq!(fresh[0].diff, edit(Some("c"), "d"));
            assert!(matches!(
                store.resolve_pending_edit(user, project.id, &request).await,
                Err(StorageError::Conflict(_))
            ));
            request.revision = 3;
            request.decision = EditDecision::Rejected;
            store
                .resolve_pending_edit(user, project.id, &request)
                .await
                .unwrap();
            complete_edit(&store, user, second.id, "four", &edit(Some("c"), "e")).await;
            let fresh = store.list_pending_edits(user, project.id).await.unwrap();
            assert_eq!(fresh[0].revision, 4);
            assert_eq!(fresh[0].diff, edit(Some("c"), "e"));
            complete_edit(&store, user, first.id, "five", &edit(Some("e"), "c")).await;
            assert!(
                store
                    .list_pending_edits(user, project.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            complete_edit(&store, user, first.id, "six", &edit(None, "new")).await;
            assert_eq!(
                store.list_pending_edits(user, project.id).await.unwrap()[0].revision,
                6
            );
        });
    }

    #[test]
    fn pending_completion_is_atomic_and_requires_owned_existing_source() {
        let store = test_store();
        let user = test_user(&store, "u", UserRole::User);
        let other = test_user(&store, "other", UserRole::User);
        block_on(async {
            let project = edit_project(&store, user).await;
            let session = store
                .create_session("s", None, None, Some(project.id), user, 1)
                .await
                .unwrap();
            let diff = edit(Some("a"), "b");
            assert!(matches!(
                store
                    .complete_tool_step(user, session.id, "missing", true, "written", Some(&diff))
                    .await,
                Err(StorageError::NotFound(_))
            ));
            store
                .upsert_tool_step(
                    session.id,
                    1,
                    "source",
                    "write_file",
                    "preview",
                    1,
                    Some(&diff),
                )
                .await
                .unwrap();
            assert!(
                store
                    .list_pending_edits(user, project.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                store
                    .complete_tool_step(other, session.id, "source", true, "written", Some(&diff))
                    .await
                    .is_err()
            );
            assert!(store.list_pending_edits(other, project.id).await.is_err());
            let request = ResolveEditRequest {
                path: diff.path.clone(),
                revision: 1,
                decision: EditDecision::Rejected,
            };
            assert!(
                store
                    .resolve_pending_edit(other, project.id, &request)
                    .await
                    .is_err()
            );
            store.db.execute("CREATE TRIGGER fail_pending BEFORE INSERT ON pending_edits BEGIN SELECT RAISE(ABORT, 'test failure'); END", &[]).await.unwrap();
            assert!(
                store
                    .complete_tool_step(user, session.id, "source", true, "written", Some(&diff))
                    .await
                    .is_err()
            );
            assert_eq!(store.list_tool_steps(session.id).await.unwrap()[0].ok, None);
            store
                .db
                .execute("DROP TRIGGER fail_pending", &[])
                .await
                .unwrap();
            store
                .complete_tool_step(user, session.id, "source", false, "failed", Some(&diff))
                .await
                .unwrap();
            assert!(
                store
                    .list_pending_edits(user, project.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            let mut unavailable = edit(None, "binary changed");
            unavailable.old_unavailable = true;
            unavailable.backup_path = Some("backup".into());
            complete_edit(&store, user, session.id, "unreadable", &unavailable).await;
            complete_edit(
                &store,
                user,
                session.id,
                "later",
                &edit(Some("binary changed"), "last"),
            )
            .await;
            let merged = store.list_pending_edits(user, project.id).await.unwrap();
            assert!(merged[0].diff.old_unavailable);
            assert_eq!(merged[0].diff.backup_path.as_deref(), Some("backup"));
            assert_eq!(merged[0].diff.old, None);
            assert_eq!(merged[0].diff.new, "last");
            store.delete_session(session.id, user).await.unwrap();
            assert_eq!(
                store.list_pending_edits(user, project.id).await.unwrap(),
                merged
            );
            store.delete_project(project.id, user).await.unwrap();
            assert!(
                store
                    .db
                    .execute("SELECT 1 FROM pending_edits", &[])
                    .await
                    .unwrap()
                    .rows
                    .is_empty()
            );
        });
    }

    #[test]
    fn upgrade_17_keeps_history_and_step_18_replays_without_backfill() {
        block_on(async {
            let db = RusqliteDb::open_in_memory().unwrap();
            migrations::apply_through(&db, 17).await.unwrap();
            db.execute("PRAGMA user_version = 17", &[]).await.unwrap();
            let store = Store::new(db);
            let user = store
                .insert_user("u", "hash", UserRole::User, 1)
                .await
                .unwrap()
                .id;
            // Seed projects using the historical schema, before plugin defaults existed.
            let project_id = store.db.execute("INSERT INTO projects(name,mode,path,user_id,created_at) VALUES('p','remote','p',?,1)", &[DbValue::Int(user.get())]).await.unwrap().last_insert_rowid;
            let project = store.get_project(project_id, user).await.unwrap();
            // Seed with the old schema rather than today's session decoder.
            let session_id = store.db.execute("INSERT INTO sessions (name, project_id, user_id, created_at) VALUES ('s', ?, ?, 1)", &[DbValue::Int(project.id), DbValue::Int(user.get())]).await.unwrap().last_insert_rowid;
            let diff = edit(Some("a"), "b");
            // Old builds stored completed history and permission previews in the same diff column.
            store.db.execute("INSERT INTO tool_steps (session_id, anchor_message_id, tool_call_id, name, summary, ok, diff, created_at) VALUES (?, 1, 'history', 'write_file', 'write', 1, ?, 1)", &[DbValue::Int(session_id), DbValue::Text(serde_json::to_string(&diff).unwrap())]).await.unwrap();
            store.db.execute("INSERT INTO tool_steps (session_id, anchor_message_id, tool_call_id, name, summary, diff, created_at) VALUES (?, 1, 'preview', 'write_file', 'write', ?, 1)", &[DbValue::Int(session_id), DbValue::Text(serde_json::to_string(&diff).unwrap())]).await.unwrap();
            store.migrate().await.unwrap();
            assert_eq!(schema_version(&store.db).await, migrations::SCHEMA_VERSION);
            store
                .complete_tool_step(user, session_id, "history", true, "replay", Some(&diff))
                .await
                .unwrap();
            assert!(
                store
                    .list_pending_edits(user, project.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            store
                .complete_tool_step(user, session_id, "preview", true, "written", Some(&diff))
                .await
                .unwrap();
            let pending = store.list_pending_edits(user, project.id).await.unwrap();
            migrations::apply_through(&store.db, 18).await.unwrap();
            migrations::apply_through(&store.db, 18).await.unwrap();
            assert_eq!(
                store.list_pending_edits(user, project.id).await.unwrap(),
                pending
            );
            let request = ResolveEditRequest {
                path: "a.txt".into(),
                revision: 1,
                decision: EditDecision::Pending,
            };
            assert!(matches!(
                store.resolve_pending_edit(user, project.id, &request).await,
                Err(StorageError::InvalidValue(_))
            ));
        });
    }

    #[test]
    fn interim_calls_round_trip_and_migrations_replay() {
        block_on(async {
            let db = RusqliteDb::open_in_memory().unwrap();
            migrations::apply_through(&db, 13).await.unwrap();
            migrations::apply_through(&db, 16).await.unwrap();
            migrations::apply_through(&db, 16).await.unwrap();
            let store = Store::new(db);
            store.migrate().await.unwrap();
            let user = store
                .insert_user("u", "hash", UserRole::Admin, 1)
                .await
                .unwrap();
            let session = store
                .create_session("s", None, None, None, user.id, 1)
                .await
                .unwrap();
            let plain = store
                .insert_message(session.id, Role::Assistant, "plain", 1)
                .await
                .unwrap();
            let calls = vec![ToolCall {
                id: "wire-id".into(),
                name: "read_file".into(),
                arguments: "{\"path\":\"a\"}".into(),
            }];
            let interim = store
                .insert_interim_message(session.id, Role::Assistant, "", 2, None, Some(&calls))
                .await
                .unwrap();
            assert_eq!(interim.tool_calls.as_ref(), Some(&calls));
            assert_eq!(
                store.list_messages(session.id).await.unwrap(),
                vec![plain, interim]
            );
            assert!(
                store.list_messages(session.id).await.unwrap()[0]
                    .tool_calls
                    .is_none()
            );
        });
    }

    #[test]
    fn streamed_tools_flag_resets_only_for_server_or_kind_changes() {
        let store = test_store();
        block_on(async {
            let mut connection = store
                .insert_connection(&NewConnection {
                    name: "server".into(),
                    kind: ProviderKind::LlamaCpp,
                    base_url: "http://server".into(),
                    model: None,
                    context_limit: None,
                })
                .await
                .unwrap();
            assert!(!connection.tool_stream_unsupported);
            store
                .set_tool_stream_unsupported(connection.id, connection.tool_stream_revision)
                .await
                .unwrap();
            connection.name = "renamed".into();
            store.update_connection(&connection).await.unwrap();
            assert!(
                store
                    .get_connection(connection.id)
                    .await
                    .unwrap()
                    .tool_stream_unsupported
            );
            assert!(store.list_connections().await.unwrap()[0].tool_stream_unsupported);
            connection.base_url = "http://new-server".into();
            store.update_connection(&connection).await.unwrap();
            assert!(
                !store
                    .get_connection(connection.id)
                    .await
                    .unwrap()
                    .tool_stream_unsupported
            );
            connection = store.get_connection(connection.id).await.unwrap();
            store
                .set_tool_stream_unsupported(connection.id, connection.tool_stream_revision)
                .await
                .unwrap();
            connection.kind = ProviderKind::Ollama;
            store.update_connection(&connection).await.unwrap();
            assert!(
                !store
                    .get_connection(connection.id)
                    .await
                    .unwrap()
                    .tool_stream_unsupported
            );
        });
    }

    #[test]
    fn stale_stream_detection_is_ignored_after_connection_resets() {
        let store = test_store();
        block_on(async {
            let original = store
                .insert_connection(&NewConnection {
                    name: "server".into(),
                    kind: ProviderKind::LlamaCpp,
                    base_url: "http://server-a".into(),
                    model: None,
                    context_limit: None,
                })
                .await
                .unwrap();
            let mut connection = original.clone();
            for change_kind in [false, true] {
                let before = store.get_connection(connection.id).await.unwrap();
                if change_kind {
                    connection.kind = ProviderKind::Ollama;
                } else {
                    connection.base_url = "http://server-b".into();
                }
                store.update_connection(&connection).await.unwrap();
                store
                    .set_tool_stream_unsupported(before.id, before.tool_stream_revision)
                    .await
                    .unwrap();
                let replacement = store.get_connection(connection.id).await.unwrap();
                assert!(!replacement.tool_stream_unsupported);
                assert_eq!(
                    replacement.tool_stream_revision,
                    before.tool_stream_revision + 1
                );
                connection.base_url = original.base_url.clone();
                connection.kind = original.kind;
                store.update_connection(&connection).await.unwrap();
                store
                    .set_tool_stream_unsupported(before.id, before.tool_stream_revision)
                    .await
                    .unwrap();
                let restored = store.get_connection(connection.id).await.unwrap();
                assert!(!restored.tool_stream_unsupported);
                assert_eq!(
                    restored.tool_stream_revision,
                    before.tool_stream_revision + 2
                );
                store
                    .set_tool_stream_unsupported(restored.id, restored.tool_stream_revision)
                    .await
                    .unwrap();
                connection.name = "renamed".into();
                connection.tool_stream_revision = 0;
                store.update_connection(&connection).await.unwrap();
                let renamed = store.get_connection(connection.id).await.unwrap();
                assert!(renamed.tool_stream_unsupported);
                assert_eq!(renamed.tool_stream_revision, restored.tool_stream_revision);
            }
        });
    }

    #[test]
    fn settings_roundtrip() {
        let store = test_store();
        block_on(async {
            store.set_setting("theme", "dark").await.unwrap();
            assert_eq!(
                store.get_setting("theme").await.unwrap().as_deref(),
                Some("dark")
            );
            store.set_setting("theme", "light").await.unwrap();
            assert_eq!(
                store.get_setting("theme").await.unwrap().as_deref(),
                Some("light")
            );
            assert_eq!(store.get_setting("missing").await.unwrap(), None);
            let all = store.all_settings().await.unwrap();
            assert_eq!(all.len(), 1);
            assert_eq!(all.get("theme").map(String::as_str), Some("light"));
        });
    }

    #[test]
    fn insert_setting_if_absent_does_not_overwrite() {
        let store = test_store();
        block_on(async {
            store.set_setting("auth_secret", "first").await.unwrap();
            assert!(
                !store
                    .insert_setting_if_absent("auth_secret", "second")
                    .await
                    .unwrap()
            );
            assert_eq!(
                store.get_setting("auth_secret").await.unwrap().as_deref(),
                Some("first")
            );

            assert!(
                store
                    .insert_setting_if_absent("new_key", "value")
                    .await
                    .unwrap()
            );
            assert_eq!(
                store.get_setting("new_key").await.unwrap().as_deref(),
                Some("value")
            );
        });
    }

    #[test]
    fn user_settings_roundtrip_and_scoping() {
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::Admin);
        let bob = test_user(&store, "bob", UserRole::User);
        block_on(async {
            store
                .set_user_setting(alice, "open_tabs", "[1, 2]")
                .await
                .unwrap();
            store
                .set_user_setting(alice, "active_project", "2")
                .await
                .unwrap();
            store
                .set_user_setting(bob, "open_tabs", "[3]")
                .await
                .unwrap();

            assert_eq!(
                store
                    .get_user_setting(alice, "open_tabs")
                    .await
                    .unwrap()
                    .as_deref(),
                Some("[1, 2]")
            );
            assert_eq!(
                store
                    .get_user_setting(alice, "active_project")
                    .await
                    .unwrap()
                    .as_deref(),
                Some("2")
            );
            assert_eq!(
                store
                    .get_user_setting(bob, "open_tabs")
                    .await
                    .unwrap()
                    .as_deref(),
                Some("[3]")
            );
            assert_eq!(
                store.get_user_setting(bob, "active_project").await.unwrap(),
                None
            );

            let alice_all = store.all_user_settings(alice).await.unwrap();
            assert_eq!(alice_all.len(), 2);
            assert_eq!(
                alice_all.get("open_tabs").map(String::as_str),
                Some("[1, 2]")
            );
        });
    }

    #[test]
    fn setup_save_is_atomic_when_transport_validation_fails() {
        let store = test_store();
        let user = test_user(&store, "setup", UserRole::User);
        block_on(async {
            let probe = openwebide_core::ModelProbe {
                server_id: None,
                kind: ProviderKind::Ollama,
                base_url: "http://server".into(),
                transport: openwebide_core::ServerSettingsUpdate {
                    api_key: Some("secret".into()),
                    timeout_seconds: Some(0),
                    ..Default::default()
                },
                model: None,
            };
            let profiles = vec![openwebide_core::ModelProfile {
                selection: openwebide_core::ModelSelection {
                    server_id: 0,
                    model: "main".into(),
                },
                settings: Default::default(),
            }];
            assert!(
                store
                    .save_model_setup(user, &probe, &profiles)
                    .await
                    .is_err()
            );
            assert!(store.list_connections().await.unwrap().is_empty());
            assert!(store.model_setup(user).await.unwrap().profiles.is_empty());
            let mut probe = probe;
            probe.transport.timeout_seconds = Some(300);
            let (server, setup) = store
                .save_model_setup(user, &probe, &profiles)
                .await
                .unwrap();
            assert_eq!(setup.defaults.primary.unwrap().server_id, server.id);
            assert!(store.server_settings(server.id).await.unwrap().has_api_key);
        });
    }

    #[test]
    fn discovery_cache_and_stream_capabilities_are_scoped_and_reject_stale_servers() {
        let store = test_store();
        block_on(async {
            let connection = store
                .insert_connection(&NewConnection {
                    name: "server".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://old".into(),
                    model: None,
                    context_limit: None,
                })
                .await
                .unwrap();
            let detection = openwebide_core::ModelDetection {
                context_limit: Some(8192),
                ..Default::default()
            };
            store
                .save_model_detection(&connection, "one", &detection)
                .await
                .unwrap();
            store
                .set_model_tool_stream_unsupported(
                    connection.id,
                    "one",
                    connection.tool_stream_revision,
                )
                .await
                .unwrap();
            assert!(
                store
                    .model_tool_stream_unsupported(&connection, "one")
                    .await
                    .unwrap()
            );
            assert!(
                !store
                    .model_tool_stream_unsupported(&connection, "two")
                    .await
                    .unwrap()
            );
            assert_eq!(
                store.model_detection(&connection, "one").await.unwrap(),
                Some(detection.clone())
            );
            assert_eq!(
                store.model_detection(&connection, "two").await.unwrap(),
                None
            );
            store
                .save_server_settings(
                    connection.id,
                    &openwebide_core::ServerSettingsUpdate {
                        preset: Some(openwebide_core::ServerPreset::LiteLlm),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let updated = store.get_connection(connection.id).await.unwrap();
            assert_eq!(store.model_detection(&updated, "one").await.unwrap(), None);
            assert!(
                !store
                    .model_tool_stream_unsupported(&updated, "one")
                    .await
                    .unwrap()
            );
            store
                .save_model_detection(&connection, "one", &detection)
                .await
                .unwrap();
            store
                .set_model_tool_stream_unsupported(
                    connection.id,
                    "one",
                    connection.tool_stream_revision,
                )
                .await
                .unwrap();
            assert_eq!(store.model_detection(&updated, "one").await.unwrap(), None);
            assert!(
                !store
                    .model_tool_stream_unsupported(&updated, "one")
                    .await
                    .unwrap()
            );
        });
    }

    #[test]
    fn connection_tool_selection_is_shared_persisted_and_validated() {
        let store = test_store();
        block_on(async {
            let mut connection = store
                .insert_connection(&NewConnection {
                    name: "test".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://localhost".into(),
                    model: None,
                    context_limit: Some(4096),
                })
                .await
                .unwrap();
            assert_eq!(
                connection.tool_selection,
                openwebide_core::ToolSelection::All
            );
            for selection in [
                openwebide_core::ToolSelection::Selected(vec!["read_file".into()]),
                openwebide_core::ToolSelection::ChatOnly,
                openwebide_core::ToolSelection::All,
            ] {
                store
                    .save_server_settings(
                        connection.id,
                        &openwebide_core::ServerSettingsUpdate {
                            tool_selection: Some(selection.clone()),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    store
                        .get_connection(connection.id)
                        .await
                        .unwrap()
                        .tool_selection,
                    selection
                );
                assert_eq!(
                    store.list_connections().await.unwrap()[0].tool_selection,
                    selection
                );
                assert_eq!(
                    store
                        .server_settings(connection.id)
                        .await
                        .unwrap()
                        .tool_selection,
                    selection
                );
                store
                    .save_server_settings(connection.id, &Default::default())
                    .await
                    .unwrap();
                assert_eq!(
                    store
                        .get_connection(connection.id)
                        .await
                        .unwrap()
                        .tool_selection,
                    selection
                );
            }
            connection.tool_selection =
                openwebide_core::ToolSelection::Selected(vec!["read_file".into()]);
            store.update_connection(&connection).await.unwrap();
            connection.tool_selection =
                openwebide_core::ToolSelection::Selected(vec!["bad name".into()]);
            assert!(store.update_connection(&connection).await.is_err());
            assert!(
                store
                    .save_server_settings(
                        connection.id,
                        &openwebide_core::ServerSettingsUpdate {
                            tool_selection: Some(connection.tool_selection),
                            ..Default::default()
                        }
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .get_connection(connection.id)
                    .await
                    .unwrap()
                    .tool_selection,
                openwebide_core::ToolSelection::Selected(vec!["read_file".into()])
            );
            store.migrate().await.unwrap();
        });
    }

    #[test]
    fn connection_crud() {
        let store = test_store();
        block_on(async {
            let conn = store
                .insert_connection(&NewConnection {
                    name: "local-ollama".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://localhost:11434".into(),
                    model: Some("qwen2.5-coder:7b".into()),
                    context_limit: Some(8192),
                })
                .await
                .unwrap();
            assert!(conn.id > 0);
            assert!(conn.enabled);
            assert_eq!(conn.model.as_deref(), Some("qwen2.5-coder:7b"));
            assert_eq!(conn.context_limit, Some(8192));

            let mut conn = store.get_connection(conn.id).await.unwrap();
            assert_eq!(conn.context_limit, Some(8192));
            conn.name = "renamed".into();
            conn.enabled = false;
            conn.context_limit = Some(32_768);
            store.update_connection(&conn).await.unwrap();
            let reloaded = store.get_connection(conn.id).await.unwrap();
            assert_eq!(reloaded.name, "renamed");
            assert!(!reloaded.enabled);
            assert_eq!(reloaded.context_limit, Some(32_768));

            assert_eq!(store.list_connections().await.unwrap().len(), 1);
            assert_eq!(
                store.list_connections().await.unwrap()[0].context_limit,
                Some(32_768)
            );

            // Clearing the field writes NULL, which round-trips as `None`.
            let mut cleared = reloaded;
            cleared.context_limit = None;
            store.update_connection(&cleared).await.unwrap();
            assert_eq!(
                store
                    .get_connection(cleared.id)
                    .await
                    .unwrap()
                    .context_limit,
                None
            );

            store.delete_connection(cleared.id).await.unwrap();
            assert!(store.get_connection(cleared.id).await.is_err());
            assert!(store.list_connections().await.unwrap().is_empty());
        });
    }

    #[test]
    fn system_prompts_are_owned_and_session_references_cannot_cross_accounts() {
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::Admin);
        let bob = test_user(&store, "bob", UserRole::User);
        block_on(async {
            let a = store
                .insert_system_prompt(alice, "coder", "Alice's instructions")
                .await
                .unwrap();
            let b = store
                .insert_system_prompt(bob, "coder", "Bob's instructions")
                .await
                .unwrap();
            assert_ne!(a.id, b.id);
            assert_eq!(
                store.list_system_prompts(alice).await.unwrap(),
                vec![a.clone()]
            );
            assert_eq!(
                store.list_system_prompts(bob).await.unwrap(),
                vec![b.clone()]
            );
            assert!(matches!(
                store.get_system_prompt(a.id, bob).await,
                Err(StorageError::NotFound(_))
            ));
            assert!(matches!(
                store
                    .update_system_prompt(a.id, bob, "changed", "wrong")
                    .await,
                Err(StorageError::NotFound(_))
            ));
            assert!(matches!(
                store.delete_system_prompt(a.id, bob).await,
                Err(StorageError::NotFound(_))
            ));
            assert!(matches!(
                store
                    .create_session("wrong", None, Some(a.id), None, bob, 1)
                    .await,
                Err(StorageError::NotFound(_))
            ));
            for owner in [alice, bob] {
                let prompt = if owner == alice { &a } else { &b };
                let session = store
                    .create_session("chat", None, Some(prompt.id), None, owner, 1)
                    .await
                    .unwrap();
                store
                    .set_user_setting(owner, "default_prompt", &prompt.id.to_string())
                    .await
                    .unwrap();
                if owner == alice {
                    store.delete_system_prompt(a.id, alice).await.unwrap();
                    assert_eq!(
                        store
                            .get_session(session.id, alice)
                            .await
                            .unwrap()
                            .system_prompt_id,
                        None
                    );
                    assert_eq!(
                        store
                            .get_user_setting(alice, "default_prompt")
                            .await
                            .unwrap(),
                        None
                    );
                }
            }
            assert_eq!(
                store.get_system_prompt(b.id, bob).await.unwrap().content,
                "Bob's instructions"
            );
        });
    }

    #[test]
    fn system_prompt_migration_preserves_libraries_sessions_defaults_and_replays() {
        block_on(async {
            let db = RusqliteDb::open_in_memory().unwrap();
            migrations::apply_through(&db, 32).await.unwrap();
            let store = Store::new(db);
            let alice = store
                .insert_user("alice", "hash", UserRole::Admin, 1)
                .await
                .unwrap()
                .id;
            let bob = store
                .insert_user("bob", "hash", UserRole::User, 1)
                .await
                .unwrap()
                .id;
            let prompt = store.db.execute("INSERT INTO system_prompts (name, content) VALUES ('coder', 'Legacy instructions')", &[]).await.unwrap().last_insert_rowid;
            for owner in [alice, bob] {
                store.db.execute("INSERT INTO sessions (name, user_id, system_prompt_id, created_at) VALUES ('chat', ?, ?, 1)", &[DbValue::Int(owner.get()), DbValue::Int(prompt)]).await.unwrap();
                store
                    .set_user_setting(owner, "default_prompt", &prompt.to_string())
                    .await
                    .unwrap();
            }
            store.migrate().await.unwrap();
            for owner in [alice, bob] {
                let prompts = store.list_system_prompts(owner).await.unwrap();
                assert_eq!(prompts.len(), 1);
                assert_eq!(prompts[0].content, "Legacy instructions");
                let id = prompts[0].id;
                if owner == alice {
                    assert_eq!(id, prompt);
                }
                assert_eq!(
                    store.list_sessions(owner).await.unwrap()[0].system_prompt_id,
                    Some(id)
                );
                assert_eq!(
                    store
                        .get_user_setting(owner, "default_prompt")
                        .await
                        .unwrap(),
                    Some(id.to_string())
                );
            }
            let before = store.list_system_prompts(bob).await.unwrap();
            migrations::apply_through(&store.db, 33).await.unwrap();
            assert_eq!(store.list_system_prompts(bob).await.unwrap(), before);
            assert!(
                store
                    .db
                    .execute("PRAGMA foreign_key_check", &[])
                    .await
                    .unwrap()
                    .rows
                    .is_empty()
            );
        });
    }

    #[test]
    fn first_account_inherits_pre_auth_system_prompts_and_chat_references() {
        block_on(async {
            let db = RusqliteDb::open_in_memory().unwrap();
            migrations::apply_through(&db, 32).await.unwrap();
            db.execute(
                "INSERT INTO system_prompts (id, name, content) VALUES (42, 'legacy', 'Keep me')",
                &[],
            )
            .await
            .unwrap();
            db.execute("INSERT INTO sessions (id, name, system_prompt_id, created_at) VALUES (5, 'legacy chat', 42, 1)", &[]).await.unwrap();
            let store = Store::new(db);
            store.migrate().await.unwrap();
            let user = store
                .insert_user("first", "hash", UserRole::Admin, 1)
                .await
                .unwrap()
                .id;
            store.reassign_orphaned_system_prompts(user).await.unwrap();
            store.reassign_orphaned_sessions(user).await.unwrap();
            assert_eq!(
                store.get_system_prompt(42, user).await.unwrap().content,
                "Keep me"
            );
            assert_eq!(
                store.get_session(5, user).await.unwrap().system_prompt_id,
                Some(42)
            );
        });
    }

    #[test]
    fn system_prompt_crud() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let prompt = store
                .insert_system_prompt(user_id, "coder", "You are a coding agent.")
                .await
                .unwrap();
            assert!(prompt.id > 0);
            assert_eq!(prompt.name, "coder");

            assert_eq!(store.list_system_prompts(user_id).await.unwrap().len(), 1);

            let updated = store
                .update_system_prompt(prompt.id, user_id, "helper", "You are a helpful agent.")
                .await
                .unwrap();
            assert_eq!(updated.id, prompt.id);
            assert_eq!(updated.name, "helper");
            assert_eq!(updated.content, "You are a helpful agent.");

            assert!(
                store
                    .update_system_prompt(9999, user_id, "x", "y")
                    .await
                    .is_err()
            );

            store
                .delete_system_prompt(prompt.id, user_id)
                .await
                .unwrap();
            assert!(store.list_system_prompts(user_id).await.unwrap().is_empty());
        });
    }

    #[test]
    fn session_connection_change_preserves_history_and_checks_ownership() {
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::Admin);
        let bob = test_user(&store, "bob", UserRole::User);
        block_on(async {
            let connection = store
                .insert_connection(&NewConnection {
                    name: "local".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://localhost:11434".into(),
                    model: Some("model".into()),
                    context_limit: None,
                })
                .await
                .unwrap();
            let session = store
                .create_session("history", None, None, None, alice, 1)
                .await
                .unwrap();
            store
                .insert_message(session.id, Role::User, "hello", 2)
                .await
                .unwrap();
            assert!(
                store
                    .set_session_connection(session.id, connection.id, bob)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .set_session_connection(session.id, 9999, alice)
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .get_session(session.id, alice)
                    .await
                    .unwrap()
                    .connection_id,
                None
            );
            let changed = store
                .set_session_connection(session.id, connection.id, alice)
                .await
                .unwrap();
            assert_eq!(changed.connection_id, Some(connection.id));
            assert_eq!(changed.name, "history");
            assert_eq!(
                store.list_messages(session.id).await.unwrap()[0].content,
                "hello"
            );
        });
    }

    #[test]
    fn new_session_mode_is_auto_and_creation_rolls_back_if_mode_cannot_be_saved() {
        let store = test_store();
        let user = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let legacy = store
                .create_session("legacy", None, None, None, user, 1)
                .await
                .unwrap();
            store
                .db
                .execute(
                    "DELETE FROM user_settings WHERE user_id = ? AND key = ?",
                    &[
                        DbValue::Int(user.get()),
                        DbValue::Text(openwebide_core::ApprovalMode::setting_key(legacy.id)),
                    ],
                )
                .await
                .unwrap();
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let project = store
                    .create_project(
                        &NewProject {
                            name: "p".into(),
                            mode,
                            path: Some("p".into()),
                        },
                        user,
                        1,
                    )
                    .await
                    .unwrap();
                let session = store
                    .create_session("new", None, None, Some(project.id), user, 2)
                    .await
                    .unwrap();
                assert_eq!(
                    store
                        .get_user_setting(
                            user,
                            &openwebide_core::ApprovalMode::setting_key(session.id)
                        )
                        .await
                        .unwrap()
                        .as_deref(),
                    Some("\"auto\"")
                );
            }
            assert!(
                store
                    .get_user_setting(user, &openwebide_core::ApprovalMode::setting_key(legacy.id))
                    .await
                    .unwrap()
                    .is_none()
            );
            let before = store.list_sessions(user).await.unwrap();
            store.db.execute("CREATE TRIGGER reject_session_mode BEFORE INSERT ON user_settings BEGIN SELECT RAISE(ABORT, 'mode unavailable'); END", &[]).await.unwrap();
            assert!(
                store
                    .create_session("failed", None, None, None, user, 3)
                    .await
                    .is_err()
            );
            assert_eq!(store.list_sessions(user).await.unwrap(), before);
        });
    }

    #[test]
    fn prompt_attachments_persist_reload_and_rewind_without_losing_images_or_snapshots() {
        let store = test_store();
        let user = test_user(&store, "attachments", UserRole::Admin);
        block_on(async {
            let session = store
                .create_session("images", None, None, None, user, 1)
                .await
                .unwrap();
            let image =
                openwebide_core::PromptImage::from_bytes("test.png".into(), b"\x89PNG\r\n\x1a\n")
                    .unwrap();
            let prompt = openwebide_core::PromptContent {
                text: "Explain @file:source.rs".into(),
                images: vec![image],
                references: vec![openwebide_core::prompt::PromptReference {
                    mention: openwebide_core::prompt::Mention {
                        kind: openwebide_core::prompt::MentionKind::File,
                        path: "source.rs".into(),
                    },
                    content: "original contents".into(),
                }],
            };
            let message = store
                .insert_message(session.id, Role::User, &prompt.encode().unwrap(), 2)
                .await
                .unwrap();
            store
                .insert_message(session.id, Role::Assistant, "description", 3)
                .await
                .unwrap();
            let messages = store.list_messages(session.id).await.unwrap();
            assert_eq!(
                openwebide_core::PromptContent::parse(&messages[0].content).unwrap(),
                prompt
            );
            let plan = store
                .prepare_rewind(user, session.id, message.id)
                .await
                .unwrap();
            assert_eq!(
                openwebide_core::PromptContent::parse(&plan.prompt).unwrap(),
                prompt
            );
            assert!(
                store
                    .complete_rewind(user, session.id, message.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                store
                    .insert_message(session.id, Role::User, "[Open WebIDE prompt]\n{invalid}", 4)
                    .await
                    .is_err()
            );
            assert!(store.list_messages(session.id).await.unwrap().is_empty());
        });
    }

    #[test]
    fn sessions_and_messages() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let session = store
                .create_session("first session", None, None, None, user_id, 1_700_000_000)
                .await
                .unwrap();
            assert!(session.id > 0);
            assert_eq!(session.system_prompt_id, None);
            assert_eq!(session.user_id, Some(user_id));

            let reloaded = store.get_session(session.id, user_id).await.unwrap();
            assert_eq!(reloaded.name, "first session");

            let renamed = store
                .rename_session(session.id, "renamed", user_id)
                .await
                .unwrap();
            assert_eq!(renamed.name, "renamed");

            let msg = store
                .insert_message(session.id, Role::User, "hello", 1_700_000_001)
                .await
                .unwrap();
            assert_eq!(msg.id, 1);

            let messages = store.list_messages(session.id).await.unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].role, Role::User);
            assert_eq!(messages[0].content, "hello");
            assert_eq!(messages[0].usage, None);

            store.delete_session(session.id, user_id).await.unwrap();
            assert!(store.get_session(session.id, user_id).await.is_err());
            assert!(store.list_messages(session.id).await.unwrap().is_empty());
        });
    }

    #[test]
    fn oversized_persisted_usage_saturates_restored_telemetry() {
        let row = QueryRow {
            values: vec![
                DbValue::Int(1),
                DbValue::Int(1),
                DbValue::Text("assistant".into()),
                DbValue::Text("hello".into()),
                DbValue::Int(1),
                DbValue::Int(i64::MAX),
                DbValue::Int(i64::MAX),
                DbValue::Int(1),
                DbValue::Int(0),
                DbValue::Null,
            ],
        };
        let message = message_from_row(&row).unwrap();
        let entries = vec![ConversationEntry::Message(message); 3];
        let mut telemetry = openwebide_core::SessionTelemetry::default();
        telemetry.restore_from_conversation(&entries);
        assert_eq!(
            telemetry.context_tokens,
            usize::try_from(i64::MAX)
                .unwrap_or(usize::MAX)
                .saturating_mul(2)
        );
        assert_eq!(telemetry.total_prompt_tokens, usize::MAX);
        assert_eq!(telemetry.total_completion_tokens, usize::MAX);
    }

    #[test]
    fn message_usage_roundtrips_through_list_messages_and_conversation() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let session = store
                .create_session("s", None, None, None, user_id, 1)
                .await
                .unwrap();
            let usage = TurnTelemetry {
                context: Some(openwebide_core::ContextBreakdown {
                    system: 20,
                    files: 30,
                    tool_output: 10,
                    history: 50,
                    tools: 10,
                }),
                prompt_tokens: 120,
                completion_tokens: 30,
                eval_duration_ms: 900,
                estimated: true,
            };
            store
                .insert_message_with_usage(session.id, Role::User, "hi", 2, None)
                .await
                .unwrap();
            let assistant = store
                .insert_message_with_usage(session.id, Role::Assistant, "hello", 3, Some(&usage))
                .await
                .unwrap();
            assert_eq!(assistant.usage, Some(usage));

            let messages = store.list_messages(session.id).await.unwrap();
            assert_eq!(messages[0].usage, None);
            assert_eq!(messages[1].usage, Some(usage));

            let conversation = store.list_conversation(session.id).await.unwrap();
            let ConversationEntry::Message(last) = conversation.last().unwrap() else {
                panic!("expected a message");
            };
            assert_eq!(last.usage, Some(usage));
        });
    }

    #[test]
    fn migrate_is_idempotent_and_adds_usage_and_context_limit_columns() {
        let store = test_store();
        block_on(async {
            store.migrate().await.unwrap();

            for column in [
                "prompt_tokens",
                "completion_tokens",
                "eval_duration_ms",
                "usage_estimated",
            ] {
                let res = store
                    .db
                    .execute(
                        &format!(
                            "SELECT 1 FROM pragma_table_info('messages') WHERE name = '{column}'"
                        ),
                        &[],
                    )
                    .await
                    .unwrap();
                assert!(!res.rows.is_empty(), "missing messages.{column}");
            }
            let res = store
                .db
                .execute(
                    "SELECT 1 FROM pragma_table_info('connections') WHERE name = 'context_limit'",
                    &[],
                )
                .await
                .unwrap();
            assert!(!res.rows.is_empty(), "missing connections.context_limit");
        });
    }

    #[test]
    fn conversation_interleaves_tool_steps() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let session = store
                .create_session("s", None, None, None, user_id, 1)
                .await
                .unwrap();

            // Turn 1: user message, a gated write that succeeds, assistant reply.
            let user1 = store
                .insert_message(session.id, Role::User, "make a file", 2)
                .await
                .unwrap();
            store
                .upsert_tool_step(
                    session.id,
                    user1.id,
                    "call-1",
                    "write_file",
                    "write a.txt",
                    3,
                    None,
                )
                .await
                .unwrap();
            let diff = FileDiff {
                path: "a.txt".into(),
                old: None,
                new: "hi".into(),
                old_unavailable: false,
                backup_path: None,
            };
            store
                .complete_tool_step(
                    user_id,
                    session.id,
                    "call-1",
                    true,
                    "wrote a.txt",
                    Some(&diff),
                )
                .await
                .unwrap();
            store
                .insert_message(session.id, Role::Assistant, "done", 4)
                .await
                .unwrap();

            // Turn 2: another message and step, to prove per-anchor ordering.
            let user2 = store
                .insert_message(session.id, Role::User, "again", 5)
                .await
                .unwrap();
            store
                .upsert_tool_step(
                    session.id,
                    user2.id,
                    "call-2",
                    "read_file",
                    "read a.txt",
                    6,
                    None,
                )
                .await
                .unwrap();
            store
                .complete_tool_step(user_id, session.id, "call-2", true, "read a.txt", None)
                .await
                .unwrap();
            store
                .insert_message(session.id, Role::Assistant, "done again", 7)
                .await
                .unwrap();

            let convo = store.list_conversation(session.id).await.unwrap();
            assert_eq!(convo.len(), 6);
            assert!(matches!(
                convo[0],
                ConversationEntry::Message(ref m)
                    if m.role == Role::User && m.content == "make a file"
            ));
            match &convo[1] {
                ConversationEntry::ToolStep(ts) => {
                    assert_eq!(ts.tool_call_id, "call-1");
                    assert_eq!(ts.name, "write_file");
                    assert_eq!(ts.ok, Some(true));
                    assert_eq!(ts.result_summary.as_deref(), Some("wrote a.txt"));
                    assert_eq!(ts.anchor_message_id, user1.id);
                    assert!(ts.diff.is_some());
                }
                other @ (ConversationEntry::Message(_) | ConversationEntry::Task(_)) => {
                    panic!("expected tool step, got {other:?}")
                }
            }
            assert!(matches!(
                convo[2],
                ConversationEntry::Message(ref m)
                    if m.role == Role::Assistant && m.content == "done"
            ));
            assert!(matches!(
                convo[3],
                ConversationEntry::Message(ref m) if m.role == Role::User && m.content == "again"
            ));
            match &convo[4] {
                ConversationEntry::ToolStep(ts) => {
                    assert_eq!(ts.tool_call_id, "call-2");
                    assert_eq!(ts.ok, Some(true));
                    assert!(ts.diff.is_none());
                }
                other @ (ConversationEntry::Message(_) | ConversationEntry::Task(_)) => {
                    panic!("expected tool step, got {other:?}")
                }
            }
            assert!(matches!(
                convo[5],
                ConversationEntry::Message(ref m)
                    if m.role == Role::Assistant && m.content == "done again"
            ));

            // The LLM context path is unchanged: messages only, no tool steps.
            let messages = store.list_messages(session.id).await.unwrap();
            assert_eq!(messages.len(), 4);
        });
    }

    #[test]
    fn session_system_prompt_reference() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let prompt = store
                .insert_system_prompt(user_id, "coder", "You are a coding agent.")
                .await
                .unwrap();
            let session = store
                .create_session("s", None, Some(prompt.id), None, user_id, 1)
                .await
                .unwrap();
            assert_eq!(session.system_prompt_id, Some(prompt.id));

            store
                .delete_system_prompt(prompt.id, user_id)
                .await
                .unwrap();
            let reloaded = store.get_session(session.id, user_id).await.unwrap();
            assert_eq!(reloaded.system_prompt_id, None);
        });
    }

    #[test]
    fn deleting_connection_nulls_session() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let conn = store
                .insert_connection(&NewConnection {
                    name: "c".into(),
                    kind: ProviderKind::LlamaCpp,
                    base_url: "http://localhost:8080".into(),
                    model: None,
                    context_limit: None,
                })
                .await
                .unwrap();
            let session = store
                .create_session("s", Some(conn.id), None, None, user_id, 1)
                .await
                .unwrap();
            assert_eq!(session.connection_id, Some(conn.id));

            store.delete_connection(conn.id).await.unwrap();

            let sessions = store.list_sessions(user_id).await.unwrap();
            assert_eq!(sessions[0].connection_id, None);
        });
    }

    #[test]
    fn project_crud() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let project = store
                .create_project(
                    &NewProject {
                        name: "my-app".into(),
                        mode: WorkspaceMode::Remote,
                        path: Some("projects/my-app".into()),
                    },
                    user_id,
                    1_700_000_000,
                )
                .await
                .unwrap();
            assert!(project.id > 0);
            assert_eq!(project.mode, WorkspaceMode::Remote);
            assert_eq!(project.path.as_deref(), Some("projects/my-app"));
            assert_eq!(project.user_id, Some(user_id));

            let reloaded = store.get_project(project.id, user_id).await.unwrap();
            assert_eq!(reloaded.name, "my-app");

            let renamed = store
                .rename_project(project.id, "renamed-app", user_id)
                .await
                .unwrap();
            assert_eq!(renamed.name, "renamed-app");

            assert_eq!(store.list_projects(user_id).await.unwrap().len(), 1);

            store.delete_project(project.id, user_id).await.unwrap();
            assert!(store.get_project(project.id, user_id).await.is_err());
            assert!(store.list_projects(user_id).await.unwrap().is_empty());
        });
    }

    #[test]
    fn create_project_dedups_by_folder() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let new = NewProject {
                name: "my-app".into(),
                mode: WorkspaceMode::Remote,
                path: Some("projects/my-app".into()),
            };
            let first = store.create_project(&new, user_id, 1).await.unwrap();
            // The same folder again returns the existing project.
            let again = store.create_project(&new, user_id, 2).await.unwrap();
            assert_eq!(again.id, first.id);
            assert_eq!(store.list_projects(user_id).await.unwrap().len(), 1);

            // A different mode or a pathless project is a distinct project.
            let local = store
                .create_project(
                    &NewProject {
                        name: "my-app".into(),
                        mode: WorkspaceMode::Local,
                        path: Some("projects/my-app".into()),
                    },
                    user_id,
                    3,
                )
                .await
                .unwrap();
            assert_ne!(local.id, first.id);
            let pathless = store
                .create_project(
                    &NewProject {
                        name: "other".into(),
                        mode: WorkspaceMode::Local,
                        path: None,
                    },
                    user_id,
                    4,
                )
                .await
                .unwrap();
            assert_ne!(pathless.id, local.id);
            assert_eq!(store.list_projects(user_id).await.unwrap().len(), 3);
        });
    }

    /// The database's current schema version (`PRAGMA user_version`).
    async fn schema_version(db: &RusqliteDb) -> i64 {
        db.execute("PRAGMA user_version", &[]).await.unwrap().rows[0]
            .get_int(0)
            .unwrap()
    }

    #[test]
    fn migrate_sets_version_and_is_idempotent() {
        let db = RusqliteDb::open_in_memory().unwrap();
        block_on(async {
            let store = Store::new(db);
            store.migrate().await.unwrap();
            assert_eq!(schema_version(&store.db).await, migrations::SCHEMA_VERSION);
            // A second migrate is a no-op that keeps the version.
            store.migrate().await.unwrap();
            assert_eq!(schema_version(&store.db).await, migrations::SCHEMA_VERSION);
        });
    }

    #[test]
    fn migrate_when_current_does_no_work() {
        let db = RusqliteDb::open_in_memory().unwrap();
        block_on(async {
            // A global setting set before the upgrade...
            migrations::apply_through(&db, 1).await.unwrap();
            let store = Store::new(db);
            store.set_setting("theme", "dark").await.unwrap();
            store.migrate().await.unwrap();
            // ...is copied for users that exist at upgrade time. A user
            // created later, even with migrate re-run, inherits nothing.
            let user = store
                .insert_user("late", "hash", UserRole::User, 2)
                .await
                .unwrap();
            store.migrate().await.unwrap();
            assert!(store.all_user_settings(user.id).await.unwrap().is_empty());
        });
    }

    #[test]
    fn legacy_settings_copy_is_allow_listed() {
        let db = RusqliteDb::open_in_memory().unwrap();
        block_on(async {
            // Stop before the legacy-settings copy step.
            migrations::apply_through(&db, 6).await.unwrap();
            let store = Store::new(db);
            store.db.execute(

                "INSERT INTO users (username, password_hash, role, created_at) VALUES (?, 'hash', 'admin', 1)",

                &[DbValue::Text("alice".into())]

            ).await.unwrap();

            let user = store
                .db
                .execute(
                    "SELECT id FROM users WHERE username = ?",
                    &[DbValue::Text("alice".into())],
                )
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();

            let user = UserRecord {
                id: UserId::new(user),
                username: "alice".into(),
                password_hash: "hash".into(),
                role: UserRole::Admin,
                created_at: 1,
                token_epoch: 0,
            };
            store.set_setting("theme", "dark").await.unwrap();
            store
                .set_setting("web_search_api_key", "secret")
                .await
                .unwrap();
            store.migrate().await.unwrap();
            let settings = store.all_user_settings(user.id).await.unwrap();
            assert_eq!(settings.len(), 1);
            assert_eq!(settings.get("theme").map(String::as_str), Some("dark"));
        });
    }

    #[test]
    fn unversioned_database_replays_cleanly() {
        let db = RusqliteDb::open_in_memory().unwrap();
        block_on(async {
            // A pre-versioning database: every object the old
            // re-apply-at-startup created, but user_version still 0.
            migrations::apply_through(&db, 9).await.unwrap();
            let store = Store::new(db);
            store.migrate().await.unwrap();
            assert_eq!(schema_version(&store.db).await, migrations::SCHEMA_VERSION);
        });
    }

    #[test]
    fn newer_schema_is_rejected() {
        let db = RusqliteDb::open_in_memory().unwrap();
        block_on(async {
            db.execute("PRAGMA user_version = 999", &[]).await.unwrap();
            let store = Store::new(db);
            let err = store.migrate().await.unwrap_err();
            assert!(
                err.to_string().contains("newer than this build"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn upgrade_main_schema_preserves_skills_and_goals_and_adds_host_questions_and_monitors() {
        block_on(async {
            let db = RusqliteDb::open_in_memory().unwrap();
            migrations::apply_through(&db, 40).await.unwrap();
            let store = Store::new(db);
            let user = store
                .insert_user("upgrade", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            store
                .set_user_setting(user, "upgrade-marker", "keep")
                .await
                .unwrap();
            store.migrate().await.unwrap();
            store.migrate().await.unwrap();
            assert_eq!(schema_version(&store.db).await, migrations::SCHEMA_VERSION);
            assert_eq!(
                store
                    .get_user_setting(user, "upgrade-marker")
                    .await
                    .unwrap()
                    .as_deref(),
                Some("keep")
            );
            for table in [
                "project_skills",
                "goal_workers",
                "host_operations",
                "agent_questions",
                "monitors",
            ] {
                let rows = store
                    .db
                    .execute(
                        "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
                        &[DbValue::Text(table.into())],
                    )
                    .await
                    .unwrap();
                assert_eq!(rows.rows.len(), 1, "{table}");
            }
        });
    }
    #[test]
    fn test_migration_dedups_existing_projects() {
        let db = RusqliteDb::open_in_memory().unwrap();
        block_on(async {
            // Stop before the dedup step: a fully migrated schema would
            // reject the duplicate insert below via the unique index.
            migrations::apply_through(&db, 5).await.unwrap();
            let store = Store::new(db);
            store.db.execute(

                "INSERT INTO users (username, password_hash, role, created_at) VALUES (?, 'hash', 'admin', 1)",

                &[DbValue::Text("alice".into())]

            ).await.unwrap();

            let user = store
                .db
                .execute(
                    "SELECT id FROM users WHERE username = ?",
                    &[DbValue::Text("alice".into())],
                )
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();

            let user = UserRecord {
                id: UserId::new(user),
                username: "alice".into(),
                password_hash: "hash".into(),
                role: UserRole::Admin,
                created_at: 1,
                token_epoch: 0,
            };

            // Directly insert two projects with the exact same path
            store.db.execute(
                "INSERT INTO projects (name, mode, path, user_id, created_at) VALUES (?, ?, ?, ?, ?)",
                &[
                    DbValue::Text("dup1".into()),
                    DbValue::Text("remote".into()),
                    DbValue::Text("repos/dup".into()),
                    DbValue::Int(user.id.get()),
                    DbValue::Int(100),
                ],
            ).await.unwrap();
            let p1_id = store
                .db
                .execute("SELECT last_insert_rowid()", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();

            store.db.execute(
                "INSERT INTO projects (name, mode, path, user_id, created_at) VALUES (?, ?, ?, ?, ?)",
                &[
                    DbValue::Text("dup2".into()),
                    DbValue::Text("remote".into()),
                    DbValue::Text("repos/dup".into()),
                    DbValue::Int(user.id.get()),
                    DbValue::Int(200),
                ],
            ).await.unwrap();
            let p2_id = store
                .db
                .execute("SELECT last_insert_rowid()", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();

            // Insert a session on p2
            store.db.execute(
                "INSERT INTO sessions (name, project_id, user_id, created_at) VALUES (?, ?, ?, ?)",
                &[
                    DbValue::Text("session-on-dup".into()),
                    DbValue::Int(p2_id),
                    DbValue::Int(user.id.get()),
                    DbValue::Int(250),
                ],
            ).await.unwrap();

            assert_eq!(store.list_projects(user.id).await.unwrap().len(), 2);

            // Run the rest of the migration
            store.migrate().await.unwrap();

            // Now there should be only 1 project
            let projs = store.list_projects(user.id).await.unwrap();
            assert_eq!(projs.len(), 1);
            assert_eq!(projs[0].id, p1_id);

            // And the session on p2 was reassigned to p1
            let sessions = store.list_sessions(user.id).await.unwrap();
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].project_id, Some(p1_id));
        });
    }

    #[test]
    fn docker_workspace_paths_rewritten() {
        let db = RusqliteDb::open_in_memory().unwrap();
        block_on(async {
            // Stop before the rewrite step.
            migrations::apply_through(&db, 10).await.unwrap();
            let store = Store::new(db);
            store.db.execute(

                "INSERT INTO users (username, password_hash, role, created_at) VALUES (?, 'hash', 'admin', 1)",

                &[DbValue::Text("alice".into())]

            ).await.unwrap();

            let user = store
                .db
                .execute(
                    "SELECT id FROM users WHERE username = ?",
                    &[DbValue::Text("alice".into())],
                )
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();

            let user = UserRecord {
                id: UserId::new(user),
                username: "alice".into(),
                password_hash: "hash".into(),
                role: UserRole::Admin,
                created_at: 1,
                token_epoch: 0,
            };
            // A Docker install from before the /workspace mount.
            store.db.execute(
                "INSERT INTO projects (name, mode, path, user_id, created_at) VALUES (?, ?, ?, ?, ?)",
                &[
                    DbValue::Text("app".into()),
                    DbValue::Text("remote".into()),
                    DbValue::Text("workspace/foo".into()),
                    DbValue::Int(user.id.get()),
                    DbValue::Int(100),
                ],
            ).await.unwrap();

            // The mount has `foo` but not the stale `workspace/foo`.
            let probe = |rel: &str| rel == "foo";
            store.migrate_with(&probe).await.unwrap();
            let projs = store.list_projects(user.id).await.unwrap();
            assert_eq!(projs[0].path.as_deref(), Some("foo"));

            // A second migrate is a no-op.
            store.migrate_with(&probe).await.unwrap();
            let projs = store.list_projects(user.id).await.unwrap();
            assert_eq!(projs.len(), 1);
            assert_eq!(projs[0].path.as_deref(), Some("foo"));
        });
    }

    #[test]
    fn local_workspace_folder_untouched() {
        let db = RusqliteDb::open_in_memory().unwrap();
        block_on(async {
            migrations::apply_through(&db, 10).await.unwrap();
            let store = Store::new(db);
            store.db.execute(

                "INSERT INTO users (username, password_hash, role, created_at) VALUES (?, 'hash', 'admin', 1)",

                &[DbValue::Text("alice".into())]

            ).await.unwrap();

            let user = store
                .db
                .execute(
                    "SELECT id FROM users WHERE username = ?",
                    &[DbValue::Text("alice".into())],
                )
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();

            let user = UserRecord {
                id: UserId::new(user),
                username: "alice".into(),
                password_hash: "hash".into(),
                role: UserRole::Admin,
                created_at: 1,
                token_epoch: 0,
            };
            store.db.execute(
                "INSERT INTO projects (name, mode, path, user_id, created_at) VALUES (?, ?, ?, ?, ?)",
                &[
                    DbValue::Text("app".into()),
                    DbValue::Text("remote".into()),
                    DbValue::Text("workspace/foo".into()),
                    DbValue::Int(user.id.get()),
                    DbValue::Int(100),
                ],
            ).await.unwrap();

            // A local install where `workspace/foo` really exists.
            let probe = |rel: &str| rel == "workspace/foo";
            store.migrate_with(&probe).await.unwrap();
            let projs = store.list_projects(user.id).await.unwrap();
            assert_eq!(projs[0].path.as_deref(), Some("workspace/foo"));
        });
    }

    #[test]
    fn docker_rewrite_skips_conflict() {
        let db = RusqliteDb::open_in_memory().unwrap();
        block_on(async {
            migrations::apply_through(&db, 10).await.unwrap();
            let store = Store::new(db);
            store.db.execute(

                "INSERT INTO users (username, password_hash, role, created_at) VALUES (?, 'hash', 'admin', 1)",

                &[DbValue::Text("alice".into())]

            ).await.unwrap();

            let user = store
                .db
                .execute(
                    "SELECT id FROM users WHERE username = ?",
                    &[DbValue::Text("alice".into())],
                )
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();

            let user = UserRecord {
                id: UserId::new(user),
                username: "alice".into(),
                password_hash: "hash".into(),
                role: UserRole::Admin,
                created_at: 1,
                token_epoch: 0,
            };
            // The owner already has a project at the stripped path.
            store.db.execute(
                "INSERT INTO projects (name, mode, path, user_id, created_at) VALUES (?, ?, ?, ?, ?)",
                &[
                    DbValue::Text("app".into()),
                    DbValue::Text("remote".into()),
                    DbValue::Text("foo".into()),
                    DbValue::Int(user.id.get()),
                    DbValue::Int(100),
                ],
            ).await.unwrap();
            store.db.execute(
                "INSERT INTO projects (name, mode, path, user_id, created_at) VALUES (?, ?, ?, ?, ?)",
                &[
                    DbValue::Text("app-old".into()),
                    DbValue::Text("remote".into()),
                    DbValue::Text("workspace/foo".into()),
                    DbValue::Int(user.id.get()),
                    DbValue::Int(200),
                ],
            ).await.unwrap();

            let probe = |rel: &str| rel == "foo";
            store.migrate_with(&probe).await.unwrap();
            let projs = store.list_projects(user.id).await.unwrap();
            assert_eq!(projs.len(), 2);
            assert_eq!(projs[0].path.as_deref(), Some("foo"));
            assert_eq!(projs[1].path.as_deref(), Some("workspace/foo"));
        });
    }

    #[test]
    fn session_belongs_to_project() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let project = store
                .create_project(
                    &NewProject {
                        name: "my-app".into(),
                        mode: WorkspaceMode::Local,
                        path: None,
                    },
                    user_id,
                    1,
                )
                .await
                .unwrap();
            let session = store
                .create_session("s", None, None, Some(project.id), user_id, 2)
                .await
                .unwrap();
            assert_eq!(session.project_id, Some(project.id));

            let for_project = store
                .list_sessions_for_project(project.id, user_id)
                .await
                .unwrap();
            assert_eq!(for_project.len(), 1);
            assert_eq!(for_project[0].id, session.id);

            // Deleting the project cascades to its sessions.
            store.delete_project(project.id, user_id).await.unwrap();
            assert!(store.get_session(session.id, user_id).await.is_err());
        });
    }

    #[test]
    fn users_scoping_isolates_data() {
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::Admin);
        let bob = test_user(&store, "bob", UserRole::User);
        block_on(async {
            let project = store
                .create_project(
                    &NewProject {
                        name: "alice-app".into(),
                        mode: WorkspaceMode::Local,
                        path: None,
                    },
                    alice,
                    1,
                )
                .await
                .unwrap();
            let session = store
                .create_session("s", None, None, Some(project.id), alice, 2)
                .await
                .unwrap();

            // Bob sees none of Alice's projects or sessions.
            assert!(store.list_projects(bob).await.unwrap().is_empty());
            assert!(store.list_sessions(bob).await.unwrap().is_empty());
            assert!(store.get_project(project.id, bob).await.is_err());
            assert!(store.get_session(session.id, bob).await.is_err());

            // Alice still sees her own.
            assert_eq!(store.list_projects(alice).await.unwrap().len(), 1);
            assert_eq!(store.list_sessions(alice).await.unwrap().len(), 1);
        });
    }

    #[test]
    fn first_user_inherits_orphaned_projects() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            // A project created before accounts existed has no owner.
            let res = store
                .db
                .execute(
                    "INSERT INTO projects (name, mode, path, created_at) VALUES (?, ?, ?, ?)",
                    &[
                        DbValue::Text("legacy".into()),
                        DbValue::Text("local".into()),
                        DbValue::Null,
                        DbValue::Int(1),
                    ],
                )
                .await
                .unwrap();
            let orphan_id = res.last_insert_rowid;

            let reassigned = store.reassign_orphaned_projects(user_id).await.unwrap();
            assert_eq!(reassigned, 1);

            let reloaded = store.get_project(orphan_id, user_id).await.unwrap();
            assert_eq!(reloaded.user_id, Some(user_id));
        });
    }

    #[test]
    fn first_user_inherits_orphaned_sessions() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            // A session created before accounts existed has no owner.
            let res = store
                .db
                .execute(
                    "INSERT INTO sessions (name, created_at) VALUES (?, ?)",
                    &[DbValue::Text("legacy".into()), DbValue::Int(1)],
                )
                .await
                .unwrap();
            let orphan_id = res.last_insert_rowid;

            let reassigned = store.reassign_orphaned_sessions(user_id).await.unwrap();
            assert_eq!(reassigned, 1);

            let reloaded = store.get_session(orphan_id, user_id).await.unwrap();
            assert_eq!(reloaded.user_id, Some(user_id));
        });
    }

    #[test]
    fn migrate_cancel_timestamp_from_version_16() {
        block_on(async {
            let db = RusqliteDb::open_in_memory().unwrap();
            migrations::apply_through(&db, 16).await.unwrap();
            let store = Store::new(db);
            let user = store
                .insert_user("alice", "hash", UserRole::Admin, 1)
                .await
                .unwrap();
            // Seed with the old schema rather than today's session decoder.
            let session_id = store.db.execute("INSERT INTO sessions (name, project_id, user_id, created_at) VALUES ('s', ?, ?, 1)", &[DbValue::Null, DbValue::Int(user.id.get())]).await.unwrap().last_insert_rowid;
            store
                .db
                .execute(
                    "INSERT INTO run_cancels (session_id) VALUES (?)",
                    &[DbValue::Int(session_id)],
                )
                .await
                .unwrap();
            store.migrate().await.unwrap();
            assert_eq!(schema_version(&store.db).await, migrations::SCHEMA_VERSION);
            assert!(!store.cancel_requested_since(session_id, 0).await.unwrap());
            store.request_cancel(session_id, 1000).await.unwrap();
            migrations::apply_through(&store.db, 17).await.unwrap();
            assert!(store.cancel_requested_since(session_id, 999).await.unwrap());
            store.migrate().await.unwrap();
            assert!(store.cancel_requested_since(session_id, 999).await.unwrap());
        });
    }

    #[test]
    fn cancel_flag_lifecycle() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let session = store
                .create_session("s", None, None, None, user_id, 1)
                .await
                .unwrap();
            assert!(!store.cancel_requested_since(session.id, 0).await.unwrap());
            store.request_cancel(session.id, 1000).await.unwrap();
            assert!(store.cancel_requested_since(session.id, 999).await.unwrap());
            assert!(
                !store
                    .cancel_requested_since(session.id, 1000)
                    .await
                    .unwrap()
            );
            assert!(
                !store
                    .cancel_requested_since(session.id, 2000)
                    .await
                    .unwrap()
            );
            store.request_cancel(session.id, 3000).await.unwrap();
            assert!(
                store
                    .cancel_requested_since(session.id, 2000)
                    .await
                    .unwrap()
            );
        });
    }

    #[test]
    fn tool_permission_lifecycle() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);
        block_on(async {
            let session = store
                .create_session("s", None, None, None, user_id, 1)
                .await
                .unwrap();
            assert_eq!(
                store
                    .take_tool_permission(session.id, "a1t1c0")
                    .await
                    .unwrap(),
                None
            );

            store
                .set_tool_permission(session.id, "a1t1c0", true)
                .await
                .unwrap();
            store
                .set_tool_permission(session.id, "a1t1c1", false)
                .await
                .unwrap();

            // Other tool calls are unaffected by a take.
            assert_eq!(
                store
                    .take_tool_permission(session.id, "a1t1c2")
                    .await
                    .unwrap(),
                None
            );

            // A decision answers exactly one wait: taking it consumes it.
            assert_eq!(
                store
                    .take_tool_permission(session.id, "a1t1c0")
                    .await
                    .unwrap(),
                Some(true)
            );
            assert_eq!(
                store
                    .take_tool_permission(session.id, "a1t1c0")
                    .await
                    .unwrap(),
                None
            );
            // ...and leaves the other calls' decisions in place.
            assert_eq!(
                store
                    .take_tool_permission(session.id, "a1t1c1")
                    .await
                    .unwrap(),
                Some(false)
            );

            store
                .set_tool_permission(session.id, "a1t1c1", true)
                .await
                .unwrap();
            store
                .set_tool_permission(session.id, "a10t1c0", true)
                .await
                .unwrap();
            let other = store
                .create_session("other", None, None, None, user_id, 1)
                .await
                .unwrap();
            store
                .set_tool_permission(other.id, "a1t1c1", false)
                .await
                .unwrap();
            store
                .clear_tool_permissions_for_run(session.id, 1)
                .await
                .unwrap();
            assert_eq!(
                store
                    .take_tool_permission(session.id, "a10t1c0")
                    .await
                    .unwrap(),
                Some(true)
            );
            assert_eq!(
                store
                    .take_tool_permission(other.id, "a1t1c1")
                    .await
                    .unwrap(),
                Some(false)
            );
            assert_eq!(
                store
                    .take_tool_permission(session.id, "a1t1c1")
                    .await
                    .unwrap(),
                None
            );
        });
    }

    #[test]
    fn delete_project_is_owner_scoped() {
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::Admin);
        let bob = test_user(&store, "bob", UserRole::User);

        block_on(async {
            let p1 = store
                .create_project(
                    &openwebide_core::NewProject {
                        name: "p1".into(),
                        mode: openwebide_core::WorkspaceMode::Local,
                        path: None,
                    },
                    alice,
                    1,
                )
                .await
                .unwrap();
            let s1 = store
                .create_session("s1", None, None, Some(p1.id), alice, 1)
                .await
                .unwrap();
            store
                .insert_message(s1.id, openwebide_core::Role::User, "hello", 1)
                .await
                .unwrap();

            // Bob tries to delete Alice's project
            let err = store.delete_project(p1.id, bob).await.unwrap_err();
            assert!(matches!(err, StorageError::NotFound(_)));

            // Alice's data should be intact
            assert!(store.get_project(p1.id, alice).await.is_ok());
            assert_eq!(store.get_session(s1.id, alice).await.unwrap().id, s1.id);
            assert_eq!(store.list_messages(s1.id).await.unwrap().len(), 1);

            // Alice deletes it
            store.delete_project(p1.id, alice).await.unwrap();
            assert!(matches!(
                store.get_project(p1.id, alice).await.unwrap_err(),
                StorageError::NotFound(_)
            ));
            assert!(matches!(
                store.get_session(s1.id, alice).await.unwrap_err(),
                StorageError::NotFound(_)
            ));
            assert_eq!(store.list_messages(s1.id).await.unwrap().len(), 0);
        });
    }

    #[test]
    fn create_session_rejects_foreign_project() {
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::Admin);
        let bob = test_user(&store, "bob", UserRole::User);

        block_on(async {
            let p1 = store
                .create_project(
                    &openwebide_core::NewProject {
                        name: "p1".into(),
                        mode: openwebide_core::WorkspaceMode::Local,
                        path: None,
                    },
                    alice,
                    1,
                )
                .await
                .unwrap();

            // Bob tries to create a session in Alice's project
            let err = store
                .create_session("s1", None, None, Some(p1.id), bob, 1)
                .await
                .unwrap_err();
            assert!(matches!(err, StorageError::NotFound(_)));
        });
    }

    #[test]
    fn insert_first_admin_only_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let db_path =
            std::env::temp_dir().join(format!("openwebide_test_{}_{}.db", std::process::id(), id));

        let db1 = crate::rusqlite_db::RusqliteDb::open(&db_path).unwrap();
        let store1 = Store::new(db1);
        block_on(store1.migrate()).unwrap();

        let db2 = crate::rusqlite_db::RusqliteDb::open(&db_path).unwrap();
        let store2 = Store::new(db2);

        block_on(async {
            let (res1, res2) = futures::future::join(
                store1.insert_first_admin("alice", "hash", 1),
                store2.insert_first_admin("bob", "hash2", 2),
            )
            .await;

            let success1 = matches!(res1, Ok(Some(_)));
            let success2 = matches!(res2, Ok(Some(_)));

            assert!(success1 || success2, "At least one should succeed");
            assert!(
                !(success1 && success2),
                "Both should not succeed in creating the first admin"
            );

            assert_eq!(store1.count_users().await.unwrap(), 1);
        });

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::Admin);

        block_on(async {
            let p1 = store
                .create_project(
                    &openwebide_core::NewProject {
                        name: "p1".into(),
                        mode: openwebide_core::WorkspaceMode::Local,
                        path: None,
                    },
                    alice,
                    1,
                )
                .await
                .unwrap();

            // A transaction that fails midway
            let res: Result<(), _> = store
                .db
                .transaction(|tx| async move {
                    tx.execute(
                        "UPDATE projects SET name = 'new' WHERE id = ?",
                        &[DbValue::Int(p1.id)],
                    )
                    .await?;

                    Err(StorageError::InvalidValue("fake error".into()))
                })
                .await;

            assert!(res.is_err());

            // Rollback should have kept the old name
            let p_after = store.get_project(p1.id, alice).await.unwrap();
            assert_eq!(p_after.name, "p1");
        });
    }

    #[test]
    fn duplicate_connection_name_is_conflict() {
        let store = test_store();
        // let alice = test_user(&store, "alice", UserRole::Admin);

        block_on(async {
            store
                .insert_connection(&openwebide_core::NewConnection {
                    name: "conn1".into(),
                    base_url: "http://test".into(),
                    kind: openwebide_core::ProviderKind::Ollama,
                    model: None,
                    context_limit: None,
                })
                .await
                .unwrap();

            let err = store
                .insert_connection(&openwebide_core::NewConnection {
                    name: "conn1".into(),
                    base_url: "http://test2".into(),
                    kind: openwebide_core::ProviderKind::Ollama,
                    model: None,
                    context_limit: None,
                })
                .await
                .unwrap_err();

            match err {
                StorageError::Conflict(msg) => assert!(
                    msg.contains("connections.name"),
                    "Expected connections.name in conflict msg, got: {msg}"
                ),
                _ => panic!("Expected Conflict error"),
            }
        });
    }

    #[test]
    fn duplicate_system_prompt_is_conflict() {
        let store = test_store();
        let user_id = test_user(&store, "alice", UserRole::Admin);

        block_on(async {
            store
                .insert_system_prompt(user_id, "prompt1", "sys")
                .await
                .unwrap();
            let err = store
                .insert_system_prompt(user_id, "prompt1", "sys2")
                .await
                .unwrap_err();
            match err {
                StorageError::Conflict(msg) => assert!(
                    msg.contains("system_prompts.name"),
                    "Expected system_prompts.name in conflict msg, got: {msg}"
                ),
                _ => panic!("Expected Conflict error"),
            }
        });
    }

    #[test]
    fn foreign_keys_enabled() {
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::Admin);

        block_on(async {
            // connection_id 999 does not exist
            let err = store
                .create_session("s1", Some(999), None, None, alice, 1)
                .await
                .unwrap_err();
            assert!(matches!(err, StorageError::Db(_)));
            if let StorageError::Db(msg) = err {
                assert!(
                    msg.contains("constraint"),
                    "Expected constraint error, got {msg}"
                );
            }
        });
    }

    #[test]
    fn login_failures_tracking() {
        let store = test_store();
        block_on(async {
            let user = "test_user";
            assert_eq!(store.login_failures(user).await.unwrap(), None);

            store.record_login_failure(user, 100).await.unwrap();
            let (f, t) = store.login_failures(user).await.unwrap().unwrap();
            assert_eq!(f, 1);
            assert_eq!(t, 100);

            store.record_login_failure(user, 101).await.unwrap();
            let (f, t) = store.login_failures(user).await.unwrap().unwrap();
            assert_eq!(f, 2);
            assert_eq!(t, 101);

            // 25 hours later -> purged
            store.record_login_failure(user, 101 + 90000).await.unwrap();
            let (f, t) = store.login_failures(user).await.unwrap().unwrap();
            assert_eq!(f, 1);
            assert_eq!(t, 90101);

            store.clear_login_failures(user).await.unwrap();
            assert_eq!(store.login_failures(user).await.unwrap(), None);
        });
    }

    #[test]
    fn test_bump_token_epoch() {
        let store = test_store();
        let alice = test_user(&store, "alice", UserRole::User);
        block_on(async {
            let u1 = store.get_user(alice).await.unwrap().unwrap();
            assert_eq!(u1.token_epoch, 0);

            store.bump_token_epoch(alice).await.unwrap();

            let u2 = store.get_user(alice).await.unwrap().unwrap();
            assert_eq!(u2.token_epoch, 1);
        });
    }

    #[test]
    fn migrate_12_13_idempotent() {
        let store = test_store();
        block_on(async {
            store.migrate().await.unwrap();
            store.migrate().await.unwrap();
        });
    }
}
