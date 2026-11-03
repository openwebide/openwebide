use super::*;
use openwebide_core::{editor::syntax_contracts::LANGUAGE_CASES, highlight::language_from_path};

fn engine() -> LanguageSyntax {
    let mut grammars = BTreeMap::new();
    let mut definitions = Vec::new();
    for language in SYNTAX_PROVIDERS
        .iter()
        .map(|provider| provider.language)
        .chain([SyntaxLanguage::Plain])
        .chain(cfg!(feature = "sql").then_some(SyntaxLanguage::Sql))
    {
        let id = format!("{language:?}").to_ascii_lowercase();
        if grammars.contains_key(&id) {
            continue;
        }
        grammars.insert(id.clone(), LanguageGrammar::standard(language));
        definitions.push(openwebide_plugin_sdk::serde_json::from_value(openwebide_plugin_sdk::serde_json::json!({"id":id,"name":id,"features":{"syntax":true,"folding":true,"structure":true}})).unwrap());
    }
    LanguageSyntax::new(definitions, grammars).unwrap()
}
fn request(text: &str, language: &str, document: &str, revision: u64) -> AnalysisRequest {
    AnalysisRequest {
        version: protocol::VERSION,
        document: document.into(),
        request: revision,
        revision,
        language: language.into(),
        path: document.into(),
        source_digest: protocol::source_digest(text),
        source: protocol::SourceChange::Full { text: text.into() },
        tab_width: 4,
    }
}
#[test]
#[cfg(feature = "all")]
fn every_existing_language_runs_through_the_public_component_protocol() {
    let mut engine = engine();
    for &(path, source, fold) in LANGUAGE_CASES {
        let language = format!("{:?}", language_from_path(path)).to_ascii_lowercase();
        for source in [source.to_string(), source.replace('\n', "\r\n")] {
            let input = LanguageInput::Analyze(request(&source, &language, path, 1));
            engine
                .handle(LanguageInput::Close {
                    version: protocol::VERSION,
                    document: path.into(),
                })
                .unwrap();
            let output = engine.handle(input.clone()).unwrap();
            protocol::validate_output(&input, &output, Some(&source)).unwrap();
            let LanguageOutput::Analyzed(response) = output else {
                unreachable!()
            };
            assert!(
                matches!(response.status, AnalysisStatus::Ready { .. }),
                "{path}: {:?}",
                response.status
            );
            assert!(response.analysis.unwrap().folds.contains(&fold), "{path}");
        }
    }
}

#[test]
#[cfg(feature = "rust")]
fn incremental_edits_close_configuration_changes_and_eviction_require_the_correct_source() {
    let mut engine = engine();
    let source = "fn main() { let value = 1; }\r\n";
    let first = request(source, "rust", "file", 1);
    let mut edit = request(&format!("// 文😀\r\n{source}"), "rust", "file", 2);
    edit.source = protocol::SourceChange::Replace {
        base_revision: 1,
        start: 0,
        end: 0,
        text: "// 文😀\r\n".into(),
    };
    let missing = engine.handle(LanguageInput::Analyze(edit.clone())).unwrap();
    assert!(
        matches!(missing, LanguageOutput::Analyzed(response) if response.status == AnalysisStatus::NeedsSource)
    );
    engine.handle(LanguageInput::Analyze(first)).unwrap();
    let input = LanguageInput::Analyze(edit.clone());
    let output = engine.handle(input.clone()).unwrap();
    protocol::validate_output(&input, &output, Some(&format!("// 文😀\r\n{source}"))).unwrap();
    assert!(
        matches!(output, LanguageOutput::Analyzed(response) if response.status == AnalysisStatus::Ready { incremental:true })
    );
    engine
        .handle(LanguageInput::Close {
            version: protocol::VERSION,
            document: "file".into(),
        })
        .unwrap();
    let missing = engine.handle(LanguageInput::Analyze(edit.clone())).unwrap();
    assert!(
        matches!(missing, LanguageOutput::Analyzed(response) if response.status == AnalysisStatus::NeedsSource)
    );
    for index in 0..MAX_DOCUMENTS + 1 {
        engine
            .handle(LanguageInput::Analyze(request(
                source,
                "rust",
                &format!("file-{index}"),
                1,
            )))
            .unwrap();
    }
    assert_eq!(engine.documents.len(), MAX_DOCUMENTS);
    assert!(!engine.documents.contains_key("file-0"));
    assert!(engine.handle(LanguageInput::Analyze(edit)).is_ok());
}

#[test]
#[cfg(any(feature = "html", feature = "markdown"))]
fn delegated_bodies_request_installed_languages_without_bundled_child_grammars() {
    let engine = engine();
    let mut engine = LanguageSyntax::new_delegated(engine.definitions, engine.grammars).unwrap();
    let mut cases = Vec::new();
    #[cfg(feature = "html")]
    cases.push((
        "html",
        "<script>const text = \"文😀\";</script>\n<style>a { color: red; }</style>\n",
        vec!["javascript", "css"],
    ));
    #[cfg(feature = "markdown")]
    cases.push((
        "markdown",
        "# 文😀\n\n```custom-language\n〈hello〉\n```\n\n```rs\nfn main() {}\n```\n",
        vec!["markdown-inline", "custom-language", "rust"],
    ));
    for (language, source, expected) in cases {
        for source in [source.to_string(), source.replace('\n', "\r\n")] {
            let input = LanguageInput::Analyze(request(&source, language, language, 1));
            engine
                .handle(LanguageInput::Close {
                    version: protocol::VERSION,
                    document: language.into(),
                })
                .unwrap();
            let output = engine.handle(input.clone()).unwrap();
            protocol::validate_output(&input, &output, Some(&source)).unwrap();
            let LanguageOutput::Analyzed(response) = output else {
                unreachable!()
            };
            assert!(matches!(response.status, AnalysisStatus::Ready { .. }));
            let data = response.analysis.unwrap();
            let scopes = &data.structure.as_ref().unwrap().scopes;
            assert_eq!(
                scopes
                    .iter()
                    .map(|scope| scope.language.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            let rows = data.highlights.as_ref().unwrap();
            let mut row_start = 0;
            for (row, spans) in source.split('\n').zip(rows) {
                let mut start = 0;
                for span in spans {
                    if scopes.iter().any(|scope| {
                        row_start + start < scope.range.end
                            && row_start + span.end > scope.range.start
                    }) {
                        assert_eq!(
                            span.kind,
                            protocol::TokenKind::Plain,
                            "{language}: embedded color escaped delegation"
                        );
                    }
                    start = span.end;
                }
                row_start += row.len() + 1;
            }
        }
    }
}

#[test]
fn independently_selected_families_preserve_source_and_folds_over_the_public_protocol() {
    use openwebide_core::editor::syntax_contracts::CONFIG_CASES;
    let engine = engine();
    let mut engine = LanguageSyntax::new_delegated(engine.definitions, engine.grammars).unwrap();
    for (path, source, fold) in LANGUAGE_CASES
        .iter()
        .map(|(path, source, fold)| (*path, *source, Some(*fold)))
        .chain(
            CONFIG_CASES
                .iter()
                .map(|(path, source)| (*path, *source, None)),
        )
        .chain([
            ("text.txt", "文😀\nplain text\n", None),
            ("query.sql", "SELECT '文😀';\n", None),
        ])
    {
        let id = format!("{:?}", language_from_path(path)).to_ascii_lowercase();
        if !engine.grammars.contains_key(&id) {
            continue;
        }
        for source in [source.to_string(), source.replace('\n', "\r\n")] {
            let input = LanguageInput::Analyze(request(&source, &id, path, 1));
            engine
                .handle(LanguageInput::Close {
                    version: protocol::VERSION,
                    document: path.into(),
                })
                .unwrap();
            let output = engine.handle(input.clone()).unwrap();
            protocol::validate_output(&input, &output, Some(&source)).unwrap();
            let LanguageOutput::Analyzed(response) = output else {
                unreachable!()
            };
            assert!(
                matches!(response.status, AnalysisStatus::Ready { .. }),
                "{path}: {:?}",
                response.status
            );
            let data = response.analysis.unwrap();
            if id == "plain" {
                assert!(
                    data.highlights.is_none(),
                    "Plain editing does not require token rows"
                );
            } else {
                assert_eq!(
                    data.highlights.as_ref().expect(path).len(),
                    source.split('\n').count(),
                    "{path}"
                );
            }
            if let Some(fold) = fold {
                assert!(data.folds.contains(&fold), "{path}");
            }
        }
    }
}

#[test]
#[cfg(feature = "sql")]
fn sql_snapshots_keep_exact_rows_with_trailing_newlines_and_crlf() {
    for source in ["SELECT '文😀';", "SELECT '文😀';\n", "SELECT '文😀';\r\n"] {
        let mut syntax =
            SyntaxDocument::with_delegated_provider(SyntaxLanguage::Sql, None).unwrap();
        let (status, analysis) = syntax.prepare_shared(Arc::new(source.into()), 4, || true);
        assert!(matches!(status, SyntaxStatus::Ready { .. }));
        let analysis = analysis.unwrap();
        let data = analysis.language_data("sql", |language| {
            format!("{language:?}").to_ascii_lowercase()
        });
        assert!(
            data.is_some(),
            "SQL could not export its exact source: {analysis:?}"
        );
        protocol::validate_analysis(&data.unwrap(), source, "sql").unwrap();
    }
}

#[test]
#[cfg(not(feature = "rust"))]
fn delegated_components_reject_grammar_bindings_omitted_from_their_features() {
    let definition: LanguageDefinition = openwebide_plugin_sdk::serde_json::from_value(openwebide_plugin_sdk::serde_json::json!({"id":"rust", "name":"Rust", "features":{"syntax":true}})).unwrap();
    assert!(
        LanguageSyntax::new_delegated(
            vec![definition],
            BTreeMap::from([(
                "rust".into(),
                LanguageGrammar::standard(SyntaxLanguage::Rust)
            )])
        )
        .is_err()
    );
}
