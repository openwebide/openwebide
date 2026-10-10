//! Host component runtime; every plugin uses the same imports and limits.
pub mod build;
pub mod bundled;
pub use openwebide_plugin_sdk as sdk;

use anyhow::{Context, Result, bail};
use openwebide_plugin_sdk::{Outcome, Tool};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({path: "../plugin-sdk/wit", world: "plugin"});

pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
pub const CAPABILITIES: &[&str] = &[
    "http",
    "records",
    "collections",
    "jobs",
    "runs",
    "workspace",
    "clock",
    "completion",
];

/// Adapters implement general primitives, never feature-specific dispatch.
pub trait HostServices: Send {
    fn request(&mut self, capability: &str, payload: &str) -> Result<String, String>;
}

struct ReadOnly<H>(H);
impl<H: HostServices> HostServices for ReadOnly<H> {
    fn request(&mut self, capability: &str, payload: &str) -> Result<String, String> {
        let read =
            openwebide_core::plugins::execution::context_request_allowed(capability, payload);
        if !read {
            return Err("Plugin context hooks cannot mutate host state".into());
        }
        self.0.request(capability, payload)
    }
}
struct NoServices;
impl HostServices for NoServices {
    fn request(&mut self, _: &str, _: &str) -> Result<String, String> {
        Err("Plugin metadata cannot call host services".into())
    }
}

struct State<H> {
    services: H,
    grants: Vec<String>,
    table: ResourceTable,
    wasi: WasiCtx,
    limits: StoreLimits,
}
impl<H: HostServices> WasiView for State<H> {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
impl<H: HostServices> openwebide::plugin::host::Host for State<H> {
    fn request(&mut self, capability: String, payload: String) -> Result<String, String> {
        if !CAPABILITIES.contains(&capability.as_str()) || !self.grants.contains(&capability) {
            return Err("Plugin capability is not granted.".into());
        }
        if payload.len() > MAX_MESSAGE_BYTES {
            return Err("Plugin request exceeds its limit.".into());
        }
        let response = self.services.request(&capability, &payload)?;
        if response.len() > MAX_MESSAGE_BYTES {
            return Err("Plugin response exceeds its limit.".into());
        }
        Ok(response)
    }
}

pub struct Runtime {
    engine: Engine,
}
impl Runtime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true).consume_fuel(true);
        Ok(Self {
            engine: Engine::new(&config)?,
        })
    }
    pub fn validate(&self, bytes: &[u8]) -> Result<()> {
        Component::new(&self.engine, bytes).context("Invalid plugin component")?;
        Ok(())
    }
    /// Build-time bundles and installation use the same public export contract.
    pub fn validate_exports(
        &self,
        bytes: &[u8],
        manifest: &openwebide_core::plugins::PluginManifest,
    ) -> Result<()> {
        let tools: Vec<openwebide_core::plugins::PluginTool> =
            serde_json::from_value(serde_json::to_value(self.tools(bytes, NoServices, &[])?)?)?;
        if tools != manifest.contributions.tools {
            bail!("Compiled tools do not match the manifest.");
        }
        if self.events(bytes)? != manifest.contributions.events {
            bail!("Compiled events do not match the manifest.");
        }
        Ok(())
    }
    fn instantiate<H: HostServices + 'static>(
        &self,
        bytes: &[u8],
        services: H,
        grants: &[String],
    ) -> Result<(Store<State<H>>, Plugin)> {
        if grants
            .iter()
            .any(|grant| !CAPABILITIES.contains(&grant.as_str()))
        {
            bail!("Unsupported plugin capability");
        }
        let component = Component::new(&self.engine, bytes)?;
        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        Plugin::add_to_linker::<_, wasmtime::component::HasSelf<_>>(&mut linker, |state| state)?;
        let mut store = Store::new(
            &self.engine,
            State {
                services,
                grants: grants.to_vec(),
                table: ResourceTable::new(),
                wasi: WasiCtxBuilder::new().build(),
                limits: StoreLimitsBuilder::new()
                    .memory_size(64 * 1024 * 1024)
                    .instances(16)
                    .build(),
            },
        );
        store.limiter(|state| &mut state.limits);
        // Include JSON/base64 transfer and bounded HTML parsing, not just small
        // fixture handlers. Fuel still bounds untrusted CPU work per invocation.
        store.set_fuel(250_000_000)?;
        let plugin = Plugin::instantiate(&mut store, &component, &linker)?;
        Ok((store, plugin))
    }
    pub fn tools<H: HostServices + 'static>(
        &self,
        bytes: &[u8],
        services: H,
        grants: &[String],
    ) -> Result<Vec<Tool>> {
        let (mut store, plugin) = self.instantiate(bytes, services, grants)?;
        let json = plugin.call_tools(&mut store)?;
        if json.len() > MAX_MESSAGE_BYTES {
            bail!("Plugin definitions exceed their limit");
        }
        let tools: Vec<Tool> = serde_json::from_str(&json)?;
        if tools.len() > 100 {
            bail!("Invalid plugin tool count");
        }
        let mut names = std::collections::BTreeSet::new();
        for tool in &tools {
            if tool.name.is_empty()
                || tool.name.len() > 64
                || !tool
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                || !names.insert(&tool.name)
                || tool.description.trim().is_empty()
                || tool.description.len() > 4096
                || tool.parameters.get("type").and_then(|v| v.as_str()) != Some("object")
            {
                bail!("Invalid plugin tool definition");
            }
        }
        Ok(tools)
    }
    pub fn events(&self, bytes: &[u8]) -> Result<Vec<String>> {
        let (mut store, plugin) = self.instantiate(bytes, NoServices, &[])?;
        let json = plugin.call_events(&mut store)?;
        if json.len() > 8192 {
            bail!("Plugin event declarations exceed their limit");
        }
        let events: Vec<String> = serde_json::from_str(&json)?;
        let mut names = std::collections::BTreeSet::new();
        if events.len() > 100
            || events.iter().any(|name| {
                !names.insert(name)
                    || openwebide_core::plugins::execution::validate_event(
                        name,
                        &serde_json::Value::Null,
                    )
                    .is_err()
            })
        {
            bail!("Invalid plugin event declarations");
        }
        Ok(events)
    }
    pub fn execute<H: HostServices + 'static>(
        &self,
        bytes: &[u8],
        services: H,
        grants: &[String],
        name: &str,
        arguments: &str,
    ) -> Result<Outcome> {
        if name.len() > 64 || arguments.len() > MAX_MESSAGE_BYTES {
            bail!("Plugin call exceeds its limit");
        }
        let (mut store, plugin) = self.instantiate(bytes, services, grants)?;
        let json = plugin
            .call_execute(&mut store, name, arguments)?
            .map_err(anyhow::Error::msg)?;
        if json.len() > MAX_MESSAGE_BYTES {
            bail!("Plugin outcome exceeds its limit");
        }
        Ok(serde_json::from_str(&json)?)
    }
    pub fn context<H: HostServices + 'static>(
        &self,
        bytes: &[u8],
        services: H,
        grants: &[String],
        mut input: sdk::ContextInput,
    ) -> Result<sdk::ContextContribution> {
        input.budget_bytes = input.budget_bytes.min(8192);
        let (mut store, plugin) = self.instantiate(bytes, ReadOnly(services), grants)?;
        let json = plugin
            .call_context(&mut store, &serde_json::to_string(&input)?)?
            .map_err(anyhow::Error::msg)?;
        if json.len() > MAX_MESSAGE_BYTES {
            bail!("Plugin context exceeds its limit");
        }
        let contribution: sdk::ContextContribution = serde_json::from_str(&json)?;
        if contribution
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.len() > input.budget_bytes)
        {
            bail!("Plugin context exceeds its budget");
        }
        let tools = self.tools(bytes, NoServices, &[])?;
        let mut disabled = std::collections::BTreeSet::new();
        if contribution
            .disabled_tools
            .iter()
            .any(|name| !disabled.insert(name) || !tools.iter().any(|tool| tool.name == *name))
        {
            bail!("Plugin context can disable only its own declared tools");
        }
        Ok(contribution)
    }
    pub fn event<H: HostServices + 'static>(
        &self,
        bytes: &[u8],
        services: H,
        grants: &[String],
        input: sdk::EventInput,
    ) -> Result<Outcome> {
        openwebide_core::plugins::execution::validate_event(&input.name, &input.payload)
            .map_err(anyhow::Error::msg)?;
        if !self.events(bytes)?.contains(&input.name) {
            bail!("Event is not declared by this plugin");
        }
        let (mut store, plugin) = self.instantiate(bytes, services, grants)?;
        let json = plugin
            .call_event(&mut store, &serde_json::to_string(&input)?)?
            .map_err(anyhow::Error::msg)?;
        if json.len() > MAX_MESSAGE_BYTES {
            bail!("Plugin event outcome exceeds its limit");
        }
        Ok(serde_json::from_str(&json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Records;
    impl HostServices for Records {
        fn request(&mut self, capability: &str, payload: &str) -> Result<String, String> {
            assert_eq!(capability, "records");
            Ok(payload.into())
        }
    }
    fn fixture() -> Vec<u8> {
        static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        BYTES
            .get_or_init(|| {
                if let Ok(path) = std::env::var("OPENWEBIDE_PLUGIN_FIXTURE") {
                    return std::fs::read(path).expect("compiled fixture");
                }
                let root = tempfile::tempdir().unwrap();
                let source = root.path().join("source");
                let stage = root.path().join("build");
                std::fs::create_dir_all(source.join("src")).unwrap();
                std::fs::create_dir_all(&stage).unwrap();
                let manifest = include_str!("../../plugin-sdk/examples/fixture/Cargo.toml")
                    .replace("{ path = \"../..\" }", "\"=0.1.0\"");
                std::fs::write(source.join("Cargo.toml"), manifest).unwrap();
                std::fs::write(
                    source.join("Cargo.lock"),
                    include_str!("../../plugin-sdk/examples/fixture/Cargo.lock"),
                )
                .unwrap();
                std::fs::write(
                    source.join("src/lib.rs"),
                    include_str!("../../plugin-sdk/examples/fixture/src/lib.rs"),
                )
                .unwrap();
                // Build scripts execute natively: prove isolation separately from WASM.
            let secret = root.path().join("private.txt");
            std::fs::write(&secret, "host-only sentinel").unwrap();
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let script = format!(r#"
                fn main() {{
                    let source = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
                    assert!(std::fs::read({secret:?}).is_err(), "host read escaped build sandbox");
                    assert!(std::fs::write(source.join("mutation.txt"), "bad").is_err(), "immutable source was writable");
                    assert!(std::net::TcpStream::connect_timeout(&"{address}".parse().unwrap(), std::time::Duration::from_secs(1)).is_err(), "build network was available");
                }}
            "#);
            std::fs::write(source.join("build.rs"), script).unwrap();
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::fs::PermissionsExt;
                for file in ["Cargo.toml", "Cargo.lock", "src/lib.rs", "build.rs"] {
                    std::fs::set_permissions(source.join(file), std::fs::Permissions::from_mode(0o600)).unwrap();
                }
                for directory in [&source, &source.join("src")] {
                    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
                }
            }
            let bytes = build::compile(&source, "sdk_fixture", &stage)
                    .unwrap_or_else(|error| panic!("isolated source compilation: {error:#}"));
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(source.join("Cargo.toml")).unwrap().permissions().mode() & 0o777, 0o600);
                assert_eq!(std::fs::metadata(&source).unwrap().permissions().mode() & 0o777, 0o700);
            }
            bytes
            })
            .clone()
    }
    #[test]
    fn public_sdk_component_executes_with_grants_and_rejects_ungranted_access() {
        let runtime = Runtime::new().unwrap();
        let bytes = fixture();
        let tools = runtime.tools(&bytes, Records, &[]).unwrap();
        assert_eq!(tools[0].name, "fixture_echo");
        assert!(!tools[0].requires_approval);
        let denied = runtime.execute(&bytes, Records, &[], "fixture_echo", "{}");
        assert!(denied.unwrap_err().to_string().contains("not granted"));
        let result = runtime
            .execute(
                &bytes,
                Records,
                &["records".into()],
                "fixture_echo",
                "{\"value\":42}",
            )
            .unwrap();
        assert_eq!(result.content, "{\"value\":42}");
    }
    #[test]
    fn bounded_large_transfers_fit_the_invocation_fuel_budget() {
        let runtime = Runtime::new().unwrap();
        // "eHh4" decodes to "xxx"; exercise the full 2 MiB HTTP body limit.
        let arguments = serde_json::json!({"encoded":"eHh4".repeat(699_050)}).to_string();
        let outcome = runtime
            .execute(&fixture(), Records, &[], "fixture_large_input", &arguments)
            .unwrap();
        assert!(outcome.ok);
        assert_eq!(outcome.content, "x".repeat(16_384));
    }
    #[test]
    fn context_reads_require_grants_and_cannot_write_or_disable_other_tools() {
        let runtime = Runtime::new().unwrap();
        let bytes = fixture();
        assert!(
            runtime
                .context(&bytes, Records, &[], sdk::ContextInput { budget_bytes: 64 })
                .is_err()
        );
        let grants = ["records".into()];
        let context = runtime
            .context(
                &bytes,
                Records,
                &grants,
                sdk::ContextInput { budget_bytes: 64 },
            )
            .unwrap();
        assert_eq!(context.prompt.as_deref(), Some("Stored fact"));
        assert_eq!(context.disabled_tools, vec!["fixture_echo"]);
        for budget_bytes in [1, 2, 3] {
            assert!(
                runtime
                    .context(&bytes, Records, &grants, sdk::ContextInput { budget_bytes })
                    .is_err()
            );
        }
        assert!(
            runtime
                .context(
                    &bytes,
                    Records,
                    &["records".into(), "completion".into()],
                    sdk::ContextInput { budget_bytes: 4 }
                )
                .is_err()
        );
    }
    #[test]
    fn public_sdk_completion_uses_only_the_declared_general_capability() {
        struct Model;
        impl HostServices for Model {
            fn request(&mut self, capability: &str, payload: &str) -> Result<String, String> {
                assert_eq!(capability, "completion");
                let request: openwebide_core::plugins::completion::CompletionRequest =
                    serde_json::from_str(payload).unwrap();
                request.validate().unwrap();
                assert_eq!(request.prompt, "Fact");
                Ok(serde_json::json!({"text":"Fact title"}).to_string())
            }
        }
        let runtime = Runtime::new().unwrap();
        let bytes = fixture();
        let input=serde_json::json!({"system_prompt":"Plugin policy","prompt":"Fact","profile":"fast","max_output_tokens":128}).to_string();
        assert!(
            runtime
                .execute(&bytes, Model, &[], "fixture_completion", &input)
                .is_err()
        );
        assert_eq!(
            runtime
                .execute(
                    &bytes,
                    Model,
                    &["completion".into()],
                    "fixture_completion",
                    &input
                )
                .unwrap()
                .content,
            "Fact title"
        );
    }
    #[test]
    fn host_events_execute_plugin_owned_behavior_with_the_same_capability_grants() {
        let runtime = Runtime::new().unwrap();
        let bytes = fixture();
        let input = sdk::EventInput {
            name: "job_due".into(),
            payload: serde_json::json!({"job_id":7,"due_at":100}),
        };
        assert!(runtime.event(&bytes, Records, &[], input.clone()).is_err());
        let result = runtime
            .event(&bytes, Records, &["records".into()], input)
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert!(result.ok);
        assert_eq!(value["operation"]["value"]["event"], "job_due");
        assert_eq!(value["operation"]["value"]["payload"]["job_id"], 7);
        for input in [
            sdk::EventInput {
                name: "undeclared_event".into(),
                payload: serde_json::Value::Null,
            },
            sdk::EventInput {
                name: "../invalid".into(),
                payload: serde_json::Value::Null,
            },
            sdk::EventInput {
                name: "job_due".into(),
                payload: serde_json::json!("x".repeat(256 * 1024)),
            },
        ] {
            assert!(
                runtime
                    .event(&bytes, Records, &["records".into()], input)
                    .is_err()
            );
        }
    }
    #[test]
    fn traps_and_exhausted_fuel_do_not_poison_subsequent_calls() {
        let runtime = Runtime::new().unwrap();
        let bytes = fixture();
        for name in ["fixture_panic", "fixture_loop"] {
            assert!(runtime.execute(&bytes, Records, &[], name, "{}").is_err());
        }
        assert!(
            runtime
                .execute(&bytes, Records, &["records".into()], "fixture_echo", "{}")
                .unwrap()
                .ok
        );
        assert!(runtime.validate(b"not a component").is_err());
        assert!(
            runtime
                .tools(&bytes, Records, &["built_in_web".into()])
                .is_err()
        );
    }
}
