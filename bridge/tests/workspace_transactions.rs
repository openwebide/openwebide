mod common;
use common::{HttpResponse, http};
use openwebide_bridge::{ServerConfig, run_server};
use tokio::net::TcpListener;

#[tokio::test]
async fn workspace_dispatch_accepts_only_backend_service_authority() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("keep"), "unchanged").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = ServerConfig::new(
        directory.path().canonicalize().unwrap(),
        "dummy_secret".into(),
        Some("paired-token".into()),
    );
    let expected_root = openwebide_bridge::workspace_transactions::Host::root_identity(
        &config.workspace_root,
        "",
        42,
        1,
    )
    .unwrap();
    let server = tokio::spawn(run_server(listener, config));
    let token = openwebide_auth::sign_token_expires("dummy_secret", 42, 9999999999, 0);
    for path in ["/workspace/execute", "/workspace/root"] {
        for (headers, expected) in [
            (String::new(), 401),
            (
                format!("Origin: http://localhost:3000\r\nAuthorization: Bearer {token}\r\n"),
                401,
            ),
            (
                "Origin: http://localhost:3000\r\nAuthorization: Bearer paired-token\r\n".into(),
                401,
            ),
            ("Authorization: Bearer dummy_secret\r\n".into(), 400),
            (
                format!(
                    "Authorization: Bearer dummy_secret\r\n{}: true\r\n",
                    openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER
                ),
                403,
            ),
        ] {
            let raw = format!(
                "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\n{headers}Content-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
            let response = HttpResponse::parse(&http(port, &raw).await);
            assert_eq!(response.status, expected, "{}", response.body);
        }
    }
    for (path, expected) in [("", 200), ("../outside", 400), ("keep", 400)] {
        let body = serde_json::json!({"user":42,"project":1,"path":path}).to_string();
        let raw = format!(
            "POST /workspace/root HTTP/1.1\r\nHost: localhost:{port}\r\nAuthorization: Bearer dummy_secret\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let response = HttpResponse::parse(&http(port, &raw).await);
        assert_eq!(response.status, expected, "{}", response.body);
        if expected == 200 {
            let root: openwebide_core::workspace_transaction::WorkspaceRootIdentity =
                serde_json::from_str(&response.body).unwrap();
            assert_eq!(root.identity, expected_root);
        }
    }
    assert_eq!(
        std::fs::read_to_string(directory.path().join("keep")).unwrap(),
        "unchanged"
    );
    server.abort();
    let _ = server.await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let rotated = ServerConfig::new(
        directory.path().canonicalize().unwrap(),
        "rotated_secret".into(),
        None,
    );
    let server = tokio::spawn(run_server(listener, rotated));
    let body = serde_json::json!({"user":42,"project":1,"path":""}).to_string();
    for (secret, status) in [("dummy_secret", 401), ("rotated_secret", 200)] {
        let raw = format!(
            "POST /workspace/root HTTP/1.1\r\nHost: localhost:{port}\r\nAuthorization: Bearer {secret}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let response = HttpResponse::parse(&http(port, &raw).await);
        assert_eq!(response.status, status, "{}", response.body);
        if status == 200 {
            let root: openwebide_core::workspace_transaction::WorkspaceRootIdentity =
                serde_json::from_str(&response.body).unwrap();
            assert_eq!(root.identity, expected_root);
        }
    }
    server.abort();
    let _ = server.await;
}
