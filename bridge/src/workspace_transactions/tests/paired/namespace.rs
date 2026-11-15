use super::*;
use crate::native_recovery::{ANCHOR, DATABASE, DIRECTORY, Namespace};
use std::os::unix::fs::{PermissionsExt, symlink};

fn damaged_home(case: usize) -> PathBuf {
    let home = tempfile::Builder::new()
        .prefix("openwebide-recovery-admission-")
        .tempdir()
        .unwrap()
        .keep();
    drop(Namespace::open(&home).unwrap());
    let database = home.join(DIRECTORY).join(DATABASE);
    match case {
        0 => std::fs::rename(database, home.join("original.sqlite3")).unwrap(),
        1 => {
            std::fs::rename(&database, home.join("original.sqlite3")).unwrap();
            std::fs::copy(home.join("original.sqlite3"), database).unwrap();
        }
        2 => {
            std::fs::rename(home.join(DIRECTORY), home.join("original-directory")).unwrap();
            std::fs::create_dir(home.join(DIRECTORY)).unwrap();
            std::fs::set_permissions(home.join(DIRECTORY), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            std::fs::copy(home.join("original-directory").join(DATABASE), database).unwrap();
        }
        3 => std::fs::rename(home.join(ANCHOR), home.join("original-anchor")).unwrap(),
        4 => {
            std::fs::rename(home.join(ANCHOR), home.join("original-anchor")).unwrap();
            std::fs::copy(home.join("original-anchor"), home.join(ANCHOR)).unwrap();
        }
        5 => {
            std::fs::copy(home.join(ANCHOR), home.join("original-anchor-copy")).unwrap();
            std::fs::write(home.join(ANCHOR), b"{\"format\":999}").unwrap();
        }
        6 => {
            std::fs::rename(home.join(DIRECTORY), home.join("original-directory")).unwrap();
            symlink(home.join("original-directory"), home.join(DIRECTORY)).unwrap();
        }
        _ => unreachable!(),
    }
    assert!(Namespace::open(&home).is_err());
    home
}

#[tokio::test]
async fn namespace_loss_and_identical_copies_cannot_admit_file_workers_in_either_mode() {
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        for case in 0..7 {
            let fixture = fixture_mode(2, mode).await;
            let root = fixture.directory.path().join("project");
            let before = std::fs::read(root.join("before.rs")).unwrap();
            let after = std::fs::read(root.join("after.rs")).unwrap();
            let original_before = same_file::Handle::from_path(root.join("before.rs")).unwrap();
            let original_after = same_file::Handle::from_path(root.join("after.rs")).unwrap();
            let home = damaged_home(case);
            let mut host = Host::new();
            host.recovery_home = Arc::new(home.clone());
            let result = if mode == WorkspaceMode::Local {
                let client = files(&host, &fixture, Arc::new(Mutex::new(Vec::new()))).await;
                let delegated = delegation(&fixture, &client).await;
                let authority = serve(fixture.backend.clone()).await;
                client
                    .execute(
                        authority.url.clone(),
                        delegated,
                        fixture.command.request.clone(),
                    )
                    .await
            } else {
                host.execute(
                    fixture.directory.path().into(),
                    fixture.command.clone(),
                    fixture.backend.clone(),
                )
                .await
            };
            println!(
                "namespace admission mode={mode:?} case={case} retained_fixture={} admitted={}",
                home.display(),
                fixture.backend.last_worker.lock().unwrap().is_some()
            );
            assert!(
                result.is_err(),
                "Namespace replacement admitted native file work: mode={mode:?}, case={case}"
            );
            assert!(result.unwrap_err().contains("Native recovery namespace"));
            assert!(fixture.backend.last_worker.lock().unwrap().is_none());
            assert_eq!(fixture.backend.snapshot_reads.load(Ordering::SeqCst), 0);
            assert_eq!(std::fs::read(root.join("before.rs")).unwrap(), before);
            assert_eq!(std::fs::read(root.join("after.rs")).unwrap(), after);
            assert_eq!(
                same_file::Handle::from_path(root.join("before.rs")).unwrap(),
                original_before
            );
            assert_eq!(
                same_file::Handle::from_path(root.join("after.rs")).unwrap(),
                original_after
            );
            assert!(Namespace::open(&home).is_err());
        }
    }
}
