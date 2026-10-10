//! Database transport for the shared plugin installation policy.
use super::*;
use openwebide_core::plugins::{
    PluginError, PluginInstallation, PluginPackage, PluginSource, RecordPlugin, record_installation,
};
const KEY: &str = "plugin_installations";

impl<D: Db> Store<D> {
    pub async fn plugin_installations(
        &self,
        user: UserId,
    ) -> Result<Vec<PluginInstallation>, StorageError> {
        self.get_user_setting(user, KEY).await?.map_or_else(
            || Ok(Vec::new()),
            |json| serde_json::from_str(&json).map_err(|error| StorageError::Db(error.to_string())),
        )
    }
    pub async fn record_plugin(
        &self,
        user: UserId,
        request: &RecordPlugin,
        now: i64,
    ) -> Result<Vec<PluginInstallation>, StorageError> {
        self.db
            .transaction(|tx| async move {
                Store::new(tx)
                    .record_plugin_in_transaction(user, request, now)
                    .await
            })
            .await
    }
    async fn record_plugin_in_transaction(
        &self,
        user: UserId,
        request: &RecordPlugin,
        now: i64,
    ) -> Result<Vec<PluginInstallation>, StorageError> {
        let current = self.plugin_installations(user).await?;
        let next = record_installation(current, request, now).map_err(|error| match error {
            PluginError::Conflict(message) => StorageError::Conflict(message),
            PluginError::Invalid(message) => StorageError::InvalidRequest(message),
            PluginError::Host(message) => StorageError::Db(message),
        })?;
        let json =
            serde_json::to_string(&next).map_err(|error| StorageError::Db(error.to_string()))?;
        self.set_user_setting(user, KEY, &json).await?;
        if let Some(package) = &request.package {
            self.db.execute("INSERT INTO plugin_defaults(user_id,repository,path,package) VALUES(?,?,?,?) ON CONFLICT(user_id,repository,path) DO UPDATE SET package=excluded.package", &[
                DbValue::Int(user.get()), DbValue::Text(request.prepared.source.repository.clone()), DbValue::Text(request.prepared.source.path.clone()),
                DbValue::Text(serde_json::to_string(package).map_err(|error| StorageError::Db(error.to_string()))?),
            ]).await?;
            let projects = self
                .db
                .execute(
                    "SELECT id FROM projects WHERE user_id=?",
                    &[DbValue::Int(user.get())],
                )
                .await?;
            for row in projects.rows {
                self.apply_plugin_default(user, row.get_int(0)?, package, now)
                    .await?;
            }
        }
        if !next
            .iter()
            .find(|entry| {
                entry.prepared.source.repository == request.prepared.source.repository
                    && entry.prepared.source.path == request.prepared.source.path
            })
            .is_some_and(|entry| entry.default_enabled)
        {
            self.db
                .execute(
                    "DELETE FROM plugin_defaults WHERE user_id=? AND repository=? AND path=?",
                    &[
                        DbValue::Int(user.get()),
                        DbValue::Text(request.prepared.source.repository.clone()),
                        DbValue::Text(request.prepared.source.path.clone()),
                    ],
                )
                .await?;
        }
        let installation = next
            .iter()
            .find(|entry| {
                entry.prepared.source.repository == request.prepared.source.repository
                    && entry.prepared.source.path == request.prepared.source.path
            })
            .ok_or_else(|| StorageError::Conflict("Plugin installation changed".into()))?;
        self.sync_plugin_background(
            user,
            None,
            &installation.prepared,
            installation.default_enabled,
            now,
        )
        .await?;
        self.acknowledge_bundled_plugin(user, &request.prepared.source)
            .await?;
        Ok(next)
    }
    async fn acknowledged_bundled_plugins(
        &self,
        user: UserId,
    ) -> Result<Vec<PluginSource>, StorageError> {
        self.get_user_setting(user, "bundled_plugins_initialized")
            .await?
            .map_or_else(
                || Ok(Vec::new()),
                |json| {
                    serde_json::from_str(&json).map_err(|error| StorageError::Db(error.to_string()))
                },
            )
    }
    async fn acknowledge_bundled_plugin(
        &self,
        user: UserId,
        source: &PluginSource,
    ) -> Result<(), StorageError> {
        if !openwebide_core::plugins::bundled_plugin_sources()
            .iter()
            .any(|default| default.repository == source.repository && default.path == source.path)
        {
            return Ok(());
        }
        let mut applied = self.acknowledged_bundled_plugins(user).await?;
        if !applied
            .iter()
            .any(|entry| entry.repository == source.repository && entry.path == source.path)
        {
            applied.push(source.clone());
            self.set_user_setting(
                user,
                "bundled_plugins_initialized",
                &serde_json::to_string(&applied)
                    .map_err(|error| StorageError::Db(error.to_string()))?,
            )
            .await?;
        }
        Ok(())
    }
    pub async fn pending_bundled_plugins(
        &self,
        user: UserId,
    ) -> Result<Vec<PluginSource>, StorageError> {
        let applied = self.acknowledged_bundled_plugins(user).await?;
        let installed = self.plugin_installations(user).await?;
        Ok(openwebide_core::plugins::bundled_plugin_sources()
            .into_iter()
            .filter(|source| {
                !applied
                    .iter()
                    .any(|entry| entry.repository == source.repository && entry.path == source.path)
                    && !installed.iter().any(|entry| {
                        entry.prepared.source.repository == source.repository
                            && entry.prepared.source.path == source.path
                    })
            })
            .collect())
    }
    /// Recheck inside one transaction so concurrent initialization/removal cannot restore opt-outs.
    pub async fn initialize_bundled_plugins(
        &self,
        user: UserId,
        packages: &[PluginPackage],
        now: i64,
    ) -> Result<(), StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let pending = store.pending_bundled_plugins(user).await?;
                for source in pending {
                    let package = packages
                        .iter()
                        .find(|package| package.prepared.source == source)
                        .ok_or_else(|| {
                            StorageError::InvalidRequest("Missing bundled plugin package".into())
                        })?;
                    store
                        .record_plugin_in_transaction(
                            user,
                            &RecordPlugin {
                                approved_capabilities: Vec::new(),
                                prepared: package.prepared.clone(),
                                package: Some(Box::new(package.clone())),
                                revision: None,
                                update_policy: None,
                            },
                            now,
                        )
                        .await?;
                }
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::plugins::testing::receipt;
    #[test]
    fn installation_records_are_user_owned_atomic_idempotent_and_revision_checked() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let owner = store
                .insert_user("owner", "hash", UserRole::Admin, 0)
                .await
                .unwrap();
            let other = store
                .insert_user("other", "hash", UserRole::User, 0)
                .await
                .unwrap();
            let request = RecordPlugin {
                approved_capabilities: Vec::new(),
                update_policy: None,
                package: None,
                prepared: receipt(),
                revision: None,
            };
            let entries = store.record_plugin(owner.id, &request, 1).await.unwrap();
            assert_eq!(entries.len(), 1);
            assert!(
                store
                    .plugin_installations(other.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                store.record_plugin(owner.id, &request, 2).await.unwrap(),
                entries
            );
            let mut changed = request.clone();
            changed.prepared.source.commit = "b".repeat(40);
            assert!(matches!(
                store.record_plugin(owner.id, &changed, 3).await,
                Err(StorageError::Conflict(_))
            ));
            assert_eq!(store.plugin_installations(owner.id).await.unwrap(), entries);
            changed.revision = Some(entries[0].revision);
            let entries = store.record_plugin(owner.id, &changed, 4).await.unwrap();
            assert_eq!(entries[0].prepared.source.commit, "b".repeat(40));
            assert!(matches!(
                store.record_plugin(owner.id, &changed, 5).await,
                Err(StorageError::Conflict(_))
            ));
        });
    }
}

use openwebide_core::plugins::{ProjectPlugin, ProjectPluginCommand, RemovePlugin, marketplace::*};
fn plugin_error(error: PluginError) -> StorageError {
    match error {
        PluginError::Conflict(message) => StorageError::Conflict(message),
        PluginError::Invalid(message) => StorageError::InvalidRequest(message),
        PluginError::Host(message) => StorageError::Db(message),
    }
}
impl<D: Db> Store<D> {
    pub async fn plugin_marketplaces(
        &self,
        user: UserId,
    ) -> Result<MarketplaceSettings, StorageError> {
        self.get_user_setting(user, "plugin_marketplaces")
            .await?
            .map_or_else(
                || Ok(MarketplaceSettings::default()),
                |value| {
                    serde_json::from_str::<MarketplaceSettings>(&value)
                        .map(MarketplaceSettings::ensure_official)
                        .map_err(|error| StorageError::Db(error.to_string()))
                },
            )
    }
    pub async fn save_plugin_marketplaces(
        &self,
        user: UserId,
        request: &SaveMarketplaces,
    ) -> Result<MarketplaceSettings, StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let next = save_sources(store.plugin_marketplaces(user).await?, request)
                    .map_err(plugin_error)?;
                store
                    .set_user_setting(
                        user,
                        "plugin_marketplaces",
                        &serde_json::to_string(&next)
                            .map_err(|error| StorageError::Db(error.to_string()))?,
                    )
                    .await?;
                Ok(next)
            })
            .await
    }
    pub async fn cache_plugin_marketplaces(
        &self,
        user: UserId,
        revision: i64,
        catalogs: Vec<CachedMarketplace>,
    ) -> Result<MarketplaceSettings, StorageError> {
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let next =
                    cache_catalogs(store.plugin_marketplaces(user).await?, revision, catalogs)
                        .map_err(plugin_error)?;
                store
                    .set_user_setting(
                        user,
                        "plugin_marketplaces",
                        &serde_json::to_string(&next)
                            .map_err(|error| StorageError::Db(error.to_string()))?,
                    )
                    .await?;
                Ok(next)
            })
            .await
    }
    pub async fn project_plugins(
        &self,
        user: UserId,
        project: i64,
    ) -> Result<Vec<ProjectPlugin>, StorageError> {
        self.get_project(project, user).await?;
        let result = self.db.execute("SELECT id,revision,enabled,prepared FROM project_plugins WHERE user_id=? AND project_id=? ORDER BY repository,path",&[DbValue::Int(user.get()),DbValue::Int(project)]).await?;
        result
            .rows
            .iter()
            .map(|row| {
                Ok(ProjectPlugin {
                    id: row.get_int(0)?,
                    revision: row.get_int(1)?,
                    enabled: row.get_int(2)? != 0,
                    prepared: serde_json::from_str(row.get_text(3)?)
                        .map_err(|error| StorageError::Db(error.to_string()))?,
                })
            })
            .collect()
    }
    pub async fn project_plugin_command(
        &self,
        user: UserId,
        project: i64,
        command: &ProjectPluginCommand,
        now: i64,
    ) -> Result<Vec<ProjectPlugin>, StorageError> {
        self.db
            .transaction(|tx| async move {
                Store::new(tx)
                    .project_plugin_command_in_transaction(user, project, command, now)
                    .await
            })
            .await
    }
    async fn project_plugin_command_in_transaction(
        &self,
        user: UserId,
        project: i64,
        command: &ProjectPluginCommand,
        now: i64,
    ) -> Result<Vec<ProjectPlugin>, StorageError> {
        let current = self.project_plugins(user, project).await?;
        match command {
            ProjectPluginCommand::Enable {
                package,
                installation_revision,
                revision,
            } => {
                package.validate().map_err(plugin_error)?;
                package
                    .prepared
                    .manifest
                    .validate_activation()
                    .map_err(plugin_error)?;
                let receipt = &package.prepared;
                let installations = self.plugin_installations(user).await?;
                let installation = installations.iter().find(|entry|entry.prepared.source==receipt.source && entry.prepared.manifest==receipt.manifest && entry.prepared.digest==receipt.digest && entry.hosts.contains(&receipt.host_id))
                        .ok_or_else(||StorageError::Conflict("Prepare this installed version on the project host before enabling it.".into()))?;
                if installation.revision != *installation_revision {
                    return Err(StorageError::Conflict(
                        "Installed plugin changed. Refresh before enabling it.".into(),
                    ));
                }
                let previous = current.iter().find(|entry| {
                    entry.prepared.source.repository == receipt.source.repository
                        && entry.prepared.source.path == receipt.source.path
                });
                if previous.map(|entry| entry.revision) != *revision {
                    return Err(StorageError::Conflict(
                        "Project plugin changed. Refresh before enabling it.".into(),
                    ));
                }
                let existing_skills = self.project_skills(user, project).await?;
                let previous_ids = if let Some(previous) = previous {
                    self.db
                        .execute(
                            "SELECT skill_id FROM project_plugin_skills WHERE plugin_id=?",
                            &[DbValue::Int(previous.id)],
                        )
                        .await?
                        .rows
                        .iter()
                        .map(|row| row.get_int(0))
                        .collect::<Result<Vec<_>, _>>()?
                } else {
                    Vec::new()
                };
                let unrelated = existing_skills
                    .entries
                    .iter()
                    .filter(|entry| !previous_ids.contains(&entry.id))
                    .collect::<Vec<_>>();
                if unrelated.len() + package.skills.len() > openwebide_core::skills::MAX_SKILLS {
                    return Err(StorageError::InvalidRequest(
                        "This project would exceed 100 skills.".into(),
                    ));
                }
                if package
                    .skills
                    .iter()
                    .any(|draft| unrelated.iter().any(|entry| entry.draft.name == draft.name))
                {
                    return Err(StorageError::Conflict("A contributed skill name already exists in this project. Remove or rename that skill first.".into()));
                }
                let json = serde_json::to_string(receipt)
                    .map_err(|error| StorageError::Db(error.to_string()))?;
                let id = if let Some(previous) = previous {
                    self.db.execute("UPDATE project_plugins SET revision=revision+1,enabled=1,prepared=? WHERE id=?",&[DbValue::Text(json),DbValue::Int(previous.id)]).await?;
                    previous.id
                } else {
                    self.db.execute("INSERT INTO project_plugins(user_id,project_id,repository,path,prepared) VALUES(?,?,?,?,?)",&[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(receipt.source.repository.clone()),DbValue::Text(receipt.source.path.clone()),DbValue::Text(json)]).await?.last_insert_rowid
                };
                for old in existing_skills
                    .entries
                    .iter()
                    .filter(|entry| previous_ids.contains(&entry.id))
                {
                    if !package
                        .skills
                        .iter()
                        .any(|draft| draft.name == old.draft.name)
                    {
                        self.db
                            .execute(
                                "DELETE FROM project_skills WHERE id=?",
                                &[DbValue::Int(old.id)],
                            )
                            .await?;
                    }
                }
                for draft in &package.skills {
                    let mut draft = draft.clone();
                    draft.enabled = true;
                    let json = serde_json::to_string(&draft)
                        .map_err(|error| StorageError::Db(error.to_string()))?;
                    let skill = if let Some(old) = existing_skills.entries.iter().find(|entry| {
                        previous_ids.contains(&entry.id) && entry.draft.name == draft.name
                    }) {
                        self.db.execute("UPDATE project_skills SET draft=?,revision=revision+1,updated_at=? WHERE id=?",&[DbValue::Text(json),DbValue::Int(now),DbValue::Int(old.id)]).await?;
                        old.id
                    } else {
                        self.db.execute("INSERT INTO project_skills(user_id,project_id,name,draft,updated_at) VALUES(?,?,?,?,?)",&[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(draft.name.clone()),DbValue::Text(json),DbValue::Int(now)]).await?.last_insert_rowid
                    };
                    self.db.execute("INSERT OR IGNORE INTO project_plugin_skills(plugin_id,skill_id) VALUES(?,?)",&[DbValue::Int(id),DbValue::Int(skill)]).await?;
                }
            }
            ProjectPluginCommand::Disable { id, revision } => {
                let entry = current
                    .iter()
                    .find(|entry| entry.id == *id && entry.revision == *revision)
                    .ok_or_else(|| {
                        StorageError::Conflict(
                            "Project plugin changed. Refresh before disabling it.".into(),
                        )
                    })?;
                let skills = self.project_skills(user, project).await?;
                let ids = self
                    .db
                    .execute(
                        "SELECT skill_id FROM project_plugin_skills WHERE plugin_id=?",
                        &[DbValue::Int(entry.id)],
                    )
                    .await?
                    .rows
                    .iter()
                    .map(|row| row.get_int(0))
                    .collect::<Result<Vec<_>, _>>()?;
                for entry in skills
                    .entries
                    .into_iter()
                    .filter(|entry| ids.contains(&entry.id))
                {
                    let mut draft = entry.draft;
                    draft.enabled = false;
                    self.db.execute("UPDATE project_skills SET draft=?,revision=revision+1,updated_at=? WHERE id=?",&[DbValue::Text(serde_json::to_string(&draft).map_err(|error|StorageError::Db(error.to_string()))?),DbValue::Int(now),DbValue::Int(entry.id)]).await?;
                }
                self.db
                    .execute(
                        "UPDATE project_plugins SET enabled=0,revision=revision+1 WHERE id=?",
                        &[DbValue::Int(*id)],
                    )
                    .await?;
            }
        }
        let bindings = self.project_plugins(user, project).await?;
        for binding in &bindings {
            self.sync_plugin_background(
                user,
                Some(project),
                &binding.prepared,
                binding.enabled,
                now,
            )
            .await?;
        }
        Ok(bindings)
    }
    pub(super) async fn inherit_plugin_defaults(
        &self,
        user: UserId,
        project: i64,
        now: i64,
    ) -> Result<(), StorageError> {
        let defaults = self
            .db
            .execute(
                "SELECT package FROM plugin_defaults WHERE user_id=?",
                &[DbValue::Int(user.get())],
            )
            .await?;
        for row in defaults.rows {
            let package: openwebide_core::plugins::PluginPackage =
                serde_json::from_str(row.get_text(0)?)
                    .map_err(|error| StorageError::Db(error.to_string()))?;
            self.apply_plugin_default(user, project, &package, now)
                .await?;
        }
        Ok(())
    }
    async fn apply_plugin_default(
        &self,
        user: UserId,
        project: i64,
        package: &openwebide_core::plugins::PluginPackage,
        now: i64,
    ) -> Result<(), StorageError> {
        let bindings = self.project_plugins(user, project).await?;
        let previous = bindings.iter().find(|binding| {
            binding.prepared.source.repository == package.prepared.source.repository
                && binding.prepared.source.path == package.prepared.source.path
        });
        if previous.is_some_and(|binding| {
            !binding.enabled
                || (binding.prepared.source == package.prepared.source
                    && binding.prepared.digest == package.prepared.digest)
        }) {
            return Ok(());
        }
        let installation = self
            .plugin_installations(user)
            .await?
            .into_iter()
            .find(|entry| entry.prepared.source == package.prepared.source)
            .ok_or_else(|| StorageError::Conflict("Plugin installation changed.".into()))?;
        self.project_plugin_command_in_transaction(
            user,
            project,
            &ProjectPluginCommand::Enable {
                package: Box::new(package.clone()),
                installation_revision: installation.revision,
                revision: previous.map(|binding| binding.revision),
            },
            now,
        )
        .await?;
        Ok(())
    }
    pub async fn remove_plugin(
        &self,
        user: UserId,
        request: &RemovePlugin,
    ) -> Result<Vec<PluginInstallation>, StorageError> {
        request.source.validate().map_err(plugin_error)?;
        self.db.transaction(|tx|async move {
            let store=Store::new(tx);
            let mut entries=store.plugin_installations(user).await?;
            let entry=entries.iter().find(|entry|entry.prepared.source.repository==request.source.repository && entry.prepared.source.path==request.source.path && entry.revision==request.revision)
                .ok_or_else(||StorageError::Conflict("Plugin installation changed. Refresh before removing it.".into()))?;
            let scope=[DbValue::Int(user.get()),DbValue::Text(entry.prepared.source.repository.clone()),DbValue::Text(entry.prepared.source.path.clone())];
            store.db.execute("DELETE FROM project_skills WHERE id IN (SELECT s.skill_id FROM project_plugin_skills s JOIN project_plugins p ON p.id=s.plugin_id WHERE p.user_id=? AND p.repository=? AND p.path=?)",&scope).await?;
            store.db.execute("DELETE FROM project_plugins WHERE user_id=? AND repository=? AND path=?",&scope).await?;
            store.db.execute("DELETE FROM plugin_defaults WHERE user_id=? AND repository=? AND path=?",&scope).await?;
            store.db.execute("DELETE FROM plugin_background WHERE user_id=? AND plugin=?", &[DbValue::Int(user.get()),DbValue::Text(entry.prepared.storage_namespace())]).await?;
            store.acknowledge_bundled_plugin(user, &request.source).await?;
            entries.retain(|entry|entry.prepared.source.repository!=request.source.repository || entry.prepared.source.path!=request.source.path);
            store.set_user_setting(user,KEY,&serde_json::to_string(&entries).map_err(|error|StorageError::Db(error.to_string()))?).await?;
            Ok(entries)
        }).await
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::{
        NewProject, SkillCommand, WorkspaceMode,
        plugins::PluginPackage,
        plugins::testing::{catalog, package},
    };
    #[test]
    fn retired_activation_is_atomic_and_keeps_preferences_and_data_in_both_modes() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("owner", "hash", UserRole::Admin, 0)
                    .await
                    .unwrap()
                    .id;
                let project = store
                    .create_project(
                        &NewProject {
                            name: "project".into(),
                            mode,
                            path: None,
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                let mut package = package();
                package.prepared.manifest.compatibility.plugin_api = 2;
                package.prepared.manifest.contributions.tool_groups =
                    vec![openwebide_core::plugins::PluginToolGroup::SkillAuthoring];
                let installations = vec![PluginInstallation {
                    prepared: package.prepared.clone(),
                    revision: 1,
                    hosts: vec![package.prepared.host_id.clone()],
                    installed_at: 0,
                    default_enabled: true,
                    update_policy: openwebide_core::plugins::PluginUpdatePolicy::Notify,
                }];
                store
                    .set_user_setting(user, KEY, &serde_json::to_string(&installations).unwrap())
                    .await
                    .unwrap();
                let skills = store
                    .skill_command(
                        user,
                        project,
                        &SkillCommand::Create {
                            draft: package.skills[0].clone(),
                        },
                        false,
                        1,
                    )
                    .await
                    .unwrap();
                let error = store
                    .project_plugin_command(
                        user,
                        project,
                        &ProjectPluginCommand::Enable {
                            package: Box::new(package.clone()),
                            installation_revision: 1,
                            revision: None,
                        },
                        2,
                    )
                    .await
                    .unwrap_err();
                assert!(
                    matches!(error, StorageError::InvalidRequest(ref text) if text.contains("Update required"))
                );
                assert_eq!(store.project_skills(user, project).await.unwrap(), skills);
                assert!(
                    store
                        .project_plugins(user, project)
                        .await
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(
                    store.plugin_installations(user).await.unwrap(),
                    installations
                );
                let entries = store
                    .record_plugin(
                        user,
                        &RecordPlugin {
                            prepared: package.prepared,
                            revision: Some(1),
                            package: None,
                            update_policy: Some(openwebide_core::plugins::PluginUpdatePolicy::Off),
                            approved_capabilities: Vec::new(),
                        },
                        3,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    entries[0].update_policy,
                    openwebide_core::plugins::PluginUpdatePolicy::Off
                );
                assert_eq!(entries[0].prepared, installations[0].prepared);
                assert_eq!(store.project_skills(user, project).await.unwrap(), skills);
            }
        });
    }
    #[test]
    fn package_skills_activate_update_disable_and_remove_through_shared_project_and_session_contract()
     {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                store.migrate().await.unwrap();
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("owner", "hash", UserRole::Admin, 0)
                    .await
                    .unwrap()
                    .id;
                let other = store
                    .insert_user("other", "hash", UserRole::User, 0)
                    .await
                    .unwrap()
                    .id;
                let project = store
                    .create_project(
                        &NewProject {
                            name: "project".into(),
                            mode,
                            path: Some("project".into()),
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                let session = store
                    .create_session("session", None, None, Some(project), user, 0)
                    .await
                    .unwrap()
                    .id;
                let package = package();
                let entries = store
                    .record_plugin(
                        user,
                        &RecordPlugin {
                            approved_capabilities: Vec::new(),
                            update_policy: None,
                            package: None,
                            prepared: package.prepared.clone(),
                            revision: None,
                        },
                        1,
                    )
                    .await
                    .unwrap();
                assert!(
                    store
                        .project_skills(user, project)
                        .await
                        .unwrap()
                        .entries
                        .is_empty()
                );
                let command = ProjectPluginCommand::Enable {
                    package: Box::new(package.clone()),
                    installation_revision: entries[0].revision,
                    revision: None,
                };
                assert!(
                    store
                        .project_plugin_command(other, project, &command, 2)
                        .await
                        .is_err()
                );
                let binding = store
                    .project_plugin_command(user, project, &command, 2)
                    .await
                    .unwrap()
                    .remove(0);
                assert!(
                    store
                        .project_plugin_command(user, project, &command, 2)
                        .await
                        .is_err()
                );
                let second = store
                    .create_project(
                        &NewProject {
                            name: "second".into(),
                            mode,
                            path: None,
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap()
                    .id;
                store
                    .project_plugin_command(user, second, &command, 2)
                    .await
                    .unwrap();
                let skills = store.session_skills(user, session).await.unwrap();
                let skill = skills.entries[0].clone();
                assert_eq!(skill.draft, package.skills[0]);
                assert!(skill.plugin.is_some());
                let read = store
                    .skill_command(
                        user,
                        project,
                        &SkillCommand::Read {
                            id: skill.id,
                            resource: None,
                        },
                        true,
                        3,
                    )
                    .await
                    .unwrap();
                assert_eq!(read.entries[0], skill);
                for command in [
                    SkillCommand::Delete {
                        id: skill.id,
                        revision: skill.revision,
                    },
                    SkillCommand::Update {
                        id: skill.id,
                        revision: skill.revision,
                        draft: skill.draft.clone(),
                    },
                ] {
                    assert!(
                        store
                            .skill_command(user, project, &command, false, 3)
                            .await
                            .is_err()
                    );
                }
                let mut updated: PluginPackage = package.clone();
                updated.prepared.source.commit = "b".repeat(40);
                updated.prepared.digest = "d".repeat(64);
                updated.prepared.manifest.version = "0.2.0".into();
                updated.skills[0].instructions = "Updated package instructions".into();
                let entries = store
                    .record_plugin(
                        user,
                        &RecordPlugin {
                            approved_capabilities: Vec::new(),
                            update_policy: None,
                            package: None,
                            prepared: updated.prepared.clone(),
                            revision: Some(entries[0].revision),
                        },
                        4,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    store.session_skills(user, session).await.unwrap().entries[0]
                        .draft
                        .instructions,
                    skill.draft.instructions
                );
                let binding = store
                    .project_plugin_command(
                        user,
                        project,
                        &ProjectPluginCommand::Enable {
                            package: Box::new(updated.clone()),
                            installation_revision: entries[0].revision,
                            revision: Some(binding.revision),
                        },
                        5,
                    )
                    .await
                    .unwrap()
                    .remove(0);
                let skills = store.project_skills(user, project).await.unwrap();
                assert_eq!(skills.entries[0].id, skill.id);
                assert_eq!(
                    skills.entries[0].draft.instructions,
                    updated.skills[0].instructions
                );
                assert_eq!(skills.entries[0].plugin.as_ref().unwrap().version, "0.2.0");
                // Roll back explicitly; the current project remains pinned until activation.
                let entries = store
                    .record_plugin(
                        user,
                        &RecordPlugin {
                            approved_capabilities: Vec::new(),
                            update_policy: None,
                            package: None,
                            prepared: package.prepared.clone(),
                            revision: Some(entries[0].revision),
                        },
                        6,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    store.project_skills(user, project).await.unwrap().entries[0]
                        .draft
                        .instructions,
                    updated.skills[0].instructions
                );
                let binding = store
                    .project_plugin_command(
                        user,
                        project,
                        &ProjectPluginCommand::Enable {
                            package: Box::new(package.clone()),
                            installation_revision: entries[0].revision,
                            revision: Some(binding.revision),
                        },
                        6,
                    )
                    .await
                    .unwrap()
                    .remove(0);
                assert_eq!(
                    store.project_skills(user, project).await.unwrap().entries[0]
                        .draft
                        .instructions,
                    package.skills[0].instructions
                );
                let binding = store
                    .project_plugin_command(
                        user,
                        project,
                        &ProjectPluginCommand::Disable {
                            id: binding.id,
                            revision: binding.revision,
                        },
                        6,
                    )
                    .await
                    .unwrap()
                    .remove(0);
                assert!(!binding.enabled);
                assert!(
                    !store.session_skills(user, session).await.unwrap().entries[0]
                        .draft
                        .enabled
                );
                assert!(
                    store
                        .skill_command(
                            user,
                            project,
                            &SkillCommand::Read {
                                id: skill.id,
                                resource: None
                            },
                            true,
                            7
                        )
                        .await
                        .is_err()
                );
                let mut draft = package.skills[0].clone();
                draft.name = "personal-review".into();
                store
                    .skill_command(user, project, &SkillCommand::Create { draft }, false, 7)
                    .await
                    .unwrap();
                assert!(
                    store
                        .remove_plugin(
                            user,
                            &RemovePlugin {
                                source: package.prepared.source.clone(),
                                revision: entries[0].revision - 1
                            }
                        )
                        .await
                        .is_err()
                );
                store
                    .remove_plugin(
                        user,
                        &RemovePlugin {
                            source: package.prepared.source,
                            revision: entries[0].revision,
                        },
                    )
                    .await
                    .unwrap();
                assert!(
                    store
                        .project_plugins(user, second)
                        .await
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    store
                        .project_skills(user, second)
                        .await
                        .unwrap()
                        .entries
                        .is_empty()
                );
                let skills = store.project_skills(user, project).await.unwrap();
                assert_eq!(skills.entries.len(), 1);
                assert_eq!(skills.entries[0].draft.name, "personal-review");
                assert!(
                    store
                        .project_plugins(user, project)
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        });
    }
    #[test]
    fn activation_conflicts_are_atomic_and_marketplace_settings_are_user_owned() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let user = store
                .insert_user("owner", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let other = store
                .insert_user("other", "hash", UserRole::User, 0)
                .await
                .unwrap()
                .id;
            let project = store
                .create_project(
                    &NewProject {
                        name: "project".into(),
                        mode: WorkspaceMode::Local,
                        path: None,
                    },
                    user,
                    0,
                )
                .await
                .unwrap()
                .id;
            let package = package();
            let entries = store
                .record_plugin(
                    user,
                    &RecordPlugin {
                        approved_capabilities: Vec::new(),
                        update_policy: None,
                        package: None,
                        prepared: package.prepared.clone(),
                        revision: None,
                    },
                    1,
                )
                .await
                .unwrap();
            store
                .skill_command(
                    user,
                    project,
                    &SkillCommand::Create {
                        draft: package.skills[0].clone(),
                    },
                    false,
                    2,
                )
                .await
                .unwrap();
            assert!(matches!(
                store
                    .project_plugin_command(
                        user,
                        project,
                        &ProjectPluginCommand::Enable {
                            package: Box::new(package.clone()),
                            installation_revision: entries[0].revision,
                            revision: None
                        },
                        3
                    )
                    .await,
                Err(StorageError::Conflict(_))
            ));
            assert!(
                store
                    .project_plugins(user, project)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                store.project_skills(user, project).await.unwrap().entries[0]
                    .plugin
                    .is_none()
            );
            let legacy = MarketplaceSettings {
                revision: 7,
                sources: Vec::new(),
                catalogs: Vec::new(),
            };
            store
                .set_user_setting(
                    other,
                    "plugin_marketplaces",
                    &serde_json::to_string(&legacy).unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                store.plugin_marketplaces(other).await.unwrap(),
                legacy.ensure_official()
            );
            store
                .set_user_setting(
                    other,
                    "plugin_marketplaces",
                    &serde_json::to_string(&MarketplaceSettings::default()).unwrap(),
                )
                .await
                .unwrap();
            assert!(matches!(
                store
                    .save_plugin_marketplaces(
                        user,
                        &SaveMarketplaces {
                            revision: 0,
                            sources: Vec::new()
                        }
                    )
                    .await,
                Err(StorageError::InvalidRequest(_))
            ));
            let cache = catalog();
            let settings = store
                .save_plugin_marketplaces(
                    user,
                    &SaveMarketplaces {
                        revision: 0,
                        sources: vec![MarketplaceSource::official(), cache.source.clone()],
                    },
                )
                .await
                .unwrap();
            let settings = store
                .cache_plugin_marketplaces(user, settings.revision, vec![cache.clone()])
                .await
                .unwrap();
            let retained = store
                .cache_plugin_marketplaces(user, settings.revision, Vec::new())
                .await
                .unwrap();
            assert_eq!(retained.catalogs, vec![cache]);
            assert_eq!(
                store.plugin_marketplaces(other).await.unwrap(),
                MarketplaceSettings::default()
            );
            assert!(
                store
                    .save_plugin_marketplaces(
                        user,
                        &SaveMarketplaces {
                            revision: settings.revision,
                            sources: Vec::new()
                        }
                    )
                    .await
                    .is_err()
            );
        });
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::{NewProject, WorkspaceMode, plugins::testing::package};
    #[test]
    fn installation_defaults_apply_to_existing_and_future_projects_without_reviving_disable_overrides()
     {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                store.migrate().await.unwrap();
                let user = store
                    .insert_user("owner", "hash", UserRole::User, 0)
                    .await
                    .unwrap()
                    .id;
                let other = store
                    .insert_user("other", "hash", UserRole::User, 0)
                    .await
                    .unwrap()
                    .id;
                let project = store
                    .create_project(
                        &NewProject {
                            name: "first".into(),
                            mode,
                            path: None,
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap();
                let other_project = store
                    .create_project(
                        &NewProject {
                            name: "other".into(),
                            mode,
                            path: None,
                        },
                        other,
                        0,
                    )
                    .await
                    .unwrap();
                let package = package();
                let entries = store
                    .record_plugin(
                        user,
                        &RecordPlugin {
                            approved_capabilities: Vec::new(),
                            update_policy: None,
                            prepared: package.prepared.clone(),
                            revision: None,
                            package: Some(Box::new(package.clone())),
                        },
                        1,
                    )
                    .await
                    .unwrap();
                assert!(entries[0].default_enabled);
                assert!(
                    store
                        .project_skills(user, project.id)
                        .await
                        .unwrap()
                        .entries[0]
                        .draft
                        .enabled
                );
                assert!(
                    store
                        .project_plugins(other, other_project.id)
                        .await
                        .unwrap()
                        .is_empty()
                );
                let binding = store
                    .project_plugins(user, project.id)
                    .await
                    .unwrap()
                    .remove(0);
                store
                    .project_plugin_command(
                        user,
                        project.id,
                        &ProjectPluginCommand::Disable {
                            id: binding.id,
                            revision: binding.revision,
                        },
                        2,
                    )
                    .await
                    .unwrap();
                let mut updated = package.clone();
                updated.prepared.source.commit = "b".repeat(40);
                updated.prepared.manifest.version = "0.2.0".into();
                updated.prepared.digest = "d".repeat(64);
                updated.skills[0].instructions = "New instructions".into();
                let entries = store
                    .record_plugin(
                        user,
                        &RecordPlugin {
                            approved_capabilities: Vec::new(),
                            update_policy: None,
                            prepared: updated.prepared.clone(),
                            revision: Some(entries[0].revision),
                            package: Some(Box::new(updated.clone())),
                        },
                        3,
                    )
                    .await
                    .unwrap();
                assert!(!store.project_plugins(user, project.id).await.unwrap()[0].enabled);
                assert!(
                    !store
                        .project_skills(user, project.id)
                        .await
                        .unwrap()
                        .entries[0]
                        .draft
                        .enabled
                );
                let future = store
                    .create_project(
                        &NewProject {
                            name: "future".into(),
                            mode,
                            path: None,
                        },
                        user,
                        4,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    store.project_skills(user, future.id).await.unwrap().entries[0]
                        .draft
                        .instructions,
                    "New instructions"
                );
                store
                    .remove_plugin(
                        user,
                        &RemovePlugin {
                            source: updated.prepared.source,
                            revision: entries[0].revision,
                        },
                    )
                    .await
                    .unwrap();
                assert!(
                    store
                        .project_plugins(user, project.id)
                        .await
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    store
                        .project_skills(user, future.id)
                        .await
                        .unwrap()
                        .entries
                        .is_empty()
                );
                let fresh = store
                    .create_project(
                        &NewProject {
                            name: "fresh".into(),
                            mode,
                            path: None,
                        },
                        user,
                        5,
                    )
                    .await
                    .unwrap();
                assert!(
                    store
                        .project_plugins(user, fresh.id)
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        });
    }
}

#[cfg(test)]
mod default_conflict_tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::{NewProject, SkillCommand, WorkspaceMode, plugins::testing::package};
    #[test]
    fn conflicting_personal_content_rolls_back_defaults_and_all_project_activation() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let user = store
                .insert_user("owner", "hash", UserRole::User, 0)
                .await
                .unwrap()
                .id;
            let first = store
                .create_project(
                    &NewProject {
                        name: "local".into(),
                        mode: WorkspaceMode::Local,
                        path: None,
                    },
                    user,
                    0,
                )
                .await
                .unwrap();
            let second = store
                .create_project(
                    &NewProject {
                        name: "remote".into(),
                        mode: WorkspaceMode::Remote,
                        path: None,
                    },
                    user,
                    0,
                )
                .await
                .unwrap();
            let package = package();
            let mut personal = package.skills[0].clone();
            personal.instructions = "Personal content".into();
            store
                .skill_command(
                    user,
                    second.id,
                    &SkillCommand::Create {
                        draft: personal.clone(),
                    },
                    false,
                    0,
                )
                .await
                .unwrap();
            assert!(
                store
                    .record_plugin(
                        user,
                        &RecordPlugin {
                            approved_capabilities: Vec::new(),
                            prepared: package.prepared.clone(),
                            revision: None,
                            package: Some(Box::new(package)),
                            update_policy: None
                        },
                        1
                    )
                    .await
                    .is_err()
            );
            assert!(store.plugin_installations(user).await.unwrap().is_empty());
            assert!(
                store
                    .project_plugins(user, first.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                store
                    .project_skills(user, first.id)
                    .await
                    .unwrap()
                    .entries
                    .is_empty()
            );
            assert_eq!(
                store.project_skills(user, second.id).await.unwrap().entries[0].draft,
                personal
            );
            let future = store
                .create_project(
                    &NewProject {
                        name: "future".into(),
                        mode: WorkspaceMode::Local,
                        path: None,
                    },
                    user,
                    2,
                )
                .await
                .unwrap();
            assert!(
                store
                    .project_plugins(user, future.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
        });
    }
}

#[cfg(test)]
mod bundled_tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::{
        NewProject, WorkspaceMode,
        plugins::{
            PluginTool, PluginUpdatePolicy, RustPlugin, bundled_plugin_sources, testing::package,
        },
    };
    fn packages() -> Vec<PluginPackage> {
        bundled_plugin_sources()
            .into_iter()
            .map(|source| {
                let mut pkg = package();
                pkg.prepared.manifest.name = source.path.rsplit('/').next().unwrap().into();
                pkg.prepared.source = source;
                pkg.prepared.manifest.compatibility.plugin_api = 3;
                pkg.prepared.manifest.contributions.tools = vec![PluginTool {
                    name: format!("fixture_{}", pkg.prepared.manifest.name.replace('-', "_")),
                    description: "Fixture tool".into(),
                    parameters: serde_json::json!({"type":"object"}),
                    requires_approval: false,
                }];
                pkg.prepared.manifest.executable = Some(RustPlugin {
                    manifest: "Cargo.toml".into(),
                    library: "fixture_plugin".into(),
                    sdk_version: "0.1.0".into(),
                    capabilities: vec!["records".into()],
                });
                if pkg.prepared.manifest.name != "skill-authoring" {
                    pkg.prepared.manifest.contributions.skills.clear();
                    pkg.skills.clear();
                }
                pkg
            })
            .collect()
    }
    #[test]
    fn bundled_defaults_are_once_per_account_and_preserve_removals_versions_and_project_opt_outs() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                store.migrate().await.unwrap();
                let owner = store
                    .insert_user("owner", "hash", UserRole::Admin, 0)
                    .await
                    .unwrap()
                    .id;
                let other = store
                    .insert_user("other", "hash", UserRole::User, 0)
                    .await
                    .unwrap()
                    .id;
                let project = store
                    .create_project(
                        &NewProject {
                            name: "existing".into(),
                            mode,
                            path: None,
                        },
                        owner,
                        0,
                    )
                    .await
                    .unwrap();
                let packages = packages();
                // A failed partial batch cannot leave installed defaults or an initialized marker.
                assert!(
                    store
                        .initialize_bundled_plugins(owner, &packages[..3], 1)
                        .await
                        .is_err()
                );
                assert!(store.plugin_installations(owner).await.unwrap().is_empty());
                assert_eq!(store.pending_bundled_plugins(owner).await.unwrap().len(), 4);
                store
                    .initialize_bundled_plugins(owner, &packages, 1)
                    .await
                    .unwrap();
                let entries = store.plugin_installations(owner).await.unwrap();
                assert_eq!(entries.len(), 4);
                assert!(entries.iter().all(|entry| entry.default_enabled
                    && entry.update_policy == PluginUpdatePolicy::Notify));
                assert!(
                    store
                        .pending_bundled_plugins(owner)
                        .await
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(store.pending_bundled_plugins(other).await.unwrap().len(), 4);
                assert_eq!(
                    store
                        .project_plugins(owner, project.id)
                        .await
                        .unwrap()
                        .len(),
                    4
                );
                let binding = store
                    .project_plugins(owner, project.id)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|entry| entry.prepared.source != entries[0].prepared.source)
                    .unwrap();
                store
                    .project_plugin_command(
                        owner,
                        project.id,
                        &ProjectPluginCommand::Disable {
                            id: binding.id,
                            revision: binding.revision,
                        },
                        2,
                    )
                    .await
                    .unwrap();
                let first = entries[0].clone();
                store
                    .remove_plugin(
                        owner,
                        &RemovePlugin {
                            source: first.prepared.source.clone(),
                            revision: first.revision,
                        },
                    )
                    .await
                    .unwrap();
                store
                    .initialize_bundled_plugins(owner, &[], 3)
                    .await
                    .unwrap();
                assert_eq!(store.plugin_installations(owner).await.unwrap().len(), 3);
                assert!(
                    store
                        .pending_bundled_plugins(owner)
                        .await
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    store
                        .project_plugins(owner, project.id)
                        .await
                        .unwrap()
                        .iter()
                        .all(|entry| entry.prepared.source != first.prepared.source)
                );
                assert!(
                    !store
                        .project_plugins(owner, project.id)
                        .await
                        .unwrap()
                        .iter()
                        .find(|entry| entry.id == binding.id)
                        .unwrap()
                        .enabled
                );
                let future = store
                    .create_project(
                        &NewProject {
                            name: "future".into(),
                            mode,
                            path: None,
                        },
                        owner,
                        4,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    store.project_plugins(owner, future.id).await.unwrap().len(),
                    3
                );
                // An explicit newer installation is never replaced by the bundled baseline.
                let mut newer = packages[0].clone();
                newer.prepared.source.commit = "b".repeat(40);
                newer.prepared.manifest.version = "0.1.9".into();
                store
                    .record_plugin(
                        other,
                        &RecordPlugin {
                            approved_capabilities: Vec::new(),
                            prepared: newer.prepared.clone(),
                            package: Some(Box::new(newer)),
                            revision: None,
                            update_policy: Some(PluginUpdatePolicy::Off),
                        },
                        5,
                    )
                    .await
                    .unwrap();
                store
                    .initialize_bundled_plugins(other, &packages, 6)
                    .await
                    .unwrap();
                let installed = store.plugin_installations(other).await.unwrap();
                assert_eq!(installed.len(), 4);
                let preserved = installed
                    .iter()
                    .find(|entry| entry.prepared.source.path == "plugins/web")
                    .unwrap();
                assert_eq!(preserved.prepared.manifest.version, "0.1.9");
                assert_eq!(preserved.update_policy, PluginUpdatePolicy::Off);
            }
        });
    }
}
