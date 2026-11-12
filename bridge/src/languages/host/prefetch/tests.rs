use super::*;
use crate::languages::host::reuse::tests::{Fixture, input};
use protocol::PrefetchKind;

fn fixture() -> Fixture {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.root.path().join("peer.py"),
        include_str!("../../../../../tools/fixtures/prefetch_server.py"),
    )
    .unwrap();
    fixture
}

async fn marked(fixture: &Fixture, name: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.root.path().join(name).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

fn prefetch(path: &str, request: u64, kind: PrefetchKind) -> Request {
    Request {
        query: Query::Prefetch { kind },
        ..input("A", path, "unsaved 文😀", request)
    }
}

#[tokio::test]
async fn one_prefetch_slot_covers_discovery_and_cancellation_does_not_steal_it() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A", 1)).await;
    let permit = fixture.host.inner.prefetch.acquire().unwrap();
    let mut unavailable = fixture.scope.clone();
    unavailable.root = fixture.root.path().join("missing");
    let error = fixture
        .host
        .query(unavailable, &prefetch("b.notes", 2, PrefetchKind::Symbols))
        .await
        .unwrap_err();
    assert!(
        error.contains("deferred"),
        "Discovery must not begin before admission: {error}"
    );
    fixture.query(&input("A", "a.notes", "A current", 2)).await;
    assert!(*permit.cancelled.borrow());
    assert!(fixture.host.inner.prefetch.acquire().is_err());
    drop(permit);
    fixture
        .host
        .query(
            fixture.scope.clone(),
            &prefetch("b.notes", 3, PrefetchKind::Symbols),
        )
        .await
        .unwrap();
    assert_eq!(fixture.starts(), 1);
}

#[tokio::test]
async fn cold_prefetch_never_allocates_starts_or_displaces_another_scope() {
    let fixture = fixture();
    let request = prefetch("a.notes", 1, PrefetchKind::Symbols);
    assert!(
        fixture
            .host
            .query(fixture.scope.clone(), &request)
            .await
            .unwrap_err()
            .contains("existing editing-client")
    );
    assert!(fixture.host.inner.entries.lock().unwrap().is_empty());
    assert!(!fixture.root.path().join("calls").exists());
    fixture.query(&input("A", "a.notes", "A", 1)).await;
    let original = fixture
        .host
        .inner
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .process_identity()
        .unwrap();
    for changed in 0..5 {
        let mut scope = fixture.scope.clone();
        let mut request = prefetch("a.notes", 2, PrefetchKind::Symbols);
        match changed {
            0 => scope.owner = "other".into(),
            1 => scope.project += 1,
            2 => scope.provider = "other".into(),
            3 => scope.digest = "b".repeat(64),
            _ => request.client = "other".into(),
        }
        assert!(fixture.host.query(scope, &request).await.is_err());
        let entries = fixture.host.inner.entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries
                .values()
                .next()
                .unwrap()
                .process_identity()
                .unwrap()
                .matches(&original)
        );
    }
    assert_eq!(fixture.starts(), 1);
}

#[tokio::test]
async fn parked_client_prefetch_cannot_reclaim_the_current_clients_process_or_dirty_sources() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A", 1)).await;
    fixture
        .query(&input("A", "b.notes", "A dependency", 2))
        .await;
    let original = fixture
        .host
        .inner
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .process_identity()
        .unwrap();
    let (server_b, second) = fixture.query(&input("B", "a.notes", "B", 1)).await;
    assert_eq!(second, json!({"a.notes":"B"}));
    let error = fixture
        .host
        .query(
            fixture.scope.clone(),
            &prefetch("b.notes", 3, PrefetchKind::Symbols),
        )
        .await
        .unwrap_err();
    assert!(error.contains("own warm service"), "{error}");
    let (still_b, current_b) = fixture.query(&input("B", "a.notes", "B after", 2)).await;
    assert_eq!(
        still_b.server, server_b.server,
        "Speculation must not replace the current client's service identity"
    );
    assert_eq!(current_b, json!({"a.notes":"B after"}));
    let (_, restored_a) = fixture.query(&input("A", "a.notes", "A after", 4)).await;
    assert_eq!(
        restored_a,
        json!({"a.notes":"A after","b.notes":"A dependency"})
    );
    assert!(
        fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .iter()
            .find(|(key, _)| key.client == "A")
            .unwrap()
            .1
            .process_identity()
            .is_some_and(|current| current.matches(&original))
    );
    assert_eq!(fixture.starts(), 1);
}

#[tokio::test]
async fn warm_prefetch_uses_ordinary_source_bound_diagnostics_and_symbols() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A", 1)).await;
    let symbols = prefetch("b.notes", 2, PrefetchKind::Symbols);
    let response = fixture
        .host
        .query(fixture.scope.clone(), &symbols)
        .await
        .unwrap();
    response.validate(&symbols).unwrap();
    let values: Vec<protocol::results::Symbol> = serde_json::from_value(response.result).unwrap();
    assert_eq!(values[0].name, "Item");
    assert_eq!(
        values[0].target.source_digest.as_deref(),
        Some(symbols.source_digest.as_str())
    );
    let diagnostics = prefetch("b.notes", 3, PrefetchKind::Diagnostics);
    let response = fixture
        .host
        .query(fixture.scope.clone(), &diagnostics)
        .await
        .unwrap();
    response.validate(&diagnostics).unwrap();
    assert!(response.diagnostics_ready);
    let (_, sources) = fixture.query(&input("A", "a.notes", "A", 2)).await;
    assert_eq!(sources, json!({"a.notes":"A", "b.notes":"unsaved 文😀"}));
    assert_eq!(fixture.starts(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("b.notes")).unwrap(),
        "disk:b.notes"
    );
}

#[tokio::test]
async fn interactive_work_preempts_another_document_and_prefetch_cannot_fan_out() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A", 1)).await;
    std::fs::write(fixture.root.path().join("hold-symbols"), "").unwrap();
    let host = fixture.host.clone();
    let scope = fixture.scope.clone();
    let task = tokio::spawn(async move {
        host.query(scope, &prefetch("b.notes", 2, PrefetchKind::Symbols))
            .await
    });
    marked(&fixture, "symbols-started").await;
    assert!(
        fixture
            .host
            .query(
                fixture.scope.clone(),
                &prefetch("c.notes", 3, PrefetchKind::Diagnostics)
            )
            .await
            .unwrap_err()
            .contains("deferred")
    );
    // A stale interactive request must not cancel valid speculative work.
    assert!(
        fixture
            .host
            .query(fixture.scope.clone(), &input("A", "a.notes", "A", 1))
            .await
            .is_err()
    );
    assert!(!task.is_finished());
    let (_, sources) = fixture.query(&input("A", "a.notes", "A current", 2)).await;
    assert_eq!(sources["b.notes"], "unsaved 文😀");
    assert!(task.await.unwrap().unwrap_err().contains("superseded"));
    marked(&fixture, "symbols-cancelled").await;
    assert_eq!(fixture.starts(), 1);
    assert!(
        fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .all(|entry| entry.activity.lock().unwrap().active.is_empty())
    );
}

#[tokio::test]
async fn stalled_prefetch_times_out_cancels_rpc_and_leaves_interactive_work_available() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A", 1)).await;
    std::fs::write(fixture.root.path().join("hold-symbols"), "").unwrap();
    let error = fixture
        .host
        .query(
            fixture.scope.clone(),
            &prefetch("b.notes", 2, PrefetchKind::Symbols),
        )
        .await
        .unwrap_err();
    assert!(error.contains("time budget"), "{error}");
    marked(&fixture, "symbols-cancelled").await;
    let (_, sources) = fixture
        .query(&input("A", "a.notes", "A after timeout", 2))
        .await;
    assert_eq!(sources["b.notes"], "unsaved 文😀");
    assert_eq!(fixture.starts(), 1);
}

#[tokio::test]
async fn speculative_admission_preserves_active_reads_and_command_pins() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A", 1)).await;
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
    let (active, cancelled) =
        ActiveRequest::register(entry.clone(), &input("A", "a.notes", "A", 2)).unwrap();
    let request = prefetch("b.notes", 3, PrefetchKind::Symbols);
    assert!(
        fixture
            .host
            .query(fixture.scope.clone(), &request)
            .await
            .unwrap_err()
            .contains("deferred")
    );
    assert!(!*cancelled.borrow());
    drop(active);
    entry
        .active_uses
        .store(1, std::sync::atomic::Ordering::Relaxed);
    assert!(
        fixture
            .host
            .query(fixture.scope.clone(), &request)
            .await
            .unwrap_err()
            .contains("deferred")
    );
    entry
        .active_uses
        .store(0, std::sync::atomic::Ordering::Relaxed);
    fixture
        .host
        .query(fixture.scope.clone(), &request)
        .await
        .unwrap();
    assert_eq!(fixture.starts(), 1);
}

#[tokio::test]
async fn interactive_work_in_another_project_cancels_prefetch_without_mixing_sources() {
    let fixture = fixture();
    fixture
        .query(&input("A", "a.notes", "first project", 1))
        .await;
    let mut other = fixture.scope.clone();
    other.project += 1;
    fixture
        .host
        .query(other.clone(), &input("A", "a.notes", "other project", 1))
        .await
        .unwrap();
    std::fs::write(fixture.root.path().join("hold-symbols"), "").unwrap();
    let host = fixture.host.clone();
    let task = tokio::spawn(async move {
        host.query(other, &prefetch("b.notes", 2, PrefetchKind::Symbols))
            .await
    });
    marked(&fixture, "symbols-started").await;
    let (_, sources) = fixture
        .query(&input("A", "a.notes", "first project current", 2))
        .await;
    assert_eq!(sources, json!({"a.notes":"first project current"}));
    assert!(task.await.unwrap().unwrap_err().contains("superseded"));
    marked(&fixture, "symbols-cancelled").await;
    assert_eq!(fixture.starts(), 2);
}

#[tokio::test]
async fn ignored_cancellation_suspends_prefetch_across_handoff_until_actual_process_restart() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A", 1)).await;
    let original = fixture
        .host
        .inner
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .process_identity()
        .unwrap();
    std::fs::write(fixture.root.path().join("hold-symbols"), "").unwrap();
    std::fs::write(fixture.root.path().join("ignore-cancel"), "").unwrap();
    let error = fixture
        .host
        .query(
            fixture.scope.clone(),
            &prefetch("b.notes", 2, PrefetchKind::Symbols),
        )
        .await
        .unwrap_err();
    assert!(error.contains("time budget"), "{error}");
    marked(&fixture, "symbols-cancelled").await;
    let error = fixture
        .host
        .query(
            fixture.scope.clone(),
            &prefetch("b.notes", 3, PrefetchKind::Symbols),
        )
        .await
        .unwrap_err();
    assert!(
        error.contains("suspended"),
        "Expected suspension without another native RPC; got {error}"
    );
    fixture.query(&input("B", "a.notes", "B", 1)).await;
    let mut other = prefetch("b.notes", 2, PrefetchKind::Symbols);
    other.client = "B".into();
    let error = fixture
        .host
        .query(fixture.scope.clone(), &other)
        .await
        .unwrap_err();
    assert!(error.contains("suspended"), "{error}");
    let calls = std::fs::read_to_string(fixture.root.path().join("calls")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(
                |line| serde_json::from_str::<Value>(line).unwrap()["method"]
                    == "textDocument/documentSymbol"
            )
            .count(),
        1
    );
    assert_eq!(fixture.starts(), 1);
    {
        let entries = fixture.host.inner.entries.lock().unwrap();
        entries
            .iter()
            .find(|(key, _)| key.client == "B")
            .unwrap()
            .1
            .stop();
    }
    tokio::time::timeout(Duration::from_secs(5), original.completion.clone().wait())
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(fixture.root.path().join("hold-symbols")).unwrap();
    fixture
        .query(&input("B", "a.notes", "B after restart", 2))
        .await;
    other.request = 3;
    other.revision = 3;
    fixture
        .host
        .query(fixture.scope.clone(), &other)
        .await
        .unwrap();
    assert_eq!(fixture.starts(), 2);
    let entries = fixture.host.inner.entries.lock().unwrap();
    assert!(
        entries
            .iter()
            .find(|(key, _)| key.client == "B")
            .unwrap()
            .1
            .process_identity()
            .is_some_and(|current| !current.matches(&original))
    );
}
