use super::*;
use std::process::Command;

fn owned_home() -> PathBuf {
    tempfile::Builder::new()
        .prefix("openwebide-recovery-namespace-case-")
        .tempdir()
        .unwrap()
        .keep()
}

#[test]
fn repeated_open_keeps_the_same_original_namespace_and_private_database() {
    let home = owned_home();
    let first = Namespace::open(&home).unwrap();
    let anchor = same_file::Handle::from_path(home.join(ANCHOR)).unwrap();
    let db = same_file::Handle::from_path(home.join(DIRECTORY).join(DATABASE)).unwrap();
    first.ensure_current().unwrap();
    drop(first);
    let second = Namespace::open(&home).unwrap();
    second.ensure_current().unwrap();
    assert_eq!(
        anchor,
        same_file::Handle::from_path(home.join(ANCHOR)).unwrap()
    );
    assert_eq!(
        db,
        same_file::Handle::from_path(home.join(DIRECTORY).join(DATABASE)).unwrap()
    );
    let connection = Connection::open_with_flags(
        home.join(DIRECTORY).join(DATABASE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM recovery_namespace", [], |row| row
                .get::<_, u32>(0))
            .unwrap(),
        1
    );
    println!("retained_namespace_fixture={}", home.display());
}

#[test]
fn a_partial_initialization_never_turns_into_a_clean_first_run() {
    let home = owned_home();
    let mut options = std::fs::OpenOptions::new();
    use std::os::unix::fs::OpenOptionsExt;
    options.write(true).create_new(true).mode(0o600);
    let anchor = options.open(home.join(ANCHOR)).unwrap();
    anchor.sync_all().unwrap();
    assert!(Namespace::open(&home).is_err());
    assert_eq!(std::fs::read(home.join(ANCHOR)).unwrap(), b"");
    assert!(!home.join(DIRECTORY).exists());
}

#[test]
fn a_busy_initializer_fails_without_an_unbounded_wait_or_workspace_effect() {
    let home = owned_home();
    let namespace = Namespace::open(&home).unwrap();
    let lock = File::open(home.join(ANCHOR)).unwrap();
    lock.try_lock().unwrap();
    assert_eq!(Namespace::open(&home).err().unwrap(), BUSY);
    assert_eq!(namespace.ensure_current().unwrap_err(), BUSY);
    lock.unlock().unwrap();
    namespace.ensure_current().unwrap();
}

#[test]
fn account_home_ignores_profile_environment() {
    const CHILD: &str = "OPENWEBIDE_TEST_PRINCIPAL_HOME_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let home = home().unwrap();
        println!(
            "principal_home_digest={}",
            openwebide_core::plugins::content_digest(home.as_os_str().as_bytes())
        );
        return;
    }
    let mut fingerprints = Vec::new();
    for _ in 0..2 {
        let fake = owned_home();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_recovery::tests::account_home_ignores_profile_environment",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("HOME", &fake)
            .env("USERPROFILE", &fake)
            .env("XDG_STATE_HOME", &fake)
            .env("XDG_CONFIG_HOME", &fake)
            .env("XDG_CACHE_HOME", &fake)
            .current_dir(&fake)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let fingerprint = text
            .lines()
            .find_map(|line| line.strip_prefix("principal_home_digest="))
            .unwrap()
            .to_owned();
        assert_ne!(
            fingerprint,
            openwebide_core::plugins::content_digest(fake.as_os_str().as_bytes())
        );
        fingerprints.push(fingerprint);
    }
    assert_eq!(fingerprints[0], fingerprints[1]);
}

#[test]
fn a_fresh_process_reopens_only_the_original_registry_objects() {
    const CHILD: &str = "OPENWEBIDE_TEST_NAMESPACE_HOME";
    if let Some(home) = std::env::var_os(CHILD) {
        Namespace::open(Path::new(&home))
            .expect("The original namespace must survive a fresh process")
            .ensure_current()
            .unwrap();
        return;
    }
    let home = owned_home();
    drop(Namespace::open(&home).unwrap());
    let run = || {
        Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "native_recovery::tests::a_fresh_process_reopens_only_the_original_registry_objects", "--nocapture"])
        .env(CHILD, &home).output().unwrap()
    };
    let original = run();
    std::fs::write(home.join("child-original.stdout"), &original.stdout).unwrap();
    std::fs::write(home.join("child-original.stderr"), &original.stderr).unwrap();
    assert!(original.status.success());
    let db = home.join(DIRECTORY).join(DATABASE);
    std::fs::rename(&db, home.join("retained-original.sqlite3")).unwrap();
    std::fs::copy(home.join("retained-original.sqlite3"), &db).unwrap();
    let replaced = run();
    std::fs::write(home.join("child-replaced.stdout"), &replaced.stdout).unwrap();
    std::fs::write(home.join("child-replaced.stderr"), &replaced.stderr).unwrap();
    assert_eq!(replaced.status.code(), Some(101));
    assert!(
        String::from_utf8(replaced.stderr)
            .unwrap()
            .contains(INVALID)
    );
    println!("retained_process_fixture={}", home.display());
}

#[tokio::test]
async fn ordinary_vfs_and_execution_roots_cannot_reach_the_namespace() {
    use crate::runs::native_vfs::{NativeFsVfs, workspace::WorkspaceFiles};
    use openwebide_core::Vfs;
    let home = owned_home();
    let namespace = Namespace::open(&home).unwrap();
    let before = std::fs::read(home.join(DIRECTORY).join(DATABASE)).unwrap();
    std::os::unix::fs::symlink(home.join(DIRECTORY), home.join("alias")).unwrap();
    for captured in [false, true] {
        for root in [&home, &home.join(DIRECTORY), &home.join("alias")] {
            let files: Box<dyn Vfs> = if captured {
                Box::new(WorkspaceFiles::new(
                    root.clone(),
                    crate::folder_coordination::Coordinator::default(),
                ))
            } else {
                Box::new(NativeFsVfs { root: root.clone() })
            };
            let paths = if *root == home {
                vec![
                    format!("{DIRECTORY}/{DATABASE}"),
                    ANCHOR.into(),
                    format!("alias/{DATABASE}"),
                ]
            } else {
                vec![DATABASE.into()]
            };
            for path in paths {
                assert!(files.read_bytes(&path).await.is_err());
                assert!(files.write(&path, "unwanted").await.is_err());
                assert!(files.delete(&path).await.is_err());
                assert!(
                    files
                        .move_atomic(&path, "moved", openwebide_core::vfs::VfsEntryKind::File)
                        .await
                        .is_err()
                );
            }
        }
    }
    assert!(Folder::capture(&home.join(DIRECTORY)).is_err());
    assert!(Folder::capture(&home.join("alias")).is_err());
    assert_eq!(
        std::fs::read(home.join(DIRECTORY).join(DATABASE)).unwrap(),
        before
    );
    namespace.ensure_current().unwrap();
}

#[test]
fn private_metadata_fifos_are_rejected_without_waiting_for_a_peer() {
    const CHILD: &str = "OPENWEBIDE_TEST_METADATA_FIFO_HOME";
    if let Some(home) = std::env::var_os(CHILD) {
        assert!(Namespace::open(Path::new(&home)).is_err());
        return;
    }
    for anchor in [false, true] {
        let home = tempfile::Builder::new()
            .prefix("openwebide-recovery-fifo-case-")
            .tempdir()
            .unwrap()
            .keep();
        drop(Namespace::open(&home).unwrap());
        let path = if anchor {
            home.join(ANCHOR)
        } else {
            home.join(DIRECTORY).join(DATABASE)
        };
        std::fs::rename(&path, home.join("original-private-file")).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live NUL-terminated owned fixture pathname.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_recovery::tests::private_metadata_fifos_are_rejected_without_waiting_for_a_peer", "--nocapture"])
            .env(CHILD, &home).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let start = std::time::Instant::now();
        let mut early = false;
        while start.elapsed() < std::time::Duration::from_secs(2) {
            if child.try_wait().unwrap().is_some() {
                early = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !early {
            // The expected red must release its actual blocked reader before
            // asserting, preserving a clean terminal result and the fixture.
            // SAFETY: The fixture pathname remains live; successful ownership
            // of the descriptor is immediately closed exactly once below.
            let descriptor = unsafe {
                libc::open(
                    name.as_ptr(),
                    libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            assert!(
                descriptor >= 0,
                "Blocked FIFO reader was not waiting for its peer"
            );
            // SAFETY: open returned the live owned descriptor above.
            assert_eq!(unsafe { libc::close(descriptor) }, 0);
            let cleanup = std::time::Instant::now();
            while child.try_wait().unwrap().is_none()
                && cleanup.elapsed() < std::time::Duration::from_secs(2)
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
            }
        }
        let output = child.wait_with_output().unwrap();
        std::fs::write(home.join("child.stdout"), &output.stdout).unwrap();
        std::fs::write(home.join("child.stderr"), &output.stderr).unwrap();
        assert!(output.status.success());
        println!(
            "metadata_fifo_anchor={anchor} rejected_without_peer={early} retained_fixture={}",
            home.display()
        );
        assert!(
            early,
            "Private metadata FIFO blocked before its regular-file check"
        );
    }
}

#[test]
fn installation_state_can_live_below_private_application_data() {
    let home = owned_home().join(".spin/principal");
    std::fs::create_dir_all(&home).unwrap();
    let first = Namespace::open(&home)
        .expect("Installation state must support the persistent application-data volume");
    first.ensure_current().unwrap();
    assert!(Folder::capture(&home).is_err());
    drop(first);
    Namespace::open(&home).unwrap().ensure_current().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn packaged_principal_uses_the_same_original_state_across_containers() {
    if std::env::var_os("OPENWEBIDE_TEST_PACKAGED_PRINCIPAL").is_none() {
        return;
    }
    let directory = home().unwrap();
    assert_eq!(directory, Path::new("/app/.spin/native-principal"));
    Namespace::open(&directory)
        .unwrap()
        .ensure_current()
        .unwrap();
    let anchor = std::fs::read(directory.join(ANCHOR)).unwrap();
    println!(
        "packaged_namespace_digest={}",
        openwebide_core::plugins::content_digest(&anchor)
    );
}
