use super::*;
mod cancellation;
mod delegation;
mod language_review;
mod resources;
mod scope;
mod worker;
use crate::rusqlite_db::RusqliteDb;
use futures::executor::block_on;
use openwebide_core::{
    MemoryVfs, Vfs,
    editor::{Document, EditorRecoveryFile, RecoveryScroll},
    vfs::VfsEntryKind,
    workspace_entries::{WorkspaceEntries, parent},
    workspace_transaction::{
        self as workflow, EditorRecoveryPatch, EntryState, FileOperation, TransactionFiles,
        WorkspaceJournal, WorkspacePlan,
    },
};
use std::sync::atomic::{AtomicBool, Ordering};

struct Files {
    memory: MemoryVfs,
    root: String,
}
impl WorkspaceEntries for Files {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.memory
            .canonicalize(path)
            .await
            .map_err(|error| error.to_string())
    }
    async fn list(&self, path: &str) -> Result<Vec<openwebide_core::FileEntry>, String> {
        self.memory
            .list(path)
            .await
            .map_err(|error| error.to_string())
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.memory
            .read_bytes(path)
            .await
            .map_err(|error| error.to_string())
    }
    async fn create(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        self.memory
            .create(path, kind)
            .await
            .map_err(|error| error.to_string())
    }
    async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.memory
            .write_bytes(path, bytes)
            .await
            .map_err(|error| error.to_string())
    }
    async fn delete(&self, path: &str) -> Result<(), String> {
        self.memory
            .delete(path)
            .await
            .map_err(|error| error.to_string())
    }
}
impl TransactionFiles for Files {
    fn root_identity(&self) -> &str {
        &self.root
    }
    async fn replace_atomic(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        WorkspaceEntries::write_bytes(self, path, bytes).await
    }
    async fn remove_entry(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        let entry = self
            .memory
            .list(parent(path))
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or("Entry missing")?;
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
struct Journal<'a, D: Db> {
    store: &'a Store<D>,
    user: UserId,
    project: i64,
    id: String,
    editor: EditorRecoveryPatch,
    change_editor: AtomicBool,
    lose_commit_ack: AtomicBool,
}
impl<D: Db> WorkspaceJournal for Journal<'_, D> {
    async fn prepare(&self, plan: &WorkspacePlan) -> Result<PreparedJournal, String> {
        self.store
            .begin_workspace_transaction(
                self.user,
                self.project,
                &BeginWorkspaceTransaction {
                    language_review: None,
                    id: self.id.clone(),
                    plan: plan.clone(),
                    editor: self.editor.clone(),
                },
                100,
            )
            .await
            .map(|response| response.journal)
            .map_err(|error| error.to_string())
    }
    async fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &TransactionProgress,
    ) -> Result<u64, String> {
        if progress.phase == TransactionPhase::Committed
            && self.change_editor.swap(false, Ordering::Relaxed)
        {
            let record = self
                .store
                .editor_recovery(self.user, self.project)
                .await
                .map_err(|error| error.to_string())?;
            let mut state = record.state;
            state.files[0].document.as_mut().unwrap().text =
                std::sync::Arc::new("newer concurrent draft".into());
            self.store
                .save_editor_recovery(self.user, self.project, record.revision, &state)
                .await
                .map_err(|error| error.to_string())?;
        }
        let response = self
            .store
            .advance_workspace_transaction(
                self.user,
                self.project,
                &AdvanceWorkspaceTransaction {
                    id: id.into(),
                    generation: generation.into(),
                    revision,
                    progress: progress.clone(),
                },
                100,
            )
            .await
            .map_err(|error| error.to_string())?;
        if progress.phase == TransactionPhase::Committed
            && self.lose_commit_ack.swap(false, Ordering::Relaxed)
        {
            return Err("Commit response lost".into());
        }
        Ok(response.revision)
    }
    async fn load(&self, id: Option<&str>) -> Result<Option<JournalRecord>, String> {
        self.store
            .workspace_transaction(self.user, self.project, id)
            .await
            .map(|record| record.map(|record| record.journal))
            .map_err(|error| error.to_string())
    }
}
struct Fixture {
    user: UserId,
    other: UserId,
    project: Project,
    plan: WorkspacePlan,
    editor: EditorRecoveryPatch,
    files: Files,
}
async fn fixture(store: &Store<RusqliteDb>, mode: WorkspaceMode) -> Fixture {
    store.migrate().await.unwrap();
    store.migrate().await.unwrap();
    let user = store
        .insert_user("owner", "hash", UserRole::Admin, 1)
        .await
        .unwrap()
        .id;
    let other = store
        .insert_user("other", "hash", UserRole::User, 1)
        .await
        .unwrap()
        .id;
    let project = store
        .create_project(
            &NewProject {
                name: "workspace".into(),
                mode,
                path: Some("project".into()),
            },
            user,
            1,
        )
        .await
        .unwrap();
    let mut document = Document::new("disk 文😀\r\n");
    document.replace_selections("unsaved ", None).unwrap();
    let before = EditorRecoveryFile {
        path: "a.rs".into(),
        document: Some(document.recovery()),
        scroll: RecoveryScroll::default(),
        read_only: false,
    };
    let mut after = before.clone();
    after.path = "b.rs".into();
    let root = EditorRecoveryRoot::for_project(&project);
    let state = openwebide_core::editor::EditorRecovery {
        format: 1,
        root: Some(root.clone()),
        selected: Some("a.rs".into()),
        files: vec![before.clone()],
    };
    store
        .save_editor_recovery(user, project.id, 0, &state)
        .await
        .unwrap();
    let editor = EditorRecoveryPatch {
        revision: 1,
        root,
        before: vec![before],
        after: vec![after],
        order_before: vec!["a.rs".into()],
        order_after: vec!["b.rs".into()],
        selected_before: Some("a.rs".into()),
        selected_after: Some("b.rs".into()),
    };
    let identity = format!("owner-{}-project-{}", user.get(), project.id);
    let bytes = "disk 文😀\r\n".as_bytes().to_vec();
    let plan = WorkspacePlan {
        version: 1,
        root: identity.clone(),
        operations: vec![FileOperation::Rename {
            from: "a.rs".into(),
            to: "b.rs".into(),
            overwrite: false,
            ignore_if_exists: false,
        }],
        roots: vec!["a.rs".into(), "b.rs".into()],
        before: [
            ("a.rs".into(), EntryState::file(bytes.clone())),
            ("b.rs".into(), EntryState::Missing),
        ]
        .into(),
        after: [
            ("a.rs".into(), EntryState::Missing),
            ("b.rs".into(), EntryState::file(bytes.clone())),
        ]
        .into(),
    };
    let files = Files {
        memory: MemoryVfs::new(),
        root: identity,
    };
    files.memory.write_bytes("a.rs", &bytes).await.unwrap();
    Fixture {
        user,
        other,
        project,
        plan,
        editor,
        files,
    }
}
fn journal<'a>(store: &'a Store<RusqliteDb>, fixture: &Fixture) -> Journal<'a, RusqliteDb> {
    Journal {
        store,
        user: fixture.user,
        project: fixture.project.id,
        id: "a".repeat(32),
        editor: fixture.editor.clone(),
        change_editor: AtomicBool::new(false),
        lose_commit_ack: AtomicBool::new(false),
    }
}

#[test]
fn filesystem_and_editor_recovery_commit_together_in_both_modes() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let adapter = journal(&store, &fixture);
            let result = workflow::apply(&fixture.files, &adapter, fixture.plan.clone(), || true)
                .await
                .unwrap();
            assert_eq!(result.progress.phase, TransactionPhase::Committed);
            assert_eq!(
                fixture.files.memory.read("b.rs").await.unwrap(),
                "disk 文😀\r\n"
            );
            let record = store
                .editor_recovery(fixture.user, fixture.project.id)
                .await
                .unwrap();
            assert_eq!(record.revision, 2);
            assert_eq!(record.state.selected.as_deref(), Some("b.rs"));
            assert_eq!(record.state.files[0], fixture.editor.after[0]);
            let stored = store
                .workspace_transaction(fixture.user, fixture.project.id, Some(&result.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.journal, result);
            store
                .delete_project(fixture.project.id, fixture.user)
                .await
                .unwrap();
            assert!(
                store
                    .db
                    .execute("SELECT 1 FROM workspace_transactions", &[])
                    .await
                    .unwrap()
                    .rows
                    .is_empty()
            );
        }
    });
}
#[test]
fn concurrent_editor_changes_roll_back_files_and_preserve_the_newer_draft() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let adapter = journal(&store, &fixture);
            adapter.change_editor.store(true, Ordering::Relaxed);
            let error = workflow::apply(&fixture.files, &adapter, fixture.plan.clone(), || true)
                .await
                .unwrap_err();
            assert_eq!(error.recovery, workflow::RecoveryState::Restored);
            assert!(error.message.contains("newer editor draft"));
            assert_eq!(
                workflow::capture(&fixture.files, &fixture.plan.roots, || true)
                    .await
                    .unwrap(),
                fixture.plan.before
            );
            let record = store
                .editor_recovery(fixture.user, fixture.project.id)
                .await
                .unwrap();
            assert_eq!(record.revision, 2);
            assert_eq!(record.state.selected.as_deref(), Some("a.rs"));
            assert_eq!(
                record.state.files[0]
                    .document
                    .as_ref()
                    .unwrap()
                    .text
                    .as_str(),
                "newer concurrent draft"
            );
        }
    });
}
#[test]
fn committed_acknowledgement_loss_is_idempotent_for_editor_and_files() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let adapter = journal(&store, &fixture);
            adapter.lose_commit_ack.store(true, Ordering::Relaxed);
            assert_eq!(
                workflow::apply(&fixture.files, &adapter, fixture.plan.clone(), || true)
                    .await
                    .unwrap()
                    .progress
                    .phase,
                TransactionPhase::Committed
            );
            let recovery = store
                .editor_recovery(fixture.user, fixture.project.id)
                .await
                .unwrap();
            assert_eq!(recovery.revision, 2);
            assert_eq!(recovery.state.files[0], fixture.editor.after[0]);
            assert_eq!(
                workflow::capture(&fixture.files, &fixture.plan.roots, || true)
                    .await
                    .unwrap(),
                fixture.plan.after
            );
        }
    });
}
#[test]
fn pending_journals_are_owned_revision_checked_and_cannot_lose_their_project() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let input = BeginWorkspaceTransaction {
                language_review: None,
                id: "a".repeat(32),
                plan: fixture.plan.clone(),
                editor: fixture.editor.clone(),
            };
            let first = store
                .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 100)
                .await
                .unwrap();
            let retry = store
                .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 101)
                .await
                .unwrap();
            assert_eq!(first.journal, retry.journal);
            let mut concurrent = input.clone();
            concurrent.id = "b".repeat(32);
            assert!(matches!(
                store
                    .begin_workspace_transaction(fixture.user, fixture.project.id, &concurrent, 102)
                    .await,
                Err(StorageError::Conflict(_))
            ));
            assert!(matches!(
                store
                    .workspace_transaction(fixture.other, fixture.project.id, Some(&input.id))
                    .await,
                Err(StorageError::NotFound(_))
            ));
            assert!(matches!(
                store
                    .begin_workspace_transaction(fixture.other, fixture.project.id, &input, 100)
                    .await,
                Err(StorageError::NotFound(_))
            ));
            assert!(matches!(
                store.delete_project(fixture.project.id, fixture.user).await,
                Err(StorageError::Conflict(_))
            ));
            store
                .rename_project(fixture.project.id, "renamed", fixture.user)
                .await
                .unwrap();
            let advance = AdvanceWorkspaceTransaction {
                id: input.id.clone(),
                generation: first.journal.generation.clone(),
                revision: 1,
                progress: TransactionProgress {
                    phase: TransactionPhase::Applying,
                    ..TransactionProgress::default()
                },
            };
            store
                .advance_workspace_transaction(fixture.user, fixture.project.id, &advance, 103)
                .await
                .unwrap();
            assert!(matches!(
                store
                    .advance_workspace_transaction(fixture.user, fixture.project.id, &advance, 104)
                    .await,
                Err(StorageError::Conflict(_))
            ));
            let mut forged = advance;
            forged.revision = 2;
            forged.progress.completed = 1;
            assert!(matches!(
                store
                    .advance_workspace_transaction(fixture.user, fixture.project.id, &forged, 105)
                    .await,
                Err(StorageError::InvalidRequest(_))
            ));
        }
    });
}
#[test]
fn durable_active_steps_recover_after_reopening_the_database() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("recovery.sqlite");
            let store = Store::new(RusqliteDb::open(&path).unwrap());
            let fixture = fixture(&store, mode).await;
            let input = BeginWorkspaceTransaction {
                language_review: None,
                id: "a".repeat(32),
                plan: fixture.plan.clone(),
                editor: fixture.editor.clone(),
            };
            let prepared = store
                .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 100)
                .await
                .unwrap();
            let mut progress = TransactionProgress {
                phase: TransactionPhase::Applying,
                ..TransactionProgress::default()
            };
            store
                .advance_workspace_transaction(
                    fixture.user,
                    fixture.project.id,
                    &AdvanceWorkspaceTransaction {
                        id: input.id.clone(),
                        generation: prepared.journal.generation.clone(),
                        revision: 1,
                        progress: progress.clone(),
                    },
                    101,
                )
                .await
                .unwrap();
            progress.active = true;
            store
                .advance_workspace_transaction(
                    fixture.user,
                    fixture.project.id,
                    &AdvanceWorkspaceTransaction {
                        id: input.id.clone(),
                        generation: prepared.journal.generation.clone(),
                        revision: 2,
                        progress,
                    },
                    102,
                )
                .await
                .unwrap();
            fixture
                .files
                .memory
                .create("b.rs", VfsEntryKind::File)
                .await
                .unwrap();
            drop(store);
            let reopened = Store::new(RusqliteDb::open(&path).unwrap());
            reopened.migrate().await.unwrap();
            let pending = reopened
                .workspace_transaction(fixture.user, fixture.project.id, None)
                .await
                .unwrap()
                .unwrap();
            assert!(pending.journal.progress.active);
            let adapter = journal(&reopened, &fixture);
            assert_eq!(
                workflow::recover(&fixture.files, &adapter, pending.journal, || true)
                    .await
                    .unwrap()
                    .progress
                    .phase,
                TransactionPhase::Restored
            );
            assert_eq!(
                workflow::capture(&fixture.files, &fixture.plan.roots, || true)
                    .await
                    .unwrap(),
                fixture.plan.before
            );
            assert_eq!(
                reopened
                    .editor_recovery(fixture.user, fixture.project.id)
                    .await
                    .unwrap()
                    .revision,
                1
            );
            reopened
                .delete_project(fixture.project.id, fixture.user)
                .await
                .unwrap();
        }
    });
}
