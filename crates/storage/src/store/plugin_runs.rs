//! Durable raw prompt submissions, scoped by existing plugin authority.
use super::*;
use crate::db::QueryRow;
use openwebide_core::plugins::{PreparedPlugin, execution::PluginExecutionContext, runs::*};
use openwebide_core::{ModelSelection, QueuedPromptKey};

const COLUMNS: &str =
    "id,revision,run_key,session_id,state,created_at,detail,message_id,permission_id";
fn encode(value: &impl serde::Serialize) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| StorageError::Db(error.to_string()))
}
fn run(row: &QueryRow) -> Result<PluginRun, StorageError> {
    Ok(PluginRun {
        id: row.get_int(0)?,
        revision: row.get_int(1)?,
        key: row.get_text(2)?.into(),
        session_id: row.get_int_opt(3),
        state: serde_json::from_value(serde_json::Value::String(row.get_text(4)?.into()))
            .map_err(|error| StorageError::Db(error.to_string()))?,
        created_at: row.get_int(5)?,
        detail: row.get_text(6)?.into(),
        message_id: row.get_int_opt(7),
        permission_id: row.get_text_opt(8).map(str::to_owned),
    })
}
impl<D: Db> Store<D> {
    pub(super) async fn plugin_runs_in_transaction(
        &self,
        user: UserId,
        plugin: &PreparedPlugin,
        context: &PluginExecutionContext,
        request: &RunRequest,
        now: i64,
    ) -> Result<RunResult, StorageError> {
        request.validate().map_err(StorageError::InvalidRequest)?;
        if now < 0 {
            return Err(StorageError::InvalidRequest("Invalid run clock".into()));
        }
        let scope = vec![
            DbValue::Int(user.get()),
            DbValue::Int(context.project_id.unwrap_or(0)),
            DbValue::Text(plugin.storage_namespace()),
        ];
        let mut id = None;
        match request {
            RunRequest::Submit {
                key,
                prompt,
                target,
                model,
                completion,
                prerequisites,
            } => {
                if !self.plugin_namespace_enabled(user, context, plugin).await? {
                    return Err(StorageError::Conflict("Plugin is no longer enabled".into()));
                }
                if completion.as_ref().is_some_and(|completion| {
                    !plugin
                        .manifest
                        .contributions
                        .events
                        .contains(&completion.event)
                }) {
                    return Err(StorageError::InvalidRequest(
                        "Plugin does not declare this completion event".into(),
                    ));
                }
                let encoded = encode(request)?;
                let mut params = scope.clone();
                params.push(DbValue::Text(key.clone()));
                let existing = self.db.execute(&format!("SELECT {COLUMNS},request FROM plugin_runs WHERE user_id=? AND project_scope=? AND plugin=? AND run_key=?"), &params).await?;
                if let Some(row) = existing.rows.first() {
                    if row.get_text(9)? != encoded {
                        return Err(StorageError::Conflict(
                            "Run key already refers to another submission".into(),
                        ));
                    }
                    // A retry after source updates returns the original immutable submission.
                    return Ok(RunResult {
                        runs: vec![run(row)?],
                        next_after: None,
                    });
                }
                // The enclosing grant transaction also queues the prompt: no edit or
                // deletion can race between these scoped reads and the new submission.
                // An identical retry above returns prior work even if its source record
                // has since advanced, preserving the existing idempotency contract.
                for condition in prerequisites {
                    if !plugin
                        .manifest
                        .executable
                        .as_ref()
                        .is_some_and(|rust| rust.capabilities.contains(&condition.capability))
                    {
                        return Err(StorageError::InvalidRequest(
                            "Run prerequisite capability is not granted".into(),
                        ));
                    }
                    let read = openwebide_core::plugins::records::RecordRequest {
                        collection: condition.collection.clone(),
                        operation: openwebide_core::plugins::records::RecordOperation::Read {
                            id: condition.id,
                        },
                    };
                    let records = if condition.capability == "collections" {
                        let result = self
                            .plugin_collections_in_transaction(user, plugin, context, &read, now)
                            .await?;
                        if !result.enabled {
                            return Err(StorageError::Conflict(
                                "Run prerequisite collection is disabled".into(),
                            ));
                        }
                        result.records
                    } else {
                        self.plugin_project_records_in_transaction(
                            user,
                            context.project_id,
                            &plugin.storage_namespace(),
                            &read,
                            now,
                        )
                        .await?
                        .records
                    };
                    if !records
                        .first()
                        .is_some_and(|record| record.revision == condition.revision)
                    {
                        return Err(StorageError::Conflict(
                            "Run prerequisite record changed".into(),
                        ));
                    }
                }
                let count = self.db.execute("SELECT count(*) FROM plugin_runs WHERE user_id=? AND project_scope=? AND plugin=?", &scope).await?.rows[0].get_int(0)?;
                if count >= MAX_RUNS {
                    return Err(StorageError::Conflict(
                        "Plugin run history holds at most 1,000 entries; delete terminal runs"
                            .into(),
                    ));
                }
                let model = model.as_ref().or(context.primary.as_ref());
                if let Some(model) = model
                    && !self.get_connection(model.server_id).await?.enabled
                {
                    return Err(StorageError::InvalidRequest(
                        "Run model server is disabled".into(),
                    ));
                }
                let session = match target {
                    RunTarget::Origin => context.session_id.ok_or_else(|| {
                        StorageError::InvalidRequest("Run has no originating conversation".into())
                    })?,
                    RunTarget::Session { id } => *id,
                    RunTarget::New { title } => {
                        self.create_session_unlocked(
                            title,
                            model.map(|model| model.server_id),
                            None,
                            context.project_id,
                            user,
                            now,
                        )
                        .await?
                        .id
                    }
                };
                if self.get_session(session, user).await?.project_id != context.project_id {
                    return Err(StorageError::InvalidRequest(
                        "Choose a conversation in this project".into(),
                    ));
                }
                let queued = self
                    .enqueue_prompt_in_transaction(user, session, prompt, now, false)
                    .await?;
                params.extend([
                    DbValue::Text(encoded),
                    DbValue::Text(encode(plugin)?),
                    DbValue::Text(encode(context)?),
                    DbValue::Text(plugin.host_id.clone()),
                    DbValue::Int(session),
                    DbValue::Int(queued.id),
                    model
                        .map(encode)
                        .transpose()?
                        .map_or(DbValue::Null, DbValue::Text),
                    DbValue::Int(now),
                ]);
                let inserted = self.db.execute("INSERT INTO plugin_runs(user_id,project_scope,plugin,run_key,request,prepared,context,host_id,session_id,queued_id,model,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)", &params).await?.last_insert_rowid;
                if let Some(completion) = completion {
                    let count = self.db.execute("SELECT count(*) FROM plugin_jobs WHERE user_id=? AND project_scope=? AND plugin=? AND background_id IS NULL", &scope).await?.rows[0].get_int(0)?;
                    if count >= openwebide_core::plugins::jobs::MAX_JOBS {
                        return Err(StorageError::Conflict("Plugin event queue is full; delete terminal events before submitting a completion callback".into()));
                    }
                    let mut callback_context = context.clone();
                    callback_context.primary = model.cloned();
                    let mut callback = scope.clone();
                    // '$' is excluded from author-selected job keys: internal callbacks cannot collide.
                    callback.extend([
                        DbValue::Text(format!("$run:{inserted}")),
                        DbValue::Text(completion.event.clone()),
                        DbValue::Text(encode(&completion.payload)?),
                        DbValue::Text(encode(plugin)?),
                        DbValue::Text(encode(&callback_context)?),
                        DbValue::Text(plugin.host_id.clone()),
                    ]);
                    let job = self.db.execute("INSERT INTO plugin_jobs(user_id,project_scope,plugin,job_key,due_at,event,payload,prepared,context,host_id,state) VALUES(?,?,?,?,0,?,?,?,?,?,'waiting')", &callback).await?.last_insert_rowid;
                    self.db
                        .execute(
                            "UPDATE plugin_runs SET completion_job=? WHERE id=?",
                            &[DbValue::Int(job), DbValue::Int(inserted)],
                        )
                        .await?;
                }
                id = Some(inserted);
            }
            RunRequest::Cancel {
                id: selected,
                revision,
            }
            | RunRequest::Delete {
                id: selected,
                revision,
            } => {
                let mut params = scope.clone();
                params.push(DbValue::Int(*selected));
                let rows = self.db.execute(&format!("SELECT {COLUMNS},queued_id,lease FROM plugin_runs WHERE user_id=? AND project_scope=? AND plugin=? AND id=?"), &params).await?;
                let row = rows
                    .rows
                    .first()
                    .ok_or_else(|| StorageError::NotFound("Plugin run".into()))?;
                let current = run(row)?;
                if current.revision != *revision {
                    return Err(StorageError::Conflict("Plugin run changed".into()));
                }
                if matches!(request, RunRequest::Delete { .. }) {
                    if !current.state.terminal() {
                        return Err(StorageError::Conflict(
                            "Only terminal runs can be deleted".into(),
                        ));
                    }
                    self.db
                        .execute(
                            "DELETE FROM plugin_runs WHERE id=?",
                            &[DbValue::Int(*selected)],
                        )
                        .await?;
                    return Ok(RunResult {
                        runs: Vec::new(),
                        next_after: None,
                    });
                }
                if current.state.terminal() || current.state == RunState::Cancelling {
                    return Err(StorageError::Conflict(
                        "Run is already finished or cancelling".into(),
                    ));
                }
                let delivered = current.message_id.is_some();
                let state = if delivered { "cancelling" } else { "cancelled" };
                self.db.execute("UPDATE plugin_runs SET state=?,revision=revision+1,detail='Cancellation requested',permission_id=NULL WHERE id=?", &[DbValue::Text(state.into()), DbValue::Int(*selected)]).await?;
                if let Some(queued) = row.get_int_opt(9) {
                    self.db
                        .execute(
                            "DELETE FROM queued_prompts WHERE id=?",
                            &[DbValue::Int(queued)],
                        )
                        .await?;
                }
                if delivered && let Some(session) = current.session_id {
                    let active = self.db.execute("SELECT 1 FROM session_run_leases WHERE session_id=? AND token=? AND expires_at>?", &[DbValue::Int(session), DbValue::Text(format!("plugin-run-{selected}-{}", row.get_text(10)?)), DbValue::Int(now)]).await?;
                    if !active.rows.is_empty() {
                        self.request_cancel(session, now.saturating_mul(1000).saturating_add(1))
                            .await?;
                    }
                }
                id = Some(*selected);
            }
            RunRequest::Read { id: selected } => id = Some(*selected),
            RunRequest::List { .. } => (),
        }
        let mut params = scope;
        let (filter, limit) = match id {
            Some(id) => {
                params.push(DbValue::Int(id));
                ("id=?", 1)
            }
            None => {
                let RunRequest::List { after } = request else {
                    unreachable!("submission returned an ID")
                };
                params.push(DbValue::Int(*after));
                ("id>?", RUN_PAGE_SIZE + 1)
            }
        };
        let rows = self.db.execute(&format!("SELECT {COLUMNS} FROM plugin_runs WHERE user_id=? AND project_scope=? AND plugin=? AND {filter} ORDER BY id LIMIT {limit}"), &params).await?;
        if id.is_some() && rows.rows.is_empty() {
            return Err(StorageError::NotFound("Plugin run".into()));
        }
        let mut runs = rows.rows.iter().map(run).collect::<Result<Vec<_>, _>>()?;
        let next_after = if runs.len() > usize::try_from(RUN_PAGE_SIZE).expect("bounded page size")
        {
            runs.pop();
            runs.last().map(|run| run.id)
        } else {
            None
        };
        Ok(RunResult { runs, next_after })
    }
    /// Selected model belongs to the immutable queued submission, not its conversation.
    pub async fn plugin_prompt_model(
        &self,
        user: UserId,
        session: i64,
        key: QueuedPromptKey,
    ) -> Result<Option<ModelSelection>, StorageError> {
        self.get_session(session, user).await?;
        let result = self.db.execute("SELECT r.model FROM plugin_runs r JOIN queued_prompts q ON q.id=r.queued_id WHERE r.user_id=? AND r.session_id=? AND q.id=? AND q.revision=?", &[DbValue::Int(user.get()), DbValue::Int(session), DbValue::Int(key.id), DbValue::Int(key.revision)]).await?;
        result
            .rows
            .first()
            .and_then(|row| row.get_text_opt(0))
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| StorageError::Db(error.to_string()))
    }
}

#[cfg(test)]
mod tests;

fn expiry(now: i64) -> Result<i64, StorageError> {
    if now < 0 {
        return Err(StorageError::InvalidRequest("Invalid run clock".into()));
    }
    now.checked_add(120)
        .ok_or_else(|| StorageError::InvalidRequest("Invalid run lease expiry".into()))
}
fn lease_token(value: &str) -> Result<(), StorageError> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StorageError::InvalidRequest("Invalid run lease".into()));
    }
    Ok(())
}
impl<D: Db> Store<D> {
    pub async fn claim_plugin_runs(
        &self,
        host: &str,
        after: i64,
        token: &str,
        now: i64,
    ) -> Result<RunDeliveryPage, StorageError> {
        lease_token(token)?;
        let until = expiry(now)?;
        if host.is_empty() || host.len() > 1024 || after < 0 {
            return Err(StorageError::InvalidRequest("Invalid run claim".into()));
        }
        self.db.transaction(|tx| async move {
            let store=Store::new(tx);
            // Recovery is allowed only while the prompt remains unconsumed.
            store.db.execute("UPDATE plugin_runs SET state='pending',revision=revision+1,lease=NULL,lease_expires_at=NULL WHERE host_id=? AND state='leased' AND lease_expires_at<=? AND queued_id IS NOT NULL AND message_id IS NULL", &[DbValue::Text(host.into()),DbValue::Int(now)]).await?;
            store.db.execute("UPDATE plugin_runs SET state='interrupted',revision=revision+1,permission_id=NULL,detail='Host stopped after prompt delivery; the prompt will not be replayed' WHERE host_id=? AND state IN ('leased','running','blocked','cancelling') AND lease_expires_at<=? AND message_id IS NOT NULL", &[DbValue::Text(host.into()),DbValue::Int(now)]).await?;
            let rows=store.db.execute(&format!("SELECT {COLUMNS},user_id,prepared,context,queued_id FROM plugin_runs WHERE host_id=? AND state='pending' AND id>? ORDER BY id LIMIT 64"), &[DbValue::Text(host.into()),DbValue::Int(after)]).await?;
            let more=rows.rows.len()==64; let mut cursor=after; let mut deliveries=Vec::new();
            for row in &rows.rows {
                if deliveries.len()==8 { return Ok(RunDeliveryPage{runs:deliveries,next_after:Some(cursor)}); }
                let mut entry=run(row)?; cursor=entry.id;
                let user=UserId::new(row.get_int(9)?);
                let prepared:PreparedPlugin=serde_json::from_str(row.get_text(10)?).map_err(|error|StorageError::Db(error.to_string()))?;
                let context:PluginExecutionContext=serde_json::from_str(row.get_text(11)?).map_err(|error|StorageError::Db(error.to_string()))?;
                if !store.plugin_namespace_enabled(user,&context,&prepared).await? {continue;}
                let Some(session)=entry.session_id else {continue;};
                if store.session_run_active(user,session,now).await? {continue;}
                let queue=store.list_queued_prompts(user,session).await?;
                let Some(prompt)=queue.first().filter(|prompt|Some(prompt.id)==row.get_int_opt(12)) else {continue;};
                let host_path=match context.project_id {
                    Some(project) if store.get_project(project,user).await?.mode==WorkspaceMode::Local => {
                        let Some(binding)=store.get_user_setting(user,&format!("scheduled_host_{project}")).await? else {continue;};
                        let binding:openwebide_core::scheduled::HostBinding=serde_json::from_str(&binding).map_err(|error|StorageError::Db(error.to_string()))?;
                        if binding.host_id!=host || binding.path.is_empty() || binding.path.len()>4096 {continue;}
                        Some(binding.path)
                    }
                    _=>None,
                };
                entry.state=RunState::Leased; entry.revision+=1;
                store.db.execute("UPDATE plugin_runs SET state='leased',revision=revision+1,lease=?,lease_expires_at=? WHERE id=?", &[DbValue::Text(token.into()),DbValue::Int(until),DbValue::Int(entry.id)]).await?;
                let mut prompt = prompt.clone();
                store.db.execute("UPDATE queued_prompts SET revision=revision+1 WHERE id=?", &[DbValue::Int(prompt.id)]).await?;
                prompt.revision += 1;
                deliveries.push(RunDelivery{user_id:user.get(),run:entry,prompt,host_path,lease:RunLease{host_id:host.into(),id:cursor,lease:token.into()},lease_expires_at:until});
            }
            Ok(RunDeliveryPage{runs:deliveries,next_after:more.then_some(cursor)})
        }).await
    }
    async fn run_lease_in_transaction(
        &self,
        lease: &RunLease,
        now: i64,
    ) -> Result<PluginRun, StorageError> {
        lease_token(&lease.lease)?;
        expiry(now)?;
        let rows=self.db.execute(&format!("SELECT {COLUMNS} FROM plugin_runs WHERE id=? AND host_id=? AND lease=? AND lease_expires_at>?"), &[DbValue::Int(lease.id),DbValue::Text(lease.host_id.clone()),DbValue::Text(lease.lease.clone()),DbValue::Int(now)]).await?;
        rows.rows
            .first()
            .map(run)
            .transpose()?
            .ok_or_else(|| StorageError::Conflict("Plugin run lease is no longer current".into()))
    }
    pub async fn renew_plugin_run(
        &self,
        lease: &RunLease,
        now: i64,
    ) -> Result<RunLeaseStatus, StorageError> {
        let until = expiry(now)?;
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let current = store.run_lease_in_transaction(lease, now).await?;
                if current.state.terminal() {
                    return Err(StorageError::Conflict("Run already finished".into()));
                }
                store
                    .db
                    .execute(
                        "UPDATE plugin_runs SET lease_expires_at=? WHERE id=?",
                        &[DbValue::Int(until), DbValue::Int(lease.id)],
                    )
                    .await?;
                Ok(RunLeaseStatus {
                    expires_at: until,
                    cancel_requested: current.state == RunState::Cancelling,
                })
            })
            .await
    }
    pub async fn report_plugin_run(
        &self,
        lease: &RunLease,
        report: &RunReport,
        now: i64,
    ) -> Result<(), StorageError> {
        report.validate().map_err(StorageError::InvalidRequest)?;
        let until = expiry(now)?;
        self.db.transaction(|tx| async move {
            let store=Store::new(tx); let current=store.run_lease_in_transaction(lease,now).await?;
            if current.state.terminal() {
                return if current.state==report.state && current.detail==report.detail && current.permission_id==report.permission_id {Ok(())} else {Err(StorageError::Conflict("Run already finished".into()))};
            }
            if current.message_id.is_none() && !matches!(report.state,RunState::Failed|RunState::Cancelled) {return Err(StorageError::Conflict("Run prompt has not been delivered".into()));}
            let state=if current.state==RunState::Cancelling && !report.state.terminal() {RunState::Cancelling} else {report.state.clone()};
            let serialized=encode(&state)?; let state=serialized.trim_matches('"');
            store.db.execute("UPDATE plugin_runs SET state=?,revision=revision+1,detail=?,permission_id=?,lease_expires_at=? WHERE id=?", &[DbValue::Text(state.into()),DbValue::Text(report.detail.clone()),if state=="cancelling" {DbValue::Null} else {report.permission_id.clone().map_or(DbValue::Null,DbValue::Text)},DbValue::Int(until),DbValue::Int(lease.id)]).await?;
            if report.state.terminal() {store.db.execute("DELETE FROM queued_prompts WHERE id=(SELECT queued_id FROM plugin_runs WHERE id=?)", &[DbValue::Int(lease.id)]).await?;}
            Ok(())
        }).await
    }
    pub async fn release_plugin_run(&self, lease: &RunLease, now: i64) -> Result<(), StorageError> {
        self.db.transaction(|tx| async move {
            let store=Store::new(tx); let current=store.run_lease_in_transaction(lease,now).await?;
            if current.state!=RunState::Leased || current.message_id.is_some() {return Err(StorageError::Conflict("Only an undelivered run can return to the queue".into()));}
            store.db.execute("UPDATE plugin_runs SET state='pending',revision=revision+1,lease=NULL,lease_expires_at=NULL WHERE id=?", &[DbValue::Int(lease.id)]).await?;
            Ok(())
        }).await
    }
}
