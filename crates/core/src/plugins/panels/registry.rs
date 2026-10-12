//! Stable panel identities derived from enabled, host-validated packages.
use std::collections::{BTreeMap, BTreeSet};

use super::{PanelDefinition, validate_definition};
use crate::plugins::ProjectPlugin;

pub const MAX_REGISTERED_PANELS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredPanel {
    /// Repository-scoped identity, stable across package updates and host changes.
    pub namespace: String,
    pub plugin_namespace: String,
    pub plugin_label: String,
    /// Replacing this digest invalidates existing panel instances and requests.
    pub digest: String,
    pub definition: PanelDefinition,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelRegistry {
    pub panels: Vec<RegisteredPanel>,
    pub errors: Vec<String>,
}

impl PanelRegistry {
    /// One bad package cannot remove built-in navigation or another plugin's panel.
    pub fn from_bindings(bindings: &[ProjectPlugin]) -> Self {
        let mut registry = Self::default();
        let mut panels = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();
        for binding in bindings.iter().filter(|binding| binding.enabled) {
            let plugin = &binding.prepared;
            let plugin_namespace = plugin.storage_namespace();
            for definition in &plugin.manifest.contributions.panels {
                if let Err(error) = validate_definition(definition) {
                    registry
                        .errors
                        .push(format!("{}: {error}", plugin.manifest.display_name));
                    continue;
                }
                let namespace = format!("plugin:{plugin_namespace}/{}", definition.id);
                let panel = RegisteredPanel {
                    namespace: namespace.clone(),
                    plugin_namespace: plugin_namespace.clone(),
                    plugin_label: plugin.manifest.display_name.clone(),
                    digest: plugin.digest.clone(),
                    definition: definition.clone(),
                };
                if panels.insert(namespace.clone(), panel).is_some() {
                    ambiguous.insert(namespace);
                }
            }
        }
        for namespace in ambiguous {
            panels.remove(&namespace);
            registry
                .errors
                .push(format!("Panel identity is ambiguous: {namespace}"));
        }
        if panels.len() > MAX_REGISTERED_PANELS {
            registry
                .errors
                .push("Only the first 256 plugin panels are available".into());
        }
        registry.panels = panels.into_values().take(MAX_REGISTERED_PANELS).collect();
        registry
    }

    pub fn find(&self, namespace: &str) -> Option<&RegisteredPanel> {
        self.panels
            .iter()
            .find(|panel| panel.namespace == namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> ProjectPlugin {
        serde_json::from_value(serde_json::json!({
            "id": 1, "revision": 1, "enabled": true,
            "prepared": {
                "source": {"repository":"https://example.com/plugins.git","commit":"a".repeat(40),"path":"plugins/reviews"},
                "host_id":"server", "digest":"a".repeat(64),
                "manifest": {
                    "schemaVersion":1,"publisher":"example","name":"reviews","version":"0.1.0",
                    "displayName":"Reviews","description":"Review pull requests","license":"MIT","compatibility":{"pluginApi":4},
                    "executable":{"manifest":"Cargo.toml","library":"reviews","sdkVersion":"0.1.0","capabilities":["http"]},
                    "contributions":{"skills":[],"panels":[{"id":"reviews","title":"Reviews","icon":"git_pull_request","view":"render"}]}
                }
            }
        })).unwrap()
    }
    #[test]
    fn registry_preserves_identity_across_updates_and_separates_repositories() {
        let original = binding();
        let registry = PanelRegistry::from_bindings(std::slice::from_ref(&original));
        assert!(registry.errors.is_empty());
        let key = registry.panels[0].namespace.clone();
        let mut updated = original.clone();
        updated.prepared.digest = "b".repeat(64);
        updated.prepared.source.commit = "b".repeat(40);
        updated.prepared.host_id = "paired-host".into();
        updated.prepared.manifest.version = "0.2.0".into();
        let next = PanelRegistry::from_bindings(std::slice::from_ref(&updated));
        assert_eq!(next.panels[0].namespace, key);
        assert_ne!(next.panels[0].digest, registry.panels[0].digest);
        updated.prepared.source.repository = "https://other.example/plugins.git".into();
        let distinct = PanelRegistry::from_bindings(&[original.clone(), updated]);
        assert_eq!(distinct.panels.len(), 2);
        assert_ne!(distinct.panels[0].namespace, distinct.panels[1].namespace);
        let mut disabled = original;
        disabled.enabled = false;
        assert!(PanelRegistry::from_bindings(&[disabled]).panels.is_empty());
    }
    #[test]
    fn ambiguous_and_invalid_panels_leave_other_navigation_available() {
        let mut invalid = binding();
        invalid.prepared.manifest.contributions.panels[0].id = "invalid/id".into();
        let registry = PanelRegistry::from_bindings(&[binding(), invalid]);
        assert_eq!(registry.panels.len(), 1);
        assert_eq!(registry.errors.len(), 1);
        assert!(registry.find(&registry.panels[0].namespace).is_some());
        let duplicate = PanelRegistry::from_bindings(&[binding(), binding()]);
        assert!(duplicate.panels.is_empty());
        assert_eq!(duplicate.errors.len(), 1);
    }
}
