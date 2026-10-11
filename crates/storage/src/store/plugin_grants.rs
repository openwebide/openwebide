//! Durable authority snapshots for general host callbacks.
use super::*;
use openwebide_core::plugins::{
    PreparedPlugin, default_bindings,
    execution::{PluginExecutionContext, PluginHostRequest},
    records::RecordRequest,
};

type AuthorityFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<(PreparedPlugin, openwebide_core::ChatSession), StorageError>,
            > + Send
            + 'a,
    >,
>;
type ContextAuthorityFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<(PreparedPlugin, PluginExecutionContext), StorageError>,
            > + Send
            + 'a,
    >,
>;
type HostFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, StorageError>> + Send + 'a>>;

impl<D: Db> Store<D> {
    /// Chat callers and sessionless callers use the same pinned authority check.
    pub fn authorize_plugin_host<'a>(
        &'a self,
        user: UserId,
        session: i64,
        request: &'a PluginHostRequest,
        now: i64,
    ) -> AuthorityFuture<'a> {
        Box::pin(async move {
            self.db
                .transaction(|tx| async move {
                    let store = Store::new(tx);
                    let (plugin, _) = store
                        .plugin_authority_in_transaction(user, Some(session), request, now)
                        .await?;
                    Ok((plugin, store.get_session(session, user).await?))
                })
                .await
        })
    }
    /// UI/background authority does not require a synthetic chat session.
    pub fn authorize_plugin_context<'a>(
        &'a self,
        user: UserId,
        request: &'a PluginHostRequest,
        now: i64,
    ) -> ContextAuthorityFuture<'a> {
        self.authorize_plugin_execution(user, None, request, now)
    }
    pub fn authorize_plugin_execution<'a>(
        &'a self,
        user: UserId,
        session: Option<i64>,
        request: &'a PluginHostRequest,
        now: i64,
    ) -> ContextAuthorityFuture<'a> {
        Box::pin(async move {
            self.db
                .transaction(|tx| async move {
                    Store::new(tx)
                        .plugin_authority_in_transaction(user, session, request, now)
                        .await
                })
                .await
        })
    }
    async fn plugin_authority_in_transaction(
        &self,
        user: UserId,
        session: Option<i64>,
        request: &PluginHostRequest,
        now: i64,
    ) -> Result<(PreparedPlugin, PluginExecutionContext), StorageError> {
        let (plugin, context) = self
            .plugin_grant_in_transaction(user, session, &request.grant, now)
            .await?;
        if !plugin
            .manifest
            .executable
            .as_ref()
            .is_some_and(|rust| rust.capabilities.contains(&request.capability))
        {
            return Err(StorageError::InvalidRequest(
                "Plugin capability is not granted".into(),
            ));
        }
        Ok((plugin, context))
    }
    async fn plugin_grant_in_transaction(
        &self,
        user: UserId,
        session: Option<i64>,
        grant: &str,
        now: i64,
    ) -> Result<(PreparedPlugin, PluginExecutionContext), StorageError> {
        let grant = self.db.execute("SELECT prepared,project_scope,session_id,primary_model,user_action,job_id,job_lease FROM plugin_execution_grants WHERE token=? AND user_id=? AND expires_at>?", &[
            DbValue::Text(grant.to_owned()), DbValue::Int(user.get()), DbValue::Int(now),
        ]).await?;
        let row = grant
            .rows
            .first()
            .ok_or_else(|| StorageError::NotFound("Plugin execution grant".into()))?;
        let grant_session = row.get_int_opt(2);
        if session != grant_session {
            return Err(StorageError::NotFound("Plugin execution grant".into()));
        }
        if let Some(job) = row.get_int_opt(5) {
            let lease = row.get_text(6)?;
            let current = self.db.execute("SELECT 1 FROM plugin_jobs WHERE id=? AND user_id=? AND state='leased' AND lease=? AND lease_expires_at>? AND (background_id IS NULL OR EXISTS(SELECT 1 FROM plugin_background b WHERE b.id=plugin_jobs.background_id AND b.prepared=plugin_jobs.prepared))", &[DbValue::Int(job),DbValue::Int(user.get()),DbValue::Text(lease.into()),DbValue::Int(now)]).await?;
            if current.rows.is_empty() {
                return Err(StorageError::Conflict(
                    "Job lease is no longer current".into(),
                ));
            }
        }
        let project = row.get_int(1)?;
        let context = PluginExecutionContext {
            user_action: row.get_int(4)? != 0,
            project_id: (project != 0).then_some(project),
            session_id: grant_session,
            primary: row
                .get_text_opt(3)
                .map(serde_json::from_str)
                .transpose()
                .map_err(|error| StorageError::Db(error.to_string()))?,
        };
        self.validate_plugin_context(user, &context).await?;
        let plugin: PreparedPlugin = serde_json::from_str(row.get_text(0)?)
            .map_err(|error| StorageError::Db(error.to_string()))?;
        Ok((plugin, context))
    }
    /// Validate the start envelope against an issued version snapshot before
    /// forwarding a new invocation to its execution host.
    pub fn authorize_plugin_invocation<'a>(
        &'a self,
        user: UserId,
        request: &'a openwebide_core::plugins::execution::PluginStartRequest,
        now: i64,
    ) -> ContextAuthorityFuture<'a> {
        Box::pin(async move {
            request
                .call
                .prepared
                .validate()
                .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
            request
                .call
                .validate_event()
                .map_err(StorageError::InvalidRequest)?;
            self.db
                .transaction(|tx| async move {
                    let store = Store::new(tx);
                    let (plugin, context) = store
                        .plugin_grant_in_transaction(user, request.session_id, &request.grant, now)
                        .await?;
                    let supplied = &request.call.prepared;
                    if plugin.source != supplied.source
                        || plugin.manifest != supplied.manifest
                        || plugin.digest != supplied.digest
                    {
                        return Err(StorageError::Conflict(
                            "Plugin invocation does not match its execution grant".into(),
                        ));
                    }
                    Ok((plugin, context))
                })
                .await
        })
    }
    pub(super) async fn validate_plugin_context(
        &self,
        user: UserId,
        context: &PluginExecutionContext,
    ) -> Result<(), StorageError> {
        if let Some(project) = context.project_id {
            self.get_project(project, user).await?;
        }
        if let Some(session) = context.session_id
            && self.get_session(session, user).await?.project_id != context.project_id
        {
            return Err(StorageError::Conflict(
                "Plugin execution project changed".into(),
            ));
        }
        if let Some(primary) = &context.primary
            && (primary.model.trim().is_empty() || primary.model.len() > 1024)
        {
            return Err(StorageError::InvalidRequest(
                "Invalid plugin model selection".into(),
            ));
        }
        Ok(())
    }
    pub async fn issue_plugin_grant(
        &self,
        user: UserId,
        session: i64,
        plugin: &PreparedPlugin,
        token: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        let project_id = self.get_session(session, user).await?.project_id;
        self.issue_plugin_context_grant(
            user,
            &PluginExecutionContext {
                user_action: false,
                project_id,
                session_id: Some(session),
                primary: None,
            },
            plugin,
            token,
            now,
        )
        .await
    }
    /// Only an enabled exact installed receipt can acquire authority. Subsequent
    /// updates do not replace this invocation's source, project or model snapshot.
    pub async fn issue_plugin_context_grant(
        &self,
        user: UserId,
        context: &PluginExecutionContext,
        plugin: &PreparedPlugin,
        token: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        plugin
            .validate()
            .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
        if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) || now < 0 {
            return Err(StorageError::InvalidRequest(
                "Invalid plugin execution grant".into(),
            ));
        }
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            store.validate_plugin_context(user, context).await?;
            if let Some(primary) = &context.primary
                && !store.get_connection(primary.server_id).await?.enabled {
                return Err(StorageError::InvalidRequest("Plugin model server is disabled".into()));
            }
            let bindings = match context.project_id {
                Some(project) => store.project_plugins(user, project).await?,
                None => default_bindings(&store.plugin_installations(user).await?),
            };
            if plugin.manifest.executable.is_none() || !bindings.iter().any(|binding| {
                binding.enabled && binding.prepared.source == plugin.source
                    && binding.prepared.manifest == plugin.manifest && binding.prepared.digest == plugin.digest
            }) {
                return Err(StorageError::Conflict("Plugin is no longer enabled at the selected version".into()));
            }
            let expires = now.checked_add(86_400).ok_or_else(|| StorageError::InvalidRequest("Invalid grant expiry".into()))?;
            store.db.execute("DELETE FROM plugin_execution_grants WHERE expires_at<=?", &[DbValue::Int(now)]).await?;
            store.db.execute("INSERT INTO plugin_execution_grants(token,user_id,session_id,project_scope,prepared,expires_at,primary_model,user_action) VALUES(?,?,?,?,?,?,?,?)", &[
                DbValue::Text(token.into()), DbValue::Int(user.get()), context.session_id.map_or(DbValue::Null, DbValue::Int),
                DbValue::Int(context.project_id.unwrap_or(0)),
                DbValue::Text(serde_json::to_string(plugin).map_err(|error| StorageError::Db(error.to_string()))?), DbValue::Int(expires),
                context.primary.as_ref().map(serde_json::to_string).transpose()
                    .map_err(|error| StorageError::Db(error.to_string()))?.map_or(DbValue::Null, DbValue::Text),
                DbValue::Int(i64::from(context.user_action)),
            ]).await?;
            Ok(())
        }).await
    }
    pub fn plugin_host_request<'a>(
        &'a self,
        user: UserId,
        session: i64,
        request: &'a PluginHostRequest,
        now: i64,
    ) -> HostFuture<'a> {
        self.plugin_scoped_host_request(user, Some(session), request, now)
    }
    pub fn plugin_context_host_request<'a>(
        &'a self,
        user: UserId,
        request: &'a PluginHostRequest,
        now: i64,
    ) -> HostFuture<'a> {
        self.plugin_scoped_host_request(user, None, request, now)
    }
    /// Authority and writes share a transaction, so scope cannot change mid-write.
    fn plugin_scoped_host_request<'a>(
        &'a self,
        user: UserId,
        session: Option<i64>,
        request: &'a PluginHostRequest,
        now: i64,
    ) -> HostFuture<'a> {
        Box::pin(async move {
            self.db
                .transaction(|tx| async move {
                    let store = Store::new(tx);
                    let (plugin, context) = store
                        .plugin_authority_in_transaction(user, session, request, now)
                        .await?;
                    if request.capability == "jobs" {
                        let context = store
                            .plugin_job_origin_context(user, &request.grant, &context)
                            .await?;
                        let command = serde_json::from_str(&request.payload)
                            .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
                        if let openwebide_core::plugins::jobs::JobRequest::Schedule { key, .. } =
                            &command
                            && key.starts_with("background:")
                        {
                            return Err(StorageError::InvalidRequest(
                                "Background job keys are reserved for host delivery".into(),
                            ));
                        }
                        let result = store
                            .plugin_jobs_in_transaction(user, &plugin, &context, &command, now)
                            .await?;
                        return serde_json::to_string(&result)
                            .map_err(|error| StorageError::Db(error.to_string()));
                    }
                    if request.capability == "runs" {
                        let command: openwebide_core::plugins::runs::RunRequest =
                            serde_json::from_str(&request.payload)
                                .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
                        let context = store
                            .plugin_job_origin_context(user, &request.grant, &context)
                            .await?;
                        let result = store
                            .plugin_runs_in_transaction(user, &plugin, &context, &command, now)
                            .await?;
                        return serde_json::to_string(&result)
                            .map_err(|error| StorageError::Db(error.to_string()));
                    }
                    let command: RecordRequest = serde_json::from_str(&request.payload)
                        .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
                    match request.capability.as_str() {
                        "records" => {
                            let result = store
                                .plugin_project_records_in_transaction(
                                    user,
                                    context.project_id,
                                    &plugin.storage_namespace(),
                                    &command,
                                    now,
                                )
                                .await?;
                            serde_json::to_string(&result)
                                .map_err(|error| StorageError::Db(error.to_string()))
                        }
                        "collections" => {
                            let origin = store
                                .plugin_job_origin_context(user, &request.grant, &context)
                                .await?;
                            let result = store
                                .plugin_collections_in_transaction(
                                    user, &plugin, &origin, &command, now,
                                )
                                .await?;
                            serde_json::to_string(&result)
                                .map_err(|error| StorageError::Db(error.to_string()))
                        }
                        _ => Err(StorageError::InvalidRequest(
                            "Plugin host capability unavailable".into(),
                        )),
                    }
                })
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use futures::executor::block_on;
    use openwebide_core::plugins::{
        PluginPackage, PluginTool, RecordPlugin, RemovePlugin, RustPlugin,
    };
    use serde_json::json;

    fn receipt() -> PreparedPlugin {
        let mut receipt = openwebide_core::plugins::testing::receipt();
        receipt.manifest.publisher = "example".into();
        receipt.manifest.name = "notes".into();
        receipt.manifest.compatibility.plugin_api = 3;
        receipt.manifest.contributions.skills.clear();
        receipt.manifest.contributions.tools = vec![PluginTool {
            name: "notes_add".into(),
            description: "Add a note".into(),
            parameters: json!({"type":"object"}),
            requires_approval: true,
        }];
        receipt.manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "notes".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["records".into()],
        });
        receipt
    }
    fn installation(prepared: &PreparedPlugin, revision: Option<i64>) -> RecordPlugin {
        RecordPlugin {
            approved_capabilities: Vec::new(),
            prepared: prepared.clone(),
            revision,
            update_policy: None,
            package: Some(Box::new(PluginPackage {
                prepared: prepared.clone(),
                skills: Vec::new(),
            })),
        }
    }
    #[test]
    fn context_migration_preserves_deployed_chat_grants_and_replays() {
        block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            crate::migrations::apply_through(&store.db, 47)
                .await
                .unwrap();
            store
                .db
                .execute("PRAGMA user_version=47", &[])
                .await
                .unwrap();
            let user = store
                .insert_user("owner", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("s", None, None, None, user, 0)
                .await
                .unwrap()
                .id;
            let plugin = receipt();
            let token = "a".repeat(32);
            store.db.execute("INSERT INTO plugin_execution_grants(token,user_id,session_id,project_scope,prepared,expires_at) VALUES(?,?,?,?,?,?)", &[
                DbValue::Text(token.clone()), DbValue::Int(user.get()), DbValue::Int(session), DbValue::Int(0),
                DbValue::Text(serde_json::to_string(&plugin).unwrap()), DbValue::Int(100),
            ]).await.unwrap();
            store.migrate().await.unwrap();
            store.migrate().await.unwrap();
            let request = PluginHostRequest {
                grant: token,
                capability: "records".into(),
                payload: json!({"collection":"notes","operation":{"action":"list"}}).to_string(),
            };
            let (pinned, context) = store
                .authorize_plugin_execution(user, Some(session), &request, 1)
                .await
                .unwrap();
            assert_eq!(pinned, plugin);
            assert_eq!(
                context,
                PluginExecutionContext {
                    user_action: false,
                    project_id: None,
                    session_id: Some(session),
                    primary: None
                }
            );
            store
                .plugin_host_request(user, session, &request, 1)
                .await
                .unwrap();
            assert!(
                store
                    .plugin_context_host_request(user, &request, 1)
                    .await
                    .is_err()
            );
        });
    }
    #[test]
    fn sessionless_contexts_share_project_data_and_preserve_pinned_authority_in_both_modes() {
        block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
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
                let project = store
                    .create_project(
                        &NewProject {
                            name: "p".into(),
                            mode,
                            path: Some("p".into()),
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                let session = store
                    .create_session("s", None, None, Some(project), user, 0)
                    .await
                    .unwrap()
                    .id;
                let server = store
                    .insert_connection(&openwebide_core::NewConnection {
                        name: "Model".into(),
                        kind: openwebide_core::ProviderKind::Ollama,
                        base_url: "http://localhost:11434".into(),
                        model: Some("default".into()),
                        context_limit: None,
                    })
                    .await
                    .unwrap();
                let mut plugin = receipt();
                plugin
                    .manifest
                    .executable
                    .as_mut()
                    .unwrap()
                    .capabilities
                    .push("collections".into());
                store
                    .record_plugin(user, &installation(&plugin, None), 1)
                    .await
                    .unwrap();
                let context = PluginExecutionContext {
                    user_action: false,
                    project_id: Some(project),
                    session_id: None,
                    primary: Some(openwebide_core::ModelSelection {
                        server_id: server.id,
                        model: "selected".into(),
                    }),
                };
                let token = "a".repeat(32);
                store
                    .issue_plugin_context_grant(user, &context, &plugin, &token, 2)
                    .await
                    .unwrap();
                assert!(
                    store
                        .issue_plugin_context_grant(other, &context, &plugin, &"b".repeat(32), 2)
                        .await
                        .is_err()
                );
                let invocation = openwebide_core::plugins::execution::PluginStartRequest {
                    grant: token.clone(),
                    session_id: None,
                    call: openwebide_core::plugins::execution::InvokePlugin {
                        operation: openwebide_core::plugins::execution::PluginOperation::Tool,
                        prepared: PreparedPlugin {
                            host_id: "another-host".into(),
                            ..plugin.clone()
                        },
                        name: "notes_add".into(),
                        arguments: "{}".into(),
                    },
                };
                assert_eq!(
                    store
                        .authorize_plugin_invocation(user, &invocation, 3)
                        .await
                        .unwrap()
                        .1,
                    context
                );
                assert!(
                    store
                        .authorize_plugin_invocation(other, &invocation, 3)
                        .await
                        .is_err()
                );
                let mut forged = invocation.clone();
                forged
                    .call
                    .prepared
                    .manifest
                    .executable
                    .as_mut()
                    .unwrap()
                    .capabilities
                    .push("http".into());
                assert!(
                    store
                        .authorize_plugin_invocation(user, &forged, 3)
                        .await
                        .is_err()
                );
                forged = invocation.clone();
                forged.session_id = Some(session);
                assert!(
                    store
                        .authorize_plugin_invocation(user, &forged, 3)
                        .await
                        .is_err()
                );
                forged = invocation.clone();
                forged.call.prepared.source.commit = "f".repeat(40);
                assert!(
                    store
                        .authorize_plugin_invocation(user, &forged, 3)
                        .await
                        .is_err()
                );
                forged = invocation.clone();
                forged.call.prepared.digest = "f".repeat(64);
                assert!(
                    store
                        .authorize_plugin_invocation(user, &forged, 3)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .authorize_plugin_invocation(user, &invocation, 86_402)
                        .await
                        .is_err()
                );
                let call = |capability: &str, value| PluginHostRequest {
                    grant: token.clone(),
                    capability: capability.into(),
                    payload: value,
                };
                let private = call("records", json!({"collection":"notes","operation":{"action":"create","value":{"body":"UI event"}}}).to_string());
                store
                    .plugin_context_host_request(user, &private, 3)
                    .await
                    .unwrap();
                assert!(
                    store
                        .plugin_context_host_request(other, &private, 3)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .plugin_host_request(user, session, &private, 3)
                        .await
                        .is_err()
                );
                let shared = call("collections", json!({"collection":"memories","operation":{"action":"create","value":{"title":"UI memory","content":"Event content"}}}).to_string());
                store
                    .plugin_context_host_request(user, &shared, 3)
                    .await
                    .unwrap();
                assert_eq!(
                    store
                        .project_memories(user, project)
                        .await
                        .unwrap()
                        .entries
                        .len(),
                    1
                );
                // Disabling agent context must not prevent explicit UI management.
                store
                    .set_user_setting(user, &format!("project_memory_{project}"), "false")
                    .await
                    .unwrap();
                assert!(
                    store
                        .plugin_context_host_request(user, &shared, 3)
                        .await
                        .is_err()
                );
                let action_context = PluginExecutionContext {
                    user_action: true,
                    ..context.clone()
                };
                let action_token = "7".repeat(32);
                store
                    .issue_plugin_context_grant(user, &action_context, &plugin, &action_token, 3)
                    .await
                    .unwrap();
                let action_request = PluginHostRequest {
                    grant: action_token.clone(),
                    ..shared.clone()
                };
                store
                    .plugin_context_host_request(user, &action_request, 3)
                    .await
                    .unwrap();
                assert!(!store.project_memories(user, project).await.unwrap().enabled);
                assert_eq!(
                    store
                        .project_memories(user, project)
                        .await
                        .unwrap()
                        .entries
                        .len(),
                    2
                );
                // Scope is carried by the token, never accepted from plugin input.
                let mut forged_action = shared.clone();
                let mut forged_payload: serde_json::Value =
                    serde_json::from_str(&forged_action.payload).unwrap();
                forged_payload["user_action"] = json!(true);
                forged_action.payload = forged_payload.to_string();
                assert!(
                    store
                        .plugin_context_host_request(user, &forged_action, 3)
                        .await
                        .is_err()
                );
                store
                    .db
                    .execute("PRAGMA user_version=47", &[])
                    .await
                    .unwrap();
                store.migrate().await.unwrap();
                assert_eq!(
                    store
                        .authorize_plugin_context(user, &action_request, 3)
                        .await
                        .unwrap()
                        .1,
                    action_context
                );
                let (_, pinned) = store
                    .authorize_plugin_context(user, &private, 3)
                    .await
                    .unwrap();
                assert_eq!(pinned, context);
                store
                    .db
                    .execute(
                        "UPDATE connections SET enabled=0 WHERE id=?",
                        &[DbValue::Int(server.id)],
                    )
                    .await
                    .unwrap();
                // Model unavailability cannot revoke unrelated granted storage.
                store
                    .plugin_context_host_request(user, &private, 3)
                    .await
                    .unwrap();
                assert!(
                    store
                        .issue_plugin_context_grant(user, &context, &plugin, &"e".repeat(32), 3)
                        .await
                        .is_err()
                );
                store
                    .db
                    .execute(
                        "UPDATE connections SET enabled=1 WHERE id=?",
                        &[DbValue::Int(server.id)],
                    )
                    .await
                    .unwrap();

                // Replaying migrations must retain nullable scope and model snapshots.
                store
                    .db
                    .execute("PRAGMA user_version=47", &[])
                    .await
                    .unwrap();
                store.migrate().await.unwrap();
                assert_eq!(
                    store
                        .authorize_plugin_context(user, &private, 3)
                        .await
                        .unwrap()
                        .1,
                    context
                );
                // Chat authority remains separate even when the project is identical.
                let chat_token = "c".repeat(32);
                store
                    .issue_plugin_grant(user, session, &plugin, &chat_token, 3)
                    .await
                    .unwrap();
                let chat = PluginHostRequest {
                    grant: chat_token,
                    ..private.clone()
                };
                assert!(
                    store
                        .plugin_context_host_request(user, &chat, 4)
                        .await
                        .is_err()
                );
                store
                    .plugin_host_request(user, session, &chat, 4)
                    .await
                    .unwrap();
                let saved = store.plugin_installations(user).await.unwrap().remove(0);
                store
                    .remove_plugin(
                        user,
                        &RemovePlugin {
                            source: plugin.source.clone(),
                            revision: saved.revision,
                        },
                    )
                    .await
                    .unwrap();
                assert!(
                    store
                        .issue_plugin_context_grant(user, &context, &plugin, &"d".repeat(32), 5)
                        .await
                        .is_err()
                );
                store
                    .plugin_context_host_request(user, &private, 5)
                    .await
                    .unwrap();
                assert!(
                    store
                        .plugin_context_host_request(user, &private, 86_402)
                        .await
                        .is_err()
                );
                store.delete_project(project, user).await.unwrap();
                assert!(
                    store
                        .plugin_context_host_request(user, &private, 6)
                        .await
                        .is_err()
                );
            }
        });
    }
    #[test]
    fn skill_collection_callbacks_preserve_ui_records_and_plugin_provenance_in_both_modes() {
        block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
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
                let project = store
                    .create_project(
                        &NewProject {
                            name: "p".into(),
                            mode,
                            path: Some("p".into()),
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                let session = store
                    .create_session("s", None, None, Some(project), user, 0)
                    .await
                    .unwrap()
                    .id;
                let mut plugin = receipt();
                plugin.manifest.executable.as_mut().unwrap().capabilities =
                    vec!["collections".into()];
                store
                    .record_plugin(user, &installation(&plugin, None), 1)
                    .await
                    .unwrap();
                let token = "a".repeat(32);
                store
                    .issue_plugin_grant(user, session, &plugin, &token, 2)
                    .await
                    .unwrap();
                let call = |operation| PluginHostRequest {
                    grant: token.clone(),
                    capability: "collections".into(),
                    payload: json!({"collection":"skills","operation":operation}).to_string(),
                };
                let draft = |name: &str| openwebide_core::SkillDraft {
                    name: name.into(),
                    description: "Build guidance".into(),
                    instructions: "Run approved checks".into(),
                    enabled: true,
                    resources: vec![openwebide_core::SkillResource {
                        name: "references/build.md".into(),
                        content: "cargo test".into(),
                        binary: false,
                    }],
                    metadata: Default::default(),
                };
                let ui = store
                    .skill_command(
                        user,
                        project,
                        &openwebide_core::SkillCommand::Create {
                            draft: draft("ui-skill"),
                        },
                        false,
                        1,
                    )
                    .await
                    .unwrap()
                    .entries
                    .remove(0);
                let read = call(json!({"action":"read","id":ui.id}));
                let result: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(user, session, &read, 3)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    result["records"][0]["value"]["draft"]["resources"][0]["content"],
                    "cargo test"
                );
                assert!(result["records"][0]["value"]["origin"].is_null());
                assert!(
                    store
                        .plugin_host_request(other, session, &read, 3)
                        .await
                        .is_err()
                );
                let mut changed = draft("ui-skill");
                changed.instructions = "Plugin changed these instructions".into();
                let edit = call(
                    json!({"action":"update","id":ui.id,"revision":ui.revision,"value":{"draft":changed}}),
                );
                store
                    .plugin_host_request(user, session, &edit, 4)
                    .await
                    .unwrap();
                assert_eq!(
                    store.project_skills(user, project).await.unwrap().entries[0]
                        .draft
                        .instructions,
                    changed.instructions
                );
                assert!(
                    store
                        .plugin_host_request(user, session, &edit, 5)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .plugin_host_request(
                            user,
                            session,
                            &call(json!({"action":"create","value":{"draft":draft("ui-skill")}})),
                            5
                        )
                        .await
                        .is_err()
                );
                assert!(store.plugin_host_request(user, session, &call(json!({"action":"create","value":{"draft":draft("invalid-skill"),"origin":{"publisher":"forged"}}})), 5).await.is_err());
                for index in 0..9 {
                    store.plugin_host_request(user, session, &call(json!({"action":"create","value":{"draft":draft(&format!("created-{index}"))}})), 5).await.unwrap();
                }
                let first: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(user, session, &call(json!({"action":"list"})), 6)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(first["records"].as_array().unwrap().len(), 8);
                let second: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(
                            user,
                            session,
                            &call(json!({"action":"list","after":first["next"]})),
                            6,
                        )
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(second["records"].as_array().unwrap().len(), 2);
                // A managed skill may be read with its provenance, but its owner
                // controls updates and deletion through plugin lifecycle operations.
                let binding = store.db.execute("SELECT id FROM project_plugins WHERE user_id=? AND project_id=? AND repository=? AND path=?", &[
                    DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(plugin.source.repository.clone()),DbValue::Text(plugin.source.path.clone()),
                ]).await.unwrap().rows[0].get_int(0).unwrap();
                store
                    .db
                    .execute(
                        "INSERT INTO project_plugin_skills(plugin_id,skill_id) VALUES(?,?)",
                        &[DbValue::Int(binding), DbValue::Int(ui.id)],
                    )
                    .await
                    .unwrap();
                let managed: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(user, session, &read, 7)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    managed["records"][0]["value"]["origin"]["publisher"],
                    "example"
                );
                assert!(
                    store
                        .plugin_host_request(
                            user,
                            session,
                            &call(json!({"action":"delete","id":ui.id,"revision":2})),
                            7
                        )
                        .await
                        .is_err()
                );
                let personal = store
                    .project_skills(user, project)
                    .await
                    .unwrap()
                    .entries
                    .into_iter()
                    .find(|skill| skill.draft.name == "created-0")
                    .unwrap();
                let mut disabled_draft = personal.draft.clone();
                disabled_draft.enabled = false;
                store
                    .skill_command(
                        user,
                        project,
                        &openwebide_core::SkillCommand::Update {
                            id: personal.id,
                            revision: personal.revision,
                            draft: disabled_draft,
                        },
                        false,
                        7,
                    )
                    .await
                    .unwrap();
                assert!(
                    store
                        .plugin_host_request(
                            user,
                            session,
                            &call(json!({"action":"read","id":personal.id})),
                            7
                        )
                        .await
                        .is_err()
                );
                assert!(store.plugin_host_request(user, session, &call(json!({"action":"delete","id":personal.id,"revision":personal.revision+1})), 7).await.is_err());
                // Existing UI skills can approach 128 KiB before JSON escaping.
                // Editing them through a plugin preserves full resource data.
                let mut large = draft("large-ui-skill");
                large.instructions = "I".repeat(30000);
                large.resources = (0..3)
                    .map(|index| openwebide_core::SkillResource {
                        name: format!("references/{index}.txt"),
                        content: "\u{1}".repeat(30000),
                        binary: false,
                    })
                    .collect();
                let initial = store
                    .skill_command(
                        user,
                        project,
                        &openwebide_core::SkillCommand::Create {
                            draft: large.clone(),
                        },
                        false,
                        7,
                    )
                    .await
                    .unwrap()
                    .entries
                    .into_iter()
                    .find(|entry| entry.draft.name == large.name)
                    .unwrap();
                large.description = "Updated through SDK persistence".into();
                let update = call(
                    json!({"action":"update","id":initial.id,"revision":initial.revision,"value":{"draft":large}}),
                );
                let raw: RecordRequest = serde_json::from_str(&update.payload).unwrap();
                assert!(
                    raw.validate().is_err(),
                    "private records retain their smaller quota"
                );
                assert!(serde_json::to_vec(&update).unwrap().len() < 4 * 1024 * 1024);
                store
                    .plugin_host_request(user, session, &update, 7)
                    .await
                    .unwrap();
                let saved = store
                    .project_skills(user, project)
                    .await
                    .unwrap()
                    .entries
                    .into_iter()
                    .find(|entry| entry.id == initial.id)
                    .unwrap();
                assert_eq!(saved.draft, large);
                for index in 0..2 {
                    let mut another = large.clone();
                    another.name = format!("large-ui-{index}");
                    store
                        .skill_command(
                            user,
                            project,
                            &openwebide_core::SkillCommand::Create { draft: another },
                            false,
                            7,
                        )
                        .await
                        .unwrap();
                }
                let mut after = 0;
                let mut seen = std::collections::BTreeSet::new();
                loop {
                    let body = store
                        .plugin_host_request(
                            user,
                            session,
                            &call(json!({"action":"list","after":after})),
                            7,
                        )
                        .await
                        .unwrap();
                    assert!(body.len() <= 1024 * 1024 + 1024);
                    assert!(serde_json::to_vec(&body).unwrap().len() < 4 * 1024 * 1024);
                    let page: openwebide_core::plugins::records::CollectionResult =
                        serde_json::from_str(&body).unwrap();
                    for record in page.records {
                        assert!(seen.insert(record.id), "no repeated page records");
                    }
                    let Some(next) = page.next else {
                        break;
                    };
                    assert!(next > after);
                    after = next;
                }
                assert_eq!(seen.len(), 12); // Ten originals, one disabled, three large.
                store
                    .set_user_setting(user, &format!("project_skills_{project}"), "false")
                    .await
                    .unwrap();
                let disabled: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(user, session, &read, 8)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(disabled["enabled"], false);
                assert!(disabled["records"].as_array().unwrap().is_empty());
                assert!(store.plugin_host_request(user, session, &call(json!({"action":"create","value":{"draft":draft("disabled-create")}})), 8).await.is_err());
            }
        });
    }
    fn request(grant: &str) -> PluginHostRequest {
        PluginHostRequest {grant:grant.into(), capability:"records".into(), payload:json!({"collection":"notes","operation":{"action":"create","value":{"text":"owned"}}}).to_string()}
    }
    #[test]
    fn shared_collection_grants_preserve_ui_data_revisions_and_opt_out_in_both_modes() {
        block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("owner", "hash", UserRole::Admin, 0)
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
                        user,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                let session = store
                    .create_session("s", None, None, Some(project), user, 0)
                    .await
                    .unwrap()
                    .id;
                let initial = store
                    .memory_command(
                        user,
                        project,
                        &openwebide_core::MemoryCommand::Create {
                            auto_title: false,
                            title: "Existing UI memory".into(),
                            content: "Kept intact".into(),
                        },
                        false,
                        1,
                    )
                    .await
                    .unwrap()
                    .entries
                    .remove(0);
                let plugin = receipt();
                store
                    .record_plugin(user, &installation(&plugin, None), 1)
                    .await
                    .unwrap();
                let private_token = "a".repeat(32);
                store
                    .issue_plugin_grant(user, session, &plugin, &private_token, 2)
                    .await
                    .unwrap();
                let call = |token: &str, operation| PluginHostRequest {
                    grant: token.into(),
                    capability: "collections".into(),
                    payload: json!({"collection":"memories","operation":operation}).to_string(),
                };
                let list = json!({"action":"list"});
                // Granting private persistence does not authorize app-visible data.
                assert!(
                    store
                        .plugin_host_request(user, session, &call(&private_token, list.clone()), 3)
                        .await
                        .is_err()
                );
                let mut shared = plugin.clone();
                shared.source.commit = "c".repeat(40);
                shared.digest = "c".repeat(64);
                shared.manifest.version = "0.2.0".into();
                shared
                    .manifest
                    .executable
                    .as_mut()
                    .unwrap()
                    .capabilities
                    .push("collections".into());
                let mut update = installation(&shared, Some(1));
                update.approved_capabilities = vec!["collections".into()];
                store.record_plugin(user, &update, 3).await.unwrap();
                let token = "b".repeat(32);
                store
                    .issue_plugin_grant(user, session, &shared, &token, 4)
                    .await
                    .unwrap();
                let projectless = store
                    .create_session("projectless", None, None, None, user, 4)
                    .await
                    .unwrap()
                    .id;
                let global_token = "c".repeat(32);
                store
                    .issue_plugin_grant(user, projectless, &shared, &global_token, 4)
                    .await
                    .unwrap();
                let global: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(
                            user,
                            projectless,
                            &call(&global_token, list.clone()),
                            5,
                        )
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(global["enabled"], false);
                assert_eq!(global["records"], json!([]));
                assert!(
                    store
                        .plugin_host_request(
                            user,
                            projectless,
                            &call(
                                &global_token,
                                json!({"action":"create","value":{"title":"x","content":"y"}})
                            ),
                            5
                        )
                        .await
                        .is_err()
                );
                let read = store
                    .plugin_host_request(user, session, &call(&token, list.clone()), 5)
                    .await
                    .unwrap();
                let page: serde_json::Value = serde_json::from_str(&read).unwrap();
                assert_eq!(page["records"][0]["id"], initial.id);
                assert_eq!(page["records"][0]["value"]["content"], "Kept intact");
                let edit = json!({"action":"update","id":initial.id,"revision":initial.revision,"value":{"title":"Plugin-edited","content":"Visible in UI","auto_title":false}});
                store
                    .plugin_host_request(user, session, &call(&token, edit.clone()), 6)
                    .await
                    .unwrap();
                assert!(
                    store
                        .plugin_host_request(user, session, &call(&token, edit), 7)
                        .await
                        .is_err()
                );
                let ui = store.project_memories(user, project).await.unwrap();
                assert_eq!(ui.entries[0].title, "Plugin-edited");
                assert_eq!(ui.entries[0].revision, initial.revision + 1);
                let invalid =
                    json!({"action":"create","value":{"title":"ok","content":"bad","user_id":42}});
                assert!(
                    store
                        .plugin_host_request(user, session, &call(&token, invalid), 7)
                        .await
                        .is_err()
                );
                for index in 0..33 {
                    let create = json!({"action":"create","value":{"title":format!("Row {index}"),"content":"data"}});
                    store
                        .plugin_host_request(user, session, &call(&token, create), 8)
                        .await
                        .unwrap();
                }
                let first: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(user, session, &call(&token, list.clone()), 9)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(first["records"].as_array().unwrap().len(), 32);
                let second: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(
                            user,
                            session,
                            &call(&token, json!({"action":"list","after":first["next"]})),
                            9,
                        )
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(second["records"].as_array().unwrap().len(), 2);
                store
                    .memory_command(
                        user,
                        project,
                        &openwebide_core::MemoryCommand::SetEnabled { enabled: false },
                        false,
                        10,
                    )
                    .await
                    .unwrap();
                let disabled: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(user, session, &call(&token, list), 11)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(disabled["enabled"], false);
                assert_eq!(disabled["records"], json!([]));
                assert!(store.plugin_host_request(user, session, &call(&token, json!({"action":"delete","id":initial.id,"revision":initial.revision+1})), 11).await.is_err());
            }
        });
    }
    #[test]
    fn execution_grants_pin_authority_through_updates_and_removal_in_both_modes() {
        block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
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
                let session = store
                    .create_session("s", None, None, Some(project), owner, 0)
                    .await
                    .unwrap()
                    .id;
                let second = store
                    .create_session("second", None, None, Some(project), owner, 0)
                    .await
                    .unwrap()
                    .id;
                let original = receipt();
                let token = "a".repeat(32);
                assert!(
                    store
                        .issue_plugin_grant(owner, session, &original, &token, 1)
                        .await
                        .is_err()
                );
                let installed = store
                    .record_plugin(owner, &installation(&original, None), 1)
                    .await
                    .unwrap();
                store
                    .issue_plugin_grant(owner, session, &original, &token, 2)
                    .await
                    .unwrap();
                assert!(
                    store
                        .issue_plugin_grant(other, session, &original, &"d".repeat(32), 2)
                        .await
                        .is_err()
                );
                let call = request(&token);
                assert!(
                    store
                        .plugin_host_request(other, session, &call, 3)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .plugin_host_request(owner, second, &call, 3)
                        .await
                        .is_err()
                );
                let first: serde_json::Value = serde_json::from_str(
                    &store
                        .plugin_host_request(owner, session, &call, 3)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(first["records"][0]["value"]["text"], "owned");
                let mut updated = original.clone();
                updated.source.commit = "c".repeat(40);
                updated.digest = "c".repeat(64);
                updated.manifest.version = "0.2.0".into();
                updated.manifest.executable.as_mut().unwrap().capabilities = vec!["http".into()];
                let mut update = installation(&updated, Some(installed[0].revision));
                update.approved_capabilities = vec!["http".into()];
                let installed = store.record_plugin(owner, &update, 4).await.unwrap();
                let next_token = "b".repeat(32);
                store
                    .issue_plugin_grant(owner, session, &updated, &next_token, 5)
                    .await
                    .unwrap();
                assert!(
                    store
                        .issue_plugin_grant(owner, session, &original, &"e".repeat(32), 5)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .plugin_host_request(owner, session, &request(&next_token), 6)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .plugin_host_request(owner, session, &call, 6)
                        .await
                        .is_ok()
                );
                store
                    .remove_plugin(
                        owner,
                        &RemovePlugin {
                            source: updated.source,
                            revision: installed[0].revision,
                        },
                    )
                    .await
                    .unwrap();
                assert!(
                    store
                        .plugin_host_request(owner, session, &call, 8)
                        .await
                        .is_ok()
                );
                assert!(
                    store
                        .plugin_host_request(owner, session, &call, 86_402)
                        .await
                        .is_err()
                );
                store
                    .db
                    .execute(
                        "UPDATE sessions SET project_id=NULL WHERE id=?",
                        &[DbValue::Int(session)],
                    )
                    .await
                    .unwrap();
                assert!(
                    store
                        .plugin_host_request(owner, session, &call, 9)
                        .await
                        .is_err()
                );
                let rows = store
                    .db
                    .execute("SELECT COUNT(*) FROM plugin_records", &[])
                    .await
                    .unwrap();
                assert_eq!(rows.rows[0].get_int(0).unwrap(), 3);
            }
        });
    }
}
