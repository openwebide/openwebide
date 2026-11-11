use super::*;
mod resources;
use crate::{MemoryVfs, Vfs, vfs::VfsEntryKind};
use futures::{FutureExt, executor::block_on};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Default)]
struct Files {
    memory: MemoryVfs,
    identity: String,
    mutations: AtomicUsize,
    fail_at: AtomicUsize,
    fail_after: AtomicBool,
    pause_after: AtomicUsize,
    lose_access_after: AtomicUsize,
    cancel_after: AtomicUsize,
    denied: AtomicBool,
    owner: Arc<AtomicBool>,
}
impl Files {
    fn new() -> Self {
        Self {
            identity: "root-identity".into(),
            owner: Arc::new(AtomicBool::new(true)),
            ..Self::default()
        }
    }
    fn access(&self) -> Result<(), String> {
        if self.denied.load(Ordering::Relaxed) {
            Err("Folder permission required".into())
        } else {
            Ok(())
        }
    }
    fn before(&self) -> Result<usize, String> {
        self.access()?;
        let count = self.mutations.fetch_add(1, Ordering::Relaxed) + 1;
        if count == self.fail_at.load(Ordering::Relaxed) && !self.fail_after.load(Ordering::Relaxed)
        {
            return Err("Injected filesystem failure".into());
        }
        Ok(count)
    }
    async fn after(&self, count: usize) -> Result<(), String> {
        if count == self.lose_access_after.load(Ordering::Relaxed) {
            self.denied.store(true, Ordering::Relaxed);
            return Err("Folder permission lost".into());
        }
        if count == self.cancel_after.load(Ordering::Relaxed) {
            self.owner.store(false, Ordering::Relaxed);
        }
        if count == self.pause_after.load(Ordering::Relaxed) {
            futures::future::pending::<()>().await;
        }
        if count == self.fail_at.load(Ordering::Relaxed) && self.fail_after.load(Ordering::Relaxed)
        {
            return Err("Filesystem acknowledgement lost".into());
        }
        Ok(())
    }
}
impl WorkspaceEntries for Files {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.access()?;
        self.memory
            .canonicalize(path)
            .await
            .map_err(|error| error.to_string())
    }
    async fn list(&self, path: &str) -> Result<Vec<crate::FileEntry>, String> {
        self.access()?;
        self.memory
            .list(path)
            .await
            .map_err(|error| error.to_string())
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.access()?;
        self.memory
            .read_bytes(path)
            .await
            .map_err(|error| error.to_string())
    }
    async fn create(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        let count = self.before()?;
        self.memory
            .create(path, kind)
            .await
            .map_err(|error| error.to_string())?;
        self.after(count).await
    }
    async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let count = self.before()?;
        self.memory
            .write_bytes(path, bytes)
            .await
            .map_err(|error| error.to_string())?;
        self.after(count).await
    }
    async fn delete(&self, path: &str) -> Result<(), String> {
        let count = self.before()?;
        self.memory
            .delete(path)
            .await
            .map_err(|error| error.to_string())?;
        self.after(count).await
    }
}
impl TransactionFiles for Files {
    fn root_identity(&self) -> &str {
        &self.identity
    }
    async fn replace_atomic(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        WorkspaceEntries::write_bytes(self, path, bytes).await
    }
    async fn remove_entry(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        self.access()?;
        let entry = self
            .memory
            .list(parent(path))
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or("Entry disappeared")?;
        if entry.is_dir != kind.is_dir()
            || (entry.is_dir
                && !self
                    .memory
                    .list(path)
                    .await
                    .map_err(|error| error.to_string())?
                    .is_empty())
        {
            return Err("Cannot remove a changed or nonempty entry".into());
        }
        WorkspaceEntries::delete(self, path).await
    }
}
#[derive(Default)]
struct Journal {
    record: Mutex<Option<JournalRecord>>,
    fail_prepare: AtomicBool,
    lose_commit_ack: AtomicBool,
    advances: AtomicUsize,
    fail_advance: AtomicUsize,
    fail_advance_after: AtomicBool,
    replacement_after_failed_advance: Mutex<Option<JournalRecord>>,
}
impl WorkspaceJournal for Journal {
    async fn prepare(&self, plan: &WorkspacePlan) -> Result<PreparedJournal, String> {
        if self.fail_prepare.load(Ordering::Relaxed) {
            return Err("Recovery storage unavailable".into());
        }
        let mut record = self.record.lock().unwrap();
        if record.as_ref().is_some_and(|record| {
            !matches!(
                record.progress.phase,
                TransactionPhase::Committed | TransactionPhase::Restored
            )
        }) {
            return Err("Recover the pending operation first".into());
        }
        *record = Some(JournalRecord {
            id: "operation".into(),
            generation: "c".repeat(32),
            revision: 1,
            plan: plan.clone(),
            progress: TransactionProgress::default(),
        });
        Ok(PreparedJournal {
            id: "operation".into(),
            generation: "c".repeat(32),
            revision: 1,
            fingerprint: plan.fingerprint()?,
        })
    }
    async fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &TransactionProgress,
    ) -> Result<u64, String> {
        let count = self.advances.fetch_add(1, Ordering::Relaxed) + 1;
        let fail = count == self.fail_advance.load(Ordering::Relaxed);
        if fail && !self.fail_advance_after.load(Ordering::Relaxed) {
            if let Some(replacement) = self.replacement_after_failed_advance.lock().unwrap().take()
            {
                *self.record.lock().unwrap() = Some(replacement);
            }
            return Err("Recovery storage interrupted before commit".into());
        }
        let mut guard = self.record.lock().unwrap();
        let record = guard.as_mut().ok_or("Recovery journal missing")?;
        if record.id != id || record.generation != generation || record.revision != revision {
            return Err("Recovery journal revision changed".into());
        }
        record.validate_transition(progress)?;
        record.revision += 1;
        record.progress = progress.clone();
        if progress.phase == TransactionPhase::Committed
            && self.lose_commit_ack.swap(false, Ordering::Relaxed)
        {
            return Err("Commit acknowledgement lost".into());
        }
        if fail {
            return Err("Recovery storage acknowledgement lost".into());
        }
        Ok(record.revision)
    }
    async fn load(&self, id: Option<&str>) -> Result<Option<JournalRecord>, String> {
        Ok(self
            .record
            .lock()
            .unwrap()
            .clone()
            .filter(|record| id.is_none_or(|id| id == record.id)))
    }
}
fn rename(from: &str, to: &str) -> FileOperation {
    FileOperation::Rename {
        from: from.into(),
        to: to.into(),
        overwrite: false,
        ignore_if_exists: false,
    }
}
async fn fixture() -> (Files, Journal, WorkspacePlan) {
    let files = Files::new();
    files
        .memory
        .write_bytes("before/文.bin", &[0, 255, 13, 10, 42])
        .await
        .unwrap();
    let plan = prepare(
        &files,
        "root-identity".into(),
        vec![rename("before", "new/deep/folder")],
        || true,
    )
    .await
    .unwrap();
    assert_eq!(files.mutations.load(Ordering::Relaxed), 0);
    (files, Journal::default(), plan)
}

#[test]
fn recovery_preview_checks_every_durable_cursor_and_ambiguous_primitive_without_writes() {
    block_on(async {
        let (files, _, plan) = fixture().await;
        let mut record = JournalRecord {
            id: "preview".into(),
            generation: "c".repeat(32),
            revision: 1,
            plan,
            progress: TransactionProgress {
                phase: TransactionPhase::Applying,
                ..TransactionProgress::default()
            },
        };
        loop {
            let expected = capture(&files, &record.plan.roots, || true).await.unwrap();
            let count = files.mutations.load(Ordering::Relaxed);
            assert_eq!(
                preview_recovery(&files, &record, || true).await.unwrap(),
                expected
            );
            assert_eq!(files.mutations.load(Ordering::Relaxed), count);
            let mut rollback = record.clone();
            rollback.progress = TransactionProgress {
                phase: TransactionPhase::Reverting,
                completed: 0,
                active: false,
                rollback_from: Some(record.progress.completed),
            };
            assert_eq!(
                preview_recovery(&files, &rollback, || true).await.unwrap(),
                expected
            );
            let undo = Files::new();
            for (path, entry) in &expected {
                match entry {
                    EntryState::Directory => undo
                        .memory
                        .create(path, VfsEntryKind::Directory)
                        .await
                        .unwrap(),
                    EntryState::File { bytes } => {
                        undo.memory.write_bytes(path, bytes).await.unwrap();
                    }
                    EntryState::Missing => (),
                }
            }
            loop {
                let before = capture(&undo, &record.plan.roots, || true).await.unwrap();
                let mutations = undo.mutations.load(Ordering::Relaxed);
                assert_eq!(
                    preview_recovery(&undo, &rollback, || true).await.unwrap(),
                    before
                );
                assert_eq!(undo.mutations.load(Ordering::Relaxed), mutations);
                rollback.progress.active = true;
                let Ok(mutation) = rollback.active_mutation() else {
                    break;
                };
                assert_eq!(
                    preview_recovery(&undo, &rollback, || true).await.unwrap(),
                    before
                );
                mutation.execute(&undo).await.unwrap();
                let after = capture(&undo, &record.plan.roots, || true).await.unwrap();
                assert_eq!(
                    preview_recovery(&undo, &rollback, || true).await.unwrap(),
                    after
                );
                rollback.progress.completed += 1;
                rollback.progress.active = false;
            }
            record.progress.active = true;
            let Ok(mutation) = record.active_mutation() else {
                break;
            };
            assert_eq!(
                preview_recovery(&files, &record, || true).await.unwrap(),
                expected
            );
            assert_eq!(files.mutations.load(Ordering::Relaxed), count);
            mutation.execute(&files).await.unwrap();
            let after = capture(&files, &record.plan.roots, || true).await.unwrap();
            assert_eq!(
                preview_recovery(&files, &record, || true).await.unwrap(),
                after
            );
            record.progress.completed += 1;
            record.progress.active = false;
        }
    });
}

#[test]
fn recovery_preview_preserves_external_contents_and_rejects_terminal_or_stale_reviews() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        let prepared = journal.prepare(&plan).await.unwrap();
        let record = JournalRecord {
            id: prepared.id,
            generation: "c".repeat(32),
            revision: prepared.revision,
            plan: plan.clone(),
            progress: TransactionProgress {
                phase: TransactionPhase::Applying,
                ..TransactionProgress::default()
            },
        };
        assert!(preview_recovery(&files, &record, || false).await.is_err());
        let mut terminal = record.clone();
        terminal.progress = TransactionProgress::default();
        assert!(preview_recovery(&files, &terminal, || true).await.is_err());
        files
            .memory
            .write_bytes("before/external", b"keep")
            .await
            .unwrap();
        assert!(preview_recovery(&files, &record, || true).await.is_err());
        assert!(recover(&files, &journal, record, || true).await.is_err());
        assert_eq!(
            files.memory.read_bytes("before/external").await.unwrap(),
            b"keep"
        );
        assert_eq!(files.mutations.load(Ordering::Relaxed), 0);
    });
}
#[test]
fn ordered_resource_operations_preserve_binary_data_and_capture_new_parent_directories() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        assert!(plan.roots.contains(&"new".into()));
        let roundtrip: WorkspacePlan =
            serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
        assert_eq!(plan, roundtrip);
        let expected = plan.after.clone();
        let roots = plan.roots.clone();
        let result = apply(&files, &journal, plan, || true).await.unwrap();
        assert_eq!(result.progress.phase, TransactionPhase::Committed);
        assert_eq!(capture(&files, &roots, || true).await.unwrap(), expected);
        assert_eq!(
            files
                .memory
                .read_bytes("new/deep/folder/文.bin")
                .await
                .unwrap(),
            [0, 255, 13, 10, 42]
        );
    });
}
#[test]
fn every_filesystem_failure_before_or_after_acknowledgement_restores_the_entire_plan() {
    block_on(async {
        let (baseline_files, baseline_journal, baseline_plan) = fixture().await;
        apply(&baseline_files, &baseline_journal, baseline_plan, || true)
            .await
            .unwrap();
        let mutations = baseline_files.mutations.load(Ordering::Relaxed);
        for after in [false, true] {
            for fail_at in 1..=mutations {
                let (files, journal, plan) = fixture().await;
                let original = plan.before.clone();
                let roots = plan.roots.clone();
                files.fail_at.store(fail_at, Ordering::Relaxed);
                files.fail_after.store(after, Ordering::Relaxed);
                let error = apply(&files, &journal, plan, || true).await.unwrap_err();
                assert_eq!(
                    error.recovery,
                    RecoveryState::Restored,
                    "failure {fail_at}, after {after}: {error}"
                );
                assert_eq!(
                    capture(&files, &roots, || true).await.unwrap(),
                    original,
                    "failure {fail_at}, after {after}"
                );
                assert_eq!(
                    journal.load(None).await.unwrap().unwrap().progress.phase,
                    TransactionPhase::Restored
                );
            }
        }
    });
}
#[test]
fn missing_recovery_storage_and_stale_review_never_mutate_files() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        journal.fail_prepare.store(true, Ordering::Relaxed);
        assert_eq!(
            apply(&files, &journal, plan.clone(), || true)
                .await
                .unwrap_err()
                .recovery,
            RecoveryState::Unchanged
        );
        assert_eq!(files.mutations.load(Ordering::Relaxed), 0);
        journal.fail_prepare.store(false, Ordering::Relaxed);
        files
            .memory
            .write_bytes("before/文.bin", b"later external edit")
            .await
            .unwrap();
        assert_eq!(
            apply(&files, &journal, plan, || true)
                .await
                .unwrap_err()
                .recovery,
            RecoveryState::Unchanged
        );
        assert_eq!(files.mutations.load(Ordering::Relaxed), 0);
        assert!(journal.load(None).await.unwrap().is_none());
    });
}
#[test]
fn dropped_mutation_future_and_permission_regrant_recover_from_the_durable_active_step() {
    block_on(async {
        for permission in [false, true] {
            let (files, journal, plan) = fixture().await;
            let before = plan.before.clone();
            let roots = plan.roots.clone();
            if permission {
                files.lose_access_after.store(4, Ordering::Relaxed);
            } else {
                files.pause_after.store(4, Ordering::Relaxed);
            }
            if permission {
                assert_eq!(
                    apply(&files, &journal, plan, || true)
                        .await
                        .unwrap_err()
                        .recovery,
                    RecoveryState::Pending
                );
            } else {
                assert!(
                    apply(&files, &journal, plan, || true)
                        .now_or_never()
                        .is_none()
                );
            }
            let pending = journal.load(None).await.unwrap().unwrap();
            assert!(pending.progress.active);
            files.denied.store(false, Ordering::Relaxed);
            files.pause_after.store(0, Ordering::Relaxed);
            let restored = recover(&files, &journal, pending, || true).await.unwrap();
            assert_eq!(restored.progress.phase, TransactionPhase::Restored);
            assert_eq!(capture(&files, &roots, || true).await.unwrap(), before);
        }
    });
}
#[test]
fn external_edits_during_interruption_are_preserved_instead_of_forcing_rollback() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        files.pause_after.store(4, Ordering::Relaxed);
        assert!(
            apply(&files, &journal, plan, || true)
                .now_or_never()
                .is_none()
        );
        files.pause_after.store(0, Ordering::Relaxed);
        files
            .memory
            .write_bytes("new/deep/folder/文.bin", b"independent edit")
            .await
            .unwrap();
        let pending = journal.load(None).await.unwrap().unwrap();
        assert_eq!(
            recover(&files, &journal, pending, || true)
                .await
                .unwrap_err()
                .recovery,
            RecoveryState::Pending
        );
        assert_eq!(
            files
                .memory
                .read_bytes("new/deep/folder/文.bin")
                .await
                .unwrap(),
            b"independent edit"
        );
        assert_eq!(
            files.memory.read_bytes("before/文.bin").await.unwrap(),
            [0, 255, 13, 10, 42]
        );
    });
}
#[test]
fn acknowledged_commit_is_not_rolled_back_after_its_response_is_lost() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        let expected = plan.after.clone();
        let roots = plan.roots.clone();
        journal.lose_commit_ack.store(true, Ordering::Relaxed);
        assert_eq!(
            apply(&files, &journal, plan, || true)
                .await
                .unwrap()
                .progress
                .phase,
            TransactionPhase::Committed
        );
        assert_eq!(capture(&files, &roots, || true).await.unwrap(), expected);
    });
}
#[test]
fn ownership_change_stops_all_further_mutations_until_original_scope_is_restored() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        let before = plan.before.clone();
        let roots = plan.roots.clone();
        files.cancel_after.store(1, Ordering::Relaxed);
        let error = apply(&files, &journal, plan, || {
            files.owner.load(Ordering::Relaxed)
        })
        .await
        .unwrap_err();
        assert_eq!(error.recovery, RecoveryState::Pending);
        assert_eq!(files.mutations.load(Ordering::Relaxed), 1);
        let pending = journal.load(None).await.unwrap().unwrap();
        assert!(pending.progress.active);
        files.owner.store(true, Ordering::Relaxed);
        recover(&files, &journal, pending, || true).await.unwrap();
        assert_eq!(capture(&files, &roots, || true).await.unwrap(), before);
    });
}
#[test]
fn resource_options_and_immutable_plan_validation_reject_unreviewed_changes() {
    block_on(async {
        let files = Files::new();
        files.memory.write("a", "original").await.unwrap();
        files.memory.write("b", "destination").await.unwrap();
        let mut create = FileOperation::Create {
            path: "b".into(),
            overwrite: false,
            ignore_if_exists: true,
        };
        let no_op = prepare(
            &files,
            files.root_identity().into(),
            vec![create.clone()],
            || true,
        )
        .await
        .unwrap();
        assert!(no_op.changed_paths().is_empty());
        if let FileOperation::Create { overwrite, .. } = &mut create {
            *overwrite = true;
        }
        let clear = prepare(&files, files.root_identity().into(), vec![create], || true)
            .await
            .unwrap();
        assert_eq!(clear.after["b"], EntryState::file(Vec::new()));
        assert!(
            prepare(
                &files,
                files.root_identity().into(),
                vec![rename("a", "b")],
                || true
            )
            .await
            .is_err()
        );
        assert!(
            prepare(
                &files,
                files.root_identity().into(),
                vec![FileOperation::Delete {
                    path: ".git/config".into(),
                    recursive: false,
                    ignore_if_missing: true
                }],
                || true
            )
            .await
            .is_err()
        );
        files.memory.write("folder/file", "inside").await.unwrap();
        assert!(
            prepare(
                &files,
                files.root_identity().into(),
                vec![FileOperation::Delete {
                    path: "folder".into(),
                    recursive: false,
                    ignore_if_missing: false
                }],
                || true
            )
            .await
            .is_err()
        );
        let mut tampered = clear;
        tampered
            .after
            .insert("b".into(), EntryState::file(b"unreviewed".to_vec()));
        assert!(tampered.validate().is_err());
        assert_eq!(files.mutations.load(Ordering::Relaxed), 0);
    });
}

#[test]
fn every_journal_transport_failure_reconciles_its_durable_revision_before_mutating_again() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        apply(&files, &journal, plan, || true).await.unwrap();
        let advances = journal.advances.load(Ordering::Relaxed);
        for after in [false, true] {
            for fail_at in 1..=advances {
                let (files, journal, plan) = fixture().await;
                let before = plan.before.clone();
                let expected = plan.after.clone();
                let roots = plan.roots.clone();
                journal.fail_advance.store(fail_at, Ordering::Relaxed);
                journal.fail_advance_after.store(after, Ordering::Relaxed);
                let result = apply(&files, &journal, plan, || true).await;
                let persisted = journal.load(None).await.unwrap().unwrap();
                if persisted.progress.phase == TransactionPhase::Committed {
                    assert_eq!(result.unwrap().progress.phase, TransactionPhase::Committed);
                    assert_eq!(capture(&files, &roots, || true).await.unwrap(), expected);
                } else {
                    assert_eq!(
                        result.unwrap_err().recovery,
                        RecoveryState::Restored,
                        "advance {fail_at}, after {after}"
                    );
                    assert_eq!(capture(&files, &roots, || true).await.unwrap(), before);
                    assert_eq!(persisted.progress.phase, TransactionPhase::Restored);
                }
            }
        }
    });
}

#[test]
fn ordered_folder_overwrite_and_source_recreation_commit_the_exact_reviewed_tree() {
    block_on(async {
        let files = Files::new();
        let journal = Journal::default();
        files
            .memory
            .write_bytes("source/one", &[0, 255])
            .await
            .unwrap();
        files
            .memory
            .write("source/nested/two", "文😀\r\n")
            .await
            .unwrap();
        files
            .memory
            .write("target/old", "superseded")
            .await
            .unwrap();
        files.memory.write("trash", "removed").await.unwrap();
        let operations = vec![
            FileOperation::Rename {
                from: "source".into(),
                to: "target".into(),
                overwrite: true,
                ignore_if_exists: false,
            },
            FileOperation::Create {
                path: "source/created".into(),
                overwrite: false,
                ignore_if_exists: false,
            },
            FileOperation::Delete {
                path: "trash".into(),
                recursive: false,
                ignore_if_missing: false,
            },
        ];
        let plan = prepare(&files, files.root_identity().into(), operations, || true)
            .await
            .unwrap();
        let roots = plan.roots.clone();
        let after = plan.after.clone();
        apply(&files, &journal, plan, || true).await.unwrap();
        assert_eq!(capture(&files, &roots, || true).await.unwrap(), after);
        assert_eq!(
            files.memory.read_bytes("target/one").await.unwrap(),
            [0, 255]
        );
        assert_eq!(
            files.memory.read("target/nested/two").await.unwrap(),
            "文😀\r\n"
        );
        assert_eq!(files.memory.read("source/created").await.unwrap(), "");
        assert!(files.memory.read("target/old").await.is_err());
        assert!(files.memory.read("source/one").await.is_err());
    });
}

#[test]
fn identical_files_in_a_different_folder_cannot_receive_an_old_transaction() {
    block_on(async {
        let (_, journal, plan) = fixture().await;
        let mut other = Files::new();
        other.identity = "another-folder".into();
        other
            .memory
            .write_bytes("before/文.bin", &[0, 255, 13, 10, 42])
            .await
            .unwrap();
        assert_eq!(
            apply(&other, &journal, plan, || true)
                .await
                .unwrap_err()
                .recovery,
            RecoveryState::Unchanged
        );
        assert_eq!(other.mutations.load(Ordering::Relaxed), 0);
        assert!(journal.load(None).await.unwrap().is_none());
    });
}

#[test]
fn native_plans_reserve_case_aliases_of_the_recovery_namespace() {
    futures::executor::block_on(async {
        let files = Files::new();
        let mut plan = prepare(
            &files,
            "root".into(),
            vec![FileOperation::Create {
                path: ".OPENWEBIDE/config.json".into(),
                overwrite: false,
                ignore_if_exists: false,
            }],
            || true,
        )
        .await
        .unwrap();
        plan.version = 2;
        assert!(plan.validate().is_err());
    });
}

#[test]
fn native_preparation_rejects_invalid_authority_reserved_paths_and_cancellation_before_io() {
    block_on(async {
        let files = Files::new();
        files.denied.store(true, Ordering::Relaxed);
        let create = |path: &str| FileOperation::Create {
            path: path.into(),
            overwrite: false,
            ignore_if_exists: false,
        };
        let error = prepare_native(
            &files,
            "browser:1:1:legacy".into(),
            vec![create("file")],
            || true,
        )
        .await
        .unwrap_err();
        assert!(error.contains("physical folder identity"));
        let error = prepare_native(
            &files,
            "a".repeat(64),
            vec![create("file"), create(".OPENWEBIDE/notes")],
            || true,
        )
        .await
        .unwrap_err();
        assert!(error.contains("recovery namespace"));
        let error = prepare_native(&files, "a".repeat(64), vec![], || false)
            .await
            .unwrap_err();
        assert!(error.contains("superseded"));
        files.denied.store(false, Ordering::Relaxed);
        let plan = prepare_native(&files, "a".repeat(64), vec![], || true)
            .await
            .unwrap();
        assert_eq!(plan.version, 2);
        plan.validate().unwrap();
    });
}

#[test]
fn existing_execution_never_adopts_a_new_prepared_record_with_the_same_public_id() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        let before = plan.before.clone();
        let roots = plan.roots.clone();
        let original = JournalRecord {
            id: "operation".into(),
            generation: "a".repeat(32),
            revision: 1,
            plan,
            progress: TransactionProgress::default(),
        };
        let error = execute(
            &files,
            &journal,
            original,
            WorkspaceTransactionAction::Apply,
            || true,
        )
        .await
        .unwrap_err();
        assert_eq!(error.recovery, RecoveryState::Pending);
        assert_eq!(files.mutations.load(Ordering::Relaxed), 0);
        assert_eq!(capture(&files, &roots, || true).await.unwrap(), before);
        let replacement = journal.record.lock().unwrap().clone().unwrap();
        assert_eq!(replacement.generation, "c".repeat(32));
        assert_eq!(replacement.progress, TransactionProgress::default());
        assert_eq!(journal.advances.load(Ordering::Relaxed), 0);
    });
}

#[test]
fn unchanged_apply_failure_never_cancels_a_replacement_journal() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        let replacement = JournalRecord {
            id: "operation".into(),
            generation: "c".repeat(32),
            revision: 1,
            plan: plan.clone(),
            progress: TransactionProgress::default(),
        };
        *journal.record.lock().unwrap() = Some(replacement.clone());
        files
            .memory
            .write_bytes("before/文.bin", b"external edit")
            .await
            .unwrap();
        let before = capture(&files, &plan.roots, || true).await.unwrap();
        let original = JournalRecord {
            generation: "a".repeat(32),
            ..replacement.clone()
        };
        let error = execute(
            &files,
            &journal,
            original,
            WorkspaceTransactionAction::Apply,
            || true,
        )
        .await
        .unwrap_err();
        assert_eq!(error.recovery, RecoveryState::Pending);
        assert_eq!(journal.record.lock().unwrap().as_ref(), Some(&replacement));
        assert_eq!(journal.advances.load(Ordering::Relaxed), 0);
        assert_eq!(files.mutations.load(Ordering::Relaxed), 0);
        assert_eq!(capture(&files, &plan.roots, || true).await.unwrap(), before);
    });
}

#[test]
fn a_lost_phase_reply_never_recovers_or_reports_success_for_a_replacement_journal() {
    block_on(async {
        for phase in [TransactionPhase::Prepared, TransactionPhase::Committed] {
            let files = Files::new();
            let journal = Journal::default();
            let plan = prepare(
                &files,
                "root-identity".into(),
                vec![FileOperation::Create {
                    path: "new".into(),
                    overwrite: false,
                    ignore_if_exists: false,
                }],
                || true,
            )
            .await
            .unwrap();
            let replacement = JournalRecord {
                id: "operation".into(),
                generation: "d".repeat(32),
                revision: 2,
                progress: TransactionProgress {
                    phase,
                    completed: usize::from(phase == TransactionPhase::Committed),
                    ..TransactionProgress::default()
                },
                plan: plan.clone(),
            };
            replacement.validate().unwrap();
            journal.fail_advance.store(1, Ordering::Relaxed);
            *journal.replacement_after_failed_advance.lock().unwrap() = Some(replacement.clone());
            let error = apply(&files, &journal, plan.clone(), || true)
                .await
                .unwrap_err();
            assert_eq!(error.recovery, RecoveryState::Pending);
            assert_eq!(journal.record.lock().unwrap().as_ref(), Some(&replacement));
            assert_eq!(journal.advances.load(Ordering::Relaxed), 1);
            assert_eq!(files.mutations.load(Ordering::Relaxed), 0);
            assert_eq!(
                capture(&files, &plan.roots, || true).await.unwrap(),
                plan.before
            );
        }
    });
}
