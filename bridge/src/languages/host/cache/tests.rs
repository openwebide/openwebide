use super::*;
use futures::FutureExt;
use openwebide_core::plugins::languages::{intelligence::Position, source_digest};

fn allocate(root: &Path, project: &Path, key: &str) -> Result<Lease, String> {
    super::allocate(root, project, key, DEFAULT_RETAINED_BYTES)
}

#[tokio::test]
async fn confirmed_retirement_reclaims_oversized_data_after_the_last_native_lease() {
    let storage = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let lease = super::allocate(
        storage.path(),
        project.path(),
        &content_digest(b"retired"),
        10,
    )
    .unwrap();
    let data = lease.data.clone();
    fs::write(data.join("large"), [b'x'; 20]).unwrap();
    let (result, completion) = crate::exec::proc::cleanup_channel();
    drop(lease.activate(completion));
    assert!(data.exists());
    result.send_replace(Some(Ok(())));
    tokio::time::timeout(Duration::from_secs(5), async {
        while data.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn byte_retention_reclaims_idle_owned_data_and_preserves_live_unknown_and_linked_data() {
    let storage = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let active = super::allocate(
        storage.path(),
        project.path(),
        &content_digest(b"active"),
        10,
    )
    .unwrap();
    fs::write(active.data.join("cache"), b"123456").unwrap();
    let idle = allocate(storage.path(), project.path(), &content_digest(b"idle")).unwrap();
    let removed = idle.data.clone();
    fs::write(idle.data.join("cache"), b"123456").unwrap();
    drop(idle);
    let unknown = storage.path().join("v1/unknown");
    fs::create_dir(&unknown).unwrap();
    fs::write(unknown.join("keep"), [b'x'; 20]).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(project.path(), active.data.join("linked-project")).unwrap();
    fs::write(project.path().join("keep"), [b'x'; 20]).unwrap();
    drop(super::allocate(storage.path(), project.path(), &content_digest(b"next"), 10).unwrap());
    assert!(!removed.exists());
    assert!(active.data.join("cache").exists());
    assert!(unknown.join("keep").exists());
    assert!(project.path().join("keep").exists());
    fs::write(active.data.join("cache"), [b'x'; 20]).unwrap();
    assert!(
        super::allocate(
            storage.path(),
            project.path(),
            &content_digest(b"blocked"),
            10
        )
        .is_err()
    );
    let root = active
        .data
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    drop(active);
    maintain(&root, 10).unwrap();
    assert!(unknown.join("keep").exists());
    assert!(project.path().join("keep").exists());
    assert!(
        inventory(&root)
            .unwrap()
            .iter()
            .all(|entry| entry.bytes <= 10)
    );
}

fn scope(root: &Path) -> Scope {
    let script = root.join("peer.py");
    std::fs::write(
        &script,
        include_str!("../../../../../tools/fixtures/cache_server.py"),
    )
    .unwrap();
    Scope {owner:"owner".into(),project:1,root:root.canonicalize().unwrap(),provider:"community/cache".into(),digest:"a".repeat(64),server:serde_json::from_value(json!({"command":"python3","args":[script],"cache":{"versionArgs":[script,"--version"],"environment":{"FIXTURE_CACHE":"${cache}/build"}},"initializationOptions":{"directory":"${cache}/options"},"settings":{"directory":"${cache}/settings"}})).unwrap()}
}
fn host(storage: &Path) -> Host {
    let mut host = Host::default();
    host.set_cache_directory(storage.into()).unwrap();
    host
}
async fn query(host: &Host, scope: Scope) -> Value {
    let input = Request {
        version: protocol::VERSION,
        client: "client".into(),
        document: "document".into(),
        request: 1,
        revision: 1,
        language: "notes".into(),
        path: "main.notes".into(),
        source: "dirty source".into(),
        source_digest: source_digest("dirty source"),
        query: Query::Hover {
            position: Position::default(),
        },
    };
    let response = host.query(scope, &input).await.unwrap();
    response.validate(&input).unwrap();
    serde_json::from_str(response.result["contents"].as_str().unwrap()).unwrap()
}
async fn stop(host: Host) {
    let markers: Vec<_> = host
        .inner
        .entries
        .lock()
        .unwrap()
        .values()
        .filter_map(|entry| {
            entry
                .session
                .get()?
                .cache
                .as_ref()?
                .lease
                .lock()
                .unwrap()
                .as_ref()
                .map(|lease| lease.marker.clone())
        })
        .collect();
    host.shutdown();
    drop(host);
    // Wait for this process's cache lease specifically, rather than consuming
    // cleanup handles belonging to another concurrently running test runtime.
    tokio::time::timeout(Duration::from_secs(5), async {
        while markers.iter().any(|marker| marker.exists()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn public_cache_bindings_persist_derived_data_across_host_restarts_without_writing_the_project()
 {
    let root = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.notes"), "disk source").unwrap();
    let scope = scope(root.path());
    let first_host = host(storage.path());
    let first = query(&first_host, scope.clone()).await;
    assert_eq!(first["hit"], false);
    assert_eq!(first["sources"], json!(["dirty source"]));
    let cache = PathBuf::from(first["cache"].as_str().unwrap());
    assert!(cache.starts_with(storage.path().canonicalize().unwrap()));
    assert!(!cache.starts_with(&scope.root));
    assert_eq!(
        first["options"]["directory"],
        cache.parent().unwrap().join("options").to_str().unwrap()
    );
    assert_eq!(
        first["settings"]["directory"],
        cache.parent().unwrap().join("settings").to_str().unwrap()
    );
    stop(first_host).await;
    let next_host = host(storage.path());
    let next = query(&next_host, scope).await;
    assert_eq!(next["hit"], true);
    assert_eq!(next["cache"], first["cache"]);
    assert_eq!(
        std::fs::read_to_string(root.path().join("main.notes")).unwrap(),
        "disk source"
    );
    assert!(!root.path().join("build").exists());
    stop(next_host).await;
}

#[tokio::test]
async fn cache_namespaces_change_with_authority_provider_configuration_inputs_and_actual_server_version()
 {
    let root = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("settings.json"), "original").unwrap();
    let mut original = scope(root.path());
    original.server.project_inputs =
        Some(serde_json::from_value(json!({"files":["settings.json"]})).unwrap());
    let first_host = host(storage.path());
    let first = query(&first_host, original.clone()).await;
    stop(first_host).await;
    for boundary in [
        "owner",
        "project",
        "provider",
        "digest",
        "configuration",
        "inputs",
        "version",
    ] {
        std::fs::write(root.path().join("settings.json"), "original").unwrap();
        if root.path().join("server.version").exists() {
            std::fs::remove_file(root.path().join("server.version")).unwrap();
        }
        let mut scope = original.clone();
        match boundary {
            "owner" => scope.owner = "other".into(),
            "project" => scope.project += 1,
            "provider" => scope.provider = "community/other".into(),
            "digest" => scope.digest = "b".repeat(64),
            "configuration" => scope.server.initialization_options["mode"] = json!("other"),
            "inputs" => {
                std::fs::write(root.path().join("settings.json"), "changed").unwrap();
                scope.server.project_inputs =
                    Some(serde_json::from_value(json!({"files":["settings.json"]})).unwrap());
            }
            "version" => {
                std::fs::write(root.path().join("server.version"), "cache-peer-v2").unwrap();
            }
            _ => unreachable!(),
        }
        let host = host(storage.path());
        let next = query(&host, scope).await;
        assert_ne!(next["cache"], first["cache"], "{boundary}");
        assert_eq!(next["hit"], false, "{boundary}");
        stop(host).await;
    }
}

#[tokio::test]
async fn cache_leases_preserve_live_data_and_isolate_unknown_owners_without_stealing() {
    let storage = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let key = content_digest(b"scope");
    let lease = allocate(storage.path(), project.path(), &key).unwrap();
    let original = lease.data.clone();
    let marker = lease.marker.clone();
    std::fs::write(original.join("derived"), "retained").unwrap();
    let concurrent = allocate(storage.path(), project.path(), &key).unwrap();
    assert_eq!(concurrent.data, original);
    drop(concurrent);
    let (result, completion) = crate::exec::proc::cleanup_channel();
    let active = lease.activate(completion);
    active.check().unwrap();
    drop(active);
    let released = async {
        loop {
            let descriptor = file(&marker, false).unwrap();
            if descriptor.try_lock().is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let mut released = Box::pin(released);
    assert!(released.as_mut().now_or_never().is_none());
    result.send_replace(Some(Err("cleanup unconfirmed".into())));
    tokio::time::timeout(Duration::from_secs(5), released)
        .await
        .unwrap();
    let replacement = allocate(storage.path(), project.path(), &key).unwrap();
    assert_ne!(replacement.data, original);
    assert_eq!(
        std::fs::read_to_string(original.join("derived")).unwrap(),
        "retained"
    );
}

#[tokio::test]
async fn confirmed_process_exit_releases_cache_for_reuse_and_outside_projects_is_required() {
    let storage = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let key = content_digest(b"scope");
    assert!(allocate(&project.path().join("cache"), project.path(), &key).is_err());
    assert!(!project.path().join("cache").exists());
    assert!(
        allocate(
            &project.path().join("missing/../cache"),
            project.path(),
            &key
        )
        .is_err()
    );
    assert!(!project.path().join("missing").exists());
    let lease = allocate(storage.path(), project.path(), &key).unwrap();
    let original = lease.data.clone();
    let (result, completion) = crate::exec::proc::cleanup_channel();
    let marker = lease.marker.clone();
    drop(lease.activate(completion));
    result.send_replace(Some(Ok(())));
    tokio::time::timeout(Duration::from_secs(5), async {
        while marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let replacement = allocate(storage.path(), project.path(), &key).unwrap();
    assert_eq!(replacement.data, original);
}

#[test]
fn unrelated_unicode_names_and_replaced_markers_are_never_removed() {
    let storage = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let lease = allocate(storage.path(), project.path(), &content_digest(b"active")).unwrap();
    let unrelated =
        storage
            .path()
            .join("v1")
            .join(format!("{}é{}", "a".repeat(63), "b".repeat(32)));
    std::fs::create_dir(&unrelated).unwrap();
    std::fs::write(unrelated.join("keep"), "user data").unwrap();
    drop(allocate(storage.path(), project.path(), &content_digest(b"other")).unwrap());
    assert!(unrelated.join("keep").exists());
    let marker = lease.marker.clone();
    let moved = marker.with_extension("old");
    std::fs::rename(&marker, &moved).unwrap();
    std::fs::write(&marker, "replacement").unwrap();
    drop(lease);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "replacement");
}

#[test]
fn namespace_capacity_reclaims_only_inactive_owned_directories() {
    let storage = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let active = allocate(storage.path(), project.path(), &content_digest(b"active")).unwrap();
    std::fs::create_dir(storage.path().join("v1/unrelated")).unwrap();
    std::fs::write(storage.path().join("v1/unrelated/data"), "keep").unwrap();
    for i in 0..MAX_NAMESPACES + 4 {
        drop(
            allocate(
                storage.path(),
                project.path(),
                &content_digest(format!("scope-{i}").as_bytes()),
            )
            .unwrap(),
        );
    }
    assert!(active.data.exists());
    assert_eq!(
        std::fs::read_to_string(storage.path().join("v1/unrelated/data")).unwrap(),
        "keep"
    );
    assert_eq!(
        std::fs::read_dir(storage.path().join("v1"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().unwrap().is_dir() && entry.file_name() != "unrelated")
            .count(),
        MAX_NAMESPACES
    );
}
