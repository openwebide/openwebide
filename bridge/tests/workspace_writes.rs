//! The backend file adapter uses the ordinary native VFS, never a shell command.
mod common;
use common::{HttpResponse, http};
use openwebide_bridge::run_server;
use openwebide_core::{
    VfsError,
    vfs::{FileMutation, FileMutationRequest, VfsEntryKind},
};
use tokio::net::TcpListener;

async fn post(port: u16, headers: &str, request: &FileMutationRequest) -> HttpResponse {
    let body = serde_json::to_string(request).unwrap();
    HttpResponse::parse(&http(port, &format!("POST /workspace/write HTTP/1.1\r\nHost: localhost:{port}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())).await)
}

#[tokio::test]
async fn backend_mutations_require_service_authority_and_preserve_native_file_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("project")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = common::server_config(
        root.clone(),
        "service-secret".into(),
        Some("paired-token".into()),
    );
    let server = tokio::spawn(run_server(listener, config));
    let mut request = FileMutationRequest {
        root: "project".into(),
        mutation: FileMutation::Write {
            path: "file".into(),
            contents: vec![0, 255, 13, 10],
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
        assert!(!root.join("project/file").exists());
    }
    let service = "Authorization: Bearer service-secret\r\n";
    let response = post(port, service, &request).await;
    assert_eq!(response.status, 200, "{}", response.body);
    serde_json::from_str::<Result<(), VfsError>>(&response.body)
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read(root.join("project/file")).unwrap(),
        [0, 255, 13, 10]
    );
    for mutation in [
        FileMutation::Create {
            path: "dir".into(),
            kind: VfsEntryKind::Directory,
        },
        FileMutation::Create {
            path: "dir/empty".into(),
            kind: VfsEntryKind::File,
        },
        FileMutation::Copy {
            from: "file".into(),
            to: "dir/copy".into(),
        },
        FileMutation::Replace {
            path: "file".into(),
            contents: b"changed\r\n".to_vec(),
        },
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        FileMutation::Move {
            from: "file".into(),
            to: "moved".into(),
            kind: VfsEntryKind::File,
        },
        FileMutation::Remove {
            path: "dir/empty".into(),
            kind: VfsEntryKind::File,
        },
    ] {
        request.mutation = mutation;
        let response = post(port, service, &request).await;
        assert_eq!(response.status, 200, "{}", response.body);
        serde_json::from_str::<Result<(), VfsError>>(&response.body)
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        std::fs::read(root.join("project/dir/copy")).unwrap(),
        [0, 255, 13, 10]
    );
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(
            std::fs::read(root.join("project/moved")).unwrap(),
            b"changed\r\n"
        );
        assert!(!root.join("project/file").exists());
    } else {
        assert_eq!(
            std::fs::read(root.join("project/file")).unwrap(),
            b"changed\r\n"
        );
    }
    request.mutation = FileMutation::Write {
        path: "../escape".into(),
        contents: b"never".to_vec(),
    };
    let response = post(port, service, &request).await;
    assert!(matches!(
        serde_json::from_str::<Result<(), VfsError>>(&response.body).unwrap(),
        Err(VfsError::PathEscape(_))
    ));
    assert!(!root.join("escape").exists());
    request.mutation = FileMutation::Delete { path: "dir".into() };
    let response = post(port, service, &request).await;
    serde_json::from_str::<Result<(), VfsError>>(&response.body)
        .unwrap()
        .unwrap();
    assert!(!root.join("project/dir").exists());
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn a_project_root_alias_cannot_grant_access_outside_the_configured_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("alias")).unwrap();
    let port = common::start(workspace.path().canonicalize().unwrap()).await;
    let request = FileMutationRequest {
        root: "alias".into(),
        mutation: FileMutation::Write {
            path: "escaped".into(),
            contents: b"never".to_vec(),
        },
    };
    assert_eq!(
        post(port, "Authorization: Bearer dummy_secret\r\n", &request)
            .await
            .status,
        400
    );
    assert!(!outside.path().join("escaped").exists());
}
