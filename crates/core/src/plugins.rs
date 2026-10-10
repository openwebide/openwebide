//! Host-only plugin preparation policy, shared by every bridge transport.
pub mod completion;
pub mod execution;
pub mod jobs;
pub mod marketplace;
pub mod preparation;
pub mod records;
pub mod runs;
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::LazyLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::skills::archive::{ImportFile, import_files};

/// The selected core capabilities, pinned independently of marketplace refreshes.
pub fn bundled_plugin_sources() -> Vec<PluginSource> {
    #[derive(Deserialize)]
    struct Selection {
        repository: String,
        commit: String,
        paths: Vec<String>,
    }
    let selection: Selection = serde_json::from_str(include_str!("../../../plugins/bundled.json"))
        .expect("valid bundled plugin selection");
    selection
        .paths
        .into_iter()
        .map(|path| PluginSource {
            repository: selection.repository.clone(),
            commit: selection.commit.clone(),
            path,
        })
        .collect()
}

pub const MAX_PACKAGE_FILES: usize = 2048;
pub const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PACKAGE_FILE_BYTES: usize = 128 * 1024;
const SCHEMA: &str =
    "https://raw.githubusercontent.com/openwebide/plugins/main/schemas/plugin.schema.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSource {
    pub repository: String,
    pub commit: String,
    pub path: String,
}

impl PluginSource {
    pub fn validate(&self) -> Result<(), PluginError> {
        let url = url::Url::parse(&self.repository)
            .map_err(|_| invalid("Use an HTTP, HTTPS, SSH or Git repository URL."))?;
        if self.repository.len() > 2048
            || self.repository.chars().any(char::is_whitespace)
            || !matches!(url.scheme(), "http" | "https" | "ssh" | "git")
            || url.host_str().is_none()
            || url.password().is_some()
            || (url.scheme() != "ssh" && !url.username().is_empty())
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(
                "Repository URLs cannot contain credentials, queries or fragments.",
            ));
        }
        if !hex(&self.commit, &[40, 64]) {
            return Err(invalid(
                "Select a full immutable Git commit ID, not a branch or tag.",
            ));
        }
        if self.path != "." {
            package_path(&self.path)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub publisher: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub description: String,
    pub license: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readme: Option<String>,
    pub compatibility: PluginCompatibility,
    pub contributions: PluginContributions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<RustPlugin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCompatibility {
    #[serde(rename = "pluginApi")]
    pub plugin_api: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginContributions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<PluginBackground>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<PluginTool>,
    #[serde(default, rename = "toolGroups", skip_serializing_if = "Vec::is_empty")]
    pub tool_groups: Vec<PluginToolGroup>,
    #[serde(default)]
    pub skills: Vec<PluginSkill>,
}
/// Periodic, project-scoped reconciliation through the ordinary durable event queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginBackground {
    pub event: String,
    #[serde(rename = "intervalSeconds")]
    pub interval_seconds: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSkill {
    pub path: String,
}

/// API 3 source package. The host provides this pinned SDK and compiler.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustPlugin {
    pub manifest: String,
    pub library: String,
    #[serde(rename = "sdkVersion")]
    pub sdk_version: String,
    pub capabilities: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub requires_approval: bool,
}

impl PluginManifest {
    /// Historical receipts remain readable, but tool-group switches no longer
    /// activate application-owned feature behavior.
    pub fn requires_update(&self) -> bool {
        !self.contributions.tool_groups.is_empty()
    }
    pub fn validate_activation(&self) -> Result<(), PluginError> {
        if self.requires_update() {
            return Err(invalid(
                "Update required: this plugin version uses retired built-in tool groups. Choose a release with executable SDK handlers.",
            ));
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), PluginError> {
        static VERSION: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
            r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
        ).expect("static SemVer pattern")
        });
        if self.schema_version != 1
            || !matches!(self.compatibility.plugin_api, 1..=3)
            || self
                .schema
                .as_deref()
                .is_some_and(|schema| schema != SCHEMA)
        {
            return Err(invalid("This plugin manifest/API version is unsupported."));
        }
        for name in [&self.publisher, &self.name] {
            if name.is_empty()
                || name.len() > 64
                || name.starts_with('-')
                || name.ends_with('-')
                || name.contains("--")
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return Err(invalid(
                    "Plugin identities require lowercase letters, digits and single hyphens.",
                ));
            }
        }
        if self.version.len() > 128
            || self.version.chars().any(char::is_whitespace)
            || !VERSION.is_match(&self.version)
        {
            return Err(invalid("Plugin versions must use SemVer."));
        }
        for (text, limit) in [
            (&self.display_name, 128),
            (&self.description, 1024),
            (&self.license, 128),
        ] {
            if text.trim().is_empty() || text.chars().count() > limit {
                return Err(invalid("Plugin display metadata is empty or too long."));
            }
        }
        if let Some(readme) = &self.readme {
            package_path(readme)?;
        }
        if (self.contributions.skills.is_empty()
            && self.contributions.tool_groups.is_empty()
            && self.contributions.tools.is_empty())
            && self.contributions.events.is_empty()
            || self.contributions.skills.len() > crate::skills::MAX_SKILLS
            || self.contributions.tool_groups.len() > 4
            || self
                .contributions
                .tool_groups
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.contributions.tool_groups.len()
            || (!self.contributions.tool_groups.is_empty() && self.compatibility.plugin_api != 2)
        {
            return Err(invalid(
                "Plugins require skills or unique supported tool groups; tool groups need plugin API 2.",
            ));
        }
        let executable = self.executable.as_ref();
        if self.contributions.events.len() > 100
            || (!self.contributions.events.is_empty() && executable.is_none())
            || self
                .contributions
                .events
                .iter()
                .any(|name| execution::validate_event(name, &serde_json::Value::Null).is_err())
            || self
                .contributions
                .events
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.contributions.events.len()
        {
            return Err(invalid(
                "Host events require a Rust executable and unique supported event names.",
            ));
        }
        if let Some(background) = &self.contributions.background
            && (!(60..=86400).contains(&background.interval_seconds)
                || !self.contributions.events.contains(&background.event)
                || !executable
                    .is_some_and(|rust| rust.capabilities.iter().any(|name| name == "jobs")))
        {
            return Err(invalid(
                "Background events require a declared event, jobs capability and an interval of 60–86,400 seconds.",
            ));
        }
        if executable.is_some() != (self.compatibility.plugin_api == 3)
            || (executable.is_none() && !self.contributions.tools.is_empty())
            || self.contributions.tools.len() > 100
        {
            return Err(invalid(
                "Executable tools require a Rust plugin using API 3.",
            ));
        }
        if let Some(rust) = executable
            && (rust.manifest != "Cargo.toml"
                || rust.sdk_version != "0.1.0"
                || rust.library.is_empty()
                || rust.library.len() > 64
                || !rust
                    .library
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                || rust.capabilities.len() > 8
                || rust.capabilities.iter().any(|name| {
                    ![
                        "http",
                        "records",
                        "collections",
                        "jobs",
                        "runs",
                        "workspace",
                        "clock",
                        "completion",
                    ]
                    .contains(&name.as_str())
                })
                || rust
                    .capabilities
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != rust.capabilities.len()
                || (self.contributions.tools.is_empty() && self.contributions.events.is_empty())
                || !self.contributions.tool_groups.is_empty())
        {
            return Err(invalid(
                "Unsupported Rust SDK, capability or library declaration.",
            ));
        }
        let mut names = std::collections::BTreeSet::new();
        for tool in &self.contributions.tools {
            if tool.name.is_empty()
                || tool.name.len() > 64
                || !tool
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                || !names.insert(&tool.name)
                || tool.description.trim().is_empty()
                || tool.description.len() > 4096
                || tool.parameters.get("type").and_then(|value| value.as_str()) != Some("object")
            {
                return Err(invalid("Invalid executable plugin tool declaration."));
            }
        }
        let mut paths = std::collections::BTreeSet::new();
        for skill in &self.contributions.skills {
            package_path(&skill.path)?;
            if !skill.path.ends_with("/SKILL.md") || !paths.insert(&skill.path) {
                return Err(invalid("Skills require unique directory/SKILL.md paths."));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedPlugin {
    pub source: PluginSource,
    pub manifest: PluginManifest,
    pub digest: String,
    pub host_id: String,
}
impl PreparedPlugin {
    /// Stable across versions, distinct across repositories claiming the same ID.
    pub fn storage_namespace(&self) -> String {
        let mut digest = Sha256::new();
        for field in [
            &self.source.repository,
            &self.source.path,
            &self.manifest.publisher,
            &self.manifest.name,
        ] {
            digest.update(
                u64::try_from(field.len())
                    .expect("bounded field length")
                    .to_be_bytes(),
            );
            digest.update(field.as_bytes());
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
    pub fn validate(&self) -> Result<(), PluginError> {
        self.source.validate()?;
        self.manifest.validate()?;
        if !hex(&self.digest, &[64]) || self.host_id.trim().is_empty() || self.host_id.len() > 1024
        {
            return Err(invalid("Invalid plugin host receipt."));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInstallation {
    pub prepared: PreparedPlugin,
    pub revision: i64,
    pub hosts: Vec<String>,
    pub installed_at: i64,
    #[serde(default)]
    pub default_enabled: bool,
    #[serde(default)]
    pub update_policy: PluginUpdatePolicy,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordPlugin {
    /// Exact additions reviewed for this prepared version; never a blanket grant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_policy: Option<PluginUpdatePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Box<PluginPackage>>,
    pub prepared: PreparedPlugin,
    pub revision: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageFileKind {
    File,
    Executable,
    Symlink,
    Submodule,
}
#[derive(Clone, Debug)]
pub struct PackageFile {
    pub path: String,
    pub content: Vec<u8>,
    pub kind: PackageFileKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginError {
    Invalid(String),
    Conflict(String),
    Host(String),
}
impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Conflict(message) | Self::Host(message) => {
                f.write_str(message)
            }
        }
    }
}
impl std::error::Error for PluginError {}
fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::Invalid(message.into())
}
fn hex(value: &str, lengths: &[usize]) -> bool {
    lengths.contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn package_path(path: &str) -> Result<(), PluginError> {
    if path.is_empty()
        || path.len() > 512
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_@+.-".contains(&b))
    {
        return Err(invalid(
            "Plugin paths must be normalized relative paths inside the package.",
        ));
    }
    Ok(())
}

pub type PluginFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PluginError>> + Send + 'a>>;
/// Adapters supply Git object reads and atomic host filesystem publication only.
pub trait PluginHost: Send + Sync {
    fn files<'a>(&'a self, source: &'a PluginSource) -> PluginFuture<'a, Vec<PackageFile>>;
    fn publish<'a>(
        &'a self,
        source: &'a PluginSource,
        digest: &'a str,
        files: &'a [PackageFile],
    ) -> PluginFuture<'a, ()>;
    fn host_id(&self) -> String;
    fn compile<'a>(
        &'a self,
        _source: &'a PluginSource,
        _digest: &'a str,
        _manifest: &'a PluginManifest,
    ) -> PluginFuture<'a, ()> {
        Box::pin(async {
            Err(PluginError::Host(
                "Executable plugins are not supported on this host.".into(),
            ))
        })
    }
}

/// Fetch, validate, import-check and atomically publish once, in either project mode.
pub async fn prepare_plugin(
    host: &impl PluginHost,
    source: &PluginSource,
) -> Result<PreparedPlugin, PluginError> {
    source.validate()?;
    let host_id = host.host_id();
    if host_id.trim().is_empty() || host_id.len() > 1024 {
        return Err(invalid("Invalid execution host identity."));
    }
    let files = host.files(source).await?;
    let manifest = validate_files(&files)?;
    manifest.validate_activation()?;
    let digest = package_digest(&files);
    host.publish(source, &digest, &files).await?;
    if manifest.executable.is_some() {
        host.compile(source, &digest, &manifest).await?;
    }
    let prepared = PreparedPlugin {
        source: source.clone(),
        manifest,
        digest,
        host_id,
    };
    prepared.validate()?;
    Ok(prepared)
}

pub fn validate_files(files: &[PackageFile]) -> Result<PluginManifest, PluginError> {
    if files.is_empty()
        || files.len() > MAX_PACKAGE_FILES
        || files
            .iter()
            .any(|file| file.content.len() > MAX_PACKAGE_FILE_BYTES)
        || files.iter().map(|file| file.content.len()).sum::<usize>() > MAX_PACKAGE_BYTES
    {
        return Err(invalid("Plugin package exceeds its file/content limits."));
    }
    let mut entries = BTreeMap::new();
    let mut portable_paths = BTreeMap::new();
    for file in files {
        package_path(&file.path)?;
        let mut prefix = String::new();
        for part in file.path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let key = prefix.to_ascii_lowercase();
            if portable_paths
                .insert(key, prefix.clone())
                .is_some_and(|previous| previous != prefix)
            {
                return Err(invalid("Plugin paths cannot differ only by letter case."));
            }
        }
        if !matches!(
            file.kind,
            PackageFileKind::File | PackageFileKind::Executable
        ) || entries.insert(file.path.as_str(), file).is_some()
        {
            return Err(invalid(
                "Plugin packages cannot contain links, submodules or duplicate paths.",
            ));
        }
        // Exclude Git metadata and prevent file/directory collisions on publication.
        if file
            .path
            .split('/')
            .any(|part| part.eq_ignore_ascii_case(".git"))
        {
            return Err(invalid("Plugin packages cannot contain Git metadata."));
        }
    }
    for path in entries.keys() {
        for (offset, _) in path.match_indices('/') {
            if entries
                .keys()
                .any(|entry| entry.eq_ignore_ascii_case(&path[..offset]))
            {
                return Err(invalid("Plugin files cannot also be parent directories."));
            }
        }
    }
    let manifest: PluginManifest = serde_json::from_slice(
        &entries
            .get("plugin.json")
            .ok_or_else(|| invalid("The selected directory has no plugin.json."))?
            .content,
    )
    .map_err(|error| invalid(format!("Invalid plugin.json: {error}")))?;
    manifest.validate()?;
    if manifest
        .readme
        .as_deref()
        .is_some_and(|path| !entries.contains_key(path))
    {
        return Err(invalid("The declared plugin README is missing."));
    }
    let mut names = std::collections::BTreeSet::new();
    for skill in &manifest.contributions.skills {
        let directory = skill
            .path
            .strip_suffix("SKILL.md")
            .expect("validated skill path");
        let skill_files = files
            .iter()
            .filter_map(|file| {
                file.path.strip_prefix(directory).map(|name| ImportFile {
                    name: name.into(),
                    bytes: file.content.clone(),
                })
            })
            .collect();
        let draft = import_files(skill_files).map_err(invalid)?;
        let dirname = directory
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        if draft.name != dirname || !names.insert(draft.name) {
            return Err(invalid(
                "Skill directory names must match unique frontmatter names.",
            ));
        }
    }
    if manifest.executable.is_some()
        && (!entries.contains_key("Cargo.toml") || !entries.contains_key("Cargo.lock"))
    {
        return Err(invalid(
            "Rust plugins require Cargo.toml and a committed Cargo.lock.",
        ));
    }
    Ok(manifest)
}

pub fn package_digest(files: &[PackageFile]) -> String {
    let mut sorted = files.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut digest = Sha256::new();
    for file in sorted {
        digest.update((file.path.len() as u64).to_le_bytes());
        digest.update(file.path.as_bytes());
        digest.update([u8::from(file.kind == PackageFileKind::Executable)]);
        digest.update((file.content.len() as u64).to_le_bytes());
        digest.update(&file.content);
    }
    digest_hex(&digest.finalize())
}

pub fn content_digest(bytes: &[u8]) -> String {
    digest_hex(&Sha256::digest(bytes))
}
fn digest_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").expect("writing to a string");
    }
    result
}

/// Database adapters serialize this result atomically; verified content establishes defaults for project activation.
pub fn record_installation(
    mut current: Vec<PluginInstallation>,
    request: &RecordPlugin,
    now: i64,
) -> Result<Vec<PluginInstallation>, PluginError> {
    request.prepared.validate()?;
    if let Some(package) = &request.package {
        package.validate()?;
        request.prepared.manifest.validate_activation()?;
        if package.prepared != request.prepared {
            return Err(invalid(
                "Plugin content does not match the installation receipt.",
            ));
        }
    }
    let source = &request.prepared.source;
    let existing = current.iter_mut().find(|entry| {
        entry.prepared.source.repository == source.repository
            && entry.prepared.source.path == source.path
    });
    if let Some(existing) = existing {
        let same = existing.prepared.source == *source
            && existing.prepared.digest == request.prepared.digest
            && existing.prepared.manifest == request.prepared.manifest;
        if !same {
            request.prepared.manifest.validate_activation()?;
        }
        if request
            .revision
            .is_some_and(|revision| revision != existing.revision)
            || (!same && request.revision != Some(existing.revision))
            || (request
                .update_policy
                .is_some_and(|policy| policy != existing.update_policy)
                && request.revision != Some(existing.revision))
        {
            return Err(PluginError::Conflict(
                "Plugin installation changed. Refresh before updating.".into(),
            ));
        }
        if existing.prepared.manifest.publisher != request.prepared.manifest.publisher
            || existing.prepared.manifest.name != request.prepared.manifest.name
        {
            return Err(PluginError::Conflict(
                "The package identity at this source changed.".into(),
            ));
        }
        if existing.prepared.source.commit == source.commit && !same {
            return Err(PluginError::Conflict(
                "A pinned plugin version cannot change its contents.".into(),
            ));
        }
        let added = added_capabilities(&existing.prepared, &request.prepared);
        if !added.is_empty() && request.approved_capabilities != added {
            return Err(PluginError::Conflict(format!(
                "Review the plugin's new capabilities before updating: {}.",
                added.join(", ")
            )));
        }
        if !same {
            existing.hosts.clear();
            existing.prepared = request.prepared.clone();
            existing.revision += 1;
            existing.default_enabled = false;
        }
        if request.package.is_some() && !existing.default_enabled {
            existing.default_enabled = true;
            existing.revision += 1;
        }
        if let Some(policy) = request.update_policy
            && policy != existing.update_policy
        {
            existing.update_policy = policy;
            existing.revision += 1;
        }
        if !existing.hosts.contains(&request.prepared.host_id) {
            if existing.hosts.len() >= 100 {
                return Err(invalid("This plugin already has 100 host receipts."));
            }
            existing.hosts.push(request.prepared.host_id.clone());
            existing.revision += 1;
        }
    } else {
        request.prepared.manifest.validate_activation()?;
        if request.revision.is_some() {
            return Err(PluginError::Conflict(
                "This plugin installation no longer exists.".into(),
            ));
        }
        if current.len() >= 100 {
            return Err(invalid("At most 100 plugins can be installed per user."));
        }
        current.push(PluginInstallation {
            prepared: request.prepared.clone(),
            revision: 1,
            hosts: vec![request.prepared.host_id.clone()],
            installed_at: now,
            default_enabled: request.package.is_some(),
            update_policy: request.update_policy.unwrap_or_default(),
        });
    }
    current.sort_by(|a, b| {
        a.prepared
            .manifest
            .display_name
            .cmp(&b.prepared.manifest.display_name)
            .then(
                a.prepared
                    .source
                    .repository
                    .cmp(&b.prepared.source.repository),
            )
            .then(a.prepared.source.path.cmp(&b.prepared.source.path))
    });
    Ok(current)
}

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::*;
    pub fn review_files() -> Vec<PackageFile> {
        [
            (
                "plugin.json",
                include_bytes!("../tests/fixtures/pr-review/plugin.json").as_slice(),
            ),
            (
                "README.md",
                include_bytes!("../tests/fixtures/pr-review/README.md").as_slice(),
            ),
            (
                "skills/review-changes/SKILL.md",
                include_bytes!("../tests/fixtures/pr-review/skills/review-changes/SKILL.md")
                    .as_slice(),
            ),
            (
                "skills/review-changes/references/review-checks.md",
                include_bytes!(
                    "../tests/fixtures/pr-review/skills/review-changes/references/review-checks.md"
                )
                .as_slice(),
            ),
        ]
        .into_iter()
        .map(|(path, bytes)| PackageFile {
            path: path.into(),
            content: bytes.to_vec(),
            kind: PackageFileKind::File,
        })
        .collect()
    }
    pub fn source() -> PluginSource {
        PluginSource {
            repository: "https://git.example.org/plugins.git".into(),
            commit: "a".repeat(40),
            path: "plugins/pr-review".into(),
        }
    }
    pub fn package() -> PluginPackage {
        let prepared = receipt();
        let skills = import_plugin_skills(&review_files(), &prepared.manifest).unwrap();
        PluginPackage { prepared, skills }
    }
    pub fn catalog() -> marketplace::CachedMarketplace {
        let receipt = receipt();
        marketplace::CachedMarketplace {
            source: marketplace::MarketplaceSource {
                repository: receipt.source.repository.clone(),
                reference: String::new(),
                path: "marketplace.json".into(),
            },
            commit: "c".repeat(40),
            fetched_at: 1,
            catalog: marketplace::MarketplaceCatalog {
                schema: None,
                schema_version: 1,
                name: "Test marketplace".into(),
                plugins: vec![marketplace::CatalogPlugin {
                    publisher: receipt.manifest.publisher,
                    name: receipt.manifest.name,
                    display_name: receipt.manifest.display_name,
                    description: receipt.manifest.description,
                    categories: vec!["skills".into()],
                    releases: vec![marketplace::CatalogRelease {
                        version: receipt.manifest.version,
                        source: marketplace::CatalogReleaseSource {
                            commit: receipt.source.commit,
                            path: receipt.source.path,
                        },
                    }],
                }],
            },
        }
    }
    pub fn receipt() -> PreparedPlugin {
        let files = review_files();
        PreparedPlugin {
            source: source(),
            manifest: validate_files(&files).unwrap(),
            digest: package_digest(&files),
            host_id: "host-a".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use testing::*;
    #[test]
    fn storage_namespace_survives_versions_but_separates_source_identity() {
        let original = receipt();
        let mut updated = original.clone();
        updated.manifest.version = "0.2.0".into();
        updated.digest = "c".repeat(64);
        updated.source.commit = "c".repeat(40);
        assert_eq!(original.storage_namespace(), updated.storage_namespace());
        updated.source.repository = "https://example.com/another.git".into();
        assert_ne!(original.storage_namespace(), updated.storage_namespace());
        updated = original.clone();
        updated.source.path = "another-plugin".into();
        assert_ne!(original.storage_namespace(), updated.storage_namespace());
    }

    struct Host {
        id: String,
        files: Vec<PackageFile>,
        fail_fetch: bool,
        fail_publish: bool,
        published: Mutex<Vec<String>>,
    }
    impl PluginHost for Host {
        fn host_id(&self) -> String {
            self.id.clone()
        }
        fn files<'a>(&'a self, _: &'a PluginSource) -> PluginFuture<'a, Vec<PackageFile>> {
            Box::pin(async {
                if self.fail_fetch {
                    Err(PluginError::Host("offline".into()))
                } else {
                    Ok(self.files.clone())
                }
            })
        }
        fn publish<'a>(
            &'a self,
            _: &'a PluginSource,
            digest: &'a str,
            _: &'a [PackageFile],
        ) -> PluginFuture<'a, ()> {
            Box::pin(async move {
                if self.fail_publish {
                    return Err(PluginError::Host("disk full".into()));
                }
                self.published.lock().unwrap().push(digest.into());
                Ok(())
            })
        }
    }

    #[test]
    fn retired_tool_groups_remain_readable_but_cannot_be_prepared_or_reactivated() {
        futures::executor::block_on(async {
            let mut manifest = receipt().manifest;
            manifest.compatibility.plugin_api = 2;
            manifest.contributions.tool_groups = vec![PluginToolGroup::Memory];
            manifest.validate().unwrap();
            assert!(manifest.requires_update());
            let mut files = review_files();
            files
                .iter_mut()
                .find(|file| file.path == "plugin.json")
                .unwrap()
                .content = serde_json::to_vec(&manifest).unwrap();
            for id in ["server", "paired"] {
                let host = Host {
                    id: id.into(),
                    files: files.clone(),
                    fail_fetch: false,
                    fail_publish: false,
                    published: Mutex::default(),
                };
                let error = prepare_plugin(&host, &source()).await.unwrap_err();
                assert!(error.to_string().contains("Update required"));
                assert!(host.published.lock().unwrap().is_empty());
            }
            let prepared = PreparedPlugin {
                manifest,
                digest: package_digest(&files),
                ..receipt()
            };
            prepared.validate().unwrap();
            let installation = PluginInstallation {
                prepared: prepared.clone(),
                revision: 3,
                hosts: vec![prepared.host_id.clone()],
                installed_at: 1,
                default_enabled: true,
                update_policy: PluginUpdatePolicy::Notify,
            };
            let request = RecordPlugin {
                prepared: prepared.clone(),
                revision: Some(3),
                package: None,
                update_policy: Some(PluginUpdatePolicy::Off),
                approved_capabilities: Vec::new(),
            };
            let updated = record_installation(vec![installation], &request, 2).unwrap();
            assert_eq!(updated[0].prepared, prepared);
            assert_eq!(updated[0].update_policy, PluginUpdatePolicy::Off);
            assert!(updated[0].default_enabled);
            assert_eq!(updated[0].installed_at, 1);
            assert!(
                record_installation(
                    Vec::new(),
                    &RecordPlugin {
                        revision: None,
                        ..request.clone()
                    },
                    2
                )
                .is_err()
            );
            let package = PluginPackage {
                prepared: prepared.clone(),
                skills: import_plugin_skills(&files, &prepared.manifest).unwrap(),
            };
            let activation = RecordPlugin {
                revision: Some(updated[0].revision),
                package: Some(Box::new(package)),
                ..request
            };
            assert!(record_installation(updated, &activation, 2).is_err());
        });
    }

    #[test]
    fn background_events_require_declared_jobs_authority_and_bounded_intervals() {
        let mut manifest = testing::receipt().manifest;
        manifest.compatibility.plugin_api = 3;
        manifest.contributions.skills.clear();
        manifest.contributions.events = vec!["repair".into()];
        manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "background".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["jobs".into()],
        });
        manifest.contributions.background = Some(PluginBackground {
            event: "repair".into(),
            interval_seconds: 60,
        });
        manifest.validate().unwrap();
        for interval in [0, 59, 86401, i64::MAX] {
            manifest
                .contributions
                .background
                .as_mut()
                .unwrap()
                .interval_seconds = interval;
            assert!(manifest.validate().is_err());
        }
        manifest
            .contributions
            .background
            .as_mut()
            .unwrap()
            .interval_seconds = 86400;
        manifest.validate().unwrap();
        manifest.contributions.background.as_mut().unwrap().event = "undeclared".into();
        assert!(manifest.validate().is_err());
        manifest.contributions.background.as_mut().unwrap().event = "repair".into();
        manifest.executable.as_mut().unwrap().capabilities.clear();
        assert!(manifest.validate().is_err());
    }
    #[test]
    fn executable_event_contributions_do_not_require_model_tools() {
        let mut manifest = testing::receipt().manifest;
        manifest.compatibility.plugin_api = 3;
        manifest.contributions.skills.clear();
        manifest.contributions.tools.clear();
        manifest.contributions.events = vec!["job_due".into()];
        manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "event_plugin".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["records".into()],
        });
        manifest.validate().unwrap();
        manifest.contributions.events.push("job_due".into());
        assert!(manifest.validate().is_err());
        manifest.contributions.events = vec!["../invalid".into()];
        assert!(manifest.validate().is_err());
        manifest.contributions.events = vec!["job_due".into()];
        manifest.executable = None;
        manifest.compatibility.plugin_api = 2;
        assert!(manifest.validate().is_err());
    }
    #[test]
    fn rust_plugin_contract_requires_pinned_sdk_lockfile_and_executable_tools() {
        let mut value = serde_json::to_value(receipt().manifest).unwrap();
        value["compatibility"]["pluginApi"] = serde_json::json!(3);
        value.as_object_mut().unwrap().remove("readme");
        value["contributions"] = serde_json::json!({"skills":[],"tools":[{
            "name":"example_tool", "description":"An executable tool",
            "parameters":{"type":"object"},"requires_approval":true
        }]});
        value["executable"] = serde_json::json!({"manifest":"Cargo.toml","library":"example_plugin","sdkVersion":"0.1.0","capabilities":["records"]});
        let manifest: PluginManifest = serde_json::from_value(value.clone()).unwrap();
        manifest.validate().unwrap();
        let mut files = vec![PackageFile {
            path: "plugin.json".into(),
            content: serde_json::to_vec(&manifest).unwrap(),
            kind: PackageFileKind::File,
        }];
        assert!(validate_files(&files).is_err());
        for path in ["Cargo.toml", "Cargo.lock"] {
            files.push(PackageFile {
                path: path.into(),
                content: b"placeholder".to_vec(),
                kind: PackageFileKind::File,
            });
        }
        validate_files(&files).unwrap();
        for (field, invalid_value) in [
            ("sdkVersion", "9.0.0"),
            ("library", "../escape"),
            ("manifest", "../Cargo.toml"),
        ] {
            let mut invalid = value.clone();
            invalid["executable"][field] = serde_json::json!(invalid_value);
            assert!(
                serde_json::from_value::<PluginManifest>(invalid)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        let mut invalid = value;
        invalid["executable"]["capabilities"] = serde_json::json!(["built_in_web"]);
        assert!(
            serde_json::from_value::<PluginManifest>(invalid)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn local_and_remote_hosts_use_same_prepare_contract_and_failures() {
        futures::executor::block_on(async {
            for id in ["paired-local", "server-remote"] {
                let mut host = Host {
                    id: id.into(),
                    files: review_files(),
                    fail_fetch: false,
                    fail_publish: false,
                    published: Mutex::new(Vec::new()),
                };
                let result = prepare_plugin(&host, &source()).await.unwrap();
                assert_eq!(result.host_id, id);
                assert_eq!(result.manifest.name, "pr-review");
                assert_eq!(host.published.lock().unwrap().len(), 1);
                host.fail_fetch = true;
                assert!(matches!(
                    prepare_plugin(&host, &source()).await,
                    Err(PluginError::Host(_))
                ));
                host.fail_fetch = false;
                host.fail_publish = true;
                assert!(matches!(
                    prepare_plugin(&host, &source()).await,
                    Err(PluginError::Host(_))
                ));
                host.fail_publish = false;
                host.files[0].content = b"{}".to_vec();
                assert!(matches!(
                    prepare_plugin(&host, &source()).await,
                    Err(PluginError::Invalid(_))
                ));
                assert_eq!(host.published.lock().unwrap().len(), 1);
            }
        });
    }
    #[test]
    fn reference_package_matches_authoring_format_and_resource_importer() {
        assert_eq!(
            validate_files(&review_files())
                .unwrap()
                .contributions
                .skills
                .len(),
            1
        );
        let mut files = review_files();
        files.reverse();
        assert_eq!(package_digest(&files), package_digest(&review_files()));
        files[0].content.push(b'!');
        assert_ne!(package_digest(&files), package_digest(&review_files()));
    }
    #[test]
    fn reject_unpinned_sources_credentials_and_path_escape() {
        for path in ["../outside", "/tmp/plugin", "a/../b", "a\\b", "a//b"] {
            let mut input = source();
            input.path = path.into();
            assert!(input.validate().is_err());
        }
        for repository in [
            "file:///tmp/repo",
            "https://token@example.org/repo",
            "https://example.org/repo?token=x",
            "ext::sh command",
        ] {
            let mut input = source();
            input.repository = repository.into();
            assert!(input.validate().is_err());
        }
        let mut input = source();
        input.commit = "main".into();
        assert!(input.validate().is_err());
    }
    #[test]
    fn reject_case_collisions_in_files_and_parent_directories() {
        for path in ["readme.md", "Skills/another/file.md", "README.md/child"] {
            let mut files = review_files();
            files.push(PackageFile {
                path: path.into(),
                content: Vec::new(),
                kind: PackageFileKind::File,
            });
            assert!(validate_files(&files).is_err(), "{path}");
        }
    }
    #[test]
    fn reject_links_submodules_duplicate_files_and_unknown_contributions() {
        for kind in [PackageFileKind::Symlink, PackageFileKind::Submodule] {
            let mut files = review_files();
            files[1].kind = kind;
            assert!(validate_files(&files).is_err());
        }
        let mut files = review_files();
        files.push(files[1].clone());
        assert!(validate_files(&files).is_err());
        let mut files = review_files();
        let mut json: serde_json::Value = serde_json::from_slice(&files[0].content).unwrap();
        json["contributions"]["panels"] = serde_json::json!([]);
        files[0].content = serde_json::to_vec(&json).unwrap();
        assert!(validate_files(&files).is_err());
        let mut files = review_files();
        files[1].path = "../README.md".into();
        assert!(validate_files(&files).is_err());
    }
    #[test]
    fn receipts_merge_hosts_without_enabling_and_updates_require_revision() {
        let request = RecordPlugin {
            approved_capabilities: Vec::new(),
            update_policy: None,
            package: None,
            prepared: receipt(),
            revision: None,
        };
        let installed = record_installation(Vec::new(), &request, 1).unwrap();
        assert_eq!(installed[0].revision, 1);
        let mut other_host = request.clone();
        other_host.prepared.host_id = "host-b".into();
        let installed = record_installation(installed, &other_host, 2).unwrap();
        assert_eq!(installed[0].hosts, ["host-a", "host-b"]);
        let mut update = request.clone();
        update.prepared.source.commit = "b".repeat(40);
        assert!(matches!(
            record_installation(installed.clone(), &update, 3),
            Err(PluginError::Conflict(_))
        ));
        update.revision = Some(installed[0].revision);
        let next = record_installation(installed, &update, 3).unwrap();
        assert_eq!(next[0].hosts, ["host-a"]);
        assert!(matches!(
            record_installation(next, &update, 4),
            Err(PluginError::Conflict(_))
        ));
    }
    #[test]
    fn same_commit_cannot_change_its_validated_contents() {
        let request = RecordPlugin {
            approved_capabilities: Vec::new(),
            update_policy: None,
            package: None,
            prepared: receipt(),
            revision: None,
        };
        let installed = record_installation(Vec::new(), &request, 1).unwrap();
        let mut changed = request.clone();
        changed.revision = Some(1);
        changed.prepared.digest = "b".repeat(64);
        assert!(matches!(
            record_installation(installed, &changed, 2),
            Err(PluginError::Conflict(_))
        ));
    }
}

/// Validated skill instructions/resources; executable package files stay on the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPackage {
    pub prepared: PreparedPlugin,
    pub skills: Vec<crate::SkillDraft>,
}
impl PluginPackage {
    pub fn validate(&self) -> Result<(), PluginError> {
        self.prepared.validate()?;
        if self.skills.len() != self.prepared.manifest.contributions.skills.len()
            || serde_json::to_vec(self)
                .map_err(|error| invalid(error.to_string()))?
                .len()
                > MAX_PACKAGE_BYTES
        {
            return Err(invalid(
                "Plugin skill payload exceeds its contribution/content limits.",
            ));
        }
        for (draft, contribution) in self
            .skills
            .iter()
            .zip(&self.prepared.manifest.contributions.skills)
        {
            draft.validate().map_err(invalid)?;
            let name = contribution
                .path
                .trim_end_matches("/SKILL.md")
                .rsplit('/')
                .next()
                .unwrap_or_default();
            if draft.name != name {
                return Err(invalid("Plugin skills do not match their manifest."));
            }
        }
        Ok(())
    }
}
pub fn import_plugin_skills(
    files: &[PackageFile],
    manifest: &PluginManifest,
) -> Result<Vec<crate::SkillDraft>, PluginError> {
    manifest
        .contributions
        .skills
        .iter()
        .map(|skill| {
            let directory = skill
                .path
                .strip_suffix("SKILL.md")
                .ok_or_else(|| invalid("Invalid skill path."))?;
            let imported = files
                .iter()
                .filter_map(|file| {
                    file.path.strip_prefix(directory).map(|name| ImportFile {
                        name: name.into(),
                        bytes: file.content.clone(),
                    })
                })
                .collect();
            import_files(imported).map_err(invalid)
        })
        .collect()
}
pub async fn load_plugin_package(
    host: &impl PluginHost,
    expected: &PreparedPlugin,
) -> Result<PluginPackage, PluginError> {
    expected.validate()?;
    let prepared = prepare_plugin(host, &expected.source).await?;
    if prepared.source != expected.source
        || prepared.manifest != expected.manifest
        || prepared.digest != expected.digest
    {
        return Err(PluginError::Conflict(
            "The prepared package does not match the installed version.".into(),
        ));
    }
    let files = host.files(&prepared.source).await?;
    if package_digest(&files) != prepared.digest {
        return Err(PluginError::Conflict(
            "The package changed while loading its skills.".into(),
        ));
    }
    let package = PluginPackage {
        skills: import_plugin_skills(&files, &prepared.manifest)?,
        prepared,
    };
    package.validate()?;
    Ok(package)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPlugin {
    pub id: i64,
    pub revision: i64,
    pub prepared: PreparedPlugin,
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectPluginCommand {
    Enable {
        package: Box<PluginPackage>,
        installation_revision: i64,
        revision: Option<i64>,
    },
    Disable {
        id: i64,
        revision: i64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovePlugin {
    pub source: PluginSource,
    pub revision: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSkillOrigin {
    pub publisher: String,
    pub name: String,
    pub version: String,
    pub commit: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginUpdatePolicy {
    #[default]
    Notify,
    Automatic,
    Off,
}

/// Runtime authority additions are compared after preparing the actual source,
/// regardless of catalog claims or SemVer compatibility.
pub fn added_capabilities(previous: &PreparedPlugin, next: &PreparedPlugin) -> Vec<String> {
    let previous: std::collections::BTreeSet<_> = previous
        .manifest
        .executable
        .iter()
        .flat_map(|rust| rust.capabilities.iter())
        .collect();
    next.manifest
        .executable
        .iter()
        .flat_map(|rust| rust.capabilities.iter())
        .filter(|capability| !previous.contains(capability))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}
#[derive(Clone, Debug)]
pub struct PluginUpdate {
    pub installation: PluginInstallation,
    pub marketplace: marketplace::MarketplaceSource,
    pub version: String,
    pub automatic: bool,
}
/// Shared update discovery uses SemVer precedence and the installation's source.
pub fn available_updates(
    installations: &[PluginInstallation],
    markets: &marketplace::MarketplaceSettings,
) -> Vec<PluginUpdate> {
    discover_updates(installations, markets, false)
}
pub fn automatic_updates(
    installations: &[PluginInstallation],
    markets: &marketplace::MarketplaceSettings,
) -> Vec<PluginUpdate> {
    discover_updates(installations, markets, true)
}
fn discover_updates(
    installations: &[PluginInstallation],
    markets: &marketplace::MarketplaceSettings,
    automatic_only: bool,
) -> Vec<PluginUpdate> {
    installations
        .iter()
        .filter_map(|entry| {
            if entry.update_policy == PluginUpdatePolicy::Off
                || (automatic_only && entry.update_policy != PluginUpdatePolicy::Automatic)
            {
                return None;
            }
            let current = semver::Version::parse(&entry.prepared.manifest.version).ok()?;
            markets
                .catalogs
                .iter()
                .filter(|catalog| catalog.source.repository == entry.prepared.source.repository)
                .flat_map(|catalog| {
                    catalog
                        .catalog
                        .plugins
                        .iter()
                        .filter(|plugin| {
                            plugin.publisher == entry.prepared.manifest.publisher
                                && plugin.name == entry.prepared.manifest.name
                        })
                        .flat_map(move |plugin| {
                            plugin
                                .releases
                                .iter()
                                .map(move |release| (catalog, release))
                        })
                })
                .filter_map(|(catalog, release)| {
                    let version = semver::Version::parse(&release.version).ok()?;
                    (release.source.path == entry.prepared.source.path
                        && version.cmp_precedence(&current).is_gt()
                        && (version.pre.is_empty() || !current.pre.is_empty())
                        && (!automatic_only
                            || (version.major == current.major
                                && (current.major != 0 || version.minor == current.minor))))
                        .then_some((catalog, release, version))
                })
                .max_by(|a, b| a.2.cmp_precedence(&b.2))
                .map(|(catalog, release, version)| PluginUpdate {
                    installation: entry.clone(),
                    marketplace: catalog.source.clone(),
                    version: release.version.clone(),
                    automatic: entry.update_policy == PluginUpdatePolicy::Automatic
                        && version.major == current.major
                        && (current.major != 0 || version.minor == current.minor),
                })
        })
        .collect()
}

/// Historical service switches retained for reading existing plugin receipts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginToolGroup {
    Web,
    Memory,
    Scheduling,
    SkillAuthoring,
}
pub fn default_bindings(installations: &[PluginInstallation]) -> Vec<ProjectPlugin> {
    installations
        .iter()
        .filter(|entry| entry.default_enabled)
        .map(|entry| ProjectPlugin {
            id: 0,
            revision: entry.revision,
            prepared: entry.prepared.clone(),
            enabled: true,
        })
        .collect()
}

#[cfg(test)]
mod update_tests {
    use super::*;
    use testing::{catalog, receipt};
    #[test]
    fn capability_expansion_requires_exact_review_even_for_compatible_updates() {
        let mut prepared = receipt();
        prepared.manifest.compatibility.plugin_api = 3;
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.tools = vec![PluginTool {
            name: "community_lookup".into(),
            description: "Look up records".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: false,
        }];
        prepared.manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "lookup".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["records".into()],
        });
        let install = RecordPlugin {
            approved_capabilities: Vec::new(),
            update_policy: Some(PluginUpdatePolicy::Automatic),
            package: None,
            prepared: prepared.clone(),
            revision: None,
        };
        let installed = record_installation(Vec::new(), &install, 1).unwrap();
        let mut next = prepared.clone();
        next.source.commit = "b".repeat(40);
        next.digest = "b".repeat(64);
        next.manifest.version = "0.1.1".into();
        next.manifest.executable.as_mut().unwrap().capabilities =
            vec!["http".into(), "records".into()];
        assert_eq!(added_capabilities(&prepared, &next), vec!["http"]);
        let mut update = RecordPlugin {
            prepared: next.clone(),
            revision: Some(installed[0].revision),
            ..install.clone()
        };
        for approval in [
            vec![],
            vec!["records".into()],
            vec!["http".into(), "jobs".into()],
        ] {
            update.approved_capabilities = approval;
            assert!(matches!(
                record_installation(installed.clone(), &update, 2),
                Err(PluginError::Conflict(_))
            ));
        }
        update.approved_capabilities = vec!["http".into()];
        let installed = record_installation(installed, &update, 2).unwrap();
        assert_eq!(installed[0].prepared, next);
        assert_eq!(installed[0].update_policy, PluginUpdatePolicy::Automatic);
        update.prepared.source.commit = "c".repeat(40);
        update.prepared.digest = "c".repeat(64);
        update.prepared.manifest.version = "0.1.2".into();
        update
            .prepared
            .manifest
            .executable
            .as_mut()
            .unwrap()
            .capabilities = vec!["records".into()];
        update.approved_capabilities.clear();
        update.revision = Some(installed[0].revision);
        assert!(added_capabilities(&next, &update.prepared).is_empty());
        assert!(record_installation(installed.clone(), &update, 3).is_ok());
        update.revision = Some(installed[0].revision - 1);
        update.approved_capabilities = vec!["http".into()];
        assert!(record_installation(installed, &update, 3).is_err());
    }
    #[test]
    fn notify_is_default_and_updates_use_semver_source_and_compatible_auto_policy() {
        let mut cache = catalog();
        for (version, path) in [
            ("0.1.10", "plugins/pr-review"),
            ("0.1.2", "plugins/pr-review"),
            ("0.2.0", "plugins/pr-review"),
            ("1.0.0-rc.1", "plugins/pr-review"),
            ("9.0.0", "plugins/other"),
        ] {
            cache.catalog.plugins[0]
                .releases
                .push(marketplace::CatalogRelease {
                    version: version.into(),
                    source: marketplace::CatalogReleaseSource {
                        commit: "b".repeat(40),
                        path: path.into(),
                    },
                });
        }
        let mut entries = record_installation(
            Vec::new(),
            &RecordPlugin {
                approved_capabilities: Vec::new(),
                prepared: receipt(),
                revision: None,
                package: None,
                update_policy: None,
            },
            0,
        )
        .unwrap();
        // Match the fixture directory, independent of the release order.
        for release in &mut cache.catalog.plugins[0].releases {
            if release.source.path == "plugins/pr-review" {
                release.source.path = entries[0].prepared.source.path.clone();
            }
        }
        let mut markets = marketplace::MarketplaceSettings {
            revision: 1,
            sources: vec![cache.source.clone()],
            catalogs: vec![cache],
        };
        assert_eq!(entries[0].update_policy, PluginUpdatePolicy::Notify);
        assert_eq!(available_updates(&entries, &markets)[0].version, "0.2.0");
        entries[0].update_policy = PluginUpdatePolicy::Automatic;
        assert!(!available_updates(&entries, &markets)[0].automatic);
        assert_eq!(automatic_updates(&entries, &markets)[0].version, "0.1.10");
        markets.catalogs[0].catalog.plugins[0]
            .releases
            .retain(|release| release.version != "0.2.0");
        assert_eq!(available_updates(&entries, &markets)[0].version, "0.1.10");
        assert!(available_updates(&entries, &markets)[0].automatic);
        entries[0].update_policy = PluginUpdatePolicy::Off;
        assert!(available_updates(&entries, &markets).is_empty());
    }
    #[test]
    fn preferences_are_revision_checked_and_verified_content_cannot_mismatch_receipts() {
        let request = RecordPlugin {
            approved_capabilities: Vec::new(),
            prepared: receipt(),
            revision: None,
            package: None,
            update_policy: None,
        };
        let entries = record_installation(Vec::new(), &request, 0).unwrap();
        let mut update = request.clone();
        update.update_policy = Some(PluginUpdatePolicy::Off);
        assert!(record_installation(entries.clone(), &update, 1).is_err());
        update.revision = Some(entries[0].revision);
        let changed = record_installation(entries.clone(), &update, 1).unwrap();
        assert_eq!(changed[0].update_policy, PluginUpdatePolicy::Off);
        assert!(record_installation(changed, &update, 2).is_err());
        let mut wrong = testing::package();
        wrong.prepared.source.commit = "b".repeat(40);
        update.package = Some(Box::new(wrong));
        assert!(record_installation(entries, &update, 2).is_err());
    }
}
