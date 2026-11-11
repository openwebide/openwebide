use super::*;

const CHILD_SCRIPT: &str = r#"
import os,pathlib,signal,subprocess,sys,time
root=pathlib.Path(sys.argv[1])
deadline=time.monotonic()+10
if len(sys.argv)>2:
 signal.signal(signal.SIGTERM,signal.SIG_IGN)
 os.closerange(3,256)
 (root/'grandchild-ready').write_text(str(os.getpid()))
 while time.monotonic()<deadline and not (root/'allow-write').exists():time.sleep(.01)
 if (root/'allow-write').exists():(root/'late-write').write_text('a surviving original child wrote after recovery')
 while time.monotonic()<deadline:time.sleep(.1)
else:
 subprocess.Popen([sys.executable,__file__,str(root),'child'],close_fds=True)
 while time.monotonic()<deadline:time.sleep(.1)
"#;

async fn wait(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn observing_bridge_peer() {
    let Some(root) = std::env::var_os("OPENWEBIDE_TEST_OBSERVING_BRIDGE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let file = std::fs::File::open(&root).unwrap();
    file.try_lock().unwrap();
    let (owner, child, pid) = Owner::spawn(
        Path::new("python3"),
        &[
            root.join("server.py").to_string_lossy().into_owned(),
            root.to_string_lossy().into_owned(),
        ],
        &root,
        &BTreeMap::new(),
    )
    .unwrap();
    let _claim = owner
        .retain(vec![duplicate(file.as_raw_fd()).unwrap()])
        .await
        .unwrap();
    drop(file);
    wait(&root.join("grandchild-ready")).await;
    let watcher = i32::try_from(child.id().unwrap()).unwrap();
    // SAFETY: our live direct child is not reaped and cannot reuse its PID.
    // Its stopped process group will receive SIGHUP/SIGCONT when this peer dies.
    assert_eq!(unsafe { libc::kill(watcher, libc::SIGSTOP) }, 0);
    std::fs::write(
        root.join("bridge-ready"),
        serde_json::to_vec(&serde_json::json!({"watcher":child.id().unwrap(),"server":pid}))
            .unwrap(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_secs(15)).await;
    // SAFETY: bounded fixture cleanup if the controlling test never kills us.
    unsafe {
        libc::kill(watcher, libc::SIGCONT);
    }
    owner.stop();
}

struct BridgePeer(std::process::Child);
impl Drop for BridgePeer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn abrupt_bridge_loss_keeps_folder_locked_until_the_original_group_is_stopped() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("server.py"), CHILD_SCRIPT).unwrap();
    let mut peer = BridgePeer(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "exec::process_owner::tests::observing_bridge_peer",
                "--nocapture",
            ])
            .env("OPENWEBIDE_TEST_OBSERVING_BRIDGE_ROOT", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait(&root.path().join("bridge-ready")).await;
    let file = std::fs::File::open(root.path()).unwrap();
    assert!(matches!(
        file.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    peer.0.kill().unwrap();
    peer.0.wait().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match file.try_lock() {
                Ok(()) => return,
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    let grandchild = std::fs::read_to_string(root.path().join("grandchild-ready")).unwrap();
    let pid = sysinfo::Pid::from_u32(grandchild.trim().parse().unwrap());
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().without_tasks(),
    );
    assert!(
        system
            .process(pid)
            .is_none_or(|process| process.status() == sysinfo::ProcessStatus::Zombie),
        "original child is still live"
    );
    std::fs::write(root.path().join("allow-write"), "allow").unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!root.path().join("late-write").exists());
}

#[tokio::test]
async fn server_termination_is_confirmed_by_its_independent_owner() {
    let root = tempfile::tempdir().unwrap();
    let (owner, mut child, pid) = Owner::spawn(
        Path::new("/bin/sh"),
        &["-c".into(), "sleep 30".into()],
        root.path(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_ne!(child.id(), Some(pid));
    owner.stop();
    let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status}");
}

#[tokio::test]
async fn a_completed_claim_releases_folder_descriptors_and_preserves_the_warm_server() {
    let root = tempfile::tempdir().unwrap();
    let (owner, mut child, _) = Owner::spawn(
        Path::new("/bin/sh"),
        &["-c".into(), "sleep 30".into()],
        root.path(),
        &BTreeMap::new(),
    )
    .unwrap();
    let file = std::fs::File::open(root.path()).unwrap();
    file.try_lock().unwrap();
    let claim = owner
        .retain(vec![duplicate(file.as_raw_fd()).unwrap()])
        .await
        .unwrap();
    drop(file);
    let independent = std::fs::File::open(root.path()).unwrap();
    assert!(matches!(
        independent.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    claim.finish().await.unwrap();
    independent.try_lock().unwrap();
    assert!(child.try_wait().unwrap().is_none());
    owner.stop();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn a_cancelled_folder_handoff_closes_control_before_a_claim_is_acknowledged() {
    use std::io::Read;
    let root = tempfile::tempdir().unwrap();
    let file = std::fs::File::open(root.path()).unwrap();
    file.try_lock().unwrap();
    let descriptors = vec![duplicate(file.as_raw_fd()).unwrap()];
    let (stream, mut peer) = UnixStream::pair().unwrap();
    configure(&stream).unwrap();
    configure(&peer).unwrap();
    let owner = Owner {
        stop: Arc::new(stream.try_clone().unwrap()),
        stream: Arc::new(Mutex::new(stream)),
    };
    let (received, ready) = tokio::sync::oneshot::channel();
    let observer = std::thread::spawn(move || {
        let (request, descriptors) = wire::receive::<Request>(&mut peer).unwrap();
        assert!(matches!(request, Request::Retain { .. }));
        assert_eq!(descriptors.len(), 1);
        received.send(()).unwrap();
        // Simulate a watcher that holds the loan but has not acknowledged it.
        // Cancellation must close control even while exchange awaits its reply.
        assert_eq!(peer.read(&mut [0u8; 1]).unwrap(), 0);
        drop(descriptors);
    });
    let retaining = owner.clone();
    let task = tokio::spawn(async move { retaining.retain(descriptors).await });
    tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .unwrap()
        .unwrap();
    drop(file);
    let independent = std::fs::File::open(root.path()).unwrap();
    assert!(matches!(
        independent.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    task.abort();
    match task.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("The unacknowledged handoff completed before cancellation"),
    }
    tokio::task::spawn_blocking(move || observer.join().unwrap())
        .await
        .unwrap();
    independent.try_lock().unwrap();
}
