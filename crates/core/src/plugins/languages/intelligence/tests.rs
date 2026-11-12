use super::*;

#[test]
fn analysis_contexts_bind_result_data_and_validation_requires_an_empty_original_service_ack() {
    let mut input = request();
    let result = serde_json::json!({"edit":"reviewed"});
    let context = AnalysisContext {
        identities: vec![],
        revisions: vec![],
        files: vec![],
        server: "service".into(),
        result_digest: crate::plugins::content_digest(&serde_json::to_vec(&result).unwrap()),
        proof: "opaque.signature".into(),
    };
    let mut response = Response {
        version: VERSION,
        client: input.client.clone(),
        document: input.document.clone(),
        request: input.request,
        revision: input.revision,
        source_digest: input.source_digest.clone(),
        server: context.server.clone(),
        context: Some(context.clone()),
        capabilities: serde_json::json!({}),
        result: result.clone(),
        diagnostics: vec![],
        diagnostics_ready: true,
    };
    response.validate(&input).unwrap();
    response.result = serde_json::Value::Null;
    assert!(response.validate(&input).is_err());
    response.result = result.clone();
    response.server = "other".into();
    assert!(response.validate(&input).is_err());
    for proof in [
        String::new(),
        "A".repeat(2049),
        "invalid\nproof".into(),
        "文".into(),
    ] {
        let invalid = AnalysisContext {
            proof,
            ..context.clone()
        };
        assert!(invalid.validate_result(&result).is_err());
    }
    input.query = Query::ValidateAnalysis {
        context: context.clone(),
    };
    input.validate().unwrap();
    assert!(input.query.method().is_none());
    assert!(!input.query.produces_analysis());
    response.context = None;
    response.server = context.server;
    assert!(response.validate(&input).is_err());
    response.result = serde_json::Value::Null;
    response.validate(&input).unwrap();
    input.version = VERSION - 1;
    assert!(input.validate().unwrap_err().contains("protocol version"));
}

#[test]
fn prefetch_is_limited_to_read_only_diagnostics_and_symbols() {
    for (kind, foreground) in [
        (PrefetchKind::Diagnostics, Query::Synchronize),
        (PrefetchKind::Symbols, Query::Symbols),
    ] {
        let mut input = request();
        input.query = Query::Prefetch { kind, close: false };
        input.validate().unwrap();
        assert!(input.query.is_prefetch());
        assert_eq!(input.query.foreground_query(), Some(foreground.clone()));
        assert_eq!(input.query.method(), foreground.method());
        let serialized = serde_json::to_value(&input).unwrap();
        assert_eq!(
            serde_json::from_value::<Request>(serialized).unwrap(),
            input
        );
        input.source.push('!');
        assert!(input.validate().is_err());
    }
    for value in [
        serde_json::json!({"operation":"prefetch","kind":"completion"}),
        serde_json::json!({"operation":"prefetch","kind":"rename"}),
        serde_json::json!({"operation":"prefetch","kind":"symbols","position":{"line":0,"character":0}}),
    ] {
        assert!(serde_json::from_value::<Query>(value).is_err());
    }
}

#[test]
fn closed_file_prefetch_is_explicit_and_open_prefetch_keeps_its_original_wire_shape() {
    for (name, kind) in [
        ("diagnostics", PrefetchKind::Diagnostics),
        ("symbols", PrefetchKind::Symbols),
    ] {
        let legacy = serde_json::json!({"operation":"prefetch","kind":name});
        let open: Query = serde_json::from_value(legacy.clone()).unwrap();
        assert!(!open.closes_prefetch());
        assert_eq!(serde_json::to_value(&open).unwrap(), legacy);
        let closed = Query::Prefetch { kind, close: true };
        assert!(closed.is_prefetch() && closed.closes_prefetch());
        assert_eq!(closed.foreground_query(), open.foreground_query());
        let wire = serde_json::to_value(&closed).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"operation":"prefetch","kind":name,"close":true})
        );
        assert_eq!(serde_json::from_value::<Query>(wire).unwrap(), closed);
        assert!(
            serde_json::from_value::<Query>(serde_json::json!({
                "operation":"prefetch","kind":name,"close":"true"
            }))
            .is_err()
        );
    }
    assert!(!Query::Close.closes_prefetch());
    assert!(
        serde_json::from_value::<Query>(serde_json::json!({
            "operation":"hover","position":{"line":0,"character":0},"close":true
        }))
        .is_err()
    );
}

#[test]
fn workspace_search_and_extended_navigation_keep_bounded_source_bound_requests() {
    let mut input = request();
    for query in ["", "文 symbol", "foo::Bar"] {
        input.query = Query::WorkspaceSymbols {
            query: query.into(),
        };
        input.validate().unwrap();
        assert_eq!(input.query.method(), Some("workspace/symbol"));
    }
    for query in ["x".repeat(257), "bad\nquery".into(), "bad\0query".into()] {
        input.query = Query::WorkspaceSymbols { query };
        assert!(input.validate().is_err());
    }
    for query in [
        Query::TypeDefinition {
            position: Position {
                line: 999,
                character: 0,
            },
        },
        Query::Implementation {
            position: Position {
                line: 999,
                character: 0,
            },
        },
    ] {
        input.query = query;
        assert!(input.validate().is_err());
    }
}

#[test]
fn selection_formatting_validates_utf16_ranges_and_indentation_before_transport() {
    let mut input = request();
    input.source = "a😀\r\n水".into();
    input.source_digest = crate::plugins::languages::source_digest(&input.source);
    let range = Range {
        start: position(&input.source, 1).unwrap(),
        end: position(&input.source, 5).unwrap(),
    };
    input.query = Query::RangeFormatting {
        range,
        tab_width: 4,
        insert_spaces: true,
    };
    input.validate().unwrap();
    assert_eq!(input.query.method(), Some("textDocument/rangeFormatting"));
    for width in [0, 33, u32::MAX] {
        input.query = Query::RangeFormatting {
            range,
            tab_width: width,
            insert_spaces: true,
        };
        assert!(input.validate().is_err());
    }
    input.query = Query::RangeFormatting {
        range: Range {
            start: Position {
                line: 0,
                character: 2,
            },
            ..range
        },
        tab_width: 4,
        insert_spaces: true,
    };
    assert!(
        input.validate().is_err(),
        "half-surrogate ranges cannot reach a provider"
    );
    input.query = Query::RangeFormatting {
        range: Range {
            start: range.end,
            end: range.start,
        },
        tab_width: 4,
        insert_spaces: true,
    };
    assert!(
        input.validate().is_err(),
        "reversed ranges cannot reach a provider"
    );
}

#[test]
fn dialect_language_ids_require_api_seven_and_share_only_the_same_installed_server() {
    use crate::plugins::{ProjectPlugin, languages::*};
    let mut prepared = crate::plugins::testing::receipt();
    prepared.manifest.executable = None;
    prepared.manifest.compatibility.plugin_api = 7;
    prepared.manifest.contributions.languages = serde_json::from_value(serde_json::json!([
        {"id":"js", "name":"Script", "extensions":["js"], "features":{"languageServer":{"command":"community-server","languageId":"javascript"}}},
        {"id":"jsx", "name":"Script JSX", "extensions":["jsx"], "features":{"languageServer":{"command":"community-server","languageId":"javascriptreact"}}}
    ])).unwrap();
    prepared.validate().unwrap();
    for api in [5, 6] {
        let mut old = prepared.clone();
        old.manifest.compatibility.plugin_api = api;
        assert!(old.validate().is_err());
    }
    for value in ["", "javascript react", "javascript\nreact", &"x".repeat(65)] {
        let mut invalid = prepared.clone();
        invalid.manifest.contributions.languages[0]
            .features
            .language_server
            .as_mut()
            .unwrap()
            .language_id = Some(value.into());
        assert!(invalid.validate().is_err());
    }
    let registry = LanguageRegistry::from_bindings(&[ProjectPlugin {
        id: 1,
        revision: 1,
        enabled: true,
        prepared,
    }]);
    let (left, right) = (&registry.providers[0], &registry.providers[1]);
    assert!(left.shares_server(right));
    assert!(right.shares_server(left));
    for field in [
        "package",
        "digest",
        "revision",
        "host",
        "command",
        "settings",
        "root",
        "options",
        "resources",
        "api",
    ] {
        let mut other = right.clone();
        match field {
            "package" => other.plugin_namespace.push_str("-other"),
            "digest" => other.digest = "b".repeat(64),
            "revision" => other.installation_revision += 1,
            "host" => other.host_id.push_str("-other"),
            "api" => other.plugin_api = 5,
            field => {
                let server = other.definition.features.language_server.as_mut().unwrap();
                match field {
                    "command" => server.command.push_str("-other"),
                    "settings" => server.settings = serde_json::json!({"different":true}),
                    "root" => server.root_markers.push("other.json".into()),
                    "resources" => server.virtual_documents.push(
                        serde_json::from_value(
                            serde_json::json!({"scheme":"notes", "method":"notes/source"}),
                        )
                        .unwrap(),
                    ),
                    "options" => {
                        server.initialization_options = serde_json::json!({"different":true});
                    }
                    _ => unreachable!(),
                }
            }
        }
        assert!(!left.shares_server(&other), "{field}");
    }
}

#[test]
fn utf16_positions_preserve_unicode_crlf_and_empty_final_row() {
    let source = "a😀é\r\n水\n";
    for byte in [0, 1, 5, 7, 9, 12, 13] {
        let point = position(source, byte).unwrap();
        assert_eq!(offset(source, point).unwrap(), byte);
    }
    assert_eq!(
        position(source, 5).unwrap(),
        Position {
            line: 0,
            character: 3
        }
    );
    for point in [
        Position {
            line: 0,
            character: 2,
        },
        Position {
            line: 0,
            character: 5,
        },
        Position {
            line: 3,
            character: 0,
        },
    ] {
        assert!(offset(source, point).is_err());
    }
    for byte in [2, 3, 4, 6, 8, 10, 11, 14] {
        assert!(position(source, byte).is_err(), "byte {byte}");
    }
    assert_eq!(offset("", Position::default()).unwrap(), 0);
}

fn request() -> Request {
    let source = "fn main() {}\n".to_string();
    Request {
        version: VERSION,
        client: "window-a".into(),
        document: "tab-a".into(),
        request: 1,
        revision: 1,
        language: "rust".into(),
        path: "src/main.rs".into(),
        source_digest: crate::plugins::languages::source_digest(&source),
        source,
        query: Query::Synchronize,
    }
}

#[test]
fn requests_reject_forged_source_paths_positions_and_protocol() {
    let valid = request();
    valid.validate().unwrap();
    for path in [
        "/tmp/main.rs",
        "../main.rs",
        "src/../main.rs",
        "C:/main.rs",
        "src\\main.rs",
        "src//main.rs",
        "src/./main.rs",
    ] {
        let mut invalid = valid.clone();
        invalid.path = path.into();
        assert!(invalid.validate().is_err(), "{path}");
    }
    let mut invalid = valid.clone();
    invalid.source.push('x');
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.version += 1;
    assert!(invalid.validate().is_err());
    invalid = valid;
    invalid.query = Query::Hover {
        position: Position {
            line: 9,
            character: 0,
        },
    };
    assert!(invalid.validate().is_err());
}

#[test]
fn old_client_document_request_revision_and_digest_cannot_publish_results() {
    let request = request();
    let valid = Response {
        version: VERSION,
        client: request.client.clone(),
        document: request.document.clone(),
        request: request.request,
        revision: request.revision,
        source_digest: request.source_digest.clone(),
        server: "native-service-a".into(),
        context: None,
        capabilities: serde_json::json!({}),
        result: serde_json::Value::Null,
        diagnostics: vec![],
        diagnostics_ready: false,
    };
    valid.validate(&request).unwrap();
    for field in ["client", "document", "request", "revision", "source_digest"] {
        let mut json = serde_json::to_value(&valid).unwrap();
        json[field] = if matches!(field, "request" | "revision") {
            serde_json::json!(2)
        } else {
            serde_json::json!("another")
        };
        let stale: Response = serde_json::from_value(json).unwrap();
        assert!(stale.validate(&request).is_err(), "{field}");
    }
}

#[test]
fn reviewed_action_control_frames_reject_invalid_proofs_and_replacement_services() {
    let action = actions::ActionSynchronization {
        command: "a".repeat(32),
        server: "original-service".into(),
        origin: "b".repeat(64),
        review: "c".repeat(64),
    };
    for query in [
        Query::PrepareAction {
            action: action.clone(),
        },
        Query::SynchronizeAction {
            action: action.clone(),
            close: false,
        },
        Query::ReleaseAction {
            action: action.clone(),
        },
    ] {
        let mut input = request();
        input.query = query;
        input.validate().unwrap();
        let mut response = Response {
            version: VERSION,
            client: input.client.clone(),
            document: input.document.clone(),
            request: input.request,
            revision: input.revision,
            source_digest: input.source_digest.clone(),
            server: action.server.clone(),
            context: None,
            capabilities: serde_json::json!({}),
            result: serde_json::Value::Null,
            diagnostics: vec![],
            diagnostics_ready: true,
        };
        response.validate(&input).unwrap();
        response.server = "replacement-service".into();
        assert!(response.validate(&input).is_err());
    }
    for field in ["command", "server", "origin", "review"] {
        let mut invalid = serde_json::to_value(&action).unwrap();
        invalid[field] = serde_json::json!("");
        let invalid = serde_json::from_value(invalid).unwrap();
        let mut input = request();
        input.query = Query::PrepareAction { action: invalid };
        assert!(input.validate().is_err(), "{field}");
    }
}

#[test]
fn server_only_packages_require_opt_in_and_obey_provider_disablement() {
    use crate::plugins::{ProjectPlugin, languages::*};
    let mut prepared = crate::plugins::testing::receipt();
    prepared.manifest.executable = None;
    prepared.manifest.contributions.languages = serde_json::from_value(serde_json::json!([{
        "id":"notes", "name":"Notes", "extensions":["notes"],
        "features":{"languageServer":{"command":"notes-lsp","args":["--stdio"],"rootMarkers":["notes.json"]}}
    }])).unwrap();
    prepared.manifest.compatibility.plugin_api = 6;
    prepared.manifest.validate().unwrap();
    assert_eq!(
        declared_server(&prepared, "notes").unwrap().command,
        "notes-lsp"
    );
    assert!(declared_server(&prepared, "rust").is_err());
    let mut binding = ProjectPlugin {
        id: 1,
        revision: 1,
        enabled: true,
        prepared,
    };
    let preferences = LanguagePreferences::default();
    let registry = LanguageRegistry::from_bindings(&[binding.clone()]);
    assert!(matches!(
        registry.select("file.notes", "", None, &preferences),
        LanguageSelection::Plain
    ));
    assert!(matches!(
        registry.select_server("file.notes", "", None, &preferences),
        LanguageSelection::Selected(_)
    ));
    assert!(matches!(
        registry.formatter("notes", &preferences),
        LanguageSelection::Selected(_)
    ));
    let mut formatter_disabled = preferences.clone();
    formatter_disabled
        .formatters
        .insert("notes".into(), ProviderPreference::Disabled);
    assert!(matches!(
        registry.formatter("notes", &formatter_disabled),
        LanguageSelection::Plain
    ));
    assert!(matches!(
        registry.server("notes", &preferences),
        LanguageSelection::Selected(_)
    ));
    let mut disabled = preferences.clone();
    disabled
        .providers
        .insert("notes".into(), ProviderPreference::Disabled);
    assert!(matches!(
        registry.server("notes", &disabled),
        LanguageSelection::Plain
    ));
    binding.prepared.manifest.compatibility.plugin_api = 5;
    binding.prepared.manifest.validate().unwrap();
    assert!(declared_server(&binding.prepared, "notes").is_err());
    let registry = LanguageRegistry::from_bindings(&[binding.clone()]);
    assert!(matches!(
        registry.server("notes", &preferences),
        LanguageSelection::Plain
    ));
    binding.prepared.manifest.compatibility.plugin_api = 6;
    binding.enabled = false;
    let registry = LanguageRegistry::from_bindings(&[binding]);
    assert!(matches!(
        registry.server("notes", &preferences),
        LanguageSelection::Plain
    ));
}

#[test]
fn virtual_documents_require_api_eight_and_validate_the_public_resource_contract() {
    use crate::plugins::languages::*;
    let mut prepared = crate::plugins::testing::receipt();
    prepared.manifest.executable = None;
    prepared.manifest.compatibility.plugin_api = 8;
    prepared.manifest.contributions.languages = serde_json::from_value(serde_json::json!([
        {"id":"notes", "name":"Notes", "extensions":["notes"], "features":{"languageServer":{
            "command":"community-server", "virtualDocuments":[{"scheme":"notes", "method":"notes/source", "textPointer":"/body~1text/~0value"}]
        }}}
    ])).unwrap();
    prepared.validate().unwrap();
    for api in [5, 6, 7] {
        let mut old = prepared.clone();
        old.manifest.compatibility.plugin_api = api;
        assert!(old.validate().unwrap_err().to_string().contains("API 8"));
    }
    let server = prepared.manifest.contributions.languages[0]
        .features
        .language_server
        .as_ref()
        .unwrap();
    let declaration = server.virtual_documents[0].clone();
    assert_eq!(
        server.shared_configuration().virtual_documents,
        server.virtual_documents
    );
    for scheme in ["", "file", "Notes", "notes:", "1notes", &"x".repeat(65)] {
        let mut invalid = declaration.clone();
        invalid.scheme = scheme.into();
        assert!(validate_virtual_documents(&[invalid]).is_err(), "{scheme}");
    }
    for method in [
        "",
        "shutdown",
        "workspace/executeCommand",
        "workspace/applyEdit",
        "textDocument/rename",
        "$/source",
        "notes//source",
        "notes/source\n",
        &"x/".repeat(65),
    ] {
        let mut invalid = declaration.clone();
        invalid.method = method.into();
        assert!(validate_virtual_documents(&[invalid]).is_err(), "{method}");
    }
    for pointer in [
        "source",
        "/~",
        "/~2",
        "/source\n",
        &format!("/{}", "x".repeat(256)),
    ] {
        let mut invalid = declaration.clone();
        invalid.text_pointer = pointer.into();
        assert!(validate_virtual_documents(&[invalid]).is_err(), "{pointer}");
    }
    assert!(validate_virtual_documents(&[declaration.clone(), declaration.clone()]).is_err());
    assert!(validate_virtual_documents(&vec![declaration.clone(); 17]).is_err());
    for scheme in ["notes+source", "notes-source", "notes.source"] {
        for pointer in ["", "/source", "/body~1text/~0value"] {
            let mut valid = declaration.clone();
            valid.scheme = scheme.into();
            valid.text_pointer = pointer.into();
            validate_virtual_documents(&[valid]).unwrap();
        }
    }
    let encoded = serde_json::to_value(server).unwrap();
    for invalid in [
        serde_json::json!({"scheme":"notes", "method":"notes/source", "parameters":"command"}),
        serde_json::json!({"scheme":"notes", "method":"notes/source", "command":"run"}),
    ] {
        let mut invalid_server = encoded.clone();
        invalid_server["virtualDocuments"] = serde_json::json!([invalid]);
        assert!(serde_json::from_value::<LanguageServer>(invalid_server).is_err());
    }
    let without = serde_json::json!({"command":"old-server"});
    let old: LanguageServer = serde_json::from_value(without).unwrap();
    assert!(old.virtual_documents.is_empty());
    assert!(
        serde_json::to_value(old)
            .unwrap()
            .get("virtualDocuments")
            .is_none()
    );
}

#[test]
fn affected_observation_paths_are_bounded_ordered_relative_and_archived_by_default() {
    let mut context = AnalysisContext {
        identities: vec![],
        revisions: vec![],
        files: vec!["".into(), "dir".into(), "dir/file.json".into()],
        server: "original".into(),
        result_digest: "a".repeat(64),
        proof: "opaque.signature".into(),
    };
    context.validate().unwrap();
    for files in [
        vec!["../outside".into()],
        vec!["file".into(), "file".into()],
        vec!["z".into(), "a".into()],
        vec!["x".repeat(MAX_ANALYSIS_PATH_BYTES + 1)],
        vec!["file".into(); MAX_ANALYSIS_FILES + 1],
    ] {
        context.files = files;
        assert!(context.validate().is_err());
    }
    let archived:AnalysisContext=serde_json::from_value(serde_json::json!({"server":"original","result_digest":"a".repeat(64),"proof":"opaque.signature"})).unwrap();
    assert!(archived.files.is_empty());
    assert!(archived.identities.is_empty());
    assert!(archived.revisions.is_empty());
    archived.validate().unwrap();
    assert!(
        serde_json::to_value(&archived)
            .unwrap()
            .get("files")
            .is_none()
    );
    assert!(
        serde_json::to_value(&archived)
            .unwrap()
            .get("identities")
            .is_none()
    );
    for version in [1, 2] {
        let mut input = request();
        input.version = version;
        input.query = Query::ReviewAction { id: "a".repeat(32) };
        input.validate_archived().unwrap();
        assert!(input.validate().is_err());
        input.query = Query::Hover {
            position: Position::default(),
        };
        assert!(input.validate_archived().is_err());
    }
}

#[test]
fn original_object_identities_are_bounded_and_aligned_without_changing_archived_contexts() {
    let valid = AnalysisContext {
        revisions: vec![],
        files: vec!["".into(), "missing".into(), "source".into()],
        identities: vec![
            format!("u1.{}", "a".repeat(64)),
            String::new(),
            format!("w1.{}", "b".repeat(64)),
        ],
        server: "original".into(),
        result_digest: "a".repeat(64),
        proof: "opaque.signature".into(),
    };
    valid.validate().unwrap();
    assert_eq!(
        serde_json::from_value::<AnalysisContext>(serde_json::to_value(&valid).unwrap()).unwrap(),
        valid
    );
    for identities in [
        vec![format!("u1.{}", "a".repeat(64))],
        vec![String::new(); 4],
        vec![String::new(), String::new(), "u1.".into()],
        vec![
            String::new(),
            String::new(),
            format!("v1.{}", "a".repeat(64)),
        ],
        vec![
            String::new(),
            String::new(),
            format!("u1.{}", "z".repeat(64)),
        ],
        vec![
            String::new(),
            String::new(),
            "x".repeat(MAX_ANALYSIS_PATH_BYTES + 1),
        ],
    ] {
        let mut changed = valid.clone();
        changed.identities = identities;
        assert!(changed.validate().is_err());
    }
}

#[test]
fn native_metadata_is_opaque_aligned_bounded_and_omitted_for_archived_contexts() {
    let mut context = AnalysisContext {
        files: vec!["".into(), "missing".into(), "source".into()],
        identities: vec![],
        revisions: vec![
            "opaque root metadata".into(),
            "missing".into(),
            "opaque source metadata".into(),
        ],
        server: "original".into(),
        result_digest: "a".repeat(64),
        proof: "opaque.signature".into(),
    };
    context.validate().unwrap();
    for revisions in [
        vec!["one".into()],
        vec![String::new(); 3],
        vec!["control\0metadata".into(); 3],
        vec!["x".repeat(MAX_ANALYSIS_METADATA_RECORD_BYTES + 1); 3],
    ] {
        let mut changed = context.clone();
        changed.revisions = revisions;
        assert!(changed.validate().is_err());
    }
    context.files = (0..MAX_ANALYSIS_FILES)
        .map(|index| format!("file-{index:05}"))
        .collect();
    context.revisions =
        vec!["m".repeat(MAX_ANALYSIS_METADATA_BYTES / MAX_ANALYSIS_FILES); MAX_ANALYSIS_FILES];
    context.validate().unwrap();
    context.revisions[0].push('x');
    assert!(context.validate().is_err());
    let archived: AnalysisContext = serde_json::from_value(serde_json::json!({"server":"original","result_digest":"a".repeat(64),"proof":"opaque.signature"})).unwrap();
    assert!(
        serde_json::to_value(archived)
            .unwrap()
            .get("revisions")
            .is_none()
    );
}
