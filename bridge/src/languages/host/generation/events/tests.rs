use super::*;

fn paths(values: &[&std::path::Path]) -> BTreeMap<PathBuf, Stamp> {
    values
        .iter()
        .map(|path| {
            (
                path.to_path_buf(),
                Stamp::read(path, &std::fs::metadata(path).unwrap()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn events_detect_restored_writes_through_an_outside_hard_link() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let source = root.path().join("a.notes");
    let alias = outside.path().join("alias");
    let unrelated = root.path().join("calls");
    std::fs::write(&source, "one").unwrap();
    std::fs::write(&unrelated, "").unwrap();
    std::fs::hard_link(&source, &alias).unwrap();
    let modified = std::fs::metadata(&source).unwrap().modified().unwrap();
    let budget = Arc::new(Semaphore::new(4));
    let mut events = Events::with_budget(budget.clone()).unwrap();
    assert!(
        events
            .reconcile(&paths(&[root.path(), &source]), &AtomicBool::new(false))
            .unwrap()
    );
    assert!(!events.poll().unwrap());
    std::fs::write(&unrelated, "unrelated output").unwrap();
    assert!(!events.poll().unwrap());
    for _ in 0..16 {
        std::fs::write(&alias, "two").unwrap();
        std::fs::write(&alias, "one").unwrap();
        let writer = std::fs::File::options().write(true).open(&alias).unwrap();
        writer
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        writer.sync_all().unwrap();
        assert!(events.poll().unwrap());
        assert!(!events.poll().unwrap());
        assert!(
            !events
                .reconcile(&paths(&[root.path(), &source]), &AtomicBool::new(false))
                .unwrap()
        );
    }
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "one");
    assert_eq!(
        std::fs::metadata(&source).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(budget.available_permits(), 1);
    drop(events);
    assert_eq!(budget.available_permits(), 4);
}

#[test]
fn replaced_files_get_new_watches_and_old_descriptor_permits_are_released() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("a.notes");
    std::fs::write(&source, "old").unwrap();
    let budget = Arc::new(Semaphore::new(3));
    let mut events = Events::with_budget(budget.clone()).unwrap();
    events
        .reconcile(&paths(&[root.path(), &source]), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(budget.available_permits(), 0);
    let next = root.path().join("next");
    std::fs::write(&next, "new").unwrap();
    std::fs::rename(&next, &source).unwrap();
    assert!(events.poll().unwrap());
    assert!(
        events
            .reconcile(&paths(&[root.path(), &source]), &AtomicBool::new(false))
            .unwrap()
    );
    assert_eq!(budget.available_permits(), 0);
    assert!(!events.poll().unwrap());
    std::fs::write(&source, "newer").unwrap();
    assert!(events.poll().unwrap());
    assert!(
        events
            .reconcile(&paths(&[root.path()]), &AtomicBool::new(false))
            .unwrap()
    );
    assert_eq!(budget.available_permits(), 1);
    drop(events);
    assert_eq!(budget.available_permits(), 3);
}

#[test]
fn event_budget_failure_does_not_leak_partially_installed_descriptors() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("a.notes");
    std::fs::write(&source, "one").unwrap();
    let budget = Arc::new(Semaphore::new(2));
    let mut events = Events::with_budget(budget.clone()).unwrap();
    let error = events
        .reconcile(&paths(&[root.path(), &source]), &AtomicBool::new(false))
        .unwrap_err();
    assert!(error.contains("descriptor budget"), "{error}");
    drop(events);
    assert_eq!(budget.available_permits(), 2);
}

#[tokio::test]
async fn failed_watch_registration_preserves_the_last_complete_inventory() {
    use super::super::Tracker;
    use openwebide_core::plugins::languages::ProjectFiles;
    use std::path::Path;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("a.notes");
    std::fs::write(&source, "one").unwrap();
    let files = ProjectFiles::new(
        &[serde_json::from_value(serde_json::json!({
            "id":"notes", "name":"Notes", "extensions":["notes"]
        }))
        .unwrap()],
        None,
    )
    .unwrap();
    let tracker = Tracker::new(files);
    let before = tracker.observe(root.path()).await.unwrap();
    let (old_source, old_root) = {
        let state = tracker.state.lock().await;
        let inventory = state.inventory.as_ref().unwrap();
        assert_eq!(inventory.0.len(), 2);
        (
            inventory.0[Path::new("a.notes")].stamp.clone(),
            inventory.0[Path::new("")].stamp.clone(),
        )
    };
    // One queue and one directory can open; adding the source watch must fail.
    // Inject a private budget without changing the process's descriptor limit.
    let scarce = Arc::new(Semaphore::new(2));
    tracker.state.lock().await.events = Some(Events::with_budget(scarce.clone()).unwrap());
    std::fs::write(&source, "two").unwrap();
    std::fs::write(root.path().join("b.notes"), "new").unwrap();
    let error = tracker.observe(root.path()).await.unwrap_err();
    assert!(error.contains("descriptor budget"), "{error}");
    assert_eq!(scarce.available_permits(), 2);
    {
        let state = tracker.state.lock().await;
        assert_eq!(state.revision, before.revision);
        assert!(state.events.is_none());
        let inventory = state.inventory.as_ref().unwrap();
        assert_eq!(inventory.0.len(), 2);
        assert_eq!(inventory.0[Path::new("a.notes")].stamp, old_source);
        assert_eq!(inventory.0[Path::new("")].stamp, old_root);
    }
    let after = tracker.observe(root.path()).await.unwrap();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(
        tracker
            .state
            .lock()
            .await
            .inventory
            .as_ref()
            .unwrap()
            .0
            .len(),
        3
    );
    assert_eq!(tracker.observe(root.path()).await.unwrap(), after);
}
