use super::*;

#[cfg(unix)]
#[tokio::test]
async fn restored_contents_and_mtime_still_advance_observed_input_revisions() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    std::fs::write(&path, "one").unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let inputs = Inputs::new(
        root.path(),
        Some(&ProjectInputs {
            files: vec!["settings.json".into()],
            exclude: vec![],
        }),
    )
    .await
    .unwrap();
    let before = inputs.observe(root.path()).await.unwrap();
    std::fs::write(root.path().join("unrelated"), "ignored").unwrap();
    assert_eq!(
        inputs.observe(root.path()).await.unwrap().revision,
        before.revision
    );
    std::fs::write(&path, "two").unwrap();
    std::fs::write(&path, "one").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let after = inputs.observe(root.path()).await.unwrap();
    assert_eq!(after.snapshot, before.snapshot);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    assert!(after.revision > before.revision);
    assert!(!inputs.is_baseline(&after));
    assert_eq!(
        inputs.observe(root.path()).await.unwrap().revision,
        after.revision
    );
}

#[tokio::test]
async fn failed_collection_does_not_consume_part_of_a_changed_input_observation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    std::fs::write(&path, "one").unwrap();
    let inputs = Inputs::new(
        root.path(),
        Some(&ProjectInputs {
            files: vec!["*.json".into()],
            exclude: vec![],
        }),
    )
    .await
    .unwrap();
    let before = inputs.observe(root.path()).await.unwrap();
    let oversized = root.path().join("oversized.json");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(MAX_CONTENT_BYTES + 1)
        .unwrap();
    std::fs::write(&path, "two").unwrap();
    assert!(inputs.observe(root.path()).await.is_err());
    assert_eq!(inputs.revision().await, before.revision);
    std::fs::remove_file(oversized).unwrap();
    let after = inputs.observe(root.path()).await.unwrap();
    assert_eq!(after.revision, before.revision + 1);
    assert_ne!(after.snapshot, before.snapshot);
}

#[cfg(unix)]
#[tokio::test]
async fn recovery_payloads_and_file_aliases_are_never_declared_inputs() {
    let root = tempfile::tempdir().unwrap();
    let area = root
        .path()
        .join(".openwebide/recovery-0123456789abcdef0123456789abcdef");
    std::fs::create_dir_all(&area).unwrap();
    std::fs::write(area.join("settings.json"), "private").unwrap();
    std::fs::write(root.path().join("public.json"), "public").unwrap();
    std::os::unix::fs::symlink(area.join("settings.json"), root.path().join("alias.json")).unwrap();
    let inputs = Inputs::new(
        root.path(),
        Some(&ProjectInputs {
            files: vec!["**/*.json".into()],
            exclude: vec![],
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        inputs
            .baseline
            .0
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["public.json"]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn input_symlink_target_changes_invalidate_equal_content_and_directory_links_are_not_followed()
 {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    for name in ["one.json", "two.json"] {
        std::fs::write(external.path().join(name), "same content").unwrap();
    }
    symlink(external.path(), root.path().join("linked-directory")).unwrap();
    symlink(
        external.path().join("one.json"),
        root.path().join("settings.json"),
    )
    .unwrap();
    let declaration = ProjectInputs {
        files: vec!["**/*.json".into()],
        exclude: vec![],
    };
    let inputs = Inputs::new(root.path(), Some(&declaration)).await.unwrap();
    let before = inputs.current(root.path()).await.unwrap();
    assert_eq!(before.0.len(), 1);
    std::fs::remove_file(root.path().join("settings.json")).unwrap();
    symlink(
        external.path().join("two.json"),
        root.path().join("settings.json"),
    )
    .unwrap();
    let after = inputs.current(root.path()).await.unwrap();
    assert_eq!(after.0.len(), 1);
    assert_eq!(
        before.0["settings.json"].digest,
        after.0["settings.json"].digest
    );
    assert_ne!(before, after);
}

#[tokio::test]
async fn snapshots_include_content_creation_deletion_and_unicode_but_prune_excluded_outputs() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.path().join("文😀"))
        .await
        .unwrap();
    tokio::fs::create_dir(root.path().join("ignored"))
        .await
        .unwrap();
    tokio::fs::write(root.path().join("文😀/settings.json"), "one")
        .await
        .unwrap();
    let declaration = ProjectInputs {
        files: vec!["**/*.json".into()],
        exclude: vec!["ignored/**".into()],
    };
    let inputs = Inputs::new(root.path(), Some(&declaration)).await.unwrap();
    let first = inputs.current(root.path()).await.unwrap();
    assert_eq!(first.0.len(), 1);
    assert!(first.0.contains_key("文😀/settings.json"));
    tokio::fs::write(root.path().join("ignored/output.json"), "generated")
        .await
        .unwrap();
    tokio::fs::write(root.path().join("source.notes"), "edited source")
        .await
        .unwrap();
    assert_eq!(inputs.current(root.path()).await.unwrap(), first);
    tokio::fs::write(root.path().join("文😀/settings.json"), "one")
        .await
        .unwrap();
    assert_eq!(inputs.current(root.path()).await.unwrap(), first);
    tokio::fs::write(root.path().join("文😀/settings.json"), "two")
        .await
        .unwrap();
    assert_ne!(inputs.current(root.path()).await.unwrap(), first);
    tokio::fs::write(root.path().join("new.json"), "new")
        .await
        .unwrap();
    assert_eq!(inputs.current(root.path()).await.unwrap().0.len(), 2);
    tokio::fs::remove_file(root.path().join("文😀/settings.json"))
        .await
        .unwrap();
    let current = inputs.current(root.path()).await.unwrap();
    assert_eq!(current.0.len(), 1);
    assert!(current.0.contains_key("new.json"));
    assert_eq!(inputs.cache.lock().await.files.len(), 1);
}
#[tokio::test]
async fn literal_inputs_skip_unrelated_trees_and_content_budgets_apply_before_reading() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(root.path().join("unrelated"))
        .await
        .unwrap();
    let declaration = ProjectInputs {
        files: vec!["settings.json".into()],
        exclude: vec![],
    };
    let inputs = Inputs::new(root.path(), Some(&declaration)).await.unwrap();
    assert!(
        !inputs
            .patterns
            .as_ref()
            .unwrap()
            .includes_directory("unrelated")
    );
    let file = tokio::fs::File::create(root.path().join("settings.json"))
        .await
        .unwrap();
    file.set_len(MAX_CONTENT_BYTES + 1).await.unwrap();
    assert!(
        inputs
            .current(root.path())
            .await
            .unwrap_err()
            .contains("budget")
    );
    assert_eq!(
        Inputs::new(&root.path().join("missing"), None)
            .await
            .unwrap()
            .baseline,
        Snapshot::default()
    );
}
