#![cfg(any(target_os = "linux", target_os = "macos"))]
mod common;
use common::{HttpResponse, http};
use openwebide_bridge::{ServerConfig, run_server};
use openwebide_core::workspace_transaction::companion::{FolderReply, REQUEST_LIMIT};
use tokio::net::TcpListener;

async fn post(port: u16, headers: &str, body: &str) -> HttpResponse {
    let request = format!(
        "POST /workspace/files HTTP/1.1\r\nHost: localhost:{port}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    HttpResponse::parse(&http(port, &request).await)
}

#[tokio::test]
async fn physical_folder_capabilities_require_browser_account_authority_and_cannot_be_called_by_plugins()
 {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("source"), "unchanged").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = ServerConfig::new(
        directory.path().canonicalize().unwrap(),
        "service-secret".into(),
        Some("paired-token".into()),
    );
    let server = tokio::spawn(run_server(listener, config));
    let token = openwebide_auth::sign_token_expires("service-secret", 42, 9999999999, 0);
    let body = serde_json::json!({"user":42,"project":1,"action":{"operation":"open","path":"","expected":null}}).to_string();
    for (headers, status) in [
        (String::new(), 401),
        ("Authorization: Bearer service-secret\r\n".into(), 401),
        (
            "Origin: https://untrusted.example\r\nAuthorization: Bearer paired-token\r\n".into(),
            403,
        ),
        (
            format!(
                "Origin: http://localhost:3000\r\nAuthorization: Bearer {token}\r\n{}: true\r\n",
                openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER
            ),
            403,
        ),
    ] {
        let response = post(port, &headers, &body).await;
        assert_eq!(response.status, status, "{}", response.body);
    }
    let headers = format!("Origin: http://localhost:3000\r\nAuthorization: Bearer {token}\r\n");
    let wrong = body.replace("\"user\":42", "\"user\":43");
    assert_eq!(post(port, &headers, &wrong).await.status, 401);
    let response = post(port, &headers, &body).await;
    assert_eq!(response.status, 200, "{}", response.body);
    let FolderReply::Opened {
        session, challenge, ..
    } = serde_json::from_str(&response.body).unwrap()
    else {
        panic!()
    };
    let proof = std::fs::read_to_string(directory.path().join(&challenge)).unwrap();
    assert!(!response.body.contains(&proof));
    let proven = serde_json::json!({"user":42,"project":1,"action":{"operation":"prove","session":session,"proof":proof}}).to_string();
    assert_eq!(post(port, &headers, &proven).await.status, 200);
    assert!(!directory.path().join(challenge).exists());
    let read = serde_json::json!({"user":42,"project":1,"action":{"operation":"file","session":session,"sequence":1,"action":{"operation":"read","path":"source"}}}).to_string();
    let response = post(port, &headers, &read).await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(matches!(
        serde_json::from_str::<FolderReply>(&response.body).unwrap(),
        FolderReply::File { outcome: Ok(_) }
    ));
    let other_project = read.replace("\"project\":1", "\"project\":2");
    assert_eq!(post(port, &headers, &other_project).await.status, 400);
    let huge = format!(
        "{{\"user\":42,\"project\":1,\"action\":{{\"operation\":\"open\",\"path\":\"{}\",\"expected\":null}}}}",
        "x".repeat(REQUEST_LIMIT)
    );
    assert_eq!(post(port, &headers, &huge).await.status, 413);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("source")).unwrap(),
        "unchanged"
    );
    server.abort();
    let _ = server.await;
}
