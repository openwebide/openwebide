//! Generic character transactions controlled by contributed rules and regions.
use super::*;
impl Document {
    pub(super) fn type_declared_character(
        &mut self,
        ch: char,
        syntax: &Structure,
    ) -> Result<bool, EditError> {
        let mut changes = Vec::new();
        let mut created = std::collections::BTreeMap::new();
        for selection in &self.selections {
            let range = selection.range();
            let configuration = syntax
                .configuration_at(range.start)
                .expect("contributed context");
            let after = syntax.next_character(range.end);
            let start = line_start(&self.text, range.start);
            let prefix = &self.text[start..range.start];
            if !syntax.available()
                && range.is_empty()
                && syntax.is_closing_at(range.start, ch)
                && prefix.chars().all(|ch| matches!(ch, ' ' | '\t'))
                && let Some(indentation) = self.newline_indentation.iter().find(|indentation| {
                    indentation.revision == self.revision
                        && indentation.position == range.start
                        && indentation.closing == ch
                })
            {
                let text = format!("{}{ch}", indentation.base);
                changes.push((
                    Edit::replace(
                        start..range.end + if after == Some(ch) { ch.len_utf8() } else { 0 },
                        &text,
                    ),
                    Selection::caret(text.len()),
                ));
                continue;
            }
            if range.is_empty()
                && syntax.is_closing_at(range.start, ch)
                && syntax.is_code(range.start)
                && prefix.chars().all(|ch| matches!(ch, ' ' | '\t'))
                && let Some(&(open, _, _)) =
                    syntax
                        .brackets
                        .iter()
                        .rev()
                        .find(|&&(position, bracket, pair)| {
                            position < range.start
                                && syntax.closing_at(position, bracket) == Some(ch)
                                && pair.is_none_or(|close| close >= range.start)
                        })
            {
                let open_start = line_start(&self.text, open);
                let base: String = self.text[open_start..open]
                    .chars()
                    .take_while(|ch| matches!(ch, ' ' | '\t'))
                    .collect();
                let text = format!("{base}{ch}");
                changes.push((
                    Edit::replace(
                        start..range.end + if after == Some(ch) { ch.len_utf8() } else { 0 },
                        &text,
                    ),
                    Selection::caret(text.len()),
                ));
                continue;
            }
            let quote = configuration
                .auto_closing_pairs
                .iter()
                .any(|pair| pair.close == ch && pair.open == ch)
                || configuration
                    .surrounding_pairs
                    .iter()
                    .any(|pair| pair.close == ch && pair.open == ch);
            let paired_closer = self.automatic_closer_at(range.start, ch)
                || syntax.is_closing_at(range.start, ch)
                    && syntax.brackets.iter().any(|&(position, bracket, partner)| {
                        position == range.start && bracket == ch && partner.is_some()
                    });
            if range.is_empty()
                && after == Some(ch)
                && (paired_closer || quote && syntax.quote_closes_at_character(range.start, ch))
            {
                changes.push((
                    Edit::replace(range.start..range.start + ch.len_utf8(), ch.to_string()),
                    Selection::caret(ch.len_utf8()),
                ));
                continue;
            }
            let automatic = syntax.automatic_pair(range.start, range.end, ch);
            let close = if !range.is_empty() {
                // A surrounding pair is an explicit selection command. Its
                // declaration has no automatic-pair context exclusions and
                // remains usable before host analysis supplies any regions.
                configuration
                    .surrounding_pairs
                    .iter()
                    .find(|pair| pair.open == ch)
                    .map(|pair| pair.close)
            } else {
                automatic
                    .filter(|pair| {
                        pair.after.is_some()
                            || after.is_none_or(|after| {
                                after.is_whitespace()
                                    || matches!(after, ',' | ';' | ':')
                                    || configuration
                                        .brackets
                                        .iter()
                                        .any(|bracket| bracket.close == after)
                            })
                    })
                    .map(|pair| pair.close)
            };
            if let Some(close) = close {
                let selected = &self.text[range.clone()];
                let text = format!("{ch}{selected}{close}");
                let first = ch.len_utf8();
                let last = first + selected.len();
                let after = if selection.anchor <= selection.head {
                    Selection {
                        anchor: first,
                        head: last,
                    }
                } else {
                    Selection {
                        anchor: last,
                        head: first,
                    }
                };
                created.insert(range.start, (ch, close));
                changes.push((Edit::replace(range, text), after));
            } else {
                changes.push(typed_character(*selection, ch));
            }
        }
        let mut ordered: Vec<_> = changes.iter().map(|(edit, _)| edit).collect();
        ordered.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let (mut source, mut target) = (0, 0);
        let mut pairs = Vec::new();
        for edit in ordered {
            target += edit
                .range
                .start
                .checked_sub(source)
                .ok_or(EditError::OverlappingEdits)?;
            if let Some(&(opening, closing)) = created.get(&edit.range.start) {
                pairs.push(AutomaticPair {
                    open: target,
                    close: target + edit.text.len() - closing.len_utf8(),
                    opening,
                    closing,
                });
            }
            target += edit.text.len();
            source = edit.range.end;
        }
        let changed = self.apply_caret_edits(changes)?;
        if changed {
            self.automatic_pairs.extend(pairs);
            let excess = self
                .automatic_pairs
                .len()
                .saturating_sub(super::super::MAX_SELECTIONS);
            self.automatic_pairs.drain(..excess);
        }
        Ok(changed)
    }

    pub(super) fn delete_declared_pairs(&mut self, syntax: &Structure) -> Result<bool, EditError> {
        let mut changes = Vec::new();
        for selection in &self.selections {
            if selection.anchor != selection.head {
                return Ok(false);
            }
            let position = selection.head;
            let Some((open, close)) = self.text[..position]
                .chars()
                .next_back()
                .zip(self.text[position..].chars().next())
            else {
                return Ok(false);
            };
            let configuration = syntax
                .configuration_at(position)
                .expect("contributed context");
            let declared = configuration
                .auto_closing_pairs
                .iter()
                .any(|pair| pair.open == open && pair.close == close)
                || configuration
                    .surrounding_pairs
                    .iter()
                    .any(|pair| pair.open == open && pair.close == close);
            let matched = declared
                && (syntax.brackets.iter().any(|&(offset, bracket, partner)| {
                    offset + open.len_utf8() == position
                        && bracket == open
                        && partner == Some(position)
                }) || syntax.protected.iter().any(|(range, complete, kind)| {
                    *complete
                        && matches!(
                            kind,
                            super::super::structure::RegionKind::String
                                | super::super::structure::RegionKind::Template
                        )
                        && range.start + open.len_utf8() == position
                        && range.end == position + close.len_utf8()
                }));
            if !matched {
                return Ok(false);
            }
            changes.push((
                Edit::replace(position - open.len_utf8()..position + close.len_utf8(), ""),
                Selection::caret(0),
            ));
        }
        self.apply_caret_edits(changes)
    }
}
