//! Schema migrations.
//!
//! Spin has no automatic migration runner, so the app applies migrations
//! on every request. Migrations are version-gated with `PRAGMA
//! user_version`: each numbered step runs exactly once, and a database
//! whose version is newer than this build's [`SCHEMA_VERSION`] refuses to
//! start (a rollback deploy fails loudly).
//!
//! Latest steps:
//! - 14: `add_message_tool_calls` persists interim wire calls.
//! - 15: `add_connection_tool_stream_unsupported` persists the streamed-tools memo.
//! - 17: `add_cancel_requested_at_ms` isolates cancellation by run start time.
//!
//! Rules for changing the schema:
//! - Append a new numbered step at the end of [`apply_step`] and bump
//!   [`SCHEMA_VERSION`].
//! - Every step must stay idempotent: pre-versioning databases start at
//!   version 0 and replay every step.
//! - Never edit or reorder a shipped step.

use crate::StorageError;
use crate::db::Db;

/// The highest schema version this build knows how to apply.
pub const SCHEMA_VERSION: i64 = 56;

pub const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        username TEXT NOT NULL UNIQUE,
        password_hash TEXT NOT NULL,
        role TEXT NOT NULL CHECK (role IN ('admin', 'user')) DEFAULT 'user',
        created_at INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS connections (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL UNIQUE,
        kind TEXT NOT NULL CHECK (kind IN ('ollama', 'llamacpp')),
        base_url TEXT NOT NULL,
        model TEXT,
        enabled INTEGER NOT NULL DEFAULT 1
    )",
    "CREATE TABLE IF NOT EXISTS system_prompts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL UNIQUE,
        content TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS projects (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        mode TEXT NOT NULL CHECK (mode IN ('remote', 'local')),
        path TEXT,
        created_at INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS sessions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        connection_id INTEGER REFERENCES connections(id) ON DELETE SET NULL,
        created_at INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK (role IN ('system', 'user', 'assistant')),
        content TEXT NOT NULL,
        created_at INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_messages_session ON messages (session_id)",
    // Agent tool steps, persisted so a session's steps survive a tab switch.
    // Kept out of `messages` (which feeds the LLM context); `anchor_message_id`
    // is the user message that started the turn, so steps render right after it.
    "CREATE TABLE IF NOT EXISTS tool_steps (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        anchor_message_id INTEGER NOT NULL,
        tool_call_id TEXT NOT NULL,
        name TEXT NOT NULL,
        summary TEXT NOT NULL,
        ok INTEGER,
        result_summary TEXT,
        diff TEXT,
        created_at INTEGER NOT NULL,
        UNIQUE (session_id, tool_call_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_tool_steps_session ON tool_steps (session_id)",
    "CREATE TABLE IF NOT EXISTS run_cancels (
        session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE
    )",
    "CREATE TABLE IF NOT EXISTS tool_permissions (
        session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        tool_call_id TEXT NOT NULL,
        decision INTEGER NOT NULL,
        PRIMARY KEY (session_id, tool_call_id)
    )",
    "CREATE TABLE IF NOT EXISTS user_settings (
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        key TEXT NOT NULL,
        value TEXT NOT NULL,
        PRIMARY KEY (user_id, key)
    )",
];

/// Run one numbered migration step (1-based, in landing order). `probe`
/// answers "does this mount-relative directory exist?" and is only
/// consulted by step 11.
async fn apply_step<D: Db>(
    db: &D,
    step: i64,
    probe: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Result<(), StorageError> {
    match step {
        32 => {
            db.execute(
                "CREATE TABLE IF NOT EXISTS push_subscriptions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                endpoint TEXT NOT NULL UNIQUE, subscription TEXT NOT NULL,
                created_at INTEGER NOT NULL)",
                &[],
            )
            .await?;
            db.execute(
                "CREATE TABLE IF NOT EXISTS push_notifications (
                user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                event_key TEXT NOT NULL, created_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, session_id, event_key))",
                &[],
            )
            .await?;
            db.execute("CREATE TABLE IF NOT EXISTS push_deliveries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                subscription_id INTEGER NOT NULL REFERENCES push_subscriptions(id) ON DELETE CASCADE,
                session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                payload TEXT NOT NULL, event TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                next_attempt INTEGER NOT NULL, expires_at INTEGER NOT NULL,
                lease_until INTEGER NOT NULL DEFAULT 0)", &[]).await?;
            db.execute("CREATE INDEX IF NOT EXISTS idx_push_due ON push_deliveries(next_attempt, lease_until)", &[]).await?;
            Ok(())
        }
        1 => {
            for stmt in MIGRATIONS {
                db.execute(stmt, &[]).await?;
            }
            Ok(())
        }
        2 => add_session_system_prompt_column(db).await,
        3 => add_session_project_column(db).await,
        4 => add_project_user_column(db).await,
        5 => add_session_user_column(db).await,
        6 => dedup_duplicate_projects(db).await,
        7 => migrate_legacy_settings_to_user_settings(db).await,
        8 => add_message_usage_columns(db).await,
        9 => add_connection_context_limit_column(db).await,
        10 => create_project_path_index(db).await,
        11 => rewrite_docker_workspace_paths(db, probe).await,
        12 => create_login_failures(db).await,
        13 => add_user_token_epoch(db).await,
        14 => add_message_tool_calls(db).await,
        15 => add_connection_tool_stream_unsupported(db).await,
        16 => add_connection_tool_stream_revision(db).await,
        17 => add_cancel_requested_at_ms(db).await,
        18 => create_pending_edits(db).await,
        19 => create_model_settings(db).await,
        20 => share_model_profiles(db).await,
        21 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('tool_steps') WHERE name = 'checkpoint'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute("ALTER TABLE tool_steps ADD COLUMN checkpoint TEXT", &[])
                    .await?;
            }
            db.execute(
                "CREATE TABLE IF NOT EXISTS session_rewinds (
                session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                message_id INTEGER NOT NULL, conversation TEXT NOT NULL, plan TEXT NOT NULL
            )",
                &[],
            )
            .await?;
            db.execute(
                "CREATE TABLE IF NOT EXISTS rewind_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                message_id INTEGER NOT NULL, conversation TEXT NOT NULL
            )",
                &[],
            )
            .await?;
            Ok(())
        }
        22 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('pending_edits') WHERE name = 'file'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute("ALTER TABLE pending_edits ADD COLUMN file TEXT", &[])
                    .await?;
            }
            db.execute("CREATE TABLE IF NOT EXISTS run_changes (id INTEGER PRIMARY KEY AUTOINCREMENT, project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE, path TEXT NOT NULL, state TEXT NOT NULL, UNIQUE(session_id, message_id, path))", &[]).await?;
            db.execute("CREATE TABLE IF NOT EXISTS project_reviews (project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, plan TEXT NOT NULL)", &[]).await?;
            db.execute("CREATE TABLE IF NOT EXISTS run_review_history (project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE, path TEXT NOT NULL, revision INTEGER NOT NULL, request TEXT NOT NULL, state TEXT NOT NULL, PRIMARY KEY (session_id, message_id, path, revision))", &[]).await?;
            Ok(())
        }
        23 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('messages') WHERE name = 'context_breakdown'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute(
                    "ALTER TABLE messages ADD COLUMN context_breakdown TEXT",
                    &[],
                )
                .await?;
            }
            Ok(())
        }
        24 => {
            db.execute("CREATE TABLE IF NOT EXISTS queued_prompts (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, revision INTEGER NOT NULL DEFAULT 1, content TEXT NOT NULL, created_at INTEGER NOT NULL, guidance INTEGER NOT NULL DEFAULT 0)", &[]).await?;
            db.execute("CREATE INDEX IF NOT EXISTS idx_queued_prompts_session ON queued_prompts(session_id, id)", &[]).await?;
            Ok(())
        }
        25 => {
            db.execute("CREATE TABLE IF NOT EXISTS todo_updates (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, anchor_message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE, plan TEXT NOT NULL, created_at INTEGER NOT NULL)", &[]).await?;
            db.execute("CREATE INDEX IF NOT EXISTS idx_todo_updates_session ON todo_updates(session_id, id)", &[]).await?;
            Ok(())
        }
        26 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('tool_steps') WHERE name = 'timing'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute("ALTER TABLE tool_steps ADD COLUMN timing TEXT", &[])
                    .await?;
            }
            Ok(())
        }
        27 => {
            for column in ["pinned", "archived", "auto_title", "title_revision"] {
                if db
                    .execute(
                        "SELECT 1 FROM pragma_table_info('sessions') WHERE name = ?",
                        &[crate::db::DbValue::Text(column.into())],
                    )
                    .await?
                    .rows
                    .is_empty()
                {
                    db.execute(
                        &format!(
                            "ALTER TABLE sessions ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0"
                        ),
                        &[],
                    )
                    .await?;
                }
            }
            db.execute("CREATE INDEX IF NOT EXISTS idx_sessions_owner_project_archived ON sessions(user_id, project_id, archived)", &[]).await?;
            Ok(())
        }
        28 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('tool_steps') WHERE name = 'execution_order'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute(
                    "ALTER TABLE tool_steps ADD COLUMN execution_order INTEGER",
                    &[],
                )
                .await?;
                db.execute(
                    "UPDATE tool_steps SET execution_order = id WHERE checkpoint IS NOT NULL",
                    &[],
                )
                .await?;
            }
            db.execute("CREATE TABLE IF NOT EXISTS task_runs (session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, task_id TEXT NOT NULL, anchor_message_id INTEGER NOT NULL, snapshot TEXT NOT NULL, PRIMARY KEY(session_id, task_id))", &[]).await?;
            db.execute("CREATE INDEX IF NOT EXISTS idx_task_runs_anchor ON task_runs(session_id, anchor_message_id)", &[]).await?;
            Ok(())
        }
        29 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('user_settings') WHERE name = 'revision'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute(
                    "ALTER TABLE user_settings ADD COLUMN revision INTEGER NOT NULL DEFAULT 0",
                    &[],
                )
                .await?;
            }
            Ok(())
        }
        30 => {
            db.execute("CREATE TABLE IF NOT EXISTS session_goals (session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE, goal TEXT NOT NULL)", &[]).await?;
            Ok(())
        }
        31 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('connections') WHERE name = 'tool_selection'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute("ALTER TABLE connections ADD COLUMN tool_selection TEXT NOT NULL DEFAULT '\"all\"'", &[]).await?;
            }
            Ok(())
        }
        33 => {
            db.execute(
                "CREATE TABLE IF NOT EXISTS project_memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                title TEXT NOT NULL, content TEXT NOT NULL,
                revision INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL)",
                &[],
            )
            .await?;
            db.execute("CREATE INDEX IF NOT EXISTS idx_project_memories ON project_memories(user_id, project_id)", &[]).await?;
            Ok(())
        }
        34 => scope_system_prompts(db).await,
        35 => {
            for sql in [
                "CREATE TABLE IF NOT EXISTS scheduled_tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, revision INTEGER NOT NULL DEFAULT 1, draft TEXT NOT NULL, enabled INTEGER NOT NULL, next_run INTEGER, host_id TEXT NOT NULL, path TEXT)",
                "CREATE INDEX IF NOT EXISTS idx_scheduled_due ON scheduled_tasks(host_id, enabled, next_run)",
                "CREATE TABLE IF NOT EXISTS scheduled_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL REFERENCES scheduled_tasks(id) ON DELETE CASCADE, due_at INTEGER NOT NULL, status TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '', permission_id TEXT, queued_id INTEGER REFERENCES queued_prompts(id) ON DELETE SET NULL, message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL, claimed_until INTEGER NOT NULL DEFAULT 0, UNIQUE(task_id, due_at))",
                "CREATE TABLE IF NOT EXISTS execution_hosts (id TEXT PRIMARY KEY, name TEXT NOT NULL, last_seen INTEGER NOT NULL)",
                "CREATE TABLE IF NOT EXISTS session_run_leases (session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE, token TEXT NOT NULL, expires_at INTEGER NOT NULL)",
            ] {
                db.execute(sql, &[]).await?;
            }
            Ok(())
        }
        36 => {
            db.execute(
                "CREATE TABLE IF NOT EXISTS session_title_activity (
                session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                user_turns INTEGER NOT NULL, updated_at INTEGER NOT NULL)",
                &[],
            )
            .await?;
            db.execute(
                "CREATE TABLE IF NOT EXISTS assistance_cache (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                request TEXT NOT NULL, result TEXT NOT NULL, created_at INTEGER NOT NULL,
                UNIQUE(user_id, request))",
                &[],
            )
            .await?;
            Ok(())
        }
        37 => {
            let columns = db
                .execute(
                    "SELECT 1 FROM pragma_table_info('project_memories') WHERE name = 'auto_title'",
                    &[],
                )
                .await?;
            if columns.rows.is_empty() {
                db.execute(
                    "ALTER TABLE project_memories ADD COLUMN auto_title INTEGER NOT NULL DEFAULT 0",
                    &[],
                )
                .await?;
            }
            Ok(())
        }
        38 => {
            if !db
                .execute(
                    "SELECT 1 FROM pragma_table_info('scheduled_runs') WHERE name='session_id'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                return Ok(());
            }
            // Rebuild together so sessionless task definitions retain run history.
            // Runs point to their actual session, independently of target policy.
            for sql in [
                "CREATE TABLE IF NOT EXISTS scheduled_tasks_next (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE, session_id INTEGER REFERENCES sessions(id) ON DELETE CASCADE, revision INTEGER NOT NULL DEFAULT 1, draft TEXT NOT NULL, enabled INTEGER NOT NULL, next_run INTEGER, host_id TEXT NOT NULL, path TEXT)",
                "INSERT INTO scheduled_tasks_next SELECT * FROM scheduled_tasks",
                "CREATE TABLE IF NOT EXISTS scheduled_runs_next (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL REFERENCES scheduled_tasks_next(id) ON DELETE CASCADE, due_at INTEGER NOT NULL, status TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '', permission_id TEXT, queued_id INTEGER REFERENCES queued_prompts(id) ON DELETE SET NULL, message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL, claimed_until INTEGER NOT NULL DEFAULT 0, session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL, UNIQUE(task_id, due_at))",
                "INSERT INTO scheduled_runs_next SELECT r.*, t.session_id FROM scheduled_runs r JOIN scheduled_tasks t ON t.id=r.task_id",
                "DROP TABLE scheduled_runs",
                "DROP TABLE scheduled_tasks",
                "ALTER TABLE scheduled_tasks_next RENAME TO scheduled_tasks",
                "ALTER TABLE scheduled_runs_next RENAME TO scheduled_runs",
                "CREATE INDEX IF NOT EXISTS idx_scheduled_due ON scheduled_tasks(host_id, enabled, next_run)",
            ] {
                db.execute(sql, &[]).await?;
            }
            Ok(())
        }
        39 => {
            db.execute("CREATE TABLE IF NOT EXISTS project_skills (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, name TEXT NOT NULL, draft TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL, UNIQUE(user_id, project_id, name))", &[]).await?;
            Ok(())
        }
        40 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('scheduled_runs') WHERE name='goal_revision'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute(
                    "ALTER TABLE scheduled_runs ADD COLUMN goal_revision INTEGER",
                    &[],
                )
                .await?;
            }
            db.execute("CREATE TABLE IF NOT EXISTS goal_workers (session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE, task_id INTEGER NOT NULL UNIQUE REFERENCES scheduled_tasks(id) ON DELETE CASCADE, revision INTEGER NOT NULL, turns INTEGER NOT NULL DEFAULT 0, stalls INTEGER NOT NULL DEFAULT 0)", &[]).await?;
            Ok(())
        }
        41 => {
            db.execute(
                "CREATE TABLE IF NOT EXISTS host_operations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                request_id TEXT NOT NULL, target TEXT NOT NULL, state TEXT NOT NULL,
                data TEXT NOT NULL, UNIQUE(user_id, session_id, request_id))",
                &[],
            )
            .await?;
            db.execute("CREATE INDEX IF NOT EXISTS idx_host_operations_target ON host_operations(target, state)", &[]).await?;
            Ok(())
        }
        42 => {
            db.execute("CREATE TABLE IF NOT EXISTS agent_questions (session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, tool_call_id TEXT NOT NULL, step_id INTEGER NOT NULL REFERENCES tool_steps(id) ON DELETE CASCADE, anchor_message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE, request TEXT NOT NULL, reply TEXT, created_at INTEGER NOT NULL, PRIMARY KEY(session_id, tool_call_id))", &[]).await?;
            Ok(())
        }
        43 => {
            db.execute("CREATE TABLE IF NOT EXISTS monitors (task_id INTEGER PRIMARY KEY REFERENCES scheduled_tasks(id) ON DELETE CASCADE, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, interval_seconds INTEGER NOT NULL, remaining INTEGER NOT NULL, expires_at INTEGER NOT NULL)", &[]).await?;
            Ok(())
        }
        44 => {
            db.execute("CREATE TABLE IF NOT EXISTS project_plugins (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, repository TEXT NOT NULL, path TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, enabled INTEGER NOT NULL DEFAULT 1, prepared TEXT NOT NULL, UNIQUE(user_id, project_id, repository, path))", &[]).await?;
            db.execute("CREATE TABLE IF NOT EXISTS project_plugin_skills (plugin_id INTEGER NOT NULL REFERENCES project_plugins(id) ON DELETE CASCADE, skill_id INTEGER NOT NULL UNIQUE REFERENCES project_skills(id) ON DELETE CASCADE, PRIMARY KEY(plugin_id, skill_id))", &[]).await?;
            Ok(())
        }
        45 => {
            db.execute("CREATE TABLE IF NOT EXISTS plugin_defaults (user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, repository TEXT NOT NULL, path TEXT NOT NULL, package TEXT NOT NULL, PRIMARY KEY(user_id,repository,path))", &[]).await?;
            Ok(())
        }
        46 => {
            db.execute("CREATE TABLE IF NOT EXISTS plugin_records (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, project_scope INTEGER NOT NULL, plugin TEXT NOT NULL, collection TEXT NOT NULL, value TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL)", &[]).await?;
            db.execute("CREATE INDEX IF NOT EXISTS plugin_records_scope ON plugin_records(user_id,project_scope,plugin,collection,id)", &[]).await?;
            db.execute("CREATE TRIGGER IF NOT EXISTS delete_project_plugin_records AFTER DELETE ON projects BEGIN DELETE FROM plugin_records WHERE project_scope=OLD.id; END", &[]).await?;
            Ok(())
        }
        47 => {
            db.execute("CREATE TABLE IF NOT EXISTS plugin_execution_grants (token TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, project_scope INTEGER NOT NULL, prepared TEXT NOT NULL, expires_at INTEGER NOT NULL)", &[]).await?;
            db.execute("CREATE INDEX IF NOT EXISTS plugin_execution_grant_expiry ON plugin_execution_grants(expires_at)", &[]).await?;
            Ok(())
        }
        48 => {
            db.execute("DROP TRIGGER IF EXISTS delete_project_plugin_grants", &[])
                .await?;
            // This unreleased rebuild preserves action and job authority markers on replay.
            db.execute("CREATE TABLE IF NOT EXISTS plugin_execution_grants_context (token TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, session_id INTEGER REFERENCES sessions(id) ON DELETE CASCADE, project_scope INTEGER NOT NULL, prepared TEXT NOT NULL, expires_at INTEGER NOT NULL, primary_model TEXT, user_action INTEGER NOT NULL DEFAULT 0, job_id INTEGER, job_lease TEXT)", &[]).await?;
            let primary = db.execute("SELECT 1 FROM pragma_table_info('plugin_execution_grants') WHERE name='primary_model'", &[]).await?;
            let action = db.execute("SELECT 1 FROM pragma_table_info('plugin_execution_grants') WHERE name='user_action'", &[]).await?;
            let primary = if primary.rows.is_empty() {
                "NULL"
            } else {
                "primary_model"
            };
            let action = if action.rows.is_empty() {
                "0"
            } else {
                "user_action"
            };
            let job_id = db.execute("SELECT 1 FROM pragma_table_info('plugin_execution_grants') WHERE name='job_id'", &[]).await?;
            let job_lease = db.execute("SELECT 1 FROM pragma_table_info('plugin_execution_grants') WHERE name='job_lease'", &[]).await?;
            let job_id = if job_id.rows.is_empty() {
                "NULL"
            } else {
                "job_id"
            };
            let job_lease = if job_lease.rows.is_empty() {
                "NULL"
            } else {
                "job_lease"
            };
            db.execute(&format!("INSERT OR IGNORE INTO plugin_execution_grants_context SELECT token,user_id,session_id,project_scope,prepared,expires_at,{primary},{action},{job_id},{job_lease} FROM plugin_execution_grants"), &[]).await?;
            db.execute("DROP TABLE plugin_execution_grants", &[])
                .await?;
            db.execute(
                "ALTER TABLE plugin_execution_grants_context RENAME TO plugin_execution_grants",
                &[],
            )
            .await?;
            db.execute("CREATE INDEX IF NOT EXISTS plugin_execution_grant_expiry ON plugin_execution_grants(expires_at)", &[]).await?;
            db.execute("CREATE TRIGGER IF NOT EXISTS delete_project_plugin_grants AFTER DELETE ON projects BEGIN DELETE FROM plugin_execution_grants WHERE project_scope=OLD.id; END", &[]).await?;
            Ok(())
        }
        49 => {
            let columns = db.execute("SELECT 1 FROM pragma_table_info('plugin_execution_grants') WHERE name='user_action'", &[]).await?;
            if columns.rows.is_empty() {
                db.execute("ALTER TABLE plugin_execution_grants ADD COLUMN user_action INTEGER NOT NULL DEFAULT 0", &[]).await?;
            }
            Ok(())
        }
        50 => {
            db.execute("CREATE TABLE IF NOT EXISTS plugin_jobs (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, project_scope INTEGER NOT NULL, plugin TEXT NOT NULL, job_key TEXT NOT NULL, due_at INTEGER NOT NULL, expires_at INTEGER, event TEXT NOT NULL, payload TEXT NOT NULL, prepared TEXT NOT NULL, context TEXT NOT NULL, host_id TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending', revision INTEGER NOT NULL DEFAULT 1, attempts INTEGER NOT NULL DEFAULT 0, lease TEXT, lease_expires_at INTEGER, detail TEXT, UNIQUE(user_id,project_scope,plugin,job_key))", &[]).await?;
            db.execute("CREATE INDEX IF NOT EXISTS plugin_jobs_due ON plugin_jobs(host_id,state,due_at,id)", &[]).await?;
            db.execute("CREATE TRIGGER IF NOT EXISTS delete_project_plugin_jobs AFTER DELETE ON projects BEGIN DELETE FROM plugin_jobs WHERE project_scope=OLD.id; END", &[]).await?;
            for (name, declaration) in [("job_id", "INTEGER"), ("job_lease", "TEXT")] {
                let columns = db
                    .execute(
                        "SELECT 1 FROM pragma_table_info('plugin_execution_grants') WHERE name=?",
                        &[crate::db::DbValue::Text(name.into())],
                    )
                    .await?;
                if columns.rows.is_empty() {
                    db.execute(
                        &format!(
                            "ALTER TABLE plugin_execution_grants ADD COLUMN {name} {declaration}"
                        ),
                        &[],
                    )
                    .await?;
                }
            }
            Ok(())
        }
        51 => {
            db.execute("CREATE TABLE IF NOT EXISTS plugin_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, project_scope INTEGER NOT NULL, plugin TEXT NOT NULL, run_key TEXT NOT NULL, request TEXT NOT NULL, prepared TEXT NOT NULL, context TEXT NOT NULL, host_id TEXT NOT NULL, session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL, queued_id INTEGER REFERENCES queued_prompts(id) ON DELETE SET NULL, message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL, model TEXT, state TEXT NOT NULL DEFAULT 'pending', revision INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL, lease TEXT, lease_expires_at INTEGER, detail TEXT NOT NULL DEFAULT '', permission_id TEXT, UNIQUE(user_id,project_scope,plugin,run_key))", &[]).await?;
            db.execute(
                "CREATE INDEX IF NOT EXISTS plugin_runs_pending ON plugin_runs(host_id,state,id)",
                &[],
            )
            .await?;
            db.execute("CREATE TRIGGER IF NOT EXISTS delete_project_plugin_runs AFTER DELETE ON projects BEGIN DELETE FROM queued_prompts WHERE id IN (SELECT queued_id FROM plugin_runs WHERE project_scope=OLD.id); DELETE FROM plugin_runs WHERE project_scope=OLD.id; END", &[]).await?;
            db.execute("CREATE TRIGGER IF NOT EXISTS delete_plugin_run_queue AFTER DELETE ON plugin_runs BEGIN DELETE FROM queued_prompts WHERE id=OLD.queued_id; END", &[]).await?;
            db.execute("CREATE TRIGGER IF NOT EXISTS cancel_plugin_run_queue BEFORE DELETE ON queued_prompts BEGIN UPDATE plugin_runs SET state='cancelled',revision=revision+1,detail='Queued prompt removed' WHERE queued_id=OLD.id AND state IN ('pending','leased') AND message_id IS NULL; END", &[]).await?;
            db.execute("CREATE TRIGGER IF NOT EXISTS cancel_session_plugin_runs BEFORE DELETE ON sessions BEGIN UPDATE plugin_runs SET state=CASE WHEN message_id IS NULL THEN 'cancelled' ELSE 'interrupted' END,revision=revision+1,detail='Conversation removed',permission_id=NULL WHERE session_id=OLD.id AND state IN ('pending','leased','running','blocked','cancelling'); END", &[]).await?;
            Ok(())
        }
        52 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('plugin_runs') WHERE name='completion_job'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute("ALTER TABLE plugin_runs ADD COLUMN completion_job INTEGER REFERENCES plugin_jobs(id) ON DELETE SET NULL", &[]).await?;
            }
            // Every terminal transition, including queue/session deletion and recovery,
            // releases the reserved event in the same transaction as the run status.
            db.execute("CREATE TRIGGER IF NOT EXISTS complete_plugin_run_event AFTER UPDATE OF state ON plugin_runs WHEN NEW.state IN ('completed','failed','cancelled','interrupted') AND OLD.state NOT IN ('completed','failed','cancelled','interrupted') BEGIN UPDATE plugin_jobs SET state='pending',revision=revision+1,payload=json_object('run',json_object('id',NEW.id,'revision',NEW.revision,'key',NEW.run_key,'session_id',NEW.session_id,'state',NEW.state,'created_at',NEW.created_at,'detail',NEW.detail,'message_id',NEW.message_id,'permission_id',NEW.permission_id),'data',json(payload)) WHERE id=NEW.completion_job AND state='waiting'; END", &[]).await?;
            db.execute("CREATE TRIGGER IF NOT EXISTS delete_plugin_run_event AFTER DELETE ON plugin_runs BEGIN UPDATE plugin_jobs SET state='cancelled',revision=revision+1 WHERE id=OLD.completion_job AND state='waiting'; END", &[]).await?;
            Ok(())
        }
        53 => {
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('plugin_jobs') WHERE name='scope'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute(
                    "ALTER TABLE plugin_jobs ADD COLUMN scope TEXT NOT NULL DEFAULT 'origin'",
                    &[],
                )
                .await?;
            }
            Ok(())
        }
        54 => {
            for (column, sql) in [
                (
                    "plugin_owner",
                    "ALTER TABLE scheduled_tasks ADD COLUMN plugin_owner TEXT",
                ),
                (
                    "plugin_state",
                    "ALTER TABLE scheduled_tasks ADD COLUMN plugin_state TEXT NOT NULL DEFAULT '{}'",
                ),
                (
                    "plugin_updated_at",
                    "ALTER TABLE scheduled_tasks ADD COLUMN plugin_updated_at INTEGER NOT NULL DEFAULT 0",
                ),
            ] {
                if db.execute(&format!("SELECT 1 FROM pragma_table_info('scheduled_tasks') WHERE name='{column}'"), &[]).await?.rows.is_empty() {
                    db.execute(sql,&[]).await?;
                }
            }
            Ok(())
        }
        55 => {
            db.execute("CREATE TABLE IF NOT EXISTS plugin_task_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL REFERENCES scheduled_tasks(id) ON DELETE CASCADE, history_key TEXT NOT NULL, due_at INTEGER NOT NULL, run_id INTEGER, linked_run INTEGER REFERENCES plugin_runs(id) ON DELETE SET NULL, revision INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL, status TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '', session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL, message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL, permission_id TEXT, UNIQUE(task_id,history_key))",&[]).await?;
            // This is a data reference projection, not task dispatch or recurrence policy.
            // Preserve the last raw status before its history entry is deleted.
            for (name, event, reference) in [
                (
                    "project_plugin_task_run",
                    "AFTER UPDATE OF state,detail,session_id,message_id,permission_id ON plugin_runs",
                    "NEW",
                ),
                (
                    "snapshot_deleted_plugin_task_run",
                    "BEFORE DELETE ON plugin_runs",
                    "OLD",
                ),
            ] {
                db.execute(&format!("CREATE TRIGGER IF NOT EXISTS {name} {event} BEGIN UPDATE plugin_task_runs SET revision=revision+1,status=CASE {reference}.state WHEN 'pending' THEN 'queued' WHEN 'leased' THEN 'claimed' WHEN 'completed' THEN 'complete' ELSE {reference}.state END,detail={reference}.detail,session_id={reference}.session_id,message_id={reference}.message_id,permission_id={reference}.permission_id WHERE linked_run={reference}.id; END"),&[]).await?;
            }
            Ok(())
        }
        56 => {
            db.execute("CREATE TABLE IF NOT EXISTS plugin_background (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE, project_scope INTEGER NOT NULL, plugin TEXT NOT NULL, host_id TEXT NOT NULL, prepared TEXT NOT NULL, next_due INTEGER NOT NULL, UNIQUE(user_id,project_scope,plugin))",&[]).await?;
            db.execute("CREATE INDEX IF NOT EXISTS plugin_background_due ON plugin_background(host_id,next_due,id)",&[]).await?;
            if db
                .execute(
                    "SELECT 1 FROM pragma_table_info('plugin_jobs') WHERE name='background_id'",
                    &[],
                )
                .await?
                .rows
                .is_empty()
            {
                db.execute(
                    "ALTER TABLE plugin_jobs ADD COLUMN background_id INTEGER",
                    &[],
                )
                .await?;
            }
            Ok(())
        }
        other => Err(StorageError::Db(format!("unknown migration step {other}"))),
    }
}

/// Read the database's current schema version.
async fn read_user_version<D: Db>(db: &D) -> Result<i64, StorageError> {
    let res = db.execute("PRAGMA user_version", &[]).await?;
    Ok(res
        .rows
        .first()
        .map(|row| row.get_int(0))
        .transpose()?
        .unwrap_or(0))
}

// Rebuild the global name constraint as (user_id, name). Preserve the first
// account's IDs, copy the previously shared library to other existing accounts,
// and restore session references after DROP TABLE applies ON DELETE SET NULL.
async fn scope_system_prompts<D: Db>(db: &D) -> Result<(), StorageError> {
    if !db
        .execute(
            "SELECT 1 FROM pragma_table_info('system_prompts') WHERE name = 'user_id'",
            &[],
        )
        .await?
        .rows
        .is_empty()
    {
        return Ok(());
    }
    for sql in [
        "CREATE TABLE system_prompts_owned (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL, content TEXT NOT NULL,
            user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
            UNIQUE(user_id, name))",
        "INSERT INTO system_prompts_owned (id, name, content, user_id)
            SELECT id, name, content, (SELECT MIN(id) FROM users) FROM system_prompts",
        "INSERT INTO system_prompts_owned (name, content, user_id)
            SELECT p.name, p.content, u.id FROM system_prompts p CROSS JOIN users u
            WHERE u.id != (SELECT MIN(id) FROM users)",
        "CREATE TABLE system_prompt_session_refs AS
            SELECT id AS session_id, system_prompt_id AS prompt_id FROM sessions
            WHERE system_prompt_id IS NOT NULL",
        "DROP TABLE system_prompts",
        "ALTER TABLE system_prompts_owned RENAME TO system_prompts",
        "UPDATE sessions SET system_prompt_id = (
            SELECT owned.id FROM system_prompt_session_refs r
            JOIN system_prompts original ON original.id = r.prompt_id
            JOIN system_prompts owned ON owned.name = original.name
                AND owned.user_id IS COALESCE(sessions.user_id, (SELECT MIN(id) FROM users))
            WHERE r.session_id = sessions.id)
            WHERE id IN (SELECT session_id FROM system_prompt_session_refs)",
        "UPDATE user_settings SET value = CAST((
            SELECT owned.id FROM system_prompts original
            JOIN system_prompts owned ON owned.name = original.name
                AND owned.user_id = user_settings.user_id
            WHERE original.id = CAST(user_settings.value AS INTEGER)) AS TEXT)
            WHERE key = 'default_prompt' AND value != ''
                AND EXISTS (SELECT 1 FROM system_prompts WHERE id = CAST(user_settings.value AS INTEGER))",
        "DROP TABLE system_prompt_session_refs",
    ] {
        db.execute(sql, &[]).await?;
    }
    Ok(())
}

/// Apply all pending migration steps up to [`SCHEMA_VERSION`].
///
/// `probe` answers "does this mount-relative directory exist?" for the
/// steps that need the filesystem (step 11); pass `&|_| false` when there
/// is no filesystem.
pub async fn apply<D: Db>(
    db: &D,
    probe: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Result<(), StorageError> {
    let version = read_user_version(db).await?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if version > SCHEMA_VERSION {
        return Err(StorageError::Db(format!(
            "database schema version {version} is newer than this build ({SCHEMA_VERSION})"
        )));
    }
    db.transaction(|tx| async move {
        // Re-read inside the transaction: a concurrent first request may
        // have already migrated while we waited for the write lock.
        let version = read_user_version(&tx).await?;
        if version > SCHEMA_VERSION {
            return Err(StorageError::Db(format!(
                "database schema version {version} is newer than this build ({SCHEMA_VERSION})"
            )));
        }
        for step in (version + 1)..=SCHEMA_VERSION {
            apply_step(&tx, step, probe).await?;
        }
        // PRAGMA takes no bound parameters, so the version is formatted in.
        tx.execute(&format!("PRAGMA user_version = {SCHEMA_VERSION}"), &[])
            .await?;
        Ok(())
    })
    .await
}

/// Run steps `1..=target` without touching `user_version`, simulating a
/// pre-versioning database that an old build already migrated.
#[cfg(test)]
pub(crate) async fn apply_through<D: Db>(db: &D, target: i64) -> Result<(), StorageError> {
    for step in 1..=target {
        apply_step(db, step, &|_| false).await?;
    }
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` is not idempotent, so probe first.
async fn add_message_usage_columns<D: Db>(db: &D) -> Result<(), StorageError> {
    for column in [
        "prompt_tokens",
        "completion_tokens",
        "eval_duration_ms",
        "usage_estimated",
    ] {
        let res = db
            .execute(
                &format!("SELECT 1 FROM pragma_table_info('messages') WHERE name = '{column}'"),
                &[],
            )
            .await?;
        if res.rows.is_empty() {
            db.execute(
                &format!("ALTER TABLE messages ADD COLUMN {column} INTEGER"),
                &[],
            )
            .await?;
        }
    }
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` is not idempotent, so probe first.
async fn add_connection_context_limit_column<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('connections') WHERE name = 'context_limit'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute(
            "ALTER TABLE connections ADD COLUMN context_limit INTEGER",
            &[],
        )
        .await?;
    }
    Ok(())
}

/// Migrate existing global preferences (theme, default_connection, default_prompt)
/// into user_settings for existing users.
async fn migrate_legacy_settings_to_user_settings<D: Db>(db: &D) -> Result<(), StorageError> {
    db.execute(
        "INSERT OR IGNORE INTO user_settings (user_id, key, value)
         SELECT u.id, s.key, s.value
         FROM users u
         CROSS JOIN settings s
         WHERE s.key IN ('theme', 'default_connection', 'default_prompt')",
        &[],
    )
    .await?;
    Ok(())
}

/// Unique index backing `create_project`'s dedup: one project per
/// (owner, mode, path). Pathless projects are excluded, and SQLite treats
/// NULL `user_id` values as distinct in unique indexes.
async fn create_project_path_index<D: Db>(db: &D) -> Result<(), StorageError> {
    db.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_owner_path
         ON projects (user_id, mode, path) WHERE path IS NOT NULL",
        &[],
    )
    .await?;
    Ok(())
}

/// Rewrite pre-`/workspace`-mount Docker project paths. Before the mount,
/// remote paths were stored relative to the container root
/// (`workspace/foo`); now the mount root *is* `/workspace`, so those rows
/// point at `/workspace/workspace/foo`. A row is rewritten to its
/// `workspace/`-stripped form only when the stale directory is gone and
/// the stripped one exists on the mount (so a local install that really
/// has a `workspace/` folder is left alone). Rows whose owner already has
/// a project at the stripped path are skipped (the unique index).
async fn rewrite_docker_workspace_paths<D: Db>(
    db: &D,
    probe: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Result<(), StorageError> {
    use crate::db::DbValue;
    let res = db
        .execute(
            "SELECT id, path FROM projects WHERE mode = 'remote' AND path LIKE 'workspace/%'",
            &[],
        )
        .await?;

    for row in res.rows {
        let id = row.get_int(0)?;
        let path = row.get_text(1)?.to_string();
        let Some(stripped) = path.strip_prefix("workspace/") else {
            continue;
        };
        if probe(&path) || !probe(stripped) {
            continue;
        }
        match db
            .execute(
                "UPDATE projects SET path = ? WHERE id = ?",
                &[DbValue::Text(stripped.to_string()), DbValue::Int(id)],
            )
            .await
        {
            Ok(_) => {}
            // Another project of the same owner already has the stripped
            // path (unique index); leave this row as is.
            Err(StorageError::Conflict(_)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Deduplicate any historical duplicate projects sharing (user_id, mode, path),
/// reassigning their sessions to the preserved project before removing the duplicate.
async fn dedup_duplicate_projects<D: Db>(db: &D) -> Result<(), StorageError> {
    use crate::db::DbValue;
    let res = db
        .execute(
            "SELECT COALESCE(user_id, 0), mode, path, MIN(id) as keep_id, COUNT(*) as cnt
             FROM projects
             WHERE path IS NOT NULL
             GROUP BY COALESCE(user_id, 0), mode, path
             HAVING cnt > 1",
            &[],
        )
        .await?;

    for row in res.rows {
        let user_id = row.get_int(0)?;
        let mode = row.get_text(1)?.to_string();
        let path = row.get_text(2)?.to_string();
        let keep_id = row.get_int(3)?;

        let dups = db
            .execute(
                "SELECT id FROM projects
                 WHERE COALESCE(user_id, 0) = ? AND mode = ? AND path = ? AND id != ?",
                &[
                    DbValue::Int(user_id),
                    DbValue::Text(mode),
                    DbValue::Text(path),
                    DbValue::Int(keep_id),
                ],
            )
            .await?;

        for dup_row in dups.rows {
            let dup_id = dup_row.get_int(0)?;
            db.execute(
                "UPDATE sessions SET project_id = ? WHERE project_id = ?",
                &[DbValue::Int(keep_id), DbValue::Int(dup_id)],
            )
            .await?;
            db.execute("DELETE FROM projects WHERE id = ?", &[DbValue::Int(dup_id)])
                .await?;
        }
    }
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` is not idempotent, so probe first. The
/// `users` table is created above before this runs, so the FK target exists.
async fn add_project_user_column<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('projects') WHERE name = 'user_id'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute(
            "ALTER TABLE projects ADD COLUMN user_id INTEGER REFERENCES users(id)",
            &[],
        )
        .await?;
    }
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` is not idempotent, so probe first.
async fn add_session_user_column<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'user_id'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute(
            "ALTER TABLE sessions ADD COLUMN user_id INTEGER REFERENCES users(id)",
            &[],
        )
        .await?;
    }
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` is not idempotent, so probe first.
async fn add_session_system_prompt_column<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'system_prompt_id'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute(
            "ALTER TABLE sessions ADD COLUMN system_prompt_id INTEGER
             REFERENCES system_prompts(id) ON DELETE SET NULL",
            &[],
        )
        .await?;
    }
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` is not idempotent, so probe first. The
/// `projects` table is created above before this runs, so the FK target
/// exists.
async fn add_session_project_column<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'project_id'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute(
            "ALTER TABLE sessions ADD COLUMN project_id INTEGER
             REFERENCES projects(id) ON DELETE CASCADE",
            &[],
        )
        .await?;
    }
    Ok(())
}

async fn create_login_failures<D: Db>(db: &D) -> Result<(), StorageError> {
    db.execute(
        "CREATE TABLE IF NOT EXISTS login_failures (
            username TEXT PRIMARY KEY,
            failures INTEGER NOT NULL,
            last_failed_at INTEGER NOT NULL
        )",
        &[],
    )
    .await?;
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` is not idempotent, so probe first.
async fn add_user_token_epoch<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('users') WHERE name = 'token_epoch'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute(
            "ALTER TABLE users ADD COLUMN token_epoch INTEGER NOT NULL DEFAULT 0",
            &[],
        )
        .await?;
    }
    Ok(())
}

async fn add_message_tool_calls<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('messages') WHERE name = 'tool_calls'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute("ALTER TABLE messages ADD COLUMN tool_calls TEXT", &[])
            .await?;
    }
    Ok(())
}

async fn add_connection_tool_stream_unsupported<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('connections') WHERE name = 'tool_stream_unsupported'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute(
            "ALTER TABLE connections ADD COLUMN tool_stream_unsupported INTEGER NOT NULL DEFAULT 0",
            &[],
        )
        .await?;
    }
    Ok(())
}

async fn add_connection_tool_stream_revision<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('connections') WHERE name = 'tool_stream_revision'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute(
            "ALTER TABLE connections ADD COLUMN tool_stream_revision INTEGER NOT NULL DEFAULT 0",
            &[],
        )
        .await?;
    }
    Ok(())
}

async fn add_cancel_requested_at_ms<D: Db>(db: &D) -> Result<(), StorageError> {
    let res = db
        .execute(
            "SELECT 1 FROM pragma_table_info('run_cancels') WHERE name = 'requested_at_ms'",
            &[],
        )
        .await?;
    if res.rows.is_empty() {
        db.execute(
            "ALTER TABLE run_cancels ADD COLUMN requested_at_ms INTEGER NOT NULL DEFAULT 0",
            &[],
        )
        .await?;
    }
    Ok(())
}

async fn create_pending_edits<D: Db>(db: &D) -> Result<(), StorageError> {
    db.execute(
        "CREATE TABLE IF NOT EXISTS pending_edits (
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            diff TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK (revision > 0),
            decision TEXT NOT NULL CHECK (decision IN ('pending', 'accepted', 'rejected')),
            PRIMARY KEY (project_id, path)
        )",
        &[],
    )
    .await?;
    let columns = db
        .execute(
            "SELECT 1 FROM pragma_table_info('tool_steps') WHERE name = 'completion_applied'",
            &[],
        )
        .await?;
    if columns.rows.is_empty() {
        db.execute(
            "ALTER TABLE tool_steps ADD COLUMN completion_applied INTEGER NOT NULL DEFAULT 0",
            &[],
        )
        .await?;
        // Historical completions remain history even if a client replays them.
        db.execute(
            "UPDATE tool_steps SET completion_applied = 1 WHERE ok IS NOT NULL",
            &[],
        )
        .await?;
    }
    Ok(())
}

async fn create_model_settings<D: Db>(db: &D) -> Result<(), StorageError> {
    db.execute(
        "CREATE TABLE IF NOT EXISTS model_settings (
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        server_id INTEGER NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
        model TEXT NOT NULL,
        settings TEXT NOT NULL,
        PRIMARY KEY (user_id, server_id, model)
    )",
        &[],
    )
    .await?;
    db.execute(
        "CREATE TABLE IF NOT EXISTS server_transport (
        server_id INTEGER PRIMARY KEY REFERENCES connections(id) ON DELETE CASCADE,
        settings TEXT NOT NULL
    )",
        &[],
    )
    .await?;
    // Preserve the old model-specific context setting for every existing user.
    db.execute(
        "INSERT OR IGNORE INTO model_settings (user_id, server_id, model, settings)
        SELECT users.id, connections.id, connections.model,
        json_object('context_limit', connections.context_limit)
        FROM users CROSS JOIN connections WHERE connections.model IS NOT NULL
        AND connections.context_limit IS NOT NULL",
        &[],
    )
    .await?;
    Ok(())
}

async fn share_model_profiles<D: Db>(db: &D) -> Result<(), StorageError> {
    use crate::db::DbValue;
    db.execute(
        "CREATE TABLE IF NOT EXISTS model_profiles (
        server_id INTEGER NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
        model TEXT NOT NULL, settings TEXT NOT NULL, PRIMARY KEY (server_id, model))",
        &[],
    )
    .await?;
    let legacy = db
        .execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'model_settings'",
            &[],
        )
        .await?;
    if !legacy.rows.is_empty() {
        // Resolve legacy conflicts deterministically: the earliest account's profile wins.
        let rows = db.execute("SELECT user_id, server_id, model, settings FROM model_settings ORDER BY user_id, server_id, model", &[]).await?;
        for row in rows.rows {
            let user = row.get_int(0)?;
            let mut profile: openwebide_core::ModelSettings =
                serde_json::from_str(row.get_text(3)?)
                    .map_err(|e| StorageError::Db(e.to_string()))?;
            let old = db
                .execute(
                    "SELECT value FROM user_settings WHERE user_id = ? AND key = 'model_defaults'",
                    &[DbValue::Int(user)],
                )
                .await?;
            let mut defaults: serde_json::Value = old
                .rows
                .first()
                .map(|row| {
                    serde_json::from_str(row.get_text(0)?)
                        .map_err(|e| StorageError::Db(e.to_string()))
                })
                .transpose()?
                .unwrap_or_else(|| serde_json::json!({}));
            if profile.auto_compact_threshold.is_none() {
                profile.auto_compact_threshold = defaults
                    .get("auto_compact_threshold")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u8::try_from(value).ok());
            }
            if let Some(fast) = profile.fast.take()
                && defaults.get("fast").is_none_or(serde_json::Value::is_null)
            {
                defaults["fast"] =
                    serde_json::to_value(fast).map_err(|e| StorageError::Db(e.to_string()))?;
                db.execute("INSERT INTO user_settings (user_id, key, value) VALUES (?, 'model_defaults', ?) ON CONFLICT(user_id, key) DO UPDATE SET value = excluded.value", &[DbValue::Int(user), DbValue::Text(defaults.to_string())]).await?;
            }
            db.execute("INSERT OR IGNORE INTO model_profiles (server_id, model, settings) VALUES (?, ?, ?)", &[DbValue::Int(row.get_int(1)?), DbValue::Text(row.get_text(2)?.into()), DbValue::Text(serde_json::to_string(&profile).map_err(|e| StorageError::Db(e.to_string()))?)]).await?;
        }
        db.execute("DROP TABLE model_settings", &[]).await?;
    }
    // Older users could configure a threshold without creating an explicit model profile.
    let defaults = db
        .execute(
            "SELECT value FROM user_settings WHERE key = 'model_defaults' ORDER BY user_id",
            &[],
        )
        .await?;
    for row in defaults.rows {
        let value: serde_json::Value =
            serde_json::from_str(row.get_text(0)?).map_err(|e| StorageError::Db(e.to_string()))?;
        let Some(threshold) = value
            .get("auto_compact_threshold")
            .and_then(serde_json::Value::as_u64)
        else {
            continue;
        };
        let Some(primary) = value.get("primary").and_then(|value| {
            serde_json::from_value::<openwebide_core::ModelSelection>(value.clone()).ok()
        }) else {
            continue;
        };
        db.execute("INSERT OR IGNORE INTO model_profiles (server_id, model, settings) SELECT ?, ?, ? WHERE EXISTS (SELECT 1 FROM connections WHERE id = ?)", &[DbValue::Int(primary.server_id), DbValue::Text(primary.model), DbValue::Text(serde_json::json!({"auto_compact_threshold":threshold}).to_string()), DbValue::Int(primary.server_id)]).await?;
    }
    db.execute("INSERT OR IGNORE INTO settings (key, value) SELECT key, value FROM user_settings WHERE key GLOB 'model_detection_*' ORDER BY user_id", &[]).await?;
    db.execute(
        "DELETE FROM user_settings WHERE key GLOB 'model_detection_*'",
        &[],
    )
    .await?;
    Ok(())
}
