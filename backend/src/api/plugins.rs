//! Authenticated database and remote bridge transport for plugin installation.
use super::*;
use openwebide_core::plugins::{PluginSource, PreparedPlugin, RecordPlugin};

/// Spin HTTP/storage primitives for the shared plugin planning workflow.
#[derive(Clone)]
pub(crate) struct PlanningHost<'a> {
    store: HostStore<'a>,
    user: openwebide_core::UserId,
    session: i64,
    cancelled: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    cancelled_preparations: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    clock: std::time::Instant,
}
#[derive(Clone)]
enum HostStore<'a> {
    Borrowed(&'a openwebide_storage::Store<crate::state::AppDb>),
    Owned(std::sync::Arc<openwebide_storage::Store<crate::state::AppDb>>),
}
impl<'a> PlanningHost<'a> {
    pub fn new(state: &'a AppState, user: openwebide_core::UserId, session: i64) -> Self {
        Self {
            store: HostStore::Borrowed(&state.store),
            user,
            session,
            cancelled: Default::default(),
            cancelled_preparations: Default::default(),
            clock: std::time::Instant::now(),
        }
    }
    fn store(&self) -> &openwebide_storage::Store<crate::state::AppDb> {
        match &self.store {
            HostStore::Borrowed(store) => store,
            HostStore::Owned(store) => store,
        }
    }
    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T, String> {
        send_plugin_rpc(self.store(), self.user, path, body).await
    }
    async fn flush_preparations(&self) {
        use openwebide_core::plugins::preparation::{PreparationCommand, preparation_request};
        let ids = std::mem::take(&mut *self.cancelled_preparations.lock().unwrap());
        for id in ids {
            let _ = preparation_request(self, &PreparationCommand::Cancel { id }).await;
        }
    }
    pub async fn flush_cancelled(&self) {
        self.flush_preparations().await;
        let ids = std::mem::take(&mut *self.cancelled.lock().unwrap());
        for id in ids {
            let _ = self
                .send::<serde_json::Value>("/plugins/cancel", json!({"id":id}))
                .await;
        }
    }
}
async fn send_plugin_rpc<T: serde::de::DeserializeOwned>(
    store: &openwebide_storage::Store<crate::state::AppDb>,
    user: UserId,
    path: &str,
    mut body: serde_json::Value,
) -> Result<T, String> {
    body["user"] = json!(user.get());
    let (status, response) = crate::bridge::send(store, path, body.to_string())
        .await
        .map_err(|error| {
            let error = ApiError::from(error);
            error.log_for_route("POST", path);
            error.public_message().to_owned()
        })?;
    if status != 200 {
        return Err(openwebide_core::plugins::execution::host_rpc_error(
            status, &response,
        ));
    }
    serde_json::from_slice(&response)
        .map_err(|_| "Plugin execution host returned an invalid response".into())
}
impl openwebide_core::plugins::preparation::PreparationClientHost for PlanningHost<'_> {
    fn request<'a>(
        &'a self,
        command: &'a openwebide_core::plugins::preparation::PreparationCommand,
    ) -> openwebide_core::plugins::preparation::PreparationFuture<
        'a,
        Result<openwebide_core::plugins::preparation::PluginPreparation, String>,
    > {
        Box::pin(async move {
            self.send(
                &format!("/plugins/prepare/{}", command.operation()),
                command.host_payload(),
            )
            .await
        })
    }
    fn sleep(
        &self,
        milliseconds: u32,
    ) -> openwebide_core::plugins::preparation::PreparationFuture<'_, ()> {
        Box::pin(spin_sdk::time::sleep(std::time::Duration::from_millis(
            u64::from(milliseconds),
        )))
    }
    fn now_millis(&self) -> f64 {
        self.clock.elapsed().as_secs_f64() * 1000.0
    }
    fn abandon(&self, id: String) {
        self.cancelled_preparations.lock().unwrap().push(id);
    }
}
impl PlanningHost<'static> {
    pub fn owned(
        store: std::sync::Arc<openwebide_storage::Store<crate::state::AppDb>>,
        user: openwebide_core::UserId,
        session: i64,
    ) -> Self {
        Self {
            store: HostStore::Owned(store),
            user,
            session,
            cancelled: Default::default(),
            cancelled_preparations: Default::default(),
            clock: std::time::Instant::now(),
        }
    }
}
impl openwebide_agent::plugins::execution::PluginTransport for PlanningHost<'_> {
    async fn ensure_prepared(&self, expected: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        let result =
            openwebide_core::plugins::preparation::prepare_on_host(self, &expected.source).await;
        self.flush_preparations().await;
        result
    }

    async fn start(
        &self,
        call: openwebide_core::plugins::execution::InvokePlugin,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        self.send("/plugins/invoke", json!({"call":call})).await
    }
    async fn resume(
        &self,
        response: openwebide_core::plugins::execution::ContinuePlugin,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        self.send("/plugins/continue", json!({"continuation":response}))
            .await
    }
    fn cancel(&self, id: String) {
        self.cancelled.lock().unwrap().push(id);
    }
    async fn flush_cancellations(&self) {
        self.flush_cancelled().await;
    }
}
impl openwebide_agent::plugins::execution::GrantedHost for PlanningHost<'_> {
    async fn request(
        &self,
        request: &openwebide_core::plugins::execution::PluginHostRequest,
    ) -> Result<String, String> {
        execute_host_request(self.store(), self.user, Some(self.session), request)
            .await
            .map_err(|error| {
                error.log_for_route("POST", "/api/sessions/plugin-host");
                error.public_message().to_owned()
            })
    }
}

pub(crate) async fn execute_host_request(
    store: &openwebide_storage::Store<crate::state::AppDb>,
    user: UserId,
    session: Option<i64>,
    request: &openwebide_core::plugins::execution::PluginHostRequest,
) -> Result<String, ApiError> {
    if request.capability == "completion" {
        let (_, context) = store
            .authorize_plugin_execution(user, session, request, now())
            .await?;
        let input: openwebide_core::plugins::completion::CompletionRequest =
            serde_json::from_str(&request.payload)
                .map_err(|error| ApiError::bad_request(error.to_string()))?;
        input.validate().map_err(ApiError::bad_request)?;
        let (connection, model) = if let Some(selection) = context.primary {
            (selection.server_id, Some(selection.model))
        } else if let Some(session) = context.session_id {
            let connection = store
                .get_session(session, user)
                .await?
                .connection_id
                .ok_or_else(|| ApiError::bad_request("Session has no model connection"))?;
            (connection, None)
        } else {
            return Err(ApiError::bad_request("No primary model is configured"));
        };
        let runtime =
            super::model_setup::runtime_store(store, user, connection, model.as_deref()).await?;
        let source = super::model_operations::ModelSource { store, user };
        let result = openwebide_agent::plugins::completion::complete(&source, runtime, input)
            .await
            .map_err(ApiError::bad_request)?;
        serde_json::to_string(&result).map_err(|error| ApiError::internal(error.to_string()))
    } else {
        match session {
            Some(session) => {
                store
                    .plugin_host_request(user, session, request, now())
                    .await
            }
            None => {
                store
                    .plugin_context_host_request(user, request, now())
                    .await
            }
        }
        .map_err(Into::into)
    }
}

pub(crate) async fn start_invocation(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let input: openwebide_core::plugins::execution::PluginStartRequest =
        parse_json(read_body(req, 5 * 1024 * 1024).await?)?;
    state
        .store
        .authorize_plugin_invocation(user.id, &input, now())
        .await?;
    let result: openwebide_core::plugins::execution::PluginInvocation = send_plugin_rpc(
        &state.store,
        user.id,
        "/plugins/invoke",
        json!({"call":input.call}),
    )
    .await
    .map_err(ApiError::bad_request)?;
    Ok(json_response(200, &result))
}
pub(crate) async fn continue_invocation(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let input: openwebide_core::plugins::execution::ContinuePlugin =
        parse_json(read_body(req, 5 * 1024 * 1024).await?)?;
    let result: openwebide_core::plugins::execution::PluginInvocation = send_plugin_rpc(
        &state.store,
        user.id,
        "/plugins/continue",
        json!({"continuation":input}),
    )
    .await
    .map_err(ApiError::bad_request)?;
    Ok(json_response(200, &result))
}
pub(crate) async fn cancel_invocation(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        id: String,
    }
    let input: Input = parse_json(read_body(req, 4096).await?)?;
    let result: serde_json::Value = send_plugin_rpc(
        &state.store,
        user.id,
        "/plugins/cancel",
        json!({"id":input.id}),
    )
    .await
    .map_err(ApiError::bad_request)?;
    Ok(json_response(200, &result))
}

pub(crate) async fn execution_grants(
    state: &AppState,
    user: UserId,
    session: i64,
    plugins: &[PreparedPlugin],
) -> Result<std::collections::BTreeMap<String, String>, ApiError> {
    let current = state.store.get_session(session, user).await?;
    context_grants(
        state,
        user,
        &openwebide_core::plugins::execution::PluginExecutionContext {
            user_action: false,
            project_id: current.project_id,
            session_id: Some(session),
            primary: None,
        },
        plugins,
    )
    .await
}
pub(crate) async fn context_grants(
    state: &AppState,
    user: UserId,
    context: &openwebide_core::plugins::execution::PluginExecutionContext,
    plugins: &[PreparedPlugin],
) -> Result<std::collections::BTreeMap<String, String>, ApiError> {
    if plugins.len() > 64 {
        return Err(ApiError::bad_request("Too many executable plugins"));
    }
    let mut unique = std::collections::BTreeSet::new();
    if plugins.iter().any(|plugin| !unique.insert(&plugin.digest)) {
        return Err(ApiError::bad_request("Duplicate executable plugin"));
    }
    let mut context = context.clone();
    if context.primary.is_none() && context.session_id.is_none() {
        context.primary = state.store.model_setup(user).await?.defaults.primary;
    }
    let mut grants = std::collections::BTreeMap::new();
    for plugin in plugins {
        let token = format!("{:032x}", rand::random::<u128>());
        state
            .store
            .issue_plugin_context_grant(user, &context, plugin, &token, now())
            .await?;
        grants.insert(plugin.digest.clone(), token);
    }
    Ok(grants)
}
pub(crate) async fn grant_context(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let input: openwebide_core::plugins::execution::PluginGrantRequest =
        parse_json(read_body(req, 256 * 1024).await?)?;
    Ok(json_response(
        200,
        &context_grants(state, user.id, &input.context, &input.plugins).await?,
    ))
}
pub(crate) async fn context_host_request(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let input: openwebide_core::plugins::execution::PluginHostRequest =
        parse_json(read_body(req, 4 * 1024 * 1024).await?)?;
    Ok(json_response(
        200,
        &execute_host_request(&state.store, user.id, None, &input).await?,
    ))
}
pub(crate) async fn grant_execution(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let session = path_id(
        path.strip_suffix("/plugin-grants")
            .ok_or_else(|| ApiError::bad_request("Expected plugin grant path"))?,
        "/api/sessions",
    )?;
    let plugins: Vec<PreparedPlugin> = parse_json(read_body(req, 256 * 1024).await?)?;
    Ok(json_response(
        200,
        &execution_grants(state, user.id, session, &plugins).await?,
    ))
}
pub(crate) async fn host_request(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let session = path_id(
        path.strip_suffix("/plugin-host")
            .ok_or_else(|| ApiError::bad_request("Expected plugin host path"))?,
        "/api/sessions",
    )?;
    let request: openwebide_core::plugins::execution::PluginHostRequest =
        parse_json(read_body(req, 4 * 1024 * 1024).await?)?;
    Ok(json_response(
        200,
        &execute_host_request(&state.store, user.id, Some(session), &request).await?,
    ))
}

pub(crate) async fn list(state: &AppState, user: AuthedUser) -> Result<JsonResp, ApiError> {
    ensure_bundled_plugins(state, user.id).await;
    Ok(json_response(
        200,
        &state.store.plugin_installations(user.id).await?,
    ))
}
pub(crate) async fn record(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let request: RecordPlugin = parse_json(
        read_body(
            req,
            openwebide_core::plugins::MAX_PACKAGE_BYTES + 256 * 1024,
        )
        .await?,
    )?;
    Ok(json_response(
        200,
        &state.store.record_plugin(user.id, &request, now()).await?,
    ))
}
pub(crate) async fn prepare(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    if path != "/api/plugins/prepare" {
        let project = path_id(
            path.strip_suffix("/plugins/prepare")
                .ok_or_else(|| ApiError::bad_request("Expected plugin path"))?,
            "/api/projects",
        )?;
        super::files::remote_project_path(state, user.id, project, "").await?;
    }
    let source: PluginSource = parse_json(read_body(req, 16 * 1024).await?)?;
    source
        .validate()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let (status, body) = crate::bridge::send(
        &state.store,
        "/plugins/prepare",
        json!({"source":source,"user":user.id.get()}).to_string(),
    )
    .await?;
    if status != 200 {
        return Err(if status == 400 {
            ApiError::bad_request(openwebide_core::plugins::execution::host_rpc_error(
                status, &body,
            ))
        } else {
            ApiError::bad_gateway("Plugin preparation failed on the execution host.")
        });
    }
    let prepared: PreparedPlugin =
        serde_json::from_slice(&body).map_err(|error| ApiError::internal(error.to_string()))?;
    prepared
        .validate()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if prepared.source != source {
        return Err(ApiError::internal(
            "Plugin host returned a different source.",
        ));
    }
    Ok(json_response(200, &prepared))
}

pub(crate) async fn preparation(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    use openwebide_core::plugins::preparation::{PluginPreparation, PreparationCommand};
    if path != "/api/plugins/preparation" {
        let project = path_id(
            path.strip_suffix("/plugins/preparation")
                .ok_or_else(|| ApiError::bad_request("Expected plugin preparation path"))?,
            "/api/projects",
        )?;
        super::files::remote_project_path(state, user.id, project, "").await?;
    }
    let command: PreparationCommand = parse_json(read_body(req, 16 * 1024).await?)?;
    command
        .validate()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let mut payload = command.host_payload();
    payload["user"] = json!(user.id.get());
    let (status, body) = crate::bridge::send(
        &state.store,
        &format!("/plugins/prepare/{}", command.operation()),
        payload.to_string(),
    )
    .await?;
    if status != 200 {
        return Err(if status == 400 {
            ApiError::bad_request(openwebide_core::plugins::execution::host_rpc_error(
                status, &body,
            ))
        } else {
            ApiError::bad_gateway("Plugin preparation is unavailable on the execution host.")
        });
    }
    let progress: PluginPreparation =
        serde_json::from_slice(&body).map_err(|error| ApiError::internal(error.to_string()))?;
    progress
        .validate()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if let PreparationCommand::Status { id } | PreparationCommand::Cancel { id } = command
        && progress.id != id
    {
        return Err(ApiError::internal(
            "Plugin host returned a different preparation.",
        ));
    }
    Ok(json_response(200, &progress))
}

use openwebide_core::plugins::{PluginPackage, ProjectPluginCommand, RemovePlugin, marketplace::*};
pub(crate) async fn marketplaces(state: &AppState, user: AuthedUser) -> Result<JsonResp, ApiError> {
    Ok(json_response(
        200,
        &state.store.plugin_marketplaces(user.id).await?,
    ))
}
pub(crate) async fn save_marketplaces(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let request: SaveMarketplaces = parse_json(read_body(req, 64 * 1024).await?)?;
    Ok(json_response(
        200,
        &state
            .store
            .save_plugin_marketplaces(user.id, &request)
            .await?,
    ))
}
pub(crate) async fn refresh_marketplaces(
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let current = state.store.plugin_marketplaces(user.id).await?;
    let mut catalogs = Vec::new();
    let mut failures = Vec::new();
    for source in &current.sources {
        let result = async {
            let (status,body)=crate::bridge::send(&state.store,"/plugins/catalog",json!({"marketplace":source,"user":user.id.get()}).to_string()).await.map_err(|_|"Plugin execution host is unavailable.".to_string())?;
            if status!=200 { return Err("Could not refresh this marketplace on the server host. Check its repository, reference and catalog path.".to_string()); }
            let catalog:CachedMarketplace=serde_json::from_slice(&body).map_err(|error|error.to_string())?;
            catalog.validate().map_err(|error|error.to_string())?;
            if catalog.source!=*source {return Err("Catalog host returned a different source.".into());}
            Ok(catalog)
        }.await;
        match result {
            Ok(catalog) => catalogs.push(catalog),
            Err(message) => failures.push(MarketplaceFailure {
                source: source.clone(),
                message,
            }),
        }
    }
    let settings = state
        .store
        .cache_plugin_marketplaces(user.id, current.revision, catalogs)
        .await?;
    Ok(json_response(
        200,
        &MarketplaceRefresh { settings, failures },
    ))
}
fn project_id(path: &str, suffix: &str) -> Result<i64, ApiError> {
    path_id(
        path.strip_suffix(suffix)
            .ok_or_else(|| ApiError::bad_request("Expected project plugin path"))?,
        "/api/projects",
    )
}
pub(crate) async fn project_list(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    ensure_bundled_plugins(state, user.id).await;
    Ok(json_response(
        200,
        &state
            .store
            .project_plugins(user.id, project_id(path, "/plugins")?)
            .await?,
    ))
}
pub(crate) async fn project_command(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let command: ProjectPluginCommand =
        parse_json(read_body(req, openwebide_core::plugins::MAX_PACKAGE_BYTES).await?)?;
    Ok(json_response(
        200,
        &state
            .store
            .project_plugin_command(user.id, project_id(path, "/plugins")?, &command, now())
            .await?,
    ))
}
pub(crate) async fn remove(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let request: RemovePlugin = parse_json(read_body(req, 16 * 1024).await?)?;
    Ok(json_response(
        200,
        &state.store.remove_plugin(user.id, &request).await?,
    ))
}
pub(crate) async fn package(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    if path != "/api/plugins/package" {
        let project = project_id(path, "/plugins/package")?;
        super::files::remote_project_path(state, user.id, project, "").await?;
    }
    let expected: PreparedPlugin = parse_json(read_body(req, 256 * 1024).await?)?;
    expected
        .validate()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let (status, body) = crate::bridge::send(
        &state.store,
        "/plugins/package",
        json!({"prepared":expected,"user":user.id.get()}).to_string(),
    )
    .await?;
    if status != 200 {
        return Err(ApiError::bad_gateway(
            "Could not load the installed plugin on this host.",
        ));
    }
    let package: PluginPackage =
        serde_json::from_slice(&body).map_err(|error| ApiError::internal(error.to_string()))?;
    package
        .validate()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if package.prepared.source != expected.source
        || package.prepared.manifest != expected.manifest
        || package.prepared.digest != expected.digest
    {
        return Err(ApiError::bad_gateway(
            "Host returned a different plugin version.",
        ));
    }
    Ok(json_response(200, &package))
}

/// Account initialization uses the normal server-host primitives and shared storage lifecycle.
/// Host downtime does not prevent sign-in or viewing existing installations; refresh retries it.
pub(super) async fn ensure_bundled_plugins(state: &AppState, user: UserId) {
    let result: Result<(), ApiError> = async {
        let pending = state.store.pending_bundled_plugins(user).await?;
        if pending.is_empty() {
            return Ok(());
        }
        let mut packages = Vec::new();
        for source in pending {
            let adapter = PlanningHost::new(state, user, 0);
            let result =
                openwebide_core::plugins::preparation::prepare_on_host(&adapter, &source).await;
            adapter.flush_preparations().await;
            let prepared = result.map_err(ApiError::bad_gateway)?;
            let (status, body) = crate::bridge::send(
                &state.store,
                "/plugins/package",
                json!({"prepared":prepared,"user":user.get()}).to_string(),
            )
            .await?;
            if status != 200 {
                return Err(ApiError::bad_gateway("Bundled plugin host unavailable"));
            }
            let package: PluginPackage = serde_json::from_slice(&body)
                .map_err(|error| ApiError::internal(error.to_string()))?;
            package
                .validate()
                .map_err(|error| ApiError::internal(error.to_string()))?;
            if package.prepared != prepared {
                return Err(ApiError::bad_gateway("Bundled plugin receipt mismatch"));
            }
            packages.push(package);
        }
        state
            .store
            .initialize_bundled_plugins(user, &packages, now())
            .await?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        eprintln!("Bundled plugin initialization deferred: {error:?}");
    }
}

#[cfg(test)]
mod completion_tests;
