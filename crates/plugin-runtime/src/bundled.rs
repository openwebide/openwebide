//! Precompiled source snapshots for offline host defaults; normal grants still apply.
use anyhow::{Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use openwebide_core::plugins::{PluginSource, content_digest};
use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledBundle {
    pub artifacts: Vec<BundledArtifact>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BundledArtifact {
    pub source: PluginSource,
    pub package_digest: String,
    pub sdk_digest: String,
    pub toolchain: String,
    pub artifact_digest: String,
    pub component: String,
}
impl BundledArtifact {
    pub fn new(source: PluginSource, package_digest: String, bytes: &[u8]) -> Self {
        Self {
            source,
            package_digest,
            sdk_digest: crate::build::sdk_digest(),
            toolchain: crate::build::TOOLCHAIN.into(),
            artifact_digest: content_digest(bytes),
            component: STANDARD.encode(bytes),
        }
    }
}
impl CompiledBundle {
    pub fn component(&self, source: &PluginSource, digest: &str) -> Result<Option<Vec<u8>>> {
        let mut matches = self
            .artifacts
            .iter()
            .filter(|entry| &entry.source == source);
        let Some(entry) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            bail!("Duplicate bundled plugin source");
        }
        if entry.package_digest != digest
            || entry.sdk_digest != crate::build::sdk_digest()
            || entry.toolchain != crate::build::TOOLCHAIN
        {
            return Ok(None);
        }
        if entry.component.len() > 45 * 1024 * 1024 {
            bail!("Bundled component exceeds its limit");
        }
        let bytes = STANDARD.decode(&entry.component)?;
        if bytes.len() > 32 * 1024 * 1024 || content_digest(&bytes) != entry.artifact_digest {
            bail!("Bundled component digest mismatch");
        }
        Ok(Some(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundles_match_immutable_source_package_sdk_and_toolchain_and_reject_corruption() {
        let source = PluginSource {
            repository: "https://example.test/plugins.git".into(),
            commit: "a".repeat(40),
            path: "plugins/example".into(),
        };
        let mut bundle = CompiledBundle {
            artifacts: vec![BundledArtifact::new(
                source.clone(),
                "package".into(),
                b"component",
            )],
        };
        assert_eq!(
            bundle.component(&source, "package").unwrap(),
            Some(b"component".to_vec())
        );
        assert!(
            bundle
                .component(&source, "other-package")
                .unwrap()
                .is_none()
        );
        let mut other = source.clone();
        other.commit = "b".repeat(40);
        assert!(bundle.component(&other, "package").unwrap().is_none());
        bundle.artifacts[0].sdk_digest = "old-sdk".into();
        assert!(bundle.component(&source, "package").unwrap().is_none());
        bundle.artifacts[0].sdk_digest = crate::build::sdk_digest();
        bundle.artifacts[0].toolchain = "old-toolchain".into();
        assert!(bundle.component(&source, "package").unwrap().is_none());
        bundle.artifacts[0].toolchain = crate::build::TOOLCHAIN.into();
        bundle.artifacts[0].component = STANDARD.encode(b"changed");
        assert!(bundle.component(&source, "package").is_err());
        bundle.artifacts[0] = BundledArtifact::new(source.clone(), "package".into(), b"component");
        bundle.artifacts.push(BundledArtifact::new(
            source.clone(),
            "package".into(),
            b"component",
        ));
        assert!(bundle.component(&source, "package").is_err());
    }
}
