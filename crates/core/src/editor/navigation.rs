//! Source-coordinate navigation independent of the edit-view projection.
use super::{
    Structure,
    lines::{lines, row_at},
};
use crate::highlight::Language;

/// One-based logical line and Unicode character column. Tabs count as one source
/// character; visual indentation remains a separate presentation concern.
pub fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let offset = text.floor_char_boundary(offset.min(text.len()));
    let rows = lines(text);
    let row = row_at(&rows, offset);
    (
        row + 1,
        text[rows[row].start..offset.min(rows[row].body_end)]
            .chars()
            .count()
            + 1,
    )
}

/// Clamp a one-based source coordinate to the document, without entering CRLF
/// separators or splitting Unicode characters.
pub fn offset_at_line_column(text: &str, line: usize, column: usize) -> usize {
    let rows = lines(text);
    let row = &rows[line.saturating_sub(1).min(rows.len() - 1)];
    text[row.start..row.body_end]
        .char_indices()
        .nth(column.saturating_sub(1))
        .map_or(row.body_end, |(offset, _)| row.start + offset)
}

/// Parse the go-to input and resolve it to a safe source offset.
pub fn navigation_target(text: &str, query: &str) -> Option<usize> {
    let (line, column) = navigation_coordinates(query)?;
    Some(offset_at_line_column(text, line, column))
}

pub(super) fn navigation_coordinates(query: &str) -> Option<(usize, usize)> {
    let (line, column) = query
        .trim()
        .split_once(':')
        .map_or((query.trim(), "1"), |(line, column)| {
            (line.trim(), column.trim())
        });
    let line = line.parse::<usize>().ok().filter(|line| *line > 0)?;
    let column = column.parse::<usize>().ok().filter(|column| *column > 0)?;
    Some((line, column))
}

pub fn has_adjacent_bracket(text: &str, offset: usize) -> bool {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return false;
    }
    let adjacent = text[offset..]
        .chars()
        .next()
        .into_iter()
        .chain(text[..offset].chars().next_back());
    adjacent
        .into_iter()
        .any(|ch| matches!(ch, '(' | ')' | '[' | ']' | '{' | '}'))
}

/// Adjacent code bracket and its mate; literal/comment brackets are opaque.
pub fn matching_bracket(text: &str, language: Language, offset: usize) -> Option<(usize, usize)> {
    if !has_adjacent_bracket(text, offset) {
        return None;
    }
    let structure = Structure::new(text, language);
    matching_bracket_in(text, &structure, offset)
}

pub fn matching_bracket_with_context(
    text: &str,
    context: &Structure,
    offset: usize,
) -> Option<(usize, usize)> {
    if !has_adjacent_bracket(text, offset) || !context.matches_source(text) {
        return None;
    }
    matching_bracket_in(text, context, offset)
}

fn matching_bracket_in(text: &str, structure: &Structure, offset: usize) -> Option<(usize, usize)> {
    let previous = text[..offset]
        .char_indices()
        .next_back()
        .map(|(offset, _)| offset);
    [Some(offset), previous]
        .into_iter()
        .flatten()
        .find_map(|at| {
            let index = structure
                .brackets
                .binary_search_by_key(&at, |bracket| bracket.0)
                .ok()?;
            structure.brackets[index].2.map(|mate| (at, mate))
        })
}

/// Visual widths for indentation guides, rounded to complete indentation steps.
/// Blank rows continue the common indentation of the surrounding content.
pub fn indent_guide_columns(text: &str, indentation: super::Indentation) -> Vec<usize> {
    if text.len() > super::MAX_STRUCTURE_BYTES {
        return text.split('\n').map(|_| 0).collect();
    }
    guide_columns(text.split('\n'), indentation)
}

/// Indexed documents and standalone callers share blank-row continuation policy.
pub(super) fn guide_columns<'a>(
    rows: impl IntoIterator<Item = &'a str>,
    indentation: super::Indentation,
) -> Vec<usize> {
    let mut columns = Vec::new();
    let mut previous = 0;
    let mut blanks = None;
    for row in rows {
        if row.chars().all(char::is_whitespace) {
            blanks.get_or_insert(columns.len());
            columns.push(0);
            continue;
        }
        let end = row
            .bytes()
            .take_while(|ch| matches!(ch, b' ' | b'\t'))
            .count();
        let width = indentation.visual_width(&row[..end]);
        let width = width / indentation.width() * indentation.width();
        if let Some(start) = blanks.take() {
            columns[start..].fill(previous.min(width));
        }
        columns.push(width);
        previous = width;
    }
    // A trailing blank run has no following indentation, so its guides stay zero.
    columns
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_coordinates_round_trip_unicode_tabs_empty_rows_and_crlf() {
        let text = "文😀\r\n\tvalue\n\n";
        for (line, column) in [
            (1, 1),
            (1, 2),
            (1, 3),
            (2, 1),
            (2, 2),
            (2, 7),
            (3, 1),
            (4, 1),
        ] {
            let offset = offset_at_line_column(text, line, column);
            assert_eq!(line_column(text, offset), (line, column));
            assert!(text.is_char_boundary(offset));
        }
        assert_eq!(offset_at_line_column(text, 999, 999), text.len());
        assert_eq!(offset_at_line_column(text, 0, 0), 0);
        assert_eq!(line_column("", 999), (1, 1));
    }
    #[test]
    fn guides_use_tab_stops_and_continue_blank_rows_without_creating_blocks() {
        let indentation = super::super::Indentation {
            width: 4,
            tab_width: 8,
            ..Default::default()
        };
        assert_eq!(
            indent_guide_columns("root\n \tchild\n\n    sibling\nnext\n", indentation),
            vec![0, 8, 4, 4, 0, 0]
        );
    }
    #[test]
    fn go_to_validation_is_shared_and_clamps_valid_coordinates() {
        assert_eq!(navigation_target("first\n文😀", " 2:2 "), Some(9));
        assert_eq!(navigation_target("first\n文😀", "2"), Some(6));
        assert_eq!(navigation_target("x", "999:999"), Some(1));
        for query in ["", "0", "1:0", "-1", "1:x", "1:2:3"] {
            assert!(navigation_target("x", query).is_none());
        }
    }
    #[test]
    fn indexed_navigation_matches_source_coordinates_after_edits_and_history() {
        use super::super::{Document, Edit, Selection};
        let source = format!(
            "{}\r\n{}\n",
            "文😀e\u{301}\t".repeat(4000),
            "tail".repeat(300)
        );
        let mut document = Document::new(&source);
        for stage in 0..4 {
            for line in [0, 1, 2, 3, 999, usize::MAX] {
                for column in [0, 1, 2, 511, 512, 513, 9000, usize::MAX] {
                    let query = format!("{line}:{column}");
                    assert_eq!(
                        document.navigation_target(&query),
                        navigation_target(document.text(), &query)
                    );
                }
            }
            for query in ["", " 2:2 ", "1", "0", "1:0", "-1", "1:x", "1:2:3"] {
                assert_eq!(
                    document.navigation_target(query),
                    navigation_target(document.text(), query)
                );
            }
            match stage {
                0 => {
                    document
                        .apply(
                            vec![Edit {
                                range: 0..0,
                                text: "new\r\n".into(),
                            }],
                            vec![Selection::caret(0)],
                            None,
                        )
                        .unwrap();
                }
                1 => {
                    document.undo();
                }
                2 => {
                    document.redo();
                }
                _ => {}
            }
        }
    }

    #[test]
    fn bracket_navigation_ignores_literals_and_supports_either_side_of_a_pair() {
        let text = "fn f() { let s = \"} 文😀\"; /* { */ work(); }";
        let open = text.find('{').unwrap();
        let close = text.rfind('}').unwrap();
        assert_eq!(
            matching_bracket(text, Language::Rust, open),
            Some((open, close))
        );
        assert_eq!(
            matching_bracket(text, Language::Rust, open + 1),
            Some((open, close))
        );
        assert_eq!(
            matching_bracket(text, Language::Rust, close),
            Some((close, open))
        );
        assert!(matching_bracket(text, Language::Rust, text.find("} 文").unwrap()).is_none());
        assert!(matching_bracket(text, Language::Plain, open).is_none());
        assert!(matching_bracket("{", Language::Rust, 0).is_none());
    }
}
