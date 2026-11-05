use super::*;

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
