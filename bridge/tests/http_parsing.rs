//! Integration tests for the bridge's hyper-based HTTP/1 server: body framing, header/body
//! limits, connection resilience, and the WebSocket upgrade.

mod common;

use std::time::{Duration, Instant};

use common::{HttpResponse, TestDir, http, post, start, ws_with_headers};
use futures::{SinkExt, StreamExt};
use openwebide_bridge::http::Limits;
use openwebide_bridge::{ServerConfig, run_server};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

async fn init_git_repo(path: &std::path::Path) {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["init", "-b", "main"]).current_dir(path);
    assert!(cmd.status().await.unwrap().success());

    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["config", "user.name", "Tester"])
        .current_dir(path);
    assert!(cmd.status().await.unwrap().success());

    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["config", "user.email", "test@example.com"])
        .current_dir(path);
    assert!(cmd.status().await.unwrap().success());
}

/// Start a bridge server with custom limits (rather than the defaults `common::start` uses).
async fn start_with_limits(workspace_root: std::path::PathBuf, limits: Limits) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind 127.0.0.1:0");
    let port = listener.local_addr().expect("local_addr failed").port();
    let mut config = ServerConfig::new(workspace_root, std::sync::Arc::from("dummy_secret"), None);
    config.limits = limits;
    tokio::spawn(async move {
        run_server(listener, config).await;
    });
    port
}

/// The status code of the last `HTTP/1.1 <code>` status line in a raw response, tolerating a
/// leading `100 Continue` interim response.
fn final_status(raw: &str) -> u16 {
    raw.rmatch_indices("HTTP/1.1 ")
        .next()
        .and_then(|(idx, _)| raw[idx + "HTTP/1.1 ".len()..].split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[tokio::test]
async fn exec_body_1_5kb_single_write() {
    let test_dir = TestDir::new();
    let port = start(test_dir.path.clone()).await;
    let marker = test_dir.path.join("marker_1_5kb");
    let padding = "x".repeat(1536);
    let payload = serde_json::json!({
        "command": format!("touch {}", marker.display()),
        "padding": padding,
    })
    .to_string();
    assert!(payload.len() > 1536);

    let resp = post(
        port,
        "/exec",
        &payload,
        &[("Content-Type", "application/json")],
    )
    .await;

    assert_eq!(resp.status, 200);
    assert!(marker.exists());
}

#[tokio::test]
async fn exec_headers_and_body_split_writes() {
    let test_dir = TestDir::new();
    let port = start(test_dir.path.clone()).await;
    let marker = test_dir.path.join("marker_split_writes");
    let payload = serde_json::json!({
        "command": format!("touch {}", marker.display()),
    })
    .to_string();

    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("failed to connect to bridge");

    let head = format!(
        "POST /exec HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer dummy_secret\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        payload.len()
    );
    stream
        .write_all(head.as_bytes())
        .await
        .expect("failed to write head");
    tokio::time::sleep(Duration::from_millis(50)).await;
    stream
        .write_all(payload.as_bytes())
        .await
        .expect("failed to write body");

    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw).await;
    let resp = HttpResponse::parse(&String::from_utf8_lossy(&raw));

    assert_eq!(resp.status, 200);
    assert!(marker.exists());
}

#[tokio::test]
async fn commit_message_1200_chars() {
    let test_dir = TestDir::new();
    init_git_repo(&test_dir.path).await;
    std::fs::write(test_dir.path.join("file.txt"), b"content").unwrap();
    let port = start(test_dir.path.clone()).await;

    let message = "a".repeat(1200);
    let payload = serde_json::json!({
        "cwd": ".",
        "message": message,
        "include_untracked": true,
    })
    .to_string();

    let resp = post(
        port,
        "/git/commit",
        &payload,
        &[("Content-Type", "application/json")],
    )
    .await;

    assert_eq!(resp.status, 200, "response body: {}", resp.body);
}

#[tokio::test]
async fn chunked_body_accepted() {
    let test_dir = TestDir::new();
    let port = start(test_dir.path.clone()).await;
    let marker = test_dir.path.join("marker_chunked");
    let payload = serde_json::json!({
        "command": format!("touch {}", marker.display()),
    })
    .to_string();

    let split = payload.len() / 2;
    let (first, second) = payload.split_at(split);
    let chunked_body = format!(
        "{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n0\r\n\r\n",
        first.len(),
        second.len()
    );

    let req = format!(
        "POST /exec HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer dummy_secret\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{chunked_body}"
    );
    let raw = http(port, &req).await;
    let resp = HttpResponse::parse(&raw);

    assert_eq!(resp.status, 200, "response body: {}", resp.body);
    assert!(marker.exists());
}

#[tokio::test]
async fn expect_100_continue() {
    let test_dir = TestDir::new();
    let port = start(test_dir.path.clone()).await;
    let marker = test_dir.path.join("marker_100_continue");
    let payload = serde_json::json!({
        "command": format!("touch {}", marker.display()),
    })
    .to_string();

    let req = format!(
        "POST /exec HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer dummy_secret\r\nContent-Type: application/json\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    let raw = http(port, &req).await;

    assert_eq!(final_status(&raw), 200, "raw response: {raw}");
    assert!(marker.exists());
}

#[tokio::test]
async fn head_over_16k_is_431_fast() {
    let test_dir = TestDir::new();
    let port = start(test_dir.path.clone()).await;

    let huge_header = "X-Large: ".to_string() + &"a".repeat(17_000);
    let req = format!(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer dummy_secret\r\n{huge_header}\r\n\r\n"
    );

    let started = Instant::now();
    let raw = http(port, &req).await;
    let elapsed = started.elapsed();

    let resp = HttpResponse::parse(&raw);
    assert_eq!(resp.status, 431);
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
}

#[tokio::test]
async fn body_over_1mib_is_413() {
    let test_dir = TestDir::new();
    let port = start(test_dir.path.clone()).await;
    let padding = "x".repeat(1024 * 1024 + 1);
    let payload = serde_json::json!({ "command": "true", "padding": padding }).to_string();

    let resp = post(
        port,
        "/exec",
        &payload,
        &[("Content-Type", "application/json")],
    )
    .await;

    assert_eq!(resp.status, 413);
}

#[tokio::test]
async fn idle_connection_closed_after_head_timeout() {
    let test_dir = TestDir::new();
    let limits = Limits {
        head_timeout: Duration::from_millis(200),
        ..Limits::default()
    };
    let port = start_with_limits(test_dir.path.clone(), limits).await;

    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("failed to connect to bridge");
    // Never send a request head; the connection should be closed once the head timeout fires.
    let started = Instant::now();
    let mut buf = [0u8; 16];
    let n = stream.read(&mut buf).await.unwrap_or(0);
    let elapsed = started.elapsed();

    assert_eq!(n, 0, "expected EOF once the idle connection was closed");
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
}

#[tokio::test]
async fn upgrade_header_case_insensitive() {
    let test_dir = TestDir::new();
    let port = start(test_dir.path.clone()).await;

    let req = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer dummy_secret\r\nUPGRADE: WebSocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    // A successful upgrade leaves the connection open for the WebSocket session, so read a
    // bounded response rather than waiting for EOF (unlike a plain HTTP request/response).
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("failed to connect to bridge");
    stream
        .write_all(req.as_bytes())
        .await
        .expect("failed to write request");
    let mut buf = [0u8; 1024];
    let n = stream
        .read(&mut buf)
        .await
        .expect("failed to read upgrade response");
    let raw = String::from_utf8_lossy(&buf[..n]);

    assert!(
        raw.starts_with("HTTP/1.1 101"),
        "expected 101 Switching Protocols, got: {raw}"
    );
}

#[tokio::test]
async fn upgrade_bad_key_is_400() {
    let test_dir = TestDir::new();
    let port = start(test_dir.path.clone()).await;

    let req = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer dummy_secret\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: short\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    let raw = http(port, &req).await;
    let resp = HttpResponse::parse(&raw);

    assert_eq!(resp.status, 400);
}

#[tokio::test]
async fn git_show_missing_path_is_400() {
    let test_dir = TestDir::new();
    init_git_repo(&test_dir.path).await;
    let port = start(test_dir.path.clone()).await;

    let resp = post(
        port,
        "/git/show",
        r#"{"cwd": "."}"#,
        &[("Content-Type", "application/json")],
    )
    .await;

    assert_eq!(resp.status, 400);
    assert!(resp.body.contains("missing path parameter"));
}

#[tokio::test]
async fn ws_oversized_message_closes_connection() {
    let test_dir = TestDir::new();
    let port = start(test_dir.path.clone()).await;

    let mut ws = ws_with_headers(port, &[("Origin", "http://localhost:3000")])
        .await
        .expect("ws connect failed");

    let oversized = "a".repeat(17 * 1024 * 1024);
    // Sending may itself fail once the server closes the connection after the oversized frame.
    let _ = ws.send(Message::Text(oversized.into())).await;
    let next = ws.next().await;
    assert!(
        !matches!(next, Some(Ok(Message::Text(_)))),
        "server should not echo an oversized message back"
    );

    // The server must still be serving other connections.
    let still_serving = ws_with_headers(port, &[("Origin", "http://localhost:3000")]).await;
    assert!(
        still_serving.is_ok(),
        "server stopped serving after oversized message"
    );
}

#[tokio::test]
async fn ws_connection_holds_accept_permit() {
    let test_dir = TestDir::new();
    let limits = Limits {
        max_connections: 1,
        ..Limits::default()
    };
    let port = start_with_limits(test_dir.path.clone(), limits).await;

    let ws = ws_with_headers(port, &[("Origin", "http://localhost:3000")])
        .await
        .expect("first ws connect should succeed");

    // The sole connection permit is held by the open WS; a second connection's request must
    // not be served until it is released, even though the TCP handshake itself succeeds
    // (it just sits in the OS accept backlog).
    let mut second = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("tcp connect should still succeed");
    second
        .write_all(format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer dummy_secret\r\n\r\n").as_bytes())
        .await
        .expect("failed to write request");

    let mut buf = [0u8; 16];
    let blocked = tokio::time::timeout(Duration::from_millis(300), second.read(&mut buf)).await;
    assert!(
        blocked.is_err(),
        "second connection should not be served while the WS holds the sole permit"
    );

    drop(ws);

    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), second.read_to_end(&mut raw))
        .await
        .expect("second connection should be served once the WS permit is released")
        .expect("read should succeed");
    let resp = HttpResponse::parse(&String::from_utf8_lossy(&raw));
    assert_eq!(resp.status, 200);
}

#[tokio::test]
async fn http10_keep_alive_closes_and_half_closed_clients_receive_response() {
    let dir = TestDir::new();
    let port = start(dir.path.clone()).await;
    let raw = tokio::time::timeout(
        Duration::from_secs(2),
        http(
            port,
            "GET /health HTTP/1.0\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n",
        ),
    )
    .await
    .unwrap();
    assert!(raw.contains("200 OK"));
    let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    client
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    client.shutdown().await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(2), client.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.contains("200 OK"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_half_closed_command_without_cancellation_opt_in_still_completes() {
    let dir = TestDir::new();
    let port = start(dir.path.clone()).await;
    let body = serde_json::json!({"command": "sleep .1; printf completed"}).to_string();
    let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    client.write_all(format!(
        "POST /exec HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer dummy_secret\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ).as_bytes()).await.unwrap();
    client.shutdown().await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(2), client.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    let response = HttpResponse::parse(&response);
    assert_eq!(response.status, 200, "{}", response.body);
    let outcome: openwebide_core::CommandOutcome = serde_json::from_str(&response.body).unwrap();
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.stdout, "completed");
}

#[tokio::test]
async fn websocket_rejects_oversized_message() {
    let dir = TestDir::new();
    let port = start(dir.path.clone()).await;
    let mut ws = ws_with_headers(port, &[("Authorization", "Bearer dummy_secret")])
        .await
        .unwrap();
    let _ = ws
        .send(Message::Text("x".repeat(16 * 1024 * 1024 + 1).into()))
        .await;
    let result = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .unwrap();
    assert!(matches!(
        result,
        None | Some(Err(_) | Ok(Message::Close(_)))
    ));
}

#[tokio::test]
async fn stalled_request_body_times_out_and_releases_connection() {
    let dir = TestDir::new();
    let port = start_with_limits(
        dir.path.clone(),
        Limits {
            max_connections: 1,
            ..Limits::default()
        },
    )
    .await;
    let mut stalled = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stalled.write_all(b"POST /exec HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer dummy_secret\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{").await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(
        Duration::from_secs(35),
        stalled.read_to_string(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(response.contains("400 Bad Request"));
    assert!(response.contains("timed out"));
    let next = tokio::time::timeout(
        Duration::from_secs(2),
        http(
            port,
            "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        ),
    )
    .await
    .unwrap();
    assert!(next.contains("200 OK"));
}
