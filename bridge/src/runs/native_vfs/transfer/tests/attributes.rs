use super::*;
use std::{
    ffi::{CStr, CString},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    },
    time::SystemTime,
};

const ATTRIBUTE: &CStr = c"user.openwebide-resource-test";

fn set_attribute(path: &Path, name: &CStr, value: &[u8]) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: Path/name are live NUL-terminated strings and value is readable
    // for the supplied length throughout this call.
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    // SAFETY: Same string/buffer lifetimes as the Darwin call above.
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    assert_eq!(status, 0, "{}", std::io::Error::last_os_error());
}
fn attribute(path: &Path, name: &CStr) -> Vec<u8> {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut bytes = vec![0_u8; 256];
    // SAFETY: Path/name remain valid and bytes is writable for its full length.
    #[cfg(target_os = "macos")]
    let length = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            0,
            0,
        )
    };
    // SAFETY: Same string/buffer lifetimes as the Darwin call above.
    #[cfg(target_os = "linux")]
    let length = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    assert!(length >= 0, "{}", std::io::Error::last_os_error());
    bytes.truncate(usize::try_from(length).unwrap());
    bytes
}

#[cfg(target_os = "macos")]
fn set_acl(path: &Path) {
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
#[cfg(target_os = "macos")]
fn acl(path: &Path) -> Vec<u8> {
    let entries = acl_lines(path);
    assert!(
        !entries.is_empty(),
        "The disposable object must have a native ACL"
    );
    entries
}
#[cfg(target_os = "macos")]
fn acl_lines(path: &Path) -> Vec<u8> {
    let result = std::process::Command::new("/bin/ls")
        .arg("-lde")
        .arg(path)
        .output()
        .unwrap();
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    let entries = text.lines().skip(1).collect::<Vec<_>>().join("\n");
    entries.into_bytes()
}

#[cfg(target_os = "linux")]
fn set_acl(path: &Path) {
    set_attribute(path, c"system.posix_acl_access", &linux_acl());
}
#[cfg(target_os = "linux")]
fn linux_acl() -> Vec<u8> {
    // Linux POSIX ACL xattr format: version 2 and ordered tag/permission/id
    // entries. A named root entry makes this an extended ACL rather than mode
    // bits alone. All objects are private, disposable test files/directories.
    let mut bytes = 2_u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1_u16, 7_u16, u32::MAX),
        (2, 4, 0),
        (4, 5, u32::MAX),
        (16, 5, u32::MAX),
        (32, 1, u32::MAX),
    ] {
        bytes.extend(tag.to_le_bytes());
        bytes.extend(permissions.to_le_bytes());
        bytes.extend(id.to_le_bytes());
    }
    bytes
}
#[cfg(target_os = "linux")]
fn acl(path: &Path) -> Vec<u8> {
    attribute(path, c"system.posix_acl_access")
}

#[derive(Debug, PartialEq, Eq)]
struct Attributes {
    device: u64,
    inode: u64,
    links: u64,
    owner: (u32, u32),
    mode: u32,
    modified: SystemTime,
    accessed: SystemTime,
    created: Option<SystemTime>,
    attribute: Vec<u8>,
    acl: Vec<u8>,
}
impl Attributes {
    fn read(path: &Path) -> Self {
        let metadata = std::fs::metadata(path).unwrap();
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            links: metadata.nlink(),
            owner: (metadata.uid(), metadata.gid()),
            mode: metadata.mode(),
            modified: metadata.modified().unwrap(),
            accessed: metadata.accessed().unwrap(),
            created: metadata.created().ok(),
            attribute: attribute(path, ATTRIBUTE),
            acl: acl(path),
        }
    }
    fn install(path: &Path) -> Self {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o751)).unwrap();
        set_attribute(path, ATTRIBUTE, &[0, 255, 1, 254]);
        set_acl(path);
        Self::read(path)
    }
}

#[tokio::test]
async fn file_and_directory_round_trips_preserve_inodes_hard_links_acls_and_extended_attributes() {
    for directory in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let vfs = NativeFsVfs {
            root: root.path().canonicalize().unwrap(),
        };
        let kind = if directory {
            VfsEntryKind::Directory
        } else {
            VfsEntryKind::File
        };
        let leaf = if directory {
            "source/nested/file"
        } else {
            "source"
        };
        vfs.write_bytes(leaf, &[0, 255, 1]).await.unwrap();
        vfs.create("held", VfsEntryKind::Directory).await.unwrap();
        std::fs::hard_link(root.path().join(leaf), root.path().join("linked")).unwrap();
        let source = root.path().join("source");
        let handle = original(&source);
        let attributes = Attributes::install(&source);
        let child = directory.then(|| Attributes::install(&root.path().join(leaf)));
        vfs.move_atomic("source", "held/original", kind)
            .await
            .unwrap();
        let retained = root.path().join("held/original");
        assert_eq!(original(&retained), handle);
        assert_eq!(Attributes::read(&retained), attributes);
        if let Some(child) = &child {
            assert_eq!(Attributes::read(&retained.join("nested/file")), *child);
        }
        vfs.move_atomic("held/original", "source", kind)
            .await
            .unwrap();
        assert_eq!(original(&source), handle);
        assert_eq!(Attributes::read(&source), attributes);
        if let Some(child) = &child {
            assert_eq!(Attributes::read(&root.path().join(leaf)), *child);
        }
        assert_eq!(
            original(&root.path().join(leaf)),
            original(&root.path().join("linked"))
        );
        assert_eq!(vfs.read_bytes(leaf).await.unwrap(), [0, 255, 1]);
    }
}

fn inherited_attributes(path: &Path) -> (u32, u32, u32, Vec<u8>) {
    let metadata = std::fs::metadata(path).unwrap();
    #[cfg(target_os = "macos")]
    let entries = acl_lines(path);
    #[cfg(target_os = "linux")]
    let entries = acl(path);
    (metadata.mode(), metadata.uid(), metadata.gid(), entries)
}

#[tokio::test]
async fn journal_creations_inherit_the_destination_parent_including_limited_acls() {
    use crate::runs::native_vfs::resources::ResourceFiles;
    use openwebide_core::workspace_transaction::{ResourceScope, TransactionFiles};
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o2771)).unwrap();
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,write,execute,file_inherit,directory_inherit,limit_inherit",
            ])
            .arg(&parent)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: geteuid reads process identity and has no pointer arguments.
        if unsafe { libc::geteuid() } == 0 {
            std::os::unix::fs::chown(&parent, None, Some(12345)).unwrap();
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o2771)).unwrap();
        }
        set_attribute(&parent, c"system.posix_acl_default", &linux_acl());
    }
    std::fs::File::create_new(parent.join("control-file")).unwrap();
    std::fs::create_dir(parent.join("control-directory")).unwrap();
    std::fs::File::create_new(parent.join("control-directory/child")).unwrap();
    let scope = ResourceScope {
        transaction: "0123456789abcdef0123456789abcdef".into(),
        fingerprint: "a".repeat(64),
    };
    let files = ResourceFiles::new(
        &std::fs::File::open(root.path()).unwrap(),
        "creation-inheritance",
        scope.clone(),
    )
    .unwrap();
    for (ordinal, path, kind) in [
        (0, "parent/new-file", VfsEntryKind::File),
        (1, "parent/new-directory", VfsEntryKind::Directory),
        (2, "parent/new-directory/child", VfsEntryKind::File),
    ] {
        files
            .create_resource(&scope, ordinal, path, kind)
            .await
            .unwrap();
    }
    for (actual, control) in [
        ("new-file", "control-file"),
        ("new-directory", "control-directory"),
        ("new-directory/child", "control-directory/child"),
    ] {
        assert_eq!(
            inherited_attributes(&parent.join(actual)),
            inherited_attributes(&parent.join(control)),
            "{actual}"
        );
    }
    assert!(!inherited_attributes(&parent.join("new-file")).3.is_empty());
    files.finish_retained(&scope).await.unwrap();
}
