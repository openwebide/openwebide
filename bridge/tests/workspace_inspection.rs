#![cfg(any(target_os = "linux", target_os = "macos"))]
mod common;
use common::{HttpResponse, http};
use openwebide_bridge::{ServerConfig, run_server, workspace_transactions::Host};
use openwebide_core::workspace_transaction::companion::{
    FileAction, FileValue, FolderInspection, NativeFolderInspection,
};
use tokio::net::TcpListener;

async fn post(port: u16, headers: &str, request: &NativeFolderInspection) -> HttpResponse {
    let body = serde_json::to_string(request).unwrap();
    HttpResponse::parse(&http(port,&format!("POST /workspace/inspect HTTP/1.1\r\nHost: localhost:{port}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len())).await)
}
#[tokio::test]
async fn remote_inspection_is_service_only_complete_read_only_and_bound_to_the_physical_root() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("project")).unwrap();
    std::fs::write(root.join("project/source"), "original").unwrap();
    let private = format!(".openwebide-proof-{}", "a".repeat(32));
    std::fs::write(root.join("project").join(&private), "private payload").unwrap();
    let identity = Host::root_identity(&root, "project", 42, 7).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = ServerConfig::new(
        root.clone(),
        "service-secret".into(),
        Some("paired-token".into()),
    );
    let server = tokio::spawn(run_server(listener, config));
    let mut request = NativeFolderInspection {
        user: 42,
        project: 7,
        path: "project".into(),
        inspection: FolderInspection {
            root: identity,
            action: FileAction::List {
                path: String::new(),
            },
        },
    };
    let token = openwebide_auth::sign_token_expires("service-secret", 42, 9999999999, 0);
    for (headers, status) in [
        (String::new(), 401),
        (
            format!("Origin: http://localhost:3000\r\nAuthorization: Bearer {token}\r\n"),
            401,
        ),
        (
            "Origin: http://localhost:3000\r\nAuthorization: Bearer paired-token\r\n".into(),
            401,
        ),
        (
            format!(
                "Authorization: Bearer service-secret\r\n{}: true\r\n",
                openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER
            ),
            403,
        ),
    ] {
        assert_eq!(post(port, &headers, &request).await.status, status);
    }
    let service = "Authorization: Bearer service-secret\r\n";
    let response = post(port, service, &request).await;
    assert_eq!(response.status, 200, "{}", response.body);
    let FileValue::Entries(entries) = serde_json::from_str(&response.body).unwrap() else {
        panic!()
    };
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry.name == private));
    assert!(!response.body.contains("private payload"));
    request.inspection.action = FileAction::Read {
        path: "source".into(),
    };
    let response = post(port, service, &request).await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        matches!(serde_json::from_str::<FileValue>(&response.body).unwrap(),FileValue::Bytes(bytes) if bytes==b"original")
    );
    request.inspection.action = FileAction::Read {
        path: private.clone(),
    };
    assert_eq!(post(port, service, &request).await.status, 400);
    request.inspection.action = FileAction::Create {
        ordinal: 0,
        path: "created".into(),
        is_dir: false,
    };
    assert_eq!(post(port, service, &request).await.status, 400);
    assert!(!root.join("project/created").exists());
    assert!(!root.join("project/.openwebide").exists());
    std::fs::rename(root.join("project"), root.join("kept")).unwrap();
    std::fs::create_dir(root.join("project")).unwrap();
    std::fs::write(root.join("project/source"), "replacement").unwrap();
    request.inspection.action = FileAction::Read {
        path: "source".into(),
    };
    assert_eq!(post(port, service, &request).await.status, 400);
    assert_eq!(
        std::fs::read_to_string(root.join("kept").join(private)).unwrap(),
        "private payload"
    );
    server.abort();
}
