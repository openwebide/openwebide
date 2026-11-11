//! Behavioral contracts shared by host and browser adapter tests.

use crate::{Vfs, VfsError, vfs::VfsEntryKind};

mod resource_transfers;
pub use resource_transfers::workspace_resource_transfer_contract;

/// Run on an empty, disposable workspace; creation must be exclusive for files and idempotent for directories.
pub async fn vfs_creation_contract(vfs: &impl Vfs) {
    vfs.create("created", VfsEntryKind::Directory)
        .await
        .unwrap();
    vfs.create("created", VfsEntryKind::Directory)
        .await
        .unwrap();
    vfs.create("created/file.txt", VfsEntryKind::File)
        .await
        .unwrap();
    assert_eq!(vfs.read("created/file.txt").await.unwrap(), "");
    vfs.write("created/file.txt", "keep these bytes\n")
        .await
        .unwrap();
    assert!(matches!(
        vfs.create("created/file.txt", VfsEntryKind::File).await,
        Err(VfsError::AlreadyExists(_))
    ));
    assert_eq!(
        vfs.read("created/file.txt").await.unwrap(),
        "keep these bytes\n"
    );
    for kind in [VfsEntryKind::File, VfsEntryKind::Directory] {
        assert!(matches!(
            vfs.create("../escape", kind).await,
            Err(VfsError::PathEscape(_))
        ));
    }
    let entries = vfs.list("created").await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "created/file.txt");
    assert!(!entries[0].is_dir);
    vfs.delete("created").await.unwrap();
    assert!(matches!(
        vfs.read("created/file.txt").await,
        Err(VfsError::NotFound(_))
    ));
}

/// The same restoration, conflict and interrupted-retry contract runs against
/// local browser and remote workspace adapters. Uses an empty disposable root.
pub async fn rewind_contract(files: &impl crate::rewind::RewindFiles) {
    use crate::{RewindFile, RewindPlan, rewind::restore_files};
    files.write("existing.txt", "after").await.unwrap();
    files.write("created.txt", "created").await.unwrap();
    let plan = RewindPlan {
        skipped: Default::default(),
        message_id: 1,
        prompt: "change files".into(),
        files: vec![
            RewindFile {
                path: "existing.txt".into(),
                before: Some("before".into()),
                backup_path: None,
                binary_before: None,
                binary_after: None,
                deleted: false,
                after: "after".into(),
            },
            RewindFile {
                path: "created.txt".into(),
                before: None,
                backup_path: None,
                binary_before: None,
                binary_after: None,
                deleted: false,
                after: "created".into(),
            },
        ],
    };
    files.write("created.txt", "user's edit").await.unwrap();
    assert!(restore_files(files, &plan, || true).await.is_err());
    assert_eq!(
        files.read("existing.txt").await.unwrap().as_deref(),
        Some(b"after".as_slice())
    );
    files.write("created.txt", "created").await.unwrap();
    assert!(restore_files(files, &plan, || false).await.is_err());
    assert_eq!(
        files.read("existing.txt").await.unwrap().as_deref(),
        Some(b"after".as_slice())
    );
    // Simulate an interrupted restore: one file has already reached its target.
    files.write("existing.txt", "before").await.unwrap();
    restore_files(files, &plan, || true).await.unwrap();
    assert_eq!(
        files.read("existing.txt").await.unwrap().as_deref(),
        Some(b"before".as_slice())
    );
    assert_eq!(files.read("created.txt").await.unwrap(), None);
    restore_files(files, &plan, || true).await.unwrap();
    files.write_bytes("binary.dat", &[0, 254]).await.unwrap();
    let binary_plan = RewindPlan {
        skipped: Default::default(),
        message_id: 1,
        prompt: "binary and deleted files".into(),
        files: vec![
            RewindFile {
                path: "binary.dat".into(),
                before: None,
                after: String::new(),
                backup_path: None,
                binary_before: Some("AP8=".into()),
                binary_after: Some("AP4=".into()),
                deleted: false,
            },
            RewindFile {
                path: "gone/nested.txt".into(),
                before: Some("restore".into()),
                after: String::new(),
                backup_path: None,
                binary_before: None,
                binary_after: None,
                deleted: true,
            },
        ],
    };
    restore_files(files, &binary_plan, || true).await.unwrap();
    assert_eq!(files.read("binary.dat").await.unwrap(), Some(vec![0, 255]));
    assert_eq!(
        files.read("gone/nested.txt").await.unwrap(),
        Some(b"restore".to_vec())
    );
    restore_files(files, &binary_plan, || true).await.unwrap();
    files.delete("binary.dat").await.unwrap();
    files.delete("gone/nested.txt").await.unwrap();
    files.delete("existing.txt").await.unwrap();
}

/// Project checkpoints cover arbitrary bytes, deletion and creation, while
/// leaving Git metadata and generated output outside the restoration boundary.
pub async fn project_checkpoint_contract(vfs: &impl Vfs) {
    use crate::rewind::{ProjectCheckpoint, capture_project};
    vfs.write(".gitignore", "target/\n").await.unwrap();
    vfs.write("source.txt", "before").await.unwrap();
    vfs.write_bytes("binary.dat", &[0, 255, 1]).await.unwrap();
    vfs.write("nested/deleted.txt", "restore me").await.unwrap();
    vfs.write(".git/HEAD", "first commit").await.unwrap();
    vfs.write("target/generated", "old artifact").await.unwrap();
    let before = capture_project(vfs).await.unwrap();
    vfs.write("source.txt", "after").await.unwrap();
    vfs.write_bytes("binary.dat", &[0, 254, 2]).await.unwrap();
    vfs.delete("nested").await.unwrap();
    vfs.write("created.txt", "new").await.unwrap();
    vfs.write(".git/HEAD", "second commit").await.unwrap();
    vfs.write("target/generated", "new artifact").await.unwrap();
    let after = capture_project(vfs).await.unwrap();
    let mut checkpoint = ProjectCheckpoint::from_snapshots(before, Some(after));
    checkpoint.compact();
    let changes = checkpoint.changes().unwrap();
    assert_eq!(changes.len(), 4);
    assert!(
        changes
            .iter()
            .all(|file| !file.path.starts_with(".git/") && !file.path.starts_with("target/"))
    );
    let binary = changes
        .iter()
        .find(|file| file.path == "binary.dat")
        .unwrap();
    assert_eq!(binary.before_bytes().unwrap(), Some(vec![0, 255, 1]));
    assert_eq!(binary.after_bytes().unwrap(), Some(vec![0, 254, 2]));
    let deleted = changes
        .iter()
        .find(|file| file.path == "nested/deleted.txt")
        .unwrap();
    assert_eq!(deleted.after_bytes().unwrap(), None);
    assert_eq!(
        deleted.before_bytes().unwrap(),
        Some(b"restore me".to_vec())
    );
    for path in [
        "source.txt",
        "binary.dat",
        "created.txt",
        ".git",
        "target",
        ".gitignore",
    ] {
        vfs.delete(path).await.unwrap();
    }
}

/// Git ignore rules and checkpoint limits behave identically on browser and host filesystems.
pub async fn checkpoint_coverage_contract(vfs: &impl Vfs) {
    use crate::rewind::{ProjectCheckpoint, capture_file, capture_project};
    vfs.write(
        ".gitignore",
        "**/[Bb]in/\n**/[Oo]bj/\n*.cache\n!keep.cache\n/root-only.txt\npruned/\n# comment\n\n",
    )
    .await
    .unwrap();
    vfs.write("source.csproj", "before").await.unwrap();
    vfs.write("app/.gitignore", "!local.cache\n/nested-only.txt\n")
        .await
        .unwrap();
    for path in [
        "app/bin/artifact",
        "app/obj/artifact",
        "drop.cache",
        "keep.cache",
        "app/local.cache",
        "app/drop.cache",
        "root-only.txt",
        "app/root-only.txt",
        "app/nested-only.txt",
        "pruned/.gitignore",
        "pruned/keep.cache",
    ] {
        vfs.write(path, "unchanged").await.unwrap();
    }
    let large = vec![0u8; usize::try_from(crate::vfs::MAX_READ_BYTES).unwrap() + 1];
    vfs.write_bytes("large.bin", &large).await.unwrap();
    vfs.write_bytes("app/obj/large.bin", &large).await.unwrap();
    let before = capture_project(vfs).await.unwrap();
    assert!(before.files.contains_key("source.csproj"));
    for path in ["keep.cache", "app/local.cache", "app/root-only.txt"] {
        assert!(before.files.contains_key(path), "{path}");
    }
    for path in [
        "drop.cache",
        "app/drop.cache",
        "root-only.txt",
        "app/nested-only.txt",
        "app/bin/artifact",
        "app/obj/artifact",
        "pruned/keep.cache",
    ] {
        assert!(!before.files.contains_key(path), "{path}");
    }
    assert_eq!(before.skipped["large.bin"], "file exceeds 10 MiB");
    assert_eq!(before.skipped["app/obj/"], "gitignored");
    // Explicit file edits still checkpoint ignored paths.
    assert!(
        capture_file(vfs, "drop.cache")
            .await
            .unwrap()
            .files
            .contains_key("drop.cache")
    );
    // Crossing the size limit or changing ignore rules must never look like creation/deletion.
    vfs.write("large.bin", "now small").await.unwrap();
    vfs.write("source.csproj", "after").await.unwrap();
    vfs.write(".gitignore", "*.cache\nsource.csproj\n")
        .await
        .unwrap();
    let mut checkpoint =
        ProjectCheckpoint::from_snapshots(before, Some(capture_project(vfs).await.unwrap()));
    checkpoint.compact();
    let changes = checkpoint.changes().unwrap();
    assert!(changes.iter().all(|file| file.path != "large.bin"
        && file.path != "source.csproj"
        && !file.path.starts_with("app/bin/")
        && !file.path.starts_with("app/obj/")));
    for path in [
        ".gitignore",
        "source.csproj",
        "app",
        "large.bin",
        "drop.cache",
        "keep.cache",
        "root-only.txt",
        "pruned",
    ] {
        vfs.delete(path).await.unwrap();
    }
}

/// The same hunk restore, manual-edit conflict, binary and deleted-file review
/// contract runs against local and remote workspace adapters.
pub async fn review_contract(files: &impl crate::rewind::RewindFiles) {
    use crate::{EditDecision, ReviewRequest, RewindFile, RunChange, rewind::restore_files};
    files
        .write("review.txt", "ONE\r\nkeep\nTHREE\n")
        .await
        .unwrap();
    let record = RunChange::new(
        1,
        2,
        RewindFile::from_bytes(
            "review.txt".into(),
            Some(b"one\r\nkeep\nthree".to_vec()),
            Some(b"ONE\r\nkeep\nTHREE\n".to_vec()),
        ),
    )
    .unwrap();
    let request = |revision, decision, hunk| ReviewRequest {
        session_id: 1,
        message_id: 2,
        path: "review.txt".into(),
        revision,
        decision,
        hunk,
    };
    let accepted = record
        .prepare(request(1, EditDecision::Accepted, Some(0)))
        .unwrap();
    restore_files(files, &accepted.restore, || true)
        .await
        .unwrap();
    assert_eq!(
        files.read("review.txt").await.unwrap().unwrap(),
        b"ONE\r\nkeep\nTHREE\n"
    );
    let rejected = accepted
        .reviewed
        .prepare(request(2, EditDecision::Rejected, Some(1)))
        .unwrap();
    files.write("review.txt", "manual change").await.unwrap();
    assert!(
        restore_files(files, &rejected.restore, || true)
            .await
            .is_err()
    );
    assert_eq!(
        files.read("review.txt").await.unwrap().unwrap(),
        b"manual change"
    );
    files
        .write("review.txt", "ONE\r\nkeep\nTHREE\n")
        .await
        .unwrap();
    assert!(
        restore_files(files, &rejected.restore, || false)
            .await
            .is_err()
    );
    restore_files(files, &rejected.restore, || true)
        .await
        .unwrap();
    restore_files(files, &rejected.restore, || true)
        .await
        .unwrap();
    assert_eq!(
        files.read("review.txt").await.unwrap().unwrap(),
        b"ONE\r\nkeep\nthree"
    );
    for (path, before, after) in [
        ("binary.dat", Some(vec![0, 255]), Some(vec![0, 254])),
        ("deleted/nested.txt", Some(b"restore".to_vec()), None),
        ("created.txt", None, Some(b"created".to_vec())),
    ] {
        if let Some(bytes) = &after {
            files.write_bytes(path, bytes).await.unwrap();
        }
        let record = RunChange::new(
            1,
            2,
            RewindFile::from_bytes(path.into(), before.clone(), after),
        )
        .unwrap();
        let plan = record
            .prepare(ReviewRequest {
                session_id: 1,
                message_id: 2,
                path: path.into(),
                revision: 1,
                decision: EditDecision::Rejected,
                hunk: None,
            })
            .unwrap();
        restore_files(files, &plan.restore, || true).await.unwrap();
        assert_eq!(files.read(path).await.unwrap(), before);
        if before.is_some() {
            files.delete(path).await.unwrap();
        }
    }
    files.delete("review.txt").await.unwrap();
}

/// Atomic replacement and typed, nonrecursive removal must have identical
/// semantics on the native execution host and File System Access adapters.
pub async fn workspace_transaction_primitives_contract(vfs: &impl Vfs) {
    use crate::workspace_transaction::{EntryState, VfsTransactionFiles, WorkspaceMutation};
    vfs.create("transaction", VfsEntryKind::Directory)
        .await
        .unwrap();
    vfs.create("transaction/file", VfsEntryKind::File)
        .await
        .unwrap();
    vfs.write_bytes("transaction/file", &[0, 255, 1])
        .await
        .unwrap();
    let files = VfsTransactionFiles {
        vfs,
        identity: "owned-test-root",
    };
    let mutation = WorkspaceMutation {
        path: "transaction/file".into(),
        before: EntryState::file([0, 255, 1]),
        after: EntryState::file([254, 0, 2]),
    };
    mutation.execute(&files).await.unwrap();
    mutation.execute(&files).await.unwrap();
    assert_eq!(
        vfs.read_bytes("transaction/file").await.unwrap(),
        [254, 0, 2]
    );
    vfs.write_bytes("transaction/file", b"external change")
        .await
        .unwrap();
    assert!(mutation.execute(&files).await.is_err());
    assert_eq!(
        vfs.read_bytes("transaction/file").await.unwrap(),
        b"external change"
    );
    assert!(
        vfs.replace_atomic("transaction/missing", b"new")
            .await
            .is_err()
    );
    assert!(vfs.read_bytes("transaction/missing").await.is_err());
    assert!(vfs.replace_atomic("transaction", b"new").await.is_err());
    assert!(
        vfs.remove_entry("transaction", VfsEntryKind::Directory)
            .await
            .is_err()
    );
    assert!(
        vfs.remove_entry("transaction/file", VfsEntryKind::Directory)
            .await
            .is_err()
    );
    assert!(
        vfs.remove_entry("transaction", VfsEntryKind::File)
            .await
            .is_err()
    );
    assert_eq!(
        vfs.read_bytes("transaction/file").await.unwrap(),
        b"external change"
    );
    assert!(vfs.replace_atomic("../escape", b"new").await.is_err());
    assert!(
        vfs.remove_entry("../escape", VfsEntryKind::File)
            .await
            .is_err()
    );
    assert_eq!(vfs.list("transaction").await.unwrap().len(), 1);
    vfs.remove_entry("transaction/file", VfsEntryKind::File)
        .await
        .unwrap();
    vfs.remove_entry("transaction", VfsEntryKind::Directory)
        .await
        .unwrap();
}

/// Run the shared journaled workflow against an empty real filesystem adapter.
/// Database ownership/CAS/atomic editor commits have their own storage contract.
pub async fn workspace_transaction_contract(vfs: &impl Vfs) {
    use crate::workspace_transaction::{
        self as transaction, FileOperation, JournalRecord, PreparedJournal, TransactionProgress,
        VfsTransactionFiles, WorkspaceJournal, WorkspacePlan,
    };
    use std::sync::Mutex;
    struct Journal(Mutex<Option<JournalRecord>>);
    impl WorkspaceJournal for Journal {
        async fn prepare(&self, plan: &WorkspacePlan) -> Result<PreparedJournal, String> {
            let id = "0123456789abcdef0123456789abcdef".to_string();
            let prepared = PreparedJournal {
                id: id.clone(),
                generation: "c".repeat(32),
                revision: 1,
                fingerprint: plan.fingerprint()?,
            };
            *self.0.lock().unwrap() = Some(JournalRecord {
                id,
                generation: prepared.generation.clone(),
                revision: 1,
                plan: plan.clone(),
                progress: TransactionProgress::default(),
            });
            Ok(prepared)
        }
        async fn advance(
            &self,
            id: &str,
            generation: &str,
            revision: u64,
            progress: &TransactionProgress,
        ) -> Result<u64, String> {
            let mut state = self.0.lock().unwrap();
            let record = state.as_mut().ok_or("Missing journal")?;
            if record.id != id || record.generation != generation || record.revision != revision {
                return Err("Stale journal".into());
            }
            record.validate_transition(progress)?;
            record.progress = progress.clone();
            record.revision += 1;
            Ok(record.revision)
        }
        async fn load(&self, id: Option<&str>) -> Result<Option<JournalRecord>, String> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .as_ref()
                .filter(|record| id.is_none_or(|id| id == record.id))
                .cloned())
        }
    }
    vfs.create("resource/original/sub", VfsEntryKind::Directory)
        .await
        .unwrap();
    vfs.create("resource/original/sub/data", VfsEntryKind::File)
        .await
        .unwrap();
    vfs.write_bytes("resource/original/sub/data", &[0, 255, 128, 1])
        .await
        .unwrap();
    vfs.create("resource/delete", VfsEntryKind::File)
        .await
        .unwrap();
    vfs.write("resource/delete", "remove after copying")
        .await
        .unwrap();
    let files = VfsTransactionFiles {
        vfs,
        identity: "owned-test-root",
    };
    let operations = vec![
        FileOperation::Rename {
            from: "resource/original".into(),
            to: "resource/renamed".into(),
            overwrite: false,
            ignore_if_exists: false,
        },
        FileOperation::Create {
            path: "resource/new/file".into(),
            overwrite: false,
            ignore_if_exists: false,
        },
        FileOperation::Delete {
            path: "resource/delete".into(),
            recursive: false,
            ignore_if_missing: false,
        },
    ];
    let plan = transaction::prepare(&files, "owned-test-root".into(), operations, || true)
        .await
        .unwrap();
    let journal = Journal(Mutex::new(None));
    let committed = transaction::apply(&files, &journal, plan, || true)
        .await
        .unwrap();
    assert_eq!(
        committed.progress.phase,
        transaction::TransactionPhase::Committed
    );
    assert_eq!(
        vfs.read_bytes("resource/renamed/sub/data").await.unwrap(),
        [0, 255, 128, 1]
    );
    assert_eq!(vfs.read("resource/new/file").await.unwrap(), "");
    assert!(vfs.read("resource/delete").await.is_err());
    assert!(vfs.list("resource/original").await.is_err());

    // Recover an operation whose create primitive completed but whose cursor
    // acknowledgement was interrupted. Repeating restoration is harmless.
    let plan = transaction::prepare(
        &files,
        "owned-test-root".into(),
        vec![FileOperation::Create {
            path: "resource/interrupted".into(),
            overwrite: false,
            ignore_if_exists: false,
        }],
        || true,
    )
    .await
    .unwrap();
    let prepared = journal.prepare(&plan).await.unwrap();
    let mut record = JournalRecord {
        id: prepared.id,
        generation: "c".repeat(32),
        revision: prepared.revision,
        plan,
        progress: TransactionProgress {
            phase: transaction::TransactionPhase::Applying,
            active: true,
            ..TransactionProgress::default()
        },
    };
    *journal.0.lock().unwrap() = Some(record.clone());
    record
        .active_mutation()
        .unwrap()
        .execute(&files)
        .await
        .unwrap();
    record = transaction::recover(&files, &journal, record.clone(), || true)
        .await
        .unwrap();
    assert_eq!(
        record.progress.phase,
        transaction::TransactionPhase::Restored
    );
    transaction::recover(&files, &journal, record.clone(), || true)
        .await
        .unwrap();
    assert!(vfs.read("resource/interrupted").await.is_err());
    assert_eq!(
        vfs.read_bytes("resource/renamed/sub/data").await.unwrap(),
        [0, 255, 128, 1]
    );
    vfs.delete("resource").await.unwrap();
}
