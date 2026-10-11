//! Durable claims, saved prompts, and ownership policy above the database adapter.
use super::*;
use openwebide_core::scheduled::{
    DispatchResult, ExecutionHost, HostBinding, MonitorCommand, ScheduledTask, SessionTarget,
    TaskCommand, TaskDelivery, TaskDraft, TaskRun,
};
fn encode<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|e| StorageError::Db(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_str(value).map_err(|e| StorageError::InvalidValue(e.to_string()))
}
const TASK_COLUMNS: &str = "id, revision, project_id, draft, next_run, host_id, path";
impl<D: Db> Store<D> {
    pub async fn scheduled_tasks(
        &self,
        user: UserId,
        project: Option<i64>,
        now: i64,
    ) -> Result<Vec<ScheduledTask>, StorageError> {
        self.tasks_in_scope(user, project, None, now).await
    }
    /// Owned monitor data projection. Reading it does not run scheduling/expiry policy.
    pub async fn scheduled_monitors(
        &self,
        user: UserId,
        session: i64,
        now: i64,
    ) -> Result<Vec<ScheduledTask>, StorageError> {
        let project = self.get_session(session, user).await?.project_id;
        self.tasks_in_scope(user, project, Some(session), now).await
    }
    /// A filesystem transport binding for unattended execution, shared with core goal workers.
    pub async fn bind_background_host(
        &self,
        user: UserId,
        project: i64,
        binding: &HostBinding,
    ) -> Result<(), StorageError> {
        let project = self.get_project(project, user).await?;
        if project.mode != WorkspaceMode::Local
            || binding.host_id.is_empty()
            || binding.host_id.len() > 256
            || binding.path.is_empty()
            || binding.path.len() > 4096
        {
            return Err(StorageError::InvalidRequest(
                "Invalid project execution host binding".into(),
            ));
        }
        self.set_user_setting(
            user,
            &format!("scheduled_host_{}", project.id),
            &encode(binding)?,
        )
        .await
    }
    async fn tasks_in_scope(
        &self,
        user: UserId,
        project: Option<i64>,
        monitor_session: Option<i64>,
        now: i64,
    ) -> Result<Vec<ScheduledTask>, StorageError> {
        if let Some(project) = project {
            self.get_project(project, user).await?;
        }
        let rows = self.db.execute(&format!("SELECT {TASK_COLUMNS} FROM scheduled_tasks WHERE user_id = ? AND project_id IS ? AND NOT EXISTS (SELECT 1 FROM goal_workers g WHERE g.task_id=scheduled_tasks.id) AND ((? IS NULL AND NOT EXISTS(SELECT 1 FROM monitors WHERE task_id=scheduled_tasks.id)) OR EXISTS(SELECT 1 FROM monitors WHERE task_id=scheduled_tasks.id AND session_id=?)) ORDER BY id"), &[DbValue::Int(user.get()), project.map_or(DbValue::Null, DbValue::Int), monitor_session.map_or(DbValue::Null, DbValue::Int), monitor_session.map_or(DbValue::Null, DbValue::Int)]).await?;
        let mut tasks = Vec::new();
        for row in rows.rows {
            let id = row.get_int(0)?;
            let host_id = row.get_text(5)?.to_owned();
            let hosts = self
                .db
                .execute(
                    "SELECT last_seen FROM execution_hosts WHERE id = ?",
                    &[DbValue::Text(host_id.clone())],
                )
                .await?;
            let history = self.db.execute("SELECT * FROM (SELECT id,task_id,due_at,status,detail,message_id,permission_id,session_id FROM scheduled_runs WHERE task_id=? UNION ALL SELECT id+4503599627370496,task_id,due_at,status,detail,message_id,permission_id,session_id FROM plugin_task_runs WHERE task_id=?) ORDER BY id DESC LIMIT 1", &[DbValue::Int(id),DbValue::Int(id)]).await?;
            tasks.push(ScheduledTask {
                id,
                revision: row.get_int(1)?,
                project_id: row.get_int_opt(2),
                draft: decode(row.get_text(3)?)?,
                next_run: row.get_int_opt(4),
                host_id,
                host_available: hosts.rows.first().is_some_and(|row| {
                    row.get_int_opt(0)
                        .is_some_and(|seen| now.saturating_sub(seen) < 30)
                }),
                last_run: history.rows.first().map(run_row).transpose()?,
            });
        }
        Ok(tasks)
    }
    pub fn scheduled_command<'a>(
        &'a self,
        user: UserId,
        project: Option<i64>,
        command: &'a TaskCommand,
        binding: Option<&'a HostBinding>,
        agent: bool,
        now: i64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<ScheduledTask>, StorageError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.db.transaction(|tx| async move {
                let store = Store::new(tx);
                let monitor_session = if let TaskCommand::Monitor { session_id, .. } = command {
                    let session = store.get_session(*session_id, user).await?;
                    if session.project_id != project {
                        return Err(StorageError::InvalidRequest("Monitor belongs to another project.".into()));
                    }
                    if let Some(binding) = binding {
                        if agent || !matches!(command,TaskCommand::Monitor { command: MonitorCommand::List {}, .. }) {
                            return Err(StorageError::InvalidRequest("Only the user can authorize an execution host.".into()));
                        }
                        if binding.host_id.is_empty() || binding.host_id.len()>256 || binding.path.is_empty() || binding.path.len()>4096 {
                            return Err(StorageError::InvalidRequest("Invalid execution host.".into()));
                        }
                        store.set_user_setting(user,&format!("scheduled_host_{}",project.unwrap_or(0)),&encode(binding)?).await?;
                    }
                    Some(*session_id)
                } else { None };
                if matches!(command, TaskCommand::Monitor { command: MonitorCommand::Start { .. }, .. }) {
                    let checking = store.db.execute("SELECT 1 FROM session_run_leases l JOIN scheduled_runs r ON l.token='scheduled-'||r.id JOIN monitors m ON m.task_id=r.task_id WHERE l.session_id=? AND l.expires_at>?", &[monitor_session.map_or(DbValue::Null,DbValue::Int),DbValue::Int(now)]).await?;
                    if !checking.rows.is_empty() { return Err(StorageError::InvalidRequest("A monitor check cannot create another monitor to extend its deadline. Configure bounded repeats when starting it.".into())); }
                }
                let tasks = store.tasks_in_scope(user, project, monitor_session, now).await?;
                let normalized;
                let original = command;
                let command = if let TaskCommand::Monitor { session_id, command } = original {
                    normalized = match command {
                        MonitorCommand::Start { .. } => TaskCommand::Create { draft: command.draft(*session_id, now).map_err(StorageError::InvalidRequest)?.expect("start has a draft") },
                        MonitorCommand::List {} => TaskCommand::List,
                        MonitorCommand::Cancel { id, revision } => TaskCommand::SetEnabled { id: *id, revision: *revision, enabled: false },
                    };
                    &normalized
                } else { original };
                if let TaskCommand::Update{id,..}|TaskCommand::SetEnabled{id,..}|TaskCommand::Delete{id,..}=command {
                    let rows=store.db.execute("SELECT plugin_owner FROM scheduled_tasks WHERE id=? AND user_id=?", &[DbValue::Int(*id),DbValue::Int(user.get())]).await?;
                    if rows.rows.first().is_some_and(|row|row.get_text_opt(0).is_some()) {
                        return Err(StorageError::Conflict("Manage this task through its plugin".into()));
                    }
                }
                let target = |id,revision| tasks.iter().find(|task|task.id==id && task.revision==revision).ok_or_else(||StorageError::Conflict("Task changed or was removed. Refresh before editing.".into()));
                match command {
                    TaskCommand::Monitor { .. } => unreachable!("monitor command normalized"),
                    TaskCommand::List => {},
                    TaskCommand::Create{draft} | TaskCommand::Update{draft,..} => {
                        draft.validate(now).map_err(StorageError::InvalidRequest)?;
                        if let Some(model) = &draft.model
                            && !store.get_connection(model.server_id).await?.enabled {
                            return Err(StorageError::InvalidRequest("The selected model server is disabled.".into()));
                        }
                        let session_id = if draft.session_target == SessionTarget::Existing {
                            let session = store.get_session(draft.session_id,user).await?;
                            if session.project_id != project { return Err(StorageError::InvalidRequest("Choose a session in this project.".into())); }
                            DbValue::Int(session.id)
                        } else { DbValue::Null };
                        let local = if let Some(project) = project {store.get_project(project,user).await?.mode==WorkspaceMode::Local} else {false};
                        let key = format!("scheduled_host_{}", project.unwrap_or(0));
                        if agent && binding.is_some() {return Err(StorageError::InvalidRequest("Only the user can authorize an execution host.".into()));}
                        if let Some(binding) = binding {
                            if binding.host_id.is_empty() || binding.host_id.len()>256 || binding.path.len()>4096 {return Err(StorageError::InvalidRequest("Invalid execution host.".into()));}
                            store.set_user_setting(user,&key,&encode(binding)?).await?;
                        }
                        let saved_binding = store.get_user_setting(user,&key).await?.map(|value|decode::<HostBinding>(&value)).transpose()?;
                        let bound = if local {Some(saved_binding.ok_or_else(||StorageError::InvalidRequest("Connect this folder to its host before scheduling unattended runs.".into()))?)} else {None};
                        let host = bound.as_ref().map_or("server",|binding|binding.host_id.as_str());
                        let path = bound.as_ref().map_or(DbValue::Null,|binding|DbValue::Text(binding.path.clone()));
                        let next = draft.schedule.next_after(now).map_err(StorageError::InvalidRequest)?.map_or(DbValue::Null, DbValue::Int);
                        if let TaskCommand::Update{id,revision,..}=command {
                            target(*id,*revision)?;
                            store.cancel_scheduled_pending(*id).await?;
                            store.db.execute("UPDATE scheduled_tasks SET draft = ?, enabled = ?, next_run = ?, session_id = ?, host_id = ?, path = ?, revision = revision + 1 WHERE id = ? AND revision = ?", &[DbValue::Text(encode(draft)?),DbValue::Int(i64::from(draft.enabled)),next,session_id,DbValue::Text(host.into()),path,DbValue::Int(*id),DbValue::Int(*revision)]).await?;
                        } else {
                            if tasks.len()>=if monitor_session.is_some(){20}else{100} {return Err(StorageError::InvalidRequest("This scope has reached its task limit (20 monitors or 100 saved tasks).".into()));}
                            let inserted = store.db.execute("INSERT INTO scheduled_tasks(user_id,project_id,session_id,draft,enabled,next_run,host_id,path) VALUES (?,?,?,?,?,?,?,?)", &[DbValue::Int(user.get()),project.map_or(DbValue::Null,DbValue::Int),session_id,DbValue::Text(encode(draft)?),DbValue::Int(i64::from(draft.enabled)),next,DbValue::Text(host.into()),path]).await?;
                            if let TaskCommand::Monitor { session_id, command: MonitorCommand::Start { interval_seconds, max_checks, .. } } = original {
                                store.db.execute("INSERT INTO monitors(task_id,session_id,interval_seconds,remaining,expires_at) VALUES (?,?,?,?,?)", &[DbValue::Int(inserted.last_insert_rowid),DbValue::Int(*session_id),DbValue::Int(*interval_seconds),DbValue::Int(*max_checks),DbValue::Int(now.saturating_add(86400))]).await?;
                            }
                        }
                    },
                    TaskCommand::SetEnabled{id,revision,enabled} => {
                        let mut draft = target(*id,*revision)?.draft.clone();
                        draft.enabled = *enabled;
                        let next = if *enabled {draft.schedule.next_after(now).map_err(StorageError::InvalidRequest)?.ok_or_else(||StorageError::InvalidRequest("This one-time task has already passed. Edit its date to resume.".into()))?} else {now};
                        if !enabled {store.cancel_scheduled_pending(*id).await?;}
                        store.db.execute("UPDATE scheduled_tasks SET enabled = ?, draft = ?, next_run = CASE WHEN ? THEN ? ELSE next_run END, revision = revision + 1 WHERE id = ?", &[DbValue::Int(i64::from(*enabled)),DbValue::Text(encode(&draft)?),DbValue::Int(i64::from(*enabled)),DbValue::Int(next),DbValue::Int(*id)]).await?;
                    },
                    TaskCommand::Delete{id,revision} => {target(*id,*revision)?;store.cancel_scheduled_pending(*id).await?;store.db.execute("DELETE FROM scheduled_tasks WHERE id = ?", &[DbValue::Int(*id)]).await?;},
                }
                if let TaskCommand::Monitor { command: MonitorCommand::Cancel { id, .. }, .. } = original {
                    store.db.execute("UPDATE monitors SET remaining=0 WHERE task_id=?", &[DbValue::Int(*id)]).await?;
                }
                store.cleanup_monitors(now).await?;
                store.tasks_in_scope(user,project,monitor_session,now).await
            }).await
        })
    }
    /// Resolve a queued task’s override without changing its destination session.
    pub async fn scheduled_prompt_model(
        &self,
        user: UserId,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
    ) -> Result<Option<openwebide_core::ModelSelection>, StorageError> {
        self.get_session(session, user).await?;
        let rows = self.db.execute(
            "SELECT t.draft FROM queued_prompts q LEFT JOIN scheduled_runs r ON r.queued_id=q.id LEFT JOIN scheduled_tasks t ON t.id=r.task_id AND t.user_id=? WHERE q.session_id=? AND q.id=? AND q.revision=?",
            &[DbValue::Int(user.get()), DbValue::Int(session), DbValue::Int(key.id), DbValue::Int(key.revision)],
        ).await?;
        let row = rows.rows.first().ok_or_else(|| {
            StorageError::Conflict("Queued prompt changed or was removed.".into())
        })?;
        row.get_text_opt(0)
            .map(|draft| decode::<TaskDraft>(draft).map(|draft| draft.model))
            .transpose()
            .map(Option::flatten)
    }
    pub(super) async fn cancel_scheduled_pending(&self, task: i64) -> Result<(), StorageError> {
        self.db.execute("UPDATE scheduled_runs SET status = 'cancelled', detail = 'Task changed before delivery' WHERE task_id = ? AND queued_id IS NOT NULL", &[DbValue::Int(task)]).await?;
        self.db.execute("DELETE FROM queued_prompts WHERE id IN (SELECT queued_id FROM scheduled_runs WHERE task_id = ? AND queued_id IS NOT NULL)", &[DbValue::Int(task)]).await?;
        Ok(())
    }
    pub async fn scheduled_session_command(
        &self,
        user: UserId,
        session: i64,
        command: &TaskCommand,
        now: i64,
    ) -> Result<Vec<ScheduledTask>, StorageError> {
        let project = self.get_session(session, user).await?.project_id;
        let mut command = command.clone();
        if let TaskCommand::Monitor { session_id, .. } = &mut command {
            if *session_id != 0 && *session_id != session {
                return Err(StorageError::InvalidRequest(
                    "Monitor must belong to this conversation.".into(),
                ));
            }
            *session_id = session;
        }
        if let TaskCommand::Create { draft } | TaskCommand::Update { draft, .. } = &mut command
            && draft.session_target == SessionTarget::Existing
            && draft.session_id == 0
        {
            draft.session_id = session;
        }
        self.scheduled_command(user, project, &command, None, true, now)
            .await
    }
    async fn scheduled_session(
        &self,
        user: UserId,
        project: Option<i64>,
        draft: &TaskDraft,
        now: i64,
    ) -> Result<i64, StorageError> {
        if draft.session_target == SessionTarget::Existing {
            return Ok(self.get_session(draft.session_id, user).await?.id);
        }
        if draft.session_target == SessionTarget::Latest {
            let rows=self.db.execute("SELECT s.id FROM sessions s WHERE s.user_id=? AND s.project_id IS ? AND s.archived=0 ORDER BY coalesce((SELECT max(created_at) FROM messages WHERE session_id=s.id),s.created_at) DESC,s.id DESC LIMIT 1", &[DbValue::Int(user.get()),project.map_or(DbValue::Null,DbValue::Int)]).await?;
            if let Some(row) = rows.rows.first() {
                return row.get_int(0);
            }
        }
        let primary = self
            .get_user_setting(user, "model_defaults")
            .await?
            .and_then(|value| serde_json::from_str::<openwebide_core::ModelDefaults>(&value).ok())
            .and_then(|defaults| defaults.primary);
        let connection = primary
            .as_ref()
            .map(|selection| selection.server_id)
            .or(self
                .get_user_setting(user, "default_connection")
                .await?
                .and_then(|value| value.parse().ok()))
            .or(self
                .list_connections()
                .await?
                .into_iter()
                .find(|connection| connection.enabled)
                .map(|connection| connection.id))
            .ok_or_else(|| {
                StorageError::InvalidRequest(
                    "Configure a default model before scheduling a new session.".into(),
                )
            })?;
        let prompt = self
            .get_user_setting(user, "default_prompt")
            .await?
            .and_then(|value| value.parse().ok());
        let session = self
            .create_session_unlocked(&draft.title, Some(connection), prompt, project, user, now)
            .await?;
        if let Some(selection) = primary {
            self.set_user_setting(
                user,
                &format!("session_model_{}", session.id),
                &serde_json::json!({"connection_id":connection,"model":selection.model})
                    .to_string(),
            )
            .await?;
        }
        Ok(session.id)
    }
    pub async fn heartbeat_host(&self, host: &ExecutionHost, now: i64) -> Result<(), StorageError> {
        if host.id.is_empty() || host.id.len() > 256 || host.name.len() > 256 {
            return Err(StorageError::InvalidRequest(
                "Invalid execution host".into(),
            ));
        }
        self.db.execute("INSERT INTO execution_hosts(id,name,last_seen) VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,last_seen=excluded.last_seen", &[DbValue::Text(host.id.clone()),DbValue::Text(host.name.clone()),DbValue::Int(now)]).await?;
        Ok(())
    }
    async fn cleanup_monitors(&self, now: i64) -> Result<(), StorageError> {
        // Pending checks can expire; already injected work is never replayed or silently stopped.
        let expired = self
            .db
            .execute(
                "SELECT m.task_id FROM monitors m JOIN scheduled_tasks t ON t.id=m.task_id WHERE t.plugin_owner IS NULL AND m.expires_at<=?",
                &[DbValue::Int(now)],
            )
            .await?;
        for row in expired.rows {
            let id = row.get_int(0)?;
            self.cancel_scheduled_pending(id).await?;
            self.db
                .execute(
                    "UPDATE scheduled_tasks SET enabled=0,next_run=NULL WHERE id=?",
                    &[DbValue::Int(id)],
                )
                .await?;
        }
        let terminal = self.db.execute("SELECT t.id,m.session_id,t.enabled,m.expires_at,r.status,r.detail FROM scheduled_tasks t JOIN monitors m ON m.task_id=t.id LEFT JOIN scheduled_runs r ON r.id=(SELECT max(id) FROM scheduled_runs WHERE task_id=t.id) WHERE t.plugin_owner IS NULL AND (t.enabled=0 OR (t.next_run IS NULL AND r.id IS NOT NULL)) AND NOT EXISTS(SELECT 1 FROM scheduled_runs WHERE task_id=t.id AND status IN ('queued','claimed','running','blocked'))", &[]).await?;
        for row in terminal.rows {
            let id = row.get_int(0)?;
            let status = if row.get_int(3)? <= now {
                "expired"
            } else if row.get_int(2)? == 0 {
                "cancelled"
            } else {
                row.get_text_opt(4).unwrap_or("cancelled")
            };
            if status != "complete" {
                let content = format!(
                    "[Monitor #{id}: {status}] {}",
                    row.get_text_opt(5).unwrap_or("Future checks stopped.")
                );
                let session = row.get_int(1)?;
                match self.ensure_not_rewinding(session).await {
                    Ok(()) => {}
                    Err(StorageError::Conflict(_)) => continue,
                    Err(error) => return Err(error),
                }
                self.insert_interim_message_unlocked(
                    session,
                    Role::Assistant,
                    &content,
                    now,
                    None,
                    None,
                )
                .await?;
            }
            self.db
                .execute(
                    "DELETE FROM scheduled_tasks WHERE id=?",
                    &[DbValue::Int(id)],
                )
                .await?;
        }
        Ok(())
    }
    pub async fn due_scheduled(
        &self,
        host: &ExecutionHost,
        now: i64,
    ) -> Result<Vec<TaskDelivery>, StorageError> {
        self.heartbeat_host(host, now).await?;
        self.db.transaction(|tx| async move {
            let store=Store::new(tx);
            store.db.execute("UPDATE scheduled_runs SET status='cancelled', detail='Session removed before delivery' WHERE status IN ('queued','claimed') AND queued_id IS NULL", &[]).await?;
            // Recover claims only before injection. A consumed prompt is never replayed.
            store.db.execute("UPDATE scheduled_runs SET status='queued',claimed_until=0 WHERE status='claimed' AND claimed_until < ? AND queued_id IS NOT NULL", &[DbValue::Int(now)]).await?;
            store.db.execute("UPDATE scheduled_runs SET status='interrupted',detail='Host stopped; prompt was delivered and will not be replayed' WHERE status IN ('running','blocked') AND claimed_until < ?", &[DbValue::Int(now)]).await?;
            store.db.execute("UPDATE scheduled_tasks SET next_run=? WHERE enabled=1 AND next_run IS NULL AND id IN (SELECT g.task_id FROM goal_workers g JOIN scheduled_runs r ON r.task_id=g.task_id WHERE r.status='interrupted' AND r.id=(SELECT max(id) FROM scheduled_runs WHERE task_id=g.task_id))", &[DbValue::Int(now)]).await?;
            store.cleanup_monitors(now).await?;
            let rows=store.db.execute("SELECT id,user_id,project_id,draft,next_run FROM scheduled_tasks WHERE plugin_owner IS NULL AND host_id=? AND enabled=1 AND next_run <= ? AND NOT EXISTS(SELECT 1 FROM scheduled_runs r WHERE r.task_id=scheduled_tasks.id AND r.status IN ('queued','claimed','running','blocked')) ORDER BY next_run LIMIT 20", &[DbValue::Text(host.id.clone()),DbValue::Int(now)]).await?;
            for row in rows.rows {
                let task=row.get_int(0)?;let user=UserId::new(row.get_int(1)?);let project=row.get_int_opt(2);let draft:TaskDraft=decode(row.get_text(3)?)?;
                let Ok(session)=store.scheduled_session(user,project,&draft,now).await else {
                        store.db.execute("INSERT INTO scheduled_runs(task_id,due_at,status,detail) VALUES (?,?,'failed','Could not create a scheduled session. Check your default model settings.')", &[DbValue::Int(task),DbValue::Int(row.get_int(4)?)]).await?;
                        let next=draft.schedule.next_after(now).map_err(StorageError::InvalidRequest)?.map_or(DbValue::Null,DbValue::Int);
                        store.db.execute("UPDATE scheduled_tasks SET next_run=? WHERE id=?", &[next,DbValue::Int(task)]).await?;
                        continue;
                };
                let goal=store.db.execute("SELECT 1 FROM goal_workers WHERE task_id=?", &[DbValue::Int(task)]).await?;
                let queue=store.list_queued_prompts(user,session).await?;
                if !goal.rows.is_empty() && (!queue.is_empty() || store.session_run_active(user,session,now).await? || store.ensure_not_rewinding(session).await.is_err()) {continue;}

                let monitor = store.db.execute("SELECT remaining FROM monitors WHERE task_id=?", &[DbValue::Int(task)]).await?;
                let content = if monitor.rows.is_empty() {
                    format!("[Scheduled task: {} (#{task})]\n\n{}",draft.title,draft.prompt)
                } else {
                    format!("[Monitor #{task}: ephemeral follow-up check]\nInspect the current state and report the result. If the condition is met, use monitor list then cancel this monitor to stop future checks. Do not start another monitor or sleep to extend this monitor's deadline.\n\n{}", draft.prompt)
                };
                if queue.len()>=openwebide_core::chat_queue::MAX_QUEUED_PROMPTS || queue.iter().map(|entry|entry.content.len()).sum::<usize>().saturating_add(content.len())>openwebide_core::chat_queue::MAX_QUEUE_BYTES {continue;}
                let queued=store.db.execute("INSERT INTO queued_prompts(session_id,content,created_at,guidance) VALUES (?,?,?,0)", &[DbValue::Int(session),DbValue::Text(content),DbValue::Int(now)]).await?;
                store.db.execute("INSERT INTO scheduled_runs(task_id,due_at,status,queued_id,session_id,goal_revision) VALUES (?,?,'queued',?,?,(SELECT revision FROM goal_workers WHERE task_id=?))", &[DbValue::Int(task),DbValue::Int(row.get_int(4)?),DbValue::Int(queued.last_insert_rowid),DbValue::Int(session),DbValue::Int(task)]).await?;
                if !monitor.rows.is_empty() {
                    store.db.execute("UPDATE monitors SET remaining=remaining-1 WHERE task_id=?", &[DbValue::Int(task)]).await?;
                }
                let next=draft.schedule.next_after(now).map_err(StorageError::InvalidRequest)?;
                store.db.execute("UPDATE scheduled_tasks SET next_run=? WHERE id=?", &[next.map_or(DbValue::Null,DbValue::Int),DbValue::Int(task)]).await?;
            }
            let rows=store.db.execute("SELECT r.id,t.id,t.user_id,q.session_id,q.id,q.revision,q.content,q.created_at,t.path FROM scheduled_runs r JOIN scheduled_tasks t ON t.id=r.task_id JOIN queued_prompts q ON q.id=r.queued_id WHERE t.host_id=? AND t.enabled=1 AND r.status='queued' AND q.id=(SELECT id FROM queued_prompts WHERE session_id=q.session_id ORDER BY guidance DESC,id LIMIT 1) AND NOT EXISTS(SELECT 1 FROM session_run_leases l WHERE l.session_id=q.session_id AND l.expires_at>?) ORDER BY r.id LIMIT 10", &[DbValue::Text(host.id.clone()),DbValue::Int(now)]).await?;
            let mut deliveries=Vec::new();
            for row in rows.rows {
                store.db.execute("UPDATE scheduled_runs SET status='claimed',claimed_until=? WHERE id=? AND status='queued'", &[DbValue::Int(now+120),DbValue::Int(row.get_int(0)?)]).await?;
                deliveries.push(TaskDelivery{run_id:row.get_int(0)?,task_id:row.get_int(1)?,user_id:row.get_int(2)?,session_id:row.get_int(3)?,prompt:openwebide_core::QueuedPrompt{scheduled_task:Some(row.get_int(1)?),plugin_run:None,id:row.get_int(4)?,revision:row.get_int(5)?,session_id:row.get_int(3)?,content:row.get_text(6)?.into(),created_at:row.get_int(7)?,guidance:false},binding:row.get_text_opt(8).map(|path|HostBinding{host_id:host.id.clone(),path:path.into()})});
            }
            Ok(deliveries)
        }).await
    }
    /// Host primitive for foreground priority in optional background workflows.
    pub async fn session_run_active(
        &self,
        user: UserId,
        session: i64,
        now: i64,
    ) -> Result<bool, StorageError> {
        self.get_session(session, user).await?;
        let rows = self
            .db
            .execute(
                "SELECT 1 FROM session_run_leases WHERE session_id=? AND expires_at>?",
                &[DbValue::Int(session), DbValue::Int(now)],
            )
            .await?;
        Ok(!rows.rows.is_empty())
    }
    /// Resolve a delivery only for its owning scheduler host.
    pub async fn scheduled_run_session(
        &self,
        host: &str,
        run: i64,
    ) -> Result<Option<(UserId, i64, i64)>, StorageError> {
        let rows = self.db.execute("SELECT t.user_id,m.session_id,m.id FROM scheduled_runs r JOIN scheduled_tasks t ON t.id=r.task_id JOIN messages m ON m.id=r.message_id WHERE r.id=? AND t.host_id=? AND r.status IN ('claimed','running','blocked')", &[DbValue::Int(run), DbValue::Text(host.into())]).await?;
        rows.rows
            .first()
            .map(|row| {
                Ok((
                    UserId::new(row.get_int(0)?),
                    row.get_int(1)?,
                    row.get_int(2)?,
                ))
            })
            .transpose()
    }
    pub async fn scheduled_result(
        &self,
        host: &str,
        result: &DispatchResult,
        now: i64,
    ) -> Result<(), StorageError> {
        self.scheduled_result_evaluated(host, result, None, now)
            .await
    }
    pub async fn scheduled_result_evaluated(
        &self,
        host: &str,
        result: &DispatchResult,
        evaluation: Option<&super::goals::GoalTurnAssessment>,
        now: i64,
    ) -> Result<(), StorageError> {
        if ![
            "queued",
            "running",
            "blocked",
            "complete",
            "cancelled",
            "failed",
        ]
        .contains(&result.status.as_str())
            || result.detail.len() > 1024
        {
            return Err(StorageError::InvalidRequest("Invalid task result".into()));
        }
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
        let updated = store.db.execute("UPDATE scheduled_runs SET status=?, detail=?, claimed_until=?,permission_id=? WHERE id=? AND task_id IN (SELECT id FROM scheduled_tasks WHERE host_id=?) AND status IN ('claimed','running','blocked')", &[DbValue::Text(result.status.clone()),DbValue::Text(result.detail.clone()),DbValue::Int(now+120),result.permission_id.clone().map_or(DbValue::Null,DbValue::Text),DbValue::Int(result.run_id),DbValue::Text(host.into())]).await?;
            if updated.changes == 0 { return Ok(()); }
            store.apply_goal_result(host,result,evaluation,now).await?;
            // A preflight failure must not leave an undeliverable prompt at the
            // head of the chat queue. Delivered prompts already have no queue row.
            if matches!(result.status.as_str(), "failed" | "cancelled" | "complete") {
                store.db.execute("DELETE FROM queued_prompts WHERE id IN (SELECT r.queued_id FROM scheduled_runs r JOIN scheduled_tasks t ON t.id=r.task_id WHERE r.id=? AND t.host_id=? AND r.status=? AND r.queued_id IS NOT NULL)", &[DbValue::Int(result.run_id),DbValue::Text(host.into()),DbValue::Text(result.status.clone())]).await?;
            }
            if result.status == "complete" && updated.changes == 1 {
                store.db.execute("UPDATE scheduled_tasks SET next_run=?+(SELECT interval_seconds FROM monitors WHERE task_id=scheduled_tasks.id) WHERE enabled=1 AND host_id=? AND id IN (SELECT task_id FROM scheduled_runs WHERE id=? AND status='complete') AND EXISTS(SELECT 1 FROM monitors WHERE task_id=scheduled_tasks.id AND remaining>0 AND expires_at>?+interval_seconds)", &[DbValue::Int(now),DbValue::Text(host.into()),DbValue::Int(result.run_id),DbValue::Int(now)]).await?;
            }
            store.cleanup_monitors(now).await?;
            Ok(())
        }).await?;
        Ok(())
    }
    pub async fn session_run_lease(
        &self,
        user: UserId,
        session: i64,
        token: &str,
        release: bool,
        now: i64,
    ) -> Result<(), StorageError> {
        self.get_session(session, user).await?;
        if token.is_empty() || token.len() > 256 {
            return Err(StorageError::InvalidRequest("Invalid run token".into()));
        }
        if release {
            self.db
                .execute(
                    "DELETE FROM session_run_leases WHERE session_id=? AND token=?",
                    &[DbValue::Int(session), DbValue::Text(token.into())],
                )
                .await?;
            return Ok(());
        }
        let result=self.db.execute("INSERT INTO session_run_leases(session_id,token,expires_at) VALUES (?,?,?) ON CONFLICT(session_id) DO UPDATE SET token=excluded.token,expires_at=excluded.expires_at WHERE session_run_leases.token=excluded.token OR session_run_leases.expires_at<=?", &[DbValue::Int(session),DbValue::Text(token.into()),DbValue::Int(now+120),DbValue::Int(now)]).await?;
        if result.changes != 1 {
            return Err(StorageError::Conflict(
                "This chat is already running. Your scheduled prompt will wait.".into(),
            ));
        }
        if !token.starts_with("scheduled-") {
            self.yield_goal_pending(session, now).await?;
        }
        Ok(())
    }
}
fn run_row(row: &crate::db::QueryRow) -> Result<TaskRun, StorageError> {
    Ok(TaskRun {
        session_id: row.get_int_opt(7),
        id: row.get_int(0)?,
        task_id: row.get_int(1)?,
        due_at: row.get_int(2)?,
        status: row.get_text(3)?.into(),
        detail: row.get_text(4)?.into(),
        message_id: row.get_int_opt(5),
        permission_id: row.get_text_opt(6).map(str::to_owned),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::scheduled::Schedule;
    #[test]
    fn session_target_migration_preserves_pending_claims_and_history() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let user = store
                .insert_user("owner", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("Session", None, None, None, user, 0)
                .await
                .unwrap()
                .id;
            let draft = TaskDraft {
                model: None,
                auto_title: false,
                session_target: SessionTarget::Existing,
                session_id: session,
                title: "Task".into(),
                prompt: "Prompt".into(),
                schedule: Schedule::Once { at: 60 },
                enabled: true,
            };
            let id = store
                .scheduled_command(user, None, &TaskCommand::Create { draft }, None, false, 0)
                .await
                .unwrap()[0]
                .id;
            let host = ExecutionHost {
                id: "server".into(),
                name: "Host".into(),
                last_seen: 0,
            };
            let delivery = store.due_scheduled(&host, 60).await.unwrap().remove(0);
            // Recreate the deployed pre-target schema, retaining an in-flight claim.
            for sql in [
                "CREATE TABLE scheduled_tasks_legacy (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, revision INTEGER NOT NULL DEFAULT 1, draft TEXT NOT NULL, enabled INTEGER NOT NULL, next_run INTEGER, host_id TEXT NOT NULL, path TEXT)",
                "INSERT INTO scheduled_tasks_legacy SELECT id,user_id,project_id,session_id,revision,draft,enabled,next_run,host_id,path FROM scheduled_tasks",
                "CREATE TABLE scheduled_runs_legacy (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL REFERENCES scheduled_tasks_legacy(id) ON DELETE CASCADE, due_at INTEGER NOT NULL, status TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '', permission_id TEXT, queued_id INTEGER REFERENCES queued_prompts(id) ON DELETE SET NULL, message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL, claimed_until INTEGER NOT NULL DEFAULT 0, UNIQUE(task_id, due_at))",
                "INSERT INTO scheduled_runs_legacy SELECT id,task_id,due_at,status,detail,permission_id,queued_id,message_id,claimed_until FROM scheduled_runs",
                "DROP TABLE scheduled_runs",
                "DROP TABLE scheduled_tasks",
                "ALTER TABLE scheduled_tasks_legacy RENAME TO scheduled_tasks",
                "ALTER TABLE scheduled_runs_legacy RENAME TO scheduled_runs",
                "PRAGMA user_version = 37",
            ] {
                store.db.execute(sql, &[]).await.unwrap();
            }
            store.migrate().await.unwrap();
            store.migrate().await.unwrap();
            let tasks = store.scheduled_tasks(user, None, 61).await.unwrap();
            assert_eq!(tasks[0].id, id);
            assert_eq!(
                tasks[0].last_run.as_ref().unwrap().session_id,
                Some(session)
            );
            assert_eq!(
                store.list_queued_prompts(user, session).await.unwrap()[0].key(),
                delivery.prompt.key()
            );
            store
                .session_run_lease(
                    user,
                    session,
                    &format!("scheduled-{}", delivery.run_id),
                    false,
                    61,
                )
                .await
                .unwrap();
            store
                .consume_queued_prompt(
                    user,
                    session,
                    delivery.prompt.key(),
                    &delivery.prompt.content,
                    61,
                )
                .await
                .unwrap();
            assert_eq!(store.list_messages(session).await.unwrap().len(), 1);
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
    fn automatic_session_targets_resolve_at_delivery_in_each_scope_and_keep_defaults() {
        futures::executor::block_on(async {
            for mode in [
                Some(WorkspaceMode::Local),
                Some(WorkspaceMode::Remote),
                None,
            ] {
                for target in [SessionTarget::New, SessionTarget::Latest] {
                    let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                    store.migrate().await.unwrap();
                    let user = store
                        .insert_user("owner", "hash", UserRole::Admin, 0)
                        .await
                        .unwrap()
                        .id;
                    let other = store
                        .insert_user("other", "hash", UserRole::User, 0)
                        .await
                        .unwrap()
                        .id;
                    let project = if let Some(mode) = mode {
                        Some(
                            store
                                .create_project(
                                    &NewProject {
                                        name: "Project".into(),
                                        mode,
                                        path: Some("project".into()),
                                    },
                                    user,
                                    0,
                                )
                                .await
                                .unwrap()
                                .id,
                        )
                    } else {
                        None
                    };
                    let connection = store
                        .insert_connection(&NewConnection {
                            name: "Default".into(),
                            kind: ProviderKind::Ollama,
                            base_url: "http://localhost:11434".into(),
                            model: Some("default".into()),
                            context_limit: None,
                        })
                        .await
                        .unwrap()
                        .id;
                    store.set_user_setting(user,"model_defaults",&serde_json::json!({"primary":{"server_id":connection,"model":"preferred"}}).to_string()).await.unwrap();
                    let host = ExecutionHost {
                        id: if mode == Some(WorkspaceMode::Local) {
                            "paired"
                        } else {
                            "server"
                        }
                        .into(),
                        name: "Host".into(),
                        last_seen: 0,
                    };
                    let binding = HostBinding {
                        host_id: host.id.clone(),
                        path: "project".into(),
                    };
                    let draft = TaskDraft {
                        model: None,
                        auto_title: false,
                        session_target: target,
                        session_id: 0,
                        title: "Review".into(),
                        prompt: "Review changes".into(),
                        schedule: Schedule::Cron {
                            expression: "* * * * *".into(),
                            timezone: "UTC".into(),
                        },
                        enabled: true,
                    };
                    let id = store
                        .scheduled_command(
                            user,
                            project,
                            &TaskCommand::Create { draft },
                            Some(&binding),
                            false,
                            0,
                        )
                        .await
                        .unwrap()[0]
                        .id;
                    assert!(
                        store.list_sessions(user).await.unwrap().is_empty(),
                        "Saving a task must not create empty sessions"
                    );
                    let first = store.due_scheduled(&host, 60).await.unwrap().remove(0);
                    let session = store.get_session(first.session_id, user).await.unwrap();
                    assert_eq!(session.project_id, project);
                    assert_eq!(session.connection_id, Some(connection));
                    let selection: serde_json::Value = serde_json::from_str(
                        &store
                            .get_user_setting(user, &format!("session_model_{}", session.id))
                            .await
                            .unwrap()
                            .unwrap(),
                    )
                    .unwrap();
                    assert_eq!(selection["model"], "preferred");
                    store
                        .session_run_lease(
                            user,
                            session.id,
                            &format!("scheduled-{}", first.run_id),
                            false,
                            60,
                        )
                        .await
                        .unwrap();
                    store
                        .consume_queued_prompt(
                            user,
                            session.id,
                            first.prompt.key(),
                            &first.prompt.content,
                            60,
                        )
                        .await
                        .unwrap();
                    store
                        .scheduled_result(
                            &host.id,
                            &DispatchResult {
                                run_id: first.run_id,
                                status: "complete".into(),
                                detail: String::new(),
                                permission_id: None,
                            },
                            60,
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        store.scheduled_tasks(user, project, 60).await.unwrap()[0]
                            .last_run
                            .as_ref()
                            .unwrap()
                            .session_id,
                        Some(session.id)
                    );
                    // Deleting a generated session must not remove its task definition.
                    store
                        .db
                        .execute(
                            "DELETE FROM sessions WHERE id=?",
                            &[DbValue::Int(session.id)],
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        store.scheduled_tasks(user, project, 61).await.unwrap()[0].id,
                        id
                    );
                    let latest = store
                        .create_session("Latest", Some(connection), None, project, user, 80)
                        .await
                        .unwrap()
                        .id;
                    let archived = store
                        .create_session("Archived", Some(connection), None, project, user, 90)
                        .await
                        .unwrap()
                        .id;
                    store
                        .set_session_preferences(
                            user,
                            archived,
                            &openwebide_core::SessionPreferences {
                                archived: Some(true),
                                pinned: None,
                            },
                        )
                        .await
                        .unwrap();
                    store
                        .create_session("Other user", Some(connection), None, None, other, 100)
                        .await
                        .unwrap();
                    let second = store.due_scheduled(&host, 120).await.unwrap().remove(0);
                    assert_ne!(second.session_id, archived);
                    if target == SessionTarget::Latest {
                        assert_eq!(second.session_id, latest);
                    } else {
                        assert_ne!(second.session_id, latest);
                    }
                    assert!(
                        store.due_scheduled(&host, 121).await.unwrap().is_empty(),
                        "Claim recovery must reuse the same pending session"
                    );
                }
            }
        });
    }
    #[test]
    fn scheduled_contract_covers_both_adapters_and_projectless_recovery() {
        futures::executor::block_on(async {
            for mode in [
                Some(WorkspaceMode::Local),
                Some(WorkspaceMode::Remote),
                None,
            ] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("tasks", "hash", UserRole::Admin, 0)
                    .await
                    .unwrap()
                    .id;
                let other = store
                    .insert_user("other", "hash", UserRole::User, 0)
                    .await
                    .unwrap()
                    .id;
                let project = if let Some(mode) = mode {
                    Some(
                        store
                            .create_project(
                                &NewProject {
                                    name: "project".into(),
                                    mode,
                                    path: Some("project".into()),
                                },
                                user,
                                0,
                            )
                            .await
                            .unwrap()
                            .id,
                    )
                } else {
                    None
                };
                let session = store
                    .create_session("chat", None, None, project, user, 0)
                    .await
                    .unwrap()
                    .id;
                let local = mode == Some(WorkspaceMode::Local);
                let host = ExecutionHost {
                    id: if local { "paired" } else { "server" }.into(),
                    name: "Host".into(),
                    last_seen: 0,
                };
                let binding = HostBinding {
                    host_id: host.id.clone(),
                    path: "project".into(),
                };
                let draft = TaskDraft {
                    model: None,
                    session_target: SessionTarget::Existing,
                    auto_title: false,
                    title: "Review".into(),
                    prompt: "Review changes".into(),
                    session_id: session,
                    schedule: Schedule::Cron {
                        expression: "* * * * *".into(),
                        timezone: "UTC".into(),
                    },
                    enabled: true,
                };
                let create = TaskCommand::Create {
                    draft: draft.clone(),
                };
                if local {
                    assert!(
                        store
                            .scheduled_command(user, project, &create, None, false, 0)
                            .await
                            .is_err()
                    );
                }
                let saved = store
                    .scheduled_command(user, project, &create, local.then_some(&binding), false, 0)
                    .await
                    .unwrap();
                let id = saved[0].id;
                assert!(
                    store
                        .scheduled_session_command(other, session, &TaskCommand::List, 0)
                        .await
                        .is_err()
                );
                store
                    .session_run_lease(user, session, "manual", false, 1000)
                    .await
                    .unwrap();
                assert!(store.due_scheduled(&host, 1000).await.unwrap().is_empty());
                let queued = store.list_queued_prompts(user, session).await.unwrap();
                assert_eq!(queued.len(), 1);
                assert_eq!(queued[0].scheduled_task, Some(id));
                assert!(
                    store
                        .session_run_lease(user, session, "second", false, 1001)
                        .await
                        .is_err()
                );
                store
                    .session_run_lease(user, session, "manual", true, 1001)
                    .await
                    .unwrap();
                let first = store.due_scheduled(&host, 1001).await.unwrap();
                assert_eq!(first.len(), 1);
                assert_eq!(first[0].binding.is_some(), local);
                assert!(store.due_scheduled(&host, 1002).await.unwrap().is_empty());
                // Pre-injection claim expiry safely retries the same prompt, once.
                let recovered = store.due_scheduled(&host, 1122).await.unwrap();
                assert_eq!(recovered.len(), 1);
                assert_eq!(recovered[0].run_id, first[0].run_id);
                let token = format!("scheduled-{}", first[0].run_id);
                store
                    .session_run_lease(user, session, &token, false, 1122)
                    .await
                    .unwrap();
                let message = store
                    .consume_queued_prompt(
                        user,
                        session,
                        recovered[0].prompt.key(),
                        &recovered[0].prompt.content,
                        1122,
                    )
                    .await
                    .unwrap();
                assert!(message.content.starts_with("[Scheduled task:"));
                assert!(
                    store
                        .consume_queued_prompt(
                            user,
                            session,
                            recovered[0].prompt.key(),
                            &recovered[0].prompt.content,
                            1122
                        )
                        .await
                        .is_err()
                );
                // After injection a crash records interruption; no duplicate delivery.
                assert!(store.due_scheduled(&host, 1123).await.unwrap().is_empty());
                let run = store.scheduled_tasks(user, project, 1123).await.unwrap()[0]
                    .last_run
                    .clone()
                    .unwrap();
                assert_eq!(run.message_id, Some(message.id));
                store
                    .scheduled_command(
                        user,
                        project,
                        &TaskCommand::SetEnabled {
                            id,
                            revision: 1,
                            enabled: false,
                        },
                        None,
                        false,
                        1123,
                    )
                    .await
                    .unwrap();
                assert!(store.due_scheduled(&host, 1300).await.unwrap().is_empty());
                let tasks = store.scheduled_tasks(user, project, 1300).await.unwrap();
                assert_eq!(tasks[0].last_run.as_ref().unwrap().status, "interrupted");
                assert!(
                    store
                        .scheduled_command(
                            user,
                            project,
                            &TaskCommand::Delete { id, revision: 1 },
                            None,
                            false,
                            1300
                        )
                        .await
                        .is_err()
                );
                store
                    .scheduled_command(
                        user,
                        project,
                        &TaskCommand::Delete { id, revision: 2 },
                        None,
                        false,
                        1300,
                    )
                    .await
                    .unwrap();
                assert_eq!(store.list_messages(session).await.unwrap().len(), 1);
            }
        });
    }
    #[test]
    fn paused_claims_are_cancelled_and_failed_preflight_does_not_block_the_queue() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let user = store
                .insert_user("tasks", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("chat", None, None, None, user, 0)
                .await
                .unwrap()
                .id;
            let draft = TaskDraft {
                model: None,
                session_target: SessionTarget::Existing,
                auto_title: false,
                title: "Recurring".into(),
                prompt: "Prompt".into(),
                session_id: session,
                schedule: Schedule::Cron {
                    expression: "* * * * *".into(),
                    timezone: "UTC".into(),
                },
                enabled: true,
            };
            let id = store
                .scheduled_command(user, None, &TaskCommand::Create { draft }, None, false, 0)
                .await
                .unwrap()[0]
                .id;
            let host = ExecutionHost {
                id: "server".into(),
                name: "Host".into(),
                last_seen: 0,
            };
            assert_eq!(store.due_scheduled(&host, 60).await.unwrap().len(), 1);
            let tasks = store
                .scheduled_command(
                    user,
                    None,
                    &TaskCommand::SetEnabled {
                        id,
                        revision: 1,
                        enabled: false,
                    },
                    None,
                    false,
                    60,
                )
                .await
                .unwrap();
            assert_eq!(tasks[0].last_run.as_ref().unwrap().status, "cancelled");
            assert!(
                store
                    .list_queued_prompts(user, session)
                    .await
                    .unwrap()
                    .is_empty()
            );
            store
                .scheduled_command(
                    user,
                    None,
                    &TaskCommand::SetEnabled {
                        id,
                        revision: 2,
                        enabled: true,
                    },
                    None,
                    false,
                    61,
                )
                .await
                .unwrap();
            let deliveries = store.due_scheduled(&host, 120).await.unwrap();
            assert_eq!(deliveries.len(), 1);
            store
                .scheduled_result(
                    "server",
                    &DispatchResult {
                        run_id: deliveries[0].run_id,
                        status: "failed".into(),
                        detail: "Model unavailable".into(),
                        permission_id: None,
                    },
                    120,
                )
                .await
                .unwrap();
            assert!(
                store
                    .list_queued_prompts(user, session)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(store.due_scheduled(&host, 180).await.unwrap().len(), 1);
        });
    }
    #[test]
    fn job_changes_revoke_claims_before_injection_and_one_time_runs_stay_once() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let user = store
                .insert_user("tasks", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("chat", None, None, None, user, 0)
                .await
                .unwrap()
                .id;
            let draft = TaskDraft {
                model: None,
                session_target: SessionTarget::Existing,
                auto_title: false,
                title: "One".into(),
                prompt: "Prompt".into(),
                session_id: session,
                schedule: Schedule::Once { at: 60 },
                enabled: true,
            };
            let tasks = store
                .scheduled_command(user, None, &TaskCommand::Create { draft }, None, false, 0)
                .await
                .unwrap();
            let host = ExecutionHost {
                id: "server".into(),
                name: "Host".into(),
                last_seen: 0,
            };
            let deliveries = store.due_scheduled(&host, 1000).await.unwrap();
            assert_eq!(deliveries.len(), 1);
            store
                .scheduled_command(
                    user,
                    None,
                    &TaskCommand::Delete {
                        id: tasks[0].id,
                        revision: 1,
                    },
                    None,
                    false,
                    1000,
                )
                .await
                .unwrap();
            assert!(
                store
                    .list_queued_prompts(user, session)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                store
                    .consume_queued_prompt(
                        user,
                        session,
                        deliveries[0].prompt.key(),
                        &deliveries[0].prompt.content,
                        1000
                    )
                    .await
                    .is_err()
            );
            assert!(store.due_scheduled(&host, 2000).await.unwrap().is_empty());
        });
    }
}
