//! Deployment selection from public container and service metadata.
use super::{Deployment, UpdateSettings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeploymentEvidence {
    pub runtime: String,
    pub id: String,
    pub hostname: String,
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub source_path: Option<String>,
    pub source_text: Option<String>,
}

pub fn discover(identity: &str, evidence: &[DeploymentEvidence]) -> Result<Deployment, String> {
    if identity.is_empty() {
        return Err(
            "This bridge has no container identity. Configure the installation manually.".into(),
        );
    }
    let mut matches = evidence.iter().filter(|container| {
        container.hostname == identity
            || (identity.len() >= 12 && container.id.starts_with(identity))
            || container.name.trim_start_matches('/') == identity
    });
    let container = matches.next().ok_or("Could not identify this installation on the SSH host. Use the advanced deployment settings.")?;
    if matches.next().is_some() {
        return Err(
            "Multiple containers match this installation. Configure the deployment explicitly."
                .into(),
        );
    }
    let label = |key: &str| container.labels.get(key).filter(|value| !value.is_empty());
    let deployment = if let (Some(directory), Some(files), Some(service)) = (
        label("com.docker.compose.project.working_dir").filter(|_| container.runtime == "docker"),
        label("com.docker.compose.project.config_files"),
        label("com.docker.compose.service"),
    ) {
        Deployment::Compose {
            directory: directory.clone(),
            files: files.split(',').map(str::to_owned).collect(),
            service: service.clone(),
        }
    } else if let (Some(service), Some(source), Some(text)) = (
        label("PODMAN_SYSTEMD_UNIT").filter(|_| container.runtime == "podman"),
        &container.source_path,
        &container.source_text,
    ) {
        let mut section = "";
        let mut images = Vec::new();
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line;
            }
            if section == "[Container]"
                && let Some(image) = line.strip_prefix("Image=")
            {
                images.push(image.trim());
            }
        }
        if images.len() != 1
            || std::path::Path::new(images[0]).extension() != Some(std::ffi::OsStr::new("image"))
        {
            return Err("This Quadlet does not reference one image file. Configure the deployment explicitly.".into());
        }
        let parent = std::path::Path::new(source)
            .parent()
            .ok_or("Invalid Quadlet source path.")?;
        Deployment::Quadlet {
            image_file: parent.join(images[0]).to_string_lossy().into_owned(),
            service: service.clone(),
        }
    } else {
        return Err("No supported Compose or Quadlet metadata was found. Configure the installation manually.".into());
    };
    UpdateSettings {
        deployment: deployment.clone(),
        ..Default::default()
    }
    .validate()?;
    Ok(deployment)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compose_preserves_all_files_and_service() {
        let container = DeploymentEvidence {
            runtime: "docker".into(),
            id: "a".repeat(64),
            labels: BTreeMap::from([
                (
                    "com.docker.compose.project.working_dir".into(),
                    "/srv/ide".into(),
                ),
                (
                    "com.docker.compose.project.config_files".into(),
                    "/srv/ide/compose.yml,/srv/ide/ssh.yml".into(),
                ),
                ("com.docker.compose.service".into(), "ide".into()),
            ]),
            ..Default::default()
        };
        assert_eq!(
            discover(&"a".repeat(12), std::slice::from_ref(&container)).unwrap(),
            Deployment::Compose {
                directory: "/srv/ide".into(),
                files: vec!["/srv/ide/compose.yml".into(), "/srv/ide/ssh.yml".into()],
                service: "ide".into()
            }
        );
        assert!(discover("another-host", std::slice::from_ref(&container)).is_err());
        assert!(discover(&"a".repeat(12), &[container.clone(), container]).is_err());
    }
    #[test]
    fn quadlet_uses_actual_service_source_and_image_reference() {
        let mut container = DeploymentEvidence {
            runtime: "podman".into(),
            hostname: "ide".into(),
            labels: BTreeMap::from([("PODMAN_SYSTEMD_UNIT".into(), "ide.service".into())]),
            source_path: Some("/home/user/.config/containers/systemd/ide.container".into()),
            source_text: Some("[Container]\nImage=release.image\n".into()),
            ..Default::default()
        };
        assert_eq!(
            discover("ide", std::slice::from_ref(&container)).unwrap(),
            Deployment::Quadlet {
                image_file: "/home/user/.config/containers/systemd/release.image".into(),
                service: "ide.service".into()
            }
        );
        container.source_text = Some("[Container]\nImage=registry/app:latest\n".into());
        assert!(discover("ide", &[container]).is_err());
    }
}
