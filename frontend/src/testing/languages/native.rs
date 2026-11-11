//! Native RPC fixture for editor policy tests. It uses real shared language
//! preparation, grants and reply validation against the ordinary fake backend.
use crate::{backend::Backend, testing::fake_backend::FakeBackend};
use openwebide_agent::plugins::{
    actions::PluginActionHost,
    execution::{GrantedHost, PluginTransport},
};
use openwebide_core::plugins::{
    PreparedPlugin,
    execution::{
        ContinuePlugin, InvokePlugin, PluginExecutionContext, PluginGrantRequest,
        PluginHostRequest, PluginInvocation,
    },
    languages::intelligence::{Call, Response},
};
use send_wrapper::SendWrapper;
use std::rc::Rc;

#[derive(Clone)]
pub struct NativeLanguageHost {
    backend: SendWrapper<Rc<FakeBackend>>,
    project: i64,
    current: SendWrapper<Rc<dyn Fn() -> bool>>,
}

impl NativeLanguageHost {
    pub fn new(
        backend: Rc<FakeBackend>,
        project: i64,
        current: impl Fn() -> bool + 'static,
    ) -> Self {
        Self {
            backend: SendWrapper::new(backend),
            project,
            current: SendWrapper::new(Rc::new(current)),
        }
    }
}

impl PluginActionHost for NativeLanguageHost {
    async fn prepare(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        SendWrapper::new(async move {
            self.backend
                .prepare_plugin(Some(self.project), &plugin.source)
                .await
        })
        .await
    }

    async fn grant(&self, plugin: &PreparedPlugin) -> Result<String, String> {
        SendWrapper::new(async move {
            self.backend
                .plugin_context_grants(&PluginGrantRequest {
                    context: PluginExecutionContext {
                        user_action: true,
                        project_id: Some(self.project),
                        session_id: None,
                        primary: None,
                    },
                    plugins: vec![plugin.clone()],
                })
                .await?
                .get(&plugin.digest)
                .cloned()
                .ok_or_else(|| "No native-language fixture authority".into())
        })
        .await
    }

    fn is_current(&self) -> bool {
        (self.current)()
    }

    async fn language_server(&self, call: &Call) -> Result<Response, String> {
        SendWrapper::new(async move { self.backend.language_server(call).await }).await
    }
}

impl PluginTransport for NativeLanguageHost {
    async fn ensure_prepared(&self, expected: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.prepare(expected).await
    }

    async fn start(&self, _call: InvokePlugin) -> Result<PluginInvocation, String> {
        Err("The native-language fixture does not execute components".into())
    }

    async fn resume(&self, _response: ContinuePlugin) -> Result<PluginInvocation, String> {
        Err("The native-language fixture does not execute components".into())
    }

    fn cancel(&self, _id: String) {}
}

impl GrantedHost for NativeLanguageHost {
    async fn request(&self, _request: &PluginHostRequest) -> Result<String, String> {
        Err("The native-language fixture does not execute component host calls".into())
    }
}
