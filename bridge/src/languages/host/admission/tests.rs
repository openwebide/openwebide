use super::*;
use openwebide_core::{
    WorkspaceMode,
    editor::{
        Document, EditorRecovery, EditorRecoveryFile, EditorRecoveryRecord, EditorRecoveryRoot,
        RecoveryScroll,
    },
    plugins::{
        PreparedPlugin,
        languages::{
            intelligence::{
                Position, Range,
                actions::{ActionReview, ActionSynchronization, origin_fingerprint},
                results::WorkspaceChange,
                workspace_edit::{WorkspaceDocument, WorkspaceDraft},
            },
            source_digest,
        },
    },
    workspace_transaction::{
        JournalRecord, LanguageReview, TransactionProgress, VfsTransactionFiles,
    },
};
const SOURCE: &str = "text 文😀\r\n";
mod worker;
struct Fixture {
    root: tempfile::TempDir,
    host: Host,
    scope: Scope,
    prepared: PreparedPlugin,
    input: Request,
}
impl Fixture {
    fn new(owner: &str) -> Self {
        Self::with_mode(owner, "apply")
    }
    fn with_mode(owner: &str, mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.notes"), SOURCE).unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        let script = root.path().join("peer.py");
        std::fs::write(
            &script,
            include_str!("../../../../../tools/fixtures/commands_server.py"),
        )
        .unwrap();
        let server: LanguageServer =
            serde_json::from_value(json!({"command":"python3","args":[script,mode]})).unwrap();
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 9;
        prepared.manifest.executable = None;
        prepared.manifest.contributions.tools.clear();
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.languages = serde_json::from_value(json!([{"id":"notes","name":"Notes","extensions":["notes"],"features":{"languageServer":server}}])).unwrap();
        let scope = Scope {
            owner: owner.into(),
            project: 1,
            root: root.path().canonicalize().unwrap(),
            provider: prepared.storage_namespace(),
            digest: prepared.digest.clone(),
            server,
        };
        let input = Request {
            version: protocol::VERSION,
            client: "client".into(),
            document: "document".into(),
            request: 1,
            revision: 1,
            language: "notes".into(),
            path: "main.notes".into(),
            source: SOURCE.into(),
            source_digest: source_digest(SOURCE),
            query: Query::CodeActions {
                range: Range {
                    start: Position::default(),
                    end: Position {
                        line: 0,
                        character: 4,
                    },
                },
            },
        };
        Self {
            root,
            host: Host::default(),
            scope,
            prepared,
            input,
        }
    }
    async fn state(&mut self) -> WorkspaceTransactionState {
        if !matches!(self.input.query, Query::CodeActions { .. }) {
            self.input.request += 1;
            self.input.query = Query::CodeActions {
                range: Range {
                    start: Position::default(),
                    end: Position {
                        line: 0,
                        character: 4,
                    },
                },
            };
        }
        let actions = self
            .host
            .query_package(self.scope.clone(), &self.input, &self.prepared)
            .await
            .unwrap();
        self.input.request += 1;
        self.input.query = Query::ReviewAction {
            id: actions.result[0]["id"].as_str().unwrap().into(),
        };
        let response = self
            .host
            .query_package(self.scope.clone(), &self.input, &self.prepared)
            .await
            .unwrap();
        let action: ActionReview = serde_json::from_value(response.result.clone()).unwrap();
        let mode = if self.scope.owner.starts_with("paired/") {
            WorkspaceMode::Local
        } else {
            WorkspaceMode::Remote
        };
        let before = EditorRecoveryRecord {
            revision: 1,
            state: EditorRecovery {
                format: 1,
                root: Some(EditorRecoveryRoot {
                    mode,
                    path: (mode == WorkspaceMode::Remote).then(|| {
                        self.scope
                            .root
                            .file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .into()
                    }),
                }),
                selected: Some("main.notes".into()),
                files: vec![EditorRecoveryFile {
                    path: "main.notes".into(),
                    document: Some(Document::new(SOURCE).recovery()),
                    scroll: RecoveryScroll::default(),
                    read_only: false,
                }],
            },
        };
        let vfs = crate::runs::native_vfs::NativeFsVfs {
            root: self.scope.root.clone(),
        };
        let identity = crate::workspace_transactions::Host::root_identity(
            self.scope.root.parent().unwrap(),
            self.scope.root.file_name().unwrap().to_str().unwrap(),
            1,
            1,
        )
        .unwrap();
        let files = VfsTransactionFiles {
            vfs: &vfs,
            identity: &identity,
        };
        let changes = action.edit.clone().ordered().unwrap();
        let operations = changes
            .iter()
            .filter_map(|change| match change {
                WorkspaceChange::File { operation } => Some(operation.clone()),
                WorkspaceChange::Text { .. } => None,
            })
            .collect();
        let documents = [(
            "main.notes".into(),
            WorkspaceDocument::new("main.notes", Document::new(SOURCE)),
        )]
        .into();
        let mut draft = WorkspaceDraft::prepare_native(
            &files,
            identity.clone(),
            operations,
            documents,
            vec!["main.notes".into()],
            Some("main.notes".into()),
            || true,
        )
        .await
        .unwrap();
        for change in changes {
            match change {
                WorkspaceChange::File { operation } => draft.resource(operation).unwrap(),
                WorkspaceChange::Text { file } => {
                    draft.document(&files, &file.path, || true).await.unwrap();
                    draft.text(file).unwrap();
                }
            }
        }
        let after = draft.recovery(&before.state).unwrap();
        let editor = draft.recovery_patch(&before, &after).unwrap();
        let review = LanguageReview {
            prepared: self.prepared.clone(),
            binding_revision: 1,
            input: self.input.clone(),
            context: response.context.unwrap(),
            result: response.result,
            transaction_digest: LanguageReview::transaction_digest(&draft.plan, &editor).unwrap(),
        };
        WorkspaceTransactionState {
            journal: JournalRecord {
                id: "a".repeat(32),
                generation: "b".repeat(32),
                revision: 1,
                plan: draft.plan,
                progress: TransactionProgress::default(),
            },
            editor,
            editor_revision: 1,
            language_review: Some(Box::new(review)),
        }
    }
    async fn prepare(&mut self, state: &WorkspaceTransactionState) {
        let review = state.language_review.as_ref().unwrap();
        let action: ActionReview = serde_json::from_value(review.result.clone()).unwrap();
        let command = action.command.as_ref().unwrap();
        self.input.request += 1;
        self.input.query = Query::PrepareAction {
            action: ActionSynchronization {
                command: command.id.clone(),
                server: command.server.clone(),
                origin: origin_fingerprint(&review.input).unwrap(),
                review: action.fingerprint().unwrap(),
            },
        };
        self.host
            .query_package(self.scope.clone(), &self.input, &self.prepared)
            .await
            .unwrap();
    }
    fn calls(&self) -> String {
        std::fs::read_to_string(self.root.path().join("calls")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.shutdown();
    }
}

#[tokio::test]
async fn original_native_review_validates_under_lease_without_replaying_request_or_lsp_in_both_profiles()
 {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::new(owner);
        let state = fixture.state().await;
        fixture.prepare(&state).await;
        let folder = Folder::capture(&fixture.scope.root).unwrap();
        let lease = fixture
            .host
            .inner
            .coordinator
            .acquire(folder)
            .await
            .unwrap();
        let calls = fixture.calls();
        let _review = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &state, &lease)
            .await
            .unwrap();
        let review = state.language_review.as_ref().unwrap();
        let ordinary = Request {
            request: fixture.input.request + 1,
            query: Query::ValidateAnalysis {
                context: review.context.clone(),
            },
            ..review.input.clone()
        };
        let error = fixture
            .host
            .query_package(fixture.scope.clone(), &ordinary, &fixture.prepared)
            .await
            .unwrap_err();
        assert!(error.contains("matching reviewed command"), "{error}");
        assert_eq!(fixture.calls(), calls);
        assert_eq!(
            calls.lines().filter(|line| *line == "initialize").count(),
            1
        );
        assert!(!fixture.root.path().join("executions").exists());
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("main.notes")).unwrap(),
            SOURCE
        );
    }
}

#[tokio::test]
async fn a_child_folder_lease_cannot_authorize_verification_of_the_whole_project() {
    let mut fixture = Fixture::new("user:1");
    let state = fixture.state().await;
    let folder = Folder::capture(&fixture.scope.root.join("nested")).unwrap();
    let lease = fixture
        .host
        .inner
        .coordinator
        .acquire(folder)
        .await
        .unwrap();
    let error = fixture
        .host
        .validate_workspace_review(fixture.scope.clone(), &state, &lease)
        .await
        .unwrap_err();
    assert!(error.contains("original folder lease"), "{error}");
}

#[tokio::test]
async fn native_review_requires_its_prepared_command_and_rejects_forged_editor_patches() {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::new(owner);
        let state = fixture.state().await;
        let lease = fixture
            .host
            .inner
            .coordinator
            .acquire(Folder::capture(&fixture.scope.root).unwrap())
            .await
            .unwrap();
        let calls = fixture.calls();
        let error = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &state, &lease)
            .await
            .unwrap_err();
        assert!(error.contains("Prepare the complete original"), "{error}");
        fixture.prepare(&state).await;
        let mut forged = state.clone();
        forged.editor.after[0].document.as_mut().unwrap().text = Arc::new("forged output".into());
        forged.language_review.as_mut().unwrap().transaction_digest =
            LanguageReview::transaction_digest(&forged.journal.plan, &forged.editor).unwrap();
        forged
            .language_review
            .as_ref()
            .unwrap()
            .validate_transaction(&forged.journal.plan, &forged.editor)
            .unwrap();
        let error = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &forged, &lease)
            .await
            .unwrap_err();
        assert!(error.contains("editor patch differs"), "{error}");
        let mut forged = state.clone();
        forged.language_review.as_mut().unwrap().input.request += 100;
        let error = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &forged, &lease)
            .await
            .unwrap_err();
        assert!(
            error.contains("original language analysis request changed"),
            "{error}"
        );
        fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &state, &lease)
            .await
            .unwrap();
        assert_eq!(fixture.calls(), calls);
        assert!(!fixture.root.path().join("executions").exists());
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("main.notes")).unwrap(),
            SOURCE
        );
    }
}

#[tokio::test]
async fn native_review_rejects_other_owners_projects_providers_and_request_queries_without_replacement()
 {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::with_mode(owner, "edit-only");
        let state = fixture.state().await;
        let lease = fixture
            .host
            .inner
            .coordinator
            .acquire(Folder::capture(&fixture.scope.root).unwrap())
            .await
            .unwrap();
        let mut scopes = vec![];
        let mut changed = fixture.scope.clone();
        changed.owner.push_str("-other");
        scopes.push(changed);
        let mut changed = fixture.scope.clone();
        changed.project += 1;
        scopes.push(changed);
        let mut changed = fixture.scope.clone();
        changed.provider.push_str("-other");
        scopes.push(changed);
        let mut changed = fixture.scope.clone();
        changed.digest = "f".repeat(64);
        scopes.push(changed);
        let mut changed = fixture.scope.clone();
        changed.server.command.push_str("-other");
        scopes.push(changed);
        let calls = fixture.calls();
        for scope in scopes {
            assert!(
                fixture
                    .host
                    .validate_workspace_review(scope, &state, &lease)
                    .await
                    .is_err()
            );
        }
        let mut forged = state.clone();
        forged.language_review.as_mut().unwrap().input.query =
            Query::ReviewAction { id: "f".repeat(32) };
        let error = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &forged, &lease)
            .await
            .unwrap_err();
        assert!(
            error.contains("original language analysis request changed"),
            "{error}"
        );
        let mut forged = state.clone();
        forged.language_review.as_mut().unwrap().result["edit"]["files"][0]["edits"][0]["new_text"] =
            json!("forged");
        let review = forged.language_review.as_mut().unwrap();
        review.context.result_digest =
            openwebide_core::plugins::content_digest(&serde_json::to_vec(&review.result).unwrap());
        assert!(
            fixture
                .host
                .validate_workspace_review(fixture.scope.clone(), &forged, &lease)
                .await
                .is_err()
        );
        assert_eq!(fixture.calls(), calls);
        fixture.host.shutdown();
        let error = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &state, &lease)
            .await
            .unwrap_err();
        assert!(
            error.contains("original language service is unavailable"),
            "{error}"
        );
        assert_eq!(
            fixture
                .calls()
                .lines()
                .filter(|line| *line == "initialize")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn native_review_holds_original_overlay_sources_until_its_guard_is_released() {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::with_mode(owner, "edit-only");
        let state = fixture.state().await;
        let lease = fixture
            .host
            .inner
            .coordinator
            .acquire(Folder::capture(&fixture.scope.root).unwrap())
            .await
            .unwrap();
        let guard = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &state, &lease)
            .await
            .unwrap();
        let mut input = fixture.input.clone();
        input.request += 1;
        input.revision += 1;
        input.source = "new unsaved source".into();
        input.source_digest = source_digest(&input.source);
        input.query = Query::Synchronize;
        let (host, scope, prepared) = (
            fixture.host.clone(),
            fixture.scope.clone(),
            fixture.prepared.clone(),
        );
        let sync = tokio::spawn(async move { host.query_package(scope, &input, &prepared).await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !sync.is_finished(),
            "A later overlay replaced the original while native admission retained it"
        );
        guard.ensure_current().unwrap();
        drop(guard);
        tokio::time::timeout(Duration::from_secs(5), sync)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let error = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &state, &lease)
            .await
            .unwrap_err();
        assert!(error.contains("document changed or closed"), "{error}");
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("main.notes")).unwrap(),
            SOURCE
        );
    }
}

#[tokio::test]
async fn native_review_rejects_project_source_changes_before_any_mutation() {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::with_mode(owner, "edit-only");
        let state = fixture.state().await;
        std::fs::write(fixture.root.path().join("main.notes"), "external change").unwrap();
        let lease = fixture
            .host
            .inner
            .coordinator
            .acquire(Folder::capture(&fixture.scope.root).unwrap())
            .await
            .unwrap();
        let calls = fixture.calls();
        assert!(
            fixture
                .host
                .validate_workspace_review(fixture.scope.clone(), &state, &lease)
                .await
                .is_err()
        );
        assert_eq!(fixture.calls(), calls);
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("main.notes")).unwrap(),
            "external change"
        );
        assert!(!fixture.root.path().join("executions").exists());
    }
}

#[tokio::test]
async fn a_prepared_command_cannot_borrow_another_pending_commands_admission_exception() {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::new(owner);
        let original = fixture.state().await;
        let other = fixture.state().await;
        fixture.prepare(&other).await;
        fixture.prepare(&original).await;
        let lease = fixture
            .host
            .inner
            .coordinator
            .acquire(Folder::capture(&fixture.scope.root).unwrap())
            .await
            .unwrap();
        let calls = fixture.calls();
        let error = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &original, &lease)
            .await
            .unwrap_err();
        assert!(error.contains("command prevents"), "{error}");
        let review = other.language_review.as_ref().unwrap();
        let action: ActionReview = serde_json::from_value(review.result.clone()).unwrap();
        let command = action.command.as_ref().unwrap();
        fixture.input.request += 1;
        fixture.input.query = Query::ReleaseAction {
            action: ActionSynchronization {
                command: command.id.clone(),
                server: command.server.clone(),
                origin: origin_fingerprint(&review.input).unwrap(),
                review: action.fingerprint().unwrap(),
            },
        };
        fixture
            .host
            .query_package(fixture.scope.clone(), &fixture.input, &fixture.prepared)
            .await
            .unwrap();
        fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &original, &lease)
            .await
            .unwrap();
        assert_eq!(fixture.calls(), calls);
        assert!(!fixture.root.path().join("executions").exists());
    }
}

fn restore_affected_file(fixture: &Fixture) {
    let path = fixture.root.path().join("settings-old.json");
    let original = std::fs::read(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, "intervening contents").unwrap();
    std::fs::write(&path, &original).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        std::fs::metadata(path).unwrap().modified().unwrap(),
        modified
    );
}

#[tokio::test]
async fn ordinary_validation_rejects_restored_unassociated_affected_file_in_both_profiles() {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::with_mode(owner, "inputs-edit-only");
        std::fs::write(
            fixture.root.path().join("settings-old.json"),
            "{\"original\":true}",
        )
        .unwrap();
        let state = fixture.state().await;
        let calls = fixture.calls();
        restore_affected_file(&fixture);
        let review = state.language_review.as_ref().unwrap();
        let request = Request {
            request: fixture.input.request + 1,
            query: Query::ValidateAnalysis {
                context: review.context.clone(),
            },
            ..review.input.clone()
        };
        let result = fixture
            .host
            .query_package(fixture.scope.clone(), &request, &fixture.prepared)
            .await;
        assert!(
            result.is_err(),
            "Restoring an unassociated affected file must not restore original analysis: {result:?}"
        );
        assert_eq!(fixture.calls(), calls);
    }
}

#[tokio::test]
async fn native_admission_rejects_restored_unassociated_affected_file_in_both_profiles() {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::with_mode(owner, "inputs");
        std::fs::write(
            fixture.root.path().join("settings-old.json"),
            "{\"original\":true}",
        )
        .unwrap();
        let state = fixture.state().await;
        fixture.prepare(&state).await;
        let calls = fixture.calls();
        restore_affected_file(&fixture);
        let folder = Folder::capture(&fixture.scope.root).unwrap();
        let lease = fixture
            .host
            .inner
            .coordinator
            .acquire(folder)
            .await
            .unwrap();
        let result = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &state, &lease)
            .await;
        assert!(
            result.is_err(),
            "Restoring an unassociated affected file must not authorize native admission: {result:?}"
        );
        assert_eq!(fixture.calls(), calls);
    }
}

#[tokio::test]
async fn signed_affected_file_paths_cannot_be_removed_replaced_or_reordered() {
    let mut fixture = Fixture::with_mode("user:1", "inputs-edit-only");
    std::fs::write(fixture.root.path().join("settings-old.json"), "{}").unwrap();
    let state = fixture.state().await;
    let review = state.language_review.as_ref().unwrap();
    assert!(review.context.files.contains(&"settings-old.json".into()));
    let calls = fixture.calls();
    for change in 0..12 {
        let mut context = review.context.clone();
        match change {
            0 => context.files.clear(),
            1 => context.files.retain(|path| path != "settings-old.json"),
            2 => {
                context.files.push("peer.py".into());
                context.files.sort();
            }
            3 => context.files.reverse(),
            4 => context.identities.clear(),
            5 => context.identities.reverse(),
            6 => context.identities[0] = format!("u1.{}", "0".repeat(64)),
            7 => context.identities[0].clear(),
            8 => context.revisions.clear(),
            9 => context.revisions.reverse(),
            10 => context.revisions[0] = "{}".into(),
            _ => context.revisions[0].clear(),
        }
        let request = Request {
            request: fixture.input.request + change + 1,
            query: Query::ValidateAnalysis { context },
            ..review.input.clone()
        };
        assert!(
            fixture
                .host
                .query_package(fixture.scope.clone(), &request, &fixture.prepared)
                .await
                .is_err()
        );
    }
    assert_eq!(fixture.calls(), calls);
}

#[tokio::test]
async fn flat_text_edits_retain_unassociated_file_evidence_and_preserve_fresh_retry() {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::with_mode(owner, "affected-text");
        std::fs::write(fixture.root.path().join("settings-old.json"), "{}").unwrap();
        let actions = fixture
            .host
            .query_package(fixture.scope.clone(), &fixture.input, &fixture.prepared)
            .await
            .unwrap();
        fixture.input.request += 1;
        fixture.input.query = Query::ReviewAction {
            id: actions.result[0]["id"].as_str().unwrap().into(),
        };
        let response = fixture
            .host
            .query_package(fixture.scope.clone(), &fixture.input, &fixture.prepared)
            .await
            .unwrap();
        assert_eq!(
            response.result["edit"]["files"][0]["path"],
            "settings-old.json"
        );
        let original = response.context.unwrap();
        assert!(original.files.contains(&"settings-old.json".into()));
        let validation = |request, context| Request {
            request,
            query: Query::ValidateAnalysis { context },
            ..fixture.input.clone()
        };
        fixture
            .host
            .query_package(
                fixture.scope.clone(),
                &validation(3, original.clone()),
                &fixture.prepared,
            )
            .await
            .unwrap();
        let calls = fixture.calls();
        restore_affected_file(&fixture);
        assert!(
            fixture
                .host
                .query_package(
                    fixture.scope.clone(),
                    &validation(4, original),
                    &fixture.prepared
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.calls(), calls);
        // A new explicit review on the same live service observes the current
        // metadata. Validation itself never refreshes or substitutes a result.
        fixture.input.request = 5;
        let fresh = fixture
            .host
            .query_package(fixture.scope.clone(), &fixture.input, &fixture.prepared)
            .await
            .unwrap();
        let checked = Request {
            request: 6,
            query: Query::ValidateAnalysis {
                context: fresh.context.unwrap(),
            },
            ..fixture.input.clone()
        };
        fixture
            .host
            .query_package(fixture.scope.clone(), &checked, &fixture.prepared)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn metadata_changes_after_native_verification_reject_before_file_execution() {
    let mut fixture = Fixture::with_mode("user:1", "inputs");
    std::fs::write(fixture.root.path().join("settings-old.json"), "{}").unwrap();
    let state = fixture.state().await;
    fixture.prepare(&state).await;
    let folder = Folder::capture(&fixture.scope.root).unwrap();
    let lease = fixture
        .host
        .inner
        .coordinator
        .acquire(folder)
        .await
        .unwrap();
    let guard = fixture
        .host
        .validate_workspace_review(fixture.scope.clone(), &state, &lease)
        .await
        .unwrap();
    let calls = fixture.calls();
    restore_affected_file(&fixture);
    assert!(guard.ensure_current().is_err());
    assert_eq!(fixture.calls(), calls);
    assert!(!fixture.root.path().join("settings-new.json").exists());
}

#[tokio::test]
async fn command_only_review_accepts_empty_affected_paths_without_weakening_original_checks() {
    use openwebide_core::workspace_transaction::{
        OriginalMutation, ResourceLocation, TransactionFiles,
    };
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::with_mode(owner, "command-only");
        let state = fixture.state().await;
        let review = state.language_review.as_ref().unwrap();
        assert!(state.journal.plan.operations.is_empty());
        assert!(review.context.files.is_empty());
        assert!(review.context.identities.is_empty());
        assert!(review.context.revisions.is_empty());
        fixture.prepare(&state).await;
        let folder = Folder::capture(&fixture.scope.root).unwrap();
        let lease = fixture
            .host
            .inner
            .coordinator
            .acquire(folder)
            .await
            .unwrap();
        let calls = fixture.calls();
        let _guard = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &state, &lease)
            .await
            .unwrap();
        let scope = ResourceScope {
            transaction: state.journal.id.clone(),
            fingerprint: state.journal.plan.fingerprint().unwrap(),
        };
        let files = crate::runs::native_vfs::resources::ResourceFiles::reviewed(
            &std::fs::File::open(fixture.root.path()).unwrap(),
            &state.journal.plan.root,
            scope.clone(),
            state.language_review.as_deref(),
            &state.journal.plan,
        )
        .unwrap();
        // Empty evidence authorizes no original-object access. It must remain
        // a strict review, rather than falling back to the archived IO path.
        assert!(
            files
                .original_resource_matches(
                    &scope,
                    &ResourceLocation::Workspace("main.notes".into()),
                    "",
                    "main.notes",
                    OriginalMutation::default(),
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.calls(), calls);
        assert!(!fixture.root.path().join("executions").exists());
    }
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn bounded_attributes_preserve_text_and_command_reviews_without_native_file_operations() {
    use openwebide_core::{
        workspace_entries::WorkspaceEntries,
        workspace_transaction::{OriginalMutation, ResourceLocation, TransactionFiles},
    };
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::new(owner);
        for index in 0..257 {
            rustix::fs::setxattr(
                fixture.root.path().join("main.notes"),
                format!("user.openwebide-budget-{index}"),
                b"x",
                rustix::fs::XattrFlags::empty(),
            )
            .unwrap();
        }
        let state = fixture.state().await;
        assert!(state.journal.plan.operations.is_empty());
        let context = &state.language_review.as_ref().unwrap().context;
        let index = context
            .files
            .iter()
            .position(|path| path == "main.notes")
            .unwrap();
        let revision: serde_json::Value = serde_json::from_str(&context.revisions[index]).unwrap();
        assert!(revision.get("attributes").is_none());
        fixture.prepare(&state).await;
        let folder = Folder::capture(&fixture.scope.root).unwrap();
        let lease = fixture
            .host
            .inner
            .coordinator
            .acquire(folder)
            .await
            .unwrap();
        let calls = fixture.calls();
        let _guard = fixture
            .host
            .validate_workspace_review(fixture.scope.clone(), &state, &lease)
            .await
            .unwrap();
        let scope = ResourceScope {
            transaction: state.journal.id.clone(),
            fingerprint: state.journal.plan.fingerprint().unwrap(),
        };
        let files = crate::runs::native_vfs::resources::ResourceFiles::reviewed(
            &std::fs::File::open(fixture.root.path()).unwrap(),
            &state.journal.plan.root,
            scope.clone(),
            state.language_review.as_deref(),
            &state.journal.plan,
        )
        .unwrap();
        assert!(
            files
                .original_resource_matches(
                    &scope,
                    &ResourceLocation::Workspace("main.notes".into()),
                    "",
                    "main.notes",
                    OriginalMutation::default(),
                )
                .await
                .is_err()
        );
        assert_eq!(
            files.read_bytes("main.notes").await.unwrap(),
            SOURCE.as_bytes()
        );
        assert_eq!(fixture.calls(), calls);
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("main.notes")).unwrap(),
            SOURCE
        );
        assert!(!fixture.root.path().join("executions").exists());
    }
}

#[tokio::test]
async fn resource_review_requires_persistent_native_identity_without_disabling_ordinary_reads() {
    use openwebide_core::workspace_entries::WorkspaceEntries;
    let mut fixture = Fixture::with_mode("user:1", "inputs");
    std::fs::write(fixture.root.path().join("settings-old.json"), "{}").unwrap();
    let state = fixture.state().await;
    let root = std::fs::File::open(fixture.root.path()).unwrap();
    let scope = ResourceScope {
        transaction: state.journal.id.clone(),
        fingerprint: state.journal.plan.fingerprint().unwrap(),
    };
    for birthless in [false, true] {
        let mut review = state.language_review.as_deref().unwrap().clone();
        if birthless {
            review.context.identities[0].replace_range(..3, "u0.");
        } else {
            review.context.identities[0].clear();
        }
        review.context.validate().unwrap();
        assert!(
            crate::runs::native_vfs::resources::ResourceFiles::reviewed(
                &root,
                &state.journal.plan.root,
                scope.clone(),
                Some(&review),
                &state.journal.plan,
            )
            .is_err()
        );
    }
    let mut partial = state.language_review.as_deref().unwrap().clone();
    let mut revision: serde_json::Value =
        serde_json::from_str(&partial.context.revisions[0]).unwrap();
    revision.as_object_mut().unwrap().remove("attributes");
    partial.context.revisions[0] = serde_json::to_string(&revision).unwrap();
    partial.context.validate().unwrap();
    assert!(
        crate::runs::native_vfs::resources::ResourceFiles::reviewed(
            &root,
            &state.journal.plan.root,
            scope.clone(),
            Some(&partial),
            &state.journal.plan,
        )
        .err()
        .unwrap()
        .contains("attributes are unavailable")
    );
    let ordinary = crate::runs::native_vfs::resources::ResourceFiles::new(
        &root,
        &state.journal.plan.root,
        scope,
    )
    .unwrap();
    assert_eq!(
        ordinary.read_bytes("settings-old.json").await.unwrap(),
        b"{}"
    );
}
