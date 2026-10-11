use super::*;
use openwebide_core::plugins::{PluginTool, RustPlugin, execution::PluginStep};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
struct Host {
    profile: &'static str,
    current: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<&'static str>>>,
    changed: bool,
    deny: bool,
    stale_after_start: bool,
    stale_before_callback: bool,
}
impl Host {
    fn new(profile: &'static str) -> Self {
        Self {
            profile,
            current: Arc::new(AtomicBool::new(true)),
            calls: Arc::default(),
            changed: false,
            deny: false,
            stale_after_start: false,
            stale_before_callback: false,
        }
    }
    fn record(&self, value: &'static str) {
        self.calls.lock().unwrap().push(value);
    }
}
impl PluginActionHost for Host {
    async fn prepare(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.record("prepare");
        let mut prepared = plugin.clone();
        prepared.host_id = self.profile.into();
        if self.changed {
            prepared.digest = "c".repeat(64);
        }
        Ok(prepared)
    }
    async fn grant(&self, _: &PreparedPlugin) -> Result<String, String> {
        self.record("grant");
        if self.deny {
            Err("denied".into())
        } else {
            Ok("opaque".into())
        }
    }
    fn is_current(&self) -> bool {
        self.current.load(Ordering::Relaxed)
    }
}
impl PluginTransport for Host {
    async fn ensure_prepared(&self, expected: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        Ok(expected.clone())
    }

    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        self.record("start");
        assert_eq!(call.prepared.host_id, self.profile);
        assert_eq!(call.name, "remember");
        if self.stale_after_start {
            self.current.store(false, Ordering::Relaxed);
        }
        Ok(PluginInvocation {
            id: "actor".into(),
            step: PluginStep::Ready,
        })
    }
    async fn resume(&self, response: ContinuePlugin) -> Result<PluginInvocation, String> {
        self.record("resume");
        assert_eq!(response.id, "actor");
        let step = if response.sequence == 0 {
            if self.stale_before_callback {
                self.current.store(false, Ordering::Relaxed);
            }
            PluginStep::HostCall {
                sequence: 1,
                capability: "records".into(),
                payload: "{}".into(),
            }
        } else {
            assert_eq!(response.response, Ok("stored".into()));
            PluginStep::Complete {
                ok: true,
                content: "plugin result".into(),
                summary: "Saved".into(),
            }
        };
        Ok(PluginInvocation {
            id: "actor".into(),
            step,
        })
    }
    fn cancel(&self, _: String) {
        self.record("cancel");
    }
}
impl GrantedHost for Host {
    async fn request(&self, request: &PluginHostRequest) -> Result<String, String> {
        self.record("callback");
        assert_eq!(request.grant, "opaque");
        assert_eq!(request.capability, "records");
        Ok("stored".into())
    }
}
fn binding() -> ProjectPlugin {
    let mut prepared = openwebide_core::plugins::testing::receipt();
    prepared.manifest.publisher = "community".into();
    prepared.manifest.name = "notes".into();
    prepared.manifest.compatibility.plugin_api = 3;
    prepared.manifest.contributions.skills.clear();
    prepared.manifest.contributions.tools = vec![PluginTool {
        name: "remember".into(),
        description: "Store supplied facts".into(),
        parameters: serde_json::json!({"type":"object"}),
        requires_approval: true,
    }];
    prepared.manifest.executable = Some(RustPlugin {
        manifest: "Cargo.toml".into(),
        library: "notes".into(),
        sdk_version: "0.1.0".into(),
        capabilities: vec!["records".into()],
    });
    ProjectPlugin {
        id: 1,
        prepared,
        enabled: true,
        revision: 1,
    }
}
fn call() -> ToolCall {
    ToolCall {
        id: "ui".into(),
        name: "remember".into(),
        arguments: "{}".into(),
    }
}

#[test]
fn actions_use_declared_tools_and_the_shared_executor_on_both_hosts() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let host = Host::new(profile);
            let outcome = invoke_plugin_action(&host, &[binding()], &call(), true)
                .await
                .unwrap();
            assert!(outcome.ok);
            assert_eq!(outcome.content, "plugin result");
            assert_eq!(
                *host.calls.lock().unwrap(),
                ["prepare", "grant", "start", "resume", "callback", "resume"]
            );
        }
    });
}
#[test]
fn missing_disabled_ambiguous_or_unapproved_actions_have_no_host_effects() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let host = Host::new(profile);
            let mut disabled = binding();
            disabled.enabled = false;
            for bindings in [vec![], vec![disabled], vec![binding(), binding()]] {
                assert!(
                    invoke_plugin_action(&host, &bindings, &call(), true)
                        .await
                        .is_err()
                );
            }
            assert!(
                invoke_plugin_action(&host, &[binding()], &call(), false)
                    .await
                    .is_err()
            );
            assert!(host.calls.lock().unwrap().is_empty());
        }
    });
}
#[test]
fn preparation_and_authority_failures_never_start_plugin_code() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            for (changed, deny) in [(true, false), (false, true)] {
                let host = Host {
                    changed,
                    deny,
                    ..Host::new(profile)
                };
                assert!(
                    invoke_plugin_action(&host, &[binding()], &call(), true)
                        .await
                        .is_err()
                );
                assert!(!host.calls.lock().unwrap().contains(&"start"));
            }
        }
    });
}
#[test]
fn stale_actions_cancel_the_actor_before_continuation_or_storage_callback() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            for (stale_after_start, stale_before_callback) in [(true, false), (false, true)] {
                let host = Host {
                    stale_after_start,
                    stale_before_callback,
                    ..Host::new(profile)
                };
                assert!(
                    invoke_plugin_action(&host, &[binding()], &call(), true)
                        .await
                        .is_err()
                );
                let calls = host.calls.lock().unwrap();
                assert!(calls.contains(&"cancel"));
                assert!(!calls.contains(&"callback"));
            }
        }
    });
}
