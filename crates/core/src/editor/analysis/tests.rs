use super::*;

fn definitions() -> Vec<protocol::LanguageDefinition> {
    serde_json::from_value(serde_json::json!([
        {"id":"notes","name":"Notes","features":{"syntax":true}}
    ]))
    .unwrap()
}

fn data(source: &str, keyword_row: Option<usize>) -> protocol::SyntaxAnalysis {
    protocol::SyntaxAnalysis {
        highlights: Some(
            source
                .split('\n')
                .enumerate()
                .map(|(index, row)| {
                    if row.is_empty() {
                        return vec![];
                    }
                    vec![protocol::TokenSpan {
                        end: row.len(),
                        kind: if keyword_row == Some(index) {
                            protocol::TokenKind::Keyword
                        } else {
                            protocol::TokenKind::Plain
                        },
                    }]
                })
                .collect(),
        ),
        folds: vec![],
        structure: None,
    }
}

#[test]
fn host_rows_reuse_exact_styles_across_insertions_deletions_and_disjoint_changes() {
    for ending in ["\n", "\r\n"] {
        let source = format!("head{ending}文😀{ending}last{ending}");
        let previous = SyntaxAnalysis::from_language_data(
            Arc::new(source.clone()),
            "notes",
            data(&source, None),
            &definitions(),
        )
        .unwrap();
        let old = previous.highlights().unwrap();
        for (text, shared) in [
            (source.clone(), vec![(0, 0), (1, 1), (2, 2)]),
            (
                format!("head{ending}new{ending}文😀{ending}last{ending}"),
                vec![(0, 0), (2, 1), (3, 2)],
            ),
            (format!("head{ending}last{ending}"), vec![(0, 0), (1, 2)]),
            (format!("文😀{ending}last{ending}"), vec![(0, 1), (1, 2)]),
            (
                format!("changed{ending}文😀{ending}end{ending}"),
                vec![(1, 1)],
            ),
        ] {
            let snapshot = Arc::new(text.clone());
            let current = SyntaxAnalysis::from_language_data_reusing(
                snapshot.clone(),
                "notes",
                data(&text, None),
                &definitions(),
                Some(&previous),
            )
            .unwrap();
            assert!(Arc::ptr_eq(current.source_snapshot(), &snapshot));
            let rows = current.highlights().unwrap();
            for (new, before) in shared {
                assert!(
                    Arc::ptr_eq(&rows[new], &old[before]),
                    "{text:?}: row {new} from {before}"
                );
            }
            assert!(
                rows.iter()
                    .map(|row| row
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>())
                    .eq(text.split('\n').map(str::to_owned))
            );
            if text == source {
                assert!(Arc::ptr_eq(rows, old));
            }
        }
        let styled = SyntaxAnalysis::from_language_data_reusing(
            Arc::new(source.clone()),
            "notes",
            data(&source, Some(1)),
            &definitions(),
            Some(&previous),
        )
        .unwrap();
        let rows = styled.highlights().unwrap();
        assert!(Arc::ptr_eq(&rows[0], &old[0]));
        assert!(!Arc::ptr_eq(&rows[1], &old[1]));
        assert_eq!(rows[1][0].kind, protocol::TokenKind::Keyword);
        let mut malformed = data(&source, None);
        malformed.highlights.as_mut().unwrap()[1][0].end = 1;
        assert!(
            SyntaxAnalysis::from_language_data_reusing(
                Arc::new(source),
                "notes",
                malformed,
                &definitions(),
                Some(&previous)
            )
            .is_err()
        );
    }
}
