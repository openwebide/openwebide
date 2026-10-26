//! Model runtime primitives for shared background workflows.
use super::*;
use crate::state::AppDb;
use openwebide_agent::compaction::CompactionSource;
use openwebide_storage::Store;

#[derive(Clone)]
pub(crate) struct ModelSource<S> {
    pub store: S,
    pub user: UserId,
}
impl<S: std::ops::Deref<Target = Store<AppDb>> + Send + Sync> CompactionSource for ModelSource<S> {
    fn available(&self) -> bool {
        true
    }
    async fn runtime(
        &self,
        selection: &openwebide_core::ModelSelection,
    ) -> Result<openwebide_core::ModelRuntime, String> {
        super::model_setup::runtime_store(
            &self.store,
            self.user,
            selection.server_id,
            Some(&selection.model),
        )
        .await
        .map_err(|error| error.to_string())
    }
    async fn complete(
        &self,
        request: &ChatRequest,
    ) -> Result<openwebide_core::ChatCompletion, String> {
        complete(&self.store, self.user, request.clone())
            .await
            .map_err(|error| error.to_string())
    }
    async fn complete_with_timeout(
        &self,
        request: &ChatRequest,
        timeout_seconds: u32,
    ) -> Result<openwebide_core::ChatCompletion, String> {
        let mut request = request.clone();
        provider(&self.store, self.user, &mut request, Some(timeout_seconds))
            .await
            .map_err(|error| error.to_string())?
            .chat_tools(&request)
            .await
            .map_err(|error| error.to_string())
    }
    async fn context_limit(&self, request: &ChatRequest) -> Option<usize> {
        super::model_setup::context_limit_store(
            &self.store,
            self.user,
            request.connection_id,
            request.model.as_deref(),
        )
        .await
        .ok()
        .flatten()
    }
    async fn tokens(&self, request: &ChatRequest) -> Option<usize> {
        tokens(&self.store, self.user, request.clone())
            .await
            .ok()
            .flatten()
    }
}
async fn provider(
    store: &Store<AppDb>,
    user: UserId,
    request: &mut ChatRequest,
    timeout_seconds: Option<u32>,
) -> Result<Provider<SpinHttpClient>, ApiError> {
    request
        .model_settings
        .validate()
        .map_err(ApiError::bad_request)?;
    let mut runtime = super::model_setup::runtime_store(
        store,
        user,
        request.connection_id,
        request.model.as_deref(),
    )
    .await?;
    if let Some(timeout_seconds) = timeout_seconds {
        runtime.transport.timeout_seconds = runtime.transport.timeout_seconds.min(timeout_seconds);
    }
    // Explicit operation settings are already resolved by the shared workflow.
    request.model = runtime.connection.model.clone();
    request.model_settings.context_limit = runtime.settings.context_limit;
    request.model_settings.max_output_tokens =
        request.model_settings.max_output_tokens.map(|output| {
            output
                .min(runtime.settings.max_output_tokens.unwrap_or(usize::MAX))
                .min(runtime.settings.context_limit.unwrap_or(usize::MAX))
        });
    Ok(Provider::for_connection(
        &runtime.connection,
        SpinHttpClient::default().with_transport(runtime.transport),
    ))
}
pub(crate) async fn complete(
    store: &Store<AppDb>,
    user: UserId,
    mut request: ChatRequest,
) -> Result<openwebide_core::ChatCompletion, ApiError> {
    provider(store, user, &mut request, None)
        .await?
        .chat_tools(&request)
        .await
        .map_err(Into::into)
}
pub(crate) async fn tokens(
    store: &Store<AppDb>,
    user: UserId,
    mut request: ChatRequest,
) -> Result<Option<usize>, ApiError> {
    Ok(provider(store, user, &mut request, Some(5))
        .await?
        .request_tokens(&request)
        .await)
}
pub(crate) async fn route(
    req: Request,
    state: &AppState,
    user: AuthedUser,
    count: bool,
) -> Result<JsonResp, ApiError> {
    let request = parse_json(read_body(req, CHAT_BODY_LIMIT).await?)?;
    if count {
        Ok(json_response(
            200,
            &tokens(&state.store, user.id, request).await?,
        ))
    } else {
        Ok(json_response(
            200,
            &complete(&state.store, user.id, request).await?,
        ))
    }
}

pub(crate) async fn background(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let operation: openwebide_core::BackgroundCompletion =
        parse_json(read_body(req, CHAT_BODY_LIMIT).await?)?;
    if !(1..=30).contains(&operation.timeout_seconds) {
        return Err(ApiError::bad_request(
            "Background deadline must be 1–30 seconds",
        ));
    }
    let mut request = operation.request;
    let completion = provider(
        &state.store,
        user.id,
        &mut request,
        Some(operation.timeout_seconds),
    )
    .await?
    .chat_tools(&request)
    .await?;
    Ok(json_response(200, &completion))
}
