use super::*;

#[tokio::test]
async fn speculative_scan_capacity_cannot_starve_required_metadata_observations() {
    let held = SCANS.sources.clone().acquire_many_owned(3).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("HEAD");
    std::fs::write(&path, "reference").unwrap();
    let tracker = Tracker::new(MetadataFiles {
        files: std::collections::BTreeSet::from([path]),
        parents: Vec::new(),
        boundaries: std::collections::BTreeSet::new(),
    });
    tracker.observe(root.path()).await.unwrap();
    drop(held);
}

fn files() -> ProjectFiles {
    ProjectFiles::new(
        &[
            serde_json::from_value(json!({"id":"notes","name":"Notes","extensions":["notes"]}))
                .unwrap(),
        ],
        None,
    )
    .unwrap()
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[tokio::test]
async fn lost_event_coverage_advances_generation_even_when_source_metadata_is_unchanged() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.notes"), "one").unwrap();
    let tracker = Tracker::new(files());
    let before = tracker.observe(root.path()).await.unwrap();
    assert_eq!(before, tracker.observe(root.path()).await.unwrap());
    drop(tracker.state.lock().await.events.take());
    let after = tracker.observe(root.path()).await.unwrap();
    assert!(after.revision > before.revision);
    assert_eq!(after, tracker.observe(root.path()).await.unwrap());
}

#[tokio::test]
async fn stable_sources_ignore_existing_unrelated_file_writes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.notes"), "one").unwrap();
    std::fs::write(root.path().join("calls"), "").unwrap();
    let tracker = Tracker::new(files());
    let before = tracker.observe(root.path()).await.unwrap();
    assert_eq!(before, tracker.observe(root.path()).await.unwrap());
    std::fs::write(root.path().join("calls"), "unrelated log data").unwrap();
    assert_eq!(before, tracker.observe(root.path()).await.unwrap());
    std::fs::write(root.path().join("a.notes"), "two").unwrap();
    assert!(tracker.observe(root.path()).await.unwrap().revision > before.revision);
}

#[cfg(unix)]
#[tokio::test]
async fn restored_contents_mtime_hardlinks_and_directory_aba_advance_generations() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let directory = root.path().join("nested");
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("a.notes");
    std::fs::write(&path, "one").unwrap();
    let alias = outside.path().join("alias");
    std::fs::hard_link(&path, &alias).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let tracker = Tracker::new(files());
    let before = tracker.observe(root.path()).await.unwrap();
    std::fs::write(&alias, "two").unwrap();
    std::fs::write(&alias, "one").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&alias)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    let after = tracker.observe(root.path()).await.unwrap();
    assert!(after.revision > before.revision);
    std::fs::rename(&directory, root.path().join("away")).unwrap();
    std::fs::rename(root.path().join("away"), &directory).unwrap();
    assert!(tracker.observe(root.path()).await.unwrap().revision > after.revision);
}

#[cfg(unix)]
#[tokio::test]
async fn directory_aliases_follow_dependencies_without_following_cycles_or_private_payloads() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let dependency = outside.path().join("dependency.notes");
    std::fs::write(&dependency, "one").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("dependency")).unwrap();
    std::os::unix::fs::symlink(root.path(), root.path().join("cycle")).unwrap();
    let private = root
        .path()
        .join(".openwebide/recovery-0123456789abcdef0123456789abcdef");
    std::fs::create_dir_all(&private).unwrap();
    std::fs::write(private.join("private.notes"), "private").unwrap();
    std::os::unix::fs::symlink(&private, root.path().join("private-alias")).unwrap();
    let git = root.path().join(".git/objects");
    std::fs::create_dir_all(&git).unwrap();
    std::fs::write(git.join("git.notes"), "private Git payload").unwrap();
    std::os::unix::fs::symlink(&git, root.path().join("git-alias")).unwrap();
    let tracker = Tracker::new(files());
    let before = tracker.observe(root.path()).await.unwrap();
    let state = tracker.state.lock().await;
    let inventory = state.inventory.as_ref().unwrap();
    assert!(
        inventory
            .0
            .contains_key(Path::new("dependency/dependency.notes"))
    );
    assert!(inventory.0.len() < 12);
    assert!(
        inventory
            .0
            .keys()
            .all(|path| !path.to_string_lossy().contains("private.notes"))
    );
    assert!(
        inventory
            .0
            .keys()
            .all(|path| !path.to_string_lossy().contains("git.notes"))
    );
    drop(state);
    std::fs::write(&dependency, "two").unwrap();
    assert!(tracker.observe(root.path()).await.unwrap().revision > before.revision);
}

#[tokio::test]
async fn failed_inventory_never_publishes_part_of_a_new_generation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("a.notes");
    std::fs::write(&path, "one").unwrap();
    let mut tracker = Tracker::new(files());
    tracker.limit = 3;
    let before = tracker.observe(root.path()).await.unwrap();
    std::fs::write(&path, "two").unwrap();
    for name in ["b.notes", "c.notes", "d.notes"] {
        std::fs::write(root.path().join(name), "new").unwrap();
    }
    assert!(tracker.observe(root.path()).await.is_err());
    assert_eq!(tracker.state.lock().await.revision, before.revision);
    for name in ["b.notes", "c.notes", "d.notes"] {
        std::fs::remove_file(root.path().join(name)).unwrap();
    }
    assert_eq!(
        tracker.observe(root.path()).await.unwrap().revision,
        before.revision + 1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn non_file_inputs_leave_the_last_complete_generation_intact() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.notes"), "one").unwrap();
    let tracker = Tracker::new(files());
    let before = tracker.observe(root.path()).await.unwrap();
    let socket = root.path().join("socket.notes");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let error = tracker.observe(root.path()).await.unwrap_err();
    assert!(error.contains("not a regular file"), "{error}");
    assert_eq!(tracker.state.lock().await.revision, before.revision);
    drop(listener);
    std::fs::remove_file(socket).unwrap();
    assert!(tracker.observe(root.path()).await.unwrap().revision > before.revision);
}

#[tokio::test]
async fn unmeasurable_speculative_inventory_preserves_ordinary_queries() {
    use crate::languages::host::reuse::tests::{Fixture, input};
    let fixture = Fixture::new();
    std::fs::write(
        fixture.root.path().join("peer.py"),
        include_str!("../../../../../tools/fixtures/prefetch_server.py"),
    )
    .unwrap();
    fixture.query(&input("A", "a.notes", "A", 1)).await;
    let mut tracker = Tracker::new(files());
    tracker.limit = 1;
    {
        let entries = fixture.host.inner.entries.lock().unwrap();
        assert!(
            entries
                .values()
                .next()
                .unwrap()
                .session
                .get()
                .unwrap()
                .sources
                .set(tracker)
                .is_ok()
        );
    }
    let ordinary = input("A", "a.notes", "A current", 2);
    let response = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &ordinary, files())
        .await
        .unwrap();
    assert!(response.context.is_none());
    let speculative = Request {
        query: Query::Prefetch {
            kind: protocol::PrefetchKind::Symbols,
        },
        ..input("A", "b.notes", "B", 3)
    };
    let error = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &speculative, files())
        .await
        .unwrap_err();
    assert!(error.contains("budget"), "{error}");
    fixture.query(&input("A", "a.notes", "A after", 3)).await;
    let guarded = Request {
        query: Query::Formatting {
            tab_width: 4,
            insert_spaces: true,
        },
        ..input("A", "a.notes", "A after", 4)
    };
    let error = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &guarded, files())
        .await
        .unwrap_err();
    assert!(error.contains("budget"), "{error}");
    assert_eq!(fixture.starts(), 1);
    let calls = std::fs::read_to_string(fixture.root.path().join("calls")).unwrap();
    assert!(!calls.lines().any(
        |line| serde_json::from_str::<Value>(line).unwrap()["method"]
            == "textDocument/documentSymbol"
    ));
}

#[tokio::test]
async fn cancelled_observation_retains_the_worker_slot_until_actual_cleanup() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.notes"), "one").unwrap();
    let tracker = Arc::new(Tracker::new(files()));
    let before = tracker.observe(root.path()).await.unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let owner = tracker.clone();
    let path = root.path().to_path_buf();
    let task = tokio::spawn(async move {
        owner
            .observe_with(&path, move |root, files, cancelled, limit| {
                let inventory = collect(root, files, cancelled, limit)?;
                let _ = started.send(());
                resume.recv_timeout(Duration::from_secs(5)).unwrap();
                if cancelled.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                Ok(inventory)
            })
            .await
    });
    ready.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(tracker.scanning.available_permits(), 0);
    assert_eq!(tracker.state.lock().await.revision, before.revision);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while tracker.scanning.available_permits() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let after = tracker.observe(root.path()).await.unwrap();
    // A cancelled worker owned the native queue until cleanup. Rebuilding
    // coverage invalidates the earlier observation even without a disk change.
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    assert_eq!(after.revision, before.revision + 1);
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    assert_eq!(after, before);
    assert_eq!(tracker.observe(root.path()).await.unwrap(), after);
}

#[cfg(unix)]
#[tokio::test]
async fn language_analysis_rejects_an_unobserved_dependency_aba_and_can_retry() {
    use crate::languages::host::reuse::tests::{Fixture, input};
    for guarded in [false, true] {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.root.path().join("peer.py"),
            include_str!("../../../../../tools/fixtures/prefetch_server.py"),
        )
        .unwrap();
        for name in ["symbols-started", "symbols-cancelled"] {
            std::fs::write(fixture.root.path().join(name), "").unwrap();
        }
        let warm = input("A", "a.notes", "A", 1);
        if guarded {
            fixture
                .host
                .query_with_sources(fixture.scope.clone(), &warm, files())
                .await
                .unwrap();
        } else {
            fixture
                .host
                .query(fixture.scope.clone(), &warm)
                .await
                .unwrap();
        }
        std::fs::write(fixture.root.path().join("hold-symbols"), "").unwrap();
        let host = fixture.host.clone();
        let scope = fixture.scope.clone();
        let task = tokio::spawn(async move {
            let input = Request {
                query: Query::Prefetch {
                    kind: protocol::PrefetchKind::Symbols,
                },
                ..input("A", "b.notes", "unsaved 文😀", 2)
            };
            if guarded {
                host.query_with_sources(scope, &input, files()).await
            } else {
                host.query(scope, &input).await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while std::fs::metadata(fixture.root.path().join("symbols-started"))
                .unwrap()
                .len()
                == 0
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let path = fixture.root.path().join("c.notes");
        let original = std::fs::read(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "temporary").unwrap();
        std::fs::write(&path, &original).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let client = fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .find_map(|entry| entry.session.get().map(|session| session.client.clone()))
            .unwrap();
        client
            .notify("test/releaseSymbols", json!({}))
            .await
            .unwrap();
        let result = task.await.unwrap();
        if guarded {
            assert!(result.unwrap_err().contains("Project files changed"));
        } else {
            assert_eq!(result.unwrap().result[0]["name"], "Item");
        }
        std::fs::remove_file(fixture.root.path().join("hold-symbols")).unwrap();
        let next = Request {
            query: Query::Prefetch {
                kind: protocol::PrefetchKind::Symbols,
            },
            ..input("A", "b.notes", "unsaved 文😀", 3)
        };
        if guarded {
            fixture
                .host
                .query_with_sources(fixture.scope.clone(), &next, files())
                .await
                .unwrap();
        } else {
            fixture
                .host
                .query(fixture.scope.clone(), &next)
                .await
                .unwrap();
        }
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert_eq!(fixture.starts(), 1);
    }
}
