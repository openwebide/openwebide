//! WebSocket and HTTP server for the process execution bridge daemon.
//!
//! HTTP/1 parsing and connection lifecycle are handled by `hyper` (see `crate::http`); this
//! module owns request routing, the Host/Origin/CORS security baseline, the WebSocket upgrade,
//! and the session-multiplexed WebSocket protocol.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::terminals::session::SessionManager;
use http::{Limits, builder};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub mod http;
mod routes;
mod ws;
pub(crate) use ws::WriterCmd;

/// Bridge server configuration.
#[derive(Clone)]
pub struct ServerConfig {
    pub workspace_root: PathBuf,
    pub execution: Arc<dyn crate::exec::ToolExecution>,
    pub plugins: crate::plugins::NativePluginInstaller,
    pub plugin_invocations: crate::plugins::invocations::Invocations,
    pub allowed_origins: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub limits: Limits,
    /// How long an exited session is kept around (for output replay/inspection) before the
    /// reaper removes it. Running sessions are never reaped.
    pub session_ttl: Duration,
    pub secret: Arc<str>,
    pub pairing_token: Option<String>,
    pub backend_url: String,
    pub tool_stream_memos: Arc<openwebide_llm::ToolStreamMemos>,
    pub runs: Arc<crate::runs::RunRegistry>,
}

impl ServerConfig {
    pub fn new(workspace_root: PathBuf, secret: Arc<str>, pairing_token: Option<String>) -> Self {
        Self {
            workspace_root,
            execution: Arc::new(crate::exec::HostExecution),
            plugins: crate::plugins::NativePluginInstaller::new(crate::plugins::default_root()),
            plugin_invocations: Default::default(),
            allowed_origins: default_origins(),
            allowed_hosts: default_hosts(),
            limits: Limits::default(),
            session_ttl: DEFAULT_SESSION_TTL,
            secret,
            pairing_token,
            backend_url: "http://127.0.0.1:3000/api".into(),
            tool_stream_memos: Arc::new(openwebide_llm::ToolStreamMemos::default()),
            runs: Arc::new(crate::runs::RunRegistry::default()),
        }
    }
}

const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(30 * 60);
const SESSION_REAP_INTERVAL: Duration = Duration::from_secs(60);

pub fn default_origins() -> Vec<String> {
    vec![
        "http://localhost:3000".to_string(),
        "http://127.0.0.1:3000".to_string(),
        "http://localhost:8080".to_string(),
        "http://127.0.0.1:8080".to_string(),
    ]
}

pub fn default_hosts() -> Vec<String> {
    let mut hosts = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "[::1]".to_string(),
    ];
    if let Some(h) = get_system_hostname() {
        let local_h = format!("{h}.local");
        hosts.push(h);
        hosts.push(local_h);
    }
    hosts
}

pub fn get_system_hostname() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: libc::gethostname is standard POSIX and buf is a valid buffer
        let res = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if res == 0 {
            let nul_pos = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let s = std::str::from_utf8(&buf[..nul_pos]).ok()?;
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_ascii_lowercase());
            }
        }
    }
    None
}

fn extract_host_hostname(host: &str) -> &str {
    let trimmed = host.trim();
    if trimmed.starts_with('[')
        && let Some(close) = trimmed.find(']')
    {
        return &trimmed[..=close];
    }
    if let Some((h, port)) = trimmed.rsplit_once(':')
        && port.chars().all(|c| c.is_ascii_digit())
    {
        return h;
    }
    trimmed
}

fn extract_origin_hostname(origin: &str) -> Option<&str> {
    let rest = if let Some(r) = origin.strip_prefix("http://") {
        r
    } else if let Some(r) = origin.strip_prefix("https://") {
        r
    } else {
        let idx = origin.find("://")?;
        &origin[idx + 3..]
    };
    let host_and_port = rest.split('/').next().unwrap_or(rest);
    Some(extract_host_hostname(host_and_port))
}

fn is_ip_literal(h: &str) -> bool {
    let unbracketed = h
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(h);
    unbracketed.parse::<std::net::IpAddr>().is_ok()
}

fn is_host_allowed(host_raw: &str, cfg: &ServerConfig) -> bool {
    let host_norm = extract_host_hostname(host_raw).to_ascii_lowercase();
    if is_ip_literal(&host_norm) {
        return true;
    }
    for allowed in &cfg.allowed_hosts {
        let allowed_norm = extract_host_hostname(allowed).to_ascii_lowercase();
        if host_norm == allowed_norm {
            return true;
        }
    }
    false
}

/// Pure validation of Host, Origin, and Content-Type baseline.
/// Returns Ok(Some(origin_to_echo)) or Ok(None) if allowed, or Err(status_code) if rejected.
pub fn check_request(
    host: Option<&str>,
    origin: Option<&str>,
    method: &str,
    content_type: Option<&str>,
    cfg: &ServerConfig,
) -> Result<Option<String>, u16> {
    // 1. Host hostname (port ignored) must pass the Host rule; else 403.
    let host_str = match host {
        Some(h) if !h.trim().is_empty() => h.trim(),
        _ => return Err(403),
    };
    if !is_host_allowed(host_str, cfg) {
        return Err(403);
    }

    // 2. Origin present: must pass Origin rule; else 403. Origin: null -> 403.
    let echo_origin = if let Some(orig_raw) = origin {
        let orig = orig_raw.trim();
        if orig.eq_ignore_ascii_case("null") || orig.is_empty() {
            return Err(403);
        }

        let is_listed = cfg.allowed_origins.iter().any(|allowed| {
            allowed
                .trim_end_matches('/')
                .eq_ignore_ascii_case(orig.trim_end_matches('/'))
        });

        let same_host = if let Some(orig_host) = extract_origin_hostname(orig) {
            let host_host = extract_host_hostname(host_str);
            orig_host.eq_ignore_ascii_case(host_host)
        } else {
            false
        };

        if !is_listed && !same_host {
            return Err(403);
        }

        Some(orig.to_string())
    } else {
        None
    };

    // 3. POST with Origin and media type != application/json -> 415.
    if method.eq_ignore_ascii_case("POST") && echo_origin.is_some() {
        let ct = content_type.unwrap_or("");
        let media_type = ct.split(';').next().unwrap_or("").trim();
        if !media_type.eq_ignore_ascii_case("application/json") {
            return Err(415);
        }
    }

    // 4. Origin absent -> allowed.
    Ok(echo_origin)
}

/// Run the bridge server on the specified TCP listener until the process is killed.
pub async fn run_server(listener: TcpListener, config: ServerConfig) {
    run_server_until(listener, config, std::future::pending()).await;
}

/// Run the bridge server until `shutdown` resolves, then terminate every session's process
/// group (with a grace period) before returning.
pub async fn run_server_until(
    listener: TcpListener,
    config: ServerConfig,
    shutdown: impl std::future::Future<Output = ()>,
) {
    config
        .runs
        .configure_host_administration(config.pairing_token.is_none());
    let session_manager = SessionManager::new();

    let reap_sessions = session_manager.clone();
    let reap_runs = config.runs.clone();
    let session_ttl = config.session_ttl;
    let reaper = tokio::spawn(async move {
        let mut interval = tokio::time::interval(SESSION_REAP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            reap_sessions.reap(session_ttl);
            reap_sessions.reap_detached(Duration::from_secs(15 * 60));
            reap_runs.reap();
        }
    });

    let push_backend = crate::runs::backend_client::BackendClient::new(
        config.backend_url.clone(),
        config.secret.clone(),
        crate::runs::http_client::ReqwestHttpClient::default(),
    );
    let push_dispatcher = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            // The persisted queue handles retry policy; the bridge only supplies a periodic wakeup.
            let _ = push_backend.dispatch_push().await;
        }
    });
    let scheduled = tokio::spawn(crate::scheduled::serve(config.clone()));
    let plugin_jobs = tokio::spawn(crate::plugins::jobs::serve(config.clone()));
    let plugin_runs = tokio::spawn(crate::plugins::runs::serve(config.clone()));
    let host_operations = tokio::spawn(crate::host_admin::serve(config.clone()));
    let runs = config.runs.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let accept = tokio::spawn(run_accept_loop(
        listener,
        config,
        session_manager.clone(),
        stopped,
    ));

    shutdown.await;

    reaper.abort();
    push_dispatcher.abort();
    scheduled.abort();
    plugin_jobs.abort();
    plugin_runs.abort();
    host_operations.abort();
    let _ = stop.send(());
    let _ = accept.await;
    runs.shutdown().await;
    session_manager.kill_all().await;
}

/// Anything that can accept incoming connections like a `TcpListener`. Exists so the accept
/// loop's error/backoff handling can be unit-tested against a fake that fails on demand.
trait Accept {
    fn accept(
        &self,
    ) -> impl std::future::Future<Output = std::io::Result<(TcpStream, std::net::SocketAddr)>> + Send;
}

impl Accept for TcpListener {
    async fn accept(&self) -> std::io::Result<(TcpStream, std::net::SocketAddr)> {
        TcpListener::accept(self).await
    }
}

async fn run_accept_loop<A: Accept>(
    acceptor: A,
    config: ServerConfig,
    session_manager: SessionManager,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) {
    let mut connections = tokio::task::JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(config.limits.max_connections));
    let mut backoff = Duration::from_millis(10);
    const MAX_BACKOFF: Duration = Duration::from_secs(1);

    loop {
        while connections.try_join_next().is_some() {}
        let (permit, accepted) = tokio::select! {
            _ = &mut stop => break,
            result = async {
                let permit = semaphore.clone().acquire_owned().await.expect("semaphore is never closed");
                (permit, acceptor.accept().await)
            } => result,
        };
        match accepted {
            Ok((stream, addr)) => {
                backoff = Duration::from_millis(10);
                let mgr = session_manager.clone();
                let cfg = config.clone();
                connections.spawn(async move {
                    handle_connection(stream, addr, mgr, cfg, permit).await;
                });
            }
            Err(err) => {
                tracing::warn!(error = %err, "accept failed");
                drop(permit);
                tokio::select! { () = tokio::time::sleep(backoff) => {}, _ = &mut stop => break }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
    connections.shutdown().await;
}

/// Holds the accept-loop's connection permit until either this HTTP exchange finishes or, for
/// an upgraded connection, the WebSocket session it hands off to finishes. `take()` lets the
/// WebSocket branch claim the permit for its own (much longer) lifetime instead of releasing it
/// when the HTTP dispatch for the upgrade request completes.
type PermitCell = Arc<Mutex<Option<OwnedSemaphorePermit>>>;

#[tracing::instrument(skip_all, fields(peer = %addr))]
async fn handle_connection(
    stream: TcpStream,
    addr: std::net::SocketAddr,
    sessions: SessionManager,
    config: ServerConfig,
    permit: OwnedSemaphorePermit,
) {
    let io = TokioIo::new(http::WriteTimeout::new(stream));
    let limits = config.limits;
    let permit_cell: PermitCell = Arc::new(Mutex::new(Some(permit)));
    let close = Arc::new(tokio::sync::Notify::new());
    let response_close = close.clone();
    let service = service_fn(move |req| {
        let sessions = sessions.clone();
        let config = config.clone();
        let permit_cell = permit_cell.clone();
        let close = response_close.clone();
        async move {
            let response = routes::route(req, addr, sessions, config, permit_cell).await;
            if response
                .as_ref()
                .is_ok_and(|response| response.status() != hyper::StatusCode::SWITCHING_PROTOCOLS)
            {
                close.notify_one();
            }
            response
        }
    });
    let connection = builder(&limits)
        .serve_connection(io, service)
        .with_upgrades();
    tokio::pin!(connection);
    tokio::select! {
        _ = &mut connection => {},
        () = close.notified() => {
            connection.as_mut().graceful_shutdown();
            let _ = connection.await;
        }
    }
}
