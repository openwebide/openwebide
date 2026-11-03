use super::*;
use crate::editor::{Document, Indentation, Selection};
use serde_json::json;

fn definitions() -> Vec<protocol::LanguageDefinition> {
    serde_json::from_value(json!([
        {"id":"notes","name":"Notes","extensions":["notes"],"configuration":{
            "lineComment":"##","blockComment":{"open":"<#","close":"#>","nested":true},
            "brackets":[{"open":"〈","close":"〉"},{"open":"{","close":"}"}],
            "autoClosingPairs":[
                {"open":"〈","close":"〉","notIn":["String","Template","LineComment","BlockComment"]},
                {"open":"{","close":"}","notIn":["String","Template","LineComment","BlockComment"]},
                {"open":"{","close":"}","notIn":["String","LineComment","BlockComment"],"before":"\\$\\z","after":".*"},
                {"open":"§","close":"§","notIn":["String","Template","LineComment","BlockComment"]}
            ],
            "surroundingPairs":[{"open":"〈","close":"〉"},{"open":"{","close":"}"},{"open":"§","close":"§"}],
            "indentation":{"increase":"[〈{]\\s*$","decrease":"^\\s*[〉}]"}
        }},
        {"id":"embedded","name":"Embedded","configuration":{"lineComment":";;"}}
    ])).unwrap()
}
fn data() -> protocol::Structure {
    protocol::Structure {
        language: "notes".into(),
        scopes: vec![],
        selections: vec![],
        opaque_starts: vec![],
        protected: vec![],
        brackets: vec![],
    }
}
fn context(source: &str, data: protocol::Structure) -> Structure {
    Structure::from_language_data(Arc::new(source.into()), data, &definitions()).unwrap()
}

#[test]
fn uniform_line_comments_reject_conflicting_embedded_markers_and_block_fallbacks() {
    let source = Arc::new("x".to_string());
    let mut definitions = definitions();
    definitions[0].configuration.uniform_line_comments = true;
    let mut supplied = data();
    supplied.scopes.push(protocol::LanguageScope {
        range: protocol::TextRange { start: 0, end: 1 },
        language: "embedded".into(),
    });
    assert!(Structure::from_language_data(source.clone(), supplied.clone(), &definitions).is_err());
    definitions[1].configuration.line_comment = definitions[0].configuration.line_comment.clone();
    assert!(Structure::from_language_data(source.clone(), supplied.clone(), &definitions).is_ok());
    definitions[0].configuration.line_comment = None;
    definitions[1].configuration.line_comment = None;
    assert!(Structure::from_language_data(source.clone(), supplied.clone(), &definitions).is_err());
    definitions[1].configuration.block_comment = definitions[0].configuration.block_comment.clone();
    assert!(Structure::from_language_data(source, supplied, &definitions).is_ok());
}

#[test]
fn custom_ids_supply_comments_in_root_and_embedded_scopes_with_unicode_and_undo() {
    let source = "文😀\r\ntext\r\n";
    let mut data = data();
    data.scopes.push(protocol::LanguageScope {
        range: protocol::TextRange { start: 9, end: 13 },
        language: "embedded".into(),
    });
    let syntax = context(source, data);
    let mut document = Document::new(source);
    document
        .set_selections(vec![Selection::caret(0), Selection::caret(9)])
        .unwrap();
    assert!(document.toggle_line_comments_with_context(&syntax).unwrap());
    assert_eq!(document.text(), "## 文😀\r\n;; text\r\n");
    document.undo();
    assert_eq!(document.text(), source);
}

#[test]
fn declared_nested_markers_are_balanced_without_language_identity_checks() {
    for (source, expected) in [("a <# inside #> b", true), ("a #> b", false)] {
        let syntax = context(source, data());
        let mut document = Document::new(source);
        document
            .set_selections(vec![Selection {
                anchor: source.len(),
                head: 0,
            }])
            .unwrap();
        assert_eq!(
            document
                .toggle_block_comments_with_context(&syntax)
                .unwrap(),
            expected
        );
        if expected {
            assert_eq!(document.text(), format!("<# {source} #>"));
            let syntax = context(document.text(), data());
            assert!(
                document
                    .toggle_block_comments_with_context(&syntax)
                    .unwrap()
            );
            assert_eq!(document.text(), source);
        } else {
            assert_eq!(document.text(), source);
        }
    }
}

#[test]
fn unicode_automatic_pairs_surroundings_and_backspace_use_byte_boundaries() {
    let mut document = Document::new("");
    let syntax = context("", data());
    document.type_character_with_context('〈', &syntax).unwrap();
    assert_eq!(document.text(), "〈〉");
    assert_eq!(document.selections()[0], Selection::caret(3));
    let mut paired = data();
    paired.brackets = vec![
        protocol::Bracket {
            offset: 0,
            character: '〈',
            partner: Some(3),
        },
        protocol::Bracket {
            offset: 3,
            character: '〉',
            partner: Some(0),
        },
    ];
    let syntax = context(document.text(), paired);
    assert!(document.delete_pairs_with_context(&syntax).unwrap());
    assert_eq!(document.text(), "");
    document.undo();
    assert_eq!(document.text(), "〈〉");
    let mut document = Document::new("文😀");
    document
        .set_selections(vec![Selection { anchor: 7, head: 0 }])
        .unwrap();
    document
        .type_character_with_context('〈', &context("文😀", data()))
        .unwrap();
    assert_eq!(document.text(), "〈文😀〉");
    assert_eq!(
        document.selections()[0],
        Selection {
            anchor: 10,
            head: 3
        }
    );
}

#[test]
fn region_exclusions_and_template_conditions_control_pairing_without_client_lexing() {
    let source = "§text§";
    let mut protected = data();
    protected.protected.push(protocol::ProtectedRegion {
        range: protocol::TextRange {
            start: 0,
            end: source.len(),
        },
        complete: true,
        kind: protocol::RegionKind::String,
    });
    let syntax = context(source, protected);
    let mut document = Document::new(source);
    document.set_selections(vec![Selection::caret(2)]).unwrap();
    document.type_character_with_context('{', &syntax).unwrap();
    assert_eq!(document.text(), "§{text§");
    for (source, expected) in [("§$§", "§${}§"), ("§x§", "§x{§")] {
        let mut protected = data();
        protected.protected.push(protocol::ProtectedRegion {
            range: protocol::TextRange {
                start: 0,
                end: source.len(),
            },
            complete: true,
            kind: protocol::RegionKind::Template,
        });
        let syntax = context(source, protected);
        let mut document = Document::new(source);
        document.set_selections(vec![Selection::caret(3)]).unwrap();
        document.type_character_with_context('{', &syntax).unwrap();
        assert_eq!(document.text(), expected);
    }
}

#[test]
fn declared_enter_and_reindent_preserve_crlf_and_ignore_literal_brackets() {
    let source = "{}\r\n";
    let mut paired = data();
    paired.brackets = vec![
        protocol::Bracket {
            offset: 0,
            character: '{',
            partner: Some(1),
        },
        protocol::Bracket {
            offset: 1,
            character: '}',
            partner: Some(0),
        },
    ];
    let syntax = context(source, paired);
    let mut document = Document::new(source);
    document.set_selections(vec![Selection::caret(1)]).unwrap();
    document
        .newline_with_context(Indentation::default(), None, &syntax)
        .unwrap();
    assert_eq!(document.text(), "{\r\n    \r\n}\r\n");
    assert_eq!(document.selections()[0], Selection::caret(7));
    document.undo();
    assert_eq!(document.text(), source);
    let source = "{\r\nx\r\n}";
    let mut paired = data();
    paired.brackets = vec![
        protocol::Bracket {
            offset: 0,
            character: '{',
            partner: Some(6),
        },
        protocol::Bracket {
            offset: 6,
            character: '}',
            partner: Some(0),
        },
    ];
    let syntax = context(source, paired);
    let mut document = Document::new(source);
    document
        .set_selections(vec![Selection {
            anchor: 0,
            head: source.len(),
        }])
        .unwrap();
    document
        .reindent_with_context(Indentation::default(), &syntax)
        .unwrap();
    assert_eq!(document.text(), "{\r\n    x\r\n}");
}

#[test]
fn absent_analysis_and_stale_source_cannot_apply_context_sensitive_pair_rules() {
    let metadata =
        Structure::from_language_configuration(Arc::new(String::new()), "notes", &definitions())
            .unwrap();
    let mut document = Document::new("");
    document
        .type_character_with_context('{', &metadata)
        .unwrap();
    assert_eq!(document.text(), "{");
    assert!(
        document
            .type_character_with_context('{', &metadata)
            .is_err()
    );
    let mut document = Document::new("text");
    assert!(
        !document
            .toggle_line_comments_with_context(&Structure::plain(Arc::new("text".into())))
            .unwrap()
    );
}

#[test]
fn declarative_only_languages_apply_unconditional_pairs_and_enter_without_a_parser() {
    let definitions: Vec<protocol::LanguageDefinition> = serde_json::from_value(json!([{
        "id":"declarative","name":"Declarative","configuration":{
            "autoClosingPairs":[{"open":"〈","close":"〉"}],
            "indentation":{"increase":"〈\\s*$"}
        }
    }]))
    .unwrap();
    let syntax = Structure::from_language_configuration(
        Arc::new(String::new()),
        "declarative",
        &definitions,
    )
    .unwrap();
    let mut document = Document::new("");
    document.type_character_with_context('〈', &syntax).unwrap();
    assert_eq!(document.text(), "〈〉");
    let syntax =
        Structure::from_language_configuration(Arc::new("〈".into()), "declarative", &definitions)
            .unwrap();
    let mut document = Document::new("〈");
    document.set_selections(vec![Selection::caret(3)]).unwrap();
    document
        .newline_with_context(Indentation::default(), None, &syntax)
        .unwrap();
    assert_eq!(document.text(), "〈\n    ");
}

#[test]
fn empty_embedded_bodies_keep_their_declared_insertion_configuration() {
    let mut data = data();
    data.scopes.push(protocol::LanguageScope {
        range: protocol::TextRange { start: 1, end: 1 },
        language: "embedded".into(),
    });
    let syntax = context("ab", data);
    assert_eq!(
        syntax.configuration_at(1).unwrap().line_comment.as_deref(),
        Some(";;")
    );
}

#[test]
fn declared_comments_remain_available_above_the_syntax_source_limit() {
    let source = format!("{}\n", "x".repeat(64)).repeat(33_000);
    assert!(source.len() > protocol::MAX_SOURCE_BYTES);
    let syntax =
        Structure::from_language_configuration(Arc::new(source.clone()), "notes", &definitions())
            .unwrap();
    let mut document = Document::new(&source);
    assert!(document.toggle_line_comments_with_context(&syntax).unwrap());
    assert_eq!(document.text(), format!("## {source}"));
    document.undo();
    assert_eq!(document.text(), source);
}
