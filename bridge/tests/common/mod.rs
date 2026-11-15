//! Common helpers for bridge integration tests.

#![allow(dead_code)] // Each integration-test binary uses a different subset of these shared helpers.

use std::collections::HashMap;
use std::path::PathBuf;

use openwebide_bridge::{ServerConfig, run_server};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderName, HeaderValue};

use std::sync::atomic::{AtomicU64, Ordering};

static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Temporary directory for testing that deletes itself when dropped.
pub struct TestDir {
    pub path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        let count = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let unique = format!(
            "openwebide-bridge-test-{}-{}-{}",
            std::process::id(),
            count,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).expect("failed to create test temp dir");
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Keep integration-test recovery discovery outside the real OS account home.
/// Servers in one test process share their disposable namespace so reconnects
/// can still observe the original owners.
pub fn server_config(
    workspace_root: PathBuf,
    secret: std::sync::Arc<str>,
    pairing_token: Option<String>,
) -> ServerConfig {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        static RECOVERY_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let home = RECOVERY_HOME.get_or_init(|| {
            tempfile::Builder::new()
                .prefix("openwebide-integration-recovery-")
                .tempdir()
                .expect("create isolated integration recovery home")
                .keep()
        });
        ServerConfig::new_for_test(workspace_root, secret, pairing_token, home.clone())
            .expect("configure isolated integration recovery home")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    ServerConfig::new(workspace_root, secret, pairing_token)
}

/// Start a bridge server on 127.0.0.1:0 and return the bound port.
pub async fn start(workspace_root: PathBuf) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind 127.0.0.1:0");
    let port = listener.local_addr().expect("local_addr failed").port();
    let config = server_config(workspace_root, std::sync::Arc::from("dummy_secret"), None);
    tokio::spawn(async move {
        run_server(listener, config).await;
    });
    port
}

/// Send raw HTTP request to 127.0.0.1:port and return the raw response string.
pub async fn http(port: u16, raw: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("failed to connect to bridge");
    stream
        .write_all(raw.as_bytes())
        .await
        .expect("failed to write raw request");
    let mut resp = Vec::new();
    let _ = stream.read_to_end(&mut resp).await;
    String::from_utf8_lossy(&resp).to_string()
}

/// Parsed HTTP response for integration tests.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl HttpResponse {
    pub fn parse(raw: &str) -> Self {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
        let mut lines = head.lines();
        let status_line = lines.next().unwrap_or("");
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);

        let mut headers = HashMap::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }

        Self {
            status,
            headers,
            body: body.to_string(),
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

/// Helper to send a POST request with extra headers to the bridge.
pub async fn post(
    port: u16,
    path: &str,
    body: &str,
    extra_headers: &[(&str, &str)],
) -> HttpResponse {
    let mut req = format!("POST {path} HTTP/1.1\r\n");
    let mut has_host = false;
    let mut has_cl = false;
    let mut has_origin = false;
    for (k, v) in extra_headers {
        if k.eq_ignore_ascii_case("host") {
            has_host = true;
        }
        if k.eq_ignore_ascii_case("content-length") {
            has_cl = true;
        }
        if k.eq_ignore_ascii_case("origin") {
            has_origin = true;
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_host {
        req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    if has_origin {
        let token = openwebide_auth::sign_token_expires("dummy_secret", 42, 9999999999, 0);
        req.push_str(&format!("Authorization: Bearer {token}\r\n"));
    } else {
        req.push_str("Authorization: Bearer dummy_secret\r\n");
    }
    if !has_cl {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);

    let raw = http(port, &req).await;
    HttpResponse::parse(&raw)
}

/// Connect via WebSocket with custom request headers.
pub async fn ws_with_headers(
    port: u16,
    headers: &[(&str, &str)],
) -> Result<WebSocketStream<TcpStream>, String> {
    let url = format!("ws://127.0.0.1:{port}/");
    let mut req = url.into_client_request().map_err(|e| e.to_string())?;
    for (name, val) in headers {
        if let (Ok(h_name), Ok(h_val)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(val),
        ) {
            req.headers_mut().insert(h_name, h_val);
        }
    }
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    let (mut ws, _) = tokio_tungstenite::client_async(req, stream)
        .await
        .map_err(|e| e.to_string())?;
    use futures::{SinkExt, StreamExt};
    let token = openwebide_auth::sign_token_expires("dummy_secret", 42, 9999999999, 0);
    let hello = format!(r#"{{"type":"hello","token":"{token}"}}"#);
    ws.send(tokio_tungstenite::tungstenite::Message::Text(hello.into()))
        .await
        .map_err(|e| e.to_string())?;
    let resp = ws.next().await.unwrap().unwrap();
    assert!(resp.to_text().unwrap().contains("hello_ok"));
    Ok(ws)
}
