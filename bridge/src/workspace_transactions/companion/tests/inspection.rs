use super::*;
use openwebide_core::workspace_transaction::companion::{
    FileValue, FolderInspection, FolderInspector, InspectedFolder, NativeFolderInspection,
};

struct Remote {
    host: Host,
    root: PathBuf,
}
impl FolderInspector for Remote {
    async fn inspect(&self, request: &FolderInspection) -> Result<FileValue, String> {
        let request = NativeFolderInspection {
            user: 1,
            project: 1,
            path: String::new(),
            inspection: request.clone(),
        };
        let serialized = serde_json::to_vec(&request).unwrap();
        let value = self
            .host
            .inspect(
                self.root.clone(),
                serde_json::from_slice(&serialized).unwrap(),
            )
            .await?;
        Ok(serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap())
    }
}
async fn remote(case: &Case) -> InspectedFolder<Remote> {
    let identity = Transactions::root_identity(&case.root(), "", 1, 1).unwrap();
    InspectedFolder::new(
        Remote {
            host: case.host.clone(),
            root: case.root(),
        },
        identity,
    )
    .unwrap()
}

#[tokio::test]
async fn local_and_remote_review_readers_share_complete_raw_inventories_and_native_type_checks() {
    let case = Case::new();
    std::fs::create_dir(case.root().join("source")).unwrap();
    std::fs::write(case.root().join("source/file"), "original").unwrap();
    let local = case.files().await;
    let remote = remote(&case).await;
    let operations = vec![
        openwebide_core::workspace_transaction::FileOperation::Rename {
            from: "source".into(),
            to: "moved".into(),
            overwrite: false,
            ignore_if_exists: false,
        },
    ];
    let local_plan = openwebide_core::workspace_transaction::prepare_native(
        &local,
        local.root_identity().into(),
        operations.clone(),
        || true,
    )
    .await
    .unwrap();
    let remote_plan = openwebide_core::workspace_transaction::prepare_native(
        &remote,
        remote.root_identity().into(),
        operations.clone(),
        || true,
    )
    .await
    .unwrap();
    assert_eq!(local_plan.version, 2);
    assert_eq!(local_plan, remote_plan);
    // A private child may not disappear from review just because normal file
    // views hide it. Neither reader can approve moving this opaque subtree.
    let private = format!(".openwebide-proof-{}", "a".repeat(32));
    std::fs::write(case.root().join("source").join(&private), "private payload").unwrap();
    for entries in [
        local.list("source").await.unwrap(),
        remote.list("source").await.unwrap(),
    ] {
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| entry.name == private));
    }
    assert!(
        openwebide_core::workspace_transaction::prepare_native(
            &local,
            local.root_identity().into(),
            operations.clone(),
            || true
        )
        .await
        .is_err()
    );
    assert!(
        openwebide_core::workspace_transaction::prepare_native(
            &remote,
            remote.root_identity().into(),
            operations,
            || true
        )
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(case.root().join("source").join(private)).unwrap(),
        "private payload"
    );
    assert!(!case.root().join("moved").exists());
    assert!(!case.root().join(".openwebide").exists());
}

#[tokio::test]
async fn inspection_rejects_mutations_bad_roots_and_replaced_folders_before_access() {
    let case = Case::new();
    std::fs::create_dir(case.root().join("project")).unwrap();
    std::fs::write(case.root().join("project/file"), "original").unwrap();
    let identity = Transactions::root_identity(&case.root(), "project", 1, 1).unwrap();
    let request = |action| NativeFolderInspection {
        user: 1,
        project: 1,
        path: "project".into(),
        inspection: FolderInspection {
            root: identity.clone(),
            action,
        },
    };
    assert!(
        case.host
            .inspect(
                case.root(),
                request(FileAction::Create {
                    ordinal: 0,
                    path: "created".into(),
                    is_dir: false
                })
            )
            .await
            .is_err()
    );
    assert!(!case.root().join("project/created").exists());
    assert!(!case.root().join("project/.openwebide").exists());
    let mut wrong = request(FileAction::Read {
        path: "file".into(),
    });
    wrong.inspection.root = "f".repeat(64);
    assert!(case.host.inspect(case.root(), wrong).await.is_err());
    std::fs::rename(case.root().join("project"), case.root().join("kept")).unwrap();
    std::fs::create_dir(case.root().join("project")).unwrap();
    std::fs::write(case.root().join("project/file"), "replacement").unwrap();
    assert!(
        case.host
            .inspect(
                case.root(),
                request(FileAction::Read {
                    path: "file".into()
                })
            )
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(case.root().join("kept/file")).unwrap(),
        "original"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn inspection_preserves_links_and_special_objects_in_inventory_and_rejects_reading_them() {
    use std::{ffi::CString, os::unix::fs::symlink};
    let case = Case::new();
    std::fs::write(case.root().join("ordinary"), "ordinary").unwrap();
    symlink("ordinary", case.root().join("link")).unwrap();
    let fifo = CString::new(case.root().join("pipe").to_str().unwrap()).unwrap();
    // A real named pipe proves the reader rejects its type before any blocking
    // open/read; it is never replaced or dereferenced during review.
    // SAFETY: fifo is a live, NUL-terminated path in this owned temporary directory.
    // SAFETY: fifo is a live, NUL-terminated path in this owned temporary directory.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    async fn check(files: &impl WorkspaceEntries) {
        let entries = files.list("").await.unwrap();
        assert_eq!(entries.len(), 3);
        for path in ["link", "pipe"] {
            assert!(files.canonicalize(path).await.is_err());
            assert!(files.read_bytes(path).await.is_err());
        }
    }
    check(&case.files().await).await;
    check(&remote(&case).await).await;
    assert!(
        std::fs::symlink_metadata(case.root().join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
async fn inspection_blocks_reserved_case_aliases_but_reads_public_notes_in_the_control_parent() {
    let case = Case::new();
    std::fs::create_dir(case.root().join(".spin")).unwrap();
    std::fs::write(case.root().join(".spin/secret"), "runtime secret").unwrap();
    std::fs::create_dir(case.root().join(".openwebide")).unwrap();
    std::fs::write(
        case.root().join(".openwebide/recovery-notes"),
        "ordinary notes",
    )
    .unwrap();
    for path in [".spin", ".SPIN", ".ſpin", ".openwebide", ".OPENWEBIDE"] {
        assert!(Transactions::root_identity(&case.root(), path, 1, 1).is_err());
    }
    let local = case.files().await;
    let remote = remote(&case).await;
    async fn check(files: &impl WorkspaceEntries) {
        for path in [".SPIN/secret", ".ſpin/secret"] {
            assert!(files.canonicalize(path).await.is_err());
            assert!(files.read_bytes(path).await.is_err());
        }
        assert_eq!(
            files
                .read_bytes(".openwebide/recovery-notes")
                .await
                .unwrap(),
            b"ordinary notes"
        );
    }
    check(&local).await;
    check(&remote).await;
    assert_eq!(
        std::fs::read_to_string(case.root().join(".spin/secret")).unwrap(),
        "runtime secret"
    );
}
