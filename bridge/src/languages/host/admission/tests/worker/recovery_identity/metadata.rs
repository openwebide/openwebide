//! Accepted work must preserve metadata changes on the same original object.
use super::*;
use std::{
    fs::{File, FileTimes, Permissions},
    os::unix::fs::PermissionsExt,
    time::Duration,
};

#[derive(Clone, Copy, Debug)]
enum Change {
    Permissions,
    Modified,
    RestoredWrite,
    Link,
}

async fn changed(mode: WorkspaceMode, checkpoint: Checkpoint) {
    for change in [
        Change::Permissions,
        Change::Modified,
        Change::RestoredWrite,
        Change::Link,
    ] {
        let accepted = accepted(mode, checkpoint).await;
        let editor = accepted
            .backend
            .store
            .editor_recovery(UserId::new(accepted.command.user), accepted.command.project)
            .await
            .unwrap();
        let metadata = std::fs::metadata(&accepted.path).unwrap();
        let outside = tempfile::tempdir().unwrap();
        match change {
            Change::Permissions => std::fs::set_permissions(
                &accepted.path,
                Permissions::from_mode(metadata.permissions().mode() ^ 0o100),
            )
            .unwrap(),
            Change::Modified => File::options()
                .write(true)
                .open(&accepted.path)
                .unwrap()
                .set_times(
                    FileTimes::new()
                        .set_modified(metadata.modified().unwrap() + Duration::from_secs(5)),
                )
                .unwrap(),
            Change::RestoredWrite => {
                let bytes = std::fs::read(&accepted.path).unwrap();
                std::fs::write(&accepted.path, "an external write").unwrap();
                std::fs::write(&accepted.path, bytes).unwrap();
                File::options()
                    .write(true)
                    .open(&accepted.path)
                    .unwrap()
                    .set_times(FileTimes::new().set_modified(metadata.modified().unwrap()))
                    .unwrap();
            }
            Change::Link => {
                std::fs::hard_link(&accepted.path, outside.path().join("linked-original")).unwrap();
            }
        }
        assert_eq!(
            same_file::Handle::from_path(&accepted.path).unwrap(),
            accepted.original
        );
        let queries = query_count(&accepted.fixture);
        let result = accepted
            .execute(if checkpoint == Checkpoint::Prepared {
                WorkspaceTransactionAction::Apply
            } else {
                WorkspaceTransactionAction::Recover
            })
            .await;
        assert!(
            result
                .as_ref()
                .map_or(true, |outcome| outcome.error.is_some()),
            "Accepted recovery moved or cleaned a changed original object: mode={mode:?}, change={change:?}, result={result:?}"
        );
        assert_eq!(
            same_file::Handle::from_path(&accepted.path).unwrap(),
            accepted.original
        );
        if checkpoint == Checkpoint::Prepared {
            assert_eq!(accepted.backend.intent_count.load(Ordering::SeqCst), 0);
            assert_eq!(
                result.as_ref().unwrap().phase,
                Some(TransactionPhase::Restored),
                "A pre-intent rejection must not strand the workspace"
            );
        }
        assert_eq!(
            accepted
                .backend
                .store
                .editor_recovery(UserId::new(accepted.command.user), accepted.command.project)
                .await
                .unwrap(),
            editor
        );
        assert_eq!(query_count(&accepted.fixture), queries);
        assert!(!accepted.fixture.root.path().join("executions").exists());
    }
}

#[tokio::test]
async fn remote_accepted_metadata_prepared() {
    changed(WorkspaceMode::Remote, Checkpoint::Prepared).await;
}
#[tokio::test]
async fn paired_accepted_metadata_prepared() {
    changed(WorkspaceMode::Local, Checkpoint::Prepared).await;
}
#[tokio::test]
async fn remote_accepted_metadata_ambiguous_directory() {
    changed(WorkspaceMode::Remote, Checkpoint::AmbiguousTransfer).await;
}
#[tokio::test]
async fn paired_accepted_metadata_ambiguous_directory() {
    changed(WorkspaceMode::Local, Checkpoint::AmbiguousTransfer).await;
}
#[tokio::test]
async fn remote_accepted_metadata_cleanup() {
    changed(WorkspaceMode::Remote, Checkpoint::Cleanup).await;
}
#[tokio::test]
async fn paired_accepted_metadata_cleanup() {
    changed(WorkspaceMode::Local, Checkpoint::Cleanup).await;
}

async fn partial_cleanup(mode: WorkspaceMode, external_change: bool) {
    let accepted = accepted(mode, Checkpoint::Cleanup).await;
    let folder = Folder::capture(&accepted.fixture.scope.root).unwrap();
    let lease = accepted
        .fixture
        .host
        .inner
        .coordinator
        .acquire(folder.clone())
        .await
        .unwrap();
    let scope = ResourceScope {
        transaction: accepted.state.journal.id.clone(),
        fingerprint: accepted.state.journal.plan.fingerprint().unwrap(),
    };
    let files = crate::runs::native_vfs::resources::ResourceFiles::reviewed(
        folder.handle.as_file(),
        &accepted.state.journal.plan.root,
        scope.clone(),
        accepted.state.language_review.as_deref(),
        &accepted.state.journal.plan,
    )
    .unwrap();
    files
        .remove_retained_entry(
            &scope,
            "0/sibling.json",
            openwebide_core::vfs::VfsEntryKind::File,
        )
        .await
        .unwrap();
    drop(lease);
    let editor = accepted
        .backend
        .store
        .editor_recovery(UserId::new(accepted.command.user), accepted.command.project)
        .await
        .unwrap();
    if external_change {
        let mode = std::fs::metadata(&accepted.path)
            .unwrap()
            .permissions()
            .mode();
        std::fs::set_permissions(&accepted.path, Permissions::from_mode(mode ^ 0o100)).unwrap();
    }
    let queries = query_count(&accepted.fixture);
    let outcome = accepted
        .execute(WorkspaceTransactionAction::Recover)
        .await
        .unwrap();
    if external_change {
        assert!(outcome.error.is_some());
        assert_eq!(
            same_file::Handle::from_path(&accepted.path).unwrap(),
            accepted.original
        );
    } else {
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
        assert!(!accepted.path.exists());
    }
    assert_eq!(
        accepted
            .backend
            .store
            .editor_recovery(UserId::new(accepted.command.user), accepted.command.project)
            .await
            .unwrap(),
        editor
    );
    assert_eq!(query_count(&accepted.fixture), queries);
}
#[tokio::test]
async fn remote_accepted_metadata_partial_cleanup() {
    partial_cleanup(WorkspaceMode::Remote, false).await;
    partial_cleanup(WorkspaceMode::Remote, true).await;
}
#[tokio::test]
async fn paired_accepted_metadata_partial_cleanup() {
    partial_cleanup(WorkspaceMode::Local, false).await;
    partial_cleanup(WorkspaceMode::Local, true).await;
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Attribute {
    Extended,
    Acl,
    Flags,
}
fn extended_attribute(path: &std::path::Path, name: &std::ffi::CStr) -> Vec<u8> {
    let mut value = [0_u8; 256];
    let length = rustix::fs::getxattr(path, name, &mut value[..]).unwrap();
    value[..length].to_vec()
}
#[cfg(target_os = "macos")]
fn root_acl(path: &std::path::Path) -> Vec<u8> {
    let result = std::process::Command::new("/bin/ls")
        .arg("-lde")
        .arg(path)
        .output()
        .unwrap();
    assert!(result.status.success());
    String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}
#[cfg(target_os = "linux")]
fn root_acl(path: &std::path::Path) -> Vec<u8> {
    extended_attribute(path, c"system.posix_acl_access")
}
fn root_flags(path: &std::path::Path) -> u32 {
    let file = File::open(path).unwrap();
    #[cfg(target_os = "macos")]
    {
        rustix::fs::fstat(&file).unwrap().st_flags
    }
    #[cfg(target_os = "linux")]
    {
        rustix::fs::ioctl_getflags(&file).unwrap().bits()
    }
}
impl Attribute {
    pub(super) fn set(self, path: &std::path::Path) {
        match self {
            Self::Extended => rustix::fs::setxattr(
                path,
                c"user.openwebide-recovery-property",
                b"external update after the original move",
                rustix::fs::XattrFlags::empty(),
            )
            .unwrap(),
            Self::Acl => {
                #[cfg(target_os = "macos")]
                {
                    let result = std::process::Command::new("/bin/chmod")
                        .args(["+a", "everyone allow read,write"])
                        .arg(path)
                        .output()
                        .unwrap();
                    assert!(
                        result.status.success(),
                        "{}",
                        String::from_utf8_lossy(&result.stderr)
                    );
                }
                #[cfg(target_os = "linux")]
                {
                    use std::os::unix::fs::MetadataExt;
                    let mode = std::fs::metadata(path).unwrap().mode();
                    let mut bytes = 2_u32.to_le_bytes().to_vec();
                    for (tag, permission, id) in [
                        (1_u16, u16::try_from((mode >> 6) & 7).unwrap(), u32::MAX),
                        (2, 4, 0),
                        (4, u16::try_from((mode >> 3) & 7).unwrap(), u32::MAX),
                        (16, u16::try_from((mode >> 3) & 7).unwrap(), u32::MAX),
                        (32, u16::try_from(mode & 7).unwrap(), u32::MAX),
                    ] {
                        bytes.extend(tag.to_le_bytes());
                        bytes.extend(permission.to_le_bytes());
                        bytes.extend(id.to_le_bytes());
                    }
                    rustix::fs::setxattr(
                        path,
                        c"system.posix_acl_access",
                        &bytes,
                        rustix::fs::XattrFlags::empty(),
                    )
                    .unwrap();
                }
            }
            Self::Flags => {
                let file = File::open(path).unwrap();
                #[cfg(target_os = "macos")]
                {
                    use std::os::fd::AsRawFd;
                    // SAFETY: file owns the directory descriptor for this call.
                    assert_eq!(
                        unsafe {
                            libc::fchflags(file.as_raw_fd(), root_flags(path) | libc::UF_NODUMP)
                        },
                        0
                    );
                }
                #[cfg(target_os = "linux")]
                rustix::fs::ioctl_setflags(
                    &file,
                    rustix::fs::ioctl_getflags(&file).unwrap() | rustix::fs::IFlags::NODUMP,
                )
                .unwrap();
            }
        }
    }
    pub(super) fn read(self, path: &std::path::Path) -> Vec<u8> {
        match self {
            Self::Extended => extended_attribute(path, c"user.openwebide-recovery-property"),
            Self::Acl => root_acl(path),
            Self::Flags => root_flags(path).to_le_bytes().to_vec(),
        }
    }
}

async fn moved_root_attribute(mode: WorkspaceMode, checkpoint: Checkpoint, attribute: Attribute) {
    use std::os::unix::fs::MetadataExt;
    for external_change in [false, true] {
        let accepted = accepted(mode, checkpoint).await;
        // The retained directory itself was renamed. Unlike its untouched
        // descendants, it must permit the interpreter's own ctime transition.
        let root = accepted.path.parent().unwrap().parent().unwrap();
        let original = same_file::Handle::from_path(root).unwrap();
        let editor = accepted
            .backend
            .store
            .editor_recovery(UserId::new(accepted.command.user), accepted.command.project)
            .await
            .unwrap();
        let queries = query_count(&accepted.fixture);
        let mut expected_attribute = None;
        if external_change {
            let before = std::fs::metadata(root).unwrap();
            attribute.set(root);
            expected_attribute = Some(attribute.read(root));
            let after = std::fs::metadata(root).unwrap();
            assert_eq!(
                (
                    before.mode(),
                    before.nlink(),
                    before.uid(),
                    before.gid(),
                    before.size(),
                    before.modified().unwrap(),
                ),
                (
                    after.mode(),
                    after.nlink(),
                    after.uid(),
                    after.gid(),
                    after.size(),
                    after.modified().unwrap(),
                )
            );
            assert_eq!(same_file::Handle::from_path(root).unwrap(), original);
        }
        let result = accepted.execute(WorkspaceTransactionAction::Recover).await;
        if external_change {
            assert!(
                result
                    .as_ref()
                    .map_or(true, |outcome| outcome.error.is_some()),
                "Accepted recovery moved or deleted a directory with externally changed attributes: mode={mode:?}, attribute={attribute:?}, result={result:?}"
            );
            assert_eq!(same_file::Handle::from_path(root).unwrap(), original);
            assert!(accepted.path.is_file());
            assert_eq!(attribute.read(root), expected_attribute.unwrap());
        } else {
            let outcome = result.unwrap();
            assert!(outcome.error.is_none(), "{:?}", outcome.error);
            assert_eq!(
                outcome.phase,
                Some(if checkpoint == Checkpoint::Cleanup {
                    TransactionPhase::Committed
                } else {
                    TransactionPhase::Restored
                })
            );
            assert!(!root.exists());
        }
        assert_eq!(query_count(&accepted.fixture), queries);
        assert_eq!(
            accepted
                .backend
                .store
                .editor_recovery(UserId::new(accepted.command.user), accepted.command.project)
                .await
                .unwrap(),
            editor
        );
        assert!(!accepted.fixture.root.path().join("executions").exists());
    }
}

async fn moved_root_attributes(mode: WorkspaceMode, checkpoint: Checkpoint) {
    for attribute in [Attribute::Extended, Attribute::Acl, Attribute::Flags] {
        moved_root_attribute(mode, checkpoint, attribute).await;
    }
}

#[tokio::test]
async fn remote_accepted_ambiguous_directory_preserves_changed_root_attribute() {
    moved_root_attributes(WorkspaceMode::Remote, Checkpoint::AmbiguousTransfer).await;
}
#[tokio::test]
async fn paired_accepted_ambiguous_directory_preserves_changed_root_attribute() {
    moved_root_attributes(WorkspaceMode::Local, Checkpoint::AmbiguousTransfer).await;
}
#[tokio::test]
async fn remote_accepted_cleanup_preserves_changed_root_attribute() {
    moved_root_attributes(WorkspaceMode::Remote, Checkpoint::Cleanup).await;
}
#[tokio::test]
async fn paired_accepted_cleanup_preserves_changed_root_attribute() {
    moved_root_attributes(WorkspaceMode::Local, Checkpoint::Cleanup).await;
}
