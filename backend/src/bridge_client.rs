//! Outbound bridge client for the Spin backend.
//!
//! Communicates with the companion `openwebide-bridge` daemon via HTTP POST `/exec`
//! to execute workspace commands in remote mode.

use std::future::Future;

use openwebide_agent::BridgeClient;
use openwebide_core::CommandOutcome;

/// Bridge client that delegates process execution to an external bridge daemon
/// via HTTP POST `/exec`.
#[derive(Clone)]
pub struct SpinBridgeClient<S = std::sync::Arc<openwebide_storage::Store<crate::state::AppDb>>> {
    store: S,
    project_dir: String,
    host_session: Option<(i64, i64)>,
}

impl<S> SpinBridgeClient<S> {
    pub fn for_host(store: S, user: i64, session: i64) -> Self {
        Self {
            store,
            project_dir: String::new(),
            host_session: Some((user, session)),
        }
    }
    pub fn for_project(store: S, dir: String) -> Self {
        Self {
            store,
            project_dir: dir,
            host_session: None,
        }
    }
}

impl<
    S: Clone + std::ops::Deref<Target = openwebide_storage::Store<crate::state::AppDb>> + Send + Sync,
> BridgeClient for SpinBridgeClient<S>
{
    async fn host_admin(
        &self,
        request: &openwebide_core::host_admin::HostRequest,
    ) -> Result<openwebide_core::host_admin::HostResponse, String> {
        let (user, session) = self
            .host_session
            .ok_or("Host administration is available only in project-less chat.")?;
        self.store
            .require_host_session(user, session)
            .await
            .map_err(|error| error.to_string())?;
        let payload =
            serde_json::json!({"user":user, "session":session, "request":request}).to_string();
        let (status, body) = crate::bridge::send(&self.store, "/host/admin", payload)
            .await
            .map_err(|error| format!("{error:?}"))?;
        if status != 200 {
            return Err(format!(
                "Host administration bridge HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            ));
        }
        serde_json::from_slice(&body).map_err(|error| error.to_string())
    }

    async fn host_info(&self) -> Result<openwebide_core::HostInfo, String> {
        let (status, body) = crate::bridge::send(&self.store, "/host/info", "{}".into())
            .await
            .map_err(|error| format!("{error:?}"))?;
        if status != 200 {
            return Err(format!("Bridge HTTP {status}"));
        }
        serde_json::from_slice(&body).map_err(|error| error.to_string())
    }

    async fn context_status(&self) -> openwebide_agent::clients::ContextStatus {
        let work = Box::pin(futures::future::join(self.environment(), self.git_status()));
        let deadline = Box::pin(spin_sdk::time::sleep(std::time::Duration::from_secs(2)));
        match futures::future::select(work, deadline).await {
            futures::future::Either::Left((status, _)) => status,
            futures::future::Either::Right(_) => {
                openwebide_agent::clients::context_status_unavailable()
            }
        }
    }

    async fn environment(&self) -> Result<openwebide_core::ExecutionEnvironment, String> {
        let (status, body) = crate::bridge::send(&self.store, "/environment", "{}".into())
            .await
            .map_err(|error| format!("{error:?}"))?;
        if status != 200 {
            return Err(format!("Bridge HTTP {status}"));
        }
        serde_json::from_slice(&body).map_err(|error| error.to_string())
    }

    fn execute_command(
        &self,
        command: &str,
        timeout_seconds: u64,
    ) -> impl Future<Output = Result<CommandOutcome, String>> + Send {
        let payload = serde_json::json!({
            "command": command,
            "timeout_seconds": timeout_seconds,
            "cwd": self.project_dir,
            "cancel_on_disconnect": true,
        })
        .to_string();

        let store = self.store.clone();
        async move {
            let (status, body_bytes) = crate::bridge::send(&store, "/exec", payload)
                .await
                .map_err(|e| match e {
                    crate::git::BridgeError::Unreachable(msg) => format!("Failed to connect to bridge daemon: {msg}. Ensure 'openwebide-bridge' is running."),
                    crate::git::BridgeError::Unauthorized => "Bridge error (HTTP 401): bridge secret required".to_string(),
                    crate::git::BridgeError::NoSecret => "Bridge error: no secret configured (set SPIN_VARIABLE_BRIDGE_SECRET)".to_string(),
                    _ => format!("Bridge error: {e:?}"),
                })?;

            if !(200..300).contains(&status) {
                let err_text = String::from_utf8_lossy(&body_bytes);
                return Err(format!("Bridge error (HTTP {status}): {err_text}"));
            }

            serde_json::from_slice::<CommandOutcome>(&body_bytes)
                .map_err(|e| format!("Failed to parse bridge command outcome JSON: {e}"))
        }
    }

    #[allow(clippy::manual_async_fn)] // matches the explicit `impl Future + Send` style of the sibling trait methods below
    fn git_status(
        &self,
    ) -> impl Future<Output = Result<openwebide_core::GitRepoStatus, String>> + Send {
        let dir = self.project_dir.clone();
        let store = self.store.clone();
        async move {
            crate::git::repo_status(&store, &dir)
                .await
                .map_err(|e| format!("{e:?}"))
        }
    }

    fn git_diff(&self, path: Option<&str>) -> impl Future<Output = Result<String, String>> + Send {
        let path_owned = path.map(ToString::to_string);
        let dir = self.project_dir.clone();
        let store = self.store.clone();
        async move {
            crate::git::repo_diff(&store, &dir, path_owned.as_deref())
                .await
                .map_err(|e| format!("{e:?}"))
        }
    }

    fn git_commit(
        &self,
        req: &openwebide_core::GitCommitRequest,
    ) -> impl Future<Output = Result<openwebide_core::GitCommitResult, String>> + Send {
        let req_clone = req.clone();
        let dir = self.project_dir.clone();
        let store = self.store.clone();
        async move {
            crate::git::repo_commit(&store, &dir, &req_clone)
                .await
                .map_err(|e| format!("{e:?}"))
        }
    }

    fn git_checkout(
        &self,
        req: &openwebide_core::GitCheckoutRequest,
    ) -> impl Future<Output = Result<openwebide_core::GitCheckoutResult, String>> + Send {
        let req_clone = req.clone();
        let dir = self.project_dir.clone();
        let store = self.store.clone();
        async move {
            crate::git::repo_checkout(&store, &dir, &req_clone)
                .await
                .map_err(|e| format!("{e:?}"))
        }
    }
}
