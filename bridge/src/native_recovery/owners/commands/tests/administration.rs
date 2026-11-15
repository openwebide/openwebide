use super::*;
use crate::native_recovery::administration::{inspect, retire};
const NOTE: &str =
    "Reviewed the original file and command effects; the original issuer is decommissioned";
fn entry(namespace: &Namespace, kind: &str) -> serde_json::Value {
    inspect(namespace).unwrap()[kind][0].clone()
}
fn retirement(
    namespace: &Namespace,
    entry: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    retire(
        namespace,
        entry["id"].as_str().unwrap(),
        entry["expected"].as_str().unwrap(),
        NOTE,
    )
}
fn bytes(namespace: &Namespace) -> Vec<u8> {
    namespace
        .access(false, |_, _, db| Ok(db.serialize("main").unwrap().to_vec()))
        .unwrap()
}
#[tokio::test]
async fn os_owner_retirement_rechecks_original_folder_and_snapshot_and_fences_late_credentials() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for legacy in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("project");
            std::fs::create_dir(&path).unwrap();
            let coordinator = Coordinator::default();
            coordinator.set_recovery_home(Arc::new(home.path().into()));
            let namespace = Namespace::open(home.path()).unwrap();
            let folder = Folder::capture(&path).unwrap();
            let mut recovery = original();
            recovery.scope.root.mode = mode;
            recovery.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "project".into());
            recovery.activity = WorkspaceActivity {
                file_recovery: true,
                language_command: false,
            };
            let authority =
                RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
            let permit = namespace
                .permit(&folder, &authority, &recovery.scope, recovery.activity)
                .unwrap();
            let lease = coordinator
                .acquire_recovery(folder.clone(), permit.clone())
                .await
                .unwrap();
            namespace.enroll(&folder, &lease, &permit).unwrap();
            let stale = entry(&namespace, "owners");
            let before = bytes(&namespace);
            assert!(
                retirement(&namespace, &stale).is_err(),
                "A live file owner lost its physical claim"
            );
            assert_eq!(bytes(&namespace), before);
            namespace.files_admitted(&folder, &lease, &permit).unwrap();
            if legacy {
                namespace
                    .access(true, |parent, anchor, db| {
                        let mut owner = super::super::super::read(db, version(db)?)?.remove(0);
                        owner.format = 1;
                        owner.credential = None;
                        owner.origin = None;
                        owner.files = FileStage::Unknown;
                        super::super::super::context::save_owner(db, &owner)?;
                        namespace.save(parent, anchor, db)
                    })
                    .unwrap();
            }
            drop(lease);
            assert!(retirement(&namespace, &stale).is_err());
            if legacy {
                for command in [true, false] {
                    namespace
                        .access(true, |parent, anchor, db| {
                            let mut owner = super::super::super::read(db, version(db)?)?.remove(0);
                            owner.command = command;
                            super::super::super::context::save_owner(db, &owner)?;
                            namespace.save(parent, anchor, db)
                        })
                        .unwrap();
                    if command {
                        let blocked = entry(&namespace, "owners");
                        assert!(blocked["retirement_blocked"].is_string());
                        let before = bytes(&namespace);
                        assert!(retirement(&namespace, &blocked).is_err());
                        assert_eq!(bytes(&namespace), before);
                    }
                }
            }
            let current = entry(&namespace, "owners");
            let text = inspect(&namespace).unwrap().to_string();
            assert!(!text.contains(&authority.secret));
            std::fs::rename(&path, root.path().join("original")).unwrap();
            std::fs::create_dir(&path).unwrap();
            let before = bytes(&namespace);
            assert!(retirement(&namespace, &current).is_err());
            assert_eq!(bytes(&namespace), before);
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(root.path().join("original"), &path).unwrap();
            let saved = retirement(&namespace, &current).unwrap();
            assert_eq!(saved["original"], current);
            assert_eq!(retirement(&namespace, &current).unwrap(), saved);
            assert!(
                inspect(&namespace).unwrap()["owners"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            let reopened = Namespace::open_existing(home.path()).unwrap();
            assert_eq!(inspect(&reopened).unwrap()["retired"][0], saved);
            drop(coordinator.acquire(folder.clone()).await.unwrap());
            assert!(
                coordinator
                    .acquire_recovery(folder.clone(), permit)
                    .await
                    .is_err()
            );
            let changed_secret =
                RecoveryAuthority::for_scope(7, 11, &recovery.scope, "Q".repeat(43)).unwrap();
            let wrong = reopened
                .permit(&folder, &changed_secret, &recovery.scope, recovery.activity)
                .unwrap();
            assert!(
                coordinator
                    .acquire_recovery(folder.clone(), wrong)
                    .await
                    .is_err()
            );
            recovery.scope.id = "e".repeat(32);
            let next =
                RecoveryAuthority::for_scope(7, 11, &recovery.scope, "Q".repeat(43)).unwrap();
            let next = reopened
                .permit(&folder, &next, &recovery.scope, recovery.activity)
                .unwrap();
            let lease = coordinator
                .acquire_recovery(folder.clone(), next.clone())
                .await
                .unwrap();
            reopened.enroll(&folder, &lease, &next).unwrap();
            drop(lease);
        }
    }
}
#[tokio::test]
async fn retirement_never_invents_command_cleanup_or_rewrites_an_original_outcome() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.path().into()));
        let namespace = Namespace::open(home.path()).unwrap();
        let (folder, lease, permit, worker, _) =
            super::settlements::prepared(&namespace, &coordinator, root.path(), mode, 1).await;
        namespace
            .command_admitted(&folder, &lease, &permit, &worker)
            .unwrap();
        let receipt = namespace
            .command_process_cleanup(&folder, &lease, &permit, &worker)
            .unwrap();
        namespace
            .command_dispatch(&folder, &lease, &permit, &worker)
            .unwrap();
        let outcome = CommandOutcome::Uncertain {
            message: "Original effects 文😀 \"quoted\"".into(),
        };
        namespace
            .command_outcome(&folder, &lease, &permit, &worker, &outcome, false)
            .unwrap();
        let descriptors = lease.command_descriptors(root.path()).unwrap();
        drop(lease);
        let current = entry(&namespace, "owners");
        assert_eq!(current["outcome"], serde_json::to_value(&outcome).unwrap());
        assert!(current["retirement_blocked"].is_string());
        let before = bytes(&namespace);
        assert!(retirement(&namespace, &current).is_err());
        assert_eq!(bytes(&namespace), before);
        // This original private capability is what the watcher calls after
        // positively awaiting its own managed group; administration cannot do it.
        receipt.stopped(&descriptors).unwrap();
        let cleaned = entry(&namespace, "owners");
        assert_eq!(cleaned["outcome"], current["outcome"]);
        assert!(
            retirement(&namespace, &current).is_err(),
            "A stale unconfirmed view authorized retirement"
        );
        assert!(
            retirement(&namespace, &cleaned).is_err(),
            "Retained original watcher descriptors were ignored"
        );
        drop(descriptors);
        let saved = retirement(&namespace, &cleaned).unwrap();
        assert_eq!(
            saved["original"]["outcome"],
            serde_json::to_value(&outcome).unwrap()
        );
        let reopened = Namespace::open_existing(home.path()).unwrap();
        assert_eq!(
            inspect(&reopened).unwrap()["retired"][0]["original"],
            cleaned
        );
        drop(coordinator.acquire(folder).await.unwrap());
    }
}
#[tokio::test]
async fn os_owner_can_retire_only_the_exact_non_fencing_settlement_without_folder_or_backend() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.path().into()));
        let namespace = Namespace::open(home.path()).unwrap();
        let (folder, lease, permit, worker, mut recovery) =
            super::settlements::prepared(&namespace, &coordinator, root.path(), mode, 1).await;
        let outcome = CommandOutcome::Rejected {
            message: "Original command never dispatched".into(),
        };
        namespace
            .command_outcome(&folder, &lease, &permit, &worker, &outcome, true)
            .unwrap();
        recovery.command.phase = ActionPhase::Acknowledged;
        recovery.command.outcome_digest = Some(openwebide_core::plugins::content_digest(
            &serde_json::to_vec(&outcome).unwrap(),
        ));
        let mut inactive = worker.state();
        inactive.active = false;
        recovery.command.worker = Some(inactive);
        recovery.command.command_cleanup_confirmed = true;
        recovery.command.native_settlement_pending = false;
        recovery.activity.language_command = false;
        namespace
            .command_settled(&folder, &lease, &permit, &recovery)
            .unwrap();
        drop(lease);
        let pending = namespace.pending_command_settlements().unwrap().remove(0);
        let current = entry(&namespace, "settlements");
        assert!(
            !inspect(&namespace)
                .unwrap()
                .to_string()
                .contains(&worker.secret)
        );
        std::fs::rename(root.path(), home.path().join("unavailable-workspace")).unwrap();
        retirement(&namespace, &current).unwrap();
        assert!(!namespace.has_command_settlement(&pending).unwrap());
        assert!(namespace.pending_command_settlements().unwrap().is_empty());
        assert_eq!(
            inspect(&namespace).unwrap()["retired"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // Normal commands must still settle after an OS-owner retirement. This
        // exercises schema evolution through behavior, not a version assertion.
        std::fs::rename(home.path().join("unavailable-workspace"), root.path()).unwrap();
        let (folder, lease, permit, worker, mut next) =
            super::settlements::prepared(&namespace, &coordinator, root.path(), mode, 2).await;
        namespace
            .command_outcome(&folder, &lease, &permit, &worker, &outcome, true)
            .unwrap();
        next.command.phase = ActionPhase::Acknowledged;
        next.command.outcome_digest = Some(openwebide_core::plugins::content_digest(
            &serde_json::to_vec(&outcome).unwrap(),
        ));
        let mut inactive = worker.state();
        inactive.active = false;
        next.command.worker = Some(inactive);
        next.command.command_cleanup_confirmed = true;
        next.command.native_settlement_pending = false;
        next.activity.language_command = false;
        namespace
            .command_settled(&folder, &lease, &permit, &next)
            .unwrap();
        drop(lease);
        let reopened = Namespace::open_existing(home.path()).unwrap();
        assert_eq!(reopened.pending_command_settlements().unwrap().len(), 1);
        assert_eq!(
            inspect(&reopened).unwrap()["retired"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(!reopened.has_command_settlement(&pending).unwrap());
        drop(coordinator.acquire(folder).await.unwrap());
    }
}
