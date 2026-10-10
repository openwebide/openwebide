use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use openwebide_agent::{BridgeClient, CancelCheck, PermissionGate};
use openwebide_core::{
    CommandOutcome, GitCheckoutRequest, GitCheckoutResult, GitCommitRequest, GitCommitResult,
    GitRepoStatus, ToolCall,
};
use tokio::sync::{Notify, oneshot};

use crate::runs::backend_client::RunBackend;

pub struct InProcessBridgeClient {
    pub dir: PathBuf,
    pub execution: Arc<dyn crate::exec::ToolExecution>,
    pub cancel: BridgeCancel,
}

impl BridgeClient for InProcessBridgeClient {
    async fn host_info(&self) -> Result<openwebide_core::HostInfo, String> {
        self.execution
            .host_info()
            .await
            .map_err(|error| error.to_string())
    }
    async fn context_status(&self) -> openwebide_agent::clients::ContextStatus {
        tokio::time::timeout(
            Duration::from_secs(2),
            futures::future::join(self.environment(), self.git_status()),
        )
        .await
        .unwrap_or_else(|_| openwebide_agent::clients::context_status_unavailable())
    }

    async fn environment(&self) -> Result<openwebide_core::ExecutionEnvironment, String> {
        Ok(crate::exec::environment())
    }

    async fn execute_command(
        &self,
        command: &str,
        timeout_seconds: u64,
    ) -> Result<CommandOutcome, String> {
        let cancel = self.cancel.clone();
        self.execution
            .run_command(crate::exec::SpawnSpec::shell(
                command.into(),
                self.dir.clone(),
                timeout_seconds,
                async move { cancel.cancelled().await },
            ))
            .await
            .map_err(|error| error.to_string())
    }
    async fn git_status(&self) -> Result<GitRepoStatus, String> {
        match self.git(crate::exec::GitOperation::Status).await? {
            crate::exec::GitResponse::Status(status) => Ok(status),
            _ => Err("unexpected Git status response".into()),
        }
    }
    async fn git_diff(&self, path: Option<&str>) -> Result<String, String> {
        match self
            .git(crate::exec::GitOperation::Diff(path.map(String::from)))
            .await?
        {
            crate::exec::GitResponse::Diff(diff) => Ok(diff.diff),
            _ => Err("unexpected Git diff response".into()),
        }
    }
    async fn git_commit(&self, req: &GitCommitRequest) -> Result<GitCommitResult, String> {
        match self
            .git(crate::exec::GitOperation::Commit(req.clone()))
            .await?
        {
            crate::exec::GitResponse::Commit(result) => Ok(result),
            _ => Err("unexpected Git commit response".into()),
        }
    }
    async fn git_checkout(&self, req: &GitCheckoutRequest) -> Result<GitCheckoutResult, String> {
        match self
            .git(crate::exec::GitOperation::Checkout(req.clone()))
            .await?
        {
            crate::exec::GitResponse::Checkout(result) => Ok(result),
            _ => Err("unexpected Git checkout response".into()),
        }
    }
}

impl InProcessBridgeClient {
    async fn git(
        &self,
        operation: crate::exec::GitOperation,
    ) -> Result<crate::exec::GitResponse, String> {
        self.execution
            .git(crate::exec::GitRequest {
                cwd: self.dir.clone(),
                operation,
            })
            .await
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug, Default)]
pub struct BridgeCancel {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl BridgeCancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    pub async fn cancelled(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.is_cancelled() {
            notified.await;
        }
    }
}

impl CancelCheck for BridgeCancel {
    async fn cancelled(&self) {
        BridgeCancel::cancelled(self).await;
    }
    async fn check(&self) -> bool {
        self.is_cancelled()
    }
}

#[derive(Debug)]
struct PendingPermission {
    id: String,
    sender: Option<oneshot::Sender<bool>>,
    receiver: Option<oneshot::Receiver<bool>>,
}

#[derive(Clone, Debug)]
pub struct BridgeGate {
    pending: Arc<Mutex<Option<PendingPermission>>>,
    cancel: BridgeCancel,
}

impl BridgeGate {
    pub fn new(cancel: BridgeCancel) -> Self {
        Self {
            pending: Arc::new(Mutex::new(None)),
            cancel,
        }
    }
    pub fn decide(&self, id: &str, approved: bool) -> Result<(), String> {
        let mut pending = self.pending.lock().unwrap();
        let current = pending
            .as_mut()
            .filter(|p| p.id == id)
            .ok_or_else(|| format!("no pending permission for {id}"))?;
        let sender = current
            .sender
            .take()
            .ok_or_else(|| format!("no pending permission for {id}"))?;
        sender
            .send(approved)
            .map_err(|_| format!("no pending permission for {id}"))
    }
    pub fn clear(&self) {
        self.pending.lock().unwrap().take();
    }
    // Register before publishing the permission event, so an immediate reply cannot be lost.
    pub fn prepare(&self, id: &str) {
        let (sender, receiver) = oneshot::channel();
        *self.pending.lock().unwrap() = Some(PendingPermission {
            id: id.to_string(),
            sender: Some(sender),
            receiver: Some(receiver),
        });
    }
}

impl PermissionGate for BridgeGate {
    async fn approve(&self, call: &ToolCall) -> bool {
        let receiver = {
            if self.pending.lock().unwrap().is_none() {
                self.prepare(&call.id);
            }
            self.pending
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .receiver
                .take()
                .unwrap()
        };
        let decision = tokio::select! {
            result = receiver => result.unwrap_or(false),
            () = self.cancel.cancelled() => false,
            () = tokio::time::sleep(Duration::from_secs(300)) => false,
        };
        self.pending.lock().unwrap().take();
        decision
    }
}

/// Project-independent inspection and project-less host administration capability.
pub struct HostInfoClient<B> {
    pub host_administration: bool,
    pub execution: Arc<dyn crate::exec::ToolExecution>,
    pub backend: Arc<B>,
    pub user: i64,
    pub session: i64,
}
impl<B: RunBackend + 'static> BridgeClient for HostInfoClient<B> {
    async fn host_info(&self) -> Result<openwebide_core::HostInfo, String> {
        self.execution
            .host_info()
            .await
            .map_err(|error| error.to_string())
    }
    async fn host_admin(
        &self,
        request: &openwebide_core::host_admin::HostRequest,
    ) -> Result<openwebide_core::host_admin::HostResponse, String> {
        if !self.host_administration {
            return Err("Host administration requires the authenticated server bridge.".into());
        }
        crate::host_admin::request(
            self.backend.clone(),
            self.execution.clone(),
            self.user,
            self.session,
            request.clone(),
        )
        .await
    }
    async fn execute_command(
        &self,
        _command: &str,
        _timeout_seconds: u64,
    ) -> Result<CommandOutcome, String> {
        Err("Commands require a project".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call() -> ToolCall {
        ToolCall {
            id: "t".into(),
            name: "write_file".into(),
            arguments: "{}".into(),
        }
    }

    #[tokio::test]
    async fn permission_ids_are_pending_and_decisions_used_once() {
        let gate = BridgeGate::new(BridgeCancel::default());
        assert!(gate.decide("t", true).is_err());
        gate.prepare("t");
        assert!(gate.decide("other", true).is_err());
        gate.decide("t", true).unwrap();
        assert!(gate.decide("t", false).is_err());
        assert!(gate.approve(&call()).await);
        assert!(gate.decide("t", true).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn permission_timeout_denies_and_cancel_wakes_waiter() {
        let cancel = BridgeCancel::default();
        let gate = BridgeGate::new(cancel.clone());
        let waiter_gate = gate.clone();
        let waiter = tokio::spawn(async move { waiter_gate.approve(&call()).await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(300)).await;
        assert!(!waiter.await.unwrap());
        assert!(gate.decide("t", true).is_err());
        let waiter = tokio::spawn(async move { gate.approve(&call()).await });
        tokio::task::yield_now().await;
        cancel.cancel();
        assert!(!waiter.await.unwrap());
    }
}
