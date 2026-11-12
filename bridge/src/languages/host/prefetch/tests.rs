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
        query: Query::Prefetch { kind, close: false },
        ..input("A", path, "unsaved 文😀", request)
    }
}

fn closed_prefetch(path: &str, request: u64, kind: PrefetchKind) -> Request {
    Request {
        query: Query::Prefetch { kind, close: true },
        ..input("A", path, "current closed-file source 文😀", request)
    }
}

#[tokio::test]
async fn closed_file_prefetch_retains_foreground_request_capacity() {
    let fixture = fixture();
    for index in 0..128 {
        std::fs::write(
            fixture.root.path().join(format!("closed-{index}.notes")),
            "closed disk source",
        )
        .unwrap();
    }
    fixture.query(&input("A", "a.notes", "A unsaved", 1)).await;
    fixture.query(&input("A", "c.notes", "C unsaved", 2)).await;
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
    let (mut admitted, mut declined) = (0, 0);
    for index in 0..128 {
        let request = closed_prefetch(
            &format!("closed-{index}.notes"),
            index + 3,
            PrefetchKind::Symbols,
        );
        if fixture
            .host
            .query(fixture.scope.clone(), &request)
            .await
            .is_ok()
        {
            admitted += 1;
        } else {
            declined += 1;
        }
    }
    // All temporary sources have closed, but their stale-request fences remain.
    // Background churn must leave room for a newly opened foreground document.
    let (_, sources) = fixture
        .query(&input("A", "b.notes", "B foreground unsaved", 1000))
        .await;
    assert_eq!(
        sources,
        json!({"a.notes":"A unsaved","b.notes":"B foreground unsaved","c.notes":"C unsaved"})
    );
    assert!(admitted > 0 && declined > 0);
    let stale = fixture
        .host
        .query(
            fixture.scope.clone(),
            &closed_prefetch("closed-0.notes", 3, PrefetchKind::Symbols),
        )
        .await
        .unwrap_err();
    assert!(stale.contains("stale"), "{stale}");
    assert_eq!(fixture.starts(), 1);
    assert!(
        fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .process_identity()
            .unwrap()
            .matches(&original)
    );
}

#[tokio::test]
async fn closed_file_prefetch_retires_its_overlay_and_preserves_open_drafts_and_process() {
    let fixture = fixture();
    fixture
        .query(&input("A", "a.notes", "A unsaved 文😀", 1))
        .await;
    fixture.query(&input("A", "c.notes", "C unsaved", 2)).await;
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
    for (request, kind) in [(3, PrefetchKind::Symbols), (4, PrefetchKind::Diagnostics)] {
        let response = fixture
            .host
            .query(
                fixture.scope.clone(),
                &closed_prefetch("b.notes", request, kind),
            )
            .await
            .unwrap();
        assert!(
            response.context.is_none(),
            "A retired source must not return a reusable context"
        );
        assert!(
            fixture.host.inner.prefetch.acquire().is_ok(),
            "Retirement must finish before acknowledging success"
        );
    }
    let (_, sources) = fixture
        .query(&input("A", "a.notes", "A unsaved 文😀", 5))
        .await;
    assert_eq!(
        sources,
        json!({"a.notes":"A unsaved 文😀","c.notes":"C unsaved"})
    );
    assert_eq!(fixture.starts(), 1);
    assert!(
        fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .process_identity()
            .unwrap()
            .matches(&original)
    );
}

#[tokio::test]
async fn closed_file_prefetch_declines_existing_documents_without_replacing_or_closing_them() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A unsaved", 1)).await;
    for (request, path) in [(2, "a.notes"), (3, "b.notes")] {
        let mut closed = closed_prefetch(path, request, PrefetchKind::Symbols);
        if path == "a.notes" {
            closed.document = "another-document-id".into();
        } else {
            closed.document = openwebide_core::plugins::languages::source_digest("a.notes");
        }
        let error = fixture
            .host
            .query(fixture.scope.clone(), &closed)
            .await
            .unwrap_err();
        assert!(error.contains("existing open document"), "{error}");
    }
    let (_, sources) = fixture.query(&input("A", "b.notes", "B unsaved", 4)).await;
    assert_eq!(
        sources,
        json!({"a.notes":"A unsaved","b.notes":"B unsaved"})
    );
    assert_eq!(fixture.starts(), 1);
}

#[tokio::test]
async fn closed_file_retirement_failure_recovers_only_the_original_drafts_on_a_replacement() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A unsaved", 1)).await;
    fixture.query(&input("A", "c.notes", "C unsaved", 2)).await;
    std::fs::write(fixture.root.path().join("hold-symbols"), "hold").unwrap();
    let (host, scope) = (fixture.host.clone(), fixture.scope.clone());
    let pending = tokio::spawn(async move {
        host.query(scope, &closed_prefetch("b.notes", 3, PrefetchKind::Symbols))
            .await
    });
    marked(&fixture, "symbols-started").await;
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
    let original = entry.process_identity().unwrap();
    entry.session.get().unwrap().client.close();
    assert!(pending.await.unwrap().is_err());
    std::fs::remove_file(fixture.root.path().join("hold-symbols")).unwrap();
    let (_, sources) = fixture
        .query(&input("A", "a.notes", "A after restart", 4))
        .await;
    assert_eq!(
        sources,
        json!({"a.notes":"A after restart","c.notes":"C unsaved"})
    );
    tokio::time::timeout(Duration::from_secs(5), original.completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.starts(), 2);
    assert!(*entry.stopped.borrow());
}

#[tokio::test]
async fn cancelled_closed_file_prefetch_retires_before_foreground_queries_and_handoff() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A unsaved", 1)).await;
    fixture.query(&input("A", "c.notes", "C unsaved", 2)).await;
    std::fs::write(fixture.root.path().join("hold-symbols"), "hold").unwrap();
    let (host, scope) = (fixture.host.clone(), fixture.scope.clone());
    let task = tokio::spawn(async move {
        host.query(scope, &closed_prefetch("b.notes", 3, PrefetchKind::Symbols))
            .await
    });
    marked(&fixture, "symbols-started").await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let (_, a) = fixture.query(&input("A", "a.notes", "A after", 4)).await;
    assert_eq!(a, json!({"a.notes":"A after","c.notes":"C unsaved"}));
    fixture
        .query(&input("B", "a.notes", "B independent", 1))
        .await;
    let (_, restored) = fixture.query(&input("A", "a.notes", "A restored", 5)).await;
    assert_eq!(
        restored,
        json!({"a.notes":"A restored","c.notes":"C unsaved"})
    );
    assert_eq!(fixture.starts(), 1);
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

#[test]
fn speculative_slot_is_retained_until_the_last_cleanup_owner_releases_it() {
    let queue = Queue::default();
    let permit = queue.acquire().unwrap();
    let cleanup = permit.clone();
    drop(permit);
    queue.interrupt();
    assert!(*cleanup.cancelled.borrow());
    assert!(queue.acquire().is_err());
    drop(cleanup);
    assert!(queue.acquire().is_ok());
}

#[tokio::test]
async fn reopened_unsaved_document_survives_cancelled_closed_file_prefetch() {
    let fixture = fixture();
    fixture.query(&input("A", "a.notes", "A unsaved", 1)).await;
    std::fs::write(fixture.root.path().join("hold-symbols"), "hold").unwrap();
    let (host, scope) = (fixture.host.clone(), fixture.scope.clone());
    let pending = tokio::spawn(async move {
        host.query(scope, &closed_prefetch("b.notes", 2, PrefetchKind::Symbols))
            .await
    });
    marked(&fixture, "symbols-started").await;
    let reopened = input("A", "b.notes", "reopened unsaved 文😀", 3);
    let (_, sources) = fixture.query(&reopened).await;
    assert_eq!(
        sources,
        json!({"a.notes":"A unsaved","b.notes":"reopened unsaved 文😀"})
    );
    assert!(pending.await.unwrap().unwrap_err().contains("superseded"));
    let (_, sources) = fixture.query(&input("A", "a.notes", "A current", 4)).await;
    assert_eq!(
        sources,
        json!({"a.notes":"A current","b.notes":"reopened unsaved 文😀"})
    );
    assert_eq!(fixture.starts(), 1);
}

#[tokio::test]
async fn closed_file_prefetch_error_or_timeout_retires_sources_before_handoff() {
    for marker in ["fail-symbols", "hold-symbols"] {
        let fixture = fixture();
        fixture.query(&input("A", "a.notes", "A unsaved", 1)).await;
        fixture.query(&input("A", "c.notes", "C unsaved", 2)).await;
        std::fs::write(fixture.root.path().join(marker), "fixture").unwrap();
        let error = fixture
            .host
            .query(
                fixture.scope.clone(),
                &closed_prefetch("b.notes", 3, PrefetchKind::Symbols),
            )
            .await
            .unwrap_err();
        assert!(
            error.contains(if marker == "fail-symbols" {
                "fixture symbol failure"
            } else {
                "time budget"
            }),
            "{error}"
        );
        let (_, sources) = fixture.query(&input("A", "a.notes", "A after", 4)).await;
        assert_eq!(sources, json!({"a.notes":"A after","c.notes":"C unsaved"}));
        fixture.query(&input("B", "a.notes", "B separate", 1)).await;
        let (_, restored) = fixture.query(&input("A", "a.notes", "A restored", 5)).await;
        assert_eq!(
            restored,
            json!({"a.notes":"A restored","c.notes":"C unsaved"})
        );
        assert_eq!(fixture.starts(), 1);
    }
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
