use super::http::{read_body, read_json, respond};
use super::ws::handle_websocket;
use super::{PermitCell, ServerConfig, check_request};
use crate::terminals::session::SessionManager;
use crate::{
    BridgeError,
    exec::{GitOperation, GitRequest, SpawnSpec},
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{CONNECTION, CONTENT_TYPE, HOST, ORIGIN, UPGRADE};
use hyper::{HeaderMap, Request, Response, StatusCode, body::Incoming};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use std::convert::Infallible;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{
    handshake::derive_accept_key,
    protocol::{Role, WebSocketConfig},
};
use tracing::Instrument;
const MAX_WS_MESSAGE: usize = 16 * 1024 * 1024;

#[derive(Deserialize)]
struct ExecPayload {
    command: String,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    #[serde(default)]
    cwd: Option<String>,
}

fn default_timeout() -> u64 {
    30
}

/// Route a single request. `check_request` runs first for every method and path, before any
/// side effect; only requests that pass it reach a handler.
pub(super) async fn route(
    req: Request<Incoming>,
    addr: std::net::SocketAddr,
    sessions: SessionManager,
    config: ServerConfig,
    permit_cell: PermitCell,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if req
        .headers()
        .contains_key(openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER)
        && (req.uri().path() != "/health" || is_websocket_upgrade(req.headers()))
    {
        return Ok(rejection_response(403));
    }
    let host = req.headers().get(HOST).and_then(|v| v.to_str().ok());
    let origin = req.headers().get(ORIGIN).and_then(|v| v.to_str().ok());
    let content_type = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let method = req.method().as_str().to_string();

    let echo_origin = match check_request(host, origin, &method, content_type, &config) {
        Ok(o) => o,
        Err(code) => return Ok(rejection_response(code)),
    };
    let allowed_origin = echo_origin.as_deref();

    if is_websocket_upgrade(req.headers()) {
        return Ok(handle_ws_upgrade(
            req,
            allowed_origin,
            sessions,
            config,
            permit_cell,
        ));
    }

    // A reverse proxy connects over loopback on behalf of a network client.
    // Only a direct backend request may bootstrap the bridge secret.
    let direct_loopback = addr.ip().is_loopback()
        && [
            "forwarded",
            "x-forwarded-for",
            "x-forwarded-host",
            "x-forwarded-proto",
        ]
        .iter()
        .all(|header| !req.headers().contains_key(*header));
    let path = req.uri().path().to_string();

    let is_api_req = path == "/exec"
        || path == "/environment"
        || path == "/scheduler/host"
        || path == "/host/info"
        || path == "/host/admin"
        || path == "/host/probe"
        || path == "/host/input"
        || matches!(
            path.as_str(),
            "/plugins/prepare"
                | "/plugins/prepare/start"
                | "/plugins/prepare/status"
                | "/plugins/prepare/cancel"
                | "/plugins/package"
                | "/plugins/catalog"
                | "/plugins/invoke"
                | "/plugins/continue"
                | "/plugins/cancel"
        )
        || path.starts_with("/git/")
        || path == "/models/discover";
    let mut principal = None;
    if is_api_req && method != "OPTIONS" {
        let auth_header = req
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok());
        let mut authorized = false;

        if let Some(h) = auth_header
            && let Some(token) = h.strip_prefix("Bearer ")
        {
            if origin.is_none() {
                authorized =
                    crate::secret::constant_time_eq(token.as_bytes(), config.secret.as_bytes());
            } else {
                let now = i64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                )
                .unwrap_or(i64::MAX);
                principal = crate::auth::authenticate(token, &config, now).ok();
                authorized = principal.is_some();
            }
        }

        if !authorized {
            return Ok(execution_response(
                Err(BridgeError::Unauthorized("bridge token required".into())),
                allowed_origin,
            ));
        }
    }

    match (method.as_str(), path.as_str()) {
        ("OPTIONS", _) => Ok(preflight_response(req.headers(), allowed_origin)),
        (
            "POST",
            "/plugins/prepare"
            | "/plugins/prepare/start"
            | "/plugins/prepare/status"
            | "/plugins/prepare/cancel"
            | "/plugins/package"
            | "/plugins/catalog"
            | "/plugins/invoke"
            | "/plugins/continue"
            | "/plugins/cancel",
        ) => {
            let result = async {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Payload {
                    source: Option<openwebide_core::plugins::PluginSource>,
                    prepared: Option<openwebide_core::plugins::PreparedPlugin>,
                    marketplace: Option<openwebide_core::plugins::marketplace::MarketplaceSource>,
                    user: Option<i64>,
                    call: Option<openwebide_core::plugins::execution::InvokePlugin>,
                    continuation: Option<openwebide_core::plugins::execution::ContinuePlugin>,
                    id: Option<String>,
                }
                let payload: Payload = read_json(
                    req.into_body(),
                    openwebide_plugin_runtime::MAX_MESSAGE_BYTES + 256 * 1024,
                )
                .await?;
                let owner = match principal {
                    Some(crate::auth::Principal::User { user_id }) => format!("user:{user_id}"),
                    Some(crate::auth::Principal::Paired) => "paired".into(),
                    None => format!(
                        "user:{}",
                        payload.user.filter(|id| *id > 0).ok_or_else(
                            || BridgeError::Validation("An authenticated user is required.".into())
                        )?
                    ),
                };
                let operation = async {
                    use openwebide_core::plugins::PluginError;
                    let result = match path.as_str() {
                        "/plugins/invoke" => serde_json::to_value(
                            config
                                .plugin_invocations
                                .start(
                                    &config.plugins,
                                    &owner,
                                    payload.call.ok_or_else(|| {
                                        PluginError::Invalid("Plugin call is required.".into())
                                    })?,
                                )
                                .await
                                .map_err(PluginError::Invalid)?,
                        ),
                        "/plugins/continue" => serde_json::to_value(
                            config
                                .plugin_invocations
                                .resume(
                                    &owner,
                                    payload.continuation.ok_or_else(|| {
                                        PluginError::Invalid(
                                            "Plugin continuation is required.".into(),
                                        )
                                    })?,
                                )
                                .await
                                .map_err(PluginError::Invalid)?,
                        ),
                        "/plugins/cancel" => {
                            config
                                .plugin_invocations
                                .cancel(
                                    &owner,
                                    &payload.id.ok_or_else(|| {
                                        PluginError::Invalid(
                                            "Plugin invocation id is required.".into(),
                                        )
                                    })?,
                                )
                                .await
                                .map_err(PluginError::Invalid)?;
                            serde_json::to_value(serde_json::json!({"cancelled":true}))
                        }
                        "/plugins/prepare/start" => serde_json::to_value(
                            config
                                .plugins
                                .preparations
                                .start(
                                    config.plugins.clone(),
                                    owner.clone(),
                                    crate::scheduled::host(&config).id,
                                    payload.source.ok_or_else(|| {
                                        PluginError::Invalid("Plugin source is required.".into())
                                    })?,
                                )
                                .await?,
                        ),
                        "/plugins/prepare/status" | "/plugins/prepare/cancel" => {
                            let id = payload.id.ok_or_else(|| {
                                PluginError::Invalid("Plugin preparation id is required.".into())
                            })?;
                            serde_json::to_value(if path.ends_with("/cancel") {
                                config.plugins.preparations.cancel(&owner, &id).await?
                            } else {
                                config.plugins.preparations.status(&owner, &id).await?
                            })
                        }
                        "/plugins/prepare" => serde_json::to_value(
                            config
                                .plugins
                                .prepare(
                                    &owner,
                                    crate::scheduled::host(&config).id,
                                    &payload.source.ok_or_else(|| {
                                        PluginError::Invalid("Plugin source is required.".into())
                                    })?,
                                )
                                .await?,
                        ),
                        "/plugins/package" => serde_json::to_value(
                            config
                                .plugins
                                .package(
                                    &owner,
                                    crate::scheduled::host(&config).id,
                                    &payload.prepared.ok_or_else(|| {
                                        PluginError::Invalid(
                                            "Installed plugin receipt is required.".into(),
                                        )
                                    })?,
                                )
                                .await?,
                        ),
                        _ => {
                            let now = i64::try_from(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            )
                            .unwrap_or(i64::MAX);
                            serde_json::to_value(
                                config
                                    .plugins
                                    .catalog(
                                        &owner,
                                        &payload.marketplace.ok_or_else(|| {
                                            PluginError::Invalid(
                                                "Marketplace source is required.".into(),
                                            )
                                        })?,
                                        now,
                                    )
                                    .await?,
                            )
                        }
                    };
                    result.map_err(|error| PluginError::Host(error.to_string()))
                };
                let value = tokio::time::timeout(std::time::Duration::from_secs(180), operation)
                    .await
                    .map_err(|_| BridgeError::Execution("Plugin host request timed out.".into()))?
                    .map_err(|error| match error {
                        openwebide_core::plugins::PluginError::Invalid(message)
                        | openwebide_core::plugins::PluginError::Conflict(message) => {
                            BridgeError::Validation(message)
                        }
                        openwebide_core::plugins::PluginError::Host(message) => {
                            BridgeError::Execution(message)
                        }
                    })?;
                serde_json::to_string(&value)
                    .map_err(|error| BridgeError::Execution(error.to_string()))
            }
            .await;
            Ok(execution_response(result, allowed_origin))
        }

        ("POST", "/host/probe" | "/host/input") => {
            if origin.is_some() || config.pairing_token.is_some() {
                return Ok(execution_response(
                    Err(BridgeError::Forbidden(
                        "Host administration requires the authenticated server bridge.".into(),
                    )),
                    allowed_origin,
                ));
            }
            let result = async {
                let backend = crate::runs::backend_client::BackendClient::new(
                    config.backend_url.clone(),
                    config.secret.clone(),
                    crate::runs::http_client::ReqwestHttpClient::default(),
                );
                if path == "/host/probe" {
                    use openwebide_core::host_admin::HostInventoryAdapter;
                    let connection = crate::host_admin::connection(&backend)
                        .await
                        .map_err(BridgeError::Execution)?;
                    let host =
                        crate::host_admin::ssh::SshHost::new(connection, config.execution.clone())
                            .map_err(BridgeError::Execution)?;
                    serde_json::to_string(&host.overview().await.map_err(BridgeError::Execution)?)
                        .map_err(|error| BridgeError::Execution(error.to_string()))
                } else {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct InputPayload {
                        user: i64,
                        session: i64,
                        input: openwebide_core::host_admin::HostInput,
                    }
                    let body: InputPayload = read_json(req.into_body(), 16 * 1024).await?;
                    use crate::runs::backend_client::RunBackend;
                    backend
                        .host_journal(&openwebide_core::host_admin::HostJournalCommand::List {
                            user: body.user,
                            session: body.session,
                        })
                        .await
                        .map_err(BridgeError::Execution)?;
                    crate::host_admin::interactive::registry()
                        .input(body.user, body.session, body.input)
                        .map_err(BridgeError::Validation)?;
                    Ok("{\"ok\":true}".into())
                }
            }
            .await;
            Ok(execution_response(result, allowed_origin))
        }
        ("POST", "/host/admin") => {
            if origin.is_some() || config.pairing_token.is_some() {
                return Ok(execution_response(
                    Err(BridgeError::Forbidden(
                        "Host administration requires the authenticated server bridge.".into(),
                    )),
                    allowed_origin,
                ));
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct HostPayload {
                user: i64,
                session: i64,
                request: openwebide_core::host_admin::HostRequest,
            }
            let result = async {
                let body: HostPayload = read_json(req.into_body(), config.limits.max_body).await?;
                let backend = std::sync::Arc::new(crate::runs::backend_client::BackendClient::new(
                    config.backend_url.clone(),
                    config.secret.clone(),
                    crate::runs::http_client::ReqwestHttpClient::default(),
                ));
                let response = crate::host_admin::request(
                    backend,
                    config.execution.clone(),
                    body.user,
                    body.session,
                    body.request,
                )
                .await
                .map_err(BridgeError::Execution)?;
                serde_json::to_string(&response)
                    .map_err(|error| BridgeError::Execution(error.to_string()))
            }
            .await;
            Ok(execution_response(result, allowed_origin))
        }
        ("GET" | "POST", "/host/info") => Ok(execution_response(
            config.execution.host_info().await.and_then(|info| {
                serde_json::to_string(&info)
                    .map_err(|error| BridgeError::Execution(error.to_string()))
            }),
            allowed_origin,
        )),
        ("GET", "/scheduler/host") => Ok(respond(
            StatusCode::OK,
            serde_json::to_string(&crate::scheduled::host(&config)).expect("host serializes"),
            allowed_origin,
        )),
        ("GET" | "POST", "/environment") => Ok(respond(
            StatusCode::OK,
            serde_json::to_string(&crate::exec::environment()).expect("environment serializes"),
            allowed_origin,
        )),
        ("GET", "/health") => Ok(respond(
            StatusCode::OK,
            r#"{"status":"ok"}"#,
            allowed_origin,
        )),
        ("POST", "/secret") if origin.is_none() && direct_loopback => Ok(respond(
            StatusCode::OK,
            serde_json::json!({ "secret": config.secret.as_ref() }).to_string(),
            allowed_origin,
        )),
        ("POST", "/secret") => Ok(execution_response(
            Err(BridgeError::Forbidden("forbidden".into())),
            allowed_origin,
        )),
        ("POST", "/models/discover") => {
            if let Err(error) =
                read_json::<serde_json::Value>(req.into_body(), config.limits.max_body).await
            {
                return Ok(execution_response(Err(error.into()), allowed_origin));
            }
            let transport = openwebide_core::ServerTransport {
                timeout_seconds: 2,
                ..Default::default()
            };
            let client =
                crate::runs::http_client::ReqwestHttpClient::default().with_transport(transport);
            let found = openwebide_llm::discovery::discover(client).await;
            Ok(respond(
                StatusCode::OK,
                serde_json::to_string(&found).expect("discovery serializes"),
                allowed_origin,
            ))
        }
        ("POST", "/exec") => Ok(execution_response(
            handle_exec(req, &config).await,
            allowed_origin,
        )),
        (m, p) if (m == "GET" || m == "POST") && p.starts_with("/git/") => Ok(execution_response(
            handle_git(req, &config).await,
            allowed_origin,
        )),
        _ => Ok(execution_response(
            Err(BridgeError::NotFound("not found".into())),
            allowed_origin,
        )),
    }
}

/// Map a `check_request` rejection to a response. These never carry CORS headers: the request
/// failed the Host/Origin baseline, so there is no origin to trust.
fn rejection_response(code: u16) -> Response<Full<Bytes>> {
    let error = match code {
        403 => BridgeError::Forbidden("forbidden".into()),
        415 => BridgeError::UnsupportedMediaType,
        _ => BridgeError::Validation("bad request".into()),
    };
    execution_response(Err(error), None)
}

fn preflight_response(headers: &HeaderMap, allowed_origin: Option<&str>) -> Response<Full<Bytes>> {
    let private_network = headers
        .get("access-control-request-private-network")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));

    let mut builder = Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("connection", "close")
        .header("access-control-allow-methods", "GET, POST, OPTIONS")
        .header(
            "access-control-allow-headers",
            "Content-Type, Authorization",
        )
        .header("access-control-max-age", "600");
    if let Some(origin) = allowed_origin {
        builder = builder
            .header("access-control-allow-origin", origin)
            .header("vary", "Origin");
    }
    if private_network {
        builder = builder.header("access-control-allow-private-network", "true");
    }
    builder
        .body(Full::new(Bytes::new()))
        .expect("static headers always produce a valid response")
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let is_upgrade = headers
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let has_connection_upgrade = headers
        .get(CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("upgrade"));
    is_upgrade && has_connection_upgrade
}

/// A `Sec-WebSocket-Key` is always the base64 encoding of a 16-byte nonce: 24 characters, the
/// last two of which are the `==` padding forced by 16 not being a multiple of 3.
fn is_valid_ws_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    bytes.len() == 24
        && bytes.ends_with(b"==")
        && bytes[..22]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

fn handle_ws_upgrade(
    req: Request<Incoming>,
    allowed_origin: Option<&str>,
    sessions: SessionManager,
    config: ServerConfig,
    permit_cell: PermitCell,
) -> Response<Full<Bytes>> {
    let version_ok = req
        .headers()
        .get("sec-websocket-version")
        .and_then(|v| v.to_str().ok())
        == Some("13");
    let key = req
        .headers()
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
        .filter(|k| version_ok && is_valid_ws_key(k))
        .map(str::to_string);

    let Some(key) = key else {
        return execution_response(
            Err(BridgeError::Validation("invalid websocket upgrade".into())),
            allowed_origin,
        );
    };
    let accept_key = derive_accept_key(key.as_bytes());

    tokio::spawn(
        async move {
            // Claim the connection's accept permit for the life of the WebSocket session, instead
            // of letting it release when the HTTP dispatch for this upgrade request completes: an
            // open WebSocket must keep counting against `max_connections`.
            let _permit = permit_cell
                .lock()
                .expect("permit cell mutex poisoned")
                .take();
            match hyper::upgrade::on(req).await {
                Ok(upgraded) => {
                    let ws_config = WebSocketConfig::default()
                        .max_message_size(Some(MAX_WS_MESSAGE))
                        .max_frame_size(Some(MAX_WS_MESSAGE));
                    let ws_stream = WebSocketStream::from_raw_socket(
                        TokioIo::new(upgraded),
                        Role::Server,
                        Some(ws_config),
                    )
                    .await;
                    handle_websocket(ws_stream, sessions, config).await;
                }
                Err(err) => tracing::warn!(error = %err, "websocket upgrade failed"),
            }
        }
        .instrument(tracing::Span::current()),
    );

    let mut builder = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("upgrade", "websocket")
        .header("connection", "upgrade")
        .header("sec-websocket-accept", accept_key);
    if let Some(origin) = allowed_origin {
        builder = builder
            .header("access-control-allow-origin", origin)
            .header("vary", "Origin");
    }
    builder
        .body(Full::new(Bytes::new()))
        .expect("static headers always produce a valid response")
}

fn execution_response(
    result: Result<String, BridgeError>,
    origin: Option<&str>,
) -> Response<Full<Bytes>> {
    match result {
        Ok(body) => respond(StatusCode::OK, body, origin),
        Err(error) => respond(
            error.status(),
            serde_json::json!({"error": error.to_string()}).to_string(),
            origin,
        ),
    }
}

async fn handle_exec(req: Request<Incoming>, config: &ServerConfig) -> Result<String, BridgeError> {
    let payload: ExecPayload = read_json(req.into_body(), config.limits.max_body).await?;
    let cwd = crate::paths::resolve_in_root(&config.workspace_root, payload.cwd.as_deref())
        .map_err(BridgeError::Validation)?;
    let output = config
        .execution
        .run_command(SpawnSpec::shell(
            payload.command,
            cwd,
            payload.timeout_seconds,
            std::future::pending(),
        ))
        .await?;
    serde_json::to_string(&output).map_err(|error| BridgeError::Execution(error.to_string()))
}

async fn handle_git(req: Request<Incoming>, config: &ServerConfig) -> Result<String, BridgeError> {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let is_get = method == "GET";
    let body = read_body(req.into_body(), config.limits.max_body).await?;
    let repo_dir = if is_get && body.is_empty() {
        None
    } else {
        serde_json::from_slice::<serde_json::Value>(&body)
            .map_err(|error| BridgeError::Validation(format!("invalid JSON: {error}")))?
            .get("cwd")
            .and_then(|value| value.as_str().filter(|s| !s.is_empty()).map(String::from))
    };
    let cwd = match repo_dir {
        Some(dir) => crate::paths::resolve_in_root(&config.workspace_root, Some(&dir))
            .map_err(BridgeError::Validation)?,
        None if is_get => config.workspace_root.clone(),
        None => return Err(BridgeError::Validation("cwd is missing or empty".into())),
    };
    #[derive(Deserialize, Default)]
    struct PathRequest {
        path: Option<String>,
    }
    let operation = match (method.as_str(), path.as_str()) {
        ("POST", "/git/history") => {
            GitOperation::History(serde_json::from_slice(&body).map_err(|error| {
                BridgeError::Validation(format!("invalid history payload: {error}"))
            })?)
        }
        ("POST", "/git/commit-diff") => {
            GitOperation::CommitDiff(serde_json::from_slice(&body).map_err(|error| {
                BridgeError::Validation(format!("invalid commit diff payload: {error}"))
            })?)
        }
        ("POST", "/git/stash") => {
            GitOperation::Stash(serde_json::from_slice(&body).map_err(|error| {
                BridgeError::Validation(format!("invalid stash request: {error}"))
            })?)
        }
        ("GET" | "POST", "/git/index-diff") => GitOperation::IndexDiff,
        ("GET" | "POST", "/git/status") => GitOperation::Status,
        ("GET" | "POST", "/git/path-status") => GitOperation::PathChanges,
        ("POST", "/git/path") => {
            GitOperation::PathAction(serde_json::from_slice(&body).map_err(|error| {
                BridgeError::Validation(format!("invalid Git path payload: {error}"))
            })?)
        }
        ("GET" | "POST", "/git/diff") => GitOperation::Diff(
            serde_json::from_slice::<PathRequest>(&body)
                .unwrap_or_default()
                .path,
        ),
        ("GET" | "POST", "/git/show") => GitOperation::Show(
            serde_json::from_slice::<PathRequest>(&body)
                .unwrap_or_default()
                .path
                .ok_or_else(|| BridgeError::Validation("missing path parameter".into()))?,
        ),
        ("GET" | "POST", "/git/branches") => GitOperation::Branches,
        ("POST", "/git/commit") => {
            GitOperation::Commit(serde_json::from_slice(&body).map_err(|error| {
                BridgeError::Validation(format!("invalid commit payload: {error}"))
            })?)
        }
        ("POST", "/git/checkout") => {
            GitOperation::Checkout(serde_json::from_slice(&body).map_err(|error| {
                BridgeError::Validation(format!("invalid checkout payload: {error}"))
            })?)
        }
        ("POST", "/git/sync") => {
            GitOperation::Sync(serde_json::from_slice(&body).map_err(|error| {
                BridgeError::Validation(format!("invalid sync payload: {error}"))
            })?)
        }
        _ => {
            return Err(BridgeError::Validation(format!(
                "unrecognized git route: {method} {path}"
            )));
        }
    };
    let output = config.execution.git(GitRequest { cwd, operation }).await?;
    serde_json::to_string(&output).map_err(|error| BridgeError::Execution(error.to_string()))
}
