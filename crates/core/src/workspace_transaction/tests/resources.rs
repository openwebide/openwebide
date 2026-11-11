use super::*;

struct ResourceFiles {
    files: Files,
    journal: Arc<Journal>,
}
impl ResourceFiles {
    fn authorize(&self, scope: &ResourceScope) -> Result<String, String> {
        self.files.access()?;
        let guard = self.journal.record.lock().unwrap();
        let record = guard.as_ref().ok_or("Missing owned resource journal")?;
        if record.id != scope.transaction || record.plan.fingerprint()? != scope.fingerprint {
            return Err("Retained originals belong to another journal".into());
        }
        Ok(format!("retained/{}", scope.transaction))
    }
    fn path(scope: &ResourceScope, location: &ResourceLocation) -> String {
        match location {
            ResourceLocation::Workspace(path) => path.clone(),
            ResourceLocation::Retained(slot) => format!("retained/{}/{slot}", scope.transaction),
        }
    }
    fn cleanup_decision(&self) {
        assert_eq!(
            self.journal
                .record
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .progress
                .phase,
            TransactionPhase::Finalizing
        );
    }
}
impl WorkspaceEntries for ResourceFiles {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.files.canonicalize(path).await
    }
    async fn list(&self, path: &str) -> Result<Vec<crate::FileEntry>, String> {
        self.files.list(path).await
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.files.read_bytes(path).await
    }
    async fn create(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        self.files.create(path, kind).await
    }
    async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.files.write_bytes(path, bytes).await
    }
    async fn delete(&self, path: &str) -> Result<(), String> {
        self.files.delete(path).await
    }
}
impl TransactionFiles for ResourceFiles {
    fn root_identity(&self) -> &str {
        &self.files.identity
    }
    fn supports_resource_moves(&self) -> bool {
        true
    }
    async fn replace_atomic(&self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        panic!("Resource journals must never reconstruct originals from bytes")
    }
    async fn remove_entry(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        self.files.remove_entry(path, kind).await
    }
    async fn retained_canonicalize(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<String, String> {
        let prefix = self.authorize(scope)?;
        let value = self
            .files
            .memory
            .canonicalize(&format!("{prefix}/{path}"))
            .await
            .map_err(|error| error.to_string())?;
        if value == prefix {
            Ok(String::new())
        } else {
            value
                .strip_prefix(&format!("{prefix}/"))
                .map(str::to_owned)
                .ok_or_else(|| "Retained path escaped its area".into())
        }
    }
    async fn retained_list(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<Vec<crate::FileEntry>, String> {
        let prefix = self.authorize(scope)?;
        self.files
            .memory
            .list(&format!("{prefix}/{path}"))
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|mut entry| {
                entry.path = entry
                    .path
                    .strip_prefix(&format!("{prefix}/"))
                    .ok_or("Invalid retained child path")?
                    .into();
                Ok(entry)
            })
            .collect()
    }
    async fn retained_read_bytes(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<Vec<u8>, String> {
        let prefix = self.authorize(scope)?;
        self.files
            .memory
            .read_bytes(&format!("{prefix}/{path}"))
            .await
            .map_err(|error| error.to_string())
    }
    async fn transfer_resource(
        &self,
        scope: &ResourceScope,
        from: &ResourceLocation,
        to: &ResourceLocation,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        let prefix = self.authorize(scope)?;
        let count = self.files.before()?;
        if matches!(to, ResourceLocation::Retained(_)) {
            self.files
                .memory
                .create("retained", VfsEntryKind::Directory)
                .await
                .unwrap();
            self.files
                .memory
                .create(&prefix, VfsEntryKind::Directory)
                .await
                .unwrap();
        }
        self.files
            .memory
            .move_atomic(&Self::path(scope, from), &Self::path(scope, to), kind)
            .await
            .map_err(|error| error.to_string())?;
        self.files.after(count).await
    }
    async fn remove_retained_entry(
        &self,
        scope: &ResourceScope,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.cleanup_decision();
        let prefix = self.authorize(scope)?;
        self.files
            .remove_entry(&format!("{prefix}/{path}"), kind)
            .await
    }
    async fn finish_retained(&self, scope: &ResourceScope) -> Result<(), String> {
        self.cleanup_decision();
        let prefix = self.authorize(scope)?;
        if self
            .files
            .memory
            .list("retained")
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.path == prefix)
        {
            self.files
                .remove_entry(&prefix, VfsEntryKind::Directory)
                .await?;
        }
        Ok(())
    }
}

async fn fixture() -> (ResourceFiles, Arc<Journal>, WorkspacePlan) {
    let journal = Arc::new(Journal::default());
    let files = ResourceFiles {
        files: Files::new(),
        journal: journal.clone(),
    };
    files
        .files
        .memory
        .write_bytes("source/file", &[0, 255, 1])
        .await
        .unwrap();
    files
        .files
        .memory
        .write("target/prior", "old target")
        .await
        .unwrap();
    files.files.memory.write("old", "deleted").await.unwrap();
    let mut plan = prepare(
        &files,
        files.root_identity().into(),
        vec![
            FileOperation::Rename {
                from: "source".into(),
                to: "target".into(),
                overwrite: true,
                ignore_if_exists: false,
            },
            FileOperation::Create {
                path: "target/new/empty".into(),
                overwrite: false,
                ignore_if_exists: false,
            },
            FileOperation::Delete {
                path: "old".into(),
                recursive: false,
                ignore_if_missing: false,
            },
            FileOperation::Create {
                path: "target/file".into(),
                overwrite: true,
                ignore_if_exists: false,
            },
        ],
        || true,
    )
    .await
    .unwrap();
    plan.version = 2;
    plan.validate().unwrap();
    (files, journal, plan)
}

#[test]
fn resource_journals_commit_and_dispose_originals_only_after_the_decision() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        let record = apply(&files, journal.as_ref(), plan.clone(), || true)
            .await
            .unwrap();
        assert_eq!(record.progress.phase, TransactionPhase::Committed);
        assert_eq!(
            capture(&files, &plan.roots, || true).await.unwrap(),
            plan.after
        );
        assert!(
            files
                .files
                .memory
                .list("retained")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            ResourceScript::prepare(&plan).unwrap().steps.len()
                < files.files.mutations.load(Ordering::Relaxed)
        );
    });
}

#[test]
fn every_resource_filesystem_failure_restores_or_resumes_the_durable_decision() {
    block_on(async {
        let (baseline, journal, plan) = fixture().await;
        apply(&baseline, journal.as_ref(), plan, || true)
            .await
            .unwrap();
        let total = baseline.files.mutations.load(Ordering::Relaxed);
        for after in [false, true] {
            for failure in 1..=total {
                let (files, journal, plan) = fixture().await;
                files.files.fail_at.store(failure, Ordering::Relaxed);
                files.files.fail_after.store(after, Ordering::Relaxed);
                let result = apply(&files, journal.as_ref(), plan.clone(), || true).await;
                let mut record = journal.load(None).await.unwrap().unwrap();
                if record.progress.phase == TransactionPhase::Finalizing {
                    assert!(result.is_err());
                    assert!(record.progress.commits_editor());
                    record = recover(&files, journal.as_ref(), record, || true)
                        .await
                        .unwrap();
                    assert_eq!(record.progress.phase, TransactionPhase::Committed);
                } else {
                    assert_eq!(
                        record.progress.phase,
                        TransactionPhase::Restored,
                        "failure={failure}, after={after}"
                    );
                }
                let expected = if record.progress.phase == TransactionPhase::Committed {
                    &plan.after
                } else {
                    &plan.before
                };
                assert_eq!(
                    &capture(&files, &plan.roots, || true).await.unwrap(),
                    expected,
                    "failure={failure}, after={after}"
                );
                assert!(
                    files
                        .files
                        .memory
                        .list("retained")
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        }
    });
}

#[test]
fn every_resource_journal_failure_reconciles_before_any_further_mutation() {
    block_on(async {
        let (baseline, journal, plan) = fixture().await;
        apply(&baseline, journal.as_ref(), plan, || true)
            .await
            .unwrap();
        let total = journal.advances.load(Ordering::Relaxed);
        for after in [false, true] {
            for failure in 1..=total {
                let (files, journal, plan) = fixture().await;
                journal.fail_advance.store(failure, Ordering::Relaxed);
                journal.fail_advance_after.store(after, Ordering::Relaxed);
                let _ = apply(&files, journal.as_ref(), plan.clone(), || true).await;
                let mut record = journal.load(None).await.unwrap().unwrap();
                let decided = record.progress.commits_editor();
                if !matches!(
                    record.progress.phase,
                    TransactionPhase::Committed | TransactionPhase::Restored
                ) {
                    record = recover(&files, journal.as_ref(), record, || true)
                        .await
                        .unwrap();
                }
                assert_eq!(
                    record.progress.phase,
                    if decided {
                        TransactionPhase::Committed
                    } else {
                        TransactionPhase::Restored
                    },
                    "failure={failure}, after={after}"
                );
                let expected = if decided { &plan.after } else { &plan.before };
                assert_eq!(
                    &capture(&files, &plan.roots, || true).await.unwrap(),
                    expected
                );
                assert!(
                    files
                        .files
                        .memory
                        .list("retained")
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        }
    });
}

#[test]
fn finalization_never_rolls_back_later_workspace_edits() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        files.files.fail_at.store(
            ResourceScript::prepare(&plan).unwrap().steps.len() + 1,
            Ordering::Relaxed,
        );
        assert_eq!(
            apply(&files, journal.as_ref(), plan, || true)
                .await
                .unwrap_err()
                .recovery,
            RecoveryState::Pending
        );
        let record = journal.load(None).await.unwrap().unwrap();
        assert_eq!(record.progress.phase, TransactionPhase::Finalizing);
        files
            .files
            .memory
            .write("target/file", "later external content")
            .await
            .unwrap();
        let committed = recover(&files, journal.as_ref(), record, || true)
            .await
            .unwrap();
        assert_eq!(committed.progress.phase, TransactionPhase::Committed);
        assert_eq!(
            files.files.memory.read("target/file").await.unwrap(),
            "later external content"
        );
    });
}

#[test]
fn ambiguous_resource_transfer_and_changed_originals_are_preserved_on_recovery() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        files.files.pause_after.store(1, Ordering::Relaxed);
        assert!(
            apply(&files, journal.as_ref(), plan.clone(), || true)
                .now_or_never()
                .is_none()
        );
        let record = journal.load(None).await.unwrap().unwrap();
        assert!(record.progress.active);
        files
            .files
            .memory
            .write("retained/operation/0/prior", "external retained content")
            .await
            .unwrap();
        let count = files.files.mutations.load(Ordering::Relaxed);
        assert_eq!(
            recover(&files, journal.as_ref(), record.clone(), || true)
                .await
                .unwrap_err()
                .recovery,
            RecoveryState::Pending
        );
        assert_eq!(files.files.mutations.load(Ordering::Relaxed), count);
        assert_eq!(
            files
                .files
                .memory
                .read("retained/operation/0/prior")
                .await
                .unwrap(),
            "external retained content"
        );
        files
            .files
            .memory
            .write("retained/operation/0/prior", "old target")
            .await
            .unwrap();
        let restored = recover(&files, journal.as_ref(), record, || true)
            .await
            .unwrap();
        assert_eq!(restored.progress.phase, TransactionPhase::Restored);
        assert_eq!(
            capture(&files, &plan.roots, || true).await.unwrap(),
            plan.before
        );
    });
}

#[test]
fn unsupported_resource_adapters_reject_before_creating_a_journal() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        assert_eq!(
            apply(&files.files, journal.as_ref(), plan, || true)
                .await
                .unwrap_err()
                .recovery,
            RecoveryState::Unchanged
        );
        assert!(journal.load(None).await.unwrap().is_none());
        assert_eq!(files.files.mutations.load(Ordering::Relaxed), 0);
    });
}

async fn interrupted_full_script() -> (
    ResourceFiles,
    Arc<Journal>,
    WorkspacePlan,
    JournalRecord,
    usize,
) {
    let (files, journal, plan) = fixture().await;
    let count = ResourceScript::prepare(&plan).unwrap().steps.len();
    files.files.pause_after.store(count, Ordering::Relaxed);
    assert!(
        apply(&files, journal.as_ref(), plan.clone(), || true)
            .now_or_never()
            .is_none()
    );
    let record = journal.load(None).await.unwrap().unwrap();
    assert!(record.progress.active);
    (files, journal, plan, record, count)
}

#[test]
fn every_interrupted_inverse_resource_primitive_resumes_without_repeating_a_move() {
    block_on(async {
        for offset in 1..=7 {
            let (files, journal, plan, record, forward) = interrupted_full_script().await;
            files
                .files
                .pause_after
                .store(forward + offset, Ordering::Relaxed);
            assert!(
                recover(&files, journal.as_ref(), record, || true)
                    .now_or_never()
                    .is_none()
            );
            let latest = journal.load(None).await.unwrap().unwrap();
            assert_eq!(latest.progress.phase, TransactionPhase::Reverting);
            assert!(latest.progress.active);
            assert_eq!(latest.progress.rollback_from, Some(forward));
            files.files.pause_after.store(0, Ordering::Relaxed);
            let restored = recover(&files, journal.as_ref(), latest, || true)
                .await
                .unwrap();
            assert_eq!(restored.progress.phase, TransactionPhase::Restored);
            assert_eq!(
                capture(&files, &plan.roots, || true).await.unwrap(),
                plan.before
            );
            assert!(
                files
                    .files
                    .memory
                    .list("retained")
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    });
}

#[test]
fn every_inverse_journal_acknowledgement_reconciles_its_authoritative_cursor() {
    block_on(async {
        let (files, journal, _, record, _) = interrupted_full_script().await;
        files.files.pause_after.store(0, Ordering::Relaxed);
        let before = journal.advances.load(Ordering::Relaxed);
        recover(&files, journal.as_ref(), record, || true)
            .await
            .unwrap();
        let advances = journal.advances.load(Ordering::Relaxed) - before;
        for after in [false, true] {
            for offset in 1..=advances {
                let (files, journal, plan, record, _) = interrupted_full_script().await;
                files.files.pause_after.store(0, Ordering::Relaxed);
                journal.fail_advance.store(
                    journal.advances.load(Ordering::Relaxed) + offset,
                    Ordering::Relaxed,
                );
                journal.fail_advance_after.store(after, Ordering::Relaxed);
                let _ = recover(&files, journal.as_ref(), record, || true).await;
                let latest = journal.load(None).await.unwrap().unwrap();
                let restored = recover(&files, journal.as_ref(), latest, || true)
                    .await
                    .unwrap();
                assert_eq!(
                    restored.progress.phase,
                    TransactionPhase::Restored,
                    "offset={offset}, after={after}"
                );
                assert_eq!(
                    capture(&files, &plan.roots, || true).await.unwrap(),
                    plan.before
                );
                assert!(
                    files
                        .files
                        .memory
                        .list("retained")
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        }
    });
}

#[test]
fn unreviewed_private_objects_are_never_deleted_as_cleanup() {
    block_on(async {
        let (files, journal, plan) = fixture().await;
        files.files.fail_at.store(
            ResourceScript::prepare(&plan).unwrap().steps.len() + 1,
            Ordering::Relaxed,
        );
        apply(&files, journal.as_ref(), plan, || true)
            .await
            .unwrap_err();
        let record = journal.load(None).await.unwrap().unwrap();
        assert_eq!(record.progress.phase, TransactionPhase::Finalizing);
        files
            .files
            .memory
            .write("retained/operation/unreviewed", "preserve this")
            .await
            .unwrap();
        let count = files.files.mutations.load(Ordering::Relaxed);
        assert_eq!(
            recover(&files, journal.as_ref(), record, || true)
                .await
                .unwrap_err()
                .recovery,
            RecoveryState::Pending
        );
        assert_eq!(files.files.mutations.load(Ordering::Relaxed), count);
        assert_eq!(
            files
                .files
                .memory
                .read("retained/operation/unreviewed")
                .await
                .unwrap(),
            "preserve this"
        );
    });
}
