//! The browser journal API represented by a real SQLite store for the paired
//! filesystem contract. The production core engine owns the whole workflow.
use super::*;
use openwebide_core::{
    UserId,
    workspace_transaction::{
        self as transaction, JournalRecord, PreparedJournal, TransactionProgress, WorkspaceJournal,
        WorkspacePlan, WorkspaceTransactionState,
    },
};
struct Journal {
    backend: Arc<super::super::super::tests::Backend>,
    user: UserId,
    project: i64,
    id: String,
}
impl Journal {
    async fn state(&self) -> WorkspaceTransactionState {
        self.backend
            .store
            .workspace_transaction(self.user, self.project, Some(&self.id))
            .await
            .unwrap()
            .unwrap()
    }
}
impl WorkspaceJournal for Journal {
    async fn prepare(&self, plan: &WorkspacePlan) -> Result<PreparedJournal, String> {
        let state = self.state().await;
        if state.journal.plan != *plan || state.journal.progress != TransactionProgress::default() {
            return Err("Review no longer matches its prepared journal".into());
        }
        Ok(PreparedJournal {
            id: self.id.clone(),
            generation: state.journal.generation.clone(),
            revision: state.journal.revision,
            fingerprint: plan.fingerprint()?,
        })
    }
    async fn load(&self, id: Option<&str>) -> Result<Option<JournalRecord>, String> {
        if id.is_some_and(|id| id != self.id) {
            return Ok(None);
        }
        Ok(Some(self.state().await.journal))
    }
    async fn advance(
        &self,
        _id: &str,
        _generation: &str,
        _revision: u64,
        _progress: &TransactionProgress,
    ) -> Result<u64, String> {
        panic!("The browser's recovery reader must not write an admitted native cursor")
    }
}

#[tokio::test]
async fn prepared_account_cancellation_revokes_native_calls_without_releasing_the_physical_gate() {
    use crate::folder_coordination::Folder;
    let fixture =
        super::super::super::tests::fixture_mode(2, openwebide_core::WorkspaceMode::Local).await;
    let root = fixture.directory.path().join("project");
    let case = Case {
        directory: fixture.directory,
        host: Host::default(),
        authorities: Default::default(),
        requests: Default::default(),
        lose_advance: Arc::new(AtomicBool::new(false)),
    };
    let journal = Journal {
        backend: fixture.backend,
        user: fixture.user,
        project: fixture.project,
        id: fixture.command.request.id,
    };
    let state = journal.state().await;
    let scope = ResourceScope {
        transaction: journal.id.clone(),
        fingerprint: state.journal.plan.fingerprint().unwrap(),
    };
    let mut files = case
        .files_at("project", Arc::new(AtomicBool::new(false)))
        .await;
    files.bind(scope.clone()).await.unwrap();
    let authority = super::authority::Authority::serve(journal.backend.clone()).await;
    let grant = authority.grant(&scope.transaction, files.worker()).await;
    files.admit(authority.url.clone(), grant).await.unwrap();
    let id = case
        .host
        .state
        .lock()
        .await
        .sessions
        .keys()
        .next()
        .unwrap()
        .clone();
    assert!(
        case.call(
            &id,
            2,
            FileAction::Create {
                ordinal: 0,
                path: "before-intent".into(),
                is_dir: false
            }
        )
        .await
        .is_err()
    );
    assert!(!root.join("before-intent").exists());
    let physical = case
        .host
        .coordinator
        .acquire(Folder::capture(&root).unwrap())
        .await
        .unwrap();
    let mut pending =
        Box::pin(files.create_resource(&scope, 0, "after-cancel", VfsEntryKind::File));
    assert!(pending.as_mut().now_or_never().is_none());
    tokio::time::timeout(Duration::from_secs(3), async {
        while case.host.coordinator.waiting() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    journal
        .backend
        .store
        .advance_workspace_transaction(
            journal.user,
            journal.project,
            &transaction::AdvanceWorkspaceTransaction {
                id: journal.id.clone(),
                generation: state.journal.generation,
                revision: state.journal.revision,
                progress: TransactionProgress::prepared_cancellation(),
            },
            102,
        )
        .await
        .unwrap();
    assert_eq!(case.host.coordinator.waiting(), 1);
    assert!(pending.as_mut().now_or_never().is_none());
    assert!(!root.join("after-cancel").exists());
    drop(physical);
    assert!(pending.await.is_err());
    assert!(!root.join("after-cancel").exists());
    assert_eq!(
        std::fs::read(root.join("before.rs")).unwrap(),
        "disk 文😀\r\n".as_bytes()
    );
    files.close().await.unwrap();
}

#[tokio::test]
async fn paired_files_run_the_same_sqlite_editor_decision_and_restore_after_a_lost_native_reply() {
    use std::os::unix::fs::PermissionsExt;
    for (lost, lost_cursor) in [(false, false), (true, false), (false, true)] {
        let fixture =
            super::super::super::tests::fixture_mode(2, openwebide_core::WorkspaceMode::Local)
                .await;
        assert_eq!(fixture.user.get(), 1);
        assert_eq!(fixture.project, 1);
        let root = fixture.directory.path().join("project");
        std::fs::set_permissions(
            root.join("before.rs"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let original = same_file::Handle::from_path(root.join("before.rs")).unwrap();
        let overwritten = same_file::Handle::from_path(root.join("after.rs")).unwrap();
        let case = Case {
            directory: fixture.directory,
            host: Host::default(),
            authorities: Default::default(),
            lose_advance: Arc::new(AtomicBool::new(lost_cursor)),
            requests: Default::default(),
        };
        let journal = Journal {
            backend: fixture.backend,
            user: fixture.user,
            project: fixture.project,
            id: fixture.command.request.id,
        };
        let state = journal.state().await;
        let scope = ResourceScope {
            transaction: journal.id.clone(),
            fingerprint: state.journal.plan.fingerprint().unwrap(),
        };
        let mut files = case
            .files_at("project", Arc::new(AtomicBool::new(lost)))
            .await;
        assert_eq!(files.root_identity(), state.journal.plan.root);
        files.bind(scope.clone()).await.unwrap();
        let authority = super::authority::Authority::serve(journal.backend.clone()).await;
        let grant = authority.grant(&scope.transaction, files.worker()).await;
        files.admit(authority.url.clone(), grant).await.unwrap();
        let initial_revision = state.journal.revision;
        let outcome = transaction::execute(
            &files,
            &files.journal(&journal),
            state.journal,
            transaction::WorkspaceTransactionAction::Apply,
            || true,
        )
        .await;
        if lost_cursor {
            assert_eq!(journal.state().await.journal.revision, initial_revision + 1);
            assert_eq!(
                same_file::Handle::from_path(root.join("before.rs")).unwrap(),
                original
            );
            assert_eq!(
                same_file::Handle::from_path(root.join("after.rs")).unwrap(),
                overwritten
            );
        }
        if lost || lost_cursor {
            assert!(outcome.is_err());
            assert!(!matches!(
                journal.state().await.journal.progress.phase,
                transaction::TransactionPhase::Committed | transaction::TransactionPhase::Restored
            ));
            let mut draft = journal
                .backend
                .store
                .editor_recovery(journal.user, journal.project)
                .await
                .unwrap();
            assert_eq!(draft.state.files[0].path, "before.rs");
            draft.state.files[0].document.as_mut().unwrap().text = Arc::new("newer draft".into());
            journal
                .backend
                .store
                .save_editor_recovery(journal.user, journal.project, draft.revision, &draft.state)
                .await
                .unwrap();
            let old_request = case
                .requests
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|request| {
                    if lost_cursor {
                        matches!(request.action, FolderAction::Advance { .. })
                    } else {
                        matches!(
                            request.action,
                            FolderAction::File {
                                action: FileAction::Transfer { .. },
                                ..
                            }
                        )
                    }
                })
                .unwrap()
                .clone();
            let peer = Host::default();
            let mut recovered = case
                .files_on(&peer, "project", Arc::new(AtomicBool::new(false)))
                .await;
            recovered.bind(scope.clone()).await.unwrap();
            let grant = authority
                .grant(&scope.transaction, recovered.worker())
                .await;
            recovered.admit(authority.url.clone(), grant).await.unwrap();
            // The old native session remains active in another bridge. Its
            // cached success must be fenced by the database owner, not a
            // process-local proof registry or a time-based lease.
            assert!(
                case.host
                    .request(case.root(), "paired/user:1".into(), old_request)
                    .await
                    .is_err()
            );
            assert!(files.close().await.is_err());
            let terminal = transaction::execute(
                &recovered,
                &recovered.journal(&journal),
                journal.state().await.journal,
                transaction::WorkspaceTransactionAction::Recover,
                || true,
            )
            .await
            .unwrap();
            recovered.close().await.unwrap();
            assert_eq!(
                terminal.progress.phase,
                transaction::TransactionPhase::Restored
            );
            assert_eq!(
                same_file::Handle::from_path(root.join("before.rs")).unwrap(),
                original
            );
            assert_eq!(
                same_file::Handle::from_path(root.join("after.rs")).unwrap(),
                overwritten
            );
            assert_eq!(
                journal
                    .backend
                    .store
                    .editor_recovery(journal.user, journal.project)
                    .await
                    .unwrap()
                    .state,
                draft.state
            );
        } else {
            files.close().await.unwrap();
            assert_eq!(
                outcome.unwrap().progress.phase,
                transaction::TransactionPhase::Committed
            );
            assert_eq!(
                same_file::Handle::from_path(root.join("after.rs")).unwrap(),
                original
            );
            assert!(!root.join("before.rs").exists());
            let editor = journal
                .backend
                .store
                .editor_recovery(journal.user, journal.project)
                .await
                .unwrap();
            assert_eq!(editor.state.files[0].path, "after.rs");
            assert!(
                editor.state.files[0]
                    .document
                    .as_ref()
                    .unwrap()
                    .text
                    .contains("unsaved")
            );
        }
        assert!(
            !journal
                .backend
                .store
                .workspace_transaction_scope(journal.user, journal.project, &journal.id)
                .await
                .unwrap()
                .unwrap()
                .worker
                .unwrap()
                .active
        );
        let source = if lost || lost_cursor {
            "before.rs"
        } else {
            "after.rs"
        };
        assert_eq!(
            std::fs::metadata(root.join(source))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert!(
            !root
                .join(format!(".openwebide/recovery-{}", journal.id))
                .exists()
        );
    }
}
