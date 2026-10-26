use super::*;
// -- models & chat ----------------------------------------------------------------

pub(crate) async fn list_models(
    req: Request,
    state: &AppState,
    _user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let params = query(&req);
    let id = params
        .get("connection_id")
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| ApiError::bad_request("missing ?connection_id=<id>"))?;
    let connection = state.store.get_connection(id).await?;
    let provider = Provider::for_connection(
        &connection,
        SpinHttpClient::default()
            .with_transport(state.store.server_transport(connection.id).await?),
    );
    let models = provider.list_models().await?;
    let detected = super::model_setup::discover_models(&state.store, &connection, &models).await?;
    let chat_models = models
        .into_iter()
        .filter(|model| {
            detected
                .get(&model.name)
                .is_none_or(openwebide_core::ModelDetection::chat_capable)
        })
        .collect::<Vec<_>>();
    Ok(json_response(200, &chat_models))
}

pub(crate) async fn chat(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let body = read_body(req, CHAT_BODY_LIMIT).await?;
    let mut request: ChatRequest = parse_json(body)?;
    let runtime = super::model_setup::runtime(
        state,
        user.id,
        request.connection_id,
        request.model.as_deref(),
    )
    .await?;
    runtime.apply_to(&mut request);
    let connection = runtime.connection;
    let provider = Provider::for_connection(
        &connection,
        SpinHttpClient::default().with_transport(runtime.transport),
    );
    request.system_prompt = Some(with_temporal_context(request.system_prompt, now()));
    let reply = provider.chat(&request).await?;
    Ok(json_response(200, &json!({ "reply": reply })))
}

pub(crate) async fn chat_tools(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let body = read_body(req, CHAT_BODY_LIMIT).await?;
    let mut request: ChatRequest = parse_json(body)?;
    let runtime = super::model_setup::runtime(
        state,
        user.id,
        request.connection_id,
        request.model.as_deref(),
    )
    .await?;
    runtime.apply_to(&mut request);
    let connection = runtime.connection;
    let memo = ToolStreamMemo::new(connection.tool_stream_unsupported);
    let provider = Provider::for_connection_with_memo(
        &connection,
        SpinHttpClient::default().with_transport(runtime.transport),
        memo.clone(),
    );
    request.system_prompt = Some(with_temporal_context(request.system_prompt, now()));
    let response = provider.chat_tools(&request).await;
    if memo.take_unrecorded()
        && let Err(error) = state
            .store
            .set_model_tool_stream_unsupported(
                connection.id,
                connection.model.as_deref().unwrap_or_default(),
                connection.tool_stream_revision,
            )
            .await
    {
        eprintln!(
            "connection {}: set_tool_stream_unsupported: {error}",
            connection.id
        );
    }
    let response = response?;
    Ok(json_response(200, &response))
}

pub(crate) async fn model_context(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let params = query(&req);
    let connection_id = params
        .get("connection_id")
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| ApiError::bad_request("missing ?connection_id=<id>"))?;
    let model = params.get("model").cloned();
    let limit = super::model_setup::context_limit_store(
        &state.store,
        user.id,
        connection_id,
        model.as_deref(),
    )
    .await?;
    Ok(json_response(200, &json!({ "context_limit": limit })))
}
