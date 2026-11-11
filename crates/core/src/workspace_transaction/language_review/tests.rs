use super::*;
use crate::{
    WorkspaceMode,
    editor::{Document, EditorRecoveryFile, EditorRecoveryRoot, RecoveryScroll},
    plugins::languages::{
        intelligence::{
            Position, Range,
            actions::ServerCommand,
            results::{FileEdit, TextEdit},
        },
        source_digest,
    },
    workspace_transaction::{EntryState, FileOperation},
};
use serde_json::json;
mod admission;
mod reproduction;

fn fixture() -> (LanguageReview, WorkspacePlan, EditorRecoveryPatch) {
    let mut prepared = crate::plugins::testing::receipt();
    prepared.manifest.compatibility.plugin_api = 9;
    prepared.manifest.executable = None;
    prepared.manifest.contributions.tools.clear();
    prepared.manifest.contributions.skills.clear();
    prepared.manifest.contributions.languages = serde_json::from_value(json!([{
        "id":"custom", "name":"Custom", "extensions":["rs"],
        "features":{"languageServer":{"command":"custom-lsp"}}
    }]))
    .unwrap();
    let operation = FileOperation::Create {
        path: "new.rs".into(),
        overwrite: false,
        ignore_if_exists: false,
    };
    let plan = WorkspacePlan {
        version: 2,
        root: "a".repeat(64),
        operations: vec![operation.clone()],
        roots: vec!["new.rs".into()],
        before: [("new.rs".into(), EntryState::Missing)].into(),
        after: [("new.rs".into(), EntryState::file([]))].into(),
    };
    let editor = EditorRecoveryPatch {
        revision: 1,
        root: EditorRecoveryRoot {
            mode: WorkspaceMode::Local,
            path: None,
        },
        before: vec![],
        after: vec![EditorRecoveryFile {
            path: "new.rs".into(),
            document: Some(Document::new("changed").recovery()),
            scroll: RecoveryScroll::default(),
            read_only: false,
        }],
        order_before: vec![],
        order_after: vec!["new.rs".into()],
        selected_before: None,
        selected_after: Some("new.rs".into()),
    };
    let result = serde_json::to_value(WorkspaceEdit {
        files: vec![],
        changes: vec![
            WorkspaceChange::File { operation },
            WorkspaceChange::Text {
                file: FileEdit {
                    path: "new.rs".into(),
                    source_digest: source_digest(""),
                    revision: None,
                    edits: vec![TextEdit {
                        range: Range {
                            start: Position::default(),
                            end: Position::default(),
                        },
                        new_text: "changed".into(),
                    }],
                },
            },
        ],
    })
    .unwrap();
    let review = LanguageReview {
        prepared,
        binding_revision: 2,
        input: Request {
            version: protocol::VERSION,
            client: "client".into(),
            document: "document".into(),
            request: 7,
            revision: 3,
            language: "custom".into(),
            path: "main.rs".into(),
            source: "original".into(),
            source_digest: source_digest("original"),
            query: Query::Rename {
                position: Position::default(),
                new_name: "new".into(),
            },
        },
        context: AnalysisContext {
            identities: vec![],
            revisions: vec![],
            files: vec![],
            server: "original-service".into(),
            result_digest: crate::plugins::content_digest(&serde_json::to_vec(&result).unwrap()),
            proof: "opaque.signature".into(),
        },
        result,
        transaction_digest: LanguageReview::transaction_digest(&plan, &editor).unwrap(),
    };
    (review, plan, editor)
}

#[test]
fn durable_review_roundtrips_original_evidence_and_binds_both_transaction_halves() {
    let (review, plan, editor) = fixture();
    review.validate_transaction(&plan, &editor).unwrap();
    let encoded = serde_json::to_string(&review).unwrap();
    assert_eq!(
        serde_json::from_str::<LanguageReview>(&encoded).unwrap(),
        review
    );
    assert_eq!(
        review.transaction_digest,
        crate::plugins::content_digest(&serde_json::to_vec(&(&plan, &editor)).unwrap())
    );
    let mut changed = plan.clone();
    changed.root = "b".repeat(64);
    assert!(review.validate_transaction(&changed, &editor).is_err());
    let mut changed = editor.clone();
    changed.after[0].read_only = true;
    assert!(review.validate_transaction(&plan, &changed).is_err());
    let mut changed = editor.clone();
    changed.revision += 1;
    assert!(review.validate_transaction(&plan, &changed).is_err());
    let mut changed = editor.clone();
    changed.after[0].document = Some(Document::new("replacement").recovery());
    assert!(review.validate_transaction(&plan, &changed).is_err());
}

#[test]
fn result_tampering_and_unrelated_or_old_protocol_queries_cannot_become_reviews() {
    let (review, plan, editor) = fixture();
    let mut changed = review.clone();
    changed.result["changes"][1]["file"]["edits"][0]["new_text"] = json!("tampered");
    assert!(changed.validate_transaction(&plan, &editor).is_err());
    for query in [
        Query::Synchronize,
        Query::ValidateAnalysis {
            context: review.context.clone(),
        },
        Query::Formatting {
            tab_width: 4,
            insert_spaces: true,
        },
        Query::CodeActions {
            range: Range::default(),
        },
    ] {
        let mut changed = review.clone();
        changed.input.query = query;
        assert!(changed.validate().is_err());
    }
    let mut changed = review.clone();
    changed.input.version = 1;
    assert!(changed.validate().is_err());
    let mut changed = review.clone();
    changed.binding_revision = 0;
    assert!(changed.validate().is_err());
    let mut changed = serde_json::to_value(&review).unwrap();
    changed["grant"] = json!("not-persisted-authority");
    assert!(serde_json::from_value::<LanguageReview>(changed).is_err());
}

#[test]
fn matching_transaction_digest_cannot_substitute_other_resource_operations() {
    let (mut review, mut plan, editor) = fixture();
    plan.operations[0] = FileOperation::Create {
        path: "new.rs".into(),
        overwrite: true,
        ignore_if_exists: false,
    };
    review.transaction_digest = LanguageReview::transaction_digest(&plan, &editor).unwrap();
    assert!(review.validate_transaction(&plan, &editor).is_err());
}

#[test]
fn command_review_keeps_exact_arguments_and_original_action_and_service() {
    let (mut review, plan, editor) = fixture();
    let edit = review.workspace_edit().unwrap();
    let action = ActionReview {
        title: "Create and run".into(),
        edit,
        command: Some(ServerCommand {
            id: "c".repeat(32),
            server: review.context.server.clone(),
            name: "fixture.run".into(),
            arguments: vec![json!({"value":"original"})],
        }),
    };
    review.input.query = Query::ReviewAction { id: "c".repeat(32) };
    review.result = serde_json::to_value(&action).unwrap();
    review.context.result_digest =
        crate::plugins::content_digest(&serde_json::to_vec(&review.result).unwrap());
    review.validate_transaction(&plan, &editor).unwrap();
    let mut changed = review.clone();
    changed.result["command"]["arguments"][0]["value"] = json!("replacement");
    assert!(changed.validate().is_err());
    for (id, server) in [
        ("d".repeat(32), review.context.server.clone()),
        ("c".repeat(32), "replacement-service".into()),
    ] {
        let mut changed = review.clone();
        changed.result["command"]["id"] = json!(id);
        changed.result["command"]["server"] = json!(server);
        changed.context.result_digest =
            crate::plugins::content_digest(&serde_json::to_vec(&changed.result).unwrap());
        assert!(changed.validate().is_err());
    }
}

#[test]
fn escaped_admitted_source_uses_the_request_budget_instead_of_the_small_scope_budget() {
    let (mut review, plan, editor) = fixture();
    review.input.source = "\0".repeat(crate::plugins::languages::MAX_SOURCE_BYTES);
    review.input.source_digest = source_digest(&review.input.source);
    assert!(serde_json::to_vec(&review).unwrap().len() > super::super::RECOVERY_SCOPE_BODY_LIMIT);
    review.validate_transaction(&plan, &editor).unwrap();
    review.result = json!({"files":[{
        "path":"new.rs","source_digest":source_digest(""),"revision":null,
        "edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},
            "new_text":"x".repeat(protocol::MAX_RESULT_BYTES)}]
    }]});
    review.context.result_digest =
        crate::plugins::content_digest(&serde_json::to_vec(&review.result).unwrap());
    assert!(review.validate().unwrap_err().contains("oversized"));
}

#[test]
fn optional_transaction_review_keeps_old_wire_records_readable_without_rewriting_them() {
    use super::super::{
        BeginWorkspaceTransaction, JournalRecord, TransactionProgress, WorkspaceTransactionState,
    };
    let (review, plan, editor) = fixture();
    let mut input = BeginWorkspaceTransaction {
        id: "a".repeat(32),
        plan: plan.clone(),
        editor: editor.clone(),
        language_review: None,
    };
    let old = serde_json::to_value(&input).unwrap();
    assert!(old.get("language_review").is_none());
    assert!(
        serde_json::from_value::<BeginWorkspaceTransaction>(old)
            .unwrap()
            .language_review
            .is_none()
    );
    input.language_review = Some(Box::new(review.clone()));
    let encoded = serde_json::to_value(&input).unwrap();
    assert_eq!(
        encoded["language_review"],
        serde_json::to_value(&review).unwrap()
    );
    let mut state = WorkspaceTransactionState {
        journal: JournalRecord {
            id: input.id,
            generation: "b".repeat(32),
            revision: 1,
            plan,
            progress: TransactionProgress::default(),
        },
        editor,
        editor_revision: 1,
        language_review: None,
    };
    let old = serde_json::to_value(&state).unwrap();
    assert!(old.get("language_review").is_none());
    assert!(
        serde_json::from_value::<WorkspaceTransactionState>(old)
            .unwrap()
            .language_review
            .is_none()
    );
    state.language_review = Some(Box::new(review));
    assert_eq!(
        serde_json::from_value::<WorkspaceTransactionState>(serde_json::to_value(&state).unwrap())
            .unwrap()
            .language_review,
        state.language_review
    );
}

#[test]
fn retained_workspace_reviews_share_command_provider_selection_and_revocation_policy() {
    use crate::plugins::languages::{
        LanguagePreferences, LanguageRegistry, ProviderPreference, RegisteredLanguage,
    };
    let (review, _, _) = fixture();
    let namespace = format!(
        "plugin:{}/language:{}",
        review.prepared.storage_namespace(),
        review.input.language
    );
    let provider = RegisteredLanguage {
        namespace: namespace.clone(),
        plugin_namespace: review.prepared.storage_namespace(),
        plugin_label: "Custom".into(),
        digest: review.prepared.digest.clone(),
        installation_revision: review.binding_revision,
        host_id: review.prepared.host_id.clone(),
        plugin_api: review.prepared.manifest.compatibility.plugin_api,
        definition: review.prepared.manifest.contributions.languages[0].clone(),
    };
    let registry = LanguageRegistry {
        providers: vec![provider],
        errors: vec![],
    };
    review
        .validate_provider(&registry, &LanguagePreferences::default())
        .unwrap();
    let mut disabled = LanguagePreferences::default();
    disabled
        .providers
        .insert("custom".into(), ProviderPreference::Disabled);
    assert!(review.validate_provider(&registry, &disabled).is_err());
    for field in ["revision", "digest", "missing"] {
        let mut changed = registry.clone();
        match field {
            "revision" => changed.providers[0].installation_revision += 1,
            "digest" => changed.providers[0].digest = "f".repeat(64),
            _ => changed.providers.clear(),
        }
        assert!(
            review
                .validate_provider(&changed, &LanguagePreferences::default())
                .is_err()
        );
    }
}

#[test]
fn archived_protocol_two_evidence_keeps_its_original_serialization_and_identity() {
    let (mut review, plan, editor) = fixture();
    review.input.version = 2;
    assert!(review.input.validate().is_err());
    review.validate_transaction(&plan, &editor).unwrap();
    let original = serde_json::to_string(&review).unwrap();
    assert!(
        serde_json::to_value(&review.context)
            .unwrap()
            .get("files")
            .is_none()
    );
    let digest = review.fingerprint().unwrap();
    let restored: LanguageReview = serde_json::from_str(&original).unwrap();
    restored.validate_transaction(&plan, &editor).unwrap();
    assert_eq!(restored.fingerprint().unwrap(), digest);
    assert_eq!(serde_json::to_string(&restored).unwrap(), original);
}
