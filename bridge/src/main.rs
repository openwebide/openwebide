use std::net::SocketAddr;
use std::path::PathBuf;

use openwebide_bridge::{ServerConfig, run_server_until};
use tokio::net::TcpListener;

/// Resolves once the process receives Ctrl+C or, on Unix, SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(windows)]
    {
        let _ = ctrl_c.await;
    }
}

struct Args {
    host: String,
    port: u16,
    workspace: PathBuf,
    allowed_origins: Vec<String>,
    allowed_hosts: Vec<String>,
    secret_file: Option<PathBuf>,
    backend_url: String,
    pairing_token: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut host = std::env::var("OPENWEBIDE_BRIDGE_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let mut port: u16 = std::env::var("OPENWEBIDE_BRIDGE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3001);
    let mut workspace = std::env::var("OPENWEBIDE_BRIDGE_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut secret_file = None;
    let mut backend_url = std::env::var("OPENWEBIDE_BRIDGE_BACKEND_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:3000/api".into());

    let mut pairing_token = None;
    if let Ok(pt) = std::env::var("OPENWEBIDE_BRIDGE_TOKEN") {
        if pt.len() < 16 {
            return Err(
                "OPENWEBIDE_BRIDGE_TOKEN must be at least 16 characters if set".to_string(),
            );
        }
        pairing_token = Some(pt);
    }

    let mut allowed_origins = Vec::new();
    if let Ok(env_origins) = std::env::var("OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS") {
        for orig in env_origins.split(',') {
            let trimmed = orig.trim();
            if !trimmed.is_empty() {
                allowed_origins.push(trimmed.to_string());
            }
        }
    }

    let mut allowed_hosts = Vec::new();
    if let Ok(env_hosts) = std::env::var("OPENWEBIDE_BRIDGE_ALLOWED_HOSTS") {
        for h in env_hosts.split(',') {
            let trimmed = h.trim();
            if !trimmed.is_empty() {
                allowed_hosts.push(trimmed.to_string());
            }
        }
    }

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "OpenWebIDE Bridge Daemon\n\n\
                     Usage: openwebide-bridge [OPTIONS]\n\n\
                     Options:\n\
                       --host <HOST>             Host to bind on (default: 127.0.0.1, env: OPENWEBIDE_BRIDGE_HOST)\n\
                       -p, --port <PORT>         Port to bind on (default: 3001, env: OPENWEBIDE_BRIDGE_PORT)\n\
                       -w, --workspace <DIR>     Workspace root directory (default: current dir, env: OPENWEBIDE_BRIDGE_WORKSPACE)\n\
                       --allowed-origin <ORIGIN> Allowed CORS Origin (repeatable, env: OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS)\n\
                       --allowed-host <HOSTNAME> Allowed Host header (repeatable, env: OPENWEBIDE_BRIDGE_ALLOWED_HOSTS)\n\
                       --backend-url <URL>       Backend API URL (default: http://127.0.0.1:3000/api, env: OPENWEBIDE_BRIDGE_BACKEND_URL)\n\
                       --secret-file <PATH>      Path to secret file (default: $XDG_CONFIG_HOME/openwebide/bridge-secret)\n\
                       --token <TOKEN>           Pairing token (min 16 chars, env: OPENWEBIDE_BRIDGE_TOKEN)\n\
                       --help                    Show this help message\n"
                );
                std::process::exit(0);
            }
            "--host" => {
                host = args
                    .next()
                    .ok_or_else(|| "missing value for --host".to_string())?;
            }
            "-p" | "--port" => {
                let p_str = args
                    .next()
                    .ok_or_else(|| "missing value for --port".to_string())?;
                port = p_str
                    .parse()
                    .map_err(|e| format!("invalid port '{p_str}': {e}"))?;
            }
            "-w" | "--workspace" => {
                let w_str = args
                    .next()
                    .ok_or_else(|| "missing value for --workspace".to_string())?;
                workspace = PathBuf::from(w_str);
            }
            "--backend-url" => {
                backend_url = args
                    .next()
                    .ok_or_else(|| "missing value for --backend-url".to_string())?;
            }
            "--secret-file" => {
                let w_str = args
                    .next()
                    .ok_or_else(|| "missing value for --secret-file".to_string())?;
                secret_file = Some(PathBuf::from(w_str));
            }
            "--token" => {
                let t_str = args
                    .next()
                    .ok_or_else(|| "missing value for --token".to_string())?;
                if t_str.len() < 16 {
                    return Err("--token must be at least 16 characters if set".to_string());
                }
                pairing_token = Some(t_str);
            }
            "--allowed-origin" => {
                let orig = args
                    .next()
                    .ok_or_else(|| "missing value for --allowed-origin".to_string())?;
                allowed_origins.push(orig.trim().to_string());
            }
            "--allowed-host" => {
                let h = args
                    .next()
                    .ok_or_else(|| "missing value for --allowed-host".to_string())?;
                allowed_hosts.push(h.trim().to_string());
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
    }

    Ok(Args {
        host,
        port,
        workspace,
        allowed_origins,
        allowed_hosts,
        secret_file,
        backend_url,
        pairing_token,
    })
}

fn main() -> anyhow::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Some(result) = openwebide_bridge::exec::process_owner::run_if_requested() {
            return result.map_err(anyhow::Error::msg);
        }
        openwebide_bridge::exec::process_owner::register_executable()
            .map_err(anyhow::Error::msg)?;
    }
    run()
}

#[tokio::main]
async fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = match parse_args() {
        Ok(a) => a,
        Err(err) => {
            tracing::error!(error = %err);
            std::process::exit(1);
        }
    };

    let workspace_root = match openwebide_bridge::paths::canonical_root(&args.workspace) {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(error = %err);
            std::process::exit(1);
        }
    };

    let env_secret = std::env::var("OPENWEBIDE_BRIDGE_SECRET").ok();
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    #[allow(deprecated)] // home_dir is deprecated but standard practice here
    let home = std::env::home_dir();
    let (secret, secret_source) =
        match openwebide_bridge::secret::load_or_create(env_secret, args.secret_file, home, xdg) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e);
                std::process::exit(1);
            }
        };
    let secret: std::sync::Arc<str> = std::sync::Arc::from(secret.as_str());

    let mut config = ServerConfig::new(workspace_root, secret.clone(), args.pairing_token);
    config
        .set_language_memory_limits(
            openwebide_bridge::languages::host::MemoryLimits::from_environment()
                .map_err(anyhow::Error::msg)?,
        )
        .map_err(anyhow::Error::msg)?;
    config.backend_url = args.backend_url;
    for orig in args.allowed_origins {
        if !config
            .allowed_origins
            .iter()
            .any(|o| o.eq_ignore_ascii_case(&orig))
        {
            config.allowed_origins.push(orig);
        }
    }
    for h in args.allowed_hosts {
        if !config
            .allowed_hosts
            .iter()
            .any(|host| host.eq_ignore_ascii_case(&h))
        {
            config.allowed_hosts.push(h);
        }
    }

    let addr_str = format!("{}:{}", args.host, args.port);
    let addr: SocketAddr = addr_str
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid socket address {addr_str}: {e}"))?;

    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "bridge listening (WebSocket and HTTP /exec)");
    tracing::info!(workspace = %config.workspace_root.display(), origins = %config.allowed_origins.join(", "), hosts = %config.allowed_hosts.join(", "), "bridge configuration");
    match secret_source {
        openwebide_bridge::secret::SecretSource::Env => {
            tracing::info!("secret source: OPENWEBIDE_BRIDGE_SECRET environment variable");
        }
        openwebide_bridge::secret::SecretSource::File(p) => {
            tracing::info!(path = %p.display(), "secret source: file");
            if !addr.ip().is_loopback() {
                tracing::info!(path = %p.display(), "backends on other hosts need SPIN_VARIABLE_BRIDGE_SECRET set to this file's contents");
            }
        }
    }

    run_server_until(listener, config, shutdown_signal()).await;

    Ok(())
}
