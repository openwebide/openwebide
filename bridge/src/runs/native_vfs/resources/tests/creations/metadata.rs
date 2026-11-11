//! Durable creation witnesses also protect unpublished and last-link objects.
use super::*;
use std::os::unix::fs::MetadataExt;

fn change(path: &std::path::Path) {
    rustix::fs::setxattr(
        path,
        c"user.openwebide-created-update",
        b"external metadata",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
}
fn unchanged(path: &std::path::Path, object: &same_file::Handle) {
    assert_eq!(&same_file::Handle::from_path(path).unwrap(), object);
    let mut bytes = [0_u8; 64];
    let length =
        rustix::fs::getxattr(path, c"user.openwebide-created-update", &mut bytes[..]).unwrap();
    assert_eq!(&bytes[..length], b"external metadata");
}

fn claim_version(active: &Active, version: Option<u32>) {
    let path = active.directory.path().join(base()).join(format!(
        "receipts/{}.claim",
        active.record.progress.completed,
    ));
    let bytes = std::fs::read(&path).unwrap();
    let body = bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(3)
        .unwrap()
        .0
        + 1;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes[body..]).unwrap();
    assert_eq!(value["version"], 2);
    match version {
        Some(version) => {
            value["version"] = version.into();
        }
        None => {
            assert!(value.as_object_mut().unwrap().remove("version").is_some());
        }
    }
    let mut updated = bytes[..body].to_vec();
    updated.extend(serde_json::to_vec(&value).unwrap());
    std::fs::write(path, updated).unwrap();
}

#[tokio::test]
async fn archived_claims_keep_empty_artifact_and_anchor_recovery() {
    for (ordinal, stage) in [(2, 2), (3, 2), (3, 4)] {
        let mut active = Active::new(ordinal).await;
        active.stop_creation(stage);
        // Archived claim bodies have no version discriminator. The old file
        // creation order could also persist its anchor before its receipt.
        claim_version(&active, None);
        if stage == 4 {
            std::fs::remove_file(active.directory.path().join(base()).join("receipts/3.json"))
                .unwrap();
        }
        active.restore().await;
    }
}

#[tokio::test]
async fn future_claims_and_lost_receipts_never_adopt_current_metadata() {
    for future in [false, true] {
        let mut active = Active::new(3).await;
        active.stop_creation(if future { 2 } else { 4 });
        let receipt = active.directory.path().join(base()).join("receipts/3.json");
        if future {
            claim_version(&active, Some(3));
        } else {
            std::fs::remove_file(&receipt).unwrap();
        }
        let path = active.directory.path().join(active.temporary());
        let object = same_file::Handle::from_path(&path).unwrap();
        change(&path);
        active.reopen();
        assert!(
            transaction::recover(
                &active.files,
                &active.journal,
                active.record.clone(),
                owns_workspace
            )
            .await
            .is_err()
        );
        unchanged(&path, &object);
        assert!(
            !receipt.exists(),
            "Recovery must not manufacture a new baseline"
        );
        if !future {
            assert_eq!(
                same_file::Handle::from_path(
                    active.directory.path().join(base()).join("anchors/3")
                )
                .unwrap(),
                object,
            );
        }
    }
}

#[tokio::test]
async fn every_recorded_creation_boundary_preserves_later_metadata() {
    for ordinal in [2, 3] {
        let baseline = Active::new(ordinal).await;
        let total = baseline.stop_creation(0);
        for stage in 3..=total {
            let mut active = Active::new(ordinal).await;
            active.stop_creation(stage);
            let root = active.directory.path();
            let path = [
                root.join(active.temporary()),
                root.join(base()).join(format!("new/{ordinal}")),
                root.join(&active.path),
            ]
            .into_iter()
            .find(|path| path.exists())
            .unwrap();
            let object = same_file::Handle::from_path(&path).unwrap();
            change(&path);
            active.reopen();
            assert!(
                transaction::recover(
                    &active.files,
                    &active.journal,
                    active.record.clone(),
                    owns_workspace
                )
                .await
                .is_err(),
                "Creation metadata was discarded at ordinal={ordinal} stage={stage}",
            );
            unchanged(&path, &object);
        }
    }
}

#[test]
fn last_creation_anchor_preserves_later_metadata_after_rollback() {
    for external in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let files = files(&directory);
        files
            .create_owned(0, "created", VfsEntryKind::File)
            .unwrap();
        files
            .remove_created(0, "created", VfsEntryKind::File)
            .unwrap();
        let anchor = directory.path().join(base()).join("anchors/0");
        assert_eq!(std::fs::metadata(&anchor).unwrap().nlink(), 1);
        let object = same_file::Handle::from_path(&anchor).unwrap();
        if external {
            change(&anchor);
        }
        let result = futures::executor::block_on(files.finish_retained(&files.scope));
        if external {
            assert!(result.is_err());
            unchanged(&anchor, &object);
            assert_eq!(std::fs::read(&anchor).unwrap(), b"");
        } else {
            result.unwrap();
            assert!(!anchor.exists());
        }
    }
}

#[test]
fn archived_creation_receipts_keep_their_original_recovery_contract() {
    let directory = tempfile::tempdir().unwrap();
    let files = files(&directory);
    files
        .create_owned(0, "created", VfsEntryKind::File)
        .unwrap();
    let path = directory.path().join(base()).join("receipts/0.json");
    let bytes = std::fs::read(&path).unwrap();
    let body = bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(3)
        .unwrap()
        .0
        + 1;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes[body..]).unwrap();
    assert!(value.as_object_mut().unwrap().remove("revision").is_some());
    let mut archived = bytes[..body].to_vec();
    archived.extend(serde_json::to_vec(&value).unwrap());
    std::fs::write(path, archived).unwrap();
    // A historical receipt never claimed a native attribute baseline. It must
    // not acquire a new one by sampling the current object during recovery.
    change(&directory.path().join("created"));
    files
        .create_owned(0, "created", VfsEntryKind::File)
        .unwrap();
    files
        .remove_created(0, "created", VfsEntryKind::File)
        .unwrap();
    futures::executor::block_on(files.finish_retained(&files.scope)).unwrap();
    assert!(!directory.path().join(base()).exists());
}

#[tokio::test]
async fn partial_creation_namespace_intents_resume_and_unknown_intents_are_preserved() {
    for length in [0, 1, 80, usize::MAX] {
        let mut active = Active::new(3).await;
        active.stop_creation(1);
        let root = active.directory.path();
        let identity = active.files.files.root.identity("target/new").unwrap();
        let bytes = format!(
            "openwebide-created:1\n{}\n{}\n2\ndirectory-namespace:1\n{}\n",
            active.files.files.scope.transaction,
            active.files.files.scope.fingerprint,
            identity.fingerprint(),
        )
        .into_bytes();
        std::fs::write(
            root.join(base()).join("receipts/2.namespace.part"),
            &bytes[..length.min(bytes.len())],
        )
        .unwrap();
        active.restore().await;
    }
    for suffix in ["namespace", "namespace.part"] {
        let mut active = Active::new(3).await;
        active.stop_creation(1);
        let path = active
            .directory
            .path()
            .join(base())
            .join(format!("receipts/2.{suffix}"));
        std::fs::write(&path, b"unrelated metadata").unwrap();
        active.reopen();
        assert!(
            transaction::recover(
                &active.files,
                &active.journal,
                active.record.clone(),
                owns_workspace
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"unrelated metadata");
    }
}
