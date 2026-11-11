use super::*;
use crate::languages::host::reuse::tests::{Fixture, input};

fn sources() -> ProjectFiles {
    ProjectFiles::new(
        &[
            serde_json::from_value(json!({"id":"notes","name":"Notes","extensions":["notes"]}))
                .unwrap(),
        ],
        None,
    )
    .unwrap()
}

#[tokio::test]
async fn analysis_context_cannot_be_attached_to_different_result_data() {
    let fixture = Fixture::new();
    let input = input("A", "a.notes", "A unsaved", 1);
    let mut response = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &input, sources())
        .await
        .unwrap();
    assert!(response.context.is_some());
    response.result = json!({"different":"unreviewed result data"});
    assert!(response.validate(&input).is_err());
}
fn validation(input: &Request, context: protocol::AnalysisContext, request: u64) -> Request {
    Request {
        request,
        query: Query::ValidateAnalysis { context },
        ..input.clone()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn physical_folder_proof_must_precede_original_analysis_without_suppressing_source_events() {
    use openwebide_core::workspace_transaction::companion::{
        FolderAction, FolderReply, FolderRequest,
    };

    for prepare_first in [false, true] {
        let fixture = Fixture::new();
        let input = input("A", "a.notes", "A unsaved", 1);
        let mut analysis = None;
        if !prepare_first {
            analysis = Some(
                fixture
                    .host
                    .query_with_sources(fixture.scope.clone(), &input, sources())
                    .await
                    .unwrap(),
            );
        }
        let companion = crate::workspace_transactions::companion::Host::new(
            fixture.host.inner.coordinator.clone(),
        );
        let workspace = fixture.scope.root.parent().unwrap().to_path_buf();
        let request = |action| FolderRequest {
            user: 1,
            project: 1,
            action,
        };
        let opened = companion
            .request(
                workspace.clone(),
                fixture.scope.owner.clone(),
                request(FolderAction::Open {
                    path: fixture
                        .scope
                        .root
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .into(),
                    expected: None,
                }),
            )
            .await
            .unwrap();
        let FolderReply::Opened {
            session, challenge, ..
        } = opened
        else {
            panic!("Expected a physical folder challenge")
        };
        let proof = std::fs::read_to_string(fixture.scope.root.join(challenge)).unwrap();
        assert!(matches!(
            companion
                .request(
                    workspace.clone(),
                    fixture.scope.owner.clone(),
                    request(FolderAction::Prove {
                        session: session.clone(),
                        proof,
                    })
                )
                .await
                .unwrap(),
            FolderReply::Ready { .. }
        ));
        if prepare_first {
            analysis = Some(
                fixture
                    .host
                    .query_with_sources(fixture.scope.clone(), &input, sources())
                    .await
                    .unwrap(),
            );
        }
        let calls = std::fs::read(fixture.scope.root.join("calls")).unwrap();
        let checked = fixture
            .host
            .query_with_sources(
                fixture.scope.clone(),
                &validation(&input, analysis.unwrap().context.unwrap(), 2),
                sources(),
            )
            .await;
        if prepare_first {
            checked.unwrap();
        } else {
            assert!(checked.unwrap_err().contains("analysis context changed"));
        }
        assert_eq!(fixture.starts(), 1);
        assert_eq!(
            std::fs::read(fixture.scope.root.join("calls")).unwrap(),
            calls
        );
        assert_eq!(
            std::fs::read_to_string(fixture.scope.root.join("a.notes")).unwrap(),
            "disk:a.notes"
        );
        companion
            .request(
                workspace,
                fixture.scope.owner.clone(),
                request(FolderAction::Close { session }),
            )
            .await
            .unwrap();
    }
}

#[test]
fn signed_context_is_bounded_and_binds_origin_generation_overlay_service_and_issuer() {
    let analysis = Analysis::default();
    let input = input("A", "a.notes", "unsaved 文😀", 1);
    let observation = generation::Observation { revision: 7 };
    let proof = analysis
        .issue("original", &input, observation, 9, &Value::Null)
        .unwrap();
    let mut wrong_result = proof.clone();
    wrong_result.result_digest = "a".repeat(64);
    assert!(
        analysis
            .verify(&wrong_result, "original", &input, observation, 9)
            .is_err()
    );
    let current = validation(&input, proof.clone(), 2);
    analysis
        .verify(&proof, "original", &current, observation, 9)
        .unwrap();
    assert!(
        Analysis::default()
            .verify(&proof, "original", &current, observation, 9)
            .is_err()
    );
    assert!(
        analysis
            .verify(&proof, "replacement", &current, observation, 9)
            .is_err()
    );
    assert!(
        analysis
            .verify(
                &proof,
                "original",
                &current,
                generation::Observation { revision: 8 },
                9
            )
            .is_err()
    );
    assert!(
        analysis
            .verify(&proof, "original", &current, observation, 10)
            .is_err()
    );
    for field in 0..6 {
        let mut other = current.clone();
        match field {
            0 => other.client = "other".into(),
            1 => other.document = "other".into(),
            2 => other.path = "b.notes".into(),
            3 => other.language = "other".into(),
            4 => other.revision += 1,
            _ => {
                other.source.push('x');
                other.source_digest =
                    openwebide_core::plugins::languages::source_digest(&other.source);
            }
        }
        assert!(
            analysis
                .verify(&proof, "original", &other, observation, 9)
                .is_err()
        );
    }
    for signature in [false, true] {
        let mut altered = proof.clone();
        let index = if signature {
            altered.proof.find('.').unwrap() + 1
        } else {
            0
        };
        let replacement = if altered.proof.as_bytes()[index] == b'A' {
            "B"
        } else {
            "A"
        };
        altered.proof.replace_range(index..index + 1, replacement);
        assert!(
            analysis
                .verify(&altered, "original", &current, observation, 9)
                .is_err()
        );
    }
    let oversized = protocol::AnalysisContext {
        proof: "A".repeat(2049),
        ..proof
    };
    assert!(
        analysis
            .verify(&oversized, "original", &current, observation, 9)
            .is_err()
    );
}

#[test]
fn expiration_does_not_turn_a_later_proof_into_an_older_valid_one() {
    let mut analysis = Analysis::default();
    let input = input("A", "a.notes", "A", 1);
    let observation = generation::Observation { revision: 1 };
    let proof = analysis
        .issue("service", &input, observation, 1, &Value::Null)
        .unwrap();
    analysis.started -= Duration::from_secs(LIFETIME + 1);
    assert!(
        analysis
            .verify(&proof, "service", &input, observation, 1)
            .is_err()
    );
    let renewed = analysis
        .issue("service", &input, observation, 1, &Value::Null)
        .unwrap();
    analysis
        .verify(&renewed, "service", &input, observation, 1)
        .unwrap();
    assert!(
        analysis
            .verify(&proof, "service", &input, observation, 1)
            .is_err()
    );
}

#[tokio::test]
async fn retained_context_checks_never_query_or_replace_the_original_server() {
    let fixture = Fixture::new();
    let input = input("A", "a.notes", "A unsaved", 1);
    let response = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &input, sources())
        .await
        .unwrap();
    let proof = response.context.unwrap();
    let calls = std::fs::read(fixture.root.path().join("calls")).unwrap();
    let request = validation(&input, proof, 2);
    let response = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &request, sources())
        .await
        .unwrap();
    response.validate(&request).unwrap();
    assert!(response.result.is_null());
    assert_eq!(
        std::fs::read(fixture.root.path().join("calls")).unwrap(),
        calls
    );
    fixture.host.shutdown();
    assert!(
        fixture
            .host
            .query_with_sources(
                fixture.scope.clone(),
                &validation(&input, request_context(&request), 3),
                sources()
            )
            .await
            .is_err()
    );
    assert_eq!(fixture.starts(), 1);
}

fn request_context(input: &Request) -> protocol::AnalysisContext {
    let Query::ValidateAnalysis { context } = &input.query else {
        panic!()
    };
    context.clone()
}

#[tokio::test]
async fn dependency_and_other_unsaved_overlay_changes_reject_original_proofs() {
    for disk in [false, true] {
        let fixture = Fixture::new();
        let input = input("A", "a.notes", "A unsaved", 1);
        let response = fixture
            .host
            .query_with_sources(fixture.scope.clone(), &input, sources())
            .await
            .unwrap();
        let proof = response.context.unwrap();
        if disk {
            std::fs::write(
                fixture.root.path().join("b.notes"),
                "external dependency change",
            )
            .unwrap();
        } else {
            let dependency =
                super::super::reuse::tests::input("A", "b.notes", "other unsaved source", 2);
            fixture
                .host
                .query_with_sources(fixture.scope.clone(), &dependency, sources())
                .await
                .unwrap();
        }
        assert!(
            fixture
                .host
                .query_with_sources(
                    fixture.scope.clone(),
                    &validation(&input, proof, 3),
                    sources()
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.starts(), 1);
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("a.notes")).unwrap(),
            "disk:a.notes"
        );
    }
}

#[tokio::test]
async fn a_proof_cannot_move_to_another_owner_project_provider_or_client() {
    let fixture = Fixture::new();
    let input = input("A", "a.notes", "A unsaved", 1);
    let response = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &input, sources())
        .await
        .unwrap();
    let proof = response.context.unwrap();
    for changed in 0..5 {
        let mut scope = fixture.scope.clone();
        let mut request = validation(&input, proof.clone(), 2);
        match changed {
            0 => scope.owner = "other".into(),
            1 => scope.project += 1,
            2 => scope.provider = "other".into(),
            3 => scope.digest = "b".repeat(64),
            _ => request.client = "other".into(),
        }
        assert!(
            fixture
                .host
                .query_with_sources(scope, &request, sources())
                .await
                .is_err()
        );
    }
    assert_eq!(fixture.starts(), 1);
    assert_eq!(fixture.host.inner.entries.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn restored_dependency_bytes_and_modification_time_do_not_restore_old_analysis() {
    let fixture = Fixture::new();
    let dependency = fixture.root.path().join("b.notes");
    let original = std::fs::read(&dependency).unwrap();
    let modified = std::fs::metadata(&dependency).unwrap().modified().unwrap();
    let input = input("A", "a.notes", "A unsaved", 1);
    let response = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &input, sources())
        .await
        .unwrap();
    std::fs::write(&dependency, "intervening dependency").unwrap();
    std::fs::write(&dependency, &original).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&dependency)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(std::fs::read(&dependency).unwrap(), original);
    assert_eq!(
        std::fs::metadata(&dependency).unwrap().modified().unwrap(),
        modified
    );
    assert!(
        fixture
            .host
            .query_with_sources(
                fixture.scope.clone(),
                &validation(&input, response.context.unwrap(), 2),
                sources()
            )
            .await
            .is_err()
    );
    assert_eq!(fixture.starts(), 1);
}

#[tokio::test]
async fn closed_documents_and_changed_declared_configuration_cannot_validate_old_results() {
    for configuration in [false, true] {
        let mut fixture = Fixture::new();
        if configuration {
            std::fs::write(fixture.root.path().join("settings.json"), "{}").unwrap();
            fixture.scope.server.project_inputs =
                Some(serde_json::from_value(json!({"files":["settings.json"]})).unwrap());
        }
        let input = input("A", "a.notes", "A unsaved", 1);
        let response = fixture
            .host
            .query_with_sources(fixture.scope.clone(), &input, sources())
            .await
            .unwrap();
        if configuration {
            std::fs::write(
                fixture.root.path().join("settings.json"),
                "{\"changed\":true}",
            )
            .unwrap();
        } else {
            let close = Request {
                request: 2,
                query: Query::Close,
                ..input.clone()
            };
            fixture
                .host
                .query(fixture.scope.clone(), &close)
                .await
                .unwrap();
        }
        assert!(
            fixture
                .host
                .query_with_sources(
                    fixture.scope.clone(),
                    &validation(&input, response.context.unwrap(), 3),
                    sources()
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.starts(), 1);
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("a.notes")).unwrap(),
            "disk:a.notes"
        );
    }
}

#[tokio::test]
async fn changed_project_sources_retire_completion_and_action_handles_before_resolution() {
    let fixture = Fixture::new();
    let peer = fixture.root.path().join("peer.py");
    let script=std::fs::read_to_string(&peer).unwrap()
        .replace("\"hoverProvider\": True", "\"hoverProvider\": True, \"codeActionProvider\": True, \"completionProvider\": {}")
        .replace("    elif method == \"textDocument/hover\":",r#"    elif method == "textDocument/codeAction":
        result = [{'title':'Fix', 'edit':{'changes':{(root/'a.notes').as_uri():[{'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':0}},'newText':'fixed'}]}}}]
    elif method == "textDocument/completion":
        result = [{'label':'Item','insertText':'Item'}]
    elif method == "textDocument/hover":"#);
    std::fs::write(&peer, script).unwrap();
    let input = input("A", "a.notes", "A unsaved", 1);
    let actions = Request {
        query: Query::CodeActions {
            range: protocol::Range::default(),
        },
        ..input.clone()
    };
    let action = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &actions, sources())
        .await
        .unwrap()
        .result[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let completions = Request {
        request: 2,
        query: Query::Completion {
            position: protocol::Position::default(),
        },
        ..input.clone()
    };
    let completion = fixture
        .host
        .query_with_sources(fixture.scope.clone(), &completions, sources())
        .await
        .unwrap()
        .result[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(fixture.root.path().join("b.notes"), "changed dependency").unwrap();
    for (request, query) in [
        (3, Query::ResolveAction { id: action }),
        (4, Query::ResolveCompletion { id: completion }),
    ] {
        assert!(
            fixture
                .host
                .query_with_sources(
                    fixture.scope.clone(),
                    &Request {
                        request,
                        query,
                        ..input.clone()
                    },
                    sources()
                )
                .await
                .is_err()
        );
    }
    let calls = std::fs::read_to_string(fixture.root.path().join("calls")).unwrap();
    assert!(!calls.contains("codeAction/resolve"));
    assert!(!calls.contains("completionItem/resolve"));
    assert_eq!(fixture.starts(), 1);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_admission_binds_the_original_query_and_counter_without_changing_fresh_validation() {
    let analysis = Analysis::default();
    let original = input("A", "a.notes", "source", 1);
    let observation = generation::Observation { revision: 7 };
    let proof = analysis
        .issue(
            "original",
            &original,
            observation,
            9,
            &json!({"result":"retained"}),
        )
        .unwrap();
    analysis
        .verify_original(
            &proof,
            "original",
            &original,
            observation,
            9,
            std::path::Path::new("."),
        )
        .unwrap();
    for query_only in [false, true] {
        let mut changed = original.clone();
        if query_only {
            changed.query = Query::Symbols;
        } else {
            changed.request += 1;
        }
        analysis
            .verify(&proof, "original", &changed, observation, 9)
            .unwrap();
        assert!(
            analysis
                .verify_original(
                    &proof,
                    "original",
                    &changed,
                    observation,
                    9,
                    std::path::Path::new(".")
                )
                .is_err()
        );
    }
    analysis
        .verify(
            &proof,
            "original",
            &validation(&original, proof.clone(), 20),
            observation,
            9,
        )
        .unwrap();
}
