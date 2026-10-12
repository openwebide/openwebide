//! Durable host-operation journal. Prepared plans are immutable; claims lock the host.
use super::*;
use openwebide_core::host_admin::{HostOperation, HostPlan, MAX_OUTPUT_BYTES, OperationState};

fn encode(operation: &HostOperation) -> Result<String, StorageError> {
    serde_json::to_string(operation).map_err(|error| StorageError::InvalidValue(error.to_string()))
}
fn state_name(state: OperationState) -> &'static str {
    match state {
        OperationState::Prepared => "prepared",
        OperationState::Running => "running",
        OperationState::AwaitingReconnect => "awaiting_reconnect",
        OperationState::Succeeded => "succeeded",
        OperationState::Failed => "failed",
        OperationState::Interrupted => "interrupted",
    }
}
fn decode(row: &crate::db::QueryRow) -> Result<HostOperation, StorageError> {
    let mut operation: HostOperation = serde_json::from_str(row.get_text(1)?)
        .map_err(|error| StorageError::InvalidValue(error.to_string()))?;
    operation.id = row.get_int(0)?;
    Ok(operation)
}
impl<D: Db> Store<D> {
    pub async fn require_host_session(&self, user: i64, session: i64) -> Result<(), StorageError> {
        if self
            .get_user(UserId::new(user))
            .await?
            .is_none_or(|user| user.role != UserRole::Admin)
        {
            return Err(StorageError::InvalidRequest(
                "Host administration requires an administrator account.".into(),
            ));
        }
        if self
            .get_session(session, UserId::new(user))
            .await?
            .project_id
            .is_some()
        {
            return Err(StorageError::InvalidValue(
                "Host administration is available only in project-less chat.".into(),
            ));
        }
        Ok(())
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Journal commands preserve explicit ownership, operation and host identity fields"
    )]
    pub async fn prepare_host_operation(
        &self,
        user: i64,
        session: i64,
        request: &str,
        plan: &HostPlan,
        boot_id: &str,
        connection_revision: i64,
        now: i64,
    ) -> Result<HostOperation, StorageError> {
        self.require_host_session(user, session).await?;
        plan.validate().map_err(StorageError::InvalidValue)?;
        if request.is_empty() || request.len() > 256 || boot_id.is_empty() || boot_id.len() > 256 {
            return Err(StorageError::InvalidValue(
                "Missing or oversized operation/boot identity.".into(),
            ));
        }
        let operation = HostOperation {
            id: 0,
            user_id: user,
            session_id: session,
            request_id: request.into(),
            plan: plan.clone(),
            boot_id: boot_id.into(),
            connection_revision,
            steps_started: 0,
            input_token: None,
            live_output: String::new(),
            state: OperationState::Prepared,
            outputs: Vec::new(),
            detail: "Prepared; waiting for explicit approval.".into(),
            created_at: now,
            updated_at: now,
        };
        self.db.execute("INSERT INTO host_operations(user_id, session_id, request_id, target, state, data) VALUES(?,?,?,?,?,?) ON CONFLICT(user_id, session_id, request_id) DO NOTHING",
            &[DbValue::Int(user), DbValue::Int(session), DbValue::Text(request.into()), DbValue::Text(plan.target.clone()), DbValue::Text("prepared".into()), DbValue::Text(encode(&operation)?)]).await?;
        let rows = self.db.execute("SELECT id,data FROM host_operations WHERE user_id=? AND session_id=? AND request_id=?", &[DbValue::Int(user), DbValue::Int(session), DbValue::Text(request.into())]).await?;
        let stored = decode(
            rows.rows
                .first()
                .ok_or_else(|| StorageError::NotFound("Host operation".into()))?,
        )?;
        if stored.plan != *plan {
            return Err(StorageError::Conflict(
                "Operation identity already belongs to a different plan.".into(),
            ));
        }
        Ok(stored)
    }
    pub async fn host_operation(
        &self,
        user: i64,
        session: i64,
        id: i64,
    ) -> Result<HostOperation, StorageError> {
        self.require_host_session(user, session).await?;
        let rows = self
            .db
            .execute(
                "SELECT id,data FROM host_operations WHERE user_id=? AND session_id=? AND id=?",
                &[DbValue::Int(user), DbValue::Int(session), DbValue::Int(id)],
            )
            .await?;
        decode(
            rows.rows
                .first()
                .ok_or_else(|| StorageError::NotFound("Host operation".into()))?,
        )
    }
    pub async fn host_operations(
        &self,
        user: i64,
        session: i64,
    ) -> Result<Vec<HostOperation>, StorageError> {
        self.require_host_session(user, session).await?;
        self.db.execute("SELECT id,data FROM host_operations WHERE user_id=? AND session_id=? ORDER BY id DESC LIMIT 50", &[DbValue::Int(user), DbValue::Int(session)]).await?.rows.iter().map(decode).collect()
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Journal commands preserve explicit ownership, operation and host identity fields"
    )]
    pub async fn claim_host_operation(
        &self,
        user: i64,
        session: i64,
        id: i64,
        target: &str,
        boot_id: &str,
        connection_revision: i64,
        now: i64,
    ) -> Result<HostOperation, StorageError> {
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            if store.host_connection().await?.revision != connection_revision {
                return Err(StorageError::Conflict("Host connection changed; inspect and prepare a fresh operation.".into()));
            }
        let mut operation = store.host_operation(user, session, id).await?;
        if operation.plan.target != target
            || operation.boot_id != boot_id
            || operation.connection_revision != connection_revision
        {
            return Err(StorageError::Conflict(
                "Host or boot changed; inspect and prepare a fresh operation.".into(),
            ));
        }
        if operation.state != OperationState::Prepared {
            return Err(StorageError::Conflict(
                "Operation was already applied.".into(),
            ));
        }
        operation.state = OperationState::Running;
        operation.updated_at = now;
        operation.detail = "Approved; running on the execution host.".into();
        let result = store.db.execute("UPDATE host_operations SET state='running',data=? WHERE id=? AND user_id=? AND session_id=? AND state='prepared' AND NOT EXISTS(SELECT 1 FROM host_operations WHERE target=? AND state IN ('running','awaiting_reconnect'))",
            &[DbValue::Text(encode(&operation)?), DbValue::Int(id), DbValue::Int(user), DbValue::Int(session), DbValue::Text(target.into())]).await?;
        if result.changes == 0 {
            return Err(StorageError::Conflict(
                "Another operation is already running on this host.".into(),
            ));
        }
        Ok(operation)
        }).await
    }
    /// Only the authenticated server bridge may persist execution results.
    pub async fn save_host_operation(&self, operation: &HostOperation) -> Result<(), StorageError> {
        let previous = self
            .host_operation(operation.user_id, operation.session_id, operation.id)
            .await?;
        if previous.plan != operation.plan
            || previous.request_id != operation.request_id
            || previous.boot_id != operation.boot_id
            || previous.connection_revision != operation.connection_revision
            || previous.created_at != operation.created_at
            || !previous.state.active()
            || operation.state == OperationState::Prepared
        {
            return Err(StorageError::Conflict(
                "Host operation cannot be changed in this state.".into(),
            ));
        }
        if operation.steps_started > operation.plan.steps.len()
            || operation.outputs.len() > 32
            || operation.detail.len() > 4096
            || operation.live_output.len() > 8192
            || operation
                .input_token
                .as_ref()
                .is_some_and(|token| token.len() > 256)
            || operation
                .outputs
                .iter()
                .map(|result| result.outcome.stdout.len() + result.outcome.stderr.len())
                .sum::<usize>()
                > MAX_OUTPUT_BYTES
        {
            return Err(StorageError::InvalidValue(
                "Host operation output exceeds its retention limit.".into(),
            ));
        }
        let result = self
            .db
            .execute(
                "UPDATE host_operations SET state=?,data=? WHERE id=? AND state=? AND data=?",
                &[
                    DbValue::Text(state_name(operation.state).into()),
                    DbValue::Text(encode(operation)?),
                    DbValue::Int(operation.id),
                    DbValue::Text(state_name(previous.state).into()),
                    DbValue::Text(encode(&previous)?),
                ],
            )
            .await?;
        if result.changes == 0 {
            return Err(StorageError::Conflict(
                "Host operation changed while saving results.".into(),
            ));
        }
        Ok(())
    }
    pub async fn active_host_operations(&self) -> Result<Vec<HostOperation>, StorageError> {
        self.db.execute("SELECT id,data FROM host_operations WHERE state IN ('running','awaiting_reconnect') ORDER BY id LIMIT 50", &[]).await?.rows.iter().map(decode).collect()
    }
}

impl<D: Db> Store<D> {
    pub async fn host_connection(
        &self,
    ) -> Result<openwebide_core::host_admin::HostConnection, StorageError> {
        self.get_setting("host_administration_connection")
            .await?
            .map_or_else(
                || Ok(Default::default()),
                |value| {
                    serde_json::from_str(&value)
                        .map_err(|error| StorageError::InvalidValue(error.to_string()))
                },
            )
    }
    pub async fn save_host_connection(
        &self,
        connection: &openwebide_core::host_admin::HostConnection,
    ) -> Result<openwebide_core::host_admin::HostConnection, StorageError> {
        connection
            .validate()
            .map_err(StorageError::InvalidRequest)?;
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let current = store.host_connection().await?;
                if connection.revision != current.revision {
                    return Err(StorageError::Conflict(
                        "Host connection changed; reload before saving.".into(),
                    ));
                }
                if !store.active_host_operations().await?.is_empty() {
                    return Err(StorageError::Conflict(
                        "Wait for active host operations to finish before changing the connection."
                            .into(),
                    ));
                }
                let mut saved = connection.clone();
                saved.revision = current.revision.checked_add(1).ok_or_else(|| {
                    StorageError::InvalidValue("Host connection revision limit reached".into())
                })?;
                store
                    .set_setting(
                        "host_administration_connection",
                        &serde_json::to_string(&saved)
                            .map_err(|error| StorageError::InvalidValue(error.to_string()))?,
                    )
                    .await?;
                Ok(saved)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::{NewProject, WorkspaceMode};
    fn plan() -> HostPlan {
        HostPlan {
            target: "test-host".into(),
            title: "User configuration".into(),
            steps: vec![openwebide_core::host_admin::HostCommand {
                program: "printf".into(),
                args: vec!["hello".into()],
                cwd: "/tmp".into(),
                elevated: false,
                interactive: true,
                timeout_seconds: 30,
            }],
            checks: vec![],
            expects_reboot: false,
        }
    }
    async fn setup() -> (Store<RusqliteDb>, UserId, i64) {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        store.migrate().await.unwrap();
        let user = store
            .insert_first_admin("admin", "hash", 1)
            .await
            .unwrap()
            .unwrap()
            .id;
        store
            .save_host_connection(&openwebide_core::host_admin::HostConnection {
                destination: "test-host".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let session = store
            .create_session("Host", None, None, None, user, 1)
            .await
            .unwrap()
            .id;
        (store, user, session)
    }
    #[test]
    fn username_only_host_connection_is_persisted_without_an_address() {
        futures::executor::block_on(async {
            let (store, _, _) = setup().await;
            let mut connection = store.host_connection().await.unwrap();
            connection.username = "admin".into();
            connection.destination.clear();
            let saved = store.save_host_connection(&connection).await.unwrap();
            let restored = store.host_connection().await.unwrap();
            assert_eq!(restored, saved);
            assert!(restored.is_configured());
            assert_eq!(
                restored
                    .ssh_destination("host.containers.internal")
                    .unwrap(),
                "admin@host.containers.internal"
            );
        });
    }

    #[test]
    fn connection_changes_are_versioned_blocked_by_maintenance_and_admin_only() {
        futures::executor::block_on(async {
            let (store, user, session) = setup().await;
            let member = store
                .insert_user("member", "hash", UserRole::User, 1)
                .await
                .unwrap();
            let member_session = store
                .create_session("Host", None, None, None, member.id, 1)
                .await
                .unwrap()
                .id;
            assert!(
                store
                    .require_host_session(member.id.get(), member_session)
                    .await
                    .is_err()
            );
            let old = store.host_connection().await.unwrap();
            let saved = store.save_host_connection(&old).await.unwrap();
            assert_eq!(saved.revision, old.revision + 1);
            assert!(store.save_host_connection(&old).await.is_err());
            let prepared = store
                .prepare_host_operation(
                    user.get(),
                    session,
                    "test",
                    &plan(),
                    "boot",
                    saved.revision,
                    1,
                )
                .await
                .unwrap();
            assert!(
                store
                    .claim_host_operation(
                        user.get(),
                        session,
                        prepared.id,
                        "test-host",
                        "boot",
                        old.revision,
                        2
                    )
                    .await
                    .is_err()
            );
            let mut active = store
                .claim_host_operation(
                    user.get(),
                    session,
                    prepared.id,
                    "test-host",
                    "boot",
                    saved.revision,
                    2,
                )
                .await
                .unwrap();
            assert!(store.save_host_connection(&saved).await.is_err());
            assert!(store.delete_session(session, user).await.is_err());
            active.state = OperationState::Interrupted;
            store.save_host_operation(&active).await.unwrap();
            store.delete_session(session, user).await.unwrap();
            assert!(store.active_host_operations().await.unwrap().is_empty());
            store.save_host_connection(&saved).await.unwrap();
        });
    }
    #[test]
    fn journal_is_durable_immutable_scoped_and_serializes_host_changes() {
        futures::executor::block_on(async {
            let (store, user, session) = setup().await;
            let prepared = store
                .prepare_host_operation(user.get(), session, "call-1", &plan(), "boot", 1, 1)
                .await
                .unwrap();
            assert_eq!(
                prepared,
                store
                    .prepare_host_operation(user.get(), session, "call-1", &plan(), "boot", 1, 1)
                    .await
                    .unwrap()
            );
            let mut altered = plan();
            altered.title = "Another plan".into();
            assert!(
                store
                    .prepare_host_operation(user.get(), session, "call-1", &altered, "boot", 1, 1)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .claim_host_operation(
                        user.get(),
                        session,
                        prepared.id,
                        "wrong-host",
                        "boot",
                        1,
                        2
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .claim_host_operation(
                        user.get(),
                        session,
                        prepared.id,
                        "test-host",
                        "new-boot",
                        1,
                        2
                    )
                    .await
                    .is_err()
            );
            let mut running = store
                .claim_host_operation(user.get(), session, prepared.id, "test-host", "boot", 1, 2)
                .await
                .unwrap();
            let second = store
                .prepare_host_operation(user.get(), session, "call-2", &plan(), "boot", 1, 3)
                .await
                .unwrap();
            assert!(
                store
                    .claim_host_operation(user.get(), session, second.id, "test-host", "boot", 1, 3)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .claim_host_operation(
                        user.get(),
                        session,
                        prepared.id,
                        "test-host",
                        "boot",
                        1,
                        3
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .host_operation(user.get() + 1, session, prepared.id)
                    .await
                    .is_err()
            );
            let other = store
                .create_session("Other", None, None, None, user, 3)
                .await
                .unwrap()
                .id;
            assert!(
                store
                    .host_operation(user.get(), other, prepared.id)
                    .await
                    .is_err()
            );
            running.detail = "Retained output".into();
            store.save_host_operation(&running).await.unwrap();
            assert_eq!(
                store
                    .host_operation(user.get(), session, running.id)
                    .await
                    .unwrap(),
                running
            );
            running.state = OperationState::Succeeded;
            store.save_host_operation(&running).await.unwrap();
            assert!(store.save_host_operation(&running).await.is_err());
            assert!(
                store
                    .claim_host_operation(user.get(), session, second.id, "test-host", "boot", 1, 4)
                    .await
                    .is_ok()
            );
        });
    }
    #[test]
    fn project_sessions_reject_host_operations_in_both_modes() {
        futures::executor::block_on(async {
            let (store, user, _) = setup().await;
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let project = store
                    .create_project(
                        &NewProject {
                            name: format!("{mode:?}"),
                            mode,
                            path: Some("project".into()),
                        },
                        user,
                        2,
                    )
                    .await
                    .unwrap();
                let session = store
                    .create_session("Project", None, None, Some(project.id), user, 2)
                    .await
                    .unwrap()
                    .id;
                assert!(
                    store
                        .prepare_host_operation(user.get(), session, "call", &plan(), "boot", 1, 3)
                        .await
                        .is_err()
                );
                assert!(store.host_operations(user.get(), session).await.is_err());
            }
        });
    }
}
