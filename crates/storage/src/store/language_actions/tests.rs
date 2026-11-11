use super::*;
use crate::rusqlite_db::RusqliteDb;
use futures::executor::block_on;
use openwebide_core::{
    editor::{Document, Edit, EditorRecovery, EditorRecoveryFile, RecoveryScroll, Selection},
    plugins::{
        PluginPackage, RecordPlugin,
        execution::PluginExecutionContext,
        languages::{
            intelligence::{
                self, Query,
                actions::{ActionReview, ServerCommand},
                results::{FileEdit, TextEdit, WorkspaceEdit},
            },
            source_digest,
        },
    },
    workspace_transaction::{
        AdvanceWorkspaceTransaction, BeginWorkspaceTransaction, EditorRecoveryPatch,
        TransactionProgress, WorkspacePlan,
    },
};
use serde_json::json;

struct Scenario {
    _directory: tempfile::TempDir,
    database: std::path::PathBuf,
    store: Store<RusqliteDb>,
    user: UserId,
    other: UserId,
    project: i64,
    call: Call,
    approval: ActionApproval,
    begin: BeginWorkspaceTransaction,
    generation: String,
}
impl Scenario {
    async fn new(mode: WorkspaceMode) -> Self {
        Self::with_role(mode, "language").await
    }
    async fn with_role(mode: WorkspaceMode, role: &str) -> Self {
        Self::with_version(mode, role, 1).await
    }
    async fn with_version(mode: WorkspaceMode, role: &str, version: u32) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("actions.db");
        let store = Store::new(RusqliteDb::open(&database).unwrap());
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
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 9;
        prepared.manifest.executable = None;
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.tools.clear();
        prepared.manifest.contributions.languages = serde_json::from_value(json!([{
            "id":"custom","name":"Custom","extensions":["custom"],
                "features":{"languageServer":{"command":"custom-lsp","role":role}}
        }]))
        .unwrap();
        store
            .record_plugin(
                user,
                &RecordPlugin {
                    prepared: prepared.clone(),
                    package: Some(Box::new(PluginPackage {
                        prepared: prepared.clone(),
                        skills: vec![],
                    })),
                    approved_capabilities: vec![],
                    revision: None,
                    update_policy: None,
                },
                0,
            )
            .await
            .unwrap();
        let call = Call {
            grant: "a".repeat(32),
            session_id: None,
            prepared,
            input: intelligence::Request {
                version: intelligence::VERSION,
                client: "window".into(),
                document: "tab".into(),
                path: "main.custom".into(),
                language: "custom".into(),
                request: 1,
                revision: 1,
                source: "text 文😀\r\n".into(),
                source_digest: source_digest("text 文😀\r\n"),
                query: Query::ReviewAction { id: "b".repeat(32) },
            },
        };
        store
            .issue_plugin_context_grant(
                user,
                &PluginExecutionContext {
                    project_id: Some(project.id),
                    user_action: true,
                    ..Default::default()
                },
                &call.prepared,
                &call.grant,
                0,
            )
            .await
            .unwrap();
        let before = Document::new(&call.input.source);
        let mut after = before.clone();
        after
            .apply(
                vec![Edit::replace(0..4, "next")],
                vec![Selection::caret(4)],
                None,
            )
            .unwrap();
        let file = |document: &Document| EditorRecoveryFile {
            path: "main.custom".into(),
            document: Some(document.recovery()),
            scroll: RecoveryScroll::default(),
            read_only: false,
        };
        let root = EditorRecoveryRoot::for_project(&project);
        let recovery = store
            .save_editor_recovery(
                user,
                project.id,
                0,
                &EditorRecovery {
                    format: 1,
                    root: Some(root.clone()),
                    files: vec![file(&before)],
                    selected: Some("main.custom".into()),
                },
            )
            .await
            .unwrap();
        let id = "c".repeat(32);
        let begin = BeginWorkspaceTransaction {
            language_review: None,
            id: id.clone(),
            plan: WorkspacePlan {
                version,
                root: "physical-folder".into(),
                operations: vec![],
                roots: vec![],
                before: BTreeMap::new(),
                after: BTreeMap::new(),
            },
            editor: EditorRecoveryPatch {
                revision: recovery,
                root,
                before: vec![file(&before)],
                after: vec![file(&after)],
                order_before: vec!["main.custom".into()],
                order_after: vec!["main.custom".into()],
                selected_before: Some("main.custom".into()),
                selected_after: Some("main.custom".into()),
            },
        };
        let prepared = store
            .begin_workspace_transaction(user, project.id, &begin, 1)
            .await
            .unwrap();
        let review = ActionReview {
            title: "Review import".into(),
            edit: WorkspaceEdit {
                files: vec![FileEdit {
                    path: "main.custom".into(),
                    source_digest: call.input.source_digest.clone(),
                    revision: Some(1),
                    edits: vec![TextEdit {
                        range: intelligence::Range {
                            start: intelligence::Position::default(),
                            end: intelligence::Position {
                                line: 0,
                                character: 4,
                            },
                        },
                        new_text: "next".into(),
                    }],
                }],
                changes: vec![],
            },
            command: Some(ServerCommand {
                id: "b".repeat(32),
                server: "native-service".into(),
                name: "custom.apply".into(),
                arguments: vec![json!({"opaque":["文😀",17]})],
            }),
        };
        let approval = ActionApproval {
            id,
            prepared: call.prepared.clone(),
            binding_revision: store
                .project_plugins(user, project.id)
                .await
                .unwrap()
                .iter()
                .find(|binding| binding.prepared.source == call.prepared.source)
                .unwrap()
                .revision,
            input: call.input.clone(),
            review,
        };
        Self {
            _directory: directory,
            database,
            store,
            user,
            other,
            project: project.id,
            call,
            approval,
            begin,
            generation: prepared.journal.generation,
        }
    }
    async fn approve(&self) -> ActionRecord {
        self.store
            .approve_language_action(self.user, self.project, &self.call, &self.approval, 2)
            .await
            .unwrap()
    }
    async fn commit(&self) {
        for (revision, phase) in [
            (1, TransactionPhase::Applying),
            (2, TransactionPhase::Committed),
        ] {
            self.store
                .advance_workspace_transaction(
                    self.user,
                    self.project,
                    &AdvanceWorkspaceTransaction {
                        id: self.approval.id.clone(),
                        generation: self.generation.clone(),
                        revision,
                        progress: TransactionProgress {
                            phase,
                            completed: 0,
                            active: false,
                            rollback_from: None,
                        },
                    },
                    3,
                )
                .await
                .unwrap();
        }
    }
    async fn claim(&self) -> Result<ActionRecord, StorageError> {
        self.store
            .claim_language_action(self.user, self.project, &self.approval.id, 1, &self.call, 4)
            .await
    }
}

#[test]
fn command_claim_waits_for_resource_cleanup_after_the_editor_decision() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::with_version(mode, "language", 2).await;
            case.approve().await;
            for (revision, phase) in [
                (1, TransactionPhase::Applying),
                (2, TransactionPhase::Finalizing),
            ] {
                case.store
                    .advance_workspace_transaction(
                        case.user,
                        case.project,
                        &AdvanceWorkspaceTransaction {
                            id: case.approval.id.clone(),
                            generation: case.generation.clone(),
                            revision,
                            progress: TransactionProgress {
                                phase,
                                completed: 0,
                                active: false,
                                rollback_from: None,
                            },
                        },
                        3,
                    )
                    .await
                    .unwrap();
            }
            let editor = case
                .store
                .editor_recovery(case.user, case.project)
                .await
                .unwrap();
            assert_eq!(
                editor.state.files[0]
                    .document
                    .as_ref()
                    .unwrap()
                    .text
                    .as_str(),
                "next 文😀\r\n"
            );
            assert!(matches!(case.claim().await, Err(StorageError::Conflict(_))));
            assert_eq!(
                case.store
                    .language_action(case.user, case.project, Some(&case.approval.id))
                    .await
                    .unwrap()
                    .unwrap()
                    .phase,
                ActionPhase::Approved
            );
            case.store
                .advance_workspace_transaction(
                    case.user,
                    case.project,
                    &AdvanceWorkspaceTransaction {
                        id: case.approval.id.clone(),
                        generation: case.generation.clone(),
                        revision: 3,
                        progress: TransactionProgress {
                            phase: TransactionPhase::Committed,
                            completed: 0,
                            active: false,
                            rollback_from: None,
                        },
                    },
                    4,
                )
                .await
                .unwrap();
            assert_eq!(
                case.store
                    .editor_recovery(case.user, case.project)
                    .await
                    .unwrap()
                    .revision,
                editor.revision
            );
            assert_eq!(case.claim().await.unwrap().phase, ActionPhase::Running);
        }
    });
}

#[test]
fn command_dispatch_requires_a_durable_claim_and_retains_recovery_access_after_disablement() {
    block_on(async {
        use openwebide_core::plugins::languages::intelligence::actions::{
            ActionJournalRequest, CommandDispatch, CommandOperation,
        };
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            let mut request = CommandDispatch {
                id: case.approval.id.clone(),
                operation: CommandOperation::Start,
                call: Some(Box::new(case.call.clone())),
            };
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &request, 3)
                    .await
                    .is_err()
            );
            case.approve().await;
            case.commit().await;
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &request, 3)
                    .await
                    .is_err()
            );
            let claimed = case
                .store
                .language_action_request(
                    case.user,
                    case.project,
                    &ActionJournalRequest::Claim {
                        id: case.approval.id.clone(),
                        revision: 1,
                        call: Box::new(case.call.clone()),
                    },
                    4,
                )
                .await
                .unwrap()
                .unwrap();
            let authorized = case
                .store
                .authorize_language_command(case.user, case.project, &request, 4)
                .await
                .unwrap();
            assert_eq!(authorized.invocation, claimed.invocation().unwrap());
            assert_eq!(authorized.selection.user, case.user.get());
            assert_eq!(authorized.selection.project, case.project);
            let mut recovery = case
                .store
                .editor_recovery(case.user, case.project)
                .await
                .unwrap();
            let unchanged = recovery.state.clone();
            recovery.state.files[0].document.as_mut().unwrap().text =
                std::sync::Arc::new("new draft after claim".into());
            let revision = case
                .store
                .save_editor_recovery(case.user, case.project, recovery.revision, &recovery.state)
                .await
                .unwrap();
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &request, 4)
                    .await
                    .is_err()
            );
            case.store
                .save_editor_recovery(case.user, case.project, revision, &unchanged)
                .await
                .unwrap();
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &request, 4)
                    .await
                    .is_ok()
            );
            request.call = None;
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &request, 4)
                    .await
                    .is_err()
            );
            case.store
                .set_user_setting(
                    case.user,
                    "language_settings",
                    r#"{"defaults":{"providers":{"custom":{"mode":"disabled"}}}}"#,
                )
                .await
                .unwrap();
            request.call = Some(Box::new(case.call.clone()));
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &request, 4)
                    .await
                    .is_err()
            );
            for operation in [CommandOperation::Status, CommandOperation::Stop] {
                request.operation = operation;
                request.call = None;
                assert!(
                    case.store
                        .authorize_language_command(case.other, case.project, &request, 400)
                        .await
                        .is_err()
                );
                assert_eq!(
                    case.store
                        .authorize_language_command(case.user, case.project, &request, 400)
                        .await
                        .unwrap()
                        .invocation,
                    authorized.invocation
                );
            }
            case.store
                .language_action_request(
                    case.user,
                    case.project,
                    &ActionJournalRequest::Finish {
                        id: request.id.clone(),
                        revision: claimed.revision,
                        outcome: CommandOutcome::Uncertain {
                            message: "Original process stopped".into(),
                        },
                    },
                    400,
                )
                .await
                .unwrap();
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &request, 400)
                    .await
                    .is_ok()
            );
            request.operation = CommandOperation::Start;
            request.call = Some(Box::new(case.call.clone()));
            assert!(
                case.store
                    .authorize_language_command(case.user, case.project, &request, 400)
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn language_action_claims_recheck_preferences_and_provider_binding_revisions() {
    block_on(async {
        use openwebide_core::plugins::{
            ProjectPluginCommand,
            languages::{LanguageSettings, ProviderPreference},
        };
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for role in ["language", "diagnostics"] {
                let case = Scenario::with_role(mode, role).await;
                case.approve().await;
                case.commit().await;
                let mut settings = LanguageSettings::default();
                if role == "language" {
                    settings
                        .defaults
                        .providers
                        .insert("custom".into(), ProviderPreference::Disabled);
                } else {
                    settings.defaults.diagnostics.insert(
                        format!(
                            "plugin:{}/language:custom",
                            case.call.prepared.storage_namespace()
                        ),
                        false,
                    );
                }
                case.store
                    .set_user_setting(
                        case.user,
                        "language_settings",
                        &serde_json::to_string(&settings).unwrap(),
                    )
                    .await
                    .unwrap();
                assert!(case.claim().await.is_err());
                case.store
                    .set_user_setting(case.user, "language_settings", "{}")
                    .await
                    .unwrap();
                let binding = case
                    .store
                    .project_plugins(case.user, case.project)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|binding| binding.prepared.source == case.call.prepared.source)
                    .unwrap();
                let disabled = case
                    .store
                    .project_plugin_command(
                        case.user,
                        case.project,
                        &ProjectPluginCommand::Disable {
                            id: binding.id,
                            revision: binding.revision,
                        },
                        4,
                    )
                    .await
                    .unwrap();
                let disabled = disabled
                    .iter()
                    .find(|binding| binding.prepared.source == case.call.prepared.source)
                    .unwrap();
                let installed = case.store.plugin_installations(case.user).await.unwrap();
                let installed = installed
                    .iter()
                    .find(|entry| entry.prepared.source == case.call.prepared.source)
                    .unwrap();
                case.store
                    .project_plugin_command(
                        case.user,
                        case.project,
                        &ProjectPluginCommand::Enable {
                            package: Box::new(PluginPackage {
                                prepared: case.call.prepared.clone(),
                                skills: vec![],
                            }),
                            installation_revision: installed.revision,
                            revision: Some(disabled.revision),
                        },
                        5,
                    )
                    .await
                    .unwrap();
                assert!(case.claim().await.is_err());
                assert_eq!(
                    case.store
                        .language_action(case.user, case.project, None)
                        .await
                        .unwrap()
                        .unwrap()
                        .phase,
                    ActionPhase::Approved
                );
            }
        }
    });
}

#[test]
fn language_actions_require_manual_authority_complete_edits_and_current_drafts() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            let mut read_only = case.call.clone();
            read_only.grant = "d".repeat(32);
            case.store
                .issue_plugin_context_grant(
                    case.user,
                    &PluginExecutionContext {
                        project_id: Some(case.project),
                        user_action: false,
                        ..Default::default()
                    },
                    &case.call.prepared,
                    &read_only.grant,
                    1,
                )
                .await
                .unwrap();
            assert!(
                case.store
                    .approve_language_action(case.user, case.project, &read_only, &case.approval, 2)
                    .await
                    .is_err()
            );
            assert!(
                case.store
                    .approve_language_action(
                        case.other,
                        case.project,
                        &case.call,
                        &case.approval,
                        2
                    )
                    .await
                    .is_err()
            );
            case.approve().await;
            assert!(case.claim().await.is_err());
            case.commit().await;
            assert!(
                case.store
                    .claim_language_action(
                        case.user,
                        case.project,
                        &case.approval.id,
                        1,
                        &read_only,
                        4
                    )
                    .await
                    .is_err()
            );
            let mut recovery = case
                .store
                .editor_recovery(case.user, case.project)
                .await
                .unwrap();
            recovery.state.files[0].document.as_mut().unwrap().text =
                std::sync::Arc::new("later edit".into());
            case.store
                .save_editor_recovery(case.user, case.project, recovery.revision, &recovery.state)
                .await
                .unwrap();
            assert!(case.claim().await.is_err());
            assert_eq!(
                case.store
                    .language_action(case.user, case.project, None)
                    .await
                    .unwrap()
                    .unwrap()
                    .phase,
                ActionPhase::Approved
            );
            let rejected = case
                .store
                .reject_unstarted_language_action(
                    case.user,
                    case.project,
                    &case.approval.id,
                    1,
                    "The reviewed draft changed before dispatch",
                    5,
                )
                .await
                .unwrap();
            assert_eq!(rejected.phase, ActionPhase::Rejected);
            case.store
                .acknowledge_language_action(
                    case.user,
                    case.project,
                    &case.approval.id,
                    rejected.revision,
                    "Keep my newer draft; this command did not run",
                    6,
                )
                .await
                .unwrap();
        }
    });
}

#[test]
fn language_action_claims_survive_database_reopen_and_outcome_acknowledgement_loss() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            let approved = case.approve().await;
            assert_eq!(case.approve().await.revision, approved.revision);
            assert!(approved.invocation().is_err());
            case.commit().await;
            let running = case.claim().await.unwrap();
            running.invocation().unwrap().validate().unwrap();
            assert!(case.claim().await.is_err());
            let reopened = Store::new(RusqliteDb::open(&case.database).unwrap());
            reopened.migrate().await.unwrap();
            let durable = reopened
                .language_action(case.user, case.project, Some(&case.approval.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(durable.phase, ActionPhase::Running);
            assert!(
                reopened
                    .claim_language_action(
                        case.user,
                        case.project,
                        &case.approval.id,
                        durable.revision,
                        &case.call,
                        5
                    )
                    .await
                    .is_err()
            );
            let outcome = CommandOutcome::Uncertain {
                message: "Response was lost after native dispatch".into(),
            };
            let finished = reopened
                .finish_language_action(
                    case.user,
                    case.project,
                    &case.approval.id,
                    running.revision,
                    &outcome,
                    6,
                )
                .await
                .unwrap();
            let repeated = reopened
                .finish_language_action(
                    case.user,
                    case.project,
                    &case.approval.id,
                    running.revision,
                    &outcome,
                    7,
                )
                .await
                .unwrap();
            assert_eq!(finished.revision, repeated.revision);
            assert!(!finished.phase.terminal());
            assert!(
                reopened
                    .cancel_language_action(
                        case.user,
                        case.project,
                        &case.approval.id,
                        finished.revision,
                        8
                    )
                    .await
                    .is_err()
            );
            let acknowledged = reopened
                .acknowledge_language_action(
                    case.user,
                    case.project,
                    &case.approval.id,
                    finished.revision,
                    "I reviewed the command's workspace effects",
                    9,
                )
                .await
                .unwrap();
            assert_eq!(acknowledged.phase, ActionPhase::Acknowledged);
            assert!(acknowledged.invocation().is_err());
            assert!(
                reopened
                    .claim_language_action(
                        case.user,
                        case.project,
                        &case.approval.id,
                        acknowledged.revision,
                        &case.call,
                        10
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn archived_protocol_one_command_outcomes_remain_recoverable_after_protocol_upgrade() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            case.approve().await;
            case.commit().await;
            let running = case.claim().await.unwrap();
            let mut archived = running.approval.clone();
            archived.input.version = 1;
            assert!(archived.input.validate().is_err());
            archived.validate().unwrap();
            let mut unsupported = archived.clone();
            unsupported.input.version = intelligence::VERSION + 1;
            assert!(unsupported.validate().is_err());
            let encoded = serde_json::to_string(&archived).unwrap();
            case.store.db.execute("UPDATE language_actions SET approval=? WHERE user_id=? AND project_id=? AND id=?",&[
                DbValue::Text(encoded),DbValue::Int(case.user.get()),DbValue::Int(case.project),DbValue::Text(archived.id.clone())
            ]).await.unwrap();
            case.store
                .db
                .execute("PRAGMA user_version=64", &[])
                .await
                .unwrap();
            let reopened = Store::new(RusqliteDb::open(&case.database).unwrap());
            reopened.migrate().await.unwrap();
            let recovered = reopened
                .language_action(case.user, case.project, Some(&archived.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(recovered.approval.input.version, 1);
            assert_eq!(recovered.phase, ActionPhase::Running);
            let uncertain = reopened
                .finish_language_action(
                    case.user,
                    case.project,
                    &archived.id,
                    recovered.revision,
                    &CommandOutcome::Uncertain {
                        message: "The previous execution host outcome was lost".into(),
                    },
                    6,
                )
                .await
                .unwrap();
            let acknowledged = reopened
                .acknowledge_language_action(
                    case.user,
                    case.project,
                    &archived.id,
                    uncertain.revision,
                    "I reviewed the original command effects",
                    7,
                )
                .await
                .unwrap();
            assert_eq!(acknowledged.phase, ActionPhase::Acknowledged);
            assert_eq!(acknowledged.approval.input.version, 1);
            assert!(acknowledged.invocation().is_err());
        }
    });
}

#[test]
fn pending_language_actions_pin_recovery_and_block_other_resource_operations() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            case.approve().await;
            case.commit().await;
            let running = case.claim().await.unwrap();
            let outcome = CommandOutcome::Uncertain {
                message: "Host restarted while command was running".into(),
            };
            case.store
                .finish_language_action(
                    case.user,
                    case.project,
                    &case.approval.id,
                    running.revision,
                    &outcome,
                    5,
                )
                .await
                .unwrap();
            let mut another = case.begin.clone();
            another.id = "e".repeat(32);
            another.editor.revision += 1;
            another.editor.before.clone_from(&another.editor.after);
            assert!(
                case.store
                    .begin_workspace_transaction(case.user, case.project, &another, 1000)
                    .await
                    .is_err()
            );
            assert!(
                case.store
                    .workspace_transaction(case.user, case.project, Some(&case.approval.id))
                    .await
                    .unwrap()
                    .is_some()
            );
            // Cleanup triggered by a different project must also retain this
            // command's older, already-committed workspace recovery payload.
            let second = case
                .store
                .create_project(
                    &NewProject {
                        name: "second".into(),
                        mode,
                        path: Some("second".into()),
                    },
                    case.user,
                    1000,
                )
                .await
                .unwrap();
            let mut empty = case.begin.clone();
            empty.id = "f".repeat(32);
            empty.editor = EditorRecoveryPatch {
                revision: 0,
                root: EditorRecoveryRoot::for_project(&second),
                before: vec![],
                after: vec![],
                order_before: vec![],
                order_after: vec![],
                selected_before: None,
                selected_after: None,
            };
            case.store
                .begin_workspace_transaction(case.user, second.id, &empty, 1000)
                .await
                .unwrap();
            assert!(
                case.store
                    .workspace_transaction(case.user, case.project, Some(&case.approval.id))
                    .await
                    .unwrap()
                    .is_some()
            );
            assert!(
                case.store
                    .language_action(case.other, case.project, None)
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn cursor_changes_do_not_revoke_commands_and_prepared_actions_cancel_without_execution() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let case = Scenario::new(mode).await;
            case.approve().await;
            case.commit().await;
            let mut recovery = case
                .store
                .editor_recovery(case.user, case.project)
                .await
                .unwrap();
            recovery.state.files[0].scroll.top = 25.0;
            recovery.state.files[0]
                .document
                .as_mut()
                .unwrap()
                .selections = vec![Selection::caret(0)];
            case.store
                .save_editor_recovery(case.user, case.project, recovery.revision, &recovery.state)
                .await
                .unwrap();
            assert_eq!(case.claim().await.unwrap().phase, ActionPhase::Running);
            let cancelled = Scenario::new(mode).await;
            cancelled.approve().await;
            cancelled
                .store
                .advance_workspace_transaction(
                    cancelled.user,
                    cancelled.project,
                    &AdvanceWorkspaceTransaction {
                        id: cancelled.approval.id.clone(),
                        generation: cancelled.generation.clone(),
                        revision: 1,
                        progress: TransactionProgress::prepared_cancellation(),
                    },
                    3,
                )
                .await
                .unwrap();
            assert_eq!(
                cancelled
                    .store
                    .cancel_language_action(
                        cancelled.user,
                        cancelled.project,
                        &cancelled.approval.id,
                        1,
                        4
                    )
                    .await
                    .unwrap()
                    .phase,
                ActionPhase::Cancelled
            );
            assert!(cancelled.claim().await.is_err());
        }
    });
}

#[test]
fn deleting_a_project_preserves_pending_command_recovery_after_files_commit_in_both_modes() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for phase in [
                ActionPhase::Approved,
                ActionPhase::Running,
                ActionPhase::Rejected,
                ActionPhase::Uncertain,
            ] {
                let case = Scenario::new(mode).await;
                let mut record = case.approve().await;
                case.commit().await;
                if matches!(phase, ActionPhase::Running | ActionPhase::Uncertain) {
                    record = case.claim().await.unwrap();
                }
                if phase == ActionPhase::Uncertain {
                    record = case
                        .store
                        .finish_language_action(
                            case.user,
                            case.project,
                            &record.approval.id,
                            record.revision,
                            &CommandOutcome::Uncertain {
                                message: "Lost native reply".into(),
                            },
                            5,
                        )
                        .await
                        .unwrap();
                } else if phase == ActionPhase::Rejected {
                    record = case
                        .store
                        .reject_unstarted_language_action(
                            case.user,
                            case.project,
                            &record.approval.id,
                            record.revision,
                            "Unstarted command rejected",
                            5,
                        )
                        .await
                        .unwrap();
                }
                assert_eq!(record.phase, phase);
                assert!(matches!(
                    case.store.delete_project(case.project, case.other).await,
                    Err(StorageError::NotFound(_))
                ));
                assert!(matches!(
                    case.store.delete_project(case.project, case.user).await,
                    Err(StorageError::Conflict(_))
                ));
                // A failed deletion leaves the durable command, file journal,
                // project and editor state intact, including after reopening DB.
                let reopened = Store::new(RusqliteDb::open(&case.database).unwrap());
                assert_eq!(
                    reopened
                        .language_action(case.user, case.project, Some(&record.approval.id))
                        .await
                        .unwrap()
                        .unwrap()
                        .phase,
                    phase
                );
                assert_eq!(
                    reopened
                        .workspace_transaction(case.user, case.project, Some(&record.approval.id))
                        .await
                        .unwrap()
                        .unwrap()
                        .journal
                        .progress
                        .phase,
                    TransactionPhase::Committed
                );
                assert!(
                    reopened
                        .editor_recovery(case.user, case.project)
                        .await
                        .unwrap()
                        .state
                        .files[0]
                        .document
                        .as_ref()
                        .unwrap()
                        .text
                        .starts_with("next")
                );
                if phase == ActionPhase::Approved {
                    record = case
                        .store
                        .reject_unstarted_language_action(
                            case.user,
                            case.project,
                            &record.approval.id,
                            record.revision,
                            "Discard unstarted command",
                            6,
                        )
                        .await
                        .unwrap();
                } else if phase == ActionPhase::Running {
                    record = case
                        .store
                        .finish_language_action(
                            case.user,
                            case.project,
                            &record.approval.id,
                            record.revision,
                            &CommandOutcome::Completed {
                                result: serde_json::Value::Null,
                            },
                            6,
                        )
                        .await
                        .unwrap();
                }
                if !record.phase.terminal() {
                    record = case
                        .store
                        .acknowledge_language_action(
                            case.user,
                            case.project,
                            &record.approval.id,
                            record.revision,
                            "Reviewed outcome",
                            7,
                        )
                        .await
                        .unwrap();
                }
                assert!(record.phase.terminal());
                case.store
                    .delete_project(case.project, case.user)
                    .await
                    .unwrap();
                assert!(matches!(
                    reopened.get_project(case.project, case.user).await,
                    Err(StorageError::NotFound(_))
                ));
            }
        }
    });
}
