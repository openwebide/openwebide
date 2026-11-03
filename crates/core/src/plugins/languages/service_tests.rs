use super::*;

fn request(source: &str) -> AnalysisRequest {
    AnalysisRequest {
        version: VERSION,
        document: "account/project/client/file".into(),
        request: 1,
        revision: 1,
        language: "custom".into(),
        path: "file.custom".into(),
        tab_width: 4,
        source_digest: source_digest(source),
        source: SourceChange::Full {
            text: source.into(),
        },
    }
}

#[test]
fn source_admission_preserves_unicode_crlf_and_requires_current_delta_bases() {
    let source = "a文😀\r\n";
    let first = request(source);
    let document = LanguageDocument::apply(None, &first).unwrap().unwrap();
    let mut edit = first.clone();
    edit.request = 2;
    edit.revision = 2;
    edit.source_digest = source_digest("a字😀\r\n");
    edit.source = SourceChange::Replace {
        base_revision: 1,
        start: 1,
        end: 4,
        text: "字".into(),
    };
    assert!(LanguageDocument::apply(None, &edit).unwrap().is_none());
    assert_eq!(
        &*LanguageDocument::apply(Some(&document), &edit)
            .unwrap()
            .unwrap()
            .source,
        "a字😀\r\n"
    );
    let SourceChange::Replace { start, .. } = &mut edit.source else {
        unreachable!()
    };
    *start = 2;
    assert!(LanguageDocument::apply(Some(&document), &edit).is_err());
    edit.source = SourceChange::Full {
        text: source.into(),
    };
    assert!(LanguageDocument::apply(Some(&document), &edit).is_err());
    edit.source_digest = source_digest(source);
    edit.revision = 1;
    edit.request = 1;
    assert!(LanguageDocument::apply(Some(&document), &edit).is_err());
    edit.request = 2;
    edit.source = SourceChange::Full {
        text: "changed".into(),
    };
    edit.source_digest = source_digest("changed");
    assert!(LanguageDocument::apply(Some(&document), &edit).is_err());
}

fn analysis(source: &str) -> SyntaxAnalysis {
    SyntaxAnalysis {
        highlights: Some(
            source
                .split('\n')
                .map(|row| {
                    if row.is_empty() {
                        vec![]
                    } else {
                        vec![TokenSpan {
                            end: row.len(),
                            kind: TokenKind::Plain,
                        }]
                    }
                })
                .collect(),
        ),
        folds: vec![],
        structure: None,
    }
}

#[test]
fn admission_rejects_stale_result_identity_and_unmatched_status_data() {
    let source = "文😀\r\n";
    let request = request(source);
    let input = LanguageInput::Analyze(request.clone());
    let mut output = analysis_response(&request, AnalysisStatus::Ready { incremental: false });
    assert!(validate_output(&input, &output, Some(source)).is_err());
    let LanguageOutput::Analyzed(response) = &mut output else {
        unreachable!()
    };
    response.analysis = Some(analysis(source));
    validate_output(&input, &output, Some(source)).unwrap();
    assert!(validate_output(&input, &output, Some("other")).is_err());
    let LanguageOutput::Analyzed(response) = &mut output else {
        unreachable!()
    };
    response.revision += 1;
    assert!(validate_output(&input, &output, Some(source)).is_err());
    let LanguageOutput::Analyzed(response) = &mut output else {
        unreachable!()
    };
    response.revision = request.revision;
    response.status = AnalysisStatus::NeedsSource;
    assert!(validate_output(&input, &output, Some(source)).is_err());
    let LanguageOutput::Analyzed(response) = &mut output else {
        unreachable!()
    };
    response.analysis = None;
    validate_output(&input, &output, None).unwrap();
}

#[test]
fn token_validation_requires_exact_rows_utf8_boundaries_and_full_coverage() {
    let source = "文😀\r\n";
    let valid = analysis(source);
    validate_analysis(&valid, source, "custom").unwrap();
    for end in [1, 3, 6, 9] {
        let mut invalid = valid.clone();
        invalid.highlights.as_mut().unwrap()[0][0].end = end;
        assert!(validate_analysis(&invalid, source, "custom").is_err());
    }
    let mut invalid = valid;
    invalid.highlights.as_mut().unwrap().pop();
    assert!(validate_analysis(&invalid, source, "custom").is_err());
}

#[test]
fn structure_validation_requires_source_owned_ranges_and_reciprocal_brackets() {
    let source = "{文}\r\n";
    let mut value = analysis(source);
    value.structure = Some(Structure {
        language: "custom".into(),
        scopes: vec![],
        selections: vec![TextRange { start: 0, end: 5 }],
        opaque_starts: vec![],
        protected: vec![],
        brackets: vec![
            Bracket {
                offset: 0,
                character: '{',
                partner: Some(4),
            },
            Bracket {
                offset: 4,
                character: '}',
                partner: Some(0),
            },
        ],
    });
    validate_analysis(&value, source, "custom").unwrap();
    value.structure.as_mut().unwrap().brackets[1].partner = None;
    assert!(validate_analysis(&value, source, "custom").is_err());
    value.structure.as_mut().unwrap().brackets[1].partner = Some(0);
    value.structure.as_mut().unwrap().selections[0].end = 3;
    assert!(validate_analysis(&value, source, "custom").is_err());
    value.structure.as_mut().unwrap().selections.clear();
    value.structure.as_mut().unwrap().language = "another".into();
    assert!(validate_analysis(&value, source, "custom").is_err());
}

#[test]
fn input_limits_admit_worst_case_json_escaping_without_narrowing_source_limit() {
    let source = "\0".repeat(MAX_SOURCE_BYTES);
    let input = LanguageInput::Analyze(request(&source));
    validate_input(&input).unwrap();
    assert!(serde_json::to_string(&input).unwrap().len() <= MAX_REQUEST_BYTES);
    assert!(validate_input(&LanguageInput::Analyze(request(&(source + "x")))).is_err());
}
