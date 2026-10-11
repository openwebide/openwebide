//! Authoring check: compile source in isolation, validate exports, optionally
//! exercise a tool using canned capability responses (never real host services).
use anyhow::{Context, Result, bail};
use openwebide_plugin_runtime::{HostServices, Runtime, build, sdk};
use std::{collections::BTreeMap, path::PathBuf};

struct Fixtures(BTreeMap<String, serde_json::Value>);
impl HostServices for Fixtures {
    fn request(&mut self, capability: &str, _: &str) -> Result<String, String> {
        self.0
            .get(capability)
            .map(serde_json::Value::to_string)
            .ok_or_else(|| format!("No fixture for capability '{capability}'"))
    }
}
fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let source = PathBuf::from(
        arguments
            .next()
            .context("Provide a plugin source directory")?,
    )
    .canonicalize()?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source.join("plugin.json"))?)?;
    let library = manifest["executable"]["library"]
        .as_str()
        .context("Declare an executable Rust library")?;
    let staging = tempfile::tempdir()?;
    let bytes = build::compile(&source, library, staging.path())?;
    let runtime = Runtime::new()?;
    let tools = runtime.tools(&bytes, Fixtures(BTreeMap::new()), &[])?;
    let declared: Vec<sdk::Tool> = serde_json::from_value(
        manifest["contributions"]
            .get("tools")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )?;
    if serde_json::to_value(&tools)? != serde_json::to_value(&declared)? {
        bail!("Exported tool definitions differ from the manifest");
    }
    let events = runtime.events(&bytes)?;
    let declared_events: Vec<String> = serde_json::from_value(
        manifest["contributions"]
            .get("events")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )?;
    if events != declared_events {
        bail!("Exported events differ from the manifest");
    }
    if let Some(tool) = arguments.next() {
        let input = arguments.next().context("Provide JSON tool arguments")?;
        let fixtures = arguments
            .next()
            .context("Provide a capability fixture JSON file")?;
        let services = Fixtures(serde_json::from_slice(&std::fs::read(fixtures)?)?);
        let grants: Vec<String> =
            serde_json::from_value(manifest["executable"]["capabilities"].clone())?;
        if tool == "--event" {
            let input = serde_json::from_str(&input)?;
            let outcome = runtime.event(&bytes, services, &grants, input)?;
            println!("{}", serde_json::to_string(&outcome)?);
            if !outcome.ok {
                bail!("Plugin event reported an unsuccessful result");
            }
            return Ok(());
        }
        if tool == "--context" {
            let input = serde_json::from_str(&input)?;
            let contribution = runtime.context(&bytes, services, &grants, input)?;
            println!("{}", serde_json::to_string(&contribution)?);
            return Ok(());
        }
        if !tools.iter().any(|definition| definition.name == tool) {
            bail!("Tool is not declared by this plugin");
        }
        let outcome = runtime.execute(&bytes, services, &grants, &tool, &input)?;
        println!("{}", serde_json::to_string(&outcome)?);
        if !outcome.ok {
            bail!("Plugin reported an unsuccessful result");
        }
    } else {
        println!(
            "Validated {} exported tools and {} events ({} WASM bytes)",
            tools.len(),
            events.len(),
            bytes.len()
        );
    }
    Ok(())
}
