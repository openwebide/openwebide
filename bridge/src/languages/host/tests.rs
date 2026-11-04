use super::super::{framing, rpc};
use super::*;
use tokio::io::BufReader;

fn key(owner: &str, project: i64) -> Key {
    Key {
        owner: owner.into(),
        project,
        root: PathBuf::from("project"),
        workspace: PathBuf::from("project"),
        provider: "provider".into(),
        digest: "a".repeat(64),
        server: "configuration".into(),
        client: "client".into(),
    }
}

#[tokio::test]
async fn capacity_never_evicts_active_requests_and_revocation_is_scoped() {
    let host = Host::default();
    let first = host.entry(&key("owner-a", 1)).unwrap();
    let second = host.entry(&key("owner-a", 2)).unwrap();
    *first.used.lock().unwrap() = Instant::now() - Duration::from_secs(2);
    *second.used.lock().unwrap() = Instant::now() - Duration::from_secs(1);
    first
        .active
        .lock()
        .unwrap()
        .insert("document".into(), (1, watch::channel(false).0));
    let third = host.entry(&key("owner-a", 3)).unwrap();
    assert!(!*first.stopped.borrow());
    assert!(*second.stopped.borrow());
    third
        .active
        .lock()
        .unwrap()
        .insert("document".into(), (1, watch::channel(false).0));
    assert!(host.entry(&key("owner-a", 4)).is_err());
    let other = host.entry(&key("owner-b", 1)).unwrap();
    host.revoke("owner-a", Some(1), Some("provider"));
    assert!(*first.stopped.borrow());
    assert!(!*third.stopped.borrow());
    assert!(!*other.stopped.borrow());
    host.shutdown();
    assert!(*third.stopped.borrow());
    assert!(*other.stopped.borrow());
}

#[tokio::test]
async fn protocol_retries_index_cancellation_but_never_caller_cancellation() {
    for code in [-32801, -32802, -32800] {
        let (local, remote) = tokio::io::duplex(4096);
        let (input, output) = tokio::io::split(local);
        let (mut reader, mut writer) = tokio::io::split(remote);
        let (client, _events) = rpc::Client::connect(output, BufReader::new(input));
        let calling = client.clone();
        let call = tokio::spawn(async move {
            request_current(&calling, "textDocument/hover", json!({})).await
        });
        let first = framing::read(&mut BufReader::new(&mut reader))
            .await
            .unwrap();
        framing::write(
            &mut writer,
            &json!({"jsonrpc":"2.0","id":first["id"],"error":{"code":code,"message":"cancelled"}}),
        )
        .await
        .unwrap();
        if code == -32800 {
            assert!(call.await.unwrap().unwrap_err().contains("-32800"));
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(100),
                    framing::read(&mut BufReader::new(&mut reader))
                )
                .await
                .is_err()
            );
        } else {
            let next = framing::read(&mut BufReader::new(&mut reader))
                .await
                .unwrap();
            assert_ne!(first["id"], next["id"]);
            framing::write(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":next["id"],"result":"current"}),
            )
            .await
            .unwrap();
            assert_eq!(call.await.unwrap().unwrap(), "current");
        }
    }
}

#[tokio::test]
async fn missing_toolchain_has_actionable_bounded_diagnostics_and_startup_backoff() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file.rs"), "fn main() {}").unwrap();
    let host = Host::default();
    let scope=Scope {owner:"owner".into(),project:1,root:root.path().into(),provider:"provider".into(),digest:"a".repeat(64),server:serde_json::from_value(json!({"command":"rustup","args":["run","openwebide-lsp-missing-toolchain","rust-analyzer"]})).unwrap()};
    let mut request = Request {
        version: protocol::VERSION,
        client: "client".into(),
        document: "file".into(),
        request: 1,
        revision: 1,
        language: "rust".into(),
        path: "file.rs".into(),
        source: "fn main() {}".into(),
        source_digest: openwebide_core::plugins::languages::source_digest("fn main() {}"),
        query: Query::Synchronize,
    };
    let error = host.query(scope.clone(), &request).await.unwrap_err();
    assert!(
        error.contains("Install the server and its toolchain"),
        "{error}"
    );
    assert!(error.contains("not installed"), "{error}");
    assert!(error.len() < 4096);
    request.request += 1;
    assert!(
        host.query(scope, &request)
            .await
            .unwrap_err()
            .contains("waiting to retry")
    );
    assert_eq!(
        host.inner
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .failure
            .lock()
            .unwrap()
            .count,
        1
    );
}

#[tokio::test]
async fn project_paths_and_workspace_markers_do_not_escape_the_authorized_root() {
    let root = tempfile::tempdir().unwrap();
    let root = tokio::fs::canonicalize(root.path()).await.unwrap();
    tokio::fs::create_dir_all(root.join("nested/src"))
        .await
        .unwrap();
    tokio::fs::write(root.join("nested/Cargo.toml"), "")
        .await
        .unwrap();
    let document = documents::source_path(&root, "nested/src/new.rs")
        .await
        .unwrap();
    assert_eq!(
        documents::workspace_root(&root, &document, &["Cargo.toml".into()]).await,
        root.join("nested")
    );
    tokio::fs::write(root.join("Cargo.toml"), "").await.unwrap();
    assert_eq!(
        documents::workspace_root(&root, &document, &["Cargo.toml".into()]).await,
        root
    );
    assert!(
        documents::source_path(&root, "../outside.rs")
            .await
            .is_err()
    );
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
        assert!(
            documents::source_path(&root, "escape/new.rs")
                .await
                .is_err()
        );
    }
}
