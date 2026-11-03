//! Enter transactions interpret provider data without choosing a language.
use super::*;
use crate::editor::{Structure, structure::language_rules::matches_expression};
use crate::plugins::languages::EnterAction;

impl Document {
    pub(super) fn newline_declared(
        &mut self,
        indentation: Indentation,
        ending: Option<super::super::LineEnding>,
        syntax: &Structure,
    ) -> Result<bool, EditError> {
        let ending = ending
            .unwrap_or_else(|| super::super::LineEnding::detect(&self.text))
            .text();
        let mut changes = Vec::new();
        for selection in &self.selections {
            let range = selection.range();
            let configuration = syntax
                .configuration_at(range.start)
                .expect("contributed context");
            let start = line_start(&self.text, range.start);
            let before = &self.text[start..range.start];
            let prefix: String = before
                .chars()
                .take_while(|ch| matches!(ch, ' ' | '\t'))
                .collect();
            let line_end = self.text[range.end..]
                .find('\n')
                .map_or(self.text.len(), |offset| range.end + offset);
            let after = &self.text[range.end..line_end];
            let before_rule =
                &before[before.ceil_char_boundary(before.len().saturating_sub(8192))..];
            let after_rule = &after[..after.floor_char_boundary(after.len().min(8192))];
            let rules = &configuration.indentation;
            let allow_rules = syntax.allows_newline_indent(range.start)
                || syntax.declarative_indentation_at(range.start);
            let action = allow_rules
                .then(|| {
                    rules
                        .on_enter
                        .iter()
                        .find(|rule| {
                            matches_expression(&rule.before, before_rule)
                                && rule.after.as_ref().is_none_or(|expression| {
                                    matches_expression(expression, after_rule)
                                })
                        })
                        .map(|rule| rule.action)
                })
                .flatten();
            let increase = action.is_some_and(|action| {
                matches!(action, EnterAction::Indent | EnterAction::IndentOutdent)
            }) || action.is_none()
                && allow_rules
                && [rules.increase.as_ref(), rules.indent_next_line.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|expression| {
                        let code = syntax
                            .last_code(&self.text, start..range.start)
                            .map(|(position, ch)| &self.text[start..position + ch.len_utf8()]);
                        matches_expression(expression, code.unwrap_or(before_rule))
                    });
            let base = if action == Some(EnterAction::Outdent) {
                indentation.columns(
                    indentation
                        .visual_width(&prefix)
                        .saturating_sub(indentation.width()),
                )
            } else {
                prefix
            };
            let inner = if increase {
                format!(
                    "{base}{}",
                    indentation.columns_from(indentation.visual_width(&base), indentation.width())
                )
            } else {
                base.clone()
            };
            let split = action == Some(EnterAction::IndentOutdent)
                || increase
                    && syntax
                        .last_code(&self.text, start..range.start)
                        .is_some_and(|(position, ch)| {
                            syntax.closing_at(position, ch).is_some_and(|close| {
                                after.trim_start().starts_with(close)
                                    && syntax.brackets.iter().any(|&(offset, _, partner)| {
                                        offset == position
                                            && partner
                                                == Some(
                                                    range.end + after.len()
                                                        - after.trim_start().len(),
                                                )
                                    })
                            })
                        });
            let text = if split {
                format!("{ending}{inner}{ending}{base}")
            } else {
                format!("{ending}{inner}")
            };
            changes.push((
                Edit::replace(range, text),
                Selection::caret(ending.len() + inner.len()),
            ));
        }
        self.apply_caret_edits(changes)
    }
}
