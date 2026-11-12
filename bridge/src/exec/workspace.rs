//! Keep accepted Git/command workers inside the physical folder gate, including
//! process-group cleanup after their HTTP/run observer disappears.
use super::{ExecutionFuture, GitOperation, GitRequest, GitResponse, SpawnSpec, ToolExecution};
use crate::{
    BridgeError,
    folder_coordination::{Coordinator, Folder},
};
use std::sync::Arc;

pub(crate) struct Execution {
    pub inner: Arc<dyn ToolExecution>,
    pub coordinator: Coordinator,
}
impl GitOperation {
    fn mutates(&self) -> bool {
        match self {
            Self::Commit(_) | Self::Checkout(_) | Self::Sync(_) | Self::PathAction(_) => true,
            Self::Stash(request) => request.action != openwebide_core::git::GitStashAction::List,
            Self::History(_)
            | Self::CommitDiff(_)
            | Self::IndexDiff
            | Self::Status
            | Self::Diff(_)
            | Self::Show(_)
            | Self::Branches
            | Self::PathChanges => false,
        }
    }
}
fn stopped(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::Execution(format!("Workspace execution stopped: {error}"))
}
impl ToolExecution for Execution {
    fn run_remote_command(&self, spec: SpawnSpec) -> ExecutionFuture<super::ExecOutput> {
        self.inner.run_remote_command(spec)
    }
    fn start_terminal(
        &self,
        id: String,
        spec: SpawnSpec,
    ) -> Result<Arc<crate::terminals::session::Session>, BridgeError> {
        self.inner.start_terminal(id, spec)
    }
    fn git_root(&self, cwd: std::path::PathBuf) -> ExecutionFuture<std::path::PathBuf> {
        self.inner.git_root(cwd)
    }
    fn host_info(&self) -> ExecutionFuture<openwebide_core::HostInfo> {
        self.inner.host_info()
    }
    fn run_command(&self, spec: SpawnSpec) -> ExecutionFuture<super::ExecOutput> {
        let inner = self.inner.clone();
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            let registration = coordinator.worker().map_err(BridgeError::Execution)?;
            let (observer, abandoned) = tokio::sync::oneshot::channel::<()>();
            let worker = tokio::spawn(async move {
                let _registration = registration;
                let folder = Folder::capture(&spec.cwd).map_err(BridgeError::Validation)?;
                let SpawnSpec {
                    command,
                    args,
                    env,
                    timeout,
                    cancel,
                    ..
                } = spec;
                let stopping = coordinator.clone();
                let mut cancellation = Box::pin(async move {
                    tokio::select! { () = cancel => (), _ = abandoned => (), () = stopping.stopped() => () }
                });
                let lease = tokio::select! {
                    biased;
                    () = &mut cancellation => return Err(BridgeError::Execution("command cancelled before folder access".into())),
                    lease = coordinator.acquire(folder.clone()) => lease.map_err(BridgeError::Validation)?,
                };
                let lease = Arc::new(lease);
                let outcome = inner
                    .run_command(SpawnSpec {
                        command,
                        args,
                        cwd: folder.path,
                        env,
                        timeout,
                        cancel: cancellation,
                        folder: Some(lease.clone()),
                    })
                    .await;
                // An adapter may ignore its optional native claim. The shared
                // worker still retains exclusion through the adapter's outcome.
                drop(lease);
                outcome
            });
            // Dropping this future signals cancellation, but never drops the
            // worker's lease before its process cleanup has finished.
            let _observer = observer;
            worker.await.map_err(stopped)?
        })
    }
    fn git(&self, mut request: GitRequest) -> ExecutionFuture<GitResponse> {
        if !request.operation.mutates() {
            return self.inner.git(request);
        }
        let inner = self.inner.clone();
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            let registration = coordinator.worker().map_err(BridgeError::Execution)?;
            let (observer, abandoned) = tokio::sync::oneshot::channel::<()>();
            let worker = tokio::spawn(async move {
                let _registration = registration;
                let mut abandoned = Box::pin(abandoned);
                let cwd = Folder::capture(&request.cwd).map_err(BridgeError::Validation)?;
                let discovered = tokio::select! {
                    biased;
                    _ = &mut abandoned => return Err(BridgeError::Execution("Git operation cancelled before folder access".into())),
                    root = coordinator.prepare(inner.git_root(cwd.path.clone())) => root.map_err(BridgeError::Execution)??,
                };
                let root = Folder::capture(&discovered).map_err(BridgeError::Validation)?;
                let _lease = tokio::select! {
                    biased;
                    _ = &mut abandoned => return Err(BridgeError::Execution("Git operation cancelled before folder access".into())),
                    lease = coordinator.acquire(root.clone()) => lease.map_err(BridgeError::Validation)?,
                };
                if !cwd.current()
                    || Folder::capture(&inner.git_root(cwd.path.clone()).await?)
                        .map_err(BridgeError::Validation)?
                        .handle
                        != root.handle
                {
                    return Err(BridgeError::Validation(
                        "The Git execution folder changed while waiting".into(),
                    ));
                }
                request.cwd = cwd.path;
                // A started Git workflow may have several subprocesses. Finish
                // the accepted operation under its lease even after disconnect.
                inner.git(request).await
            });
            let _observer = observer;
            worker.await.map_err(stopped)?
        })
    }
}

#[cfg(test)]
mod tests;
