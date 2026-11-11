//! Owned, fresh metadata for the shared read-only agent language workflow.
use super::*;
use openwebide_core::plugins::languages::{
    LanguageSettings,
    tools::{LanguageToolContext, LanguageToolScope},
};

impl<D: Db> Store<D> {
    pub(super) async fn project_language_context_in_transaction(
        &self,
        user: UserId,
        project: i64,
    ) -> Result<LanguageToolContext, StorageError> {
        let project = self.get_project(project, user).await?;
        let settings: LanguageSettings = self
            .get_user_setting(user, "language_settings")
            .await?
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|_| StorageError::InvalidValue("Invalid language settings".into()))
            })
            .transpose()?
            .unwrap_or_default();
        settings.validate().map_err(StorageError::InvalidValue)?;
        Ok(LanguageToolContext {
            scope: LanguageToolScope::for_project(&project),
            bindings: self.project_plugins(user, project.id).await?,
            preferences: settings.for_project(project.id),
        })
    }
    pub fn session_language_context(
        &self,
        user: UserId,
        session: i64,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<LanguageToolContext, StorageError>> + Send + '_,
        >,
    > {
        Box::pin(async move {
            self.db
                .transaction(|tx| async move {
                    let store = Store::new(tx);
                    let session = store.get_session(session, user).await?;
                    let project = session.project_id.ok_or_else(|| {
                        StorageError::InvalidRequest("Language services require a project".into())
                    })?;
                    store
                        .project_language_context_in_transaction(user, project)
                        .await
                })
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::plugins::languages::ProviderPreference;
    use openwebide_core::plugins::{ProjectPluginCommand, RecordPlugin, testing::package};

    #[test]
    fn language_context_requires_ownership_and_reads_current_project_preferences_and_plugins() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
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
                            mode,
                            path: Some("project".into()),
                        },
                        user,
                        0,
                    )
                    .await
                    .unwrap();
                let session = store
                    .create_session("session", None, None, Some(project.id), user, 0)
                    .await
                    .unwrap();
                let none = store
                    .create_session("projectless", None, None, None, user, 0)
                    .await
                    .unwrap();
                assert!(
                    store
                        .session_language_context(other, session.id)
                        .await
                        .is_err()
                );
                assert!(store.session_language_context(user, none.id).await.is_err());
                let package = package();
                let installed = store
                    .record_plugin(
                        user,
                        &RecordPlugin {
                            prepared: package.prepared.clone(),
                            package: None,
                            approved_capabilities: vec![],
                            revision: None,
                            update_policy: None,
                        },
                        1,
                    )
                    .await
                    .unwrap();
                let bindings = store
                    .project_plugin_command(
                        user,
                        project.id,
                        &ProjectPluginCommand::Enable {
                            package: Box::new(package),
                            installation_revision: installed[0].revision,
                            revision: None,
                        },
                        2,
                    )
                    .await
                    .unwrap();
                let mut settings = LanguageSettings::default();
                settings
                    .defaults
                    .providers
                    .insert("rust".into(), ProviderPreference::Disabled);
                settings
                    .projects
                    .entry(project.id)
                    .or_default()
                    .providers
                    .insert("rust".into(), ProviderPreference::Automatic);
                store
                    .set_user_setting(
                        user,
                        "language_settings",
                        &serde_json::to_string(&settings).unwrap(),
                    )
                    .await
                    .unwrap();
                let context = store
                    .session_language_context(user, session.id)
                    .await
                    .unwrap();
                assert_eq!(context.scope, LanguageToolScope::for_project(&project));
                assert_eq!(context.bindings, bindings);
                assert_eq!(
                    context.preferences.providers["rust"],
                    ProviderPreference::Automatic
                );
                assert_eq!(
                    context.scope.root,
                    if mode == WorkspaceMode::Remote {
                        Some("project".into())
                    } else {
                        None
                    }
                );
                store
                    .project_plugin_command(
                        user,
                        project.id,
                        &ProjectPluginCommand::Disable {
                            id: bindings[0].id,
                            revision: bindings[0].revision,
                        },
                        3,
                    )
                    .await
                    .unwrap();
                assert!(
                    !store
                        .session_language_context(user, session.id)
                        .await
                        .unwrap()
                        .bindings[0]
                        .enabled
                );
                store
                    .set_user_setting(user, "language_settings", "{bad")
                    .await
                    .unwrap();
                assert!(
                    store
                        .session_language_context(user, session.id)
                        .await
                        .is_err()
                );
            }
        });
    }
}
