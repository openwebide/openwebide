//! Thin native process, RPC and clock adapter for shared raw run delivery.
use crate::{
    ServerConfig,
    runs::{Run, StartRun, backend_client::BackendClient, http_client::ReqwestHttpClient},
};
use openwebide_core::{RunEvent, RunRejectCode, RunStep, plugins::runs::*};
use std::{sync::Arc, time::Duration};
#[derive(Clone)]
struct Host {
    config: ServerConfig,
    backend: Arc<BackendClient>,
    http: ReqwestHttpClient,
}
impl openwebide_agent::plugins::runs::RunHost for Host {
    type Run = Arc<Run>;
    async fn request(&self, command: RunServiceRequest) -> Result<RunServiceResponse, String> {
        tokio::time::timeout(
            Duration::from_secs(15),
            self.backend.plugin_run_service(&command),
        )
        .await
        .map_err(|_| "Run service request timed out".to_string())?
    }
    async fn start(&self, delivery: &RunDelivery) -> Result<Arc<Run>, (RunRejectCode, String)> {
        let principal = crate::auth::Principal::User {
            user_id: delivery.user_id,
        };
        let start = StartRun {
            run_id: delivery.run_id(),
            session_id: delivery.prompt.session_id,
            content: delivery.prompt.content.clone(),
            model: None,
            editor_context: None,
            browser_preferences: None,
            queued_prompt: Some(delivery.prompt.key()),
            host_path: delivery.host_path.clone(),
        };
        let run = self.config.runs.reserve(&principal, &start)?;
        self.config
            .runs
            .prepare(
                run,
                start,
                &self.config.workspace_root,
                self.backend.clone(),
                |plan| {
                    openwebide_llm::registry::Provider::for_connection_with_memo(
                        &plan.connection,
                        self.http.clone().with_transport(plan.transport.clone()),
                        self.config
                            .tool_stream_memos
                            .get_or_insert(&plan.connection),
                    )
                },
                crate::runs::RunHost {
                    execution: self.config.execution.clone(),
                    plugins: super::transport::PluginExecutionHost {
                        installer: self.config.plugins.clone(),
                        invocations: self.config.plugin_invocations.clone(),
                        paired: self.config.pairing_token.is_some(),
                    },
                },
            )
            .await
    }
    fn snapshot(&self, run: &Arc<Run>) -> (Option<RunEvent>, Option<RunStep>) {
        run.scheduled_status()
    }
    fn cancel(&self, delivery: &RunDelivery) {
        if let Ok(run) = self.config.runs.get(
            &crate::auth::Principal::User {
                user_id: delivery.user_id,
            },
            &delivery.run_id(),
        ) {
            run.cancel.cancel();
        }
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
    fn report(&self, error: &str) {
        tracing::debug!(%error,"plugin run delivery unavailable");
    }
}
pub async fn serve(config: ServerConfig) {
    let mut hosts = vec![crate::scheduled::host(&config).id];
    if config.pairing_token.is_none() && hosts[0] != "server" {
        hosts.push("server".into());
    }
    let http = ReqwestHttpClient::default();
    let backend = Arc::new(BackendClient::new(
        config.backend_url.clone(),
        config.secret.clone(),
        http.clone(),
    ));
    openwebide_agent::plugins::runs::serve(
        Host {
            config,
            backend,
            http,
        },
        hosts,
    )
    .await;
}
