//! Durable queue primitives. No cron, prompt, monitor or recurrence policy lives here.
use super::*;
use crate::db::QueryRow;
use openwebide_core::plugins::{
    PreparedPlugin, default_bindings, execution::PluginExecutionContext, jobs::*,
};
use serde::{Serialize, de::DeserializeOwned};

const COLUMNS: &str = "id,revision,job_key,due_at,expires_at,event,payload,state,attempts,detail";
fn encode(value: &impl Serialize) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| StorageError::Db(error.to_string()))
}
fn decode<T: DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_str(value).map_err(|error| StorageError::Db(error.to_string()))
}
fn job(row: &QueryRow) -> Result<Job, StorageError> {
    Ok(Job {
        id: row.get_int(0)?,
        revision: row.get_int(1)?,
        key: row.get_text(2)?.into(),
        due_at: row.get_int(3)?,
        expires_at: row.get_int_opt(4),
        event: row.get_text(5)?.into(),
        payload: decode(row.get_text(6)?)?,
        state: serde_json::from_value(serde_json::Value::String(row.get_text(7)?.into()))
            .map_err(|error| StorageError::Db(error.to_string()))?,
        attempts: row.get_int(8)?,
        detail: row.get_text_opt(9).map(str::to_owned),
    })
}
fn token(value: &str) -> Result<(), StorageError> {
    if value.len() != 32 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(StorageError::InvalidRequest(
            "Invalid job lease token".into(),
        ));
    }
    Ok(())
}
fn lease_expiry(now: i64) -> Result<i64, StorageError> {
    if now < 0 {
        return Err(StorageError::InvalidRequest("Invalid job clock".into()));
    }
    now.checked_add(JOB_LEASE_SECONDS)
        .ok_or_else(|| StorageError::InvalidRequest("Invalid job lease expiry".into()))
}

impl<D: Db> Store<D> {
    /// Chained jobs retain the originating conversation/model without receiving chat authority.
    pub(super) async fn plugin_job_origin_context(
        &self,
        user: UserId,
        grant: &str,
        context: &PluginExecutionContext,
    ) -> Result<PluginExecutionContext, StorageError> {
        let rows = self.db.execute("SELECT jobs.context FROM plugin_execution_grants AS grants JOIN plugin_jobs AS jobs ON jobs.id=grants.job_id WHERE grants.token=? AND grants.user_id=? AND jobs.user_id=?", &[DbValue::Text(grant.into()),DbValue::Int(user.get()),DbValue::Int(user.get())]).await?;
        let Some(row) = rows.rows.first() else {
            return Ok(context.clone());
        };
        let mut origin: PluginExecutionContext = decode(row.get_text(0)?)?;
        if origin.project_id != context.project_id || origin.primary != context.primary {
            return Err(StorageError::Conflict(
                "Job execution context changed".into(),
            ));
        }
        origin.user_action = false;
        Ok(origin)
    }
    pub(super) async fn plugin_namespace_enabled(
        &self,
        user: UserId,
        context: &PluginExecutionContext,
        plugin: &PreparedPlugin,
    ) -> Result<bool, StorageError> {
        match self.validate_plugin_context(user, context).await {
            Ok(()) => (),
            Err(StorageError::NotFound(_) | StorageError::Conflict(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        let bindings = match context.project_id {
            Some(project) => self.project_plugins(user, project).await?,
            None => default_bindings(&self.plugin_installations(user).await?),
        };
        // An update may replace this namespace's installed version; existing jobs keep their snapshot.
        Ok(bindings.iter().any(|binding| {
            binding.enabled
                && binding.prepared.manifest.executable.is_some()
                && binding.prepared.source.repository == plugin.source.repository
                && binding.prepared.source.path == plugin.source.path
                && binding.prepared.storage_namespace() == plugin.storage_namespace()
        }))
    }
    pub(super) async fn plugin_jobs_in_transaction(
        &self,
        user: UserId,
        plugin: &PreparedPlugin,
        context: &PluginExecutionContext,
        request: &JobRequest,
        now: i64,
    ) -> Result<JobResult, StorageError> {
        request
            .validate(now)
            .map_err(StorageError::InvalidRequest)?;
        let scope = vec![
            DbValue::Int(user.get()),
            DbValue::Int(context.project_id.unwrap_or(0)),
            DbValue::Text(plugin.storage_namespace()),
        ];
        let mut id = None;
        match request {
            JobRequest::Schedule {
                scope: lifetime,
                key,
                due_at,
                expires_at,
                event,
                payload,
            } => {
                if !plugin.manifest.contributions.events.contains(event) {
                    return Err(StorageError::InvalidRequest(
                        "Plugin does not declare this event".into(),
                    ));
                }
                if !self.plugin_namespace_enabled(user, context, plugin).await? {
                    return Err(StorageError::Conflict("Plugin is no longer enabled".into()));
                }
                let mut params = scope.clone();
                params.push(DbValue::Text(key.clone()));
                let existing = self.db.execute(&format!("SELECT {COLUMNS},prepared,scope FROM plugin_jobs WHERE user_id=? AND project_scope=? AND plugin=? AND job_key=?"), &params).await?;
                if let Some(row) = existing.rows.first() {
                    let existing_job = job(row)?;
                    let pinned: PreparedPlugin = decode(row.get_text(10)?)?;
                    if existing_job.due_at != *due_at
                        || existing_job.expires_at != *expires_at
                        || existing_job.event != *event
                        || existing_job.payload != *payload
                        || pinned.source != plugin.source
                        || pinned.manifest != plugin.manifest
                        || pinned.digest != plugin.digest
                        || row.get_text(11)? != lifetime.as_str()
                    {
                        return Err(StorageError::Conflict(
                            "Job key already refers to another delivery".into(),
                        ));
                    }
                    return Ok(JobResult {
                        jobs: vec![existing_job],
                        next_after: None,
                    });
                }
                let count = self.db.execute("SELECT COUNT(*) FROM plugin_jobs WHERE user_id=? AND project_scope=? AND plugin=? AND background_id IS NULL", &scope).await?;
                if count.rows[0].get_int(0)? >= MAX_JOBS && !key.starts_with("background:") {
                    return Err(StorageError::InvalidRequest(
                        "Plugin job queue is full".into(),
                    ));
                }
                let mut stored_context = context.clone();
                if *lifetime == JobScope::Project {
                    stored_context.session_id = None;
                }
                params.extend([
                    DbValue::Int(*due_at),
                    expires_at.map_or(DbValue::Null, DbValue::Int),
                    DbValue::Text(event.clone()),
                    DbValue::Text(encode(payload)?),
                    DbValue::Text(encode(plugin)?),
                    DbValue::Text(encode(&stored_context)?),
                    DbValue::Text(plugin.host_id.clone()),
                    DbValue::Text(lifetime.as_str().into()),
                ]);
                id = Some(self.db.execute("INSERT INTO plugin_jobs(user_id,project_scope,plugin,job_key,due_at,expires_at,event,payload,prepared,context,host_id,scope) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)", &params).await?.last_insert_rowid);
            }
            JobRequest::Cancel {
                id: target,
                revision,
            } => {
                let mut params = scope.clone();
                params.extend([DbValue::Int(*target), DbValue::Int(*revision)]);
                let changed = self.db.execute("UPDATE plugin_jobs SET state='cancelled',revision=revision+1 WHERE user_id=? AND project_scope=? AND plugin=? AND id=? AND revision=? AND state IN ('waiting','pending','leased')", &params).await?;
                if changed.changes != 1 {
                    return Err(StorageError::Conflict(
                        "Job changed or is unavailable".into(),
                    ));
                }
                id = Some(*target);
            }
            JobRequest::Delete { id, revision } => {
                let mut params = scope.clone();
                params.extend([DbValue::Int(*id), DbValue::Int(*revision)]);
                let changed = self.db.execute("DELETE FROM plugin_jobs WHERE user_id=? AND project_scope=? AND plugin=? AND id=? AND revision=? AND state IN ('completed','failed','cancelled','expired')", &params).await?;
                if changed.changes != 1 {
                    return Err(StorageError::Conflict(
                        "Job changed, is active or is unavailable".into(),
                    ));
                }
                self.db
                    .execute(
                        "DELETE FROM plugin_execution_grants WHERE job_id=?",
                        &[DbValue::Int(*id)],
                    )
                    .await?;
                return Ok(JobResult {
                    jobs: vec![],
                    next_after: None,
                });
            }
            JobRequest::Read { id: target } => id = Some(*target),
            JobRequest::List { .. } => (),
        }
        let mut params = scope;
        let sql = if let Some(id) = id {
            params.push(DbValue::Int(id));
            format!(
                "SELECT {COLUMNS} FROM plugin_jobs WHERE user_id=? AND project_scope=? AND plugin=? AND id=?"
            )
        } else {
            let JobRequest::List { after } = request else {
                unreachable!("mutation selects its result")
            };
            params.push(DbValue::Int(*after));
            format!(
                "SELECT {COLUMNS} FROM plugin_jobs WHERE user_id=? AND project_scope=? AND plugin=? AND id>? ORDER BY id LIMIT 65"
            )
        };
        let rows = self.db.execute(&sql, &params).await?;
        let mut jobs = rows.rows.iter().map(job).collect::<Result<Vec<_>, _>>()?;
        if id.is_some() && jobs.is_empty() {
            return Err(StorageError::NotFound("plugin job".into()));
        }
        let mut bytes = 64usize;
        let mut count = 0usize;
        for entry in &jobs {
            let size = encode(entry)?.len() + 1;
            if count == usize::try_from(JOB_PAGE_SIZE).unwrap()
                || bytes.saturating_add(size) > MAX_JOB_PAGE_BYTES
            {
                break;
            }
            count += 1;
            bytes += size;
        }
        let next_after = if count < jobs.len() {
            jobs.truncate(count);
            jobs.last().map(|job| job.id)
        } else {
            None
        };
        Ok(JobResult { jobs, next_after })
    }

    /// Daemon-authenticated caller supplies its identity and a fresh random lease token.
    /// Pagination avoids a disabled namespace starving later runnable jobs.
    pub async fn claim_plugin_jobs(
        &self,
        host: &str,
        after: i64,
        lease: &str,
        now: i64,
    ) -> Result<JobDeliveryPage, StorageError> {
        token(lease)?;
        if host.is_empty() || host.len() > 1024 || after < 0 {
            return Err(StorageError::InvalidRequest(
                "Invalid job host or cursor".into(),
            ));
        }
        let expiry = lease_expiry(now)?;
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            if after == 0 { store.enqueue_plugin_background(host, now).await?; }
            let rows = store.db.execute(&format!("SELECT {COLUMNS},user_id,prepared,context,background_id FROM plugin_jobs WHERE host_id=? AND id>? AND due_at<=? AND (state='pending' OR (state='leased' AND lease_expires_at<=?)) ORDER BY id LIMIT 64"), &[DbValue::Text(host.into()),DbValue::Int(after),DbValue::Int(now),DbValue::Int(now)]).await?;
            let mut more = rows.rows.len() == usize::try_from(JOB_PAGE_SIZE).unwrap();
            let mut cursor = after;
            let mut bytes = 64usize;
            let mut jobs = Vec::new();
            for row in &rows.rows {
                if jobs.len() == MAX_JOB_DELIVERIES { more = true; break; }
                let mut entry = job(row)?;
                if entry.expires_at.is_some_and(|expiry| expiry <= now) {
                    store.db.execute("UPDATE plugin_jobs SET state='expired',revision=revision+1 WHERE id=?", &[DbValue::Int(entry.id)]).await?;
                    cursor = entry.id;
                    continue;
                }
                if let Some(background) = row.get_int_opt(13) {
                    let current = store.db.execute("SELECT 1 FROM plugin_background WHERE id=? AND prepared=?", &[DbValue::Int(background),DbValue::Text(row.get_text(11)?.into())]).await?;
                    if current.rows.is_empty() {
                        store.db.execute("UPDATE plugin_jobs SET state='cancelled',revision=revision+1 WHERE id=?", &[DbValue::Int(entry.id)]).await?;
                        cursor = entry.id;
                        continue;
                    }
                }
                let user = UserId::new(row.get_int(10)?);
                let prepared: PreparedPlugin = decode(row.get_text(11)?)?;
                let context: PluginExecutionContext = decode(row.get_text(12)?)?;
                if !store.plugin_namespace_enabled(user, &context, &prepared).await? { cursor = entry.id; continue; }
                prepared.validate().map_err(|error| StorageError::Db(error.to_string()))?;
                entry.revision += 1;
                entry.attempts += 1;
                entry.state = JobState::Leased;
                let delivery = JobDelivery {user_id:user.get(),job:entry,prepared,context,lease:lease.into(),lease_expires_at:expiry};
                let size = encode(&delivery)?.len() + 1;
                if size + 64 > MAX_JOB_DELIVERY_BYTES { return Err(StorageError::InvalidRequest("Job delivery exceeds its limit".into())); }
                if bytes.saturating_add(size) > MAX_JOB_DELIVERY_BYTES { more = true; break; }
                store.db.execute("UPDATE plugin_jobs SET state='leased',revision=revision+1,attempts=attempts+1,lease=?,lease_expires_at=? WHERE id=?", &[DbValue::Text(lease.into()), DbValue::Int(expiry), DbValue::Int(delivery.job.id)]).await?;
                cursor = delivery.job.id;
                bytes += size;
                jobs.push(delivery);
            }
            Ok(JobDeliveryPage {jobs,next_after:more.then_some(cursor)})
        }).await
    }
    /// A heartbeat extends the current lease and its callback authority together.
    pub async fn renew_plugin_job(
        &self,
        host: &str,
        id: i64,
        lease: &str,
        now: i64,
    ) -> Result<i64, StorageError> {
        token(lease)?;
        let expiry = lease_expiry(now)?;
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            let changed = store.db.execute("UPDATE plugin_jobs SET lease_expires_at=? WHERE id=? AND host_id=? AND lease=? AND state='leased' AND lease_expires_at>? AND (background_id IS NULL OR EXISTS(SELECT 1 FROM plugin_background b WHERE b.id=plugin_jobs.background_id AND b.prepared=plugin_jobs.prepared))", &[DbValue::Int(expiry),DbValue::Int(id),DbValue::Text(host.into()),DbValue::Text(lease.into()),DbValue::Int(now)]).await?;
            if changed.changes != 1 { return Err(StorageError::Conflict("Job lease is no longer current".into())); }
            store.db.execute("UPDATE plugin_execution_grants SET expires_at=? WHERE job_id=? AND job_lease=?", &[DbValue::Int(expiry),DbValue::Int(id),DbValue::Text(lease.into())]).await?;
            Ok(expiry)
        }).await
    }
    pub async fn finish_plugin_job(
        &self,
        host: &str,
        id: i64,
        lease: &str,
        success: bool,
        detail: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        token(lease)?;
        if detail.len() > 4096 || now < 0 {
            return Err(StorageError::InvalidRequest("Invalid job result".into()));
        }
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            let rows = store.db.execute("SELECT state,detail,lease_expires_at FROM plugin_jobs WHERE id=? AND host_id=? AND lease=?", &[DbValue::Int(id),DbValue::Text(host.into()),DbValue::Text(lease.into())]).await?;
            let state = if success { "completed" } else { "failed" };
            let row = rows.rows.first().ok_or_else(|| StorageError::Conflict("Job lease is no longer current".into()))?;
            if row.get_text(0)? == state && row.get_text_opt(1) == Some(detail) { return Ok(()); }
            if row.get_text(0)? != "leased" || row.get_int(2)? <= now { return Err(StorageError::Conflict("Job lease is no longer current".into())); }
            store.db.execute("UPDATE plugin_jobs SET state=?,detail=?,revision=revision+1 WHERE id=?", &[DbValue::Text(state.into()),DbValue::Text(detail.into()),DbValue::Int(id)]).await?;
            Ok(())
        }).await
    }
    /// Only the current delivery can acquire authority for its historical version.
    pub async fn issue_plugin_job_grant(
        &self,
        user: UserId,
        host: &str,
        id: i64,
        lease: &str,
        grant: &str,
        now: i64,
    ) -> Result<(PreparedPlugin, PluginExecutionContext), StorageError> {
        token(lease)?;
        token(grant)?;
        if now < 0 {
            return Err(StorageError::InvalidRequest("Invalid job clock".into()));
        }
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            let rows = store.db.execute("SELECT prepared,context,lease_expires_at FROM plugin_jobs WHERE id=? AND user_id=? AND host_id=? AND lease=? AND state='leased' AND lease_expires_at>? AND (background_id IS NULL OR EXISTS(SELECT 1 FROM plugin_background b WHERE b.id=plugin_jobs.background_id AND b.prepared=plugin_jobs.prepared))", &[DbValue::Int(id),DbValue::Int(user.get()),DbValue::Text(host.into()),DbValue::Text(lease.into()),DbValue::Int(now)]).await?;
            let row = rows.rows.first().ok_or_else(|| StorageError::Conflict("Job lease is no longer current".into()))?;
            let prepared: PreparedPlugin = decode(row.get_text(0)?)?;
            let mut context: PluginExecutionContext = decode(row.get_text(1)?)?;
            if !store.plugin_namespace_enabled(user, &context, &prepared).await? { return Err(StorageError::Conflict("Plugin is no longer enabled".into())); }
            // Background callbacks cannot inherit a manual-editing override or chat authority.
            context.user_action = false;
            context.session_id = None;
            store.db.execute("INSERT INTO plugin_execution_grants(token,user_id,session_id,project_scope,prepared,expires_at,primary_model,user_action,job_id,job_lease) VALUES(?,?,NULL,?,?,?,?,0,?,?)", &[DbValue::Text(grant.into()),DbValue::Int(user.get()),DbValue::Int(context.project_id.unwrap_or(0)),DbValue::Text(encode(&prepared)?),DbValue::Int(row.get_int(2)?),context.primary.as_ref().map(encode).transpose()?.map_or(DbValue::Null,DbValue::Text),DbValue::Int(id),DbValue::Text(lease.into())]).await?;
            Ok((prepared,context))
        }).await
    }
}

#[cfg(test)]
mod tests;
