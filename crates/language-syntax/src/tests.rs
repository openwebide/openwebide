use super::*;
use openwebide_core::{editor::syntax_contracts::LANGUAGE_CASES, highlight::language_from_path};

fn engine() -> LanguageSyntax {
    let mut grammars = BTreeMap::new();
    let mut definitions = Vec::new();
    for language in SYNTAX_PROVIDERS
        .iter()
        .map(|provider| provider.language)
        .chain([SyntaxLanguage::Plain])
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
