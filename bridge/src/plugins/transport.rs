//! Thin native transport for the shared agent invocation workflow.
use super::{NativePluginInstaller, invocations::Invocations};
use openwebide_agent::plugins::execution::PluginTransport;
use openwebide_core::plugins::execution::{ContinuePlugin, InvokePlugin, PluginInvocation};

#[derive(Clone, Debug, Default)]
pub struct PluginExecutionHost {
    pub installer: NativePluginInstaller,
    pub invocations: Invocations,
    pub paired: bool,
}
impl PluginExecutionHost {
    pub fn transport(&self, owner: i64) -> NativePluginTransport {
        NativePluginTransport {
            installer: self.installer.clone(),
            invocations: self.invocations.clone(),
            owner: if self.paired {
                "paired".into()
            } else {
                format!("user:{owner}")
            },
        }
    }
}

#[derive(Clone)]
pub struct NativePluginTransport {
    pub installer: NativePluginInstaller,
    pub invocations: Invocations,
    pub owner: String,
}
impl PluginTransport for NativePluginTransport {
    async fn ensure_prepared(
        &self,
        expected: &openwebide_core::plugins::PreparedPlugin,
    ) -> Result<openwebide_core::plugins::PreparedPlugin, String> {
        self.installer
            .prepare(&self.owner, expected.host_id.clone(), &expected.source)
            .await
            .map_err(|error| error.to_string())
    }

    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        self.invocations
            .start(&self.installer, &self.owner, call)
            .await
    }
    async fn resume(&self, response: ContinuePlugin) -> Result<PluginInvocation, String> {
        self.invocations.resume(&self.owner, response).await
    }
    fn cancel(&self, id: String) {
        let invocations = self.invocations.clone();
        let owner = self.owner.clone();
        tokio::spawn(async move {
            let _ = invocations.cancel(&owner, &id).await;
        });
    }
}
