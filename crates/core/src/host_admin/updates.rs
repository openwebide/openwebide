//! Deployment update policy. Hosts provide image inspection and durable process primitives.
use serde::{Deserialize, Serialize};

pub mod discovery;

pub const IMAGE_REPOSITORY: &str = "ghcr.io/openwebide/openwebide";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    #[default]
    Alpha,
    Preview,
    Stable,
}
impl ReleaseChannel {
    pub fn image(self) -> String {
        format!(
            "{IMAGE_REPOSITORY}:{}",
            match self {
                Self::Alpha => "nightly",
                Self::Preview => "preview",
                Self::Stable => "stable",
            }
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Deployment {
    #[default]
    Unconfigured,
    /// Exact Compose files preserve HTTPS/SSH overlays and existing volumes.
    Compose {
        directory: String,
        files: Vec<String>,
        service: String,
    },
    /// A user-owned Quadlet image file and the generated container service.
    Quadlet { image_file: String, service: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettings {
    pub channel: ReleaseChannel,
    pub deployment: Deployment,
    #[serde(default)]
    pub revision: i64,
}
impl UpdateSettings {
    pub fn validate(&self) -> Result<(), String> {
        if serde_json::to_vec(self)
            .map_err(|_| "Invalid update settings.")?
            .len()
            > 8192
        {
            return Err("Update settings exceed the 8 KiB limit.".into());
        }
        fn path(value: &str) -> bool {
            value.starts_with('/') && value.len() <= 4096 && !value.contains(['\0', '\n', '\r'])
        }
        fn service(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 128
                && !value.starts_with('-')
                && value
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "_.-@".contains(ch))
        }
        match &self.deployment {
            Deployment::Unconfigured => Ok(()),
            Deployment::Compose {
                directory,
                files,
                service: name,
            } if path(directory)
                && !files.is_empty()
                && files.len() <= 8
                && files.iter().all(|file| path(file))
                && service(name) =>
            {
                Ok(())
            }
            Deployment::Quadlet {
                image_file,
                service: name,
            } if path(image_file)
                && std::path::Path::new(image_file).extension()
                    == Some(std::ffi::OsStr::new("image"))
                && service(name)
                && name.ends_with(".service") =>
            {
                Ok(())
            }
            _ => Err("Provide absolute deployment paths and a valid service name.".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub version: String,
    pub commit: String,
    pub schema: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub digest: String,
    pub build: Build,
    pub architecture: String,
}
impl Candidate {
    pub fn validate(&self, running: &Build) -> Result<(), String> {
        if self.digest.len() != 71
            || !self.digest.starts_with("sha256:")
            || !self.digest[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("The published image has an invalid digest.".into());
        }
        if self.build.schema < running.schema {
            return Err(
                "This release cannot read the current database. Choose a newer channel.".into(),
            );
        }
        semver::Version::parse(&self.build.version)
            .map_err(|_| "The published image does not declare a valid version.")?;
        if !matches!(self.architecture.as_str(), "amd64" | "arm64")
            || self.build.commit.len() != 40
            || !self
                .build
                .commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(
                "The published image lacks compatible architecture or build metadata.".into(),
            );
        }
        Ok(())
    }
    pub fn image(&self) -> String {
        format!("{IMAGE_REPOSITORY}@{}", self.digest)
    }
    pub fn differs_from(&self, running: &Build) -> bool {
        self.build.commit != running.commit || self.build.version != running.version
    }
    pub fn release_notes(&self) -> String {
        if self.build.version.contains('-') && self.build.commit.len() == 40 {
            format!(
                "https://github.com/openwebide/openwebide/blob/{}/CHANGELOG.md",
                self.build.commit
            )
        } else {
            format!(
                "https://github.com/openwebide/openwebide/releases/tag/v{}",
                self.build.version
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateCheck {
    pub connection_revision: i64,
    pub running: Build,
    pub settings: UpdateSettings,
    pub candidate: Candidate,
    pub available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    Starting,
    Running,
    Succeeded,
    Failed,
    Unknown,
}
impl UpdatePhase {
    pub fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Unknown)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateRequest {
    Settings,
    Discover,
    Save {
        settings: UpdateSettings,
    },
    Check,
    Start {
        candidate: Candidate,
        settings_revision: i64,
        connection_revision: i64,
    },
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateResponse {
    Discovered {
        deployment: Deployment,
    },
    Settings {
        settings: UpdateSettings,
        running: Build,
        job: Option<UpdateJob>,
    },
    Check {
        check: UpdateCheck,
    },
    Status {
        job: Option<UpdateJob>,
        running: Build,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateHostRequest {
    Discover,
    Check {
        settings: UpdateSettings,
        running: Build,
    },
    Start {
        settings: UpdateSettings,
        running: Build,
        job: UpdateJob,
    },
    Status {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateJob {
    pub id: String,
    pub candidate: Candidate,
    pub phase: UpdatePhase,
    pub detail: String,
    pub settings_revision: i64,
    pub connection_revision: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateReceipt {
    pub phase: UpdatePhase,
    pub detail: String,
    #[serde(default)]
    pub idle: bool,
}

pub fn reconcile(job: &mut UpdateJob, running: &Build, receipt: Option<UpdateReceipt>, now: i64) {
    if job.candidate.build.commit == running.commit
        && job.candidate.build.version == running.version
    {
        job.phase = UpdatePhase::Succeeded;
        job.detail = "The selected build is running.".into();
    } else if job.phase.active()
        && let Some(receipt) = receipt
    {
        let age = now.saturating_sub(job.created_at);
        if receipt.phase == UpdatePhase::Succeeded {
            job.phase = if age > 1200 {
                UpdatePhase::Failed
            } else {
                UpdatePhase::Running
            };
            job.detail = "Host restarted; waiting for the selected app build. Inspect the deployment if reconnection fails.".into();
        } else if receipt.phase == UpdatePhase::Unknown && receipt.idle && age > 300 {
            job.phase = UpdatePhase::Failed;
            job.detail = "The host updater is no longer running and the selected build did not start. Inspect the deployment before retrying.".into();
        } else {
            job.phase = receipt.phase;
            job.detail = receipt.detail.chars().take(1024).collect();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UpdateStep {
    VerifyJson {
        argv: Vec<String>,
        cwd: Option<String>,
        keys: Vec<String>,
        expected: String,
    },
    Command {
        argv: Vec<String>,
        cwd: Option<String>,
    },
    ImageSetting {
        path: String,
        prefix: String,
        value: String,
        require_existing: bool,
    },
}

/// Both deployment adapters execute this shared plan; they never choose update policy.
pub fn deployment_plan(
    settings: &UpdateSettings,
    candidate: &Candidate,
) -> Result<Vec<UpdateStep>, String> {
    settings.validate()?;
    let image = candidate.image();
    let command = |argv: Vec<String>, cwd: Option<String>| UpdateStep::Command { argv, cwd };
    let setting = |path: String, prefix: &str, require_existing| UpdateStep::ImageSetting {
        path,
        prefix: prefix.into(),
        value: image.clone(),
        require_existing,
    };
    match &settings.deployment {
        Deployment::Unconfigured => Err("Configure a deployment updater in Settings.".into()),
        Deployment::Compose {
            directory,
            files,
            service,
        } => {
            let mut compose = vec!["docker".into(), "compose".into()];
            for file in files {
                compose.extend(["-f".into(), file.clone()]);
            }
            // Explicit .env input prevents inherited shell variables from overriding the pin.
            compose.extend(["--env-file".into(), format!("{directory}/.env")]);
            let mut verify = vec!["env".into(), format!("OPENWEBIDE_IMAGE={image}")];
            verify.extend(compose[..compose.len() - 2].iter().cloned());
            verify.extend(["config".into(), "--format".into(), "json".into()]);
            let mut up = compose;
            up.extend(
                [
                    "up",
                    "-d",
                    "--wait",
                    "--wait-timeout",
                    "120",
                    "--no-deps",
                    "--force-recreate",
                ]
                .map(String::from),
            );
            up.push(service.clone());
            Ok(vec![
                UpdateStep::VerifyJson {
                    argv: verify,
                    cwd: Some(directory.clone()),
                    keys: vec!["services".into(), service.clone(), "image".into()],
                    expected: image.clone(),
                },
                command(vec!["docker".into(), "pull".into(), image.clone()], None),
                setting(format!("{directory}/.env"), "OPENWEBIDE_IMAGE=", false),
                command(up, Some(directory.clone())),
            ])
        }
        Deployment::Quadlet {
            image_file,
            service,
        } => Ok(vec![
            command(
                vec![
                    "systemctl".into(),
                    "--user".into(),
                    "is-active".into(),
                    service.clone(),
                ],
                None,
            ),
            command(vec!["podman".into(), "pull".into(), image.clone()], None),
            setting(image_file.clone(), "Image=", true),
            command(
                vec!["systemctl".into(), "--user".into(), "daemon-reload".into()],
                None,
            ),
            command(
                vec![
                    "systemctl".into(),
                    "--user".into(),
                    "restart".into(),
                    service.clone(),
                ],
                None,
            ),
        ]),
    }
}

pub trait UpdateAdapter {
    fn connection_revision(&self) -> i64 {
        0
    }
    fn inspect(&self, image: &str) -> impl std::future::Future<Output = Result<Candidate, String>>;
    fn start(
        &self,
        settings: &UpdateSettings,
        job: &UpdateJob,
    ) -> impl std::future::Future<Output = Result<(), String>>;
}

#[derive(Debug)]
pub enum UpdateFailure {
    Rejected(String),
    Uncertain(String),
}
impl From<String> for UpdateFailure {
    fn from(value: String) -> Self {
        Self::Rejected(value)
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateLaunch {
    Started,
    Rejected { detail: String },
    Uncertain,
}

pub async fn check<A: UpdateAdapter>(
    adapter: &A,
    settings: UpdateSettings,
    running: Build,
) -> Result<UpdateCheck, String> {
    settings.validate()?;
    let candidate = adapter.inspect(&settings.channel.image()).await?;
    candidate.validate(&running)?;
    let available = candidate.differs_from(&running);
    Ok(UpdateCheck {
        connection_revision: adapter.connection_revision(),
        running,
        settings,
        candidate,
        available,
    })
}

/// Reinspect the approved immutable digest, never silently follow a moved channel.
pub async fn start<A: UpdateAdapter>(
    adapter: &A,
    settings: &UpdateSettings,
    running: &Build,
    job: &UpdateJob,
) -> Result<(), UpdateFailure> {
    settings.validate()?;
    if matches!(settings.deployment, Deployment::Unconfigured) {
        return Err(UpdateFailure::Rejected(
            "Configure a host updater before installing a release.".into(),
        ));
    }
    if job.settings_revision != settings.revision
        || job.phase != UpdatePhase::Starting
        || job.connection_revision != adapter.connection_revision()
    {
        return Err(UpdateFailure::Rejected(
            "Update settings changed or this update has already started. Check again.".into(),
        ));
    }
    if job.id.len() != 32 || !job.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(UpdateFailure::Rejected("Invalid update identity.".into()));
    }
    job.candidate.validate(running)?;
    if !job.candidate.differs_from(running) {
        return Err(UpdateFailure::Rejected(
            "The selected build is already running.".into(),
        ));
    }
    let candidate = adapter.inspect(&job.candidate.image()).await?;
    if candidate != job.candidate {
        return Err(UpdateFailure::Rejected(
            "Published image metadata changed. Check for updates again.".into(),
        ));
    }
    adapter
        .start(settings, job)
        .await
        .map_err(UpdateFailure::Uncertain)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn running() -> Build {
        Build {
            version: "1.0.0-alpha.6".into(),
            commit: "a".repeat(40),
            schema: 58,
        }
    }
    fn candidate() -> Candidate {
        Candidate {
            digest: format!("sha256:{}", "b".repeat(64)),
            build: Build {
                commit: "c".repeat(40),
                ..running()
            },
            architecture: "amd64".into(),
        }
    }
    #[test]
    fn rolling_commits_and_database_compatibility() {
        let mut next = candidate();
        assert!(next.differs_from(&running()));
        assert!(next.validate(&running()).is_ok());
        next.build.schema = 57;
        assert!(next.validate(&running()).is_err());
        next.build.schema = 58;
        next.digest = "sha256:untrusted".into();
        assert!(next.validate(&running()).is_err());
    }
    #[test]
    fn deployments_bound_paths_and_preserve_overlay_order() {
        let mut settings = UpdateSettings {
            deployment: Deployment::Compose {
                directory: "/srv/openwebide".into(),
                files: vec![
                    "/srv/openwebide/compose.yml".into(),
                    "/srv/openwebide/ssh.yml".into(),
                ],
                service: "openwebide".into(),
            },
            ..UpdateSettings::default()
        };
        assert!(settings.validate().is_ok());
        settings.deployment = Deployment::Quadlet {
            image_file: "relative.image".into(),
            service: "openwebide.service".into(),
        };
        assert!(settings.validate().is_err());
        assert_eq!(
            ReleaseChannel::Alpha.image(),
            format!("{IMAGE_REPOSITORY}:nightly")
        );
    }
    #[test]
    fn both_deployments_pin_the_approved_image_and_reject_stale_or_changed_metadata() {
        struct Adapter {
            candidate: Candidate,
            starts: std::cell::RefCell<Vec<UpdateSettings>>,
        }
        impl UpdateAdapter for Adapter {
            async fn inspect(&self, _image: &str) -> Result<Candidate, String> {
                Ok(self.candidate.clone())
            }
            async fn start(
                &self,
                settings: &UpdateSettings,
                _job: &UpdateJob,
            ) -> Result<(), String> {
                self.starts.borrow_mut().push(settings.clone());
                Ok(())
            }
        }
        futures::executor::block_on(async {
            for deployment in [
                Deployment::Compose {
                    directory: "/srv/openwebide".into(),
                    files: vec!["/srv/openwebide/compose.yml".into()],
                    service: "openwebide".into(),
                },
                Deployment::Quadlet {
                    image_file: "/home/test/openwebide.image".into(),
                    service: "openwebide.service".into(),
                },
            ] {
                let settings = UpdateSettings {
                    deployment,
                    ..UpdateSettings::default()
                };
                let adapter = Adapter {
                    candidate: candidate(),
                    starts: std::cell::RefCell::new(Vec::new()),
                };
                let check = check(&adapter, settings.clone(), running()).await.unwrap();
                assert!(check.available);
                let mut job = UpdateJob {
                    id: "a".repeat(32),
                    candidate: check.candidate,
                    phase: UpdatePhase::Starting,
                    detail: String::new(),
                    settings_revision: 1,
                    connection_revision: 0,
                    created_at: 1,
                };
                assert!(start(&adapter, &settings, &running(), &job).await.is_err());
                assert!(adapter.starts.borrow().is_empty());
                job.settings_revision = 0;
                job.connection_revision = 1;
                assert!(matches!(
                    start(&adapter, &settings, &running(), &job).await,
                    Err(UpdateFailure::Rejected(_))
                ));
                job.connection_revision = 0;
                job.candidate.build.commit = "d".repeat(40);
                assert!(start(&adapter, &settings, &running(), &job).await.is_err());
                job.candidate = adapter.candidate.clone();
                start(&adapter, &settings, &running(), &job).await.unwrap();
                assert_eq!(adapter.starts.borrow().len(), 1);
                let plan = deployment_plan(&settings, &job.candidate).unwrap();
                let encoded = serde_json::to_string(&plan).unwrap();
                assert!(encoded.contains(&job.candidate.image()));
                assert!(!encoded.contains(":nightly"));
            }
        });
    }
    #[test]
    fn host_success_requires_the_selected_build_to_reconnect() {
        let mut job = UpdateJob {
            id: "a".repeat(32),
            candidate: candidate(),
            phase: UpdatePhase::Running,
            detail: String::new(),
            settings_revision: 0,
            connection_revision: 0,
            created_at: 1,
        };
        reconcile(
            &mut job,
            &running(),
            Some(UpdateReceipt {
                phase: UpdatePhase::Succeeded,
                detail: String::new(),
                idle: true,
            }),
            10,
        );
        assert_eq!(job.phase, UpdatePhase::Running);
        let selected = job.candidate.build.clone();
        reconcile(&mut job, &selected, None, 11);
        assert_eq!(job.phase, UpdatePhase::Succeeded);
        reconcile(
            &mut job,
            &running(),
            Some(UpdateReceipt {
                phase: UpdatePhase::Running,
                detail: String::new(),
                idle: false,
            }),
            12,
        );
        assert_eq!(job.phase, UpdatePhase::Succeeded);
        job.phase = UpdatePhase::Unknown;
        reconcile(
            &mut job,
            &running(),
            Some(UpdateReceipt {
                phase: UpdatePhase::Unknown,
                detail: String::new(),
                idle: true,
            }),
            302,
        );
        assert_eq!(job.phase, UpdatePhase::Failed);
    }
}
