//! Comment commands use language syntax and logical-line selections.
use super::{
    Document, Edit, EditError, Selection, Structure,
    lines::{row_at, selected_rows},
};
use crate::highlight::Language;

pub fn line_comment(language: Language) -> Option<&'static str> {
    match language {
        Language::Rust
        | Language::JavaScript
        | Language::TypeScript
        | Language::Jsx
        | Language::Tsx
        | Language::Java
        | Language::CSharp
        | Language::Php
        | Language::C
        | Language::Cpp
        | Language::Go => Some("//"),
        Language::Python | Language::Shell | Language::Toml | Language::Yaml | Language::Ini => {
            Some("#")
        }
        Language::Sql => Some("--"),
        _ => None,
    }
}
pub fn block_comment(language: Language) -> Option<(&'static str, &'static str)> {
    match language {
        Language::Rust
        | Language::JavaScript
        | Language::TypeScript
        | Language::Jsx
        | Language::Tsx
        | Language::Java
        | Language::CSharp
        | Language::Php
        | Language::C
        | Language::Cpp
        | Language::Go
        | Language::Css
        | Language::Sql => Some(("/*", "*/")),
        Language::Html | Language::Xml | Language::Markdown | Language::MarkdownInline => {
            Some(("<!--", "-->"))
        }
        _ => None,
    }
}

/// Line-comment actions use a block comment when the language has no line marker.
pub fn supports_line_comment(language: Language) -> bool {
    line_comment(language).is_some() || block_comment(language).is_some()
}

pub fn supports_block_comment(language: Language) -> bool {
    block_comment(language).is_some()
}
fn balanced_comments(text: &str) -> bool {
    let mut depth = 0;
    let mut position = 0;
    while position < text.len() {
        if text[position..].starts_with("/*") {
            depth += 1;
            position += 2;
        } else if text[position..].starts_with("*/") {
            if depth == 0 {
                return false;
            }
            depth -= 1;
            position += 2;
        } else {
            position += text[position..].chars().next().unwrap().len_utf8();
        }
    }
    depth == 0
}
impl Document {
    /// Unsupported syntaxes leave text unchanged rather than introduce invalid comments.
    pub fn toggle_line_comments(&mut self, language: Language) -> Result<bool, EditError> {
        let length = self.text.len();
        self.toggle_line_comments_in(|_| (language, 0..length))
    }

    pub fn toggle_line_comments_with_context(
        &mut self,
        syntax: &Structure,
    ) -> Result<bool, EditError> {
        if !syntax.matches_source(&self.text) {
            return Err(EditError::StaleContext);
        }
        self.toggle_line_comments_in(|position| {
            (syntax.language_at(position), syntax.language_body(position))
        })
    }

    fn toggle_line_comments_in(
        &mut self,
        context: impl Fn(usize) -> (Language, std::ops::Range<usize>),
    ) -> Result<bool, EditError> {
        let rows = &self.line_index.rows;
        let mut changes = CommentChanges::default();
        let mut selected = std::collections::BTreeMap::new();
        let blank = self
            .selections
            .iter()
            .all(|selection| selection.range().is_empty());
        for (index, selection) in self.selections.iter().enumerate() {
            let range = selection.range();
            let (language, body) = context(range.start);
            let Some(marker) = line_comment(language) else {
                let Some((edit, after)) =
                    block_comment_change(&self.text, rows, *selection, language, body)
                else {
                    return Ok(false);
                };
                changes.add(edit, Some((index, after)))?;
                continue;
            };
            if range.start < body.start || range.end > body.end {
                return Ok(false);
            }
            for row in selected_rows(rows, &[*selection]).into_iter().flatten() {
                let line = &rows[row];
                let start = line.start.max(body.start);
                let end = line.body_end.min(body.end);
                let text = &self.text[start..end];
                if text.trim().is_empty() && !blank {
                    continue;
                }
                let prefix = text.len() - text.trim_start_matches([' ', '\t']).len();
                selected.insert((start + prefix, end), marker);
            }
        }
        let remove = !selected.is_empty()
            && selected
                .iter()
                .all(|(&(start, end), marker)| self.text[start..end].starts_with(marker));
        for ((start, end), marker) in selected {
            let edit = if remove {
                let mut stop = start + marker.len();
                if stop < end && self.text.as_bytes()[stop] == b' ' {
                    stop += 1;
                }
                Edit::replace(start..stop, "")
            } else {
                Edit::replace(start..start, format!("{marker} "))
            };
            changes.add(edit, None)?;
        }
        changes.apply(self)
    }
    pub fn toggle_block_comments(&mut self, language: Language) -> Result<bool, EditError> {
        let length = self.text.len();
        self.toggle_block_comments_in(|_| (language, 0..length))
    }

    pub fn toggle_block_comments_with_context(
        &mut self,
        syntax: &Structure,
    ) -> Result<bool, EditError> {
        if !syntax.matches_source(&self.text) {
            return Err(EditError::StaleContext);
        }
        self.toggle_block_comments_in(|position| {
            (syntax.language_at(position), syntax.language_body(position))
        })
    }

    fn toggle_block_comments_in(
        &mut self,
        context: impl Fn(usize) -> (Language, std::ops::Range<usize>),
    ) -> Result<bool, EditError> {
        let rows = &self.line_index.rows;
        let mut changes = CommentChanges::default();
        for (index, selection) in self.selections.iter().enumerate() {
            let (language, body) = context(selection.range().start);
            let Some((edit, after)) =
                block_comment_change(&self.text, rows, *selection, language, body)
            else {
                return Ok(false);
            };
            changes.add(edit, Some((index, after)))?;
        }
        changes.apply(self)
    }
}
fn block_comment_change(
    text: &str,
    rows: &[super::lines::Line],
    selection: Selection,
    language: Language,
    body: std::ops::Range<usize>,
) -> Option<(Edit, Selection)> {
    let mut range = selection.range();
    let (open, close) = block_comment(language)?;
    if range.is_empty() {
        let row = &rows[row_at(rows, selection.head)];
        let body = &text[row.start..row.body_end];
        range = row.start + body.len() - body.trim_start_matches([' ', '\t']).len()..row.body_end;
    }
    if selection.range().is_empty() {
        range.start = range.start.max(body.start);
        range.end = range.end.min(body.end);
    } else if range.start < body.start || range.end > body.end {
        return None;
    }
    if range.start > open.len()
        && text[..range.start].ends_with(&format!("{open} "))
        && text[range.end..].starts_with(&format!(" {close}"))
    {
        let expanded = range.start - open.len() - 1..range.end + close.len() + 1;
        if expanded.start >= body.start && expanded.end <= body.end {
            range = expanded;
        }
    }
    let selected = &text[range.clone()];
    let (text, anchor, head) = if selected.starts_with(open)
        && selected.ends_with(close)
        && selected.len() >= open.len() + close.len()
    {
        let inside = &selected[open.len()..selected.len() - close.len()];
        let inside = inside.strip_prefix(' ').unwrap_or(inside);
        let inside = inside.strip_suffix(' ').unwrap_or(inside);
        (inside.to_string(), 0, inside.len())
    } else {
        // A closer in the selection would terminate the outer comment early.
        // Rust permits nested comments; other languages must leave it alone.
        if (language == Language::Rust && !balanced_comments(selected))
            || (language != Language::Rust && selected.contains(close))
        {
            return None;
        }
        (
            format!("{open} {selected} {close}"),
            open.len() + 1,
            open.len() + 1 + selected.len(),
        )
    };
    let after = if selection.range().is_empty() {
        Selection::caret(text.len())
    } else if selection.anchor <= selection.head {
        Selection { anchor, head }
    } else {
        Selection {
            anchor: head,
            head: anchor,
        }
    };
    Some((Edit::replace(range, text), after))
}

#[derive(Default)]
struct CommentChanges {
    edits: Vec<Edit>,
    ranges: std::collections::BTreeMap<(usize, usize), usize>,
    selections: Vec<(usize, usize, Selection)>,
}

impl CommentChanges {
    fn add(&mut self, edit: Edit, selection: Option<(usize, Selection)>) -> Result<(), EditError> {
        let key = (edit.range.start, edit.range.end);
        let index = if let Some(&index) = self.ranges.get(&key) {
            if self.edits[index].text != edit.text {
                return Err(EditError::OverlappingEdits);
            }
            index
        } else {
            let index = self.edits.len();
            self.ranges.insert(key, index);
            self.edits.push(edit);
            index
        };
        if let Some((caret, after)) = selection {
            self.selections.push((caret, index, after));
        }
        Ok(())
    }

    fn apply(self, document: &mut Document) -> Result<bool, EditError> {
        document.apply_mapped_selections(self.edits, self.selections)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn configuration_and_inline_markdown_use_their_own_comment_syntax() {
        use crate::highlight::Language;
        assert_eq!(super::line_comment(Language::Ini), Some("#"));
        for language in [Language::Xml, Language::Markdown, Language::MarkdownInline] {
            assert_eq!(super::line_comment(language), None);
            assert_eq!(super::block_comment(language), Some(("<!--", "-->")));
        }
    }

    use super::*;
    #[test]
    fn line_comments_toggle_mixed_indentation_blank_lines_and_reversed_crlf_selection() {
        let original = "  x\r\n\ty\r\n\r\nz";
        let mut doc = Document::new(original);
        doc.set_selections(vec![Selection {
            anchor: 11,
            head: 0,
        }])
        .unwrap();
        doc.toggle_line_comments(Language::Rust).unwrap();
        assert_eq!(doc.text(), "  // x\r\n\t// y\r\n\r\nz");
        doc.toggle_line_comments(Language::Rust).unwrap();
        assert_eq!(doc.text(), original);
        doc.undo();
        assert!(doc.text().contains("// x"));
        let mut json = Document::new("{\"x\":1}");
        assert!(!json.toggle_line_comments(Language::Json).unwrap());
    }
    #[test]
    fn blank_line_comments_work_and_rust_nesting_must_be_balanced() {
        let mut doc = Document::new("  ");
        doc.set_selections(vec![Selection::caret(2)]).unwrap();
        doc.toggle_line_comments(Language::Rust).unwrap();
        assert_eq!(doc.text(), "  // ");
        doc.toggle_line_comments(Language::Rust).unwrap();
        assert_eq!(doc.text(), "  ");
        for text in ["/* incomplete", "orphan */"] {
            let mut doc = Document::new(text);
            let before = doc.clone();
            assert!(!doc.toggle_block_comments(Language::Rust).unwrap());
            assert_eq!(doc, before);
        }
    }

    #[test]
    fn block_comments_preserve_direction_and_reject_unsafe_nesting() {
        let mut doc = Document::new("😀");
        doc.set_selections(vec![Selection { anchor: 4, head: 0 }])
            .unwrap();
        doc.toggle_block_comments(Language::Rust).unwrap();
        assert_eq!(doc.text(), "/* 😀 */");
        assert_eq!(doc.selections(), &[Selection { anchor: 7, head: 3 }]);
        doc.toggle_block_comments(Language::Rust).unwrap();
        assert_eq!(doc.text(), "😀");
        doc.undo();
        assert_eq!(doc.text(), "/* 😀 */");
        let mut css = Document::new("x */ y");
        assert!(!css.toggle_block_comments(Language::Css).unwrap());
    }
}
