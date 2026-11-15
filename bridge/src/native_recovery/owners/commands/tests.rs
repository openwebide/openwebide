use super::*;
use crate::folder_coordination::Coordinator;
use openwebide_core::{
    WorkspaceMode,
    editor::EditorRecoveryRoot,
    plugins::languages::intelligence::actions::{ActionPhase, CommandRecoveryScope},
    workspace_transaction::WorkspaceActivity,
};
use std::sync::Arc;
mod administration;
mod callback;
mod cancellation;
mod prepared;
mod process_cleanup;
mod settlements;

#[tokio::test]
async fn command_reservation_precedes_file_admission_and_promotes_only_its_original_worker() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.path().into()));
        let folder = Folder::capture(root.path()).unwrap();
        let mut recovery = original();
        recovery.scope.root.mode = mode;
        recovery.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "project".into());
        recovery.activity.file_recovery = true;
        recovery.command.phase = ActionPhase::Approved;
        let authority =
            RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
        let namespace = Namespace::open(home.path()).unwrap();
        let permit = namespace
            .permit(&folder, &authority, &recovery.scope, recovery.activity)
            .unwrap();
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        let worker = WorkspaceTransactionWorker {
            id: "8".repeat(32),
            secret: "9".repeat(32),
        };
        namespace
            .reserve_command(
                &folder,
                &lease,
                &permit,
                CommandIntent {
                    recovery: &recovery,
                    worker: &worker,
                },
            )
            .unwrap();
        assert!(
            namespace
                .command_admitted(&folder, &lease, &permit, &worker)
                .is_err()
        );
        assert!(
            namespace
                .command_dispatch(&folder, &lease, &permit, &worker)
                .is_err()
        );
        assert!(
            namespace
                .command_intent(
                    &folder,
                    &lease,
                    &permit,
                    CommandIntent {
                        recovery: &recovery,
                        worker: &worker
                    }
                )
                .is_err()
        );
        drop(namespace);
        let namespace = Namespace::open(home.path()).unwrap();
        namespace
            .reserve_command(
                &folder,
                &lease,
                &permit,
                CommandIntent {
                    recovery: &recovery,
                    worker: &worker,
                },
            )
            .unwrap();
        let record = namespace
            .access(false, |_, _, db| Ok(read(db, version(db)?)?.remove(0)))
            .unwrap();
        assert_eq!(record.stage, CommandStage::Reserved);
        assert_eq!(record.worker.secret, worker.secret);
        recovery.activity.file_recovery = false;
        recovery.command.phase = ActionPhase::Running;
        for wrong in [
            WorkspaceTransactionWorker {
                id: "a".repeat(32),
                secret: worker.secret.clone(),
            },
            WorkspaceTransactionWorker {
                id: worker.id.clone(),
                secret: "b".repeat(32),
            },
        ] {
            assert!(
                namespace
                    .command_intent(
                        &folder,
                        &lease,
                        &permit,
                        CommandIntent {
                            recovery: &recovery,
                            worker: &wrong
                        }
                    )
                    .is_err()
            );
        }
        let (_, first) = namespace
            .command_intent(
                &folder,
                &lease,
                &permit,
                CommandIntent {
                    recovery: &recovery,
                    worker: &worker,
                },
            )
            .unwrap();
        assert!(first);
        assert!(
            !namespace
                .command_intent(
                    &folder,
                    &lease,
                    &permit,
                    CommandIntent {
                        recovery: &recovery,
                        worker: &worker
                    }
                )
                .unwrap()
                .1
        );
        namespace
            .command_admitted(&folder, &lease, &permit, &worker)
            .unwrap();
        namespace
            .command_dispatch(&folder, &lease, &permit, &worker)
            .unwrap();
        assert!(
            namespace
                .command_dispatch(&folder, &lease, &permit, &worker)
                .is_err()
        );
        drop(lease);
        assert!(coordinator.acquire(folder).await.is_err());
    }
}

#[tokio::test]
async fn full_command_capacity_does_not_publish_an_orphaned_folder_owner() {
    let home = tempfile::tempdir().unwrap();
    let roots = tempfile::tempdir().unwrap();
    let coordinator = Coordinator::default();
    coordinator.set_recovery_home(Arc::new(home.path().into()));
    let namespace = Namespace::open(home.path()).unwrap();
    let mut filled = false;
    for index in 0..MAX_OWNERS {
        let path = roots.path().join(index.to_string());
        std::fs::create_dir(&path).unwrap();
        let folder = Folder::capture(&path).unwrap();
        let mut recovery = original();
        recovery.scope.id = format!("{index:032x}");
        recovery
            .command
            .invocation
            .id
            .clone_from(&recovery.scope.id);
        let authority =
            RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
        let permit = namespace
            .permit(&folder, &authority, &recovery.scope, recovery.activity)
            .unwrap();
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        let worker = WorkspaceTransactionWorker {
            id: "8".repeat(32),
            secret: "9".repeat(32),
        };
        let before = namespace
            .access(false, |_, _, db| Ok(db.serialize("main").unwrap().to_vec()))
            .unwrap();
        let result = namespace.reserve_command(
            &folder,
            &lease,
            &permit,
            CommandIntent {
                recovery: &recovery,
                worker: &worker,
            },
        );
        if let Err(error) = result {
            assert_eq!(error, "Native command recovery capacity is full");
            assert!(
                namespace
                    .command_intent(
                        &folder,
                        &lease,
                        &permit,
                        CommandIntent {
                            recovery: &recovery,
                            worker: &worker
                        }
                    )
                    .is_err()
            );
            let after = namespace
                .access(false, |_, _, db| Ok(db.serialize("main").unwrap().to_vec()))
                .unwrap();
            assert_eq!(before, after);
            drop(lease);
            drop(namespace);
            let reopened = Namespace::open(home.path()).unwrap();
            assert!(reopened.admit(&folder, None).is_ok());
            drop(coordinator.acquire(folder).await.unwrap());
            filled = true;
            break;
        }
        drop(lease);
        assert!(coordinator.acquire(folder).await.is_err());
    }
    assert!(filled, "The bounded command ledger must reject excess work");
}

fn original() -> WorkspaceCommandRecovery {
    let scope = WorkspaceTransactionScope {
        id: "1".repeat(32),
        generation: "2".repeat(32),
        revision: 1,
        root_identity: "3".repeat(64),
        root: EditorRecoveryRoot {
            mode: WorkspaceMode::Remote,
            path: Some("project".into()),
        },
        worker: None,
        accepted_language_review_digest: None,
    };
    WorkspaceCommandRecovery {
        command: CommandRecoveryScope {
            invocation: CommandInvocation {
                id: scope.id.clone(),
                client: "window".into(),
                command: "4".repeat(32),
                server: "original".into(),
                origin: "5".repeat(64),
                review: "6".repeat(64),
                approval: "7".repeat(64),
            },
            revision: 2,
            phase: ActionPhase::Running,
            outcome_digest: None,
            worker: None,
            native_settlement_pending: false,
            command_cleanup_confirmed: false,
        },
        scope,
        activity: WorkspaceActivity {
            file_recovery: false,
            language_command: true,
        },
    }
}

#[tokio::test]
async fn original_callback_and_file_completion_survive_reopen_without_inventing_cleanup() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.path().into()));
        let folder = Folder::capture(root.path()).unwrap();
        let namespace = Namespace::open(home.path()).unwrap();
        let mut recovery = original();
        recovery.scope.root.mode = mode;
        recovery.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "project".into());
        recovery.command.phase = ActionPhase::Approved;
        recovery.activity.file_recovery = true;
        let authority =
            RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
        let permit = namespace
            .permit(&folder, &authority, &recovery.scope, recovery.activity)
            .unwrap()
            .with_origin(Some("http://127.0.0.1:3000/api/"))
            .unwrap();
        let worker = WorkspaceTransactionWorker {
            id: "8".repeat(32),
            secret: "9".repeat(32),
        };
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        namespace
            .reserve_command(
                &folder,
                &lease,
                &permit,
                CommandIntent {
                    recovery: &recovery,
                    worker: &worker,
                },
            )
            .unwrap();
        namespace.files_admitted(&folder, &lease, &permit).unwrap();
        namespace
            .files_finished(&folder, &lease, &permit, recovery.activity)
            .unwrap();
        drop(lease);
        drop(namespace);

        let namespace = Namespace::open(home.path()).unwrap();
        let pending = namespace
            .pending_command(7, 11, mode, &recovery.command.invocation)
            .unwrap()
            .unwrap();
        assert!(
            !pending.can_reject,
            "An admission or unfinished file phase is not a cleanup receipt"
        );
        assert_eq!(pending.authority, authority);
        assert_eq!(pending.worker.secret, worker.secret);
        assert_eq!(pending.origin, "http://127.0.0.1:3000/api");
        for (user, project) in [(8, 11), (7, 12)] {
            assert!(
                namespace
                    .pending_command(user, project, mode, &recovery.command.invocation)
                    .unwrap()
                    .is_none()
            );
        }
        let (reopened, original_permit) = pending.reopen().unwrap();
        let lease = coordinator
            .acquire_recovery(reopened.clone(), original_permit.clone())
            .await
            .unwrap();
        let replacement = original_permit
            .clone()
            .with_origin(Some("http://127.0.0.1:3001/api"))
            .unwrap();
        assert!(namespace.enroll(&reopened, &lease, &replacement).is_err());
        assert!(
            namespace
                .check_command_origin(
                    7,
                    11,
                    mode,
                    &recovery.command.invocation,
                    "http://127.0.0.1:3001/api"
                )
                .is_err()
        );
        recovery.activity.file_recovery = false;
        namespace
            .files_finished(&reopened, &lease, &original_permit, recovery.activity)
            .unwrap();
        namespace
            .reserve_command(
                &reopened,
                &lease,
                &original_permit,
                CommandIntent {
                    recovery: &recovery,
                    worker: &worker,
                },
            )
            .unwrap();
        drop(lease);
        drop(namespace);
        let namespace = Namespace::open(home.path()).unwrap();
        assert!(
            namespace
                .pending_command(7, 11, mode, &recovery.command.invocation)
                .unwrap()
                .unwrap()
                .can_reject
        );
        assert!(
            coordinator.acquire(folder).await.is_err(),
            "File completion alone must retain the attached command owner"
        );
    }
}

#[tokio::test]
async fn legacy_owner_upgrade_preserves_unknown_file_effects_and_rejects_context_downgrade() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let coordinator = Coordinator::default();
    coordinator.set_recovery_home(Arc::new(home.path().into()));
    let folder = Folder::capture(root.path()).unwrap();
    let namespace = Namespace::open(home.path()).unwrap();
    let recovery = original();
    let authority = RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
    let permit = namespace
        .permit(&folder, &authority, &recovery.scope, recovery.activity)
        .unwrap()
        .with_origin(Some("http://localhost/api"))
        .unwrap();
    let lease = coordinator
        .acquire_recovery(folder.clone(), permit.clone())
        .await
        .unwrap();
    namespace.enroll(&folder, &lease, &permit).unwrap();
    namespace
        .access(false, |_, _, db| {
            assert!(super::super::read(db, 5).is_err());
            let mut legacy = permit.owner.clone();
            legacy.format = 1;
            legacy.credential = None;
            legacy.origin = None;
            legacy.files = FileStage::Unknown;
            super::super::context::save_owner(db, &legacy)?;
            assert!(super::super::read(db, 5)?.remove(0).authority()?.is_none());
            let (upgraded, _) = namespace.enroll_owner(db, &permit.owner.folder, &permit)?;
            assert!(upgraded.files == FileStage::Unknown);
            assert_eq!(upgraded.authority()?, Some(authority.clone()));
            let mut forged = upgraded;
            forged.credential = Some("B".repeat(43));
            assert!(forged.validate().is_err());
            Ok(())
        })
        .unwrap();
}
#[tokio::test]
async fn original_command_intent_and_dispatch_are_durable_single_use_and_settle_only_the_original_outcome()
 {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.path().into()));
        let folder = Folder::capture(root.path()).unwrap();
        let mut recovery = original();
        recovery.scope.root.mode = mode;
        recovery.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "project".into());
        let authority =
            RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
        let namespace = Namespace::open(home.path()).unwrap();
        let permit = namespace
            .permit(&folder, &authority, &recovery.scope, recovery.activity)
            .unwrap();
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        namespace.enroll(&folder, &lease, &permit).unwrap();
        let worker = WorkspaceTransactionWorker {
            id: "8".repeat(32),
            secret: "9".repeat(32),
        };
        let (record, new) = namespace
            .command_intent(
                &folder,
                &lease,
                &permit,
                CommandIntent {
                    recovery: &recovery,
                    worker: &worker,
                },
            )
            .unwrap();
        assert!(new);
        assert_eq!(record.stage, CommandStage::Prepared);
        assert!(
            namespace
                .has_original_command(&permit, &worker, &recovery.command.invocation)
                .unwrap()
        );
        assert!(
            namespace
                .pending_command(7, 11, mode, &recovery.command.invocation)
                .unwrap()
                .is_none(),
            "Missing optional callback context must not hide original ownership"
        );
        let wrong_worker = WorkspaceTransactionWorker {
            id: worker.id.clone(),
            secret: "a".repeat(32),
        };
        assert!(
            namespace
                .has_original_command(&permit, &wrong_worker, &recovery.command.invocation)
                .is_err()
        );
        let other_home = tempfile::tempdir().unwrap();
        let other_namespace = Namespace::open(other_home.path()).unwrap();
        assert!(
            other_namespace
                .has_original_command(&permit, &worker, &recovery.command.invocation)
                .is_err()
        );
        assert!(
            !namespace
                .command_intent(
                    &folder,
                    &lease,
                    &permit,
                    CommandIntent {
                        recovery: &recovery,
                        worker: &worker
                    }
                )
                .unwrap()
                .1
        );
        assert!(
            namespace
                .command_dispatch(&folder, &lease, &permit, &worker)
                .is_err()
        );
        namespace
            .command_admitted(&folder, &lease, &permit, &worker)
            .unwrap();
        namespace
            .command_dispatch(&folder, &lease, &permit, &worker)
            .unwrap();
        assert!(
            namespace
                .command_dispatch(&folder, &lease, &permit, &worker)
                .is_err()
        );
        let outcome = CommandOutcome::Completed {
            result: serde_json::json!("文😀".repeat(30000)),
        };
        outcome.validate().unwrap();
        let before = namespace
            .access(false, |_, _, db| Ok(db.serialize("main").unwrap().len()))
            .unwrap();
        namespace
            .command_outcome(&folder, &lease, &permit, &worker, &outcome, true)
            .unwrap();
        let after = namespace
            .access(false, |_, _, db| Ok(db.serialize("main").unwrap().len()))
            .unwrap();
        assert_eq!(before, after);
        drop(namespace);
        let namespace = Namespace::open(home.path()).unwrap();
        let (retained, new) = namespace
            .command_intent(
                &folder,
                &lease,
                &permit,
                CommandIntent {
                    recovery: &recovery,
                    worker: &worker,
                },
            )
            .unwrap();
        assert!(!new);
        assert_eq!(retained.outcome, Some(outcome.clone()));
        assert!(
            namespace
                .has_original_command(&permit, &worker, &recovery.command.invocation)
                .unwrap()
        );
        assert!(
            namespace
                .command_outcome(
                    &folder,
                    &lease,
                    &permit,
                    &worker,
                    &CommandOutcome::Uncertain {
                        message: "replacement".into()
                    },
                    true
                )
                .is_err()
        );
        let mut settled = recovery.clone();
        settled.command.phase = ActionPhase::Completed;
        settled.command.revision += 1;
        settled.command.worker = Some(worker.state());
        settled.command.worker.as_mut().unwrap().active = false;
        settled.command.command_cleanup_confirmed = true;
        settled.command.outcome_digest = Some(openwebide_core::plugins::content_digest(
            &serde_json::to_vec(&outcome).unwrap(),
        ));
        settled.activity.language_command = false;
        let mut wrong = settled.clone();
        wrong.command.outcome_digest = Some("0".repeat(64));
        assert!(
            namespace
                .command_settled(&folder, &lease, &permit, &wrong)
                .is_err()
        );
        namespace
            .command_settled(&folder, &lease, &permit, &settled)
            .unwrap();
        assert!(
            !namespace
                .has_original_command(&permit, &worker, &recovery.command.invocation)
                .unwrap()
        );
        drop(lease);
        drop(coordinator.acquire(folder.clone()).await.unwrap());
        let mut next = original();
        next.scope.root = recovery.scope.root.clone();
        next.scope.id = "a".repeat(32);
        next.command.invocation.id.clone_from(&next.scope.id);
        let next_authority =
            RecoveryAuthority::for_scope(7, 11, &next.scope, "A".repeat(43)).unwrap();
        let next_permit = namespace
            .permit(&folder, &next_authority, &next.scope, next.activity)
            .unwrap();
        let lease = coordinator
            .acquire_recovery(folder.clone(), next_permit.clone())
            .await
            .unwrap();
        namespace
            .command_intent(
                &folder,
                &lease,
                &next_permit,
                CommandIntent {
                    recovery: &next,
                    worker: &worker,
                },
            )
            .unwrap();
        assert!(
            !namespace
                .has_original_command(&permit, &worker, &recovery.command.invocation)
                .unwrap()
        );
        assert!(
            namespace
                .has_original_command(&next_permit, &worker, &next.command.invocation)
                .unwrap()
        );
        drop(lease);
        assert!(
            coordinator.acquire(folder).await.is_err(),
            "Retiring the old local publisher must not authorize writes through a newer owner"
        );
    }
}

#[tokio::test]
async fn original_lease_can_publish_uncertainty_after_root_loss_without_restoring_workspace_authority()
 {
    let home = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("original");
    std::fs::create_dir(&path).unwrap();
    let coordinator = Coordinator::default();
    coordinator.set_recovery_home(Arc::new(home.path().into()));
    let folder = Folder::capture(&path).unwrap();
    let recovery = original();
    let authority = RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
    let namespace = Namespace::open(home.path()).unwrap();
    let permit = namespace
        .permit(&folder, &authority, &recovery.scope, recovery.activity)
        .unwrap();
    let lease = coordinator
        .acquire_recovery(folder.clone(), permit.clone())
        .await
        .unwrap();
    namespace.enroll(&folder, &lease, &permit).unwrap();
    let worker = WorkspaceTransactionWorker {
        id: "8".repeat(32),
        secret: "9".repeat(32),
    };
    namespace
        .command_intent(
            &folder,
            &lease,
            &permit,
            CommandIntent {
                recovery: &recovery,
                worker: &worker,
            },
        )
        .unwrap();
    namespace
        .command_admitted(&folder, &lease, &permit, &worker)
        .unwrap();
    namespace
        .command_dispatch(&folder, &lease, &permit, &worker)
        .unwrap();
    std::fs::rename(&path, parent.path().join("moved")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(!lease.covers(&folder));
    assert!(lease.retains(&folder));
    assert!(
        namespace
            .command_dispatch(&folder, &lease, &permit, &worker)
            .is_err()
    );
    let outcome = CommandOutcome::Uncertain {
        message: "Original path changed during RPC".into(),
    };
    lease.mark_uncertain();
    namespace
        .command_outcome(&folder, &lease, &permit, &worker, &outcome, false)
        .unwrap();
    let record = namespace
        .access(false, |_, _, db| Ok(read(db, version(db)?)?.remove(0)))
        .unwrap();
    assert_eq!(record.outcome, Some(outcome));
    assert!(!record.cleaned);
    assert!(
        namespace
            .command_admitted(&folder, &lease, &permit, &worker)
            .is_err()
    );
    drop(lease);
    assert!(
        coordinator
            .acquire(Folder::capture(&path).unwrap())
            .await
            .is_err()
    );
}
