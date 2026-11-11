use super::*;
use futures::FutureExt;
use openwebide_core::plugins::languages::{intelligence::Position, source_digest};

pub(in crate::languages::host) struct Fixture {
    pub(in crate::languages::host) root: tempfile::TempDir,
    pub(in crate::languages::host) host: Host,
    pub(in crate::languages::host) scope: Scope,
}
impl Fixture {
    pub(in crate::languages::host) fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for name in ["a.notes", "b.notes", "c.notes"] {
            std::fs::write(root.path().join(name), format!("disk:{name}")).unwrap();
        }
        let script = root.path().join("peer.py");
        std::fs::write(
            &script,
            include_str!("../../../../../tools/fixtures/reuse_server.py"),
        )
        .unwrap();
        let python = if cfg!(windows) { "python" } else { "python3" };
        let scope = Scope {
            owner: "owner".into(),
            project: 1,
            root: root.path().canonicalize().unwrap(),
            provider: "community/notes".into(),
            digest: "a".repeat(64),
            server: serde_json::from_value(json!({"command":python,"args":[script]})).unwrap(),
        };
        Self {
            root,
            host: Host::default(),
            scope,
        }
    }
    pub(in crate::languages::host) async fn query(&self, input: &Request) -> (Response, Value) {
        let response = self.host.query(self.scope.clone(), input).await.unwrap();
        response.validate(input).unwrap();
        let value = serde_json::from_str(response.result["contents"].as_str().unwrap()).unwrap();
        (response, value)
    }
    pub(in crate::languages::host) fn starts(&self) -> usize {
        std::fs::read_to_string(self.root.path().join("calls"))
            .unwrap()
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "initialize")
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.shutdown();
    }
}
pub(in crate::languages::host) fn input(
    client: &str,
    path: &str,
    source: &str,
    request: u64,
) -> Request {
    Request {
        version: protocol::VERSION,
        client: client.into(),
        document: source_digest(path),
        request,
        revision: request,
        language: "notes".into(),
        path: path.into(),
        source: source.into(),
        source_digest: source_digest(source),
        query: Query::Hover {
            position: Position::default(),
        },
    }
}

#[tokio::test]
async fn process_memory_eviction_and_backoff_preserve_both_clients_complete_dirty_sources() {
    let mut fixture = Fixture::new();
    fixture.host = Host::with_memory_limits(MemoryLimits {
        service_bytes: 32 * 1024 * 1024,
        total_bytes: 64 * 1024 * 1024,
        reserve_bytes: 0,
    })
    .unwrap();
    std::fs::write(fixture.root.path().join("allocate-memory"), "").unwrap();
    let (_, first) = fixture.query(&input("A", "a.notes", "A", 1)).await;
    fixture.query(&input("A", "b.notes", "B of A", 2)).await;
    let (_, second) = fixture.query(&input("B", "a.notes", "B", 1)).await;
    assert_eq!(first["pid"], second["pid"]);
    resources::enforce(&fixture.host.inner).await;
    assert_eq!(fixture.host.resource_usage().evictions, 1);
    std::fs::remove_file(fixture.root.path().join("allocate-memory")).unwrap();
    let error = fixture
        .host
        .query(fixture.scope.clone(), &input("B", "a.notes", "B after", 2))
        .await
        .unwrap_err();
    assert!(error.contains("waiting to retry"), "{error}");
    let (_, resumed) = fixture.query(&input("A", "a.notes", "A after", 3)).await;
    assert_ne!(resumed["pid"], first["pid"]);
    assert_eq!(
        resumed["open"],
        json!({"a.notes":"A after","b.notes":"B of A"})
    );
    tokio::time::sleep(Duration::from_millis(5100)).await;
    let (_, returned) = fixture.query(&input("B", "a.notes", "B after", 3)).await;
    assert_eq!(returned["pid"], resumed["pid"]);
    assert_eq!(returned["open"], json!({"a.notes":"B after"}));
    assert_eq!(fixture.starts(), 2);
}

#[tokio::test]
async fn reload_and_handoff_reuse_the_process_with_independent_complete_dirty_inventories() {
    let fixture = Fixture::new();
    let (_, first) = fixture.query(&input("A", "a.notes", "A one", 1)).await;
    fixture.query(&input("A", "b.notes", "B of A", 2)).await;
    let (new_client, second) = fixture.query(&input("B", "a.notes", "new B", 1)).await;
    assert_eq!(first["pid"], second["pid"]);
    assert_eq!(second["open"], json!({"a.notes":"new B"}));
    assert!(
        second["versions"]["a.notes"].as_i64().unwrap()
            > first["versions"]["a.notes"].as_i64().unwrap()
    );
    let (returning, third) = fixture.query(&input("A", "a.notes", "A two", 3)).await;
    assert_ne!(new_client.server, returning.server);
    assert_eq!(first["pid"], third["pid"]);
    assert_eq!(third["open"], json!({"a.notes":"A two","b.notes":"B of A"}));
    let (_, fourth) = fixture.query(&input("B", "c.notes", "C of B", 2)).await;
    assert_eq!(first["pid"], fourth["pid"]);
    assert_eq!(
        fourth["open"],
        json!({"a.notes":"new B","c.notes":"C of B"})
    );
    // Closing a parked client's file must not send didClose to the current
    // owner's server, even if that owner has the same physical path open.
    let mut close = input("A", "b.notes", "B of A", 4);
    close.query = Query::Close;
    fixture
        .host
        .query(fixture.scope.clone(), &close)
        .await
        .unwrap();
    let (_, returned) = fixture.query(&input("A", "a.notes", "A three", 5)).await;
    assert_eq!(returned["open"], json!({"a.notes":"A three"}));
    assert_eq!(first["pid"], returned["pid"]);
    assert_eq!(fixture.starts(), 1);
    for name in ["a.notes", "b.notes", "c.notes"] {
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join(name)).unwrap(),
            format!("disk:{name}")
        );
    }
}

#[tokio::test]
async fn a_live_request_keeps_its_process_and_another_client_gets_isolated_overlays() {
    let fixture = Fixture::new();
    let host = fixture.host.clone();
    let scope = fixture.scope.clone();
    let pending =
        tokio::spawn(async move { host.query(scope, &input("A", "a.notes", "hold A", 1)).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.root.path().join("hover-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (_, second) = fixture.query(&input("B", "a.notes", "B", 1)).await;
    assert_eq!(second["open"], json!({"a.notes":"B"}));
    std::fs::write(fixture.root.path().join("release-hover"), "").unwrap();
    let first = pending.await.unwrap().unwrap();
    let first: Value = serde_json::from_str(first.result["contents"].as_str().unwrap()).unwrap();
    assert_ne!(first["pid"], second["pid"]);
    assert_eq!(first["open"], json!({"a.notes":"hold A"}));
    assert_eq!(fixture.starts(), 2);
}

#[tokio::test]
async fn process_reuse_never_crosses_authority_project_root_provider_or_configuration() {
    for boundary in [
        "owner",
        "project",
        "root",
        "provider",
        "digest",
        "configuration",
    ] {
        let fixture = Fixture::new();
        let (_, first) = fixture.query(&input("A", "a.notes", "A", 1)).await;
        let other_root = tempfile::tempdir().unwrap();
        let mut scope = fixture.scope.clone();
        match boundary {
            "owner" => scope.owner = "other".into(),
            "project" => scope.project += 1,
            "root" => scope.root = other_root.path().into(),
            "provider" => scope.provider = "community/other".into(),
            "digest" => scope.digest = "b".repeat(64),
            "configuration" => scope.server.settings = json!({"changed":true}),
            _ => unreachable!(),
        }
        let response = fixture
            .host
            .query(scope, &input("B", "a.notes", "B", 1))
            .await
            .unwrap();
        let second: Value =
            serde_json::from_str(response.result["contents"].as_str().unwrap()).unwrap();
        consolidate_idle(&mut fixture.host.inner.entries.lock().unwrap());
        assert_ne!(first["pid"], second["pid"], "{boundary}");
        assert_eq!(second["open"], json!({"a.notes":"B"}), "{boundary}");
        let (_, returned) = fixture.query(&input("A", "a.notes", "A after", 2)).await;
        assert_eq!(returned["pid"], first["pid"], "{boundary}");
    }
}

#[tokio::test]
async fn workspace_discovery_is_part_of_the_reuse_identity() {
    let mut fixture = Fixture::new();
    for folder in ["one", "two"] {
        std::fs::create_dir(fixture.root.path().join(folder)).unwrap();
        std::fs::write(fixture.root.path().join(folder).join("project.json"), "{}").unwrap();
    }
    fixture.scope.server.root_markers = vec!["project.json".into()];
    let (_, first) = fixture.query(&input("A", "one/a.notes", "one", 1)).await;
    let (_, second) = fixture.query(&input("B", "two/a.notes", "two", 1)).await;
    consolidate_idle(&mut fixture.host.inner.entries.lock().unwrap());
    assert_ne!(first["pid"], second["pid"]);
    let (_, returned) = fixture
        .query(&input("A", "one/a.notes", "one after", 2))
        .await;
    assert_eq!(returned["pid"], first["pid"]);
}

#[tokio::test]
async fn cancelling_initialization_returns_complete_dirty_sources_and_the_next_request_restores_them()
 {
    let fixture = Fixture::new();
    fixture.query(&input("A", "a.notes", "A one", 1)).await;
    fixture.query(&input("A", "b.notes", "B of A", 2)).await;
    let (_, current) = fixture.query(&input("B", "a.notes", "B", 1)).await;
    let entry = fixture
        .host
        .inner
        .entries
        .lock()
        .unwrap()
        .iter()
        .find(|(key, _)| key.client == "A")
        .unwrap()
        .1
        .clone();
    let mut initializing = Box::pin(initialize(
        &fixture.scope,
        fixture.root.path(),
        &entry,
        &fixture.host.inner,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            assert!(initializing.as_mut().now_or_never().is_none());
            if entry.retained.lock().unwrap().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(initializing);
    assert!(entry.starting.lock().unwrap().is_none());
    {
        let bank = entry.retained.lock().unwrap();
        let bank = bank.as_ref().unwrap();
        assert_eq!(
            bank.overlay_source(&fixture.scope.root.join("a.notes")),
            Some("A one")
        );
        assert_eq!(
            bank.overlay_source(&fixture.scope.root.join("b.notes")),
            Some("B of A")
        );
    }
    let (_, resumed) = fixture.query(&input("A", "a.notes", "A after", 3)).await;
    assert_eq!(
        resumed["open"],
        json!({"a.notes":"A after","b.notes":"B of A"})
    );
    assert_eq!(resumed["pid"], current["pid"]);
    assert_eq!(fixture.starts(), 2);
}

#[tokio::test]
async fn changed_project_inputs_reject_handoff_without_discarding_either_clients_sources() {
    let mut fixture = Fixture::new();
    std::fs::write(fixture.root.path().join("settings.json"), "original").unwrap();
    fixture.scope.server.project_inputs =
        Some(serde_json::from_value(json!({"files":["settings.json"]})).unwrap());
    let (_, first) = fixture.query(&input("A", "a.notes", "A", 1)).await;
    fixture.query(&input("A", "b.notes", "B of A", 2)).await;
    std::fs::write(fixture.root.path().join("settings.json"), "changed").unwrap();
    let error = fixture
        .host
        .query(fixture.scope.clone(), &input("B", "a.notes", "new B", 1))
        .await
        .unwrap_err();
    assert!(error.contains("project inputs changed"), "{error}");
    let (_, resumed) = fixture.query(&input("A", "a.notes", "A after", 3)).await;
    assert_ne!(resumed["pid"], first["pid"]);
    assert_eq!(
        resumed["open"],
        json!({"a.notes":"A after","b.notes":"B of A"})
    );
    assert_eq!(fixture.starts(), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn retargeted_paths_reject_restore_but_preserve_sources_until_explicit_close() {
    let fixture = Fixture::new();
    fixture.query(&input("A", "a.notes", "A", 1)).await;
    fixture.query(&input("A", "b.notes", "B of A", 2)).await;
    fixture.query(&input("B", "a.notes", "B", 1)).await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("b.notes"), "outside secret").unwrap();
    std::fs::remove_file(fixture.root.path().join("b.notes")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("b.notes"),
        fixture.root.path().join("b.notes"),
    )
    .unwrap();
    assert!(
        fixture
            .host
            .query(fixture.scope.clone(), &input("A", "a.notes", "A after", 3))
            .await
            .is_err()
    );
    let preserved = {
        let entries = fixture.host.inner.entries.lock().unwrap();
        let entry = entries.iter().find(|(key, _)| key.client == "A").unwrap().1;
        let retained = entry.retained.lock().unwrap();
        retained
            .as_ref()
            .unwrap()
            .overlay_source(&fixture.scope.root.join("b.notes"))
            .map(str::to_owned)
    };
    assert_eq!(preserved.as_deref(), Some("B of A"));
    let mut close = input("A", "b.notes", "B of A", 4);
    close.query = Query::Close;
    fixture
        .host
        .query(fixture.scope.clone(), &close)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (_, resumed) = fixture.query(&input("A", "a.notes", "A after", 5)).await;
    assert_eq!(resumed["open"], json!({"a.notes":"A after"}));
    assert_eq!(
        std::fs::read_to_string(outside.path().join("b.notes")).unwrap(),
        "outside secret"
    );
    assert!(
        !std::fs::read_to_string(fixture.root.path().join("calls"))
            .unwrap()
            .contains("outside secret")
    );
    assert_eq!(fixture.starts(), 2);
}

#[tokio::test]
async fn overlapping_clients_release_redundant_idle_processes_without_losing_dirty_sources() {
    let fixture = Fixture::new();
    fixture
        .host
        .inner
        .reaping
        .store(true, std::sync::atomic::Ordering::Relaxed);
    fixture.query(&input("A", "b.notes", "hidden A", 1)).await;
    let host = fixture.host.clone();
    let scope = fixture.scope.clone();
    let pending =
        tokio::spawn(async move { host.query(scope, &input("A", "a.notes", "hold A", 2)).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.root.path().join("hover-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (_, second) = fixture.query(&input("B", "a.notes", "B", 1)).await;
    fixture.query(&input("B", "c.notes", "hidden B", 2)).await;
    std::fs::write(fixture.root.path().join("release-hover"), "").unwrap();
    let first = pending.await.unwrap().unwrap();
    let first: Value = serde_json::from_str(first.result["contents"].as_str().unwrap()).unwrap();
    assert_ne!(first["pid"], second["pid"]);
    let retired = {
        let entries = fixture.host.inner.entries.lock().unwrap();
        entries
            .iter()
            .find(|(key, _)| key.client == "A")
            .unwrap()
            .1
            .session
            .get()
            .unwrap()
            .process
            .lock()
            .unwrap()
            .termination()
    };
    let (_, continued) = fixture.query(&input("B", "a.notes", "B after", 3)).await;
    assert_eq!(continued["pid"], second["pid"]);
    let (_, returned) = fixture.query(&input("A", "a.notes", "A after", 3)).await;
    assert_eq!(returned["pid"], second["pid"]);
    assert_eq!(
        returned["open"],
        json!({"a.notes":"A after","b.notes":"hidden A"})
    );
    tokio::time::timeout(Duration::from_secs(5), retired.wait())
        .await
        .unwrap()
        .unwrap();
    let stale = fixture
        .host
        .query(fixture.scope.clone(), &input("B", "a.notes", "old B", 2))
        .await
        .unwrap_err();
    assert!(
        stale.contains("stale") || stale.contains("superseded"),
        "{stale}"
    );
    let (_, restored) = fixture.query(&input("B", "a.notes", "B final", 4)).await;
    assert_eq!(restored["pid"], second["pid"]);
    assert_eq!(
        restored["open"],
        json!({"a.notes":"B final","c.notes":"hidden B"})
    );
    assert_eq!(fixture.starts(), 2);
}

#[tokio::test]
async fn idle_maintenance_releases_overlap_processes_and_retains_the_newest_complete_inventory() {
    let fixture = Fixture::new();
    fixture
        .host
        .inner
        .reaping
        .store(true, std::sync::atomic::Ordering::Relaxed);
    fixture.query(&input("A", "b.notes", "hidden A", 1)).await;
    let host = fixture.host.clone();
    let scope = fixture.scope.clone();
    let pending =
        tokio::spawn(async move { host.query(scope, &input("A", "a.notes", "hold A", 2)).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.root.path().join("hover-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (_, second) = fixture.query(&input("B", "a.notes", "B", 1)).await;
    fixture.query(&input("B", "c.notes", "hidden B", 2)).await;
    // While A is still active, idle B remains useful for concurrent work.
    consolidate_idle(&mut fixture.host.inner.entries.lock().unwrap());
    {
        let entries = fixture.host.inner.entries.lock().unwrap();
        assert!(entries.values().all(|entry| entry.session.get().is_some()));
    }
    std::fs::write(fixture.root.path().join("release-hover"), "").unwrap();
    let first = pending.await.unwrap().unwrap();
    let first: Value = serde_json::from_str(first.result["contents"].as_str().unwrap()).unwrap();
    assert_ne!(first["pid"], second["pid"]);
    consolidate_idle(&mut fixture.host.inner.entries.lock().unwrap());
    {
        let entries = fixture.host.inner.entries.lock().unwrap();
        let retained = entries.iter().find(|(key, _)| key.client == "B").unwrap().1;
        assert!(retained.session.get().is_none());
        assert!(retained.starting.lock().unwrap().is_none());
        assert_eq!(
            retained
                .retained
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .overlay_source(&fixture.scope.root.join("c.notes")),
            Some("hidden B")
        );
    }
    let (_, returned) = fixture.query(&input("B", "a.notes", "B after", 3)).await;
    assert_eq!(returned["pid"], first["pid"]);
    assert_eq!(
        returned["open"],
        json!({"a.notes":"B after","c.notes":"hidden B"})
    );
    let (_, restored) = fixture.query(&input("A", "a.notes", "A after", 3)).await;
    assert_eq!(restored["pid"], first["pid"]);
    assert_eq!(
        restored["open"],
        json!({"a.notes":"A after","b.notes":"hidden A"})
    );
    assert_eq!(fixture.starts(), 2);
}

#[tokio::test]
async fn memory_sampling_does_not_pin_an_idle_process_or_discard_its_clients_dirty_sources() {
    let fixture = Fixture::new();
    fixture
        .host
        .inner
        .reaping
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (_, first) = fixture.query(&input("A", "a.notes", "A", 1)).await;
    fixture.query(&input("A", "b.notes", "hidden A", 2)).await;
    // Retain the actual production observation across another request, as the
    // asynchronous OS resource read does. It must not make an idle donor busy.
    let observation = resources::snapshot(&fixture.host.inner);
    let (_, second) = fixture.query(&input("B", "a.notes", "B", 1)).await;
    assert_eq!(first["pid"], second["pid"]);
    drop(observation);
    let (_, returned) = fixture.query(&input("A", "a.notes", "A after", 3)).await;
    assert_eq!(returned["pid"], first["pid"]);
    assert_eq!(
        returned["open"],
        json!({"a.notes":"A after","b.notes":"hidden A"})
    );
    assert_eq!(fixture.starts(), 1);
}
