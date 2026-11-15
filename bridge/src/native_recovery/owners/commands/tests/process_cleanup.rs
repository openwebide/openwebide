use super::*;

struct Fixture {
    home: tempfile::TempDir,
    root: tempfile::TempDir,
    coordinator: Coordinator,
    namespace: Namespace,
    folder: Folder,
    permit: Permit,
    lease: crate::folder_coordination::Lease,
    worker: WorkspaceTransactionWorker,
    recovery: WorkspaceCommandRecovery,
}
impl Fixture {
    async fn new(mode: WorkspaceMode) -> Self {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.path().into()));
        let namespace = Namespace::open(home.path()).unwrap();
        let folder = Folder::capture(root.path()).unwrap();
        let mut recovery = original();
        recovery.scope.root.mode = mode;
        recovery.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "project".into());
        let authority =
            RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
        let permit = namespace
            .permit(&folder, &authority, &recovery.scope, recovery.activity)
            .unwrap()
            .with_origin(Some("http://127.0.0.1:3000/api"))
            .unwrap();
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        namespace.enroll(&folder, &lease, &permit).unwrap();
        namespace
            .files_finished(&folder, &lease, &permit, recovery.activity)
            .unwrap();
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
        Self {
            home,
            root,
            coordinator,
            namespace,
            folder,
            permit,
            lease,
            worker,
            recovery,
        }
    }
    fn pending(&self) -> PendingCommand {
        self.namespace
            .pending_command(
                7,
                11,
                self.recovery.scope.root.mode,
                &self.recovery.command.invocation,
            )
            .unwrap()
            .unwrap()
    }
    fn outcome(&self, outcome: &CommandOutcome, cleaned: bool) -> Result<(), String> {
        self.namespace.command_outcome(
            &self.folder,
            &self.lease,
            &self.permit,
            &self.worker,
            outcome,
            cleaned,
        )
    }
    fn data(&self) -> Vec<u8> {
        self.namespace
            .access(false, |_, _, db| Ok(db.serialize("main").unwrap().to_vec()))
            .unwrap()
    }
}

#[tokio::test]
async fn original_process_receipts_bind_the_worker_folder_and_namespace_and_preserve_exact_outcomes()
 {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for stage in ["admitted", "dispatched", "retained"] {
            let f = Fixture::new(mode).await;
            let proof = f
                .namespace
                .command_process_cleanup(&f.folder, &f.lease, &f.permit, &f.worker)
                .unwrap();
            let descriptors = f.lease.command_descriptors(f.root.path()).unwrap();
            proof.validate(&descriptors).unwrap();
            assert!(proof.release(&descriptors).is_err());
            assert!(!f.pending().process_cleanup_confirmed);
            let original = f.data();
            for field in ["token", "owner", "key"] {
                let mut forged = serde_json::to_value(&proof).unwrap();
                let value = if field == "key" {
                    &mut forged["key"]["nonce"]
                } else {
                    &mut forged[field]
                };
                let mut changed = value.as_str().unwrap().to_string();
                changed.replace_range(..1, if changed.starts_with('0') { "1" } else { "0" });
                *value = changed.into();
                let forged: ProcessCleanup = serde_json::from_value(forged).unwrap();
                assert!(forged.validate(&descriptors).is_err(), "{field}");
                let result = forged.stopped(&descriptors);
                assert!(result.is_err() || field == "owner", "{field}");
                assert_eq!(f.data(), original);
            }
            assert!(
                proof
                    .stopped(&descriptors[..descriptors.len() - 1])
                    .is_err()
            );
            let mut reversed = descriptors
                .iter()
                .map(|fd| {
                    use std::os::fd::AsRawFd;
                    crate::exec::process_owner::duplicate(fd.as_raw_fd()).unwrap()
                })
                .collect::<Vec<_>>();
            reversed.reverse();
            assert!(proof.stopped(&reversed).is_err());
            let wrong = WorkspaceTransactionWorker {
                id: f.worker.id.clone(),
                secret: "a".repeat(32),
            };
            assert!(
                f.namespace
                    .command_process_cleanup(&f.folder, &f.lease, &f.permit, &wrong)
                    .is_err()
            );
            if stage != "admitted" {
                f.namespace
                    .command_dispatch(&f.folder, &f.lease, &f.permit, &f.worker)
                    .unwrap();
            }
            let outcome = match stage {
                "admitted" => CommandOutcome::Rejected {
                    message: "original no-dispatch 文😀\"".into(),
                },
                "dispatched" => CommandOutcome::Uncertain {
                    message: "original unknown effects 文😀\"".into(),
                },
                _ => CommandOutcome::Completed {
                    result: serde_json::json!({"original":"文😀\"".repeat(20000)}),
                },
            };
            assert!(f.outcome(&outcome, true).is_err());
            if stage == "retained" {
                f.outcome(&outcome, false).unwrap();
                assert!(!f.pending().cleaned);
                proof.release(&descriptors).unwrap();
            } else {
                proof.stopped(&descriptors).unwrap();
                assert!(f.pending().process_cleanup_confirmed);
                assert_eq!(f.pending().dispatched_cleanup, stage == "dispatched");
                assert_eq!(f.pending().can_reject, stage == "admitted");
                assert!(
                    f.namespace
                        .command_dispatch(&f.folder, &f.lease, &f.permit, &f.worker)
                        .is_err()
                );
                f.outcome(&outcome, true).unwrap();
            }
            let saved = f.data();
            proof.stopped(&descriptors).unwrap();
            assert_eq!(
                f.data(),
                saved,
                "A lost receipt acknowledgement does not create a new decision"
            );
            let namespace = Namespace::open(f.home.path()).unwrap();
            let pending = namespace
                .pending_command(7, 11, mode, &f.recovery.command.invocation)
                .unwrap()
                .unwrap();
            assert!(pending.cleaned && pending.process_cleanup_confirmed);
            assert_eq!(pending.outcome.as_ref(), Some(&outcome));
            assert!(
                f.outcome(
                    &CommandOutcome::Uncertain {
                        message: "replacement".into()
                    },
                    true
                )
                .is_err()
            );
            namespace
                .access(false, |_, _, db| {
                    assert!(read(db, 7).is_err());
                    Ok(())
                })
                .unwrap();
        }
    }
}

#[tokio::test]
async fn original_process_receipts_never_initialize_replaced_state_and_keep_moved_folder_proof() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let f = Fixture::new(mode).await;
        let proof = f
            .namespace
            .command_process_cleanup(&f.folder, &f.lease, &f.permit, &f.worker)
            .unwrap();
        let descriptors = f.lease.command_descriptors(f.root.path()).unwrap();
        f.namespace
            .command_dispatch(&f.folder, &f.lease, &f.permit, &f.worker)
            .unwrap();
        let home_parked = f.home.path().with_extension("parked");
        std::fs::rename(f.home.path(), &home_parked).unwrap();
        assert!(proof.stopped(&descriptors).is_err());
        assert!(
            !f.home.path().exists(),
            "A missing original namespace cannot be initialized by a receipt"
        );
        std::fs::rename(&home_parked, f.home.path()).unwrap();
        let parked = f.root.path().with_extension("parked");
        std::fs::rename(f.root.path(), &parked).unwrap();
        std::fs::create_dir(f.root.path()).unwrap();
        std::fs::write(f.root.path().join("sentinel"), "replacement").unwrap();
        proof.stopped(&descriptors).unwrap();
        let pending = f.pending();
        assert!(pending.dispatched_cleanup && pending.outcome.is_none());
        assert!(pending.reopen().is_err());
        assert_eq!(
            std::fs::read_to_string(f.root.path().join("sentinel")).unwrap(),
            "replacement"
        );
        std::fs::remove_file(f.root.path().join("sentinel")).unwrap();
        std::fs::remove_dir(f.root.path()).unwrap();
        std::fs::rename(&parked, f.root.path()).unwrap();
        assert!(pending.reopen().is_ok());
        drop(descriptors);
        drop(f.lease);
        assert!(
            f.coordinator
                .acquire(Folder::capture(f.root.path()).unwrap())
                .await
                .is_err(),
            "A receipt alone grants no workspace access"
        );
    }
}

#[tokio::test]
async fn cancelling_another_original_command_keeps_live_process_receipts_readable() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let f = Fixture::new(mode).await;
        let proof = f
            .namespace
            .command_process_cleanup(&f.folder, &f.lease, &f.permit, &f.worker)
            .unwrap();
        let descriptors = f.lease.command_descriptors(f.root.path()).unwrap();
        let other_root = tempfile::tempdir().unwrap();
        let folder = Folder::capture(other_root.path()).unwrap();
        let mut recovery = original();
        recovery.scope.root.mode = mode;
        recovery.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "other".into());
        recovery.scope.id = "b".repeat(32);
        recovery
            .command
            .invocation
            .id
            .clone_from(&recovery.scope.id);
        recovery.command.phase = ActionPhase::Approved;
        recovery.activity.file_recovery = true;
        let authority =
            RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
        let permit = f
            .namespace
            .permit(&folder, &authority, &recovery.scope, recovery.activity)
            .unwrap()
            .with_origin(Some("http://127.0.0.1:3000/api"))
            .unwrap();
        let lease = f
            .coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        f.namespace
            .reserve_command(
                &folder,
                &lease,
                &permit,
                CommandIntent {
                    recovery: &recovery,
                    worker: &f.worker,
                },
            )
            .unwrap();
        recovery.command.phase = ActionPhase::Cancelled;
        recovery.activity = WorkspaceActivity::default();
        f.namespace
            .command_cancelled(&folder, &lease, &permit, &f.worker, &recovery)
            .unwrap();
        proof.validate(&descriptors).unwrap();
        f.namespace
            .command_dispatch(&f.folder, &f.lease, &f.permit, &f.worker)
            .unwrap();
        let outcome = CommandOutcome::Completed {
            result: serde_json::json!({"original":true}),
        };
        f.outcome(&outcome, false).unwrap();
        proof.release(&descriptors).unwrap();
        assert_eq!(f.pending().outcome, Some(outcome));
        assert!(f.pending().cleaned);
        let namespace = Namespace::open(f.home.path()).unwrap();
        let cancelled = namespace
            .pending_command(7, 11, mode, &recovery.command.invocation)
            .unwrap()
            .unwrap();
        assert!(cancelled.cancelled && cancelled.cleaned && cancelled.outcome.is_none());
    }
}
