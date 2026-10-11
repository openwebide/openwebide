//! Authenticated native plugin RPC, independent of workspace folder mapping.
use crate::bridge::BridgeCredentials;
use openwebide_core::plugins::{PluginPackage, PluginSource, PreparedPlugin};

#[derive(Clone)]
pub struct PluginBridgeClient {
    http_url: String,
    credentials: BridgeCredentials,
}

impl PluginBridgeClient {
    pub fn new(http_url: String, credentials: BridgeCredentials) -> Self {
        Self {
            http_url,
            credentials,
        }
    }

    pub(crate) async fn plugin_request<T: serde::de::DeserializeOwned>(
        &self,
        operation: &str,
        payload: serde_json::Value,
    ) -> Result<T, String> {
        let token = self.credentials.credential().await?;
        let guard = crate::api::CommandFetchGuard(
            web_sys::AbortController::new().map_err(|error| format!("{error:?}"))?,
        );
        let response =
            gloo_net::http::Request::post(&format!("{}/plugins/{operation}", self.http_url))
                .abort_signal(Some(&guard.0.signal()))
                .header("Content-Type", "application/json")
                .header("Authorization", &format!("Bearer {token}"))
                .body(payload.to_string())
                .map_err(|error| error.to_string())?
                .send()
                .await
                .map_err(|error| format!("Plugin host unavailable: {error}"))?;
        if !response.ok() {
            return Err(openwebide_core::plugins::execution::host_rpc_error(
                response.status(),
                response.text().await.unwrap_or_default().as_bytes(),
            ));
        }
        response.json().await.map_err(|error| error.to_string())
    }

    pub async fn prepare_plugin(&self, source: &PluginSource) -> Result<PreparedPlugin, String> {
        self.plugin_request("prepare", serde_json::json!({"source": source}))
            .await
    }
    pub async fn preparation(
        &self,
        command: &openwebide_core::plugins::preparation::PreparationCommand,
    ) -> Result<openwebide_core::plugins::preparation::PluginPreparation, String> {
        command.validate().map_err(|error| error.to_string())?;
        self.plugin_request(
            &format!("prepare/{}", command.operation()),
            command.host_payload(),
        )
        .await
    }

    pub async fn plugin_package(&self, expected: &PreparedPlugin) -> Result<PluginPackage, String> {
        self.plugin_request("package", serde_json::json!({"prepared": expected}))
            .await
    }
}
