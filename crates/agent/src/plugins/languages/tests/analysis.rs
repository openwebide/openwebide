use super::*;

fn fixture(
    profile: &str,
) -> (
    Host,
    LanguageService<Host>,
    LanguageDocumentRequest,
    server_protocol::Query,
    server_protocol::Response,
) {
    let (host, mut bindings, _, _) = setup(profile);
    let manifest = &mut bindings[0].prepared.manifest;
    manifest.compatibility.plugin_api = 6;
    manifest.executable = None;
    manifest.contributions.languages[0].features.syntax = false;
    manifest.contributions.languages[0].features.language_server =
        Some(serde_json::from_value(serde_json::json!({"command":"community-lsp"})).unwrap());
    let primary_namespace = bindings[0].prepared.storage_namespace();
    let mut formatter = bindings[0].clone();
    formatter.id = 2;
    formatter.prepared.manifest.name = "formatter".into();
    formatter.prepared.source.path = "plugins/formatter".into();
    formatter.prepared.digest = "c".repeat(64);
    let formatter_namespace = formatter.prepared.storage_namespace();
    bindings.push(formatter);
    let registry = LanguageRegistry::from_bindings(&bindings);
    let primary = registry
        .providers
        .iter()
        .find(|provider| provider.plugin_namespace == primary_namespace)
        .unwrap()
        .clone();
    let formatter = registry
        .providers
        .iter()
        .find(|provider| provider.plugin_namespace == formatter_namespace)
        .unwrap();
    let mut preferences = LanguagePreferences::default();
    preferences.providers.insert(
        "custom".into(),
        ProviderPreference::Provider {
            namespace: primary.namespace.clone(),
        },
    );
    preferences.formatters.insert(
        "custom".into(),
        ProviderPreference::Provider {
            namespace: formatter.namespace.clone(),
        },
    );
    let query = server_protocol::Query::Formatting {
        tab_width: 4,
        insert_spaces: true,
    };
    let service =
        LanguageService::new_with_preferences(host.clone(), bindings, primary, preferences)
            .unwrap()
            .with_sequence(Arc::new(AtomicU64::new(1)))
            .service_for_query(&query)
            .unwrap();
    let document = LanguageDocumentRequest {
        document: "document".into(),
        path: "file.custom".into(),
        revision: 1,
        source: Arc::new("unsaved source".into()),
        tab_width: 4,
    };
    let result = serde_json::json!([]);
    let response = server_protocol::Response {
        version: server_protocol::VERSION,
        client: "client".into(),
        document: document.document.clone(),
        request: 1,
        revision: document.revision,
        source_digest: source_digest(&document.source),
        server: "test-native-service".into(),
        context: Some(server_protocol::AnalysisContext {
            identities: vec![],
            revisions: vec![],
            files: vec![],
            server: "test-native-service".into(),
            proof: "fixture".into(),
            result_digest: openwebide_core::plugins::content_digest(
                &serde_json::to_vec(&result).unwrap(),
            ),
        }),
        capabilities: serde_json::json!({}),
        result,
        diagnostics: vec![],
        diagnostics_ready: false,
    };
    (host, service, document, query, response)
}

#[test]
fn retained_analysis_keeps_the_selected_formatter_body_and_authority_on_both_hosts() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let (host, service, document, query, mut response) = fixture(profile);
            let analysis = service
                .retain_analysis("client", document, query.clone(), &response)
                .unwrap()
                .unwrap();
            assert_eq!(analysis.provider(), service.provider());
            assert_eq!(analysis.origin().query, query);
            assert_eq!(analysis.origin().source, "unsaved source");
            assert_eq!(analysis.origin().request, 1);
            assert_eq!(analysis.context(), response.context.as_ref().unwrap());
            assert_eq!(host.preparations.load(Ordering::Relaxed), 0);
            assert_eq!(host.grants.load(Ordering::Relaxed), 0);
            assert!(host.server_calls.lock().unwrap().is_empty());
            response.result = serde_json::json!({"altered":true});
            assert_eq!(analysis.result(), &serde_json::json!([]));
            drop(service);
            analysis.validate().await.unwrap();
            {
                let calls = host.server_calls.lock().unwrap();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].prepared.host_id, profile);
                assert_eq!(calls[0].prepared.manifest.name, "formatter");
                assert!(
                    matches!(&calls[0].input.query,server_protocol::Query::ValidateAnalysis { context } if context == analysis.context())
                );
            }
            assert_eq!(host.starts.load(Ordering::Relaxed), 0);
            host.deny_grant.store(true, Ordering::Relaxed);
            assert!(analysis.validate().await.is_err());
            assert_eq!(host.server_calls.lock().unwrap().len(), 1);
            assert_eq!(host.grants.load(Ordering::Relaxed), 2);
        }
    });
}

#[test]
fn retaining_analysis_rejects_mismatched_or_changed_replies_before_authority() {
    for profile in ["server", "paired"] {
        let (host, service, document, query, response) = fixture(profile);
        for changed in 0..5 {
            let mut reply = response.clone();
            match changed {
                0 => reply.client = "other-client".into(),
                1 => reply.document = "other-document".into(),
                2 => reply.revision += 1,
                3 => reply.result = serde_json::Value::Null,
                _ => reply.server = "replacement-server".into(),
            }
            assert!(
                service
                    .retain_analysis("client", document.clone(), query.clone(), &reply)
                    .is_err()
            );
        }
        let mut unproved = response;
        unproved.context = None;
        assert!(
            service
                .retain_analysis("client", document.clone(), query.clone(), &unproved)
                .unwrap()
                .is_none()
        );
        unproved.client = "other-client".into();
        assert!(
            service
                .retain_analysis("client", document.clone(), query.clone(), &unproved)
                .is_err()
        );
        assert_eq!(host.preparations.load(Ordering::Relaxed), 0);
        assert_eq!(host.grants.load(Ordering::Relaxed), 0);
        assert!(host.server_calls.lock().unwrap().is_empty());
        host.current.store(false, Ordering::Relaxed);
        assert!(
            service
                .retain_analysis("client", document, query, &unproved)
                .is_err()
        );
    }
}

#[test]
fn retained_language_review_captures_original_evidence_without_new_queries_or_grants() {
    use openwebide_core::{
        WorkspaceMode,
        editor::EditorRecoveryRoot,
        workspace_transaction::{EditorRecoveryPatch, EntryState, FileOperation, WorkspacePlan},
    };
    for profile in ["server", "paired"] {
        let (host, service, document, _, mut response) = fixture(profile);
        let operation = FileOperation::Create {
            path: "new.custom".into(),
            overwrite: false,
            ignore_if_exists: false,
        };
        let plan = WorkspacePlan {
            version: 2,
            root: "a".repeat(64),
            operations: vec![operation.clone()],
            roots: vec!["new.custom".into()],
            before: [("new.custom".into(), EntryState::Missing)].into(),
            after: [("new.custom".into(), EntryState::file([]))].into(),
        };
        let editor = EditorRecoveryPatch {
            revision: 1,
            root: EditorRecoveryRoot {
                mode: if profile == "paired" {
                    WorkspaceMode::Local
                } else {
                    WorkspaceMode::Remote
                },
                path: (profile == "server").then(|| "project".into()),
            },
            before: vec![],
            after: vec![],
            order_before: vec![],
            order_after: vec![],
            selected_before: None,
            selected_after: None,
        };
        response.result = serde_json::json!({"changes":[{"kind":"file","operation":operation}]});
        response.context.as_mut().unwrap().result_digest = openwebide_core::plugins::content_digest(
            &serde_json::to_vec(&response.result).unwrap(),
        );
        let input = server_protocol::Query::Rename {
            position: Default::default(),
            new_name: "next".into(),
        };
        let analysis = service
            .retain_analysis("client", document, input, &response)
            .unwrap()
            .unwrap();
        let review = analysis.language_review(&plan, &editor).unwrap();
        assert_eq!(review.prepared.manifest.name, "formatter");
        assert_eq!(review.prepared.host_id, profile);
        assert_eq!(
            review.binding_revision,
            analysis.provider().installation_revision
        );
        assert_eq!(review.input, analysis.origin());
        assert_eq!(&review.context, analysis.context());
        assert_eq!(&review.result, analysis.result());
        review.validate_transaction(&plan, &editor).unwrap();
        response.result = serde_json::json!({"changed":true});
        assert_ne!(response.result, review.result);
        let mut other = plan.clone();
        other.operations[0] = FileOperation::Create {
            path: "new.custom".into(),
            overwrite: true,
            ignore_if_exists: false,
        };
        assert!(analysis.language_review(&other, &editor).is_err());
        assert_eq!(host.preparations.load(Ordering::Relaxed), 0);
        assert_eq!(host.grants.load(Ordering::Relaxed), 0);
        assert!(host.server_calls.lock().unwrap().is_empty());
    }
}
