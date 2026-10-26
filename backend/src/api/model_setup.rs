use super::*;
use openwebide_core::{ModelDefaults, ModelProfile, ModelRuntime, ServerSettingsUpdate};

pub(crate) async fn get(state: &AppState, user: AuthedUser) -> Result<JsonResp, ApiError> {
    Ok(json_response(200, &state.store.model_setup(user.id).await?))
}
pub(crate) async fn defaults(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let defaults: ModelDefaults = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    state.store.save_model_defaults(user.id, &defaults).await?;
    // Keep the old default-server preference in sync for older clients.
    if let Some(primary) = &defaults.primary {
        state
            .store
            .set_user_setting(
                user.id,
                "default_connection",
                &primary.server_id.to_string(),
            )
            .await?;
    }
    get(state, user).await
}
pub(crate) async fn profile(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let profile: ModelProfile = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    state.store.save_model_profile(&profile).await?;
    get(state, user).await
}
pub(crate) async fn settings(
    req: Request,
    state: &AppState,
    path: &str,
    write: bool,
) -> Result<JsonResp, ApiError> {
    let id = server_id(path)?;
    if write {
        let update: ServerSettingsUpdate = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
        state.store.save_server_settings(id, &update).await?;
    }
    Ok(json_response(200, &state.store.server_settings(id).await?))
}
pub(crate) fn server_id(path: &str) -> Result<i64, ApiError> {
    path.strip_prefix("/api/connections/")
        .and_then(|rest| rest.split('/').next())
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| ApiError::bad_request("Expected a server id."))
}

pub(crate) async fn runtime(
    state: &AppState,
    user: UserId,
    id: i64,
    model: Option<&str>,
) -> Result<ModelRuntime, ApiError> {
    runtime_store(&state.store, user, id, model).await
}
pub(crate) async fn runtime_store(
    store: &openwebide_storage::Store<crate::state::AppDb>,
    user: UserId,
    id: i64,
    model: Option<&str>,
) -> Result<ModelRuntime, ApiError> {
    let mut connection = store.get_connection(id).await?;
    if !connection.enabled {
        return Err(ApiError::bad_request("Server is disabled."));
    }
    let transport = store.server_transport(id).await?;
    let setup = store.model_setup(user).await?;
    let model = model
        .map(str::to_string)
        .or_else(|| {
            setup
                .defaults
                .primary
                .as_ref()
                .filter(|selection| selection.server_id == id)
                .map(|selection| selection.model.clone())
        })
        .or_else(|| connection.model.clone());
    let selected_model = model;
    let model = selected_model.clone().unwrap_or_default();
    let explicit = setup
        .profiles
        .iter()
        .any(|profile| profile.selection.server_id == id && profile.selection.model == model);
    let mut settings = setup.resolve(id, &model);
    if !model.is_empty() {
        let detection = discover_model(store, &connection, &model).await?;
        settings = detection.defaults_for(&settings);
    }
    if !explicit && (connection.model.is_none() || connection.model.as_deref() == Some(&model)) {
        settings.context_limit = settings.context_limit.or(connection.context_limit);
    }
    connection.tool_stream_unsupported = connection.tool_stream_unsupported
        || store
            .model_tool_stream_unsupported(&connection, &model)
            .await?;
    connection.tool_stream_unsupported = settings
        .stream_tools
        .map_or(connection.tool_stream_unsupported, |enabled| !enabled);
    connection.context_limit = settings.context_limit;
    connection.model = selected_model;
    Ok(ModelRuntime {
        connection,
        settings,
        transport,
    })
}

pub(crate) async fn context_limit_store(
    store: &openwebide_storage::Store<crate::state::AppDb>,
    user: UserId,
    id: i64,
    model: Option<&str>,
) -> Result<Option<usize>, ApiError> {
    let mut runtime = runtime_store(store, user, id, model).await?;
    if let Some(limit) = runtime.connection.context_limit.filter(|&limit| limit > 0) {
        return Ok(Some(limit));
    }
    runtime.transport.timeout_seconds = runtime.transport.timeout_seconds.min(5);
    let provider = Provider::for_connection(
        &runtime.connection,
        SpinHttpClient::default().with_transport(runtime.transport),
    );
    let limit = provider
        .context_limit(runtime.connection.model.as_deref())
        .await
        .unwrap_or(None)
        .filter(|&limit| limit > 0);
    if let (Some(limit), Some(model)) = (
        limit,
        runtime
            .connection
            .model
            .as_deref()
            .filter(|m| !m.is_empty()),
    ) && let Ok(Some(mut cached)) = store.model_detection(&runtime.connection, model).await
        && cached.context_limit.is_none()
    {
        cached.context_limit = Some(limit);
        if runtime.connection.kind == openwebide_core::ProviderKind::Ollama {
            cached.loaded = Some(true);
            cached.source = "Ollama /api/ps runtime context".into();
        }
        let _ = store
            .save_model_detection(&runtime.connection, model, &cached)
            .await;
    }
    Ok(limit)
}

async fn probe_model(
    store: &openwebide_storage::Store<crate::state::AppDb>,
    connection: &openwebide_core::Connection,
    model: &str,
    fallback: bool,
) -> Result<openwebide_core::ModelDetection, ApiError> {
    let mut transport = store.server_transport(connection.id).await?;
    transport.timeout_seconds = transport.timeout_seconds.min(5);
    let detection = openwebide_llm::discovery::detect_with_preset(
        connection,
        model,
        transport.preset,
        SpinHttpClient::default().with_transport(transport),
    )
    .await;
    let detection = match detection {
        Ok(detected) => detected,
        Err(error @ openwebide_llm::ProviderError::Authentication) => return Err(error.into()),
        Err(error) if fallback => {
            eprintln!("model discovery: {error}");
            return Ok(openwebide_core::ModelDetection::default());
        }
        Err(error) => return Err(error.into()),
    };
    store
        .save_model_detection(connection, model, &detection)
        .await?;
    Ok(detection)
}
pub(crate) async fn discover_model(
    store: &openwebide_storage::Store<crate::state::AppDb>,
    connection: &openwebide_core::Connection,
    model: &str,
) -> Result<openwebide_core::ModelDetection, ApiError> {
    if let Some(cached) = store.model_detection(connection, model).await? {
        return Ok(cached);
    }
    probe_model(store, connection, model, true).await
}

pub(crate) async fn discover_models(
    store: &openwebide_storage::Store<crate::state::AppDb>,
    connection: &openwebide_core::Connection,
    models: &[openwebide_core::ModelInfo],
) -> Result<std::collections::BTreeMap<String, openwebide_core::ModelDetection>, ApiError> {
    let mut found = std::collections::BTreeMap::new();
    let mut missing = Vec::new();
    for model in models {
        if let Some(cached) = store.model_detection(connection, &model.name).await? {
            found.insert(model.name.clone(), cached);
        } else {
            missing.push(model.clone());
        }
    }
    if !missing.is_empty() {
        let mut transport = store.server_transport(connection.id).await?;
        transport.timeout_seconds = transport.timeout_seconds.min(5);
        let detected = openwebide_llm::discovery::detect_models(
            connection,
            &missing,
            transport.preset,
            SpinHttpClient::default().with_transport(transport),
        )
        .await?;
        for (model, result) in detected {
            if let Ok(detection) = result {
                store
                    .save_model_detection(connection, &model, &detection)
                    .await?;
                found.insert(model, detection);
            }
        }
    }
    Ok(found)
}

pub(crate) async fn native_runtime(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
    native: bool,
) -> Result<JsonResp, ApiError> {
    let params = query(&req);
    let runtime = runtime(
        state,
        user.id,
        server_id(path)?,
        params.get("model").map(String::as_str),
    )
    .await?;
    Ok(runtime_response(runtime, native))
}

pub(crate) fn runtime_response(mut runtime: ModelRuntime, native: bool) -> JsonResp {
    if !native {
        runtime.transport = Default::default();
    }
    json_response(200, &runtime)
}

#[derive(Deserialize)]
struct DetectBody {
    server_id: i64,
    model: String,
}
pub(crate) async fn detect(
    req: Request,
    state: &AppState,
    _user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let body: DetectBody = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    let connection = state.store.get_connection(body.server_id).await?;
    let detection = probe_model(&state.store, &connection, &body.model, false).await?;
    Ok(json_response(200, &detection))
}
#[derive(Deserialize)]
struct InspectBody {
    base_url: String,
    kind: Option<openwebide_core::ProviderKind>,
}
pub(crate) async fn inspect(req: Request) -> Result<JsonResp, ApiError> {
    let body: InspectBody = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    validate_url(&body.base_url)?;
    let transport = openwebide_core::ServerTransport {
        timeout_seconds: 3,
        ..Default::default()
    };
    let discovery = openwebide_llm::discovery::inspect(
        &body.base_url,
        body.kind,
        SpinHttpClient::default().with_transport(transport),
    )
    .await?;
    Ok(json_response(200, &discovery))
}
pub(crate) fn validate_url(url: &str) -> Result<(), ApiError> {
    let uri = url
        .parse::<spin_sdk::http::Uri>()
        .map_err(|_| ApiError::bad_request("Enter an HTTP or HTTPS server URL."))?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.host().is_none()
        || uri
            .authority()
            .is_some_and(|value| value.as_str().contains('@'))
        || uri.query().is_some()
        || url.contains('#')
    {
        return Err(ApiError::bad_request(
            "Use an HTTP or HTTPS base URL without credentials, query parameters or fragments.",
        ));
    }
    Ok(())
}
pub(crate) async fn discover() -> Result<JsonResp, ApiError> {
    let transport = openwebide_core::ServerTransport {
        timeout_seconds: 2,
        ..Default::default()
    };
    let found =
        openwebide_llm::discovery::discover(SpinHttpClient::default().with_transport(transport))
            .await;
    Ok(json_response(200, &found))
}

pub(crate) async fn preview(
    req: Request,
    state: &AppState,
    detect: bool,
) -> Result<JsonResp, ApiError> {
    let probe: openwebide_core::ModelProbe = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    validate_url(&probe.base_url)?;
    let transport = match probe.server_id {
        Some(id) => state.store.server_transport(id).await?,
        None => openwebide_core::ServerTransport::default(),
    }
    .updated(&probe.transport)
    .map_err(ApiError::bad_request)?;
    let preset = transport.preset;
    let mut transport = transport;
    transport.timeout_seconds = transport.timeout_seconds.min(5);
    let http = SpinHttpClient::default().with_transport(transport);
    if detect {
        let connection = openwebide_core::Connection {
            id: probe.server_id.unwrap_or(0),
            name: String::new(),
            kind: probe.kind,
            base_url: probe.base_url,
            model: probe.model.clone(),
            enabled: true,
            context_limit: None,
            tool_stream_unsupported: false,
            tool_stream_revision: 0,
            tool_selection: Default::default(),
        };
        let model = probe
            .model
            .ok_or_else(|| ApiError::bad_request("Choose a model to detect."))?;
        let result =
            openwebide_llm::discovery::detect_with_preset(&connection, &model, preset, http)
                .await?;
        Ok(json_response(200, &result))
    } else {
        let mut result =
            openwebide_llm::discovery::inspect(&probe.base_url, Some(probe.kind), http.clone())
                .await?;
        let connection = openwebide_core::Connection {
            id: probe.server_id.unwrap_or(0),
            name: String::new(),
            kind: probe.kind,
            base_url: probe.base_url,
            model: None,
            enabled: true,
            context_limit: None,
            tool_stream_unsupported: false,
            tool_stream_revision: 0,
            tool_selection: Default::default(),
        };
        let detected =
            openwebide_llm::discovery::detect_models(&connection, &result.models, preset, http)
                .await?;
        for (model, detection) in detected {
            match detection {
                Ok(detection) => {
                    result.detections.insert(model, detection);
                }
                Err(message) => {
                    result.detection_errors.insert(model, message);
                }
            }
        }
        Ok(json_response(200, &result))
    }
}

pub(crate) async fn test_model(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    let probe: openwebide_core::ModelProbe = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    validate_url(&probe.base_url)?;
    let transport = match probe.server_id {
        Some(id) => state.store.server_transport(id).await?,
        None => Default::default(),
    }
    .updated(&probe.transport)
    .map_err(ApiError::bad_request)?;
    let model = probe
        .model
        .ok_or_else(|| ApiError::bad_request("Choose a model to test."))?;
    let connection = openwebide_core::Connection {
        id: probe.server_id.unwrap_or(0),
        name: String::new(),
        kind: probe.kind,
        base_url: probe.base_url,
        model: Some(model.clone()),
        enabled: true,
        context_limit: None,
        tool_stream_unsupported: false,
        tool_stream_revision: 0,
        tool_selection: Default::default(),
    };
    let provider = Provider::for_connection(
        &connection,
        SpinHttpClient::default().with_transport(transport),
    );
    let request = ChatRequest {
        connection_id: connection.id,
        model: Some(model),
        system_prompt: None,
        messages: vec![],
        tools: vec![],
        model_settings: Default::default(),
    };
    let result = openwebide_llm::model_test::test(&provider, request).await?;
    Ok(json_response(200, &result))
}

pub(crate) async fn save_review(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    #[derive(Deserialize)]
    struct Review {
        probe: openwebide_core::ModelProbe,
        profiles: Vec<ModelProfile>,
    }
    let review: Review = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    validate_url(&review.probe.base_url)?;
    let saved = state
        .store
        .save_model_setup(user.id, &review.probe, &review.profiles)
        .await?;
    Ok(json_response(200, &saved))
}
