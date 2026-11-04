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
fn contributed_unicode_bracket_partners_support_navigation_at_scalar_boundaries() {
    let source = "〈文😀〉\r\n";
    let closing = source.find('〉').unwrap();
    let mut supplied = data();
    supplied.brackets = vec![
        protocol::Bracket {
            offset: 0,
            character: '〈',
            partner: Some(closing),
        },
        protocol::Bracket {
            offset: closing,
            character: '〉',
            partner: Some(0),
        },
    ];
    let syntax = context(source, supplied);
    for (offset, expected) in [
        (0, (0, closing)),
        (3, (0, closing)),
        (closing, (closing, 0)),
        (closing + 3, (closing, 0)),
    ] {
        assert_eq!(
            crate::editor::matching_bracket_with_context(source, &syntax, offset),
            Some(expected)
        );
    }
    for offset in [1, 4, source.len() + 1] {
        assert_eq!(
            crate::editor::matching_bracket_with_context(source, &syntax, offset),
            None
        );
    }
    assert_eq!(
        crate::editor::matching_bracket_with_context("different", &syntax, 0),
        None
    );
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
fn declared_surroundings_preserve_explicit_selections_before_analysis_and_in_protected_text() {
    for prepared in [false, true] {
        let source = "文😀";
        let syntax = if prepared {
            let mut supplied = data();
            supplied.protected.push(protocol::ProtectedRegion {
                range: protocol::TextRange {
                    start: 0,
                    end: source.len(),
                },
                complete: true,
                kind: protocol::RegionKind::String,
            });
            context(source, supplied)
        } else {
            Structure::from_language_configuration(Arc::new(source.into()), "notes", &definitions())
                .unwrap()
        };
        let mut document = Document::new(source);
        document
            .set_selections(vec![Selection {
                anchor: source.len(),
                head: 0,
            }])
            .unwrap();
        document.type_character_with_context('〈', &syntax).unwrap();
        assert_eq!(document.text(), "〈文😀〉");
        assert_eq!(
            document.selections(),
            [Selection {
                anchor: 10,
                head: 3
            }]
        );
        assert!(document.undo());
        assert_eq!(document.text(), source);
    }
}

#[test]
fn editor_created_pairs_survive_pending_analysis_and_shift_without_guessing_syntax() {
    use crate::editor::Edit;
    for opening in ['〈', '§'] {
        let mut document = Document::new("文😀 \r\n");
        document.set_selections(vec![Selection::caret(8)]).unwrap();
        let syntax = context(document.text(), data());
        document
            .type_character_with_context(opening, &syntax)
            .unwrap();
        let paired = document.text().to_string();
        let closer = document.text()[document.selections()[0].head..]
            .chars()
            .next()
            .unwrap();
        let unparsed = Structure::plain(document.shared_text());
        assert!(
            document
                .type_character_with_context(closer, &unparsed)
                .is_ok()
        );
        assert_eq!(
            document.text(),
            paired,
            "remember the inserted closer without a host reply"
        );
        document
            .set_selections(vec![Selection::caret(8 + opening.len_utf8())])
            .unwrap();
        document
            .apply(
                vec![Edit::replace(0..0, "😀")],
                vec![Selection::caret(12 + opening.len_utf8())],
                None,
            )
            .unwrap();
        assert!(document.delete_empty_pairs(Language::Plain).unwrap());
        assert_eq!(document.text(), "😀文😀 \r\n");
        document.undo();
        assert_eq!(document.text(), format!("😀{paired}"));
        document.redo();
        assert_eq!(document.text(), "😀文😀 \r\n");

        let mut replaced = Document::new("");
        replaced
            .type_character_with_context(opening, &context("", data()))
            .unwrap();
        // Replacing a delimiter with identical-looking unrelated text revokes
        // that editor-created pair, even while the other delimiter survives.
        replaced
            .apply(
                vec![Edit::replace(0..opening.len_utf8(), format!("x{opening}"))],
                vec![Selection::caret(1 + opening.len_utf8())],
                None,
            )
            .unwrap();
        assert!(!replaced.delete_empty_pairs(Language::Plain).unwrap());
    }
    let mut unrelated = Document::new("〈〉");
    unrelated.set_selections(vec![Selection::caret(3)]).unwrap();
    assert!(!unrelated.delete_empty_pairs(Language::Plain).unwrap());
}

#[test]
fn declared_closing_indent_retains_validated_enter_and_rejects_intervening_edits() {
    use crate::editor::{Edit, IndentStyle};
    for ending in ["\n", "\r\n"] {
        for (base, indentation) in [
            ("  ", Indentation::default()),
            (
                "\t",
                Indentation {
                    style: IndentStyle::Tabs,
                    ..Indentation::default()
                },
            ),
        ] {
            for intervening_edit in [false, true] {
                let source = format!("{base}block〈〉{ending}tail");
                let opening = source.find('〈').unwrap();
                let closing = source.find('〉').unwrap();
                let mut supplied = data();
                supplied.brackets = vec![
                    protocol::Bracket {
                        offset: opening,
                        character: '〈',
                        partner: Some(closing),
                    },
                    protocol::Bracket {
                        offset: closing,
                        character: '〉',
                        partner: Some(opening),
                    },
                ];
                let mut definitions = definitions();
                definitions[0].features.structure = true;
                let syntax =
                    Structure::from_language_data(Arc::new(source.clone()), supplied, &definitions)
                        .unwrap();
                let mut document = Document::new(&source);
                document
                    .set_selections(vec![Selection::caret(closing)])
                    .unwrap();
                document
                    .newline_with_context(indentation, None, &syntax)
                    .unwrap();
                let indented = document.text().to_owned();
                let caret = document.selections()[0];
                if intervening_edit {
                    let end = document.text().len();
                    document
                        .apply(vec![Edit::replace(end..end, "!")], vec![caret], None)
                        .unwrap();
                }
                let before = document.text().to_owned();
                let unparsed = Structure::from_language_configuration(
                    document.shared_text(),
                    "notes",
                    &definitions,
                )
                .unwrap();
                assert!(!unparsed.available());
                document
                    .type_character_with_context('〉', &unparsed)
                    .unwrap();
                let prefix = if intervening_edit {
                    format!("{base}{}", indentation.unit())
                } else {
                    base.to_owned()
                };
                assert_eq!(
                    document.text(),
                    format!(
                        "{base}block〈{ending}{prefix}〉{ending}{base}〉{ending}tail{}",
                        if intervening_edit { "!" } else { "" }
                    )
                );
                assert!(document.undo());
                assert_eq!(document.text(), before);
                if !intervening_edit {
                    assert_eq!(before, indented);
                    assert!(document.undo());
                    assert_eq!(document.text(), source);
                }
            }
        }
    }
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
fn embedded_indentation_is_available_without_rules_for_the_surrounding_prose() {
    let mut supplied = data();
    supplied.language = "embedded".into();
    let root = context(" x ", supplied.clone());
    assert!(!root.supports_indentation());
    supplied.scopes.push(protocol::LanguageScope {
        range: protocol::TextRange { start: 1, end: 2 },
        language: "notes".into(),
    });
    let mixed = context(" x ", supplied);
    assert!(mixed.configuration_at(0).unwrap().brackets.is_empty());
    assert!(mixed.supports_indentation());
    assert!(!Structure::plain(Arc::new(" x ".into())).supports_indentation());
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
