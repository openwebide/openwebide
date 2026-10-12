//! Shared installation planning for UI and agent callers.
use super::{
    PluginError, PluginInstallation, PluginPackage, PluginSource, PreparedPlugin, RecordPlugin,
    added_capabilities,
    marketplace::{MarketplaceSettings, MarketplaceSource},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSelection {
    pub marketplace: MarketplaceSource,
    pub publisher: String,
    pub name: String,
    pub version: String,
}
impl CatalogSelection {
    pub fn resolve(&self, markets: &MarketplaceSettings) -> Result<PluginSource, PluginError> {
        let catalog = markets
            .catalogs
            .iter()
            .find(|catalog| catalog.source == self.marketplace)
            .ok_or_else(|| PluginError::Invalid("Refresh this marketplace first.".into()))?;
        catalog
            .catalog
            .resolve(&catalog.source, &self.publisher, &self.name, &self.version)
    }
}

/// Capture the expected source and revision before host preparation begins.
/// Callers commit the resulting request through the revision-checked storage lifecycle.
pub struct InstallationPlan {
    pub source: PluginSource,
    previous: Option<PluginInstallation>,
    selection: Option<CatalogSelection>,
}
impl InstallationPlan {
    pub fn new(
        source: PluginSource,
        selection: Option<CatalogSelection>,
        installations: &[PluginInstallation],
    ) -> Result<Self, PluginError> {
        source.validate()?;
        let previous = installations
            .iter()
            .find(|entry| {
                entry.prepared.source.repository == source.repository
                    && entry.prepared.source.path == source.path
            })
            .cloned();
        Ok(Self {
            source,
            previous,
            selection,
        })
    }

    pub fn validate_prepared(&self, prepared: &PreparedPlugin) -> Result<(), PluginError> {
        prepared.validate()?;
        if prepared.source != self.source {
            return Err(PluginError::Invalid(
                "Plugin host returned a different source.".into(),
            ));
        }
        if self.selection.as_ref().is_some_and(|selection| {
            selection.publisher != prepared.manifest.publisher
                || selection.name != prepared.manifest.name
                || selection.version != prepared.manifest.version
        }) {
            return Err(PluginError::Invalid(
                "Plugin identity or version does not match its marketplace release.".into(),
            ));
        }
        Ok(())
    }

    /// Returns exact capability additions for review, without authorizing them.
    pub fn request(
        &self,
        expected: &PreparedPlugin,
        package: PluginPackage,
    ) -> Result<RecordPlugin, PluginError> {
        self.validate_prepared(expected)?;
        validate_loaded_plugin(expected, &package)?;
        Ok(RecordPlugin {
            approved_capabilities: Vec::new(),
            update_policy: None,
            revision: self.previous.as_ref().map(|entry| entry.revision),
            prepared: package.prepared.clone(),
            package: Some(Box::new(package)),
        })
    }

    pub fn capability_additions(&self, prepared: &PreparedPlugin) -> Vec<String> {
        self.previous.as_ref().map_or_else(Vec::new, |entry| {
            added_capabilities(&entry.prepared, prepared)
        })
    }
}

pub fn validate_loaded_plugin(
    expected: &PreparedPlugin,
    package: &PluginPackage,
) -> Result<(), PluginError> {
    package.validate()?;
    if package.prepared.source != expected.source
        || package.prepared.manifest != expected.manifest
        || package.prepared.digest != expected.digest
    {
        return Err(PluginError::Invalid(
            "Plugin host returned different plugin content.".into(),
        ));
    }
    Ok(())
}

/// An identity includes the source repository and package directory. Two marketplaces
/// may publish identical display names without authorizing one another's plugins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginIdentity {
    pub repository: String,
    pub path: String,
    pub publisher: String,
    pub name: String,
}
impl PluginIdentity {
    pub fn matches(&self, prepared: &PreparedPlugin) -> bool {
        self.repository == prepared.source.repository
            && self.path == prepared.source.path
            && self.publisher == prepared.manifest.publisher
            && self.name == prepared.manifest.name
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginListing {
    pub identity: PluginIdentity,
    pub display_name: String,
    pub description: String,
    pub marketplace: Option<MarketplaceSource>,
    pub installed: Option<PluginInstallation>,
    pub releases: Vec<PluginRelease>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRelease {
    pub version: String,
    pub source: PluginSource,
    pub marketplace: MarketplaceSource,
}

/// Search the retained catalogs and installations without requiring a live Git host.
/// The caller may explicitly refresh first; discovery itself remains usable offline.
pub fn search(
    markets: &MarketplaceSettings,
    installations: &[PluginInstallation],
    query: &str,
    installed_only: bool,
    limit: usize,
) -> Result<Vec<PluginListing>, PluginError> {
    search_selected(markets, installations, query, installed_only, limit, None)
}

pub fn inspect_listing(
    markets: &MarketplaceSettings,
    installations: &[PluginInstallation],
    identity: &PluginIdentity,
) -> Result<Option<PluginListing>, PluginError> {
    Ok(search_selected(markets, installations, "", false, 1, Some(identity))?.pop())
}

fn search_selected(
    markets: &MarketplaceSettings,
    installations: &[PluginInstallation],
    query: &str,
    installed_only: bool,
    limit: usize,
    selected: Option<&PluginIdentity>,
) -> Result<Vec<PluginListing>, PluginError> {
    if query.len() > 512 || !(1..=50).contains(&limit) {
        return Err(PluginError::Invalid(
            "Search accepts at most 512 bytes and 1–50 results.".into(),
        ));
    }
    let words: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut entries = Vec::new();
    // Installed-only entries are visible even if their marketplace is removed or offline.
    for installed in installations {
        let prepared = &installed.prepared;
        if selected.is_some_and(|identity| !identity.matches(prepared)) {
            continue;
        }
        entries.push(PluginListing {
            identity: PluginIdentity {
                repository: prepared.source.repository.clone(),
                path: prepared.source.path.clone(),
                publisher: prepared.manifest.publisher.clone(),
                name: prepared.manifest.name.clone(),
            },
            display_name: prepared.manifest.display_name.clone(),
            description: prepared.manifest.description.clone(),
            installed: Some(installed.clone()),
            marketplace: None,
            releases: Vec::new(),
        });
    }
    for catalog in &markets.catalogs {
        if !markets.sources.contains(&catalog.source) {
            continue;
        }
        catalog.validate()?;
        for plugin in &catalog.catalog.plugins {
            for release in &plugin.releases {
                let identity = PluginIdentity {
                    repository: catalog.source.repository.clone(),
                    path: release.source.path.clone(),
                    publisher: plugin.publisher.clone(),
                    name: plugin.name.clone(),
                };
                if selected.is_some_and(|selected| selected != &identity) {
                    continue;
                }
                let index = entries.iter().position(|entry| entry.identity == identity);
                let index = index.unwrap_or_else(|| {
                    entries.push(PluginListing {
                        identity,
                        display_name: plugin.display_name.clone(),
                        description: plugin.description.clone(),
                        marketplace: Some(catalog.source.clone()),
                        installed: None,
                        releases: Vec::new(),
                    });
                    entries.len() - 1
                });
                let entry = &mut entries[index];
                entry
                    .marketplace
                    .get_or_insert_with(|| catalog.source.clone());
                let release = PluginRelease {
                    version: release.version.clone(),
                    source: PluginSource {
                        repository: catalog.source.repository.clone(),
                        commit: release.source.commit.clone(),
                        path: release.source.path.clone(),
                    },
                    marketplace: catalog.source.clone(),
                };
                if !entry.releases.contains(&release) {
                    entry.releases.push(release);
                }
            }
        }
    }
    entries.retain(|entry| {
        (!installed_only || entry.installed.is_some()) && {
            let haystack = format!(
                "{} {} {} {}",
                entry.identity.publisher,
                entry.identity.name,
                entry.display_name,
                entry.description
            )
            .to_lowercase();
            words.iter().all(|word| haystack.contains(word))
        }
    });
    entries.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
            .then_with(|| a.identity.repository.cmp(&b.identity.repository))
            .then_with(|| a.identity.path.cmp(&b.identity.path))
    });
    entries.truncate(limit);
    Ok(entries)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementSnapshot {
    pub project_id: Option<i64>,
    pub installations: Vec<PluginInstallation>,
    pub marketplaces: MarketplaceSettings,
    pub bindings: Vec<super::ProjectPlugin>,
}

/// Persistence primitives. Authority and project identity come from the authenticated
/// adapter, never from a model argument. Store methods retain ownership and revision checks.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagementRequest {
    Snapshot,
    Record {
        request: Box<RecordPlugin>,
    },
    Project {
        command: super::ProjectPluginCommand,
    },
    Remove {
        request: super::RemovePlugin,
    },
    Marketplaces {
        request: super::marketplace::SaveMarketplaces,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{PluginUpdatePolicy, testing::receipt};

    fn installation(prepared: PreparedPlugin) -> PluginInstallation {
        PluginInstallation {
            hosts: vec![prepared.host_id.clone()],
            prepared,
            revision: 7,
            installed_at: 1,
            default_enabled: true,
            update_policy: PluginUpdatePolicy::Notify,
        }
    }

    #[test]
    fn plans_capture_revision_and_never_approve_capabilities() {
        let previous = receipt();
        let mut next = previous.clone();
        next.source.commit = "b".repeat(40);
        let plan =
            InstallationPlan::new(next.source.clone(), None, &[installation(previous)]).unwrap();
        let mut package = crate::plugins::testing::package();
        package.prepared = next.clone();
        let request = plan.request(&next, package).unwrap();
        assert_eq!(request.revision, Some(7));
        assert!(request.approved_capabilities.is_empty());
        assert!(request.update_policy.is_none());
    }

    #[test]
    fn preparation_cannot_substitute_identity_version_or_source() {
        let prepared = receipt();
        let selection = CatalogSelection {
            marketplace: MarketplaceSource::official(),
            publisher: prepared.manifest.publisher.clone(),
            name: prepared.manifest.name.clone(),
            version: prepared.manifest.version.clone(),
        };
        let plan = InstallationPlan::new(prepared.source.clone(), Some(selection), &[]).unwrap();
        plan.validate_prepared(&prepared).unwrap();
        for kind in 0..4 {
            let mut substituted = prepared.clone();
            match kind {
                0 => substituted.source.commit = "c".repeat(40),
                1 => substituted.manifest.publisher = "other".into(),
                2 => substituted.manifest.name = "other".into(),
                _ => substituted.manifest.version = "9.0.0".into(),
            }
            assert!(plan.validate_prepared(&substituted).is_err());
        }
    }

    #[test]
    fn package_digest_substitution_is_rejected() {
        let prepared = receipt();
        let mut returned = prepared.clone();
        returned.digest = "d".repeat(64);
        let package = PluginPackage {
            prepared: returned,
            skills: Vec::new(),
        };
        assert!(validate_loaded_plugin(&prepared, &package).is_err());
    }
    #[test]
    fn discovery_retains_offline_installations_and_filters_removed_sources() {
        let installed = installation(receipt());
        let catalog = crate::plugins::testing::catalog();
        let mut markets = MarketplaceSettings::default();
        markets.catalogs.push(catalog.clone());
        let found = search(
            &markets,
            std::slice::from_ref(&installed),
            "review",
            false,
            10,
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].marketplace.is_none());
        assert!(search(&markets, &[], "", false, 10).unwrap().is_empty());
        markets.sources.push(catalog.source.clone());
        let found = search(&markets, &[installed], "review", true, 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].marketplace.as_ref(), Some(&catalog.source));
        assert_eq!(found[0].releases.len(), 1);
    }

    #[test]
    fn discovery_preserves_distinct_repository_identities_and_bounds_requests() {
        let first = crate::plugins::testing::catalog();
        let mut second = first.clone();
        second.source.repository = "https://other.example.org/plugins.git".into();
        let markets = MarketplaceSettings {
            sources: vec![first.source.clone(), second.source.clone()],
            catalogs: vec![first, second],
            ..Default::default()
        };
        let results = search(&markets, &[], "review", false, 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_ne!(results[0].identity, results[1].identity);
        assert_eq!(search(&markets, &[], "", false, 1).unwrap().len(), 1);
        assert!(search(&markets, &[], &"a".repeat(513), false, 10).is_err());
        assert!(search(&markets, &[], "", false, 51).is_err());
    }
}
