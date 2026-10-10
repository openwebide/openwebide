//! Reindent selected lines from shared block contexts; not a formatter.
use super::{
    Document, Edit, EditError, Indentation, Structure, lines::selected_rows, structure::closing,
};
use crate::highlight::Language;

/// Containers support reindent through their parsed code bodies. This command
/// preserves prose/markup; it is not a whole-document formatter.
pub fn supports_reindent(language: Language) -> bool {
    super::supports_brackets(language) || matches!(language, Language::Html | Language::Markdown)
}
impl Document {
    pub fn reindent(
        &mut self,
        indentation: Indentation,
        language: Language,
    ) -> Result<bool, EditError> {
        let syntax = Structure::new(&self.text, language);
        self.reindent_in(indentation, &syntax)
    }

    pub fn reindent_with_context(
        &mut self,
        indentation: Indentation,
        syntax: &Structure,
    ) -> Result<bool, EditError> {
        if !syntax.matches_source(&self.text) {
            return Err(EditError::StaleContext);
        }
        self.reindent_in(indentation, syntax)
    }

    fn reindent_in(
        &mut self,
        indentation: Indentation,
        syntax: &Structure,
    ) -> Result<bool, EditError> {
        if !syntax.available() {
            return Ok(false);
        }
        let rows = &self.line_index.rows;
        let selected = selected_rows(rows, &self.selections);
        let mut stack: Vec<(usize, usize)> = Vec::new();
        let mut cursor = 0;
        let mut edits = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let body = &self.text[row.start..row.body_end];
            let trimmed = body.trim_start_matches([' ', '\t']);
            let prefix = &body[..body.len() - trimmed.len()];
            let first = row.start + prefix.len();
            if stack
                .last()
                .is_some_and(|(position, _)| !syntax.same_language_body(*position, first))
            {
                stack.clear();
            }
            let existing = indentation.visual_width(prefix);
            let mut columns = stack
                .last()
                .map_or(existing, |(_, columns)| columns + indentation.width());
            if let Some(&(position, ch, Some(open))) = syntax.brackets.get(cursor)
                && position == first
                && matches!(ch, ')' | ']' | '}')
                && let Some((_, base)) = stack.iter().find(|(position, _)| *position == open)
            {
                columns = *base;
            }

            if selected.iter().any(|range| range.contains(&index))
                && !trimmed.is_empty()
                && super::structure::supports_brackets(syntax.language_at(first))
                && syntax.is_code(row.start)
                && syntax.is_code(first)
            {
                let replacement = indentation.columns(columns);
                if replacement != prefix {
                    edits.push(Edit::replace(row.start..first, replacement));
                }
            } else {
                columns = existing;
            }
            while let Some(&(position, bracket, pair)) = syntax
                .brackets
                .get(cursor)
                .filter(|(position, _, _)| *position < row.end)
            {
                if stack
                    .last()
                    .is_some_and(|(open, _)| !syntax.same_language_body(*open, position))
                {
                    stack.clear();
                }
                if closing(bracket).is_some() {
                    stack.push((position, columns));
                } else if let Some(open) = pair
                    && stack.last().is_some_and(|(position, _)| *position == open)
                {
                    stack.pop();
                }
                cursor += 1;
            }
        }
        self.apply_indent_edits(edits, indentation, indentation, None)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reindent_keeps_python_block_depth_and_multiline_string_contents() {
        let original = "if ready:\n    a()\n    b()\ntext = \"\"\"\n  literal\n\"\"\"";
        let mut doc = Document::new(original);
        doc.set_selections(vec![super::super::Selection {
            anchor: 0,
            head: original.len(),
        }])
        .unwrap();
        doc.reindent(
            Indentation {
                width: 2,
                ..Indentation::default()
            },
            Language::Python,
        )
        .unwrap();
        assert_eq!(doc.text(), original);
    }

    #[test]
    fn reindent_selected_blocks_ignores_braces_in_literals_and_is_undoable() {
        let original = "fn f() {\r\nx();\r\nif x {\r\nlet s = \"}\";\r\n}\r\n}";
        let mut doc = Document::new(original);
        doc.set_selections(vec![super::super::Selection {
            anchor: 0,
            head: original.len(),
        }])
        .unwrap();
        doc.reindent(Indentation::default(), Language::Rust)
            .unwrap();
        assert_eq!(
            doc.text(),
            "fn f() {\r\n    x();\r\n    if x {\r\n        let s = \"}\";\r\n    }\r\n}"
        );
        doc.undo();
        assert_eq!(doc.text(), original);
    }
}
