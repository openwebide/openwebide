//! Transport fixtures reuse the real transaction engine. SQLite ownership,
//! quotas and native HTTP authority are covered by their adapter contracts.
use super::fake_backend::FakeBackend;
use crate::backend::{Backend, RecoveryError};
use openwebide_core::{
    FileEntry, editor::EditorRecoveryRecord, vfs::VfsEntryKind,
    workspace_entries::WorkspaceEntries, workspace_transaction::*,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
};
#[derive(Default)]
pub struct Harness {
    next_generation: Cell<u128>,
    pub records: RefCell<BTreeMap<(i64, String), WorkspaceTransactionState>>,
    pub commands: RefCell<
        BTreeMap<
            (i64, String),
            openwebide_core::plugins::languages::intelligence::actions::ActionRecord,
        >,
    >,
    pub inspections: RefCell<Vec<companion::FolderInspection>>,
    pub inspection_error: Cell<bool>,
    pub command_journal_error: Cell<bool>,
    pub lose_commit_reply: Cell<bool>,
    pub latest_delay_ms: Cell<i32>,
    pub latest_error: Cell<bool>,
    pub latest_reads: Cell<usize>,
}
fn failure(error: impl ToString) -> RecoveryError {
    RecoveryError::Conflict(error.to_string())
}
pub fn identity(project: i64) -> String {
    openwebide_core::plugins::content_digest(format!("fixture-project:{project}").as_bytes())
}
impl Harness {
    fn generation(&self) -> String {
        let next = self.next_generation.get() + 1;
        self.next_generation.set(next);
        format!("{next:032x}")
    }
    pub fn pending(
        &self,
        project: i64,
        mode: openwebide_core::WorkspaceMode,
        phase: TransactionPhase,
    ) {
        let id = "a".repeat(32);
        let state = WorkspaceTransactionState {
            language_review: None,
            journal: JournalRecord {
                id: id.clone(),
                generation: self.generation(),
                revision: 1,
                plan: WorkspacePlan {
                    version: 2,
                    root: identity(project),
                    operations: vec![],
                    roots: vec![],
                    before: BTreeMap::new(),
                    after: BTreeMap::new(),
                },
                progress: TransactionProgress {
                    phase,
                    rollback_from: matches!(
                        phase,
                        TransactionPhase::Reverting | TransactionPhase::Restored
                    )
                    .then_some(0),
                    ..TransactionProgress::default()
                },
            },
            editor: EditorRecoveryPatch {
                revision: 0,
                root: openwebide_core::editor::EditorRecoveryRoot {
                    mode,
                    path: (mode == openwebide_core::WorkspaceMode::Remote).then(|| "test".into()),
                },
                before: vec![],
                after: vec![],
                order_before: vec![],
                order_after: vec![],
                selected_before: None,
                selected_after: None,
            },
            editor_revision: 0,
        };
        state.journal.validate().unwrap();
        state.editor.validate().unwrap();
        self.records.borrow_mut().insert((project, id), state);
    }
    pub fn command(
        &self,
        project: i64,
        phase: openwebide_core::plugins::languages::intelligence::actions::ActionPhase,
    ) {
        use openwebide_core::plugins::languages::{
            intelligence::{
                self, Query,
                actions::{
                    ActionApproval, ActionPhase, ActionRecord, ActionReview, CommandOutcome,
                    ServerCommand,
                },
                results::WorkspaceEdit,
            },
            source_digest,
        };
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 9;
        prepared.manifest.executable = None;
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.tools.clear();
        prepared.manifest.contributions.languages=serde_json::from_value(serde_json::json!([{"id":"custom","name":"Custom","extensions":["custom"],"features":{"languageServer":{"command":"fixture-lsp","role":"language"}}}])).unwrap();
        let id = "a".repeat(32);
        let record = ActionRecord {
            approval: ActionApproval {
                id: id.clone(),
                prepared,
                binding_revision: 1,
                input: intelligence::Request {
                    version: intelligence::VERSION,
                    client: "window".into(),
                    document: "tab".into(),
                    path: "main.custom".into(),
                    language: "custom".into(),
                    request: 1,
                    revision: 1,
                    source: "source".into(),
                    source_digest: source_digest("source"),
                    query: Query::ReviewAction { id: "b".repeat(32) },
                },
                review: ActionReview {
                    title: "Reviewed action".into(),
                    edit: WorkspaceEdit {
                        files: vec![],
                        changes: vec![],
                    },
                    command: Some(ServerCommand {
                        id: "b".repeat(32),
                        server: "fixture-server".into(),
                        name: "fixture.apply".into(),
                        arguments: vec![],
                    }),
                },
            },
            revision: 1,
            phase,
            outcome: match phase {
                ActionPhase::Completed => Some(CommandOutcome::Completed {
                    result: serde_json::Value::Null,
                }),
                ActionPhase::Rejected => Some(CommandOutcome::Rejected {
                    message: "Unstarted".into(),
                }),
                ActionPhase::Uncertain | ActionPhase::Acknowledged => {
                    Some(CommandOutcome::Uncertain {
                        message: "Lost reply".into(),
                    })
                }
                _ => None,
            },
            acknowledgement: (phase == ActionPhase::Acknowledged)
                .then(|| "Reviewed outcome".into()),
        };
        record.validate().unwrap();
        self.commands.borrow_mut().insert((project, id), record);
    }
    pub fn load(&self, project: i64, id: Option<&str>) -> Option<WorkspaceTransactionState> {
        let records = self.records.borrow();
        if let Some(id) = id {
            return records.get(&(project, id.into())).cloned();
        }
        records
            .iter()
            .filter(|((id, _), _)| *id == project)
            .find(|(_, state)| {
                !matches!(
                    state.journal.progress.phase,
                    TransactionPhase::Committed | TransactionPhase::Restored
                )
            })
            .or_else(|| records.iter().rev().find(|((id, _), _)| *id == project))
            .map(|(_, state)| state.clone())
    }
    pub fn prepare(
        &self,
        backend: &FakeBackend,
        project: i64,
        input: &BeginWorkspaceTransaction,
    ) -> Result<WorkspaceTransactionPrepared, RecoveryError> {
        input.plan.validate().map_err(failure)?;
        input.editor.validate().map_err(failure)?;
        if let Some(review) = &input.language_review {
            review
                .validate_transaction(&input.plan, &input.editor)
                .map_err(failure)?;
        }
        if let Some(state) = self.load(project, Some(&input.id)) {
            if state.journal.plan != input.plan
                || state.editor != input.editor
                || state.language_review != input.language_review
                || state.journal.progress.phase != TransactionPhase::Prepared
            {
                return Err(failure("Different workspace intent"));
            }
            return Ok(WorkspaceTransactionPrepared {
                journal: PreparedJournal {
                    id: input.id.clone(),
                    generation: state.journal.generation.clone(),
                    revision: state.journal.revision,
                    fingerprint: input.plan.fingerprint().map_err(failure)?,
                },
                editor_revision: state.editor_revision,
            });
        }
        if self.load(project, None).is_some_and(|state| {
            !matches!(
                state.journal.progress.phase,
                TransactionPhase::Committed | TransactionPhase::Restored
            )
        }) {
            return Err(failure("Recover the pending workspace operation"));
        }
        let editor = backend
            .editor_recovery_records
            .borrow()
            .get(&project)
            .cloned()
            .unwrap_or_default();
        if editor.revision != input.editor.revision {
            return Err(failure("Stale editor recovery"));
        }
        input.editor.apply_to(&editor.state).map_err(failure)?;
        let generation = self.generation();
        self.records.borrow_mut().insert(
            (project, input.id.clone()),
            WorkspaceTransactionState {
                language_review: input.language_review.clone(),
                journal: JournalRecord {
                    id: input.id.clone(),
                    generation: generation.clone(),
                    revision: 1,
                    plan: input.plan.clone(),
                    progress: TransactionProgress::default(),
                },
                editor: input.editor.clone(),
                editor_revision: editor.revision,
            },
        );
        Ok(WorkspaceTransactionPrepared {
            journal: PreparedJournal {
                id: input.id.clone(),
                generation,
                revision: 1,
                fingerprint: input.plan.fingerprint().map_err(failure)?,
            },
            editor_revision: editor.revision,
        })
    }
    pub fn advance(
        &self,
        backend: &FakeBackend,
        project: i64,
        input: &AdvanceWorkspaceTransaction,
    ) -> Result<WorkspaceTransactionAdvanced, RecoveryError> {
        let mut state = self
            .load(project, Some(&input.id))
            .ok_or_else(|| failure("Missing journal"))?;
        if state.journal.revision != input.revision || state.journal.generation != input.generation
        {
            return Err(failure("Stale journal"));
        }
        state
            .journal
            .validate_transition(&input.progress)
            .map_err(failure)?;
        let deciding = input.progress.commits_editor() && !state.journal.progress.commits_editor();
        let editor = if deciding {
            let previous = backend
                .editor_recovery_records
                .borrow()
                .get(&project)
                .cloned()
                .unwrap_or_default();
            if previous.revision != state.editor_revision {
                return Err(failure("Stale editor recovery"));
            }
            Some(EditorRecoveryRecord {
                revision: previous.revision + 1,
                state: state.editor.apply_to(&previous.state).map_err(failure)?,
            })
        } else {
            None
        };
        state.journal.revision += 1;
        state.journal.progress = input.progress.clone();
        if let Some(editor) = &editor {
            backend
                .editor_recovery_records
                .borrow_mut()
                .insert(project, editor.clone());
        }
        let revision = state.journal.revision;
        self.records
            .borrow_mut()
            .insert((project, input.id.clone()), state);
        if deciding && self.lose_commit_reply.replace(false) {
            return Err(RecoveryError::Unavailable("Commit reply lost".into()));
        }
        Ok(WorkspaceTransactionAdvanced {
            generation: input.generation.clone(),
            revision,
            editor,
        })
    }
}
struct Journal<'a> {
    backend: &'a FakeBackend,
    project: i64,
    input: BeginWorkspaceTransaction,
}
impl WorkspaceJournal for Journal<'_> {
    async fn prepare(&self, plan: &WorkspacePlan) -> Result<PreparedJournal, String> {
        let mut input = self.input.clone();
        input.plan = plan.clone();
        self.backend
            .begin_workspace_transaction(self.project, &input)
            .await
            .map(|prepared| prepared.journal)
            .map_err(|e| e.to_string())
    }
    async fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &TransactionProgress,
    ) -> Result<u64, String> {
        self.backend
            .advance_workspace_transaction(
                self.project,
                &AdvanceWorkspaceTransaction {
                    id: id.into(),
                    generation: generation.into(),
                    revision,
                    progress: progress.clone(),
                },
            )
            .await
            .map(|ack| ack.revision)
            .map_err(|e| e.to_string())
    }
    async fn load(&self, id: Option<&str>) -> Result<Option<JournalRecord>, String> {
        self.backend
            .workspace_transaction(self.project, id)
            .await
            .map(|state| state.map(|state| state.journal))
            .map_err(|e| e.to_string())
    }
}
struct Files<'a> {
    backend: &'a FakeBackend,
    project: i64,
    identity: String,
}
impl WorkspaceEntries for Files<'_> {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.backend.canonical_file_path(self.project, path).await
    }
    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        self.backend.list_files(self.project, path).await
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.backend.read_file_bytes(self.project, path).await
    }
    async fn create(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        self.backend
            .create_file(self.project, path, kind.is_dir())
            .await
    }
    async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.backend
            .write_file_bytes(self.project, path, bytes)
            .await
    }
    async fn delete(&self, path: &str) -> Result<(), String> {
        self.backend.delete_file(self.project, path).await
    }
}
impl TransactionFiles for Files<'_> {
    fn root_identity(&self) -> &str {
        &self.identity
    }
    async fn replace_atomic(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.backend
            .write_file_bytes(self.project, path, bytes)
            .await
    }
    async fn remove_entry(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        let entry = self
            .backend
            .list_files(
                self.project,
                openwebide_core::workspace_entries::parent(path),
            )
            .await?
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or("Missing entry")?;
        if entry.is_dir != kind.is_dir()
            || (entry.is_dir
                && !self
                    .backend
                    .list_files(self.project, path)
                    .await?
                    .is_empty())
        {
            return Err("Entry changed or is not empty".into());
        }
        if kind.is_dir() {
            self.backend
                .directories
                .borrow_mut()
                .remove(&(self.project, path.into()));
            Ok(())
        } else {
            self.backend.delete_file(self.project, path).await
        }
    }
}
pub async fn execute(
    backend: &FakeBackend,
    project: i64,
    request: &ExecuteWorkspaceTransaction,
) -> Result<WorkspaceTransactionOutcome, RecoveryError> {
    let state = backend
        .workspace_transactions
        .load(project, Some(&request.id))
        .ok_or_else(|| failure("Missing journal"))?;
    if state.journal.generation != request.generation {
        return Err(failure("Reviewed operation was replaced"));
    }
    let files = Files {
        backend,
        project,
        identity: identity(project),
    };
    let journal = Journal {
        backend,
        project,
        input: BeginWorkspaceTransaction {
            language_review: state.language_review,
            id: request.id.clone(),
            plan: state.journal.plan.clone(),
            editor: state.editor,
        },
    };
    let generation = state.journal.generation.clone();
    match openwebide_core::workspace_transaction::execute(
        &files,
        &journal,
        state.journal,
        request.action,
        || true,
    )
    .await
    {
        Ok(record) => Ok(WorkspaceTransactionOutcome {
            generation,
            phase: Some(record.progress.phase),
            error: None,
        }),
        Err(error) => Ok(WorkspaceTransactionOutcome {
            generation,
            phase: journal
                .load(None)
                .await
                .ok()
                .flatten()
                .map(|record| record.progress.phase),
            error: Some(error),
        }),
    }
}
