use super::*;

#[tokio::test]
async fn original_descriptor_reads_reject_repeated_and_restored_content_changes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("data.json");
    std::fs::write(&path, "original").unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let files = Files::new(&root).unwrap();
    assert_eq!(files.read_bytes("data.json").await.unwrap(), b"original");
    let snapshot = files.finish().unwrap();
    snapshot.ensure_current(&root).unwrap();
    let files = Files::new(&root).unwrap();
    files.read_bytes("data.json").await.unwrap();
    std::fs::write(&path, "changed!").unwrap();
    std::fs::write(&path, "original").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(files.read_bytes("data.json").await.is_err());
    assert!(files.finish().is_err());
    assert!(snapshot.ensure_current(&root).is_err());
}

#[tokio::test]
async fn absent_targets_and_parent_namespaces_are_part_of_original_observation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let files = Files::new(&root).unwrap();
    assert_eq!(
        files.canonicalize("new/deep.json").await.unwrap(),
        "new/deep.json"
    );
    let snapshot = files.finish().unwrap();
    snapshot.ensure_current(&root).unwrap();
    std::fs::create_dir(root.join("new")).unwrap();
    assert!(snapshot.ensure_current(&root).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn internal_link_paths_retain_both_link_identity_and_canonical_target() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    for name in ["one.json", "two.json"] {
        std::fs::write(root.join(name), "same").unwrap();
    }
    std::os::unix::fs::symlink("one.json", root.join("alias.json")).unwrap();
    let files = Files::new(&root).unwrap();
    let canonical = files.canonicalize("alias.json").await.unwrap();
    assert_eq!(canonical, "one.json");
    assert_eq!(files.read_bytes(&canonical).await.unwrap(), b"same");
    let snapshot = files.finish().unwrap();
    assert!(snapshot.paths().contains(&"alias.json".into()));
    assert!(snapshot.paths().contains(&canonical));
    snapshot.ensure_current(&root).unwrap();
    std::fs::remove_file(root.join("alias.json")).unwrap();
    std::os::unix::fs::symlink("two.json", root.join("alias.json")).unwrap();
    assert!(snapshot.ensure_current(&root).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn hard_link_and_root_replacement_changes_reject_same_content() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap();
    let root = parent.join("selected");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("data.json"), "same").unwrap();
    let files = Files::new(&root).unwrap();
    files.read_bytes("data.json").await.unwrap();
    let snapshot = files.finish().unwrap();
    std::fs::hard_link(root.join("data.json"), parent.join("outside-link")).unwrap();
    assert!(snapshot.ensure_current(&root).is_err());
    let files = Files::new(&root).unwrap();
    files.read_bytes("data.json").await.unwrap();
    std::fs::rename(&root, parent.join("moved")).unwrap();
    std::os::unix::fs::symlink(parent.join("moved"), &root).unwrap();
    assert!(files.finish().is_err());
}

#[test]
fn metadata_capacity_covers_inflight_records_and_releases_after_rejection_and_drop() {
    let bytes = ENTRY_BYTES + "data.json".len() * 4 + "stamp".len() * 3;
    let capacity = Arc::new(Semaphore::new(bytes));
    let mut first = Snapshot {
        capacity: capacity.clone(),
        ..Snapshot::default()
    };
    first
        .retain("data.json".into(), "stamp".into(), String::new())
        .unwrap();
    assert_eq!(capacity.available_permits(), 0);
    first
        .retain("data.json".into(), "stamp".into(), String::new())
        .unwrap();
    assert_eq!(first.entries.len(), 1);
    let mut second = Snapshot {
        capacity: capacity.clone(),
        ..Snapshot::default()
    };
    assert!(
        second
            .retain("data.json".into(), "stamp".into(), String::new())
            .is_err()
    );
    assert!(second.entries.is_empty());
    drop(first);
    assert_eq!(capacity.available_permits(), bytes);
    second
        .retain("data.json".into(), "stamp".into(), String::new())
        .unwrap();
    drop(second);
    assert_eq!(capacity.available_permits(), bytes);
    let mut oversized = Snapshot::default();
    assert!(
        oversized
            .retain(
                "x".repeat(protocol::MAX_ANALYSIS_PATH_BYTES + 1),
                "stamp".into(),
                String::new()
            )
            .is_err()
    );
    assert!(oversized.entries.is_empty());
}

#[test]
fn full_resource_inventory_fits_without_creating_a_retained_result_cache() {
    let mut snapshot = Snapshot::default();
    // Resource planning already supports 10,000 entries. Metadata accounting
    // must admit that ordinary inventory rather than silently halve the limit.
    for index in 0..openwebide_core::workspace_transaction::MAX_ENTRIES {
        snapshot
            .retain(
                format!("nested/entry-{index:05}.json"),
                "m".repeat(128),
                "u".repeat(67),
            )
            .unwrap();
    }
    assert_eq!(
        snapshot.entries.len(),
        openwebide_core::workspace_transaction::MAX_ENTRIES
    );
    assert!(snapshot.bytes < MAX_BYTES);
    assert!(snapshot.paths < protocol::MAX_ANALYSIS_PATH_BYTES);
}
