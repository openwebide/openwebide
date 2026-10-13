//! Resident host execution and transport adapters for shared administration policy.
pub mod interactive;
pub mod ssh;
pub mod updates;
use crate::{exec::ToolExecution, runs::backend_client::RunBackend};
use openwebide_core::host_admin::{
    HostAdapter, HostCommand, HostJournalCommand, HostJournalResult, HostOperation,
    HostOperationAdapter, HostRequest, HostResponse, OperationState, execute_operation,
};
use openwebide_core::host_admin::{HostInventoryAdapter, inspect_host};
use ssh::SshHost;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, OnceLock},
};

fn operation_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}
fn live_operations() -> &'static Mutex<HashSet<i64>> {
    static LIVE: OnceLock<Mutex<HashSet<i64>>> = OnceLock::new();
    LIVE.get_or_init(Mutex::default)
}
struct OperationGuard(i64);
impl OperationGuard {
    fn new(id: i64) -> Self {
        live_operations().lock().unwrap().insert(id);
        Self(id)
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        live_operations().lock().unwrap().remove(&self.0);
    }
}

pub async fn connection<B: RunBackend>(
    backend: &B,
) -> Result<openwebide_core::host_admin::HostConnection, String> {
    match backend
        .host_journal(&HostJournalCommand::Connection)
        .await?
    {
        HostJournalResult::Connection(connection) => Ok(connection),
        _ => Err("Invalid host connection response".into()),
    }
}
pub async fn request<B: RunBackend + 'static>(
    backend: Arc<B>,
    execution: Arc<dyn ToolExecution>,
    user: i64,
    session: i64,
    request: HostRequest,
) -> Result<HostResponse, String> {
    let _guard = operation_gate().lock().await;
    let host = SshHost::connect(connection(&*backend).await?, execution).await;
    openwebide_core::host_admin::host_request(
        &HostRuntime { backend, host },
        user,
        session,
        request,
    )
    .await
}
struct HostRuntime<B> {
    backend: Arc<B>,
    host: Result<SshHost, String>,
}
impl<B> HostRuntime<B> {
    fn host(&self) -> Result<&SshHost, String> {
        self.host.as_ref().map_err(Clone::clone)
    }
}
impl<B: RunBackend + 'static> HostAdapter for HostRuntime<B> {
    fn connection_revision(&self) -> i64 {
        self.host.as_ref().map_or(0, |host| host.revision)
    }
    async fn overview(&self) -> Result<openwebide_core::host_admin::HostEnvironment, String> {
        self.host()?.overview().await
    }
    async fn identity(&self) -> Result<(String, String), String> {
        {
            let overview = self.host()?.overview().await?;
            Ok((overview.target, overview.boot_id))
        }
    }
    async fn environment(&self) -> Result<openwebide_core::host_admin::HostEnvironment, String> {
        inspect_host(self.host()?).await
    }
    async fn journal(&self, command: &HostJournalCommand) -> Result<HostJournalResult, String> {
        self.backend.host_journal(command).await
    }
    async fn launch(&self, operation: HostOperation) -> Result<(), String> {
        let adapter = Self {
            backend: self.backend.clone(),
            host: self.host.clone(),
        };
        spawn_operation(adapter, operation, false);
        Ok(())
    }
}
fn spawn_operation<B: RunBackend + 'static>(
    adapter: HostRuntime<B>,
    mut operation: HostOperation,
    recovery: bool,
) {
    let guard = OperationGuard::new(operation.id);
    tokio::spawn(async move {
        let _guard = guard;
        if let Err(error) = execute_operation(&adapter, &mut operation, recovery).await {
            tracing::error!(operation=operation.id, %error, "Host operation persistence failed; no further commands executed");
        }
    });
}
impl<B: RunBackend> HostOperationAdapter for HostRuntime<B> {
    async fn command_stream(
        &self,
        command: &HostCommand,
        context: openwebide_core::host_admin::HostCommandContext,
    ) -> Result<openwebide_core::host_admin::HostProcessStream, String> {
        if command.interactive {
            self.host()?.interactive(command, context)
        } else {
            let outcome = self.command(command).await?;
            Ok(Box::pin(futures::stream::once(async move {
                Ok(openwebide_core::host_admin::HostProcessEvent::Finished(
                    outcome,
                ))
            })))
        }
    }
    async fn command(
        &self,
        command: &HostCommand,
    ) -> Result<openwebide_core::CommandOutcome, String> {
        self.host()?.command(command).await
    }
    async fn save(&self, operation: &HostOperation) -> Result<(), String> {
        self.backend
            .host_journal(&HostJournalCommand::Save {
                operation: operation.clone(),
            })
            .await?;
        Ok(())
    }
    fn now(&self) -> i64 {
        now()
    }
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| i64::try_from(time.as_secs()).unwrap_or(i64::MAX))
}
/// Retry after boot or SSH outages; process-local guards distinguish resident work from stale rows.
pub async fn recover<B: RunBackend + 'static>(
    backend: Arc<B>,
    execution: Arc<dyn ToolExecution>,
) -> Result<(), String> {
    use openwebide_core::host_admin::{HostRecovery, host_recovery, reconnect_expired};
    let _guard = operation_gate().lock().await;
    let config = connection(&*backend).await?;
    if !config.is_configured() {
        return Ok(());
    }
    let HostJournalResult::Operations(operations) =
        backend.host_journal(&HostJournalCommand::Active).await?
    else {
        return Err("Invalid journal response".into());
    };
    let pending = operations
        .into_iter()
        .filter(|operation| !live_operations().lock().unwrap().contains(&operation.id))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(());
    }
    let host = SshHost::connect(config, execution).await?;
    let adapter = HostRuntime {
        backend,
        host: Ok(host),
    };
    let mut remaining = Vec::new();
    for mut operation in pending {
        if reconnect_expired(&operation, now()) {
            operation.state = OperationState::Interrupted;
            operation.input_token = None;
            operation.updated_at = now();
            operation.detail = "Host did not reconnect within an hour. Inspect the host; commands were not replayed.".into();
            adapter.save(&operation).await?;
        } else {
            remaining.push(operation);
        }
    }
    if remaining.is_empty() {
        return Ok(());
    }
    let overview = adapter.host()?.overview().await?;
    for mut operation in remaining {
        match host_recovery(
            &operation,
            &overview.target,
            &overview.boot_id,
            adapter.host()?.revision,
            now(),
        ) {
            HostRecovery::Wait => {}
            HostRecovery::Verify => {
                spawn_operation(
                    HostRuntime {
                        backend: adapter.backend.clone(),
                        host: adapter.host.clone(),
                    },
                    operation,
                    true,
                );
            }
            HostRecovery::Interrupt => {
                operation.state = OperationState::Interrupted;
                operation.input_token = None;
                operation.updated_at = now();
                operation.detail = "Host or bridge changed before completion, or reboot was not observed within an hour. Inspect the host; commands were not replayed.".into();
                adapter.save(&operation).await?;
            }
        }
    }
    Ok(())
}
pub async fn serve(config: crate::ServerConfig) {
    if config.pairing_token.is_some() {
        return;
    }
    let backend = Arc::new(crate::runs::backend_client::BackendClient::new(
        config.backend_url,
        config.secret,
        crate::runs::http_client::ReqwestHttpClient::default(),
    ));
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if let Err(error) = recover(backend.clone(), config.execution.clone()).await {
            tracing::debug!(%error, "Host operation recovery will retry");
        }
    }
}

#[cfg(test)]
mod tests;
