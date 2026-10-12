//! Catalog parsing, repository inheritance and source/cache mutation policy.
use super::{PluginError, PluginSource, hex, invalid, package_path};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_CATALOG_BYTES: usize = 1024 * 1024;
pub const MAX_MARKETPLACES: usize = 16;
const SCHEMA: &str =
    "https://raw.githubusercontent.com/openwebide/plugins/main/schemas/marketplace.schema.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceSource {
    pub repository: String,
    #[serde(default)]
    pub reference: String,
    pub path: String,
}
impl MarketplaceSource {
    pub fn official() -> Self {
        Self {
            repository: "https://github.com/openwebide/plugins.git".into(),
            reference: String::new(),
            path: "marketplace.json".into(),
        }
    }
    pub fn validate(&self) -> Result<(), PluginError> {
        PluginSource {
            repository: self.repository.clone(),
            commit: "0".repeat(40),
            path: self.path.clone(),
        }
        .validate()?;
        package_path(&self.path)?;
        let reference = &self.reference;
        if reference.len() > 256
            || reference.starts_with('-')
            || reference.starts_with('/')
            || reference.ends_with('/')
            || reference.contains("..")
            || reference.contains("//")
            || reference.ends_with('.')
            || reference
                .split('/')
                .any(|part| part.starts_with('.') || part.strip_suffix(".lock").is_some())
            || !reference
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-/.".contains(&b))
        {
            return Err(invalid(
                "Use a Git branch, tag or full commit as the marketplace reference, or leave it empty for the default branch.",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogReleaseSource {
    pub commit: String,
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogRelease {
    pub version: String,
    pub source: CatalogReleaseSource,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogPlugin {
    pub publisher: String,
    pub name: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<crate::plugins::icon::PluginIcon>,
    pub categories: Vec<String>,
    pub releases: Vec<CatalogRelease>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceCatalog {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub name: String,
    pub plugins: Vec<CatalogPlugin>,
}
impl MarketplaceCatalog {
    pub fn parse(bytes: &[u8]) -> Result<Self, PluginError> {
        if bytes.len() > MAX_CATALOG_BYTES {
            return Err(invalid("Marketplace catalog exceeds 1 MiB."));
        }
        let catalog: Self = serde_json::from_slice(bytes)
            .map_err(|error| invalid(format!("Invalid marketplace.json: {error}")))?;
        catalog.validate()?;
        Ok(catalog)
    }
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.schema_version != 1
            || self
                .schema
                .as_deref()
                .is_some_and(|schema| schema != SCHEMA)
            || self.name.trim().is_empty()
            || self.name.chars().count() > 128
            || self.plugins.len() > 200
            || serde_json::to_vec(self)
                .map_err(|error| invalid(error.to_string()))?
                .len()
                > MAX_CATALOG_BYTES
        {
            return Err(invalid("Unsupported or oversized marketplace catalog."));
        }
        let mut identities = BTreeSet::new();
        for plugin in &self.plugins {
            if !identities.insert((&plugin.publisher, &plugin.name))
                || plugin.releases.is_empty()
                || plugin.releases.len() > 32
                || plugin.categories.is_empty()
                || plugin
                    .categories
                    .iter()
                    .any(|category| !matches!(category.as_str(), "skills" | "tools" | "panels"))
                || plugin.categories.iter().collect::<BTreeSet<_>>().len()
                    != plugin.categories.len()
            {
                return Err(invalid(
                    "Catalog packages need a unique identity, supported categories and 1–32 releases.",
                ));
            }
            let mut versions = BTreeSet::new();
            for release in &plugin.releases {
                if !versions.insert(&release.version) {
                    return Err(invalid("Catalog release versions must be unique."));
                }
                // Reuse the exact package identity/version contract.
                super::PluginManifest {
                    schema: None,
                    schema_version: 1,
                    publisher: plugin.publisher.clone(),
                    name: plugin.name.clone(),
                    version: release.version.clone(),
                    display_name: plugin.display_name.clone(),
                    description: plugin.description.clone(),
                    icon: plugin.icon.clone(),
                    license: "unspecified".into(),
                    readme: None,
                    compatibility: super::PluginCompatibility { plugin_api: 1 },
                    executable: None,
                    contributions: super::PluginContributions {
                        panels: Vec::new(),
                        background: None,
                        events: Vec::new(),
                        tools: vec![],
                        tool_groups: Vec::new(),
                        skills: vec![super::PluginSkill {
                            path: "skills/example/SKILL.md".into(),
                        }],
                    },
                }
                .validate()?;
                if !hex(&release.source.commit, &[40, 64]) {
                    return Err(invalid(
                        "Catalog releases require immutable full commit IDs.",
                    ));
                }
                if release.source.path != "." {
                    package_path(&release.source.path)?;
                }
            }
        }
        Ok(())
    }
    pub fn resolve(
        &self,
        source: &MarketplaceSource,
        publisher: &str,
        name: &str,
        version: &str,
    ) -> Result<PluginSource, PluginError> {
        self.validate()?;
        source.validate()?;
        let release = self
            .plugins
            .iter()
            .find(|plugin| plugin.publisher == publisher && plugin.name == name)
            .and_then(|plugin| {
                plugin
                    .releases
                    .iter()
                    .find(|release| release.version == version)
            })
            .ok_or_else(|| invalid("This release is no longer in the selected catalog."))?;
        Ok(PluginSource {
            repository: source.repository.clone(),
            commit: release.source.commit.clone(),
            path: release.source.path.clone(),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedMarketplace {
    pub source: MarketplaceSource,
    pub commit: String,
    pub catalog: MarketplaceCatalog,
    pub fetched_at: i64,
}
impl CachedMarketplace {
    pub fn validate(&self) -> Result<(), PluginError> {
        self.source.validate()?;
        self.catalog.validate()?;
        if !hex(&self.commit, &[40, 64]) {
            return Err(invalid("Invalid catalog commit."));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<super::updates::UpdateCheckStatus>,
    pub revision: i64,
    pub sources: Vec<MarketplaceSource>,
    pub catalogs: Vec<CachedMarketplace>,
}
impl MarketplaceSettings {
    /// Restore the built-in source when reading settings saved by older versions.
    pub fn ensure_official(mut self) -> Self {
        let official = MarketplaceSource::official();
        if !self.sources.contains(&official) {
            self.sources.insert(0, official);
        }
        self
    }
}
impl Default for MarketplaceSettings {
    fn default() -> Self {
        Self {
            check: None,
            revision: 0,
            sources: vec![MarketplaceSource::official()],
            catalogs: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveMarketplaces {
    pub revision: i64,
    pub sources: Vec<MarketplaceSource>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceFailure {
    pub source: MarketplaceSource,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketplaceRefresh {
    pub settings: MarketplaceSettings,
    pub failures: Vec<MarketplaceFailure>,
}

pub fn save_sources(
    current: MarketplaceSettings,
    request: &SaveMarketplaces,
) -> Result<MarketplaceSettings, PluginError> {
    if current.revision != request.revision {
        return Err(PluginError::Conflict(
            "Marketplace settings changed. Refresh before editing.".into(),
        ));
    }
    if !request.sources.contains(&MarketplaceSource::official()) {
        return Err(invalid("The official marketplace cannot be removed."));
    }
    if request.sources.len() > MAX_MARKETPLACES {
        return Err(invalid("At most 16 marketplaces can be configured."));
    }
    for (i, source) in request.sources.iter().enumerate() {
        source.validate()?;
        if request.sources[..i].contains(source) {
            return Err(invalid("This marketplace is already configured."));
        }
    }
    Ok(MarketplaceSettings {
        check: None,
        revision: current.revision + 1,
        sources: request.sources.clone(),
        catalogs: current
            .catalogs
            .into_iter()
            .filter(|cache| request.sources.contains(&cache.source))
            .collect(),
    })
}
pub fn cache_catalogs(
    mut current: MarketplaceSettings,
    revision: i64,
    catalogs: Vec<CachedMarketplace>,
) -> Result<MarketplaceSettings, PluginError> {
    if current.revision != revision {
        return Err(PluginError::Conflict(
            "Marketplace settings changed during refresh. Try again.".into(),
        ));
    }
    for catalog in catalogs {
        catalog.validate()?;
        if !current.sources.contains(&catalog.source) {
            return Err(invalid("Marketplace source is no longer configured."));
        }
        current
            .catalogs
            .retain(|cached| cached.source != catalog.source);
        current.catalogs.push(catalog);
    }
    current.revision += 1;
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::testing::catalog;
    #[test]
    fn panel_categories_are_supported_without_admitting_arbitrary_contributions() {
        let mut value = catalog().catalog;
        value.plugins[0].categories = vec!["skills".into(), "panels".into()];
        value.validate().unwrap();
        value.plugins[0].categories.push("browser-code".into());
        assert!(value.validate().is_err());
    }
    #[test]
    fn official_marketplace_is_required_and_restored_for_legacy_settings() {
        let legacy = MarketplaceSettings {
            check: None,
            revision: 7,
            sources: vec![catalog().source],
            catalogs: vec![catalog()],
        };
        let restored = legacy.clone().ensure_official();
        assert_eq!(restored.revision, legacy.revision);
        assert_eq!(restored.catalogs, legacy.catalogs);
        assert_eq!(restored.sources[0], MarketplaceSource::official());
        assert_eq!(restored.clone().ensure_official(), restored);
        assert!(matches!(
            save_sources(
                restored.clone(),
                &SaveMarketplaces {
                    revision: restored.revision,
                    sources: legacy.sources,
                }
            ),
            Err(PluginError::Invalid(_))
        ));
        let saved = save_sources(
            restored.clone(),
            &SaveMarketplaces {
                revision: restored.revision,
                sources: vec![MarketplaceSource::official()],
            },
        )
        .unwrap();
        assert_eq!(saved.sources, vec![MarketplaceSource::official()]);
        assert!(saved.catalogs.is_empty());
    }
    #[test]
    fn releases_inherit_the_catalog_repository_and_reject_overrides_and_mutable_pins() {
        let cache = catalog();
        let plugin = &cache.catalog.plugins[0];
        let source = cache
            .catalog
            .resolve(
                &cache.source,
                &plugin.publisher,
                &plugin.name,
                &plugin.releases[0].version,
            )
            .unwrap();
        assert_eq!(source.repository, cache.source.repository);
        for target in ["plugin", "release", "source"] {
            let mut json = serde_json::to_value(&cache.catalog).unwrap();
            let entry = &mut json["plugins"][0];
            let entry = match target {
                "plugin" => entry,
                "release" => &mut entry["releases"][0],
                _ => &mut entry["releases"][0]["source"],
            };
            entry["repository"] = serde_json::json!("https://other.example/plugins.git");
            assert!(MarketplaceCatalog::parse(&serde_json::to_vec(&json).unwrap()).is_err());
        }
        let mut catalog = cache.catalog.clone();
        catalog.plugins[0].releases[0].source.commit = "main".into();
        assert!(catalog.validate().is_err());
        let mut catalog = cache.catalog.clone();
        catalog.plugins.push(catalog.plugins[0].clone());
        assert!(catalog.validate().is_err());
        assert!(MarketplaceCatalog::parse(&vec![b' '; MAX_CATALOG_BYTES + 1]).is_err());
        let mut catalog = cache.catalog;
        catalog.plugins[0].releases[0].source.path = "../outside".into();
        assert!(catalog.validate().is_err());
    }
    #[test]
    fn failed_refresh_preserves_cache_and_source_changes_reject_stale_results() {
        let cache = catalog();
        let settings = MarketplaceSettings {
            check: None,
            revision: 0,
            sources: vec![MarketplaceSource::official(), cache.source.clone()],
            catalogs: vec![cache.clone()],
        };
        let retained = cache_catalogs(settings.clone(), 0, Vec::new()).unwrap();
        assert_eq!(retained.catalogs, vec![cache.clone()]);
        let removed = save_sources(
            settings,
            &SaveMarketplaces {
                revision: 0,
                sources: vec![MarketplaceSource::official()],
            },
        )
        .unwrap();
        assert!(removed.catalogs.is_empty());
        assert!(matches!(
            cache_catalogs(removed, 0, vec![cache]),
            Err(PluginError::Conflict(_))
        ));
        for reference in [
            "-option",
            "../main",
            "refs//main",
            "main.lock",
            "main~1",
            "main:foo",
            "main x",
        ] {
            let mut source = MarketplaceSource::official();
            source.reference = reference.into();
            assert!(source.validate().is_err(), "{reference}");
        }
        for reference in ["", "main", "refs/tags/v1.0", "feature/plugin-support"] {
            let mut source = MarketplaceSource::official();
            source.reference = reference.into();
            source.validate().unwrap();
        }
    }
}
