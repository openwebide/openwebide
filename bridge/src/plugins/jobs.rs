//! Native RPC, timer and cache adapter for the shared durable delivery worker.
use crate::{ServerConfig, runs::backend_client::BackendClient};
use openwebide_agent::plugins::jobs::{JobHost, JobWorkerHost};
use openwebide_core::plugins::jobs::{JobServiceRequest, JobServiceResponse};
use std::{sync::Arc, time::Duration};

#[derive(Clone)]
struct Host {
    backend: Arc<BackendClient>,
    execution: super::transport::PluginExecutionHost,
}
impl JobHost for Host {
    async fn request(&self, command: JobServiceRequest) -> Result<JobServiceResponse, String> {
        let seconds = if matches!(command, JobServiceRequest::Callback { .. }) {
            45
        } else {
            15
        };
        tokio::time::timeout(
            Duration::from_secs(seconds),
            self.backend.plugin_job_service(&command),
        )
        .await
        .map_err(|_| "Plugin job service request timed out".to_string())?
    }
    fn now(&self) -> i64 {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
        .unwrap_or(i64::MAX)
    }
    async fn wait(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}
impl JobWorkerHost for Host {
    type Transport = super::transport::NativePluginTransport;
    fn transport(&self, user_id: i64) -> Self::Transport {
        self.execution.transport(user_id)
    }
    fn report(&self, error: &str) {
        tracing::debug!(%error, "plugin job delivery unavailable");
    }
}
pub async fn serve(config: ServerConfig) {
    let mut hosts = vec![crate::scheduled::host(&config).id];
    if config.pairing_token.is_none() && hosts[0] != "server" {
        hosts.push("server".into());
    }
    let host = Host {
        backend: Arc::new(BackendClient::new(
            config.backend_url,
            config.secret,
            crate::runs::http_client::ReqwestHttpClient::default(),
        )),
        execution: super::transport::PluginExecutionHost {
            paired: config.pairing_token.is_some(),
            installer: config.plugins,
            invocations: config.plugin_invocations,
        },
    };
    openwebide_agent::plugins::jobs::serve(host, hosts).await;
}
