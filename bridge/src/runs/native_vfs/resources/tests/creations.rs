use super::*;
use futures::FutureExt;
mod metadata;

#[tokio::test]
async fn moved_created_trees_keep_their_creation_identity_through_ambiguous_recovery() {
    for completed in [3, 4] {
        for replacement in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut native = files(&directory);
            native.identity = "a".repeat(64);
            let plan = transaction::prepare_native(
                &native,
                native.identity.clone(),
                vec![
                    FileOperation::Create {
                        path: "fresh/child".into(),
                        overwrite: false,
                        ignore_if_exists: false,
                    },
                    FileOperation::Rename {
                        from: "fresh".into(),
                        to: "moved".into(),
                        overwrite: false,
                        ignore_if_exists: false,
                    },
                    FileOperation::Delete {
                        path: "moved".into(),
                        recursive: true,
                        ignore_if_missing: false,
                    },
                ],
                owns_workspace,
            )
            .await
            .unwrap();
            native.scope.fingerprint = plan.fingerprint().unwrap();
            let files = FaultFiles {
                files: native,
                count: Cell::new(0),
                fail: Cell::new(0),
                after: false,
                pause: Cell::new(completed),
            };
            let journal = Journal::new(0, false);
            assert!(
                transaction::apply(&files, &journal, plan.clone(), owns_workspace)
                    .now_or_never()
                    .is_none()
            );
            let record = journal.load(None).await.unwrap().unwrap();
            assert!(record.progress.active);
            assert_eq!(record.progress.completed + 1, completed);
            let tree = if completed == 3 {
                directory.path().join("moved")
            } else {
                directory.path().join(base()).join("objects/0")
            };
            let path = tree.join("child");
            let original = same_file::Handle::from_path(&path).unwrap();
            let saved = tempfile::tempdir().unwrap();
            if replacement {
                std::fs::rename(&path, saved.path().join("original")).unwrap();
                std::fs::write(&path, []).unwrap();
            }
            let current = same_file::Handle::from_path(&path).unwrap();
            assert_eq!(current == original, !replacement);
            files.pause.set(0);
            let result = transaction::recover(&files, &journal, record, owns_workspace).await;
            if replacement {
                assert!(
                    result.is_err(),
                    "An external same-byte child of a created tree was adopted: completed={completed}"
                );
                assert_eq!(same_file::Handle::from_path(&path).unwrap(), current);
                assert_eq!(
                    same_file::Handle::from_path(saved.path().join("original")).unwrap(),
                    original
                );
            } else {
                assert_eq!(result.unwrap().progress.phase, TransactionPhase::Restored);
                assert!(!directory.path().join("fresh").exists());
                assert!(!directory.path().join("moved").exists());
                assert!(!directory.path().join(base()).exists());
            }
        }
    }
}

struct Active {
    directory: tempfile::TempDir,
    files: FaultFiles,
    plan: WorkspacePlan,
    journal: Journal,
    record: JournalRecord,
    path: String,
    kind: VfsEntryKind,
    originals: Vec<(&'static str, Identity)>,
}
impl Active {
    async fn new(ordinal: usize) -> Self {
        let (directory, files, plan) = fixture().await;
        let originals = ["source", "source/file", "target", "target/prior", "old"]
            .map(|path| (path, files.files.root.identity(path).unwrap()))
            .to_vec();
        let journal = Journal::new(0, false);
        journal.pause_creation.set(Some(ordinal));
        assert!(
            transaction::apply(&files, &journal, plan.clone(), owns_workspace)
                .now_or_never()
                .is_none()
        );
        journal.pause_creation.set(None);
        let record = journal.load(None).await.unwrap().unwrap();
        assert!(record.progress.active);
        assert_eq!(record.progress.completed, ordinal);
        let transaction::ResourceStep::Create { path, kind } =
            &transaction::ResourceScript::prepare(&plan).unwrap().steps[ordinal]
        else {
            panic!("Expected a creation")
        };
        Self {
            directory,
            files,
            plan,
            journal,
            record,
            path: path.clone(),
            kind: *kind,
            originals,
        }
    }
    fn temporary(&self) -> String {
        let parent = self.path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let name = format!(".openwebide-create-{ID}-{}", self.record.progress.completed);
        if parent.is_empty() {
            name
        } else {
            format!("{parent}/{name}")
        }
    }
    fn stop_creation(&self, stage: usize) -> usize {
        let mut count = 0;
        let result = self.files.files.create_owned_observed(
            self.record.progress.completed,
            &self.path,
            self.kind,
            || {
                count += 1;
                if count == stage {
                    Err(io::Error::other("Interrupted native creation"))
                } else {
                    Ok(())
                }
            },
        );
        if stage == 0 {
            result.unwrap();
        } else {
            assert!(result.is_err());
            assert_eq!(count, stage);
        }
        count
    }
    fn reopen(&mut self) {
        self.files.files = ResourceFiles::new(
            &File::open(self.directory.path()).unwrap(),
            &self.plan.root,
            self.files.files.scope.clone(),
        )
        .unwrap();
    }
    async fn inspect_and_remove_unpublished(&mut self) {
        self.reopen();
        let record = self.journal.load(None).await.unwrap().unwrap();
        assert!(
            transaction::recover(&self.files, &self.journal, record, owns_workspace)
                .await
                .is_err()
        );
        let path = self.directory.path().join(self.temporary());
        assert!(
            path.exists(),
            "An unwitnessed creation must remain for inspection"
        );
        // Explicit operator action in this fixture, outside automatic recovery.
        if self.kind.is_dir() {
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
            std::fs::remove_dir(path).unwrap();
        } else {
            assert_eq!(std::fs::read(&path).unwrap(), b"");
            std::fs::remove_file(path).unwrap();
        }
    }
    async fn restore(&mut self) {
        self.reopen();
        let record = self.journal.load(None).await.unwrap().unwrap();
        let record = transaction::recover(&self.files, &self.journal, record, owns_workspace)
            .await
            .unwrap();
        assert_eq!(record.progress.phase, TransactionPhase::Restored);
        assert_eq!(
            transaction::capture(&self.files, &self.plan.roots, owns_workspace)
                .await
                .unwrap(),
            self.plan.before
        );
        for (path, identity) in &self.originals {
            assert_eq!(&self.files.files.root.identity(path).unwrap(), identity);
        }
        assert!(
            std::fs::read_dir(self.directory.path().join(".openwebide"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[tokio::test]
async fn every_internal_creation_boundary_recovers_before_workspace_observation() {
    for ordinal in [2, 3, 6] {
        let mut baseline = Active::new(ordinal).await;
        let total = baseline.stop_creation(0);
        baseline.restore().await;
        for stage in 1..=total {
            let mut active = Active::new(ordinal).await;
            active.stop_creation(stage);
            if stage == 2 {
                active.inspect_and_remove_unpublished().await;
            }
            active.restore().await;
        }
    }
}

#[tokio::test]
async fn later_temporary_contents_are_preserved_and_cannot_be_adopted_for_recovery() {
    for ordinal in [2, 3] {
        let mut active = Active::new(ordinal).await;
        active.stop_creation(2);
        let staging = active.directory.path().join(active.temporary());
        let temporary = if active.kind.is_dir() {
            staging.join("later")
        } else {
            staging
        };
        std::fs::write(&temporary, "later external content").unwrap();
        active.reopen();
        assert!(
            transaction::recover(
                &active.files,
                &active.journal,
                active.record.clone(),
                owns_workspace
            )
            .await
            .is_err()
        );
        assert_eq!(
            std::fs::read(&temporary).unwrap(),
            b"later external content"
        );
        let area = active.files.files.area(false).unwrap().unwrap();
        assert!(
            area.directory("new")
                .unwrap()
                .list("", 1)
                .unwrap()
                .is_empty()
        );
        assert!(
            area.directory("anchors")
                .unwrap()
                .list("", 1)
                .unwrap()
                .is_empty()
        );
        if active.kind.is_dir() {
            std::fs::remove_file(&temporary).unwrap();
        } else {
            std::fs::write(&temporary, []).unwrap();
        }
        active.inspect_and_remove_unpublished().await;
        active.restore().await;
    }
}

#[tokio::test]
async fn unpublished_creation_recovery_does_not_require_a_new_hard_link() {
    use std::os::unix::fs::PermissionsExt;
    let mut active = Active::new(3).await;
    // Metadata is durable but no anchor has been created yet. Even if linking
    // is unavailable, recovery may remove this witnessed unpublished object.
    active.stop_creation(3);
    let anchors = active.directory.path().join(base()).join("anchors");
    std::fs::set_permissions(anchors, std::fs::Permissions::from_mode(0o555)).unwrap();
    active.restore().await;
}

#[tokio::test]
async fn replacement_parent_and_unowned_empty_staging_collisions_do_not_grant_ownership() {
    let mut active = Active::new(2).await;
    active.stop_creation(2);
    let root = active.directory.path();
    std::fs::rename(root.join("target"), root.join("moved-parent")).unwrap();
    std::fs::create_dir(root.join("target")).unwrap();
    std::fs::write(root.join("target/file"), [0, 255, 13, 10]).unwrap();
    active.reopen();
    assert!(
        transaction::recover(
            &active.files,
            &active.journal,
            active.record.clone(),
            owns_workspace
        )
        .await
        .is_err()
    );
    assert!(
        active
            .directory
            .path()
            .join(format!("moved-parent/.openwebide-create-{ID}-2"))
            .is_dir()
    );
    std::fs::remove_dir_all(active.directory.path().join("target")).unwrap();
    std::fs::rename(
        active.directory.path().join("moved-parent"),
        active.directory.path().join("target"),
    )
    .unwrap();
    active.inspect_and_remove_unpublished().await;
    active.restore().await;

    let mut active = Active::new(3).await;
    let temporary = active.directory.path().join(active.temporary());
    let parent = temporary.parent().unwrap();
    let modified = std::fs::metadata(parent).unwrap().modified().unwrap();
    std::fs::write(&temporary, []).unwrap();
    assert!(
        active
            .files
            .files
            .create_owned(3, &active.path, active.kind)
            .is_err()
    );
    assert!(
        transaction::recover(
            &active.files,
            &active.journal,
            active.record.clone(),
            owns_workspace
        )
        .await
        .is_err()
    );
    assert!(temporary.is_file());
    assert!(
        active
            .files
            .files
            .area(false)
            .unwrap()
            .unwrap()
            .directory("receipts")
            .unwrap()
            .kind("3.claim")
            .unwrap()
            .is_none()
    );
    std::fs::remove_file(&temporary).unwrap();
    // Clearing the external collision also changed the created parent. Return
    // its metadata to the receipt explicitly before requesting recovery again.
    File::open(parent)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    active.restore().await;
}

#[tokio::test]
async fn partial_unpublished_claims_resume_and_unknown_metadata_is_preserved() {
    let active = Active::new(3).await;
    active.stop_creation(1);
    let area = active.files.files.area(false).unwrap().unwrap();
    let bytes = area
        .directory("receipts")
        .unwrap()
        .read("3.claim", METADATA_LIMIT)
        .unwrap();
    for length in [0, 1, bytes.len() / 2, bytes.len() - 1, bytes.len()] {
        let mut active = Active::new(3).await;
        active.stop_creation(1);
        let receipts = active
            .files
            .files
            .area(false)
            .unwrap()
            .unwrap()
            .directory("receipts")
            .unwrap();
        let bytes = receipts.read("3.claim", METADATA_LIMIT).unwrap();
        receipts.remove("3.claim", VfsEntryKind::File).unwrap();
        receipts
            .write_new("3.claim.part", &bytes[..length.min(bytes.len())])
            .unwrap();
        active.restore().await;
    }
    let mut active = Active::new(3).await;
    active.stop_creation(1);
    let receipts = active
        .files
        .files
        .area(false)
        .unwrap()
        .unwrap()
        .directory("receipts")
        .unwrap();
    receipts.remove("3.claim", VfsEntryKind::File).unwrap();
    receipts
        .write_new("3.claim.part", b"unrelated user data")
        .unwrap();
    assert!(
        transaction::recover(
            &active.files,
            &active.journal,
            active.record.clone(),
            owns_workspace
        )
        .await
        .is_err()
    );
    assert_eq!(
        receipts.read("3.claim.part", METADATA_LIMIT).unwrap(),
        b"unrelated user data"
    );
    receipts.remove("3.claim.part", VfsEntryKind::File).unwrap();
    active.restore().await;
}

#[test]
fn every_unicode_claim_prefix_is_recoverable_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let files = files(&directory);
    assert!(
        files
            .create_owned_observed(0, "文😀", VfsEntryKind::File, || Err(io::Error::other(
                "Before creation"
            )))
            .is_err()
    );
    let receipts = files
        .area(false)
        .unwrap()
        .unwrap()
        .directory("receipts")
        .unwrap();
    let bytes = receipts.read("0.claim", METADATA_LIMIT).unwrap();
    receipts.remove("0.claim", VfsEntryKind::File).unwrap();
    for length in 0..=bytes.len() {
        receipts
            .write_new("0.claim.part", &bytes[..length])
            .unwrap();
        files
            .finish_creations()
            .unwrap_or_else(|error| panic!("prefix {length}: {error}"));
    }
    futures::executor::block_on(files.finish_retained(&files.scope)).unwrap();
}
