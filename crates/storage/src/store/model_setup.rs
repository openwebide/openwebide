use super::*;
use openwebide_core::{
    ModelDefaults, ModelProfile, ModelSelection, ModelSettings, ModelSetup, ServerSettings,
    ServerSettingsUpdate, ServerTransport,
};

fn encode(value: &impl serde::Serialize) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| StorageError::Db(error.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_str(value).map_err(|error| StorageError::Db(error.to_string()))
}
impl<D: Db> Store<D> {
    pub async fn model_setup(&self, user: UserId) -> Result<ModelSetup, StorageError> {
        let defaults = self
            .get_user_setting(user, "model_defaults")
            .await?
            .map(|value| decode::<ModelDefaults>(&value))
            .transpose()?
            .unwrap_or_default();
        let rows = self
            .db
            .execute(
                "SELECT server_id, model, settings FROM model_profiles ORDER BY server_id, model",
                &[],
            )
            .await?;
        let profiles = rows
            .rows
            .iter()
            .map(|row| {
                Ok(ModelProfile {
                    selection: ModelSelection {
                        server_id: row.get_int(0)?,
                        model: row.get_text(1)?.into(),
                    },
                    settings: decode::<ModelSettings>(row.get_text(2)?)?,
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        Ok(ModelSetup { defaults, profiles })
    }
    async fn validate_model_selection(
        &self,
        selection: &ModelSelection,
    ) -> Result<(), StorageError> {
        if selection.model.trim().is_empty() {
            return Err(StorageError::InvalidValue("Model name is required.".into()));
        }
        if !self.get_connection(selection.server_id).await?.enabled {
            return Err(StorageError::InvalidValue("Server is disabled.".into()));
        }
        Ok(())
    }
    pub async fn save_model_defaults(
        &self,
        user: UserId,
        defaults: &ModelDefaults,
    ) -> Result<(), StorageError> {
        for selection in [&defaults.primary, &defaults.fast].into_iter().flatten() {
            self.validate_model_selection(selection).await?;
        }
        self.set_user_setting(user, "model_defaults", &encode(defaults)?)
            .await
    }
    pub async fn save_model_profile(&self, profile: &ModelProfile) -> Result<(), StorageError> {
        self.validate_model_selection(&profile.selection).await?;
        profile
            .settings
            .validate()
            .map_err(StorageError::InvalidValue)?;
        let previous = self
            .db
            .execute(
                "SELECT settings FROM model_profiles WHERE server_id = ? AND model = ?",
                &[
                    DbValue::Int(profile.selection.server_id),
                    DbValue::Text(profile.selection.model.clone()),
                ],
            )
            .await?;
        let previous = previous
            .rows
            .first()
            .map(|row| decode::<ModelSettings>(row.get_text(0)?))
            .transpose()?
            .and_then(|settings| settings.stream_tools);
        if profile.settings.stream_tools != previous {
            self.db.execute("UPDATE connections SET tool_stream_revision = tool_stream_revision + 1 WHERE id = ?", &[DbValue::Int(profile.selection.server_id)]).await?;
        }
        let mut settings = profile.settings.clone();
        settings.fast = None;
        self.db
            .execute(
                "INSERT INTO model_profiles (server_id, model, settings) VALUES (?, ?, ?)
            ON CONFLICT(server_id, model) DO UPDATE SET settings = excluded.settings",
                &[
                    DbValue::Int(profile.selection.server_id),
                    DbValue::Text(profile.selection.model.clone()),
                    DbValue::Text(encode(&settings)?),
                ],
            )
            .await?;
        Ok(())
    }
    pub async fn server_transport(&self, id: i64) -> Result<ServerTransport, StorageError> {
        self.get_connection(id).await?;
        let rows = self
            .db
            .execute(
                "SELECT settings FROM server_transport WHERE server_id = ?",
                &[DbValue::Int(id)],
            )
            .await?;
        rows.rows
            .first()
            .map(|row| decode::<ServerTransport>(row.get_text(0)?))
            .transpose()
            .map(Option::unwrap_or_default)
    }
    pub async fn server_settings(&self, id: i64) -> Result<ServerSettings, StorageError> {
        let transport = self.server_transport(id).await?;
        Ok(ServerSettings {
            tool_selection: self.get_connection(id).await?.tool_selection,
            preset: transport.preset,
            has_api_key: transport.api_key.is_some(),
            header_names: transport.headers.keys().cloned().collect(),
            timeout_seconds: transport.timeout_seconds,
            keep_alive: transport.keep_alive,
        })
    }
    pub async fn save_server_settings(
        &self,
        id: i64,
        update: &ServerSettingsUpdate,
    ) -> Result<(), StorageError> {
        if let Some(selection) = &update.tool_selection {
            selection.validate().map_err(StorageError::InvalidValue)?;
        }
        let previous = self.server_transport(id).await?;
        let transport = previous
            .updated(update)
            .map_err(StorageError::InvalidValue)?;
        self.db.execute("INSERT INTO server_transport (server_id, settings) VALUES (?, ?) ON CONFLICT(server_id) DO UPDATE SET settings = excluded.settings", &[DbValue::Int(id), DbValue::Text(encode(&transport)?)]).await?;
        if let Some(selection) = &update.tool_selection {
            self.db
                .execute(
                    "UPDATE connections SET tool_selection = ? WHERE id = ?",
                    &[DbValue::Text(encode(selection)?), DbValue::Int(id)],
                )
                .await?;
        }
        if transport != previous {
            self.db.execute("UPDATE connections SET tool_stream_revision = tool_stream_revision + 1, tool_stream_unsupported = 0 WHERE id = ?", &[DbValue::Int(id)]).await?;
        }
        Ok(())
    }
    /// Commit a setup draft as one database change, including write-only transport secrets.
    pub async fn save_model_setup(
        &self,
        user: UserId,
        probe: &openwebide_core::ModelProbe,
        profiles: &[ModelProfile],
    ) -> Result<(Connection, ModelSetup), StorageError> {
        for profile in profiles {
            profile
                .settings
                .validate()
                .map_err(StorageError::InvalidValue)?;
        }
        if profiles.is_empty() {
            return Err(StorageError::InvalidValue("Choose a chat model.".into()));
        }
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let base_name = openwebide_core::model_setup::server_name(&probe.base_url);
                let existing = store.list_connections().await?;
                let mut generated_name = base_name.clone();
                let mut suffix = 2;
                while existing.iter().any(|server| {
                    Some(server.id) != probe.server_id && server.name == generated_name
                }) {
                    generated_name = format!("{base_name} ({suffix})");
                    suffix += 1;
                }
                let server = if let Some(id) = probe.server_id {
                    let mut connection = store.get_connection(id).await?;
                    if connection.name
                        == openwebide_core::model_setup::server_name(&connection.base_url)
                        || connection.name
                            == format!(
                                "{} @ {}",
                                connection.kind.display_name(),
                                connection.base_url
                            )
                    {
                        connection.name.clone_from(&generated_name);
                    }
                    connection.kind = probe.kind;
                    connection.base_url.clone_from(&probe.base_url);
                    store.update_connection(&connection).await?;
                    store.get_connection(id).await?
                } else {
                    store
                        .insert_connection(&NewConnection {
                            name: generated_name,
                            kind: probe.kind,
                            base_url: probe.base_url.clone(),
                            model: None,
                            context_limit: None,
                        })
                        .await?
                };
                store
                    .save_server_settings(server.id, &probe.transport)
                    .await?;
                for profile in profiles {
                    let mut profile = profile.clone();
                    profile.selection.server_id = server.id;
                    store.save_model_profile(&profile).await?;
                }
                let setup = store.model_setup(user).await?;
                let defaults = openwebide_core::model_setup::review_defaults(
                    &setup.defaults,
                    server.id,
                    profiles,
                );
                store.save_model_defaults(user, &defaults).await?;
                if let Some(primary) = &defaults.primary {
                    store
                        .set_user_setting(
                            user,
                            "default_connection",
                            &primary.server_id.to_string(),
                        )
                        .await?;
                }
                Ok((
                    store.get_connection(server.id).await?,
                    store.model_setup(user).await?,
                ))
            })
            .await
    }
    pub async fn model_tool_stream_unsupported(
        &self,
        connection: &Connection,
        model: &str,
    ) -> Result<bool, StorageError> {
        Ok(self
            .get_setting(&format!("model_stream_{}_{}", connection.id, model))
            .await?
            .and_then(|value| value.parse::<i64>().ok())
            == Some(connection.tool_stream_revision))
    }
    pub async fn set_model_tool_stream_unsupported(
        &self,
        id: i64,
        model: &str,
        revision: i64,
    ) -> Result<(), StorageError> {
        self.db.execute("INSERT INTO settings (key, value) SELECT ?, ? FROM connections WHERE id = ? AND tool_stream_revision = ? ON CONFLICT(key) DO UPDATE SET value = excluded.value", &[DbValue::Text(format!("model_stream_{id}_{model}")), DbValue::Text(revision.to_string()), DbValue::Int(id), DbValue::Int(revision)]).await?;
        Ok(())
    }
    pub async fn model_detection(
        &self,
        connection: &openwebide_core::Connection,
        model: &str,
    ) -> Result<Option<openwebide_core::ModelDetection>, StorageError> {
        let cached = self
            .get_setting(&format!("model_detection_{}_{model}", connection.id))
            .await?;
        let Some(cached) = cached else {
            return Ok(None);
        };
        let cached: DetectionCache = decode(&cached)?;
        Ok((cached.revision == connection.tool_stream_revision).then_some(cached.detection))
    }
    pub async fn save_model_detection(
        &self,
        connection: &openwebide_core::Connection,
        model: &str,
        detection: &openwebide_core::ModelDetection,
    ) -> Result<(), StorageError> {
        let value = encode(&DetectionCache {
            revision: connection.tool_stream_revision,
            detection: detection.clone(),
        })?;
        self.db.execute("INSERT INTO settings (key, value) SELECT ?, ? FROM connections WHERE id = ? AND tool_stream_revision = ? ON CONFLICT(key) DO UPDATE SET value = excluded.value", &[
            DbValue::Text(format!("model_detection_{}_{model}", connection.id)), DbValue::Text(value), DbValue::Int(connection.id), DbValue::Int(connection.tool_stream_revision)
        ]).await?;
        Ok(())
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct DetectionCache {
    revision: i64,
    detection: openwebide_core::ModelDetection,
}
