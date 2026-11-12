//! Exercise the real host behind shared coordination, not a fake cleanup future.
use super::*;
use std::{fs::TryLockError, process::Stdio};

fn execution(root: &Path) -> Arc<dyn ToolExecution> {
    crate::ServerConfig::new(root.into(), "secret".into(), None).guarded_execution()
}
async fn exists(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn unlocked(file: &std::fs::File) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match file.try_lock() {
                Ok(()) => return,
                Err(TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    file.unlock().unwrap();
}

#[tokio::test]
async fn commands_preserve_the_original_output_environment_and_exit_status() {
    let root = tempfile::tempdir().unwrap();
    let host = execution(root.path());
    let mut spec = SpawnSpec::shell(
        "printf '%s' \"$OPENWEBIDE_COMMAND_VALUE\"; printf error >&2; exit 42".into(),
        root.path().into(),
        5,
        std::future::pending(),
    );
    spec.env
        .insert("OPENWEBIDE_COMMAND_VALUE".into(), "original command".into());
    let result = host.run_command(spec).await.unwrap();
    assert_eq!(result.exit_code, Some(42));
    assert_eq!(result.stdout, "original command");
    assert_eq!(result.stderr, "error");
    let result = host
        .run_command(SpawnSpec::shell(
            "kill -TERM $$".into(),
            root.path().into(),
            5,
            std::future::pending(),
        ))
        .await
        .unwrap();
    assert_eq!(result.exit_code, None);
}

#[tokio::test]
async fn successful_background_commands_keep_the_original_folder_excluded_until_they_end() {
    let root = tempfile::tempdir().unwrap();
    let host = execution(root.path());
    let result=host.run_command(SpawnSpec::shell(
        "(n=0; while [ ! -e continue ] && [ \"$n\" -lt 500 ]; do sleep .02; n=$((n+1)); done; [ -e continue ] && printf finished > outcome) >/dev/null 2>&1 & printf acknowledged".into(),
        root.path().into(),5,std::future::pending())).await.unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, "acknowledged");
    let file = std::fs::File::open(root.path()).unwrap();
    assert!(matches!(file.try_lock(), Err(TryLockError::WouldBlock)));
    std::fs::write(root.path().join("continue"), "").unwrap();
    exists(&root.path().join("outcome")).await;
    unlocked(&file).await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("outcome")).unwrap(),
        "finished"
    );
}

const SCRIPT: &str = r#"
import os,pathlib,signal,subprocess,sys,time
root=pathlib.Path(sys.argv[1])
signal.signal(signal.SIGTERM,signal.SIG_IGN)
if len(sys.argv)>2:
 os.closerange(3,256)
 (root/'grandchild-ready').write_text(str(os.getpid()))
 deadline=time.monotonic()+15
 while time.monotonic()<deadline and not (root/'allow-write').exists():time.sleep(.01)
 if (root/'allow-write').exists():(root/'late-write').write_text('surviving writer')
else:
 (root/'watcher').write_text(str(os.getppid()))
 subprocess.Popen([sys.executable,__file__,str(root),'child'],close_fds=True)
 deadline=time.monotonic()+15
 while time.monotonic()<deadline:time.sleep(.1)
"#;

#[tokio::test]
async fn command_observing_bridge_peer() {
    let Some(root) = std::env::var_os("OPENWEBIDE_TEST_COMMAND_BRIDGE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let host = execution(&root);
    let spec = SpawnSpec::in_root(
        "python3".into(),
        vec![
            root.join("command.py").to_string_lossy().into_owned(),
            root.to_string_lossy().into_owned(),
        ],
        None,
        Default::default(),
        &root,
    )
    .unwrap();
    host.run_command(spec).await.unwrap();
}

struct Peer(std::process::Child);
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn abrupt_bridge_loss_retains_the_command_folder_until_original_descendants_are_stopped() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("command.py"), SCRIPT).unwrap();
    let mut peer = Peer(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "exec::workspace::tests::commands::command_observing_bridge_peer",
                "--nocapture",
            ])
            .env("OPENWEBIDE_TEST_COMMAND_BRIDGE_ROOT", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    exists(&root.path().join("grandchild-ready")).await;
    let watcher: i32 = std::fs::read_to_string(root.path().join("watcher"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(watcher > 1);
    let file = std::fs::File::open(root.path()).unwrap();
    assert!(matches!(file.try_lock(), Err(TryLockError::WouldBlock)));
    peer.0.kill().unwrap();
    peer.0.wait().unwrap();
    assert!(
        matches!(file.try_lock(), Err(TryLockError::WouldBlock)),
        "The independent watcher must already own the descriptors when the bridge disappears"
    );
    // No bridge task remains to perform cleanup or keep this folder excluded.
    unlocked(&file).await;
    std::fs::write(root.path().join("allow-write"), "").unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!root.path().join("late-write").exists());
}

#[tokio::test]
async fn a_confirmed_failure_before_dispatch_preserves_ordinary_folder_access() {
    let root = tempfile::tempdir().unwrap();
    let config = crate::ServerConfig::new(root.path().into(), "secret".into(), None);
    let spec = SpawnSpec::in_root(
        root.path()
            .join("missing-command")
            .to_string_lossy()
            .into_owned(),
        vec![],
        None,
        Default::default(),
        root.path(),
    )
    .unwrap();
    assert!(config.guarded_execution().run_command(spec).await.is_err());
    let _lease = tokio::time::timeout(
        Duration::from_secs(3),
        config
            .workspace_transactions
            .coordinator
            .acquire(Folder::capture(root.path()).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn a_failed_owner_reports_uncertain_cleanup_without_waiting_for_its_child_output() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("owned.py");
    std::fs::write(
        &script,
        r#"
import json,os,pathlib,sys,time
root=pathlib.Path(sys.argv[1])
(root/'ready').write_text(json.dumps({'owner':os.getppid(),'child':os.getpid()}))
deadline=time.monotonic()+10
while not (root/'stop').exists() and time.monotonic()<deadline:time.sleep(.01)
(root/'finished').write_text('finished')
"#,
    )
    .unwrap();
    let config = crate::ServerConfig::new(root.path().into(), "secret".into(), None);
    let spec = SpawnSpec::in_root(
        "python3".into(),
        vec![
            script.to_string_lossy().into_owned(),
            root.path().to_string_lossy().into_owned(),
        ],
        None,
        Default::default(),
        root.path(),
    )
    .unwrap();
    let mut operation = tokio::spawn(config.guarded_execution().run_command(spec));
    exists(&root.path().join("ready")).await;
    let ready: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("ready")).unwrap()).unwrap();
    let owner = i32::try_from(ready["owner"].as_u64().unwrap()).unwrap();
    // Refuse to signal our own test process if coordination accidentally stops
    // launching an independent owner. The fixture child has a bounded lifetime.
    if owner <= 1 || u32::try_from(owner).unwrap() == std::process::id() {
        std::fs::write(root.path().join("stop"), "").unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(3), &mut operation).await;
        panic!("The command has no independent process owner");
    }
    // The live actual child reports its unreaped direct parent. This is the
    // one test-owned watcher; do not signal this PID again after it is reaped.
    assert_eq!(unsafe { libc::kill(owner, libc::SIGKILL) }, 0);
    let observed = tokio::time::timeout(Duration::from_secs(2), &mut operation).await;
    let was_prompt = observed.is_ok();
    let result = match observed {
        Ok(result) => Some(result.unwrap()),
        Err(_) => None,
    };
    // Finish the still-live fixture writer before asserting, including on the
    // old pipe-draining path. No cleanup signal targets an unowned/reused PID.
    std::fs::write(root.path().join("stop"), "").unwrap();
    exists(&root.path().join("finished")).await;
    if !was_prompt {
        let _ = tokio::time::timeout(Duration::from_secs(3), &mut operation).await;
    }
    assert!(
        was_prompt,
        "A failed owner must not wait for its child's pipe EOF"
    );
    let error = result.unwrap().unwrap_err().to_string();
    assert!(error.contains("cleanup is unconfirmed"), "{error}");
    assert!(
        config
            .workspace_transactions
            .coordinator
            .acquire(Folder::capture(root.path()).unwrap())
            .await
            .is_err()
    );
    let physical = std::fs::File::open(root.path()).unwrap();
    assert!(matches!(physical.try_lock(), Err(TryLockError::WouldBlock)));
}

#[tokio::test]
async fn a_control_frame_rejected_before_start_does_not_quarantine_the_original_folder() {
    let root = tempfile::tempdir().unwrap();
    let config = crate::ServerConfig::new(root.path().into(), "secret".into(), None);
    let spec = SpawnSpec::shell(
        "x".repeat(5 * 1024 * 1024),
        root.path().into(),
        5,
        std::future::pending(),
    );
    let error = config
        .guarded_execution()
        .run_command(spec)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("control frame exceeds its limit"), "{error}");
    let _lease = tokio::time::timeout(
        Duration::from_secs(3),
        config
            .workspace_transactions
            .coordinator
            .acquire(Folder::capture(root.path()).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn command_directory_aliases_preserve_the_pinned_physical_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let physical = target.path().join("deeper/actual");
    std::fs::create_dir_all(&physical).unwrap();
    let alias = root.path().join("shortcut");
    std::os::unix::fs::symlink(&physical, &alias).unwrap();
    let host = execution(root.path());
    let outcome = host
        .run_command(SpawnSpec::shell(
            "printf pinned > marker; pwd -P".into(),
            alias,
            5,
            std::future::pending(),
        ))
        .await
        .unwrap();
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(
        outcome.stdout.trim(),
        physical.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(physical.join("marker")).unwrap(),
        "pinned"
    );
}
