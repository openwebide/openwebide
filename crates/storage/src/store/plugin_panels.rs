//! Exactly one execution claim per panel intent, with durable canonical views.
use super::*;
use openwebide_core::plugins::{
    PreparedPlugin, content_digest,
    panels::{coordination::*, *},
};

fn encode<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| StorageError::InvalidValue(error.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_str(value).map_err(|error| StorageError::InvalidValue(error.to_string()))
}
fn conflict(message: &str) -> StorageError {
    StorageError::Conflict(message.into())
}

impl<D: Db> Store<D> {
    /// Repeated intents return receipts without acquiring another execution claim.
    /// Ambiguous interrupted actions stay pending until the instance is closed;
    /// they are never automatically replayed after a timeout or server restart.
    pub async fn begin_plugin_panel(
        &self,
        user: UserId,
        begin: &BeginPanel,
        now: i64,
    ) -> Result<PanelClaim, StorageError> {
        begin
            .prepared
            .validate()
            .map_err(|error| StorageError::InvalidRequest(error.to_string()))?;
        let definition = begin
            .prepared
            .manifest
            .contributions
            .panels
            .iter()
            .find(|panel| panel.id == begin.input.panel)
            .ok_or_else(|| conflict("Panel is not declared"))?;
        validate_input(definition, &begin.input, begin.approved)
            .map_err(StorageError::InvalidRequest)?;
        if now < 0 {
            return Err(StorageError::InvalidRequest("Invalid panel clock".into()));
        }
        let prepared = encode(&begin.prepared)?;
        let input = encode(&begin.input)?;
        let fingerprint = content_digest(input.as_bytes());
        let revision = i64::try_from(begin.input.revision)
            .map_err(|_| StorageError::InvalidRequest("Panel revision is too large".into()))?;
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            if let Some(project) = begin.project { store.get_project(project, user).await?; }
            store.validate_enabled_plugin(user, begin.project, &begin.prepared).await?;
            // An abandoned browser must not consume an active slot forever.
            // Retain the retired identity so late requests cannot revive it.
            store.db.execute("UPDATE plugin_panel_instances SET retired=1,document=NULL,prepared='',active_request=NULL WHERE user_id=? AND retired=0 AND updated_at<?", &[
                DbValue::Int(user.get()),DbValue::Int(now.saturating_sub(3600))
            ]).await?;
            store.db.execute("DELETE FROM plugin_panel_claims WHERE user_id=? AND instance IN (SELECT instance FROM plugin_panel_instances WHERE user_id=? AND retired=1)", &[
                DbValue::Int(user.get()),DbValue::Int(user.get())
            ]).await?;
            let scope = [DbValue::Int(user.get()), DbValue::Text(begin.input.instance.clone())];
            let existing = store.db.execute("SELECT project_scope,prepared,panel,document,active_request,retired FROM plugin_panel_instances WHERE user_id=? AND instance=?", &scope).await?;
            let document = if let Some(row) = existing.rows.first() {
                if row.get_int(0)? != begin.project.unwrap_or(0) || row.get_text(1)? != prepared
                    || row.get_text(2)? != begin.input.panel || row.get_int(5)? != 0 {
                    return Err(conflict("Panel instance is no longer current"));
                }
                let claim = store.db.execute("SELECT input_fingerprint,phase,result FROM plugin_panel_claims WHERE user_id=? AND instance=? AND request=?", &[
                    scope[0].clone(),scope[1].clone(),DbValue::Text(begin.input.request.clone())
                ]).await?;
                if let Some(claim) = claim.rows.first() {
                    if claim.get_text(0)? != fingerprint { return Err(conflict("Panel request identity was reused with different inputs")); }
                    return match claim.get_text(1)? {
                        "pending" => Ok(PanelClaim::Pending),
                        "completed" => Ok(PanelClaim::Completed { document: decode(claim.get_text(2)?)? }),
                        "failed" => Ok(PanelClaim::Failed { message: claim.get_text(2)?.into() }),
                        _ => Err(StorageError::InvalidValue("Invalid panel claim phase".into())),
                    };
                }
                if row.get_text_opt(4).is_some() { return Err(conflict("Another panel request is still active")); }
                row.get_text_opt(3).map(decode::<PanelDocument>).transpose()?
            } else {
                let count = store.db.execute("SELECT COUNT(*) FROM plugin_panel_instances WHERE user_id=? AND retired=0", &[scope[0].clone()]).await?;
                if count.rows[0].get_int(0)? >= 64 { return Err(conflict("Close an existing plugin panel before opening another")); }
                if begin.input.action.is_some() || begin.input.revision != 0 || !begin.input.state.is_null() {
                    return Err(conflict("New panel instances must start with an empty view"));
                }
                store.db.execute("INSERT INTO plugin_panel_instances(user_id,instance,project_scope,prepared,panel,updated_at) VALUES(?,?,?,?,?,?)", &[
                    scope[0].clone(),scope[1].clone(),DbValue::Int(begin.project.unwrap_or(0)),DbValue::Text(prepared),DbValue::Text(begin.input.panel.clone()),DbValue::Int(now)
                ]).await?;
                None
            };
            if let Some(document) = &document {
                validate_submission(definition, document, &begin.input, begin.approved).map_err(StorageError::InvalidRequest)?;
            } else if begin.input.action.is_some() || begin.input.revision != 0 || !begin.input.state.is_null() {
                return Err(conflict("Panel has no current view"));
            }
            let count = store.db.execute("SELECT COUNT(*) FROM plugin_panel_claims WHERE user_id=? AND instance=?", &scope).await?;
            if count.rows[0].get_int(0)? >= 256 { return Err(conflict("Reopen this panel to start a new instance")); }
            store.db.execute("INSERT INTO plugin_panel_claims(user_id,instance,request,input_fingerprint,input_revision,mutating,phase,created_at) VALUES(?,?,?,?,?,?,'pending',?)", &[
                scope[0].clone(),scope[1].clone(),DbValue::Text(begin.input.request.clone()),DbValue::Text(fingerprint),DbValue::Int(revision),DbValue::Int(i64::from(begin.input.action.is_some())),DbValue::Int(now)
            ]).await?;
            store.db.execute("UPDATE plugin_panel_instances SET active_request=?,updated_at=? WHERE user_id=? AND instance=?", &[
                DbValue::Text(begin.input.request.clone()),DbValue::Int(now),scope[0].clone(),scope[1].clone()
            ]).await?;
            Ok(PanelClaim::Claimed { input: begin.input.clone() })
        }).await
    }

    /// Publish only a validated next revision for the still-current claimed instance.
    pub async fn finish_plugin_panel(
        &self,
        user: UserId,
        finish: &FinishPanel,
        now: i64,
    ) -> Result<(), StorageError> {
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            let scope = [DbValue::Int(user.get()),DbValue::Text(finish.instance.clone())];
            let instance = store.db.execute("SELECT project_scope,prepared,panel,active_request,retired FROM plugin_panel_instances WHERE user_id=? AND instance=?", &scope).await?;
            let row = instance.rows.first().ok_or_else(|| conflict("Panel instance was closed"))?;
            if row.get_int(4)? != 0 || row.get_text_opt(3) != Some(finish.request.as_str()) {
                return Err(conflict("Panel request is no longer current"));
            }
            let prepared: PreparedPlugin = decode(row.get_text(1)?)?;
            let project = row.get_int(0)?;
            store.validate_enabled_plugin(user, (project != 0).then_some(project), &prepared).await?;
            let panel_id = row.get_text(2)?;
            let definition = prepared.manifest.contributions.panels.iter().find(|panel| panel.id == panel_id)
                .ok_or_else(|| conflict("Panel definition changed"))?;
            let claim = store.db.execute("SELECT input_revision,mutating FROM plugin_panel_claims WHERE user_id=? AND instance=? AND request=? AND phase='pending'", &[
                scope[0].clone(),scope[1].clone(),DbValue::Text(finish.request.clone())
            ]).await?;
            let claim = claim.rows.first().ok_or_else(|| conflict("Panel claim was already completed"))?;
            let revision = u64::try_from(claim.get_int(0)?)
                .map_err(|_| StorageError::InvalidValue("Invalid panel revision".into()))?;
            let mutating = claim.get_int(1)? != 0;
            let (phase, result) = match &finish.outcome {
                Ok(document) => {
                    validate_document(definition, document).map_err(StorageError::InvalidRequest)?;
                    if revision.checked_add(1) != Some(document.revision) {
                        return Err(conflict("Panel returned a stale revision"));
                    }
                    ("completed",encode(document)?)
                }
                Err(message) => ("failed",message.chars().take(4096).collect()),
            };
            store.db.execute("UPDATE plugin_panel_claims SET phase=?,result=? WHERE user_id=? AND instance=? AND request=? AND phase='pending'", &[
                DbValue::Text(phase.into()),DbValue::Text(result.clone()),scope[0].clone(),scope[1].clone(),DbValue::Text(finish.request.clone())
            ]).await?;
            if phase == "completed" {
                store.db.execute("UPDATE plugin_panel_instances SET document=?,active_request=NULL,updated_at=? WHERE user_id=? AND instance=?", &[
                    DbValue::Text(result),DbValue::Int(now),scope[0].clone(),scope[1].clone()
                ]).await?;
            } else {
                // A failed mutating action may have performed some work. Retire
                // the view so a fresh request cannot replay that same revision.
                store.db.execute("UPDATE plugin_panel_instances SET active_request=NULL,retired=?,document=CASE WHEN ?=1 THEN NULL ELSE document END,prepared=CASE WHEN ?=1 THEN '' ELSE prepared END,updated_at=? WHERE user_id=? AND instance=?", &[
                    DbValue::Int(i64::from(mutating)),DbValue::Int(i64::from(mutating)),DbValue::Int(i64::from(mutating)),DbValue::Int(now),scope[0].clone(),scope[1].clone()
                ]).await?;
            }
            if let Ok(document) = &finish.outcome {
                // Older revisions cannot pass canonical-view validation, even
                // after their receipts are discarded. Keep recent duplicates cheap.
                let oldest = i64::try_from(document.revision.saturating_sub(8))
                    .map_err(|_| StorageError::InvalidRequest("Panel revision is too large".into()))?;
                store.db.execute("DELETE FROM plugin_panel_claims WHERE user_id=? AND instance=? AND phase!='pending' AND input_revision<?", &[
                    scope[0].clone(),scope[1].clone(),DbValue::Int(oldest)
                ]).await?;
            }
            Ok(())
        }).await
    }

    pub async fn close_plugin_panel(
        &self,
        user: UserId,
        instance: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        if instance.len() != 32
            || !instance
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(StorageError::InvalidRequest(
                "Invalid panel instance identity".into(),
            ));
        }
        self.db.transaction(|tx| async move {
            // Close may arrive before a delayed initial-view request. Remember
            // the retired identity even when no execution claim exists yet.
            tx.execute("INSERT OR IGNORE INTO plugin_panel_instances(user_id,instance,project_scope,prepared,panel,retired,updated_at) VALUES(?,?,0,'','',1,?)", &[
                DbValue::Int(user.get()),DbValue::Text(instance.into()),DbValue::Int(now)
            ]).await?;
            tx.execute("UPDATE plugin_panel_instances SET retired=1,document=NULL,prepared='',active_request=NULL,updated_at=? WHERE user_id=? AND instance=?", &[
                DbValue::Int(now),DbValue::Int(user.get()),DbValue::Text(instance.into())
            ]).await?;
            tx.execute("DELETE FROM plugin_panel_claims WHERE user_id=? AND instance=?", &[
                DbValue::Int(user.get()),DbValue::Text(instance.into())
            ]).await?;
            Ok(())
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rusqlite_db::RusqliteDb;
    use openwebide_core::plugins::{PluginPackage, RecordPlugin, RustPlugin};
    use serde_json::json;

    async fn setup(store: &Store<RusqliteDb>, mode: WorkspaceMode) -> (UserId, UserId, BeginPanel) {
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
                    name: "p".into(),
                    mode,
                    path: Some("p".into()),
                },
                user,
                0,
            )
            .await
            .unwrap()
            .id;
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 4;
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.panels = vec![PanelDefinition {
            id: "reviews".into(),
            title: "Reviews".into(),
            icon: PanelIcon::GitPullRequest,
            view: "render".into(),
            refresh_seconds: Some(30),
            actions: vec![PanelAction {
                id: "publish".into(),
                handler: "publish".into(),
                title: "Publish review".into(),
                requires_approval: true,
            }],
        }];
        prepared.manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "reviews".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["http".into()],
        });
        store
            .record_plugin(
                user,
                &RecordPlugin {
                    approved_capabilities: Vec::new(),
                    update_policy: None,
                    revision: None,
                    package: Some(Box::new(PluginPackage {
                        prepared: prepared.clone(),
                        skills: Vec::new(),
                    })),
                    prepared: prepared.clone(),
                },
                1,
            )
            .await
            .unwrap();
        (
            user,
            other,
            BeginPanel {
                project: Some(project),
                prepared,
                approved: false,
                input: PanelInput {
                    panel: "reviews".into(),
                    instance: "a".repeat(32),
                    request: "b".repeat(32),
                    revision: 0,
                    handler: "render".into(),
                    action: None,
                    values: BTreeMap::new(),
                    state: serde_json::Value::Null,
                },
            },
        )
    }
    fn view() -> PanelDocument {
        PanelDocument {
            schema_version: UI_VERSION,
            revision: 1,
            state: json!({"repo":"owner/repo"}),
            blocks: vec![Block::Actions {
                buttons: vec![PanelButton {
                    action: "publish".into(),
                    label: "Publish".into(),
                    values: [("number".into(), json!(42))].into(),
                    disabled: false,
                }],
            }],
        }
    }
    fn finish(begin: &BeginPanel, outcome: Result<PanelDocument, String>) -> FinishPanel {
        FinishPanel {
            instance: begin.input.instance.clone(),
            request: begin.input.request.clone(),
            outcome,
        }
    }

    #[test]
    fn panel_claims_reject_replays_tampering_and_stale_owners_in_both_modes() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                let (user, other, mut begin) = setup(&store, mode).await;
                assert!(store.begin_plugin_panel(other, &begin, 2).await.is_err());
                assert!(matches!(
                    store.begin_plugin_panel(user, &begin, 2).await.unwrap(),
                    PanelClaim::Claimed { .. }
                ));
                assert_eq!(
                    store.begin_plugin_panel(user, &begin, 3).await.unwrap(),
                    PanelClaim::Pending
                );
                let mut concurrent = begin.clone();
                concurrent.input.request = "c".repeat(32);
                assert!(
                    store
                        .begin_plugin_panel(user, &concurrent, 3)
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .finish_plugin_panel(other, &finish(&begin, Ok(view())), 3)
                        .await
                        .is_err()
                );
                let mut stale = view();
                stale.revision = 2;
                assert!(
                    store
                        .finish_plugin_panel(user, &finish(&begin, Ok(stale)), 3)
                        .await
                        .is_err()
                );
                store
                    .finish_plugin_panel(user, &finish(&begin, Ok(view())), 4)
                    .await
                    .unwrap();
                assert_eq!(
                    store.begin_plugin_panel(user, &begin, 4).await.unwrap(),
                    PanelClaim::Completed { document: view() }
                );
                begin.input.request = "d".repeat(32);
                begin.input.action = Some("publish".into());
                begin.input.handler = "publish".into();
                begin.input.revision = 1;
                begin.input.values = [("number".into(), json!(42))].into();
                begin.input.state = view().state;
                assert!(store.begin_plugin_panel(user, &begin, 5).await.is_err());
                begin.approved = true;
                begin.input.values.insert("number".into(), json!(43));
                assert!(store.begin_plugin_panel(user, &begin, 5).await.is_err());
                begin.input.values.insert("number".into(), json!(42));
                assert!(matches!(
                    store.begin_plugin_panel(user, &begin, 5).await.unwrap(),
                    PanelClaim::Claimed { .. }
                ));
                let mut changed = begin.clone();
                changed.input.state = json!({"repo":"another/repo"});
                assert!(store.begin_plugin_panel(user, &changed, 5).await.is_err());
                store
                    .finish_plugin_panel(
                        user,
                        &finish(&begin, Err("Connection lost after publish".into())),
                        6,
                    )
                    .await
                    .unwrap();
                begin.input.request = "e".repeat(32);
                assert!(store.begin_plugin_panel(user, &begin, 7).await.is_err());
                store
                    .close_plugin_panel(user, &begin.input.instance, 8)
                    .await
                    .unwrap();
                assert!(
                    store
                        .finish_plugin_panel(user, &finish(&begin, Ok(view())), 9)
                        .await
                        .is_err()
                );
            }
        });
    }

    #[test]
    fn panel_refresh_history_is_bounded_without_replaying_discarded_receipts() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                let (user, _, mut begin) = setup(&store, mode).await;
                let mut original = begin.clone();
                original.input.request = format!("{:032x}", 0);
                let mut document = view();
                for revision in 0..300 {
                    begin.input.revision = revision;
                    begin.input.request = format!("{revision:032x}");
                    assert!(matches!(
                        store
                            .begin_plugin_panel(user, &begin, 2 + i64::try_from(revision).unwrap())
                            .await
                            .unwrap(),
                        PanelClaim::Claimed { .. }
                    ));
                    document.revision = revision + 1;
                    store
                        .finish_plugin_panel(
                            user,
                            &finish(&begin, Ok(document.clone())),
                            2 + i64::try_from(revision).unwrap(),
                        )
                        .await
                        .unwrap();
                    begin.input.state = document.state.clone();
                }
                let receipts = store
                    .db
                    .execute(
                        "SELECT COUNT(*) FROM plugin_panel_claims WHERE user_id=?",
                        &[DbValue::Int(user.get())],
                    )
                    .await
                    .unwrap();
                assert_eq!(receipts.rows[0].get_int(0).unwrap(), 8);
                assert!(
                    store
                        .begin_plugin_panel(user, &original, 303)
                        .await
                        .is_err()
                );
                assert_eq!(
                    store
                        .begin_plugin_panel(
                            user,
                            &BeginPanel {
                                input: PanelInput {
                                    state: document.state.clone(),
                                    ..begin.input.clone()
                                },
                                ..begin.clone()
                            },
                            303
                        )
                        .await
                        .unwrap(),
                    PanelClaim::Completed { document }
                );
            }
        });
    }

    #[test]
    fn panel_coordination_never_persists_secret_form_inputs() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (user, _, mut begin) = setup(&store, WorkspaceMode::Remote).await;
            store.begin_plugin_panel(user, &begin, 2).await.unwrap();
            let mut document = view();
            document.blocks = vec![Block::Form {
                id: "credentials".into(),
                fields: vec![PanelField {
                    id: "token".into(),
                    label: "Token".into(),
                    kind: FieldKind::Secret { max_length: 128 },
                    value: serde_json::Value::Null,
                    required: true,
                }],
                submit: PanelButton {
                    action: "publish".into(),
                    label: "Save".into(),
                    values: BTreeMap::new(),
                    disabled: false,
                },
            }];
            store
                .finish_plugin_panel(user, &finish(&begin, Ok(document.clone())), 3)
                .await
                .unwrap();
            begin.input.request = "c".repeat(32);
            begin.input.handler = "publish".into();
            begin.input.action = Some("publish".into());
            begin.input.revision = 1;
            begin.input.state = document.state.clone();
            begin
                .input
                .values
                .insert("token".into(), json!("private-token-value"));
            begin.approved = true;
            store.begin_plugin_panel(user, &begin, 4).await.unwrap();
            let rows = store
                .db
                .execute(
                    "SELECT input_fingerprint FROM plugin_panel_claims WHERE user_id=?",
                    &[DbValue::Int(user.get())],
                )
                .await
                .unwrap();
            for row in rows.rows {
                assert_eq!(row.get_text(0).unwrap().len(), 64);
                assert!(!row.get_text(0).unwrap().contains("private-token-value"));
            }
            assert_eq!(
                store.begin_plugin_panel(user, &begin, 4).await.unwrap(),
                PanelClaim::Pending
            );
            document.revision = 2;
            store
                .finish_plugin_panel(user, &finish(&begin, Ok(document.clone())), 5)
                .await
                .unwrap();
            assert_eq!(
                store.begin_plugin_panel(user, &begin, 5).await.unwrap(),
                PanelClaim::Completed { document }
            );
            begin
                .input
                .values
                .insert("token".into(), json!("changed-token"));
            assert!(store.begin_plugin_panel(user, &begin, 5).await.is_err());
            store
                .close_plugin_panel(user, &begin.input.instance, 6)
                .await
                .unwrap();
            let rows = store
                .db
                .execute(
                    "SELECT document,prepared FROM plugin_panel_instances WHERE user_id=?",
                    &[DbValue::Int(user.get())],
                )
                .await
                .unwrap();
            assert!(rows.rows[0].get_text_opt(0).is_none());
            assert_eq!(rows.rows[0].get_text(1).unwrap(), "");
        });
    }

    #[test]
    fn deleting_a_project_retires_its_instance_even_if_a_new_project_reuses_request_ids() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                let (user, _, mut begin) = setup(&store, mode).await;
                store.begin_plugin_panel(user, &begin, 2).await.unwrap();
                let old = finish(&begin, Ok(view()));
                store
                    .delete_project(begin.project.unwrap(), user)
                    .await
                    .unwrap();
                let project = store
                    .create_project(
                        &NewProject {
                            name: "new".into(),
                            mode,
                            path: Some("new".into()),
                        },
                        user,
                        3,
                    )
                    .await
                    .unwrap()
                    .id;
                begin.project = Some(project);
                assert!(store.begin_plugin_panel(user, &begin, 4).await.is_err());
                assert!(store.finish_plugin_panel(user, &old, 4).await.is_err());
            }
        });
    }

    #[test]
    fn panel_closed_before_initial_request_cannot_reopen_in_either_mode() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                let (user, _, begin) = setup(&store, mode).await;
                store
                    .close_plugin_panel(user, &begin.input.instance, 2)
                    .await
                    .unwrap();
                assert!(store.begin_plugin_panel(user, &begin, 3).await.is_err());
                assert!(
                    store
                        .finish_plugin_panel(user, &finish(&begin, Ok(view())), 3)
                        .await
                        .is_err()
                );
            }
        });
    }

    #[test]
    fn abandoned_panel_slots_are_reclaimed_but_late_results_cannot_revive_them() {
        futures::executor::block_on(async {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (user, _, mut begin) = setup(&store, WorkspaceMode::Local).await;
            let mut abandoned = begin.clone();
            for index in 0..64 {
                begin.input.instance = format!("{index:032x}");
                store.begin_plugin_panel(user, &begin, 2).await.unwrap();
                if index == 0 {
                    abandoned = begin.clone();
                }
            }
            begin.input.instance = "f".repeat(32);
            assert!(store.begin_plugin_panel(user, &begin, 3).await.is_err());
            assert!(matches!(
                store.begin_plugin_panel(user, &begin, 3603).await.unwrap(),
                PanelClaim::Claimed { .. }
            ));
            assert!(
                store
                    .begin_plugin_panel(user, &abandoned, 3604)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .finish_plugin_panel(user, &finish(&abandoned, Ok(view())), 3604)
                    .await
                    .is_err()
            );
            let rows = store
                .db
                .execute(
                    "SELECT COUNT(*) FROM plugin_panel_claims WHERE user_id=?",
                    &[DbValue::Int(user.get())],
                )
                .await
                .unwrap();
            assert_eq!(rows.rows[0].get_int(0).unwrap(), 1);
        });
    }

    #[test]
    fn panel_pending_claim_survives_database_reopen_without_reexecution() {
        futures::executor::block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("panels.sqlite");
            let store = Store::new(RusqliteDb::open(&path).unwrap());
            let (user, _, begin) = setup(&store, WorkspaceMode::Local).await;
            assert!(matches!(
                store.begin_plugin_panel(user, &begin, 2).await.unwrap(),
                PanelClaim::Claimed { .. }
            ));
            drop(store);
            let store = Store::new(RusqliteDb::open(&path).unwrap());
            store.migrate().await.unwrap();
            assert_eq!(
                store.begin_plugin_panel(user, &begin, 100).await.unwrap(),
                PanelClaim::Pending
            );
            store
                .close_plugin_panel(user, &begin.input.instance, 101)
                .await
                .unwrap();
            assert!(store.begin_plugin_panel(user, &begin, 102).await.is_err());
        });
    }
    #[test]
    fn disabled_panels_cannot_publish_late_results_in_both_modes() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let store = Store::new(RusqliteDb::open_in_memory().unwrap());
                let (user, _, begin) = setup(&store, mode).await;
                store.begin_plugin_panel(user, &begin, 2).await.unwrap();
                let binding = store
                    .project_plugins(user, begin.project.unwrap())
                    .await
                    .unwrap()
                    .remove(0);
                store
                    .project_plugin_command(
                        user,
                        begin.project.unwrap(),
                        &openwebide_core::plugins::ProjectPluginCommand::Disable {
                            id: binding.id,
                            revision: binding.revision,
                        },
                        3,
                    )
                    .await
                    .unwrap();
                assert!(
                    store
                        .finish_plugin_panel(user, &finish(&begin, Ok(view())), 4)
                        .await
                        .is_err()
                );
                assert!(store.begin_plugin_panel(user, &begin, 4).await.is_err());
                store
                    .close_plugin_panel(user, &begin.input.instance, 5)
                    .await
                    .unwrap();
            }
        });
    }
}
