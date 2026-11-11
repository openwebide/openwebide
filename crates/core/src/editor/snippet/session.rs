use super::{Snippet, SnippetTransform};
use crate::editor::{Document, Edit, EditError, Selection};
use std::{collections::BTreeMap, ops::Range};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SnippetSession {
    fields: BTreeMap<u32, Vec<Range<usize>>>,
    choices: BTreeMap<u32, Vec<String>>,
    transforms: Vec<SnippetTransform>,
    active: u32,
}
impl SnippetSession {
    fn order(&self) -> Vec<u32> {
        self.fields
            .keys()
            .copied()
            .filter(|index| *index != 0)
            .chain(self.fields.contains_key(&0).then_some(0))
            .collect()
    }
    fn selections(&self) -> Vec<Selection> {
        self.fields
            .get(&self.active)
            .into_iter()
            .flatten()
            .map(|range| Selection {
                anchor: range.start,
                head: range.end,
            })
            .collect()
    }
    pub(super) fn contains(&self, selections: &[Selection]) -> bool {
        self.fields.get(&self.active).is_some_and(|fields| {
            selections.iter().all(|selection| {
                let selected = selection.range();
                fields
                    .iter()
                    .any(|field| field.start <= selected.start && selected.end <= field.end)
            })
        })
    }
    pub(crate) fn rebase<T: AsRef<str>>(&mut self, edits: &[Edit<T>]) -> bool {
        let Some(active) = self.fields.get(&self.active).cloned() else {
            return false;
        };
        if !edits.iter().all(|edit| {
            active
                .iter()
                .any(|range| range.start <= edit.range.start && edit.range.end <= range.end)
        }) {
            return false;
        }
        let current = self.active;
        self.fields.retain(|index, ranges| {
            ranges.retain_mut(|range| {
                let is_parent = active
                    .iter()
                    .any(|child| range.start <= child.start && child.end <= range.end);
                if *index != current
                    && !is_parent
                    && edits.iter().any(|edit| {
                        edit.range.start < range.end && range.start < edit.range.end
                            || edit.range == *range && !edit.range.is_empty()
                    })
                {
                    return false;
                }
                let start = mapped(range.start, !is_parent, edits);
                let end = mapped(range.end, is_parent, edits);
                *range = start..end.max(start);
                true
            });
            !ranges.is_empty()
        });
        for transform in &mut self.transforms {
            transform.range = mapped(transform.range.start, true, edits)
                ..mapped(transform.range.end, true, edits);
        }
        true
    }
}

fn mapped<T: AsRef<str>>(position: usize, right: bool, edits: &[Edit<T>]) -> usize {
    let mut before = 0;
    let mut after = 0;
    for edit in edits {
        if position < edit.range.start || position == edit.range.start && !right {
            break;
        }
        after += edit.range.start - before;
        if position <= edit.range.end {
            return after + if right { edit.text.as_ref().len() } else { 0 };
        }
        after += edit.text.as_ref().len();
        before = edit.range.end;
    }
    after + position - before
}

impl Document {
    /// Activate exact ranges after the completion's single transaction. Snippet
    /// state is transient; ordinary text/history/recovery remain authoritative.
    pub fn start_snippet(&mut self, snippet: Snippet) -> Result<(), EditError> {
        if snippet.fields.values().map(Vec::len).sum::<usize>() > super::MAX_FIELDS
            || snippet.transforms.len() > super::MAX_FIELDS
        {
            return Err(EditError::TooManySelections);
        }
        for range in snippet
            .fields
            .values()
            .flatten()
            .chain(snippet.transforms.iter().map(|transform| &transform.range))
        {
            if range.start > range.end || self.text.get(range.clone()).is_none() {
                return Err(EditError::InvalidSelection);
            }
        }
        let mut session = SnippetSession {
            fields: snippet.fields,
            choices: snippet.choices,
            transforms: snippet.transforms,
            active: 0,
        };
        let Some(first) = session.order().first().copied() else {
            return Ok(());
        };
        session.active = first;
        self.set_selections(session.selections())?;
        self.snippet = (first != 0).then_some(session);
        Ok(())
    }
    pub fn snippet_active(&self) -> bool {
        self.snippet.is_some()
    }
    pub fn cancel_snippet(&mut self) -> bool {
        let active = self.snippet.take().is_some();
        if active {
            self.selections.truncate(1);
        }
        active
    }
    pub fn snippet_choices_available(&self) -> bool {
        self.snippet
            .as_ref()
            .is_some_and(|session| session.choices.contains_key(&session.active))
    }
    /// Tab/Shift+Tab visits numbered fields; zero exits. Derived mirrors update
    /// through an ordinary undoable transaction before changing the selection.
    pub fn snippet_next(&mut self, backwards: bool) -> Result<bool, String> {
        if self.is_composing() {
            return Ok(false);
        }
        let Some(mut session) = self.snippet.clone() else {
            return Ok(false);
        };
        if !session.contains(&self.selections) {
            self.snippet = None;
            return Ok(false);
        }
        let value = session
            .fields
            .get(&session.active)
            .and_then(|ranges| ranges.first())
            .and_then(|range| self.text.get(range.clone()))
            .ok_or("Snippet placeholder is no longer available")?
            .to_owned();
        let edits = session
            .transforms
            .iter()
            .filter(|transform| transform.field == session.active)
            .map(|transform| {
                Ok(Edit::replace(
                    transform.range.clone(),
                    transform.apply(&value)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if !edits.is_empty() {
            let selections = self
                .selections
                .iter()
                .map(|selection| Selection {
                    anchor: mapped(selection.anchor, true, &edits),
                    head: mapped(selection.head, true, &edits),
                })
                .collect();
            for ranges in session.fields.values_mut() {
                for range in ranges {
                    *range = mapped(range.start, true, &edits)..mapped(range.end, true, &edits);
                }
            }
            for transform in &mut session.transforms {
                transform.range = mapped(transform.range.start, false, &edits)
                    ..mapped(transform.range.end, true, &edits);
            }
            self.apply(edits, selections, None)
                .map_err(|error| error.to_string())?;
        }
        let order = session.order();
        let index = order
            .iter()
            .position(|index| *index == session.active)
            .ok_or("Snippet field was removed")?;
        let next = if backwards {
            index.checked_sub(1).unwrap_or(index)
        } else {
            index + 1
        };
        let Some(next) = order.get(next).copied() else {
            self.snippet = None;
            return Ok(true);
        };
        session.active = next;
        self.set_selections(session.selections())
            .map_err(|error| error.to_string())?;
        self.snippet = (next != 0).then_some(session);
        Ok(true)
    }
    /// Choice placeholders use the same multi-selection replacement path as
    /// typed text. Returning false leaves ordinary editor shortcuts available.
    pub fn snippet_choice(&mut self, backwards: bool) -> Result<bool, String> {
        let Some(session) = self.snippet.as_ref() else {
            return Ok(false);
        };
        let Some(values) = session.choices.get(&session.active) else {
            return Ok(false);
        };
        let fields = session
            .fields
            .get(&session.active)
            .ok_or("Snippet field is no longer available")?;
        let value = fields
            .first()
            .and_then(|range| self.text.get(range.clone()))
            .unwrap_or_default();
        let index = values
            .iter()
            .position(|choice| choice == value)
            .unwrap_or(0);
        let index = if backwards {
            (index + values.len() - 1) % values.len()
        } else {
            (index + 1) % values.len()
        };
        let value = values[index].clone();
        let selections = session.selections();
        self.set_selections(selections)
            .map_err(|error| error.to_string())?;
        self.replace_selections(&value, None)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }
    pub(in crate::editor) fn rebase_snippet<T: AsRef<str>>(&mut self, edits: &[Edit<T>]) {
        if self
            .snippet
            .as_mut()
            .is_some_and(|snippet| !snippet.rebase(edits))
        {
            self.snippet = None;
        }
    }
    pub(in crate::editor) fn retain_snippet_selection(&mut self) {
        if self
            .snippet
            .as_ref()
            .is_some_and(|snippet| !snippet.contains(&self.selections))
        {
            self.snippet = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document(template: &str) -> Document {
        let snippet = Snippet::expand(template, &BTreeMap::new()).unwrap();
        let mut document = Document::new("");
        document
            .apply(
                vec![Edit::replace(0..0, snippet.text.clone())],
                vec![Selection::caret(0)],
                None,
            )
            .unwrap();
        document.start_snippet(snippet).unwrap();
        document
    }
    #[test]
    fn typing_mirrors_unicode_and_tab_visits_rebased_fields_before_zero() {
        let mut doc = document("${1:name} = $1; ${2:文}$0");
        assert_eq!(
            doc.selections(),
            &[
                Selection { anchor: 0, head: 4 },
                Selection {
                    anchor: 7,
                    head: 11
                }
            ]
        );
        doc.replace_selections("😀", None).unwrap();
        assert_eq!(doc.text(), "😀 = 😀; 文");
        assert!(doc.snippet_next(false).unwrap());
        assert_eq!(&doc.text()[doc.selections()[0].range()], "文");
        doc.snippet_next(true).unwrap();
        assert_eq!(doc.selections().len(), 2);
        doc.snippet_next(false).unwrap();
        doc.snippet_next(false).unwrap();
        assert!(!doc.snippet_active());
        assert_eq!(doc.selections(), &[Selection::caret(doc.text().len())]);
        assert!(doc.undo());
        assert_eq!(doc.text(), "name = name; 文");
        assert!(doc.undo());
        assert_eq!(doc.text(), "");
    }
    #[test]
    fn choices_transforms_and_explicit_cancellation_use_normal_history() {
        let mut doc = document("${1|one,two|}: ${1/(.*)/${1:/upcase}/} $0");
        assert!(doc.snippet_choice(false).unwrap());
        assert_eq!(doc.text(), "two: ONE ");
        doc.snippet_next(false).unwrap();
        assert_eq!(doc.text(), "two: TWO ");
        assert!(!doc.snippet_active());
        doc.undo();
        assert_eq!(doc.text(), "two: ONE ");
        let mut doc = document("${1:foo} $0");
        doc.set_selections(vec![Selection::caret(doc.text().len())])
            .unwrap();
        assert!(!doc.snippet_active());
    }
}
