use super::*;
use openwebide_core::workspace_transaction::{
    self as transaction, FileOperation, JournalRecord, PreparedJournal, TransactionPhase,
    TransactionProgress, WorkspaceJournal, WorkspacePlan,
};
use std::{cell::Cell, future::Future, sync::Mutex};
mod creations;

impl ResourceFiles {
    /// Invoke the real native primitive, with an interruption at an internal
    /// durability boundary after the caller's actual admission and active intent.
    pub(crate) fn create_resource_observed_for_test(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
        checkpoint: impl FnMut() -> io::Result<()>,
    ) -> Result<(), String> {
        self.authorize(scope).map_err(text)?;
        self.create_owned_observed(ordinal, &workspace(path)?, kind, checkpoint)
            .map_err(text)
    }
}

fn owns_workspace() -> bool {
    true
}

const ID: &str = "0123456789abcdef0123456789abcdef";
fn scope() -> ResourceScope {
    ResourceScope {
        transaction: ID.into(),
        fingerprint: "a".repeat(64),
    }
}
fn files(directory: &tempfile::TempDir) -> ResourceFiles {
    ResourceFiles::new(
        &File::open(directory.path()).unwrap(),
        "physical-root",
        scope(),
    )
    .unwrap()
}
fn base() -> String {
    format!(".openwebide/recovery-{ID}")
}

#[test]
fn owned_area_resumes_partial_initialization_and_every_empty_cleanup_suffix() {
    for removed in 0..=4 {
        let directory = tempfile::tempdir().unwrap();
        let files = files(&directory);
        let area = files.area(true).unwrap().unwrap();
        for folder in ["objects", "new", "receipts", "anchors"]
            .iter()
            .take(removed)
        {
            area.directory("")
                .unwrap()
                .remove(folder, VfsEntryKind::Directory)
                .unwrap();
        }
        futures::executor::block_on(files.finish_retained(&files.scope)).unwrap();
        futures::executor::block_on(files.finish_retained(&files.scope)).unwrap();
        assert!(
            std::fs::read_dir(directory.path().join(".openwebide"))
                .unwrap()
                .next()
                .is_none()
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let files = files(&directory);
    files
        .root
        .create(".openwebide", VfsEntryKind::Directory, true)
        .unwrap();
    files
        .root
        .create(&format!("{}.init", base()), VfsEntryKind::Directory, true)
        .unwrap();
    files
        .root
        .write_new(&format!("{}.owner.part", base()), b"{")
        .unwrap();
    files.area(true).unwrap().unwrap();
    let owner = files.root.read(&format!("{}.owner", base()), 8192).unwrap();
    files
        .root
        .write_new(&format!("{}.owner.part", base()), &owner[..owner.len() / 2])
        .unwrap();
    futures::executor::block_on(files.finish_retained(&files.scope)).unwrap();
    assert!(
        std::fs::read_dir(directory.path().join(".openwebide"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn collisions_replacements_and_unknown_payloads_are_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let files = files(&directory);
    std::fs::create_dir_all(directory.path().join(base())).unwrap();
    assert!(!files.supports_resource_moves());
    assert!(files.area(true).is_err());
    assert!(directory.path().join(base()).is_dir());
    std::fs::remove_dir(directory.path().join(base())).unwrap();
    files.area(true).unwrap().unwrap();
    std::fs::rename(
        directory.path().join(base()),
        directory.path().join("original-control"),
    )
    .unwrap();
    std::fs::create_dir(directory.path().join(base())).unwrap();
    assert!(files.area(true).is_err());
    std::fs::remove_dir(directory.path().join(base())).unwrap();
    std::fs::rename(
        directory.path().join("original-control"),
        directory.path().join(base()),
    )
    .unwrap();
    std::fs::write(directory.path().join(base()).join("unexpected"), "external").unwrap();
    assert!(futures::executor::block_on(files.finish_retained(&files.scope)).is_err());
    assert_eq!(
        std::fs::read(directory.path().join(base()).join("unexpected")).unwrap(),
        b"external"
    );
}

#[test]
fn creation_receipts_reject_external_empty_files_and_changed_contents() {
    let directory = tempfile::tempdir().unwrap();
    let files = files(&directory);
    files
        .create_owned(0, "created", VfsEntryKind::File)
        .unwrap();
    let modified = std::fs::metadata(directory.path().join("created"))
        .unwrap()
        .modified()
        .unwrap();
    files
        .create_owned(0, "created", VfsEntryKind::File)
        .unwrap();
    std::fs::rename(
        directory.path().join("created"),
        directory.path().join("original"),
    )
    .unwrap();
    std::fs::write(directory.path().join("created"), []).unwrap();
    assert!(
        !files
            .created_matches(0, "created", VfsEntryKind::File)
            .unwrap()
    );
    assert!(
        files
            .create_owned(0, "created", VfsEntryKind::File)
            .is_err()
    );
    assert!(
        files
            .remove_created(0, "created", VfsEntryKind::File)
            .is_err()
    );
    std::fs::remove_file(directory.path().join("created")).unwrap();
    std::fs::rename(
        directory.path().join("original"),
        directory.path().join("created"),
    )
    .unwrap();
    std::fs::write(directory.path().join("created"), "later contents").unwrap();
    assert!(
        files
            .remove_created(0, "created", VfsEntryKind::File)
            .is_err()
    );
    assert_eq!(
        std::fs::read(directory.path().join("created")).unwrap(),
        b"later contents"
    );
    std::fs::write(directory.path().join("created"), []).unwrap();
    File::options()
        .write(true)
        .open(directory.path().join("created"))
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    files
        .remove_created(0, "created", VfsEntryKind::File)
        .unwrap();
    assert!(
        files
            .removed_matches(0, "created", VfsEntryKind::File)
            .unwrap()
    );
    futures::executor::block_on(files.finish_retained(&files.scope)).unwrap();
}

#[test]
fn captured_descriptor_never_uses_a_replaced_root_path_or_link_parent() {
    let parent = tempfile::tempdir().unwrap();
    let original = parent.path().join("project");
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join("source"), "original").unwrap();
    let files = ResourceFiles::new(&File::open(&original).unwrap(), "root", scope()).unwrap();
    std::fs::rename(&original, parent.path().join("moved-root")).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join("source"), "replacement").unwrap();
    futures::executor::block_on(files.transfer_resource(
        &files.scope,
        &ResourceLocation::Workspace("source".into()),
        &ResourceLocation::Retained(0),
        VfsEntryKind::File,
    ))
    .unwrap();
    assert_eq!(
        std::fs::read(original.join("source")).unwrap(),
        b"replacement"
    );
    assert_eq!(
        futures::executor::block_on(files.retained_read_bytes(&files.scope, "0")).unwrap(),
        b"original"
    );
    std::os::unix::fs::symlink(&original, parent.path().join("moved-root/alias")).unwrap();
    assert!(
        files
            .create_owned(1, "alias/new", VfsEntryKind::File)
            .is_err()
    );
    assert!(!original.join("new").exists());
    let mut foreign = files.scope.clone();
    foreign.fingerprint = "b".repeat(64);
    assert!(futures::executor::block_on(files.retained_list(&foreign, "")).is_err());
}

#[tokio::test]
async fn normal_vfs_never_exposes_private_payloads_through_paths_or_aliases() {
    use openwebide_core::{Vfs, vfs::SearchOptions};
    let directory = tempfile::tempdir().unwrap();
    let files = files(&directory);
    let area = files.area(true).unwrap().unwrap();
    area.directory("objects")
        .unwrap()
        .write_new("0", b"private needle")
        .unwrap();
    std::fs::write(
        directory.path().join(".openwebide/recovery-notes"),
        "public needle",
    )
    .unwrap();
    std::os::unix::fs::symlink(".openwebide", directory.path().join("alias")).unwrap();
    let vfs = super::super::NativeFsVfs {
        root: directory.path().canonicalize().unwrap(),
    };
    for path in [
        format!("{}/objects/0", base()),
        format!("alias/recovery-{ID}/objects/0"),
    ] {
        assert!(vfs.canonicalize(&path).await.is_err());
        assert!(vfs.read(&path).await.is_err());
        assert!(vfs.write(&path, "changed").await.is_err());
        assert!(vfs.delete(&path).await.is_err());
        assert!(vfs.copy(&path, "copied").await.is_err());
        assert!(
            vfs.create(&format!("{path}-new"), VfsEntryKind::File)
                .await
                .is_err()
        );
    }
    for path in [".openwebide", "alias"] {
        let listing = vfs.list(path).await.unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].name, "recovery-notes");
    }
    let hits = vfs
        .search_content(
            "needle",
            "",
            SearchOptions {
                include_ignored: true,
            },
        )
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|hit| hit.text == "public needle"));
    assert_eq!(
        area.directory("objects").unwrap().read("0", 8192).unwrap(),
        b"private needle"
    );
}

struct Journal {
    record: Mutex<Option<JournalRecord>>,
    count: Cell<usize>,
    fail: Cell<usize>,
    after: bool,
    pause_creation: Cell<Option<usize>>,
}
impl Journal {
    fn new(fail: usize, after: bool) -> Self {
        Self {
            record: Mutex::new(None),
            count: Cell::new(0),
            fail: Cell::new(fail),
            after,
            pause_creation: Cell::new(None),
        }
    }
}
impl WorkspaceJournal for Journal {
    async fn prepare(&self, plan: &WorkspacePlan) -> Result<PreparedJournal, String> {
        let fingerprint = plan.fingerprint()?;
        *self.record.lock().unwrap() = Some(JournalRecord {
            id: ID.into(),
            generation: "c".repeat(32),
            revision: 1,
            plan: plan.clone(),
            progress: TransactionProgress::default(),
        });
        Ok(PreparedJournal {
            id: ID.into(),
            generation: "c".repeat(32),
            revision: 1,
            fingerprint,
        })
    }
    async fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &TransactionProgress,
    ) -> Result<u64, String> {
        let count = self.count.get() + 1;
        self.count.set(count);
        let fail = count == self.fail.get();
        if fail && !self.after {
            return Err("Lost before journal acknowledgement".into());
        }
        let next_revision = {
            let mut record = self.record.lock().unwrap();
            let record = record.as_mut().unwrap();
            assert_eq!(record.id, id);
            assert_eq!(record.generation, generation);
            assert_eq!(record.revision, revision);
            record.validate_transition(progress)?;
            record.progress = progress.clone();
            record.revision += 1;
            record.revision
        };
        if progress.active && self.pause_creation.get() == Some(progress.completed) {
            futures::future::pending::<()>().await;
        }
        if fail {
            return Err("Lost after journal acknowledgement".into());
        }
        Ok(next_revision)
    }
    async fn load(&self, _id: Option<&str>) -> Result<Option<JournalRecord>, String> {
        Ok(self.record.lock().unwrap().clone())
    }
}

struct FaultFiles {
    files: ResourceFiles,
    count: Cell<usize>,
    fail: Cell<usize>,
    after: bool,
    pause: Cell<usize>,
}
impl FaultFiles {
    async fn mutate<T>(
        &self,
        operation: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let count = self.count.get() + 1;
        self.count.set(count);
        let fail = count == self.fail.get();
        if fail && !self.after {
            return Err("Lost before native primitive".into());
        }
        let result = operation.await?;
        if count == self.pause.get() {
            futures::future::pending::<()>().await;
        }
        if fail {
            return Err("Lost after native primitive".into());
        }
        Ok(result)
    }
}
impl WorkspaceEntries for FaultFiles {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.files.canonicalize(path).await
    }
    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, String> {
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
impl TransactionFiles for FaultFiles {
    fn root_identity(&self) -> &str {
        self.files.root_identity()
    }
    fn supports_resource_moves(&self) -> bool {
        self.files.supports_resource_moves()
    }
    async fn original_resource_matches(
        &self,
        scope: &ResourceScope,
        location: &ResourceLocation,
        suffix: &str,
        original: &str,
        mutation: workspace_transaction::OriginalMutation,
    ) -> Result<bool, String> {
        self.files
            .original_resource_matches(scope, location, suffix, original, mutation)
            .await
    }
    async fn created_resource_matches_at(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        location: &ResourceLocation,
        suffix: &str,
        mutation: workspace_transaction::OriginalMutation,
    ) -> Result<bool, String> {
        self.files
            .created_resource_matches_at(scope, ordinal, location, suffix, mutation)
            .await
    }
    async fn reconcile_resource_creation(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.files
            .reconcile_resource_creation(scope, ordinal, path, kind)
            .await
    }
    async fn replace_atomic(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.files.replace_atomic(path, bytes).await
    }
    async fn remove_entry(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        self.files.remove_entry(path, kind).await
    }
    async fn create_resource(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.mutate(self.files.create_resource(scope, ordinal, path, kind))
            .await
    }
    async fn created_resource_matches(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<bool, String> {
        self.files
            .created_resource_matches(scope, ordinal, path, kind)
            .await
    }
    async fn remove_created_resource(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.mutate(
            self.files
                .remove_created_resource(scope, ordinal, path, kind),
        )
        .await
    }
    async fn removed_resource_matches(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<bool, String> {
        self.files
            .removed_resource_matches(scope, ordinal, path, kind)
            .await
    }
    async fn retained_canonicalize(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<String, String> {
        self.files.retained_canonicalize(scope, path).await
    }
    async fn retained_list(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<Vec<FileEntry>, String> {
        self.files.retained_list(scope, path).await
    }
    async fn retained_read_bytes(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<Vec<u8>, String> {
        self.files.retained_read_bytes(scope, path).await
    }
    async fn transfer_resource(
        &self,
        scope: &ResourceScope,
        from: &ResourceLocation,
        to: &ResourceLocation,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.mutate(self.files.transfer_resource(scope, from, to, kind))
            .await
    }
    async fn remove_retained_entry(
        &self,
        scope: &ResourceScope,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.mutate(self.files.remove_retained_entry(scope, path, kind))
            .await
    }
    async fn finish_retained(&self, scope: &ResourceScope) -> Result<(), String> {
        self.mutate(self.files.finish_retained(scope)).await
    }
}

async fn fixture() -> (tempfile::TempDir, FaultFiles, WorkspacePlan) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("source")).unwrap();
    std::fs::create_dir_all(directory.path().join("target")).unwrap();
    std::fs::write(directory.path().join("source/file"), [0, 255, 13, 10]).unwrap();
    std::fs::write(directory.path().join("target/prior"), "old target").unwrap();
    std::fs::write(directory.path().join("old"), "deleted").unwrap();
    std::fs::hard_link(
        directory.path().join("source/file"),
        directory.path().join("outside-link"),
    )
    .unwrap();
    let mut files = files(&directory);
    let mut plan = transaction::prepare(
        &files,
        files.identity.clone(),
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
        owns_workspace,
    )
    .await
    .unwrap();
    plan.version = 2;
    plan.validate().unwrap();
    files.scope.fingerprint = plan.fingerprint().unwrap();
    (
        directory,
        FaultFiles {
            files,
            count: Cell::new(0),
            fail: Cell::new(0),
            after: false,
            pause: Cell::new(0),
        },
        plan,
    )
}

async fn interrupted_fixture() -> (
    tempfile::TempDir,
    FaultFiles,
    WorkspacePlan,
    Journal,
    JournalRecord,
) {
    use futures::FutureExt;
    let (directory, files, plan) = fixture().await;
    let journal = Journal::new(0, false);
    files.pause.set(
        transaction::ResourceScript::prepare(&plan)
            .unwrap()
            .steps
            .len(),
    );
    assert!(
        transaction::apply(&files, &journal, plan.clone(), owns_workspace)
            .now_or_never()
            .is_none()
    );
    let record = journal.load(None).await.unwrap().unwrap();
    assert!(record.progress.active);
    assert_eq!(record.progress.phase, TransactionPhase::Applying);
    files.pause.set(0);
    (directory, files, plan, journal, record)
}

#[tokio::test]
async fn native_inverse_interruptions_preserve_creation_receipts_and_original_objects() {
    let (_directory, files, _plan, journal, record) = interrupted_fixture().await;
    let initial = (files.count.get(), journal.count.get());
    transaction::recover(&files, &journal, record, owns_workspace)
        .await
        .unwrap();
    let totals = (
        files.count.get() - initial.0,
        journal.count.get() - initial.1,
    );
    for (database, total) in [(false, totals.0), (true, totals.1)] {
        for after in [false, true] {
            for offset in 1..=total {
                let (directory, mut files, plan, mut journal, record) = interrupted_fixture().await;
                let link = files.files.root.identity("outside-link").unwrap();
                if database {
                    journal.fail.set(journal.count.get() + offset);
                    journal.after = after;
                } else {
                    files.fail.set(files.count.get() + offset);
                    files.after = after;
                }
                assert!(
                    transaction::recover(&files, &journal, record, owns_workspace)
                        .await
                        .is_err(),
                    "database={database} offset={offset} after={after}"
                );
                files.fail.set(0);
                journal.fail.set(0);
                files.files = ResourceFiles::new(
                    &File::open(directory.path()).unwrap(),
                    &plan.root,
                    files.files.scope.clone(),
                )
                .unwrap();
                let record = journal.load(None).await.unwrap().unwrap();
                let restored = transaction::recover(&files, &journal, record, owns_workspace)
                    .await
                    .unwrap_or_else(|error| {
                        panic!("database={database} offset={offset} after={after}: {error:?}")
                    });
                assert_eq!(restored.progress.phase, TransactionPhase::Restored);
                assert_eq!(
                    transaction::capture(&files, &plan.roots, owns_workspace)
                        .await
                        .unwrap(),
                    plan.before
                );
                assert_eq!(files.files.root.identity("source/file").unwrap(), link);
            }
        }
    }
}

#[tokio::test]
async fn native_journal_recovers_every_primitive_and_database_acknowledgement() {
    use std::os::unix::fs::MetadataExt;
    let (_directory, files, plan) = fixture().await;
    let journal = Journal::new(0, false);
    transaction::apply(&files, &journal, plan, owns_workspace)
        .await
        .unwrap();
    let mutations = files.count.get();
    let acknowledgements = journal.count.get();
    for (database, total) in [(false, mutations), (true, acknowledgements)] {
        for after in [false, true] {
            for fail in 0..=total {
                let (directory, mut files, plan) = fixture().await;
                let originals = ["source", "source/file", "target", "target/prior", "old"]
                    .map(|path| (path, files.files.root.identity(path).unwrap()));
                files.after = after;
                files.fail.set(if database { 0 } else { fail });
                let journal = Journal::new(if database { fail } else { 0 }, after);
                let _ = transaction::apply(&files, &journal, plan.clone(), owns_workspace).await;
                let record = journal.load(None).await.unwrap().unwrap();
                let decided = record.progress.commits_editor();
                files.fail.set(0);
                journal.fail.set(0);
                // Reacquire every filesystem descriptor as after a process restart.
                files.files = ResourceFiles::new(
                    &File::open(directory.path()).unwrap(),
                    &plan.root,
                    files.files.scope.clone(),
                )
                .unwrap();
                let record = if matches!(
                    record.progress.phase,
                    TransactionPhase::Committed | TransactionPhase::Restored
                ) {
                    record
                } else {
                    transaction::recover(&files, &journal, record, owns_workspace)
                        .await
                        .unwrap_or_else(|error| {
                            panic!("database={database} fail={fail} after={after}: {error:?}")
                        })
                };
                assert_eq!(
                    record.progress.phase,
                    if decided {
                        TransactionPhase::Committed
                    } else {
                        TransactionPhase::Restored
                    }
                );
                assert_eq!(
                    transaction::capture(&files, &plan.roots, owns_workspace)
                        .await
                        .unwrap(),
                    if decided { plan.after } else { plan.before },
                    "database={database} fail={fail} after={after}"
                );
                if !decided {
                    for (path, identity) in originals {
                        assert_eq!(
                            files.files.root.identity(path).unwrap(),
                            identity,
                            "native identity at {path}"
                        );
                    }
                    assert_eq!(
                        std::fs::metadata(directory.path().join("source/file"))
                            .unwrap()
                            .ino(),
                        std::fs::metadata(directory.path().join("outside-link"))
                            .unwrap()
                            .ino()
                    );
                }
                if directory.path().join(".openwebide").exists() {
                    assert!(
                        std::fs::read_dir(directory.path().join(".openwebide"))
                            .unwrap()
                            .next()
                            .is_none()
                    );
                }
            }
        }
    }
}
