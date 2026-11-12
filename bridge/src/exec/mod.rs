use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use crate::BridgeError;
use openwebide_core::{CommandOutcome, GitCheckoutRequest, GitCommitRequest, GitSyncRequest};

pub mod git;
mod host;
mod host_info;
pub mod proc;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[doc(hidden)]
pub mod process_owner;
pub(crate) mod workspace;

pub type ExecOutput = CommandOutcome;
pub type ExecutionFuture<T> = Pin<Box<dyn Future<Output = Result<T, BridgeError>> + Send>>;

pub fn environment() -> openwebide_core::ExecutionEnvironment {
    openwebide_core::ExecutionEnvironment {
        os: std::env::consts::OS.into(),
        shell: if cfg!(windows) { "cmd" } else { "sh" }.into(),
    }
}

pub struct SpawnSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    pub cancel: Pin<Box<dyn Future<Output = ()> + Send>>,
    /// The shared coordinator supplies this original physical folder claim.
    /// Transport processes and uncoordinated host probes leave it absent.
    pub(crate) folder: Option<std::sync::Arc<crate::folder_coordination::Lease>>,
}

impl SpawnSpec {
    pub fn shell(
        command: String,
        cwd: PathBuf,
        timeout_seconds: u64,
        cancel: impl Future<Output = ()> + Send + 'static,
    ) -> Self {
        #[cfg(unix)]
        let (shell, flag) = ("sh", "-c");
        #[cfg(windows)]
        let (shell, flag) = ("cmd", "/C");
        Self {
            command: shell.into(),
            args: vec![flag.into(), command],
            cwd,
            env: HashMap::new(),
            timeout: Duration::from_secs(timeout_seconds),
            cancel: Box::pin(cancel),
            folder: None,
        }
    }

    pub fn in_root(
        command: String,
        args: Vec<String>,
        cwd: Option<&str>,
        env: HashMap<String, String>,
        root: &Path,
    ) -> Result<Self, BridgeError> {
        Ok(Self {
            command,
            args,
            cwd: crate::paths::resolve_in_root(root, cwd).map_err(BridgeError::Validation)?,
            env,
            timeout: Duration::from_secs(30),
            cancel: Box::pin(std::future::pending()),
            folder: None,
        })
    }

    pub fn display_cmd(&self) -> String {
        if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }
}

pub struct GitRequest {
    pub cwd: PathBuf,
    pub operation: GitOperation,
}

pub enum GitOperation {
    History(openwebide_core::git::GitHistoryRequest),
    CommitDiff(openwebide_core::git::GitCommitDiffRequest),
    Stash(openwebide_core::git::GitStashRequest),
    IndexDiff,
    Status,
    Diff(Option<String>),
    Show(String),
    Branches,
    Commit(GitCommitRequest),
    Checkout(GitCheckoutRequest),
    Sync(GitSyncRequest),
    PathChanges,
    PathAction(openwebide_core::git::GitPathRequest),
}

/// Typed execution results; serialization preserves each existing HTTP route's JSON shape.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub enum GitResponse {
    History(openwebide_core::git::GitHistoryPage),
    CommitDiff(openwebide_core::git::GitCommitDiff),
    Stash(openwebide_core::git::GitStashResult),
    Status(openwebide_core::GitRepoStatus),
    Diff(openwebide_core::GitDiff),
    Show(openwebide_core::GitFileContent),
    Branches(Vec<openwebide_core::GitBranchInfo>),
    Commit(openwebide_core::GitCommitResult),
    Checkout(openwebide_core::GitCheckoutResult),
    Sync(openwebide_core::GitSyncResult),
    PathChanges(openwebide_core::git::GitPathChanges),
}

pub trait ToolExecution: Send + Sync {
    fn start_terminal(
        &self,
        id: String,
        spec: SpawnSpec,
    ) -> Result<std::sync::Arc<crate::terminals::session::Session>, BridgeError> {
        crate::terminals::pty::spawn_pty(id, spec, 120, 30).map_err(BridgeError::Execution)
    }
    fn host_info(&self) -> ExecutionFuture<openwebide_core::HostInfo> {
        Box::pin(async {
            Err(BridgeError::Execution(
                "Host information is unavailable".into(),
            ))
        })
    }
    fn run_command(&self, spec: SpawnSpec) -> ExecutionFuture<ExecOutput>;
    /// Trusted remote-host adapters launch a transport process locally, but its
    /// working directory does not identify the filesystem changed on that host.
    fn run_remote_command(&self, spec: SpawnSpec) -> ExecutionFuture<ExecOutput> {
        self.run_command(spec)
    }
    /// Physical write scope for Git, which can own a parent worktree even when
    /// its caller opens a child directory. Custom adapters may supply their own.
    fn git_root(&self, cwd: PathBuf) -> ExecutionFuture<PathBuf> {
        Box::pin(async move { Ok(cwd) })
    }
    fn git(&self, request: GitRequest) -> ExecutionFuture<GitResponse>;
}

#[derive(Default)]
pub struct HostExecution;

impl ToolExecution for HostExecution {
    fn git_root(&self, cwd: PathBuf) -> ExecutionFuture<PathBuf> {
        Box::pin(async move { git::execution_root(&cwd).await })
    }
    fn host_info(&self) -> ExecutionFuture<openwebide_core::HostInfo> {
        Box::pin(async {
            tokio::time::timeout(Duration::from_secs(6), host_info::snapshot())
                .await
                .map_err(|_| BridgeError::Execution("Host information timed out".into()))?
        })
    }
    fn run_command(&self, spec: SpawnSpec) -> ExecutionFuture<ExecOutput> {
        Box::pin(host::execute_command_direct(spec))
    }

    fn git(&self, request: GitRequest) -> ExecutionFuture<GitResponse> {
        Box::pin(async move {
            let dir = &request.cwd;
            Ok(match request.operation {
                GitOperation::History(req) => {
                    GitResponse::History(git::get_history(dir, &req).await?)
                }
                GitOperation::CommitDiff(req) => {
                    GitResponse::CommitDiff(git::get_commit_diff(dir, &req).await?)
                }
                GitOperation::PathChanges => {
                    GitResponse::PathChanges(git::get_path_changes(dir).await?)
                }
                GitOperation::PathAction(req) => {
                    GitResponse::PathChanges(git::apply_path_action(dir, &req).await?)
                }
                GitOperation::Stash(request) => {
                    GitResponse::Stash(git::manage_stash(dir, &request).await?)
                }
                GitOperation::IndexDiff => GitResponse::Diff(openwebide_core::GitDiff {
                    diff: git::get_index_diff(dir).await?,
                }),
                GitOperation::Status => GitResponse::Status(git::get_repo_status(dir).await?),
                GitOperation::Diff(path) => GitResponse::Diff(openwebide_core::GitDiff {
                    diff: git::get_repo_diff(dir, path.as_deref()).await?,
                }),
                GitOperation::Show(path) => {
                    GitResponse::Show(git::get_file_at_head(dir, &path).await?)
                }
                GitOperation::Branches => GitResponse::Branches(git::get_repo_branches(dir).await?),
                GitOperation::Commit(req) => {
                    GitResponse::Commit(git::commit_changes(dir, &req).await?)
                }
                GitOperation::Checkout(req) => {
                    GitResponse::Checkout(git::checkout_branch(dir, &req).await?)
                }
                GitOperation::Sync(req) => GitResponse::Sync(git::sync_repo(dir, &req).await?),
            })
        })
    }
}
