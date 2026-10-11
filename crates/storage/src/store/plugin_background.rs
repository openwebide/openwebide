//! General background event subscriptions; reconciliation behavior belongs to source.
use super::*;
use openwebide_core::plugins::{
    PreparedPlugin,
    execution::PluginExecutionContext,
    jobs::{JobRequest, JobScope},
};

impl<D: Db> Store<D> {
    pub(super) async fn sync_plugin_background(
        &self,
        user: UserId,
        project: Option<i64>,
        prepared: &PreparedPlugin,
        enabled: bool,
        now: i64,
    ) -> Result<(), StorageError> {
        let scope = [
            DbValue::Int(user.get()),
            DbValue::Int(project.unwrap_or(0)),
            DbValue::Text(prepared.storage_namespace()),
        ];
        if !enabled || prepared.manifest.contributions.background.is_none() {
            self.db.execute("DELETE FROM plugin_background WHERE user_id=? AND project_scope=? AND plugin=?", &scope).await?;
        } else {
            let json = serde_json::to_string(prepared)
                .map_err(|error| StorageError::Db(error.to_string()))?;
            let mut params = scope.to_vec();
            params.extend([
                project.map_or(DbValue::Null, DbValue::Int),
                DbValue::Text(prepared.host_id.clone()),
                DbValue::Text(json),
                DbValue::Int(now),
            ]);
            self.db.execute("INSERT INTO plugin_background(user_id,project_scope,plugin,project_id,host_id,prepared,next_due) VALUES(?,?,?,?,?,?,?) ON CONFLICT(user_id,project_scope,plugin) DO UPDATE SET host_id=excluded.host_id,prepared=excluded.prepared,next_due=CASE WHEN plugin_background.prepared<>excluded.prepared THEN excluded.next_due ELSE plugin_background.next_due END", &params).await?;
        }
        // Replacing/disabling a subscription revokes its pending actors and callback grants.
        // User-scheduled jobs continue to retain their historical source snapshots.
        self.db.execute("UPDATE plugin_jobs SET state='cancelled',revision=revision+1 WHERE user_id=? AND project_scope=? AND plugin=? AND background_id IS NOT NULL AND state IN ('pending','leased') AND NOT EXISTS(SELECT 1 FROM plugin_background b WHERE b.id=plugin_jobs.background_id AND b.prepared=plugin_jobs.prepared)", &scope).await?;
        Ok(())
    }

    /// Emit a bounded fair batch on the daemon's first queue page. Deadlines persist across restarts.
    pub(super) async fn enqueue_plugin_background(
        &self,
        host: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        let rows = self.db.execute("SELECT id,user_id,project_id,prepared FROM plugin_background WHERE host_id=? AND next_due<=? ORDER BY next_due,id LIMIT 8", &[DbValue::Text(host.into()), DbValue::Int(now)]).await?;
        for row in rows.rows {
            let id = row.get_int(0)?;
            let user = UserId::new(row.get_int(1)?);
            let prepared: PreparedPlugin = serde_json::from_str(row.get_text(3)?)
                .map_err(|error| StorageError::Db(error.to_string()))?;
            let context = PluginExecutionContext {
                project_id: row.get_int_opt(2),
                session_id: None,
                primary: None,
                user_action: false,
            };
            let Some(background) = &prepared.manifest.contributions.background else {
                continue;
            };
            let next = now
                .checked_add(background.interval_seconds)
                .ok_or_else(|| {
                    StorageError::InvalidRequest("Invalid background deadline".into())
                })?;
            let prefix = format!("background:{id}:");
            // Superseded or disabled subscriptions stop here; historical user-scheduled jobs retain their pins.
            if !self
                .plugin_namespace_enabled(user, &context, &prepared)
                .await?
            {
                self.db
                    .execute(
                        "DELETE FROM plugin_background WHERE id=?",
                        &[DbValue::Int(id)],
                    )
                    .await?;
                continue;
            }
            // These entries are host-owned bookkeeping, not plugin-requested history.
            // Keep the current delivery while removing terminal ticks before the next one.
            self.db.execute("DELETE FROM plugin_execution_grants WHERE job_id IN (SELECT id FROM plugin_jobs WHERE background_id=? AND state IN ('completed','failed','cancelled','expired'))", &[DbValue::Int(id)]).await?;
            self.db.execute("DELETE FROM plugin_jobs WHERE background_id=? AND state IN ('completed','failed','cancelled','expired')", &[DbValue::Int(id)]).await?;
            let active = self.db.execute("SELECT 1 FROM plugin_jobs WHERE background_id=? AND state IN ('pending','leased') LIMIT 1", &[DbValue::Int(id)]).await?;
            if active.rows.is_empty() {
                let created = self
                    .plugin_jobs_in_transaction(
                        user,
                        &prepared,
                        &context,
                        &JobRequest::Schedule {
                            scope: JobScope::Project,
                            key: format!("{prefix}{now}"),
                            due_at: now,
                            expires_at: None,
                            event: background.event.clone(),
                            payload: serde_json::Value::Null,
                        },
                        now,
                    )
                    .await?;
                self.db
                    .execute(
                        "UPDATE plugin_jobs SET background_id=? WHERE id=?",
                        &[DbValue::Int(id), DbValue::Int(created.jobs[0].id)],
                    )
                    .await?;
            }
            self.db
                .execute(
                    "UPDATE plugin_background SET next_due=? WHERE id=?",
                    &[DbValue::Int(next), DbValue::Int(id)],
                )
                .await?;
        }
        Ok(())
    }
}
