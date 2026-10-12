//! Shared, admin-owned deployment settings and durable self-update claims.
use super::*;
use openwebide_core::host_admin::updates::{UpdateJob, UpdateSettings};

impl<D: Db> Store<D> {
    pub async fn host_update_settings(&self) -> Result<UpdateSettings, StorageError> {
        self.get_setting("host_update_settings").await?.map_or_else(
            || Ok(UpdateSettings::default()),
            |value| {
                serde_json::from_str(&value)
                    .map_err(|error| StorageError::InvalidValue(error.to_string()))
            },
        )
    }
    pub async fn host_update_job(&self) -> Result<Option<UpdateJob>, StorageError> {
        self.get_setting("host_update_job")
            .await?
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| StorageError::InvalidValue(error.to_string()))
            })
            .transpose()
    }
    pub async fn save_host_update_settings(
        &self,
        settings: &UpdateSettings,
    ) -> Result<UpdateSettings, StorageError> {
        settings.validate().map_err(StorageError::InvalidValue)?;
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                if store.host_update_settings().await?.revision != settings.revision {
                    return Err(StorageError::Conflict(
                        "Update settings changed; reload before saving.".into(),
                    ));
                }
                if store
                    .host_update_job()
                    .await?
                    .is_some_and(|job| job.phase.active())
                {
                    return Err(StorageError::Conflict(
                        "Wait for the current host update before changing its settings.".into(),
                    ));
                }
                let mut saved = settings.clone();
                saved.revision = saved.revision.checked_add(1).ok_or_else(|| {
                    StorageError::InvalidValue("Update revision exhausted.".into())
                })?;
                store
                    .set_setting(
                        "host_update_settings",
                        &serde_json::to_string(&saved)
                            .map_err(|error| StorageError::InvalidValue(error.to_string()))?,
                    )
                    .await?;
                Ok(saved)
            })
            .await
    }
    pub async fn claim_host_update(&self, job: &UpdateJob) -> Result<(), StorageError> {
        if job.phase != openwebide_core::host_admin::updates::UpdatePhase::Starting
            || job.id.len() != 32
            || !job.id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || job.created_at <= 0
        {
            return Err(StorageError::InvalidValue(
                "Only a fresh update identity can be claimed.".into(),
            ));
        }
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                if store.host_update_settings().await?.revision != job.settings_revision {
                    return Err(StorageError::Conflict(
                        "Update settings changed; check for updates again.".into(),
                    ));
                }
                if store.host_connection().await?.revision != job.connection_revision {
                    return Err(StorageError::Conflict(
                        "Host SSH connection changed; check for updates again.".into(),
                    ));
                }
                if store
                    .host_update_job()
                    .await?
                    .is_some_and(|old| old.phase.active())
                    || !store.active_host_operations().await?.is_empty()
                {
                    return Err(StorageError::Conflict(
                        "Another host operation is active.".into(),
                    ));
                }
                store
                    .set_setting(
                        "host_update_job",
                        &serde_json::to_string(job)
                            .map_err(|error| StorageError::InvalidValue(error.to_string()))?,
                    )
                    .await
            })
            .await
    }
    pub async fn save_host_update_job(&self, job: &UpdateJob) -> Result<(), StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let Some(old) = store.host_update_job().await? else {
                    return Err(StorageError::Conflict("Host update is missing.".into()));
                };
                if old.id != job.id
                    || old.candidate != job.candidate
                    || old.settings_revision != job.settings_revision
                    || old.connection_revision != job.connection_revision
                {
                    return Err(StorageError::Conflict(
                        "Host update identity changed.".into(),
                    ));
                }
                if !old.phase.active() && job.phase.active() {
                    return Ok(());
                }
                store
                    .set_setting(
                        "host_update_job",
                        &serde_json::to_string(job)
                            .map_err(|error| StorageError::InvalidValue(error.to_string()))?,
                    )
                    .await
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::host_admin::updates::{Build, Candidate, UpdatePhase};
    #[test]
    fn host_update_claims_survive_reload_and_exclude_concurrent_updates() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let settings = store
                .save_host_update_settings(&UpdateSettings::default())
                .await
                .unwrap();
            assert_eq!(settings.revision, 1);
            assert!(
                store
                    .save_host_update_settings(&UpdateSettings::default())
                    .await
                    .is_err()
            );
            let mut job = UpdateJob {
                id: "a".repeat(32),
                candidate: Candidate {
                    digest: format!("sha256:{}", "b".repeat(64)),
                    architecture: "amd64".into(),
                    build: Build {
                        version: "1.0.0-alpha.6".into(),
                        commit: "c".repeat(40),
                        schema: 58,
                    },
                },
                phase: UpdatePhase::Starting,
                detail: "Starting".into(),
                settings_revision: 1,
                connection_revision: 0,
                created_at: 1,
            };
            store.claim_host_update(&job).await.unwrap();
            assert_eq!(store.host_update_job().await.unwrap(), Some(job.clone()));
            assert!(store.claim_host_update(&job).await.is_err());
            assert!(store.save_host_update_settings(&settings).await.is_err());
            job.phase = UpdatePhase::Failed;
            store.save_host_update_job(&job).await.unwrap();
            let saved = store.save_host_update_settings(&settings).await.unwrap();
            assert!(store.claim_host_update(&job).await.is_err()); // Old config revision.
            job.phase = UpdatePhase::Starting;
            job.settings_revision = saved.revision;
            let mut connection = store.host_connection().await.unwrap();
            connection.enabled = false;
            store.save_host_connection(&connection).await.unwrap();
            assert!(store.claim_host_update(&job).await.is_err()); // SSH changed after inspection.
        });
    }
}
