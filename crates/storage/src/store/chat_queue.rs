//! FIFO delivery is committed together with the user message, never before it.
use super::*;
use openwebide_core::{
    QueuedPrompt, QueuedPromptKey,
    chat_queue::{MAX_QUEUE_BYTES, MAX_QUEUED_PROMPTS, validate_content},
};

fn queued_from_row(row: &crate::db::QueryRow) -> Result<QueuedPrompt, StorageError> {
    Ok(QueuedPrompt {
        scheduled_task: row.get_int_opt(6),
        plugin_run: row.get_int_opt(7),
        id: row.get_int(0)?,
        session_id: row.get_int(1)?,
        revision: row.get_int(2)?,
        content: row.get_text(3)?.into(),
        created_at: row.get_int(4)?,
        guidance: row.get_int(5)? != 0,
    })
}

impl<D: Db> Store<D> {
    pub async fn list_queued_prompts(
        &self,
        user: UserId,
        session: i64,
    ) -> Result<Vec<QueuedPrompt>, StorageError> {
        self.get_session(session, user).await?;
        let result = self.db.execute("SELECT id, session_id, revision, content, created_at, guidance, (SELECT task_id FROM scheduled_runs WHERE queued_id=queued_prompts.id), (SELECT id FROM plugin_runs WHERE queued_id=queued_prompts.id) FROM queued_prompts WHERE session_id = ? ORDER BY guidance DESC, id", &[DbValue::Int(session)]).await?;
        result.rows.iter().map(queued_from_row).collect()
    }

    pub async fn enqueue_prompt(
        &self,
        user: UserId,
        session: i64,
        content: &str,
        created_at: i64,
    ) -> Result<QueuedPrompt, StorageError> {
        self.queue_prompt(user, session, content, created_at, false)
            .await
    }
    pub async fn enqueue_guidance(
        &self,
        user: UserId,
        session: i64,
        content: &str,
        created_at: i64,
    ) -> Result<QueuedPrompt, StorageError> {
        self.queue_prompt(user, session, content, created_at, true)
            .await
    }
    async fn queue_prompt(
        &self,
        user: UserId,
        session: i64,
        content: &str,
        created_at: i64,
        guidance: bool,
    ) -> Result<QueuedPrompt, StorageError> {
        validate_content(content).map_err(StorageError::InvalidValue)?;
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                store.ensure_not_rewinding(session).await?;
                store.get_session(session, user).await?;
                store.yield_goal_pending(session, created_at).await?;
                store
                    .enqueue_prompt_in_transaction(user, session, content, created_at, guidance)
                    .await
            })
            .await
    }

    /// Raw queue primitive; callers choose foreground-yield policy above it.
    pub(super) async fn enqueue_prompt_in_transaction(
        &self,
        user: UserId,
        session: i64,
        content: &str,
        created_at: i64,
        guidance: bool,
    ) -> Result<QueuedPrompt, StorageError> {
        validate_content(content).map_err(StorageError::InvalidValue)?;
        self.ensure_not_rewinding(session).await?;
        self.get_session(session, user).await?;
        let queue = self.list_queued_prompts(user, session).await?;
        if queue.len() >= MAX_QUEUED_PROMPTS
            || queue
                .iter()
                .map(|entry| entry.content.len())
                .sum::<usize>()
                .saturating_add(content.len())
                > MAX_QUEUE_BYTES
        {
            return Err(StorageError::Conflict("The queue holds at most 8 prompts and 16 MiB. Remove a prompt before adding another.".into()));
        }
        let result = self.db.execute("INSERT INTO queued_prompts (session_id, content, created_at, guidance) VALUES (?, ?, ?, ?)", &[DbValue::Int(session), DbValue::Text(content.into()), DbValue::Int(created_at), DbValue::Int(i64::from(guidance))]).await?;
        Ok(QueuedPrompt {
            scheduled_task: None,
            plugin_run: None,
            id: result.last_insert_rowid,
            session_id: session,
            revision: 1,
            content: content.into(),
            created_at,
            guidance,
        })
    }

    pub async fn update_queued_prompt(
        &self,
        user: UserId,
        session: i64,
        key: QueuedPromptKey,
        content: &str,
    ) -> Result<QueuedPrompt, StorageError> {
        validate_content(content).map_err(StorageError::InvalidValue)?;
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            let queue = store.list_queued_prompts(user, session).await?;
            let prompt = queue.iter().find(|prompt| prompt.key() == key).ok_or_else(|| StorageError::Conflict("Queued prompt changed or was already sent. Refresh the queue.".into()))?;
            if prompt.scheduled_task.is_some(){return Err(StorageError::InvalidRequest("Edit scheduled prompts in Tasks.".into()));}
            if !store.db.execute("SELECT 1 FROM plugin_runs WHERE queued_id=?", &[DbValue::Int(key.id)]).await?.rows.is_empty() { return Err(StorageError::InvalidRequest("Plugin submissions cannot be edited in the prompt queue; cancel and submit again".into())); }
            if queue.iter().filter(|entry| entry.id != key.id).map(|entry| entry.content.len()).sum::<usize>().saturating_add(content.len()) > MAX_QUEUE_BYTES {
                return Err(StorageError::Conflict("The queue is limited to 16 MiB.".into()));
            }
            store.db.execute("UPDATE queued_prompts SET content = ?, revision = revision + 1 WHERE session_id = ? AND id = ? AND revision = ?", &[DbValue::Text(content.into()), DbValue::Int(session), DbValue::Int(key.id), DbValue::Int(key.revision)]).await?;
            Ok(QueuedPrompt { content: content.into(), revision: key.revision + 1, ..prompt.clone() })
        }).await
    }

    pub async fn remove_queued_prompt(
        &self,
        user: UserId,
        session: i64,
        key: QueuedPromptKey,
    ) -> Result<(), StorageError> {
        self.get_session(session, user).await?;
        self.pause_removed_goal_prompt(user, session, key).await?;
        self.db.execute("UPDATE scheduled_runs SET status='cancelled',detail='Queued prompt removed' WHERE queued_id=? AND queued_id IN (SELECT id FROM queued_prompts WHERE session_id=? AND revision=?) AND status IN ('queued','claimed')", &[DbValue::Int(key.id),DbValue::Int(session),DbValue::Int(key.revision)]).await?;
        let result = self
            .db
            .execute(
                "DELETE FROM queued_prompts WHERE session_id = ? AND id = ? AND revision = ?",
                &[
                    DbValue::Int(session),
                    DbValue::Int(key.id),
                    DbValue::Int(key.revision),
                ],
            )
            .await?;
        if result.changes != 1 {
            return Err(StorageError::Conflict(
                "Queued prompt changed or was already sent. Refresh the queue.".into(),
            ));
        }
        Ok(())
    }

    /// Exactly one competing sender can commit this revision. Validation/run
    /// planning happens before this call, so failure leaves the prompt pending.
    pub async fn consume_queued_prompt(
        &self,
        user: UserId,
        session: i64,
        key: QueuedPromptKey,
        content: &str,
        created_at: i64,
    ) -> Result<ChatMessage, StorageError> {
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            store.ensure_not_rewinding(session).await?;
            let queue = store.list_queued_prompts(user, session).await?;
            let prompt = queue.first().ok_or_else(|| StorageError::Conflict("Queued prompt was already sent or removed.".into()))?;
            if prompt.key() != key || prompt.content != content {
                return Err(StorageError::Conflict("Queued prompt changed. Refresh the queue before sending.".into()));
            }
            if prompt.scheduled_task.is_some() {
                let allowed=store.db.execute("SELECT 1 FROM scheduled_runs r JOIN session_run_leases l ON l.session_id=? WHERE r.queued_id=? AND r.status='claimed' AND l.token=('scheduled-' || r.id) AND l.expires_at>?", &[DbValue::Int(session),DbValue::Int(key.id),DbValue::Int(created_at)]).await?;
                if allowed.rows.is_empty(){return Err(StorageError::Conflict("Scheduled prompts are delivered by their execution host.".into()));}
            }
            let plugin = store.db.execute("SELECT r.id FROM plugin_runs r WHERE r.queued_id=?", &[DbValue::Int(key.id)]).await?;
            if let Some(row) = plugin.rows.first() {
                let allowed = store.db.execute("SELECT 1 FROM plugin_runs r JOIN session_run_leases l ON l.session_id=r.session_id WHERE r.id=? AND r.state='leased' AND l.token=('plugin-run-' || r.id || '-' || r.lease) AND l.expires_at>? AND r.lease_expires_at>?", &[DbValue::Int(row.get_int(0)?), DbValue::Int(created_at), DbValue::Int(created_at)]).await?;
                if allowed.rows.is_empty() { return Err(StorageError::Conflict("Plugin prompts are delivered by their execution host".into())); }
            }
            let message = store.insert_interim_message_unlocked(session, Role::User, content, created_at, None, None).await?;
            store.db.execute("UPDATE scheduled_runs SET status='running',message_id=?,claimed_until=? WHERE queued_id=? AND status IN ('queued','claimed')", &[DbValue::Int(message.id),DbValue::Int(created_at+120),DbValue::Int(key.id)]).await?;
            store.db.execute("UPDATE plugin_runs SET state='running',revision=revision+1,message_id=? WHERE queued_id=? AND state='leased'", &[DbValue::Int(message.id), DbValue::Int(key.id)]).await?;
            store.db.execute("DELETE FROM queued_prompts WHERE session_id = ? AND id = ? AND revision = ?", &[DbValue::Int(session), DbValue::Int(key.id), DbValue::Int(key.revision)]).await?;
            Ok(message)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use futures::executor::block_on;

    #[test]
    fn queue_contract_is_owned_fifo_durable_and_atomic_in_every_workspace() {
        block_on(async {
            for mode in [
                Some(WorkspaceMode::Local),
                Some(WorkspaceMode::Remote),
                None,
            ] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                store.migrate().await.unwrap();
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("owner", "hash", UserRole::Admin, 1)
                    .await
                    .unwrap()
                    .id;
                let other = store
                    .insert_user("other", "hash", UserRole::User, 1)
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
                                    path: Some("repos/test".into()),
                                },
                                user,
                                1,
                            )
                            .await
                            .unwrap()
                            .id,
                    )
                } else {
                    None
                };
                let session = store
                    .create_session("queue", None, None, project, user, 1)
                    .await
                    .unwrap();
                let sibling = store
                    .create_session("sibling", None, None, project, user, 1)
                    .await
                    .unwrap();
                let image = openwebide_core::PromptImage::from_bytes(
                    "test.png".into(),
                    b"\x89PNG\r\n\x1a\n",
                )
                .unwrap();
                let content = openwebide_core::PromptContent {
                    text: "next step".into(),
                    images: vec![image],
                    ..Default::default()
                }
                .encode()
                .unwrap();
                let first = store
                    .enqueue_prompt(user, session.id, &content, 2)
                    .await
                    .unwrap();
                let second = store
                    .enqueue_prompt(user, session.id, "later", 3)
                    .await
                    .unwrap();
                assert_eq!(
                    store.list_queued_prompts(user, session.id).await.unwrap(),
                    [first.clone(), second.clone()]
                );
                assert!(store.list_messages(session.id).await.unwrap().is_empty());
                assert!(store.list_queued_prompts(other, session.id).await.is_err());
                assert!(
                    store
                        .enqueue_prompt(other, session.id, "foreign", 4)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .update_queued_prompt(other, session.id, first.key(), "foreign")
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .remove_queued_prompt(other, session.id, first.key())
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .consume_queued_prompt(other, session.id, first.key(), &content, 4)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .consume_queued_prompt(user, sibling.id, first.key(), &content, 4)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .consume_queued_prompt(user, session.id, second.key(), "later", 4)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .consume_queued_prompt(
                            user,
                            session.id,
                            first.key(),
                            "changed in another browser",
                            4
                        )
                        .await
                        .is_err()
                );
                let updated = store
                    .update_queued_prompt(user, session.id, second.key(), "updated later")
                    .await
                    .unwrap();
                assert_eq!(updated.revision, second.revision + 1);
                assert!(
                    store
                        .update_queued_prompt(user, session.id, second.key(), "stale")
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .remove_queued_prompt(user, session.id, second.key())
                        .await
                        .is_err()
                );
                store.db.execute("CREATE TRIGGER fail_queue_delete BEFORE DELETE ON queued_prompts BEGIN SELECT RAISE(ABORT, 'failure'); END", &[]).await.unwrap();
                assert!(
                    store
                        .consume_queued_prompt(user, session.id, first.key(), &content, 4)
                        .await
                        .is_err()
                );
                assert!(
                    store.list_messages(session.id).await.unwrap().is_empty(),
                    "delivery must roll back if removal fails"
                );
                assert_eq!(
                    store.list_queued_prompts(user, session.id).await.unwrap()[0],
                    first
                );
                store
                    .db
                    .execute("DROP TRIGGER fail_queue_delete", &[])
                    .await
                    .unwrap();
                let (a, b) = futures::join!(
                    store.consume_queued_prompt(user, session.id, first.key(), &content, 5),
                    store.consume_queued_prompt(user, session.id, first.key(), &content, 5)
                );
                assert_ne!(
                    a.is_ok(),
                    b.is_ok(),
                    "only one competing sender may deliver a prompt"
                );
                let delivered = store.list_messages(session.id).await.unwrap();
                assert_eq!(delivered.len(), 1);
                assert_eq!(delivered[0].content, content);
                assert_eq!(
                    store
                        .list_queued_prompts(user, session.id)
                        .await
                        .unwrap()
                        .as_slice(),
                    std::slice::from_ref(&updated)
                );
                store
                    .remove_queued_prompt(user, session.id, updated.key())
                    .await
                    .unwrap();
                assert!(
                    store
                        .consume_queued_prompt(user, session.id, updated.key(), &updated.content, 6)
                        .await
                        .is_err()
                );
                store
                    .enqueue_prompt(user, session.id, "delete with session", 7)
                    .await
                    .unwrap();
                store.delete_session(session.id, user).await.unwrap();
                assert!(
                    store
                        .db
                        .execute("SELECT 1 FROM queued_prompts", &[])
                        .await
                        .unwrap()
                        .rows
                        .is_empty()
                );
            }
        });
    }

    #[test]
    fn queue_enforces_capacity_validation_and_edit_limits_without_losing_prompts() {
        block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let user = store
                .insert_user("owner", "hash", UserRole::Admin, 1)
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("queue", None, None, None, user, 1)
                .await
                .unwrap()
                .id;
            for invalid in [" ", "[Open WebIDE prompt]\ninvalid"] {
                assert!(
                    store
                        .enqueue_prompt(user, session, invalid, 1)
                        .await
                        .is_err()
                );
            }
            for _ in 0..MAX_QUEUED_PROMPTS {
                store
                    .enqueue_prompt(user, session, "pending", 2)
                    .await
                    .unwrap();
            }
            assert!(
                store
                    .enqueue_prompt(user, session, "overflow", 2)
                    .await
                    .is_err()
            );
            let all = store.list_queued_prompts(user, session).await.unwrap();
            for prompt in all {
                store
                    .remove_queued_prompt(user, session, prompt.key())
                    .await
                    .unwrap();
            }
            let large = "x".repeat(6 * 1024 * 1024);
            let first = store
                .enqueue_prompt(user, session, &large, 3)
                .await
                .unwrap();
            store
                .enqueue_prompt(user, session, &large, 3)
                .await
                .unwrap();
            let small = store
                .enqueue_prompt(user, session, "small", 3)
                .await
                .unwrap();
            assert!(
                store
                    .enqueue_prompt(user, session, &large, 3)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .update_queued_prompt(user, session, small.key(), &large)
                    .await
                    .is_err()
            );
            assert_eq!(
                store.list_queued_prompts(user, session).await.unwrap()[0],
                first
            );
            assert_eq!(
                store.list_queued_prompts(user, session).await.unwrap()[2],
                small
            );
        });
    }
}

#[cfg(test)]
mod guidance_tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    #[test]
    fn guidance_precedes_pending_followups_without_reordering_other_prompts() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let user = store
                .insert_user("owner", "hash", UserRole::Admin, 1)
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("chat", None, None, None, user, 1)
                .await
                .unwrap()
                .id;
            let first = store
                .enqueue_prompt(user, session, "first followup", 2)
                .await
                .unwrap();
            let second = store
                .enqueue_prompt(user, session, "second followup", 3)
                .await
                .unwrap();
            let guidance = store
                .enqueue_guidance(user, session, "new guidance", 4)
                .await
                .unwrap();
            assert!(guidance.guidance);
            assert_eq!(
                store.list_queued_prompts(user, session).await.unwrap(),
                [guidance.clone(), first.clone(), second.clone()]
            );
            assert!(
                store
                    .consume_queued_prompt(user, session, first.key(), &first.content, 5)
                    .await
                    .is_err()
            );
            store
                .consume_queued_prompt(user, session, guidance.key(), &guidance.content, 5)
                .await
                .unwrap();
            assert_eq!(
                store.list_queued_prompts(user, session).await.unwrap(),
                [first, second]
            );
        });
    }
}
