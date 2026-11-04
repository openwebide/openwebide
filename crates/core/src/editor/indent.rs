//! Indentation commands use the document transaction engine, preserving line endings.
use std::collections::BTreeSet;

use super::{Document, Edit, EditError, Selection};
mod configured;

/// Enter can retain its own indentation decision until the next source edit.
/// This records a validated editing transaction, never inferred syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NewlineIndentation {
    pub(super) revision: u64,
    pub(super) position: usize,
    pub(super) closing: char,
    pub(super) base: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IndentStyle {
    #[default]
    Spaces,
    Tabs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Indentation {
    pub style: IndentStyle,
    pub width: usize,
    pub tab_width: usize,
}

impl Default for Indentation {
    fn default() -> Self {
        Self {
            style: IndentStyle::Spaces,
            width: 4,
            tab_width: 4,
        }
    }
}

impl Indentation {
    pub fn width(self) -> usize {
        self.width.clamp(1, 16)
    }
    pub fn unit(self) -> String {
        self.columns(self.width())
    }
    pub fn tab_width(self) -> usize {
        self.tab_width.clamp(1, 16)
    }
    pub fn columns(self, width: usize) -> String {
        self.columns_from(0, width)
    }
    pub(super) fn columns_from_len(self, start: usize, width: usize) -> usize {
        if self.style == IndentStyle::Spaces || width == 0 {
            return width;
        }
        let first = self.tab_width() - start % self.tab_width();
        if width < first {
            width
        } else {
            let rest = width - first;
            1 + rest / self.tab_width() + rest % self.tab_width()
        }
    }
    pub(super) fn columns_from(self, start: usize, width: usize) -> String {
        if self.style == IndentStyle::Spaces {
            return " ".repeat(width);
        }
        let mut result = String::new();
        let mut column = start;
        let end = start + width;
        while column < end {
            let stop = column + self.tab_width() - column % self.tab_width();
            if stop <= end {
                result.push('\t');
                column = stop;
            } else {
                result.push_str(&" ".repeat(end - column));
                break;
            }
        }
        result
    }
    pub(super) fn visual_width(self, text: &str) -> usize {
        text.chars().fold(0, |column, ch| {
            if ch == '\t' {
                column + self.tab_width() - column % self.tab_width()
            } else {
                column + 1
            }
        })
    }
}

impl Document {
    /// Indent/outdent each selected logical line once, even when selections overlap.
    pub fn indent_lines(
        &mut self,
        indentation: Indentation,
        outdent: bool,
    ) -> Result<bool, EditError> {
        let mut starts = BTreeSet::new();
        for selection in &self.selections {
            let range = selection.range();
            let start = line_start(&self.text, range.start);
            let end = if range.end > range.start && self.text[..range.end].ends_with('\n') {
                range.end - 1
            } else {
                range.end
            };
            starts.insert(start);
            for (offset, ch) in self.text[start..end].char_indices() {
                if ch == '\n' {
                    starts.insert(start + offset + 1);
                }
            }
        }
        let edits = starts
            .into_iter()
            .filter_map(|start| {
                let line = &self.text[start..];
                let prefix: String = line
                    .chars()
                    .take_while(|ch| matches!(ch, ' ' | '\t'))
                    .collect();
                let columns = indentation.visual_width(&prefix);
                if outdent && prefix.is_empty() {
                    return None;
                }
                let target = if outdent {
                    columns.saturating_sub(indentation.width())
                } else {
                    columns + indentation.width()
                };
                Some(Edit::replace(
                    start..start + prefix.len(),
                    indentation.columns(target),
                ))
            })
            .collect();
        self.apply_indent_edits(edits, indentation, indentation, Some(outdent))
    }

    /// Tab at carets advances to the next tab stop; a selection indents whole lines.
    pub fn tab(&mut self, indentation: Indentation) -> Result<bool, EditError> {
        if self
            .selections
            .iter()
            .any(|selection| selection.anchor != selection.head)
        {
            return self.indent_lines(indentation, false);
        }
        let edits = self
            .selections
            .iter()
            .map(|selection| {
                let position = selection.head;
                let column = indentation
                    .visual_width(&self.text[line_start(&self.text, position)..position]);
                let width = indentation.width();
                let text = indentation.columns_from(column, width - column % width);
                Edit::replace(position..position, text)
            })
            .collect();
        self.apply_mapped(edits)
    }

    /// Enter retains the current line's indentation and the document's line ending.
    pub fn newline(&mut self) -> Result<bool, EditError> {
        self.newline_with_ending(None)
    }

    pub fn newline_with_ending(
        &mut self,
        ending: Option<super::LineEnding>,
    ) -> Result<bool, EditError> {
        self.newline_with_structure(
            Indentation::default(),
            ending,
            crate::highlight::Language::Plain,
        )
    }

    /// Enter increases indentation after a code opener, and splits an empty pair
    /// into an indented line plus its closing line. Comments/strings remain opaque.
    pub fn newline_with_structure(
        &mut self,
        indentation: Indentation,
        ending: Option<super::LineEnding>,
        language: crate::highlight::Language,
    ) -> Result<bool, EditError> {
        let syntax = super::Structure::new(&self.text, language);
        self.newline_in(indentation, ending, &syntax)
    }

    pub fn newline_with_context(
        &mut self,
        indentation: Indentation,
        ending: Option<super::LineEnding>,
        syntax: &super::Structure,
    ) -> Result<bool, EditError> {
        if !syntax.matches_source(&self.text) {
            return Err(EditError::StaleContext);
        }
        self.newline_in(indentation, ending, syntax)
    }

    fn newline_in(
        &mut self,
        indentation: Indentation,
        ending: Option<super::LineEnding>,
        syntax: &super::Structure,
    ) -> Result<bool, EditError> {
        if syntax.configuration_at(0).is_some() {
            return self.newline_declared(indentation, ending, syntax);
        }
        let ending = ending
            .unwrap_or_else(|| super::LineEnding::detect(&self.text))
            .text();
        let mut changes = Vec::new();
        for selection in &self.selections {
            let range = selection.range();
            let language = syntax.language_at(range.start);
            let start = line_start(&self.text, range.start);
            let prefix: String = self.text[start..range.start]
                .chars()
                .take_while(|ch| matches!(ch, ' ' | '\t'))
                .collect();
            let opener = syntax.last_code(&self.text, start..range.start);
            let increase = syntax.allows_newline_indent(range.start)
                && opener.is_some_and(|(_, ch)| {
                    super::structure::closing(ch).is_some()
                        && super::structure::supports_brackets(language)
                        || language == crate::highlight::Language::Python && ch == ':'
                });
            let inner = if increase {
                format!(
                    "{prefix}{}",
                    indentation
                        .columns_from(indentation.visual_width(&prefix), indentation.width())
                )
            } else {
                prefix.clone()
            };
            let line_end = self.text[range.end..]
                .find('\n')
                .map_or(self.text.len(), |n| range.end + n);
            let remaining = &self.text[range.end..line_end];
            let split_pair = increase
                && opener.is_some_and(|(position, ch)| {
                    super::structure::closing(ch).is_some_and(|close| {
                        remaining.trim().starts_with(close)
                            && syntax.brackets.iter().any(|&(at, _, pair)| {
                                at == position
                                    && pair
                                        == Some(
                                            range.end + remaining.len()
                                                - remaining.trim_start().len(),
                                        )
                            })
                    })
                });
            let text = if split_pair {
                format!("{ending}{inner}{ending}{prefix}")
            } else {
                format!("{ending}{inner}")
            };
            let end = if split_pair {
                range.end + remaining.len() - remaining.trim_start().len()
            } else {
                range.end
            };
            changes.push((
                Edit::replace(range.start..end, text),
                ending.len() + inner.len(),
            ));
        }
        self.apply_caret_edits(
            changes
                .into_iter()
                .map(|(edit, caret)| (edit, Selection::caret(caret)))
                .collect(),
        )
    }

    /// Apply replacements with selections relative to their inserted text. Retain
    /// selection order, and reject overlapping commands before offset arithmetic.
    pub(super) fn apply_caret_edits(
        &mut self,
        changes: Vec<(Edit, Selection)>,
    ) -> Result<bool, EditError> {
        self.apply_grouped_caret_edits(changes, None)
    }

    pub(super) fn apply_grouped_caret_edits(
        &mut self,
        changes: Vec<(Edit, Selection)>,
        group: Option<u64>,
    ) -> Result<bool, EditError> {
        let mut changes: Vec<_> = changes.into_iter().enumerate().collect();
        changes.sort_by_key(|(_, (edit, _))| (edit.range.start, edit.range.end));
        let mut source = 0;
        let mut target = 0;
        let mut selections = vec![Selection::default(); changes.len()];
        for (index, (edit, selection)) in &changes {
            if edit.range.start < source {
                return Err(EditError::OverlappingEdits);
            }
            target += edit.range.start - source;
            selections[*index] = Selection {
                anchor: target + selection.anchor,
                head: target + selection.head,
            };
            target += edit.text.len();
            source = edit.range.end;
        }
        self.apply(
            changes.into_iter().map(|(_, (edit, _))| edit).collect(),
            selections,
            group,
        )
    }

    /// Map carets inside rewritten indentation by visual column rather than
    /// moving every endpoint to the end of the replacement prefix.
    pub(super) fn apply_indent_edits(
        &mut self,
        edits: Vec<Edit>,
        before: Indentation,
        after: Indentation,
        outdent: Option<bool>,
    ) -> Result<bool, EditError> {
        let map = |position| {
            let mut source = 0;
            let mut target = 0;
            for edit in &edits {
                if position < edit.range.start {
                    return target + position - source;
                }
                target += edit.range.start - source;
                if position <= edit.range.end {
                    let columns = before.visual_width(&self.text[edit.range.start..position]);
                    let columns = match outdent {
                        Some(true) => columns.saturating_sub(after.width()),
                        Some(false) => columns + after.width(),
                        None => columns,
                    };
                    let mut column = 0;
                    for (offset, ch) in edit.text.char_indices() {
                        let next = if ch == '\t' {
                            column + after.tab_width() - column % after.tab_width()
                        } else {
                            column + 1
                        };
                        if columns <= column {
                            return target + offset;
                        }
                        if columns < next {
                            // A DOM caret cannot sit inside a hard tab. Choose the
                            // nearest representable boundary, preferring the left.
                            return target
                                + offset
                                + usize::from(columns - column > next - columns);
                        }
                        column = next;
                    }
                    return target + edit.text.len();
                }
                target += edit.text.len();
                source = edit.range.end;
            }
            target + position - source
        };
        let selections = self
            .selections
            .iter()
            .map(|selection| Selection {
                anchor: map(selection.anchor),
                head: map(selection.head),
            })
            .collect();
        self.apply(edits, selections, None)
    }

    pub(super) fn apply_mapped(&mut self, edits: Vec<Edit>) -> Result<bool, EditError> {
        self.apply_mapped_selections(edits, Vec::new())
    }

    /// Apply one transaction while retaining source selections, with optional
    /// selection overrides relative to a replacement's new text.
    pub(super) fn apply_mapped_selections(
        &mut self,
        edits: Vec<Edit>,
        overrides: Vec<(usize, usize, Selection)>,
    ) -> Result<bool, EditError> {
        let mut indexed: Vec<_> = edits.into_iter().enumerate().collect();
        indexed.sort_by_key(|(_, edit)| (edit.range.start, edit.range.end));
        let mut offsets = vec![0; indexed.len()];
        let mut source = 0;
        let mut target = 0;
        for (index, edit) in &indexed {
            if edit.range.start < source {
                return Err(EditError::OverlappingEdits);
            }
            target += edit.range.start - source;
            offsets[*index] = target;
            target += edit.text.len();
            source = edit.range.end;
        }
        let edits: Vec<_> = indexed.into_iter().map(|(_, edit)| edit).collect();
        let mut selections: Vec<_> = self
            .selections
            .iter()
            .map(|selection| Selection {
                anchor: mapped_position(selection.anchor, &edits),
                head: mapped_position(selection.head, &edits),
            })
            .collect();
        for (caret, edit, after) in overrides {
            selections[caret] = Selection {
                anchor: offsets[edit] + after.anchor,
                head: offsets[edit] + after.head,
            };
        }
        self.apply(edits, selections, None)
    }
}

pub(super) fn line_start(text: &str, position: usize) -> usize {
    text[..position].rfind('\n').map_or(0, |index| index + 1)
}

fn mapped_position(position: usize, edits: &[Edit]) -> usize {
    let mut source = 0;
    let mut target = 0;
    for edit in edits {
        if position < edit.range.start {
            return target + position - source;
        }
        target += edit.range.start - source;
        if position <= edit.range.end {
            return target + edit.text.len();
        }
        target += edit.text.len();
        source = edit.range.end;
    }
    target + position - source
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_enter_preserves_crlf_unicode_pairs_comments_and_single_undo() {
        let mut doc = Document::new("  fn 😀() { }");
        doc.set_selections(vec![Selection::caret(13)]).unwrap();
        doc.newline_with_structure(
            Indentation::default(),
            Some(super::super::LineEnding::CrLf),
            crate::highlight::Language::Rust,
        )
        .unwrap();
        assert_eq!(doc.text(), "  fn 😀() {\r\n      \r\n  }");
        assert_eq!(doc.selections(), &[Selection::caret(21)]);
        doc.undo();
        assert_eq!(doc.text(), "  fn 😀() { }");
        for (text, expected) in [
            ("if x { // note", "if x { // note\n    "),
            ("// if {", "// if {\n"),
            ("let s = \"{", "let s = \"{\n"),
        ] {
            let mut doc = Document::new(text);
            doc.set_selections(vec![Selection::caret(text.len())])
                .unwrap();
            doc.newline_with_structure(
                Indentation::default(),
                None,
                crate::highlight::Language::Rust,
            )
            .unwrap();
            assert_eq!(doc.text(), expected);
        }
        let mut doc = Document::new("if ready:");
        doc.set_selections(vec![Selection::caret(9)]).unwrap();
        doc.newline_with_structure(
            Indentation::default(),
            None,
            crate::highlight::Language::Python,
        )
        .unwrap();
        assert_eq!(doc.text(), "if ready:\n    ");
    }

    #[test]
    fn enter_at_multiple_carets_is_atomic_and_overlapping_selections_are_merged() {
        let mut doc = Document::new("{}\r\n{}");
        doc.set_selections(vec![Selection::caret(5), Selection::caret(1)])
            .unwrap();
        doc.newline_with_structure(
            Indentation::default(),
            None,
            crate::highlight::Language::Rust,
        )
        .unwrap();
        assert_eq!(doc.text(), "{\r\n    \r\n}\r\n{\r\n    \r\n}");
        assert!(doc.selections()[0].head > doc.selections()[1].head);
        doc.undo();
        assert_eq!(doc.text(), "{}\r\n{}");
        doc.set_selections(vec![Selection { anchor: 0, head: 2 }, Selection::caret(1)])
            .unwrap();
        assert_eq!(doc.selections(), &[Selection { anchor: 0, head: 2 }]);
        doc.newline_with_structure(
            Indentation::default(),
            None,
            crate::highlight::Language::Rust,
        )
        .unwrap();
        assert_eq!(doc.text(), "\r\n\r\n{}");
        assert!(doc.undo());
        assert_eq!(doc.text(), "{}\r\n{}");
    }

    #[test]
    fn indentation_preserves_selection_columns_inside_existing_prefixes() {
        let mut doc = Document::new("    x");
        doc.set_selections(vec![Selection { anchor: 1, head: 5 }])
            .unwrap();
        doc.indent_lines(Indentation::default(), false).unwrap();
        assert_eq!(doc.selections(), &[Selection { anchor: 5, head: 9 }]);
        doc.indent_lines(Indentation::default(), true).unwrap();
        assert_eq!(doc.selections(), &[Selection { anchor: 1, head: 5 }]);
        let tabs = Indentation {
            style: IndentStyle::Tabs,
            width: 3,
            tab_width: 3,
        };
        doc.convert_indentation(tabs, 4).unwrap();
        assert_eq!(doc.text(), "\t x");
        assert_eq!(doc.selections(), &[Selection { anchor: 0, head: 3 }]);
    }

    #[test]
    fn block_indent_and_selected_line_indent_use_visual_columns_with_independent_widths() {
        let indentation = Indentation {
            style: IndentStyle::Tabs,
            width: 4,
            tab_width: 3,
        };
        let mut doc = Document::new("  if x {");
        doc.set_selections(vec![Selection::caret(doc.text().len())])
            .unwrap();
        doc.newline_with_structure(indentation, None, crate::highlight::Language::Rust)
            .unwrap();
        assert_eq!(doc.text(), "  if x {\n  \t\t");
        assert_eq!(
            indentation.visual_width(doc.text().split_once('\n').unwrap().1),
            6
        );
        let mut doc = Document::new("  x");
        doc.indent_lines(indentation, false).unwrap();
        assert_eq!(doc.text(), "\t\tx");
        doc.indent_lines(indentation, true).unwrap();
        assert_eq!(doc.text(), "  x");
    }

    #[test]
    fn separate_soft_and_hard_tab_stops_round_trip_outdent() {
        let indentation = Indentation {
            style: IndentStyle::Tabs,
            width: 4,
            tab_width: 3,
        };
        let mut doc = Document::new("  x");
        doc.set_selections(vec![Selection::caret(2)]).unwrap();
        doc.tab(indentation).unwrap();
        assert_eq!(doc.text(), "  \t x");
        doc.indent_lines(indentation, true).unwrap();
        assert_eq!(doc.text(), "x");
        doc.undo();
        assert_eq!(doc.text(), "  \t x");
    }

    #[test]
    fn selected_lines_indent_once_and_preserve_direction_crlf_and_final_newline() {
        let mut doc = Document::new("a\r\nb\r\nc");
        doc.set_selections(vec![
            Selection { anchor: 6, head: 0 },
            Selection { anchor: 1, head: 4 },
        ])
        .unwrap();
        doc.indent_lines(
            Indentation {
                style: IndentStyle::Spaces,
                width: 2,
                tab_width: 2,
            },
            false,
        )
        .unwrap();
        assert_eq!(doc.text(), "  a\r\n  b\r\nc");
        assert_eq!(
            doc.selections()[0],
            Selection {
                anchor: 10,
                head: 2
            }
        );
        assert!(doc.undo());
        assert_eq!(doc.text(), "a\r\nb\r\nc");
        doc.indent_lines(
            Indentation {
                style: IndentStyle::Tabs,
                width: 4,
                tab_width: 4,
            },
            false,
        )
        .unwrap();
        assert_eq!(doc.text(), "\ta\r\n\tb\r\nc");
    }

    #[test]
    fn tab_stops_outdent_and_enter_preserve_unicode_and_indentation() {
        let mut doc = Document::new("  😀value\r\n");
        doc.set_selections(vec![Selection::caret(6)]).unwrap();
        doc.tab(Indentation::default()).unwrap();
        assert_eq!(doc.text(), "  😀 value\r\n");
        doc.newline().unwrap();
        assert_eq!(doc.text(), "  😀 \r\n  value\r\n");
        doc.indent_lines(Indentation::default(), true).unwrap();
        assert_eq!(doc.text(), "  😀 \r\nvalue\r\n");
        doc.undo();
        assert_eq!(doc.text(), "  😀 \r\n  value\r\n");
    }
}
