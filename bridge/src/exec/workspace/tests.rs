use super::*;
use futures::FutureExt;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::Semaphore;

struct Steps {
    entered: Semaphore,
    cancelled: Semaphore,
    cleanup: Semaphore,
}
impl Steps {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Semaphore::new(0),
            cancelled: Semaphore::new(0),
            cleanup: Semaphore::new(0),
        })
    }
}
async fn event(event: &Semaphore) {
    tokio::time::timeout(Duration::from_secs(3), event.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}
struct Controlled(Arc<Steps>, std::path::PathBuf);
impl ToolExecution for Controlled {
    fn git_root(&self, _cwd: std::path::PathBuf) -> ExecutionFuture<std::path::PathBuf> {
        let root = self.1.clone();
        Box::pin(async move { Ok(root) })
    }

    fn run_command(&self, spec: SpawnSpec) -> ExecutionFuture<super::super::ExecOutput> {
        let steps = self.0.clone();
        Box::pin(async move {
            steps.entered.add_permits(1);
            spec.cancel.await;
            steps.cancelled.add_permits(1);
            event(&steps.cleanup).await;
            Err(BridgeError::Execution("cancelled after cleanup".into()))
        })
    }
    fn git(&self, request: GitRequest) -> ExecutionFuture<GitResponse> {
        let steps = self.0.clone();
        Box::pin(async move {
            if request.operation.mutates() {
                steps.entered.add_permits(1);
                event(&steps.cleanup).await;
            }
            Ok(GitResponse::Branches(vec![]))
        })
    }
}
fn configured(root: &Path, steps: &Arc<Steps>) -> crate::ServerConfig {
    let mut config = crate::ServerConfig::new(root.into(), "secret".into(), None);
    config.execution = Arc::new(Controlled(steps.clone(), root.into()));
    config
}

#[tokio::test]
async fn cancelled_command_observers_retain_folder_exclusion_through_process_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let steps = Steps::new();
    let config = configured(directory.path(), &steps);
    let execution = config.guarded_execution();
    let command = execution.run_command(SpawnSpec::shell(
        "ignored".into(),
        directory.path().into(),
        30,
        std::future::pending(),
    ));
    let observer = tokio::spawn(command);
    event(&steps.entered).await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    event(&steps.cancelled).await;
    let mut recovery = Box::pin(
        config
            .workspace_transactions
            .coordinator
            .acquire(Folder::capture(directory.path()).unwrap()),
    );
    assert!(recovery.as_mut().now_or_never().is_none());
    steps.cleanup.add_permits(1);
    let _recovery = tokio::time::timeout(Duration::from_secs(3), recovery)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn accepted_git_owns_its_worktree_after_disconnect_and_read_only_git_stays_available() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join(".git")).unwrap();
    std::fs::create_dir_all(directory.path().join("project/child")).unwrap();
    std::fs::create_dir(directory.path().join("sibling")).unwrap();
    let steps = Steps::new();
    let config = configured(directory.path(), &steps);
    let execution = config.run_host().execution;
    let observer = tokio::spawn(execution.git(GitRequest {
        cwd: directory.path().join("project/child"),
        operation: GitOperation::Checkout(openwebide_core::GitCheckoutRequest {
            branch: "other".into(),
            create_if_missing: false,
        }),
    }));
    event(&steps.entered).await;
    execution
        .git(GitRequest {
            cwd: directory.path().into(),
            operation: GitOperation::Status,
        })
        .await
        .unwrap();
    execution
        .git(GitRequest {
            cwd: directory.path().into(),
            operation: GitOperation::Stash(openwebide_core::git::GitStashRequest {
                action: openwebide_core::git::GitStashAction::List,
                hash: None,
                message: None,
            }),
        })
        .await
        .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    let mut sibling = Box::pin(
        config
            .workspace_transactions
            .coordinator
            .acquire(Folder::capture(&directory.path().join("sibling")).unwrap()),
    );
    assert!(sibling.as_mut().now_or_never().is_none());
    steps.cleanup.add_permits(1);
    let _sibling = tokio::time::timeout(Duration::from_secs(3), sibling)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn already_cancelled_commands_do_not_reach_the_process_adapter() {
    let directory = tempfile::tempdir().unwrap();
    let steps = Steps::new();
    let config = configured(directory.path(), &steps);
    let error = config
        .guarded_execution()
        .run_command(SpawnSpec::shell(
            "ignored".into(),
            directory.path().into(),
            30,
            async {},
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cancelled before folder access"));
    assert_eq!(steps.entered.available_permits(), 0);
}

#[tokio::test]
async fn observers_cancelled_while_queued_cannot_start_a_later_command() {
    let directory = tempfile::tempdir().unwrap();
    let steps = Steps::new();
    let config = configured(directory.path(), &steps);
    let coordinator = config.workspace_transactions.coordinator.clone();
    let lease = coordinator
        .acquire(Folder::capture(directory.path()).unwrap())
        .await
        .unwrap();
    let observer = tokio::spawn(config.guarded_execution().run_command(SpawnSpec::shell(
        "ignored".into(),
        directory.path().into(),
        30,
        std::future::pending(),
    )));
    tokio::time::timeout(Duration::from_secs(3), async {
        while coordinator.waiting() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), async {
        while coordinator.waiting() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(lease);
    assert_eq!(steps.entered.available_permits(), 0);
}

#[tokio::test]
async fn remote_host_transport_does_not_claim_its_local_launch_directory_as_the_target() {
    let directory = tempfile::tempdir().unwrap();
    let steps = Steps::new();
    let config = configured(directory.path(), &steps);
    let _lease = config
        .workspace_transactions
        .coordinator
        .acquire(Folder::capture(directory.path()).unwrap())
        .await
        .unwrap();
    steps.cleanup.add_permits(1);
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        config
            .guarded_execution()
            .run_remote_command(SpawnSpec::shell(
                "ignored".into(),
                directory.path().into(),
                30,
                async {},
            )),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    assert_eq!(steps.entered.available_permits(), 1);
}

async fn git(directory: &Path, args: &[&std::ffi::OsStr]) {
    let output = tokio::process::Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn native_git_discovers_worktrees_and_bare_roots_outside_the_opened_subdirectory() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    std::fs::create_dir(&repository).unwrap();
    git(
        &repository,
        &["init".as_ref(), "-b".as_ref(), "main".as_ref()],
    )
    .await;
    git(
        &repository,
        &[
            "-c".as_ref(),
            "user.name=Test".as_ref(),
            "-c".as_ref(),
            "user.email=test@example.invalid".as_ref(),
            "commit".as_ref(),
            "--allow-empty".as_ref(),
            "-m".as_ref(),
            "initial".as_ref(),
        ],
    )
    .await;
    let child = repository.join("project/child");
    std::fs::create_dir_all(&child).unwrap();
    assert_eq!(
        super::super::HostExecution
            .git_root(child)
            .await
            .unwrap()
            .canonicalize()
            .unwrap(),
        repository.canonicalize().unwrap()
    );
    let linked = directory.path().join("linked");
    git(
        &repository,
        &[
            "worktree".as_ref(),
            "add".as_ref(),
            "--detach".as_ref(),
            linked.as_os_str(),
        ],
    )
    .await;
    std::fs::create_dir(linked.join("child")).unwrap();
    assert_eq!(
        super::super::HostExecution
            .git_root(linked.join("child"))
            .await
            .unwrap()
            .canonicalize()
            .unwrap(),
        linked.canonicalize().unwrap()
    );
    let bare = directory.path().join("bare");
    git(
        directory.path(),
        &["init".as_ref(), "--bare".as_ref(), bare.as_os_str()],
    )
    .await;
    assert_eq!(
        super::super::HostExecution
            .git_root(bare.clone())
            .await
            .unwrap()
            .canonicalize()
            .unwrap(),
        bare.canonicalize().unwrap()
    );
}

#[tokio::test]
async fn native_git_scope_honors_inherited_git_environment() {
    const KEY: &str = "OPENWEBIDE_COORDINATOR_GIT_ROOT_TEST";
    if let Some(expected) = std::env::var_os(KEY) {
        let root = super::super::HostExecution
            .git_root(std::env::current_dir().unwrap())
            .await
            .unwrap();
        assert_eq!(
            root.canonicalize().unwrap(),
            PathBuf::from(expected).canonicalize().unwrap()
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    let unrelated = directory.path().join("unrelated");
    std::fs::create_dir(&repository).unwrap();
    std::fs::create_dir(&unrelated).unwrap();
    git(&repository, &["init".as_ref()]).await;
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "exec::workspace::tests::native_git_scope_honors_inherited_git_environment",
            "--nocapture",
        ])
        .current_dir(&unrelated)
        .env(KEY, &repository)
        .env("GIT_DIR", repository.join(".git"))
        .env("GIT_WORK_TREE", &repository)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod commands;
