use super::*;

fn record(action: u32, name: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [0, action, u32::try_from(name.len() * 2).unwrap()] {
        bytes.extend(value.to_le_bytes());
    }
    for word in name {
        bytes.extend(word.to_le_bytes());
    }
    bytes
}

#[test]
fn notifications_preserve_unrelated_writes_and_fail_closed_for_invalid_records() {
    let name = "a.notes".encode_utf16().collect::<Vec<_>>();
    let names = BTreeMap::from([((name.clone(), false), 1)]);
    let mut budget = 100;
    assert!(
        records(
            &record(
                FILE_ACTION_MODIFIED,
                &"A.NOTES".encode_utf16().collect::<Vec<_>>()
            ),
            false,
            &names,
            &mut budget
        )
        .unwrap()
    );
    assert!(
        !records(
            &record(
                FILE_ACTION_MODIFIED,
                &"calls".encode_utf16().collect::<Vec<_>>()
            ),
            true,
            &names,
            &mut budget
        )
        .unwrap()
    );
    assert!(
        records(
            &record(FILE_ACTION_ADDED, &"new".encode_utf16().collect::<Vec<_>>()),
            true,
            &names,
            &mut budget
        )
        .unwrap()
    );
    assert!(
        !records(
            &record(
                FILE_ACTION_ADDED,
                &"other-project".encode_utf16().collect::<Vec<_>>()
            ),
            false,
            &names,
            &mut budget
        )
        .unwrap()
    );
    assert!(records(&[0], true, &names, &mut budget).is_err());
    assert!(records(&record(999, &name), true, &names, &mut budget).is_err());
    assert!(
        records(
            &record(FILE_ACTION_MODIFIED, &[47]),
            true,
            &names,
            &mut budget
        )
        .is_err()
    );
    assert!(records(&record(FILE_ACTION_MODIFIED, &name), false, &names, &mut 0).is_err());
}

#[test]
fn directory_parent_modifications_do_not_invalidate_unrelated_children() {
    let name = "project".encode_utf16().collect::<Vec<_>>();
    let names = BTreeMap::from([((name.clone(), true), 1)]);
    let mut budget = 100;
    assert!(
        !records(
            &record(FILE_ACTION_MODIFIED, &name),
            false,
            &names,
            &mut budget
        )
        .unwrap()
    );
    for action in [
        FILE_ACTION_ADDED,
        FILE_ACTION_REMOVED,
        FILE_ACTION_RENAMED_OLD_NAME,
        FILE_ACTION_RENAMED_NEW_NAME,
    ] {
        assert!(records(&record(action, &name), false, &names, &mut budget).unwrap());
    }
    // A file registered under the same case-insensitive name still receives
    // content notifications; reference kinds must not hide one another.
    let names = BTreeMap::from([((name.clone(), true), 1), ((name.clone(), false), 1)]);
    assert!(
        records(
            &record(FILE_ACTION_MODIFIED, &name),
            false,
            &names,
            &mut budget
        )
        .unwrap()
    );
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !condition() {
        assert!(started.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn native_notifications_track_changes_and_retirement_releases_all_permits() {
    use std::io::Write;

    // Own the watched parent too: the system temporary directory has unrelated
    // runner activity. Finish setup writes before installing notifications.
    let parent = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir_in(parent.path()).unwrap();
    let source = root.path().join("a.notes");
    let unrelated = root.path().join("calls");
    for (path, contents) in [(&source, "one"), (&unrelated, "")] {
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    let available = Arc::new(Semaphore::new(32));
    let mut kernel = Kernel::new(available.clone()).unwrap();
    let directory = kernel
        .register(&open(root.path()).unwrap(), true, root.path())
        .unwrap();
    let file = kernel
        .register(&open(&source).unwrap(), false, &source)
        .unwrap();
    assert!(!kernel.poll().unwrap());
    std::fs::write(&unrelated, "unrelated output").unwrap();
    // The parent-only watch must tolerate unrelated namespace changes too.
    let sibling = tempfile::tempdir_in(root.path().parent().unwrap()).unwrap();
    std::fs::write(sibling.path().join("other-project"), "unrelated").unwrap();
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(5));
        assert!(!kernel.poll().unwrap());
    }
    std::fs::write(&source, "two").unwrap();
    wait_for(|| kernel.poll().unwrap());
    kernel.remove(file).unwrap();
    kernel.remove(directory).unwrap();
    drop(kernel);
    // Cancellation is asynchronous. Capacity is released only after the
    // original driver requests acknowledge completion.
    wait_for(|| available.available_permits() == 32);
}

#[test]
fn partial_registration_budget_failure_can_retire_without_freeing_live_io() {
    let root = tempfile::tempdir().unwrap();
    // One directory's handle + event fit; the second registration does not.
    let available = Arc::new(Semaphore::new(2));
    let mut kernel = Kernel::new(available.clone()).unwrap();
    let error = kernel
        .register(&open(root.path()).unwrap(), true, root.path())
        .unwrap_err();
    assert!(error.contains("descriptor budget"), "{error}");
    drop(kernel);
    wait_for(|| available.available_permits() == 2);
}

#[test]
fn directory_replacement_rejects_lost_object_coverage() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let available = Arc::new(Semaphore::new(16));
    let mut kernel = Kernel::new(available.clone()).unwrap();
    kernel.register(&open(&root).unwrap(), true, &root).unwrap();
    assert!(!kernel.poll().unwrap());
    std::fs::rename(&root, parent.path().join("old-project")).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert!(kernel.poll().unwrap_err().contains("changed identity"));
    drop(kernel);
    wait_for(|| available.available_permits() == 16);
}

#[test]
fn failed_shared_watch_setup_preserves_capacity_until_native_retirement() {
    use super::super::super::Stamp;
    use super::super::Events;
    use std::sync::atomic::AtomicBool;

    let root = tempfile::tempdir().unwrap();
    // Queue, common directory handle, parent handle/event, then no capacity
    // for the directory's own read. All partial native reads must retire.
    let available = Arc::new(Semaphore::new(4));
    let mut events = Events::with_budget(available.clone()).unwrap();
    let paths = BTreeMap::from([(
        root.path().to_path_buf(),
        Stamp::read(root.path(), &std::fs::metadata(root.path()).unwrap()).unwrap(),
    )]);
    assert!(
        events
            .reconcile(&paths, &AtomicBool::new(false))
            .unwrap_err()
            .contains("descriptor budget")
    );
    drop(events);
    wait_for(|| available.available_permits() == 4);
}
