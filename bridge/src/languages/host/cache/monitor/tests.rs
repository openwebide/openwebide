use super::*;
use openwebide_core::plugins::languages::source_digest;

struct Fixture {
    project: tempfile::TempDir,
    storage: tempfile::TempDir,
    host: Host,
    scope: Scope,
    input: Request,
}
impl Fixture {
    fn new(limit: u64) -> Self {
        let project = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        fs::write(project.path().join("main.notes"), "disk source").unwrap();
        let script = project.path().join("peer.py");
        fs::write(
            &script,
            include_str!("../../../../../../tools/fixtures/cache_server.py"),
        )
        .unwrap();
        let mut host = Host::default();
        host.set_cache_directory(storage.path().into()).unwrap();
        Arc::get_mut(&mut host.inner).unwrap().cache.retained_bytes = Ok(limit);
        let scope = Scope {
            owner: "owner".into(), project: 1, root: project.path().into(),
            provider: "community/cache".into(), digest: "a".repeat(64),
            server: serde_json::from_value(json!({"command":"python3","args":[script],
                "cache":{"versionArgs":[script,"--version"],"environment":{"FIXTURE_CACHE":"${cache}/build"}}})).unwrap(),
        };
        let input = Request {
            version: protocol::VERSION,
            client: "client".into(),
            document: "document".into(),
            request: 1,
            revision: 1,
            language: "notes".into(),
            path: "main.notes".into(),
            source: "unsaved 文😀".into(),
            source_digest: source_digest("unsaved 文😀"),
            query: Query::Hover {
                position: protocol::Position::default(),
            },
        };
        Self {
            project,
            storage,
            host,
            scope,
            input,
        }
    }
    async fn query(&mut self) -> (String, Value) {
        self.input.request += 1;
        let response = self
            .host
            .query(self.scope.clone(), &self.input)
            .await
            .unwrap();
        (
            response.server,
            serde_json::from_str(response.result["contents"].as_str().unwrap()).unwrap(),
        )
    }
    fn data(value: &Value) -> PathBuf {
        PathBuf::from(value["cache"].as_str().unwrap())
            .parent()
            .unwrap()
            .into()
    }
    async fn retired(&self, data: &Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while data.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn growing_live_cache_stops_the_process_reclaims_after_cleanup_and_restores_dirty_source() {
    let mut fixture = Fixture::new(1024 * 1024);
    fixture.scope.server.settings = json!({"growthBytes":2 * 1024 * 1024});
    let (first, value) = fixture.query().await;
    let data = Fixture::data(&value);
    let cache = PathBuf::from(value["cache"].as_str().unwrap());
    fs::write(cache.join("grow.trigger"), b"start background indexer").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fs::metadata(cache.join("grown")).is_ok_and(|meta| meta.len() >= 2 * 1024 * 1024) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    enforce(&fixture.host.inner).await;
    let usage = fixture.host.resource_usage();
    assert!(usage.cache_bytes >= 2 * 1024 * 1024);
    assert_eq!(usage.cache_evictions, 1);
    assert_eq!(usage.cache_sample_errors, 0);
    assert!(usage.cache_sample_complete);
    fixture.input.request += 1;
    let error = fixture
        .host
        .query(fixture.scope.clone(), &fixture.input)
        .await
        .unwrap_err();
    assert!(
        error.contains("waiting to retry") && error.contains("derived-cache budget"),
        "{error}"
    );
    // The native session still retains its documents; cache cleanup must not
    // depend on the session being removed or a later request restarting it.
    fixture.retired(&data).await;
    for entry in fixture.host.inner.entries.lock().unwrap().values() {
        entry.failure.lock().unwrap().until = Some(Instant::now() - Duration::from_secs(1));
    }
    let (next, value) = fixture.query().await;
    assert_ne!(first, next);
    assert_eq!(value["sources"], json!(["unsaved 文😀"]));
    assert_eq!(value["hit"], false);
    assert_eq!(
        fs::read_to_string(fixture.project.path().join("main.notes")).unwrap(),
        "disk source"
    );
    assert!(fixture.host.resource_usage().peak_cache_bytes >= 2 * 1024 * 1024);
    fixture.host.shutdown();
}

#[tokio::test]
async fn idle_owned_data_is_reclaimed_before_stopping_a_live_service() {
    let mut fixture = Fixture::new(1024 * 1024);
    let (first, _) = fixture.query().await;
    let idle = super::super::allocate(
        fixture.storage.path(),
        fixture.project.path(),
        &content_digest(b"idle"),
        DEFAULT_RETAINED_BYTES,
    )
    .unwrap();
    let data = idle.data.clone();
    fs::write(data.join("grown"), vec![0_u8; 2 * 1024 * 1024]).unwrap();
    drop(idle);
    enforce(&fixture.host.inner).await;
    assert!(!data.exists());
    assert_eq!(fixture.host.resource_usage().cache_evictions, 0);
    assert_eq!(fixture.query().await.0, first);
    fixture.host.shutdown();
}

#[tokio::test]
async fn shared_namespace_is_counted_once_and_every_live_owner_is_stopped() {
    let mut fixture = Fixture::new(1024 * 1024);
    let (_, first) = fixture.query().await;
    let entry = fixture
        .host
        .inner
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    let mut held = fixture.input.clone();
    held.document = "held-request".into();
    // An active request prevents idle-process transfer to the other client.
    let (active, _) = ActiveRequest::register(entry, &held).unwrap();
    fixture.input.client = "second-client".into();
    let (_, second) = fixture.query().await;
    assert_eq!(first["cache"], second["cache"]);
    drop(active);
    let data = Fixture::data(&first);
    fs::write(data.join("grown"), vec![0_u8; 2 * 1024 * 1024]).unwrap();
    enforce(&fixture.host.inner).await;
    let usage = fixture.host.resource_usage();
    assert_eq!(usage.cache_evictions, 2);
    assert!(
        usage.cache_bytes < 3 * 1024 * 1024,
        "shared bytes were counted twice"
    );
    fixture.retired(&data).await;
    fixture.host.shutdown();
}

#[tokio::test]
async fn unbounded_inventory_stops_owned_writers_and_preserves_unrecognized_files() {
    let mut fixture = Fixture::new(1024 * 1024);
    let (_, value) = fixture.query().await;
    let data = Fixture::data(&value);
    let root = data.parent().unwrap().parent().unwrap();
    // Root and tree scans have fixed entry/time limits. Failure to measure must
    // not leave the local producer running indefinitely or delete unknown data.
    for i in 0..257 {
        fs::write(root.join(format!("unknown-{i}")), b"preserve").unwrap();
    }
    enforce(&fixture.host.inner).await;
    assert_eq!(fixture.host.resource_usage().cache_sample_errors, 1);
    assert!(!fixture.host.resource_usage().cache_sample_complete);
    assert_eq!(fixture.host.resource_usage().cache_evictions, 1);
    assert!(
        fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .all(|entry| *entry.stopped.borrow())
    );
    assert_eq!(fs::read(root.join("unknown-0")).unwrap(), b"preserve");
    fixture.host.shutdown();
}

#[cfg(unix)]
#[tokio::test]
async fn linked_project_files_are_never_followed_or_charged_to_a_cache() {
    let mut fixture = Fixture::new(1024 * 1024);
    let (first, value) = fixture.query().await;
    let data = Fixture::data(&value);
    fs::write(
        fixture.project.path().join("large"),
        vec![0_u8; 2 * 1024 * 1024],
    )
    .unwrap();
    std::os::unix::fs::symlink(fixture.project.path(), data.join("linked-project")).unwrap();
    enforce(&fixture.host.inner).await;
    assert_eq!(fixture.host.resource_usage().cache_evictions, 0);
    assert!(fixture.host.resource_usage().cache_bytes < 1024 * 1024);
    assert_eq!(fixture.query().await.0, first);
    assert!(fixture.project.path().join("large").exists());
    fixture.host.shutdown();
}

#[tokio::test]
async fn missing_namespace_ownership_stops_producers_without_reclaiming_unknown_data() {
    let mut fixture = Fixture::new(1024 * 1024);
    let (_, value) = fixture.query().await;
    let data = Fixture::data(&value);
    fs::remove_file(data.parent().unwrap().join(".owner")).unwrap();
    fs::write(data.join("grown"), vec![0_u8; 2 * 1024 * 1024]).unwrap();
    enforce(&fixture.host.inner).await;
    assert_eq!(fixture.host.resource_usage().cache_sample_errors, 1);
    assert_eq!(fixture.host.resource_usage().cache_evictions, 1);
    assert!(data.join("grown").exists());
    assert!(
        fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .all(|entry| *entry.stopped.borrow())
    );
    fixture.host.shutdown();
}

#[tokio::test]
async fn cache_pressure_stops_the_large_namespace_and_preserves_an_independent_warm_service() {
    let mut fixture = Fixture::new(1024 * 1024);
    let (first, first_value) = fixture.query().await;
    let original = fixture.scope.provider.clone();
    fixture.scope.provider = "community/other".into();
    let (_, second_value) = fixture.query().await;
    let data = Fixture::data(&second_value);
    assert_ne!(first_value["cache"], second_value["cache"]);
    fs::write(data.join("grown"), vec![0_u8; 2 * 1024 * 1024]).unwrap();
    enforce(&fixture.host.inner).await;
    assert_eq!(fixture.host.resource_usage().cache_evictions, 1);
    fixture.retired(&data).await;
    fixture.scope.provider = original;
    assert_eq!(fixture.query().await.0, first);
    assert!(Fixture::data(&first_value).exists());
    fixture.host.shutdown();
}
