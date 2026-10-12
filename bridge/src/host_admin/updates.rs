//! SSH transport for shared deployment update policy and detached host primitives.
use super::ssh::SshHost;
use openwebide_core::host_admin::{
    HostCommand,
    updates::{self, Build, Candidate, UpdateAdapter, UpdateJob, UpdateReceipt, UpdateSettings},
};

const HELPER: &str = include_str!("update_host.py");

pub struct SshUpdates(pub SshHost);
impl SshUpdates {
    async fn primitive(
        &self,
        action: &str,
        request: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let result = self
            .0
            .command(&HostCommand {
                program: "python3".into(),
                args: vec![
                    "-c".into(),
                    HELPER.into(),
                    action.into(),
                    request.to_string(),
                ],
                cwd: "/".into(),
                elevated: false,
                interactive: false,
                timeout_seconds: 150,
            })
            .await?;
        if !result.is_success() {
            // stderr may contain registry credentials from a host tool; do not return it.
            return Err("Host updater unavailable. Check SSH, Python 3, skopeo and the user systemd manager.".into());
        }
        serde_json::from_str(&result.stdout).map_err(|_| "Invalid host updater response.".into())
    }
    pub async fn status(&self, id: &str) -> Result<UpdateReceipt, String> {
        let value = self
            .primitive("status", serde_json::json!({"id": id}))
            .await?;
        serde_json::from_value(value).map_err(|_| "Invalid update receipt.".into())
    }
}
impl UpdateAdapter for SshUpdates {
    fn connection_revision(&self) -> i64 {
        self.0.revision
    }
    async fn inspect(&self, image: &str) -> Result<Candidate, String> {
        let value = self
            .primitive("inspect", serde_json::json!({"image": image}))
            .await?;
        let text = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(String::from)
                .ok_or_else(|| format!("Published image is missing {key}."))
        };
        if text("Os")? != "linux" {
            return Err("Only Linux deployment images can be installed.".into());
        }
        let label = |key: &str| {
            value
                .get("Labels")
                .and_then(|labels| labels.get(key))
                .and_then(serde_json::Value::as_str)
                .map(String::from)
                .ok_or_else(|| {
                    "This image predates host updates and has no compatibility metadata.".to_owned()
                })
        };
        Ok(Candidate {
            digest: text("Digest")?,
            architecture: text("Architecture")?,
            build: Build {
                version: label("org.opencontainers.image.version")?,
                commit: label("org.opencontainers.image.revision")?,
                schema: label("io.openwebide.database-schema")?
                    .parse()
                    .map_err(|_| "Invalid database compatibility metadata.")?,
            },
        })
    }
    async fn start(&self, settings: &UpdateSettings, job: &UpdateJob) -> Result<(), String> {
        let plan = updates::deployment_plan(settings, &job.candidate)?;
        self.primitive(
            "start",
            serde_json::json!({"id": job.id, "plan": plan, "helper": HELPER}),
        )
        .await?;
        Ok(())
    }
}
