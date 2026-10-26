//! Run-scoped tool interface for the shared plugin management facade.
use super::management::{InstallationReview, ManagementHost, plan_installation, search_plugins};
use crate::{ToolExecutor, ToolOutcome, ToolPreview};
use openwebide_core::{
    ToolCall, ToolDefinition,
    plugins::{
        PluginUpdatePolicy, ProjectPluginCommand, RecordPlugin, RemovePlugin,
        management::{CatalogSelection, ManagementRequest, PluginIdentity},
        marketplace::{MarketplaceSource, SaveMarketplaces},
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Mutex;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    #[serde(default)]
    query: String,
    #[serde(default)]
    installed_only: bool,
    #[serde(default = "search_limit")]
    limit: usize,
}
const fn search_limit() -> usize {
    20
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatePackage {
    directory: String,
    files: Vec<openwebide_core::plugins::authoring::DraftFile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatePackage {
    directory: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inspect {
    identity: PluginIdentity,
    source: Option<openwebide_core::plugins::PluginSource>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Change {
    Install {
        source: openwebide_core::plugins::PluginSource,
        selection: Option<CatalogSelection>,
    },
    Update {
        identity: PluginIdentity,
        revision: i64,
        selection: CatalogSelection,
    },
    Rollback {
        identity: PluginIdentity,
        revision: i64,
        source: openwebide_core::plugins::PluginSource,
    },
    Enable {
        identity: PluginIdentity,
        revision: i64,
        project_revision: Option<i64>,
    },
    Disable {
        id: i64,
        revision: i64,
    },
    Remove {
        identity: PluginIdentity,
        revision: i64,
    },
    Configure {
        identity: PluginIdentity,
        revision: i64,
        update_policy: PluginUpdatePolicy,
    },
    Marketplaces {
        revision: i64,
        sources: Vec<MarketplaceSource>,
    },
}

fn parse<T: serde::de::DeserializeOwned>(call: &ToolCall) -> Result<T, String> {
    if call.arguments.len()
        > if call.name == "plugin_create" {
            openwebide_core::plugins::MAX_PACKAGE_BYTES
        } else {
            64 * 1024
        }
    {
        return Err("Plugin tool arguments exceed their content limit.".into());
    }
    serde_json::from_str(&call.arguments)
        .map_err(|error| format!("Invalid plugin tool arguments: {error}"))
}
fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "plugin_search" | "plugin_inspect" | "plugin_manage" | "plugin_create" | "plugin_validate"
    )
}

pub fn definitions() -> Vec<ToolDefinition> {
    let identity = json!({"type":"object","properties": {
        "repository":{"type":"string"},"path":{"type":"string"},
        "publisher":{"type":"string"},"name":{"type":"string"}
    },"required":["repository","path","publisher","name"],"additionalProperties":false});
    let source = json!({"type":"object","properties":{
        "repository":{"type":"string"},"path":{"type":"string"},"commit":{"type":"string"}
    },"required":["repository","path","commit"],"additionalProperties":false});
    let marketplace = json!({"type":"object","properties":{
        "repository":{"type":"string"},"reference":{"type":"string"},"path":{"type":"string"}
    },"required":["repository","reference","path"],"additionalProperties":false});
    let mut variants = Vec::new();
    for (action, properties, required) in [
        (
            "install",
            json!({"source":source,"selection":{"type":"object","properties":{
            "marketplace":marketplace,"publisher":{"type":"string"},"name":{"type":"string"},"version":{"type":"string"}
        },"required":["marketplace","publisher","name","version"],"additionalProperties":false}}),
            vec!["source"],
        ),
        (
            "enable",
            json!({"identity":identity,"revision":{"type":"integer"},"project_revision":{"type":["integer","null"]}}),
            vec!["identity", "revision"],
        ),
        (
            "disable",
            json!({"id":{"type":"integer"},"revision":{"type":"integer"}}),
            vec!["id", "revision"],
        ),
        (
            "remove",
            json!({"identity":identity,"revision":{"type":"integer"}}),
            vec!["identity", "revision"],
        ),
        (
            "configure",
            json!({"identity":identity,"revision":{"type":"integer"},"update_policy":{"enum":["notify","automatic","off"]}}),
            vec!["identity", "revision", "update_policy"],
        ),
        (
            "marketplaces",
            json!({"revision":{"type":"integer"},"sources":{"type":"array","maxItems":16,"items":marketplace}}),
            vec!["revision", "sources"],
        ),
    ] {
        let mut properties = properties.as_object().expect("schema properties").clone();
        properties.insert("action".into(), json!({"const":action}));
        let mut required = required;
        required.insert(0, "action");
        variants.push(json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}));
    }
    let selection = variants[0]["properties"]["selection"].clone();
    for (action, field, schema) in [
        ("update", "selection", selection),
        ("rollback", "source", source.clone()),
    ] {
        let mut properties =
            json!({"action":{"const":action},"identity":identity,"revision":{"type":"integer"}});
        properties
            .as_object_mut()
            .expect("schema properties")
            .insert(field.into(), schema);
        variants.push(json!({"type":"object","properties":properties,"required":["action","identity","revision",field],"additionalProperties":false}));
    }
    vec![
        ToolDefinition { name:"plugin_search".into(), description:"Search cached plugin catalogs and installed plugins. Works offline; returns source identities and available versions.".into(), parameters:json!({"type":"object","properties":{
            "query":{"type":"string","maxLength":512},"installed_only":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":50}
        },"additionalProperties":false}) },
        ToolDefinition { name:"plugin_inspect".into(), description:"Inspect a catalog or installed plugin and its project enablement. Optionally supply a pinned release source to prepare and inspect its actual manifest on the host without activating it. Get identities and sources from plugin_search.".into(), parameters:json!({"type":"object","properties":{"identity":identity,"source":source},"required":["identity"],"additionalProperties":false}) },
        ToolDefinition { name:"plugin_create".into(), description:"Create a validated plugin package in a new or empty project directory after approval. Supply relative file paths and UTF-8 contents including plugin.json and its declared resources. Rust packages need Cargo.toml, Cargo.lock and SDK source. Existing files are preserved; creation does not install or execute the plugin.".into(), parameters:json!({"type":"object","properties":{
            "directory":{"type":"string"},"files":{"type":"array","maxItems":2048,"items":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}}
        },"required":["directory","files"],"additionalProperties":false}) },
        ToolDefinition { name:"plugin_validate".into(), description:"Validate a project plugin directory: manifest, declared files, skill imports, safe paths and content limits. Returns manifest and digest. This checks source structure; Rust compilation and SDK export matching occur on host installation.".into(), parameters:json!({"type":"object","properties":{"directory":{"type":"string"}},"required":["directory"],"additionalProperties":false}) },
        ToolDefinition { name:"plugin_manage".into(), description:"Manage host plugins after approval. Install pinned commits (including updates or older releases), enable/disable for this project, remove, configure update policy, or replace marketplace sources. Changes affect subsequent runs; installations are enabled by default. Inspect current revisions before changes. Official marketplace must remain.".into(), parameters:json!({"type":"object","oneOf":variants}) },
    ]
}

/// Prepared installation previews are owned by this run, keyed by exact call arguments.
pub struct ManagementTools<E, H> {
    pub executor: E,
    pub host: H,
    reviews: Mutex<std::collections::BTreeMap<String, (String, InstallationReview)>>,
}
impl<E, H> ManagementTools<E, H> {
    pub fn new(executor: E, host: H) -> Self {
        Self {
            executor,
            host,
            reviews: Mutex::new(Default::default()),
        }
    }
}
impl<E: ToolExecutor + Sync, H: ManagementHost> ManagementTools<E, H> {
    async fn perform(&self, call: &ToolCall) -> Result<Value, String> {
        if !self.host.current() {
            return Err("Plugin management context changed.".into());
        }
        match call.name.as_str() {
            "plugin_search" => {
                let args: Search = parse(call)?;
                Ok(json!(
                    search_plugins(&self.host, &args.query, args.installed_only, args.limit)
                        .await?
                ))
            }
            "plugin_inspect" => {
                let args: Inspect = parse(call)?;
                let snapshot = self.host.metadata(ManagementRequest::Snapshot).await?;
                let listing = openwebide_core::plugins::management::inspect_listing(
                    &snapshot.marketplaces,
                    &snapshot.installations,
                    &args.identity,
                )
                .map_err(|error| error.to_string())?;
                let prepared = if let Some(source) = args.source {
                    source.validate().map_err(|error| error.to_string())?;
                    let prepared = self.host.prepare(&source).await?;
                    prepared.validate().map_err(|error| error.to_string())?;
                    if prepared.source != source || !args.identity.matches(&prepared) {
                        return Err("Plugin host returned a different source or identity.".into());
                    }
                    Some(prepared)
                } else {
                    listing
                        .as_ref()
                        .and_then(|listing| listing.installed.as_ref())
                        .map(|entry| entry.prepared.clone())
                };
                if listing.is_none() && prepared.is_none() {
                    return Err("Plugin is not in the retained catalogs or installed list. Supply its pinned source to inspect it.".into());
                }
                Ok(
                    json!({"listing":listing,"prepared":prepared,"project":snapshot.bindings.iter().find(|binding|args.identity.matches(&binding.prepared))}),
                )
            }
            "plugin_create" => {
                let args: CreatePackage = parse(call)?;
                let workspace = self
                    .host
                    .workspace()
                    .ok_or("Open a workspace to author plugins.")?;
                Ok(json!(
                    openwebide_core::plugins::authoring::create_workspace(
                        workspace,
                        &args.directory,
                        args.files,
                        || self.host.current()
                    )
                    .await?
                ))
            }
            "plugin_validate" => {
                let args: ValidatePackage = parse(call)?;
                let workspace = self
                    .host
                    .workspace()
                    .ok_or("Open a workspace to validate plugins.")?;
                Ok(json!(
                    openwebide_core::plugins::authoring::validate_workspace(
                        workspace,
                        &args.directory,
                        || self.host.current()
                    )
                    .await?
                ))
            }
            "plugin_manage" => {
                let change: Change = parse(call)?;
                if matches!(
                    change,
                    Change::Install { .. } | Change::Update { .. } | Change::Rollback { .. }
                ) {
                    let (arguments, review) = self
                        .reviews
                        .lock()
                        .map_err(|_| "Plugin review unavailable")?
                        .remove(&call.id)
                        .ok_or(
                            "An approved installation preview is required; retry this tool call.",
                        )?;
                    if arguments != call.arguments {
                        return Err("Plugin arguments changed after review.".into());
                    }
                    return Ok(management_result(&review.apply(&self.host, true).await?));
                }
                let snapshot = self.host.metadata(ManagementRequest::Snapshot).await?;
                let request = match change {
                    Change::Disable { id, revision } => ManagementRequest::Project {
                        command: ProjectPluginCommand::Disable { id, revision },
                    },
                    Change::Marketplaces { revision, sources } => ManagementRequest::Marketplaces {
                        request: SaveMarketplaces { revision, sources },
                    },
                    Change::Remove { identity, revision } => {
                        let entry = installation(&snapshot, &identity, revision)?;
                        ManagementRequest::Remove {
                            request: RemovePlugin {
                                source: entry.prepared.source.clone(),
                                revision,
                            },
                        }
                    }
                    Change::Configure {
                        identity,
                        revision,
                        update_policy,
                    } => {
                        let entry = installation(&snapshot, &identity, revision)?;
                        ManagementRequest::Record {
                            request: Box::new(RecordPlugin {
                                prepared: entry.prepared.clone(),
                                revision: Some(revision),
                                update_policy: Some(update_policy),
                                approved_capabilities: Vec::new(),
                                package: None,
                            }),
                        }
                    }
                    Change::Enable {
                        identity,
                        revision,
                        project_revision,
                    } => {
                        return Ok(management_result(
                            &super::management::enable_plugin(
                                &self.host,
                                &identity,
                                revision,
                                project_revision,
                            )
                            .await?,
                        ));
                    }
                    Change::Install { .. } | Change::Update { .. } | Change::Rollback { .. } => {
                        unreachable!()
                    }
                };
                if !self.host.current() {
                    return Err("Plugin management context changed.".into());
                }
                Ok(management_result(&self.host.metadata(request).await?))
            }
            _ => Err("Unknown plugin management tool.".into()),
        }
    }
}
fn management_result(snapshot: &openwebide_core::plugins::management::ManagementSnapshot) -> Value {
    json!({
        "project_id": snapshot.project_id,
        "installations": snapshot.installations.iter().map(|entry| json!({
            "source":entry.prepared.source,"publisher":entry.prepared.manifest.publisher,
            "name":entry.prepared.manifest.name,"version":entry.prepared.manifest.version,
            "revision":entry.revision,"default_enabled":entry.default_enabled,"update_policy":entry.update_policy,
        })).collect::<Vec<_>>(),
        "project_plugins":snapshot.bindings.iter().map(|entry|json!({
            "id":entry.id,"revision":entry.revision,"publisher":entry.prepared.manifest.publisher,
            "name":entry.prepared.manifest.name,"version":entry.prepared.manifest.version,"enabled":entry.enabled,
        })).collect::<Vec<_>>(),
        "marketplace_revision":snapshot.marketplaces.revision,"sources":snapshot.marketplaces.sources,
    })
}

async fn review_change(
    host: &impl ManagementHost,
    change: Change,
) -> Result<InstallationReview, String> {
    let (source, selection, expected) = match change {
        Change::Install { source, selection } => (source, selection, None),
        Change::Update {
            identity,
            revision,
            selection,
        } => {
            let snapshot = host.metadata(ManagementRequest::Snapshot).await?;
            installation(&snapshot, &identity, revision)?;
            let source = selection
                .resolve(&snapshot.marketplaces)
                .map_err(|error| error.to_string())?;
            (source, Some(selection), Some((identity, revision)))
        }
        Change::Rollback {
            identity,
            revision,
            source,
        } => (source, None, Some((identity, revision))),
        _ => return Err("Expected an installation, update or rollback.".into()),
    };
    if let Some((identity, _)) = &expected
        && (source.repository != identity.repository || source.path != identity.path)
    {
        return Err(
            "Updates and rollbacks must retain the installed plugin source identity.".into(),
        );
    }
    let review = plan_installation(host, source, selection).await?;
    if let Some((identity, revision)) = expected
        && (!identity.matches(review.prepared()) || review.revision() != Some(revision))
    {
        return Err("Plugin identity or revision changed; inspect before retrying.".into());
    }
    Ok(review)
}
fn installation<'a>(
    snapshot: &'a openwebide_core::plugins::management::ManagementSnapshot,
    identity: &PluginIdentity,
    revision: i64,
) -> Result<&'a openwebide_core::plugins::PluginInstallation, String> {
    let entry = snapshot
        .installations
        .iter()
        .find(|entry| identity.matches(&entry.prepared))
        .ok_or("Plugin is not installed.")?;
    if entry.revision != revision {
        return Err("Plugin changed; inspect its current revision before retrying.".into());
    }
    Ok(entry)
}

impl<E: ToolExecutor + Sync, H: ManagementHost> ToolExecutor for ManagementTools<E, H> {
    fn reserve_tool_call(&self) -> Result<(), String> {
        self.executor.reserve_tool_call()
    }
    fn has_context(&self) -> bool {
        self.executor.has_context()
    }
    async fn context(
        &mut self,
        tools: &[ToolDefinition],
        call: Option<&ToolCall>,
    ) -> Option<String> {
        self.executor.context(tools, call).await
    }
    fn describe(&self, call: &ToolCall) -> String {
        if is_tool(&call.name) {
            format!("Plugins: {}", call.name)
        } else {
            self.executor.describe(call)
        }
    }
    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        if !is_tool(&call.name) {
            return self.executor.preview(call).await;
        }
        if call.name == "plugin_manage"
            && let Ok(mut reviews) = self.reviews.lock()
        {
            reviews.remove(&call.id);
        }
        let result = async {
            if call.name == "plugin_create" {
                let args: CreatePackage = parse(call)?;
                openwebide_core::plugins::package_path(&args.directory).map_err(|error|error.to_string())?;
                let package = openwebide_core::plugins::authoring::validate_draft(&args.files)?;
                return Ok(format!("Create {}/{} {} in {}\n{} files; existing files are preserved.\nSource validation passed; this does not install or execute the plugin.",package.manifest.publisher,package.manifest.name,package.manifest.version,args.directory,package.files));
            }
            if call.name == "plugin_manage" {
                let change:Change = parse(call)?;
                if matches!(change, Change::Install { .. } | Change::Update { .. } | Change::Rollback { .. }) {
                    let review = review_change(&self.host, change).await?;
                    let note = format!("Install {}/{} {}\nSource: {} @ {} ({})\nCapabilities: {:?}\nAdded capabilities: {:?}\nEnabled by default; new runs use this version.",review.prepared().manifest.publisher,review.prepared().manifest.name,review.prepared().manifest.version,review.prepared().source.repository,review.prepared().source.commit,review.prepared().source.path,review.prepared().manifest.executable.as_ref().map(|exe| &exe.capabilities),review.additions());
                    let mut reviews = self.reviews.lock().map_err(|_| "Plugin review unavailable")?;
                    if reviews.len() >= 8 { reviews.clear(); }
                    reviews.insert(call.id.clone(),(call.arguments.clone(),review));
                    return Ok(note);
                }
            }
            Ok(format!("Plugin change:\n{}",call.arguments))
        }.await;
        Some(ToolPreview {
            diff: None,
            note: Some(
                result
                    .unwrap_or_else(|error: String| format!("Plugin preparation failed: {error}")),
            ),
        })
    }
    async fn checkpoint(
        &self,
        call: &ToolCall,
    ) -> Result<Option<openwebide_core::FileDiff>, String> {
        if is_tool(&call.name) {
            Ok(None)
        } else {
            self.executor.checkpoint(call).await
        }
    }
    async fn project_checkpoint(
        &self,
        call: &ToolCall,
    ) -> Result<Option<openwebide_core::rewind::ProjectSnapshot>, String> {
        if is_tool(&call.name) {
            Ok(None)
        } else {
            self.executor.project_checkpoint(call).await
        }
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutcome {
        if !is_tool(&call.name) {
            return self.executor.execute(call).await;
        }
        let result = self.perform(call).await;
        match result {
            Ok(value) if self.host.current() => ToolOutcome {
                ok: true,
                content: value.to_string(),
                summary: "Plugin management completed".into(),
                diff: None,
            },
            Ok(_) => ToolOutcome {
                ok: false,
                content: "Plugin management context changed.".into(),
                summary: "Plugin management interrupted".into(),
                diff: None,
            },
            Err(error) => ToolOutcome {
                ok: false,
                content: error,
                summary: "Plugin management failed".into(),
                diff: None,
            },
        }
    }
    fn acknowledge(&self, id: &str) {
        self.executor.acknowledge(id);
    }
    fn set_context_limit(&self, limit: Option<usize>) {
        self.executor.set_context_limit(limit);
    }
}

/// Mutations always show the prepared review, including when automatic approval is enabled.
pub struct ManagementGate<G> {
    pub gate: G,
}
impl<G: crate::PermissionGate> crate::PermissionGate for ManagementGate<G> {
    fn needs_approval(&self, call: &ToolCall) -> bool {
        match call.name.as_str() {
            "plugin_manage" | "plugin_create" => true,
            "plugin_search" | "plugin_inspect" | "plugin_validate" => false,
            _ => self.gate.needs_approval(call),
        }
    }
    fn uses_automatic_approval(&self) -> bool {
        self.gate.uses_automatic_approval()
    }
    fn automatically_approve(
        &self,
        call: &ToolCall,
    ) -> impl std::future::Future<Output = bool> + Send {
        let allowed = !matches!(call.name.as_str(), "plugin_manage" | "plugin_create");
        let decision = self.gate.automatically_approve(call);
        async move { allowed && decision.await }
    }
    fn approve(&self, call: &ToolCall) -> impl std::future::Future<Output = bool> + Send {
        self.gate.approve(call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PermissionGate, VfsToolExecutor};
    use futures::executor::block_on;
    use openwebide_core::{
        MemoryVfs,
        plugins::{management::ManagementSnapshot, marketplace::MarketplaceSettings, testing},
    };

    struct Host {
        requests: Mutex<Vec<ManagementRequest>>,
        workspace: MemoryVfs,
    }
    impl ManagementHost for Host {
        fn workspace(&self) -> Option<&dyn openwebide_core::Vfs> {
            Some(&self.workspace)
        }
        async fn metadata(&self, request: ManagementRequest) -> Result<ManagementSnapshot, String> {
            self.requests.lock().unwrap().push(request);
            Ok(ManagementSnapshot {
                project_id: None,
                installations: Vec::new(),
                marketplaces: MarketplaceSettings::default(),
                bindings: Vec::new(),
            })
        }
        async fn prepare(
            &self,
            _: &openwebide_core::plugins::PluginSource,
        ) -> Result<openwebide_core::plugins::PreparedPlugin, String> {
            Ok(testing::receipt())
        }
        async fn package(
            &self,
            _: &openwebide_core::plugins::PreparedPlugin,
        ) -> Result<openwebide_core::plugins::PluginPackage, String> {
            Ok(testing::package())
        }
        fn current(&self) -> bool {
            true
        }
    }
    fn install() -> ToolCall {
        ToolCall {
            id: "install-1".into(),
            name: "plugin_manage".into(),
            arguments: json!({"action":"install","source":testing::source()}).to_string(),
        }
    }
    #[test]
    fn install_requires_exact_run_owned_preview_and_cannot_reuse_it() {
        block_on(async {
            let tools = ManagementTools::new(
                VfsToolExecutor::new(MemoryVfs::new()),
                Host {
                    requests: Mutex::new(Vec::new()),
                    workspace: MemoryVfs::new(),
                },
            );
            let call = install();
            assert!(!tools.execute(&call).await.ok);
            assert!(tools.host.requests.lock().unwrap().is_empty());
            let preview = tools.preview(&call).await.unwrap();
            assert!(preview.note.unwrap().contains("Enabled by default"));
            assert!(tools.execute(&call).await.ok);
            assert!(!tools.execute(&call).await.ok);
            assert_eq!(tools.host.requests.lock().unwrap().len(), 2);
            let _ = tools.preview(&call).await;
            let mut changed = call.clone();
            changed.arguments.push(' ');
            assert!(!tools.execute(&changed).await.ok);
            assert_eq!(tools.host.requests.lock().unwrap().len(), 3);
            let _ = tools.preview(&call).await;
            let malformed = ToolCall {
                arguments: "null".into(),
                ..call.clone()
            };
            assert!(
                tools
                    .preview(&malformed)
                    .await
                    .unwrap()
                    .note
                    .unwrap()
                    .contains("failed")
            );
            assert!(
                !tools.execute(&call).await.ok,
                "a failed replacement preview must revoke its earlier review"
            );
        });
    }
    #[test]
    fn read_tools_are_ungated_and_mutations_always_require_review() {
        let gate = ManagementGate {
            gate: crate::NoopGate,
        };
        assert!(gate.needs_approval(&install()));
        for name in ["plugin_search", "plugin_inspect"] {
            let call = ToolCall {
                id: "read".into(),
                name: name.into(),
                arguments: "{}".into(),
            };
            assert!(!gate.needs_approval(&call));
        }
        assert!(!block_on(gate.automatically_approve(&install())));
    }
    #[test]
    fn discovery_delegates_workspace_tools_and_rejects_unknown_arguments() {
        block_on(async {
            let tools = ManagementTools::new(
                VfsToolExecutor::new(MemoryVfs::new()),
                Host {
                    requests: Mutex::new(Vec::new()),
                    workspace: MemoryVfs::new(),
                },
            );
            let call = ToolCall {
                id: "search".into(),
                name: "plugin_search".into(),
                arguments: "{}".into(),
            };
            let outcome = tools.execute(&call).await;
            assert!(outcome.ok);
            assert_eq!(outcome.content, "[]");
            let call = ToolCall {
                arguments: "{\"user_id\":2}".into(),
                ..call
            };
            assert!(!tools.execute(&call).await.ok);
            let call = ToolCall {
                id: "files".into(),
                name: "list_dir".into(),
                arguments: "{\"path\":\".\"}".into(),
            };
            assert!(tools.execute(&call).await.ok);
            assert_eq!(definitions().len(), 5);
        });
    }
    #[test]
    fn authoring_tools_validate_before_writing_and_preserve_existing_files() {
        block_on(async {
            let tools = ManagementTools::new(
                VfsToolExecutor::new(MemoryVfs::new()),
                Host {
                    requests: Mutex::new(Vec::new()),
                    workspace: MemoryVfs::new(),
                },
            );
            let files: Vec<_> = testing::review_files()
                .into_iter()
                .map(|file| openwebide_core::plugins::authoring::DraftFile {
                    path: file.path,
                    content: String::from_utf8(file.content).unwrap(),
                })
                .collect();
            let call = ToolCall {
                id: "create".into(),
                name: "plugin_create".into(),
                arguments: json!({"directory":"my-plugin","files":files}).to_string(),
            };
            let gate = ManagementGate {
                gate: crate::NoopGate,
            };
            assert!(gate.needs_approval(&call));
            assert!(
                tools
                    .preview(&call)
                    .await
                    .unwrap()
                    .note
                    .unwrap()
                    .contains("does not install or execute")
            );
            assert!(tools.execute(&call).await.ok);
            assert!(!tools.execute(&call).await.ok);
            let call = ToolCall {
                id: "validate".into(),
                name: "plugin_validate".into(),
                arguments: json!({"directory":"my-plugin"}).to_string(),
            };
            assert!(!gate.needs_approval(&call));
            let result = tools.execute(&call).await;
            assert!(result.ok);
            assert_eq!(
                serde_json::from_str::<Value>(&result.content).unwrap()["compiled"],
                false
            );
            assert!(tools.host.requests.lock().unwrap().is_empty());
        });
    }
}
