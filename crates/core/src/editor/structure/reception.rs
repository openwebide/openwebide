//! Structural patch resolution and proof retained between receiver yields.
use super::*;

struct Records<T> {
    output: Vec<T>,
    patch: Option<(usize, usize, std::vec::IntoIter<T>)>,
    phase: usize,
    offset: usize,
}
impl<T: Clone + PartialEq> Records<T> {
    fn new(data: StructuralRecords<T>, length: usize) -> Self {
        match data {
            StructuralRecords::Full(output) => Self {
                output,
                patch: None,
                phase: 3,
                offset: 0,
            },
            StructuralRecords::Replace { start, end, items } => Self {
                output: Vec::with_capacity(length),
                patch: Some((start, end, items.into_iter())),
                phase: 0,
                offset: 0,
            },
        }
    }
    fn advance(&mut self, previous: &[T], budget: &mut usize) -> bool {
        let Some((start, end, items)) = self.patch.as_mut() else {
            return true;
        };
        while self.phase < 3 && *budget > 0 {
            match self.phase {
                0 => {
                    if self.offset == *start {
                        self.phase = 1;
                        continue;
                    }
                    self.output.push(previous[self.offset].clone());
                    self.offset += 1;
                }
                1 => {
                    let Some(item) = items.next() else {
                        self.phase = 2;
                        self.offset = *end;
                        continue;
                    };
                    self.output.push(item);
                }
                _ => {
                    if self.offset == previous.len() {
                        self.phase = 3;
                        continue;
                    }
                    self.output.push(previous[self.offset].clone());
                    self.offset += 1;
                }
            }
            *budget -= 1;
        }
        self.phase == 3
    }
}
struct Changes {
    language: Language,
    scopes: Records<(Range<usize>, Language)>,
    selections: Records<Range<usize>>,
    opaque_starts: Records<usize>,
    protected: Records<(Range<usize>, bool, RegionKind)>,
    brackets: Records<(usize, char, Option<usize>)>,
}

pub(in crate::editor) struct StructureReception<'a> {
    source: Arc<String>,
    previous: Option<&'a Structure>,
    changes: Option<Changes>,
    output: Option<Structure>,
    phase: usize,
    index: usize,
    count: usize,
}
impl<'a> StructureReception<'a> {
    pub fn new(
        data: StructurePublication,
        source: Arc<String>,
        previous: Option<&'a Structure>,
        budget: usize,
    ) -> Option<Self> {
        let (changes, output, count) = match data {
            StructurePublication::Full(data) => {
                let count = data.record_count();
                if count > budget || data.brackets.len() > MAX_BRACKETS {
                    return None;
                }
                (None, Some(Self::structure(data, source.clone())), count)
            }
            StructurePublication::Changes { changes } => {
                let old = previous.filter(|old| old.language == changes.language)?;
                // Prove all lengths and the expanded budget before allocating any list.
                let lengths = [
                    changes.scopes.resolved_len(&old.scopes)?,
                    changes.selections.resolved_len(&old.selection_ranges)?,
                    changes.opaque_starts.resolved_len(&old.opaque_starts)?,
                    changes.protected.resolved_len(&old.protected)?,
                    changes.brackets.resolved_len(&old.brackets)?,
                ];
                let count = lengths
                    .iter()
                    .try_fold(0_usize, |sum, len| sum.checked_add(*len))?;
                if count > budget || lengths[4] > MAX_BRACKETS {
                    return None;
                }
                (
                    Some(Changes {
                        language: changes.language,
                        scopes: Records::new(changes.scopes, lengths[0]),
                        selections: Records::new(changes.selections, lengths[1]),
                        opaque_starts: Records::new(changes.opaque_starts, lengths[2]),
                        protected: Records::new(changes.protected, lengths[3]),
                        brackets: Records::new(changes.brackets, lengths[4]),
                    }),
                    None,
                    count,
                )
            }
        };
        Some(Self {
            source,
            previous,
            changes,
            output,
            phase: 0,
            index: 0,
            count,
        })
    }
    fn structure(data: StructureData, source: Arc<String>) -> Structure {
        Structure {
            source,
            language: data.language,
            scopes: data.scopes,
            selection_ranges: data.selections,
            opaque_starts: data.opaque_starts,
            protected: data.protected,
            brackets: data.brackets,
            available: true,
        }
    }
    pub fn record_count(&self) -> usize {
        self.count
    }
    pub fn advance(&mut self) -> Option<bool> {
        let mut budget = 64;
        if let Some(changes) = self.changes.as_mut() {
            let old = self.previous?;
            if !changes.scopes.advance(&old.scopes, &mut budget)
                || !changes
                    .selections
                    .advance(&old.selection_ranges, &mut budget)
                || !changes
                    .opaque_starts
                    .advance(&old.opaque_starts, &mut budget)
                || !changes.protected.advance(&old.protected, &mut budget)
                || !changes.brackets.advance(&old.brackets, &mut budget)
            {
                return Some(false);
            }
            let changes = self.changes.take()?;
            self.output = Some(Self::structure(
                StructureData {
                    language: changes.language,
                    scopes: changes.scopes.output,
                    selections: changes.selections.output,
                    opaque_starts: changes.opaque_starts.output,
                    protected: changes.protected.output,
                    brackets: changes.brackets.output,
                },
                self.source.clone(),
            ));
            return Some(false);
        }
        let result = self.output.as_ref()?;
        while self.phase < 6 && budget > 0 {
            let length = match self.phase {
                0 => result.scopes.len(),
                1 => result.selection_ranges.len(),
                2 => result.opaque_starts.len(),
                3 => result.protected.len(),
                _ => result.brackets.len(),
            };
            if self.index == length {
                self.phase += 1;
                self.index = 0;
                continue;
            }
            let valid_range = |range: &Range<usize>| {
                range.start < range.end
                    && result.source.is_char_boundary(range.start)
                    && result.source.is_char_boundary(range.end)
            };
            let valid = match self.phase {
                0 => {
                    let range = &result.scopes[self.index].0;
                    range.start <= range.end
                        && result.source.is_char_boundary(range.start)
                        && result.source.is_char_boundary(range.end)
                        && (self.index == 0 || result.scopes[self.index - 1].0.end <= range.start)
                }
                1 => {
                    let range = &result.selection_ranges[self.index];
                    valid_range(range)
                        && (self.index == 0 || {
                            let old = &result.selection_ranges[self.index - 1];
                            (old.start, old.end) < (range.start, range.end)
                        })
                }
                2 => {
                    let offset = result.opaque_starts[self.index];
                    result.source.is_char_boundary(offset)
                        && (self.index == 0 || result.opaque_starts[self.index - 1] < offset)
                }
                3 => {
                    let range = &result.protected[self.index].0;
                    valid_range(range)
                        && (self.index == 0
                            || result.protected[self.index - 1].0.end <= range.start)
                }
                4 => {
                    self.index == 0
                        || result.brackets[self.index - 1].0 < result.brackets[self.index].0
                }
                _ => Self::valid_bracket(result, self.index),
            };
            if !valid {
                return None;
            }
            self.index += 1;
            budget -= 1;
        }
        Some(self.phase == 6)
    }
    fn valid_bracket(result: &Structure, index: usize) -> bool {
        let (position, ch, partner) = result.brackets[index];
        if !matches!(ch, '(' | ')' | '[' | ']' | '{' | '}')
            || !result
                .source
                .get(position..)
                .is_some_and(|text| text.starts_with(ch))
            || result
                .region_at(position)
                .is_some_and(|(range, _, _)| range.contains(&position))
            || !supports_brackets(result.language_at(position))
        {
            return false;
        }
        let Some(partner) = partner else {
            return true;
        };
        let Ok(index) = result
            .brackets
            .binary_search_by_key(&partner, |bracket| bracket.0)
        else {
            return false;
        };
        let (_, other, back) = result.brackets[index];
        let ordered = if position < partner {
            closing(ch) == Some(other)
        } else {
            closing(other) == Some(ch)
        };
        back == Some(position) && ordered && result.same_language_body(position, partner)
    }
    pub fn finish(self) -> Option<Structure> {
        (self.changes.is_none() && self.phase == 6)
            .then_some(self.output)
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn compare(
        data: StructurePublication,
        source: Arc<String>,
        previous: Option<&Structure>,
        budget: usize,
    ) -> usize {
        let reference = data
            .clone()
            .validate_reference(source.clone(), previous, budget);
        let mut units = 0;
        let actual =
            StructureReception::new(data, source.clone(), previous, budget).and_then(|mut work| {
                loop {
                    units += 1;
                    match work.advance() {
                        Some(false) => {}
                        Some(true) => break work.finish(),
                        None => break None,
                    }
                }
            });
        assert_eq!(actual.is_some(), reference.is_some());
        if let (Some(actual), Some(reference)) = (actual, reference) {
            assert!(Arc::ptr_eq(&actual.source, &source));
            assert_eq!(
                serde_json::to_value(actual.transfer_data()).unwrap(),
                serde_json::to_value(reference.transfer_data()).unwrap()
            );
        }
        units
    }
    #[test]
    fn patched_lists_share_the_record_copy_budget() {
        use std::{cell::Cell, rc::Rc};
        #[derive(PartialEq)]
        struct Counted {
            value: usize,
            clones: Rc<Cell<usize>>,
        }
        impl Clone for Counted {
            fn clone(&self) -> Self {
                self.clones.set(self.clones.get() + 1);
                Self {
                    value: self.value,
                    clones: self.clones.clone(),
                }
            }
        }
        let clones = Rc::new(Cell::new(0));
        let previous = (0..100)
            .map(|value| Counted {
                value,
                clones: clones.clone(),
            })
            .collect::<Vec<_>>();
        let patch = || StructuralRecords::Replace {
            start: 37,
            end: 59,
            items: (100..103)
                .map(|value| Counted {
                    value,
                    clones: clones.clone(),
                })
                .collect(),
        };
        let expected = patch()
            .resolve(&previous)
            .into_iter()
            .map(|record| record.value)
            .collect::<Vec<_>>();
        let mut lists: [Records<Counted>; 5] =
            std::array::from_fn(|_| Records::new(patch(), expected.len()));
        clones.set(0);
        loop {
            let before = lists.iter().map(|list| list.output.len()).sum::<usize>();
            let copied = clones.get();
            let mut budget = 64;
            let complete = lists
                .iter_mut()
                .all(|list| list.advance(&previous, &mut budget));
            let after = lists.iter().map(|list| list.output.len()).sum::<usize>();
            assert!(after - before <= 64);
            assert!(clones.get() - copied <= 64);
            if complete {
                break;
            }
        }
        for list in lists {
            assert_eq!(
                list.output
                    .into_iter()
                    .map(|record| record.value)
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn full_and_patched_structures_match_original_validation() {
        for (language, source) in [
            (
                Language::Rust,
                "fn main() { call(\"文😀\"); }\n".repeat(512),
            ),
            (
                Language::Html,
                "<script>call();</script><style>a { color: red; }</style>\n".repeat(100),
            ),
            (
                Language::Markdown,
                "```rust\nfn main() { call(\"文😀\"); }\n```\n".repeat(100),
            ),
        ] {
            let mut service = crate::editor::SyntaxPreparations::default();
            let old = service
                .prepare("structure".to_string(), language, &source, 4, || true)
                .1
                .unwrap();
            let old = old.structure().unwrap();
            let revised = source.replacen("call", "next", 1);
            let current = service
                .prepare("structure".to_string(), language, &revised, 4, || true)
                .1
                .unwrap();
            let current = current.structure().unwrap();
            let shared = Arc::new(revised.clone());
            let full = StructurePublication::publication(current, None);
            let units = compare(full.clone(), shared.clone(), None, 100_000);
            assert!(units >= current.record_count().div_ceil(64));
            assert!(units > 2);
            let patch = StructurePublication::publication(current, Some(old));
            assert!(patch.needs_base());
            assert!(compare(patch.clone(), shared.clone(), Some(old), 100_000) > 2);
            compare(patch.clone(), shared.clone(), None, 100_000);
            compare(
                full.clone(),
                shared.clone(),
                None,
                current.record_count() - 1,
            );
            compare(
                patch.clone(),
                shared.clone(),
                Some(old),
                current.record_count() - 1,
            );
            compare(patch, shared.clone(), Some(old), current.record_count());
            let mut work = StructureReception::new(full, shared, None, 100_000).unwrap();
            assert_eq!(work.advance(), Some(false));
            assert!(work.finish().is_none());
        }
    }
    #[test]
    fn malformed_coordinates_ordering_and_links_match_original_rejections() {
        let source = Arc::new("😀 fn main() { call(\"文\"); }\n".to_string());
        let mut service = crate::editor::SyntaxPreparations::default();
        let analysis = service
            .prepare("malformed".to_string(), Language::Rust, &source, 4, || true)
            .1
            .unwrap();
        let data = analysis.structure().unwrap().transfer_data();
        let mut cases = vec![data.clone()];
        let mut wrong = data.clone();
        wrong.scopes = vec![(1..4, Language::Rust)];
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.scopes = vec![(4..8, Language::Rust), (7..10, Language::Rust)];
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.selections = std::iter::once(1..4).collect();
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.selections.reverse();
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.opaque_starts = vec![4, 4];
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.opaque_starts = vec![usize::MAX];
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.protected = vec![
            (4..8, false, RegionKind::String),
            (7..10, false, RegionKind::String),
        ];
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.brackets[0].2 = Some(usize::MAX);
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.brackets[0].1 = 'x';
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.brackets.reverse();
        cases.push(wrong);
        let mut wrong = data.clone();
        wrong.brackets = vec![(0, '(', None); MAX_BRACKETS + 1];
        cases.push(wrong);
        for data in cases {
            compare(
                StructurePublication::Full(data),
                source.clone(),
                None,
                100_000,
            );
        }
        let old = analysis.structure().unwrap();
        for (start, end) in [(1, 0), (0, usize::MAX), (usize::MAX, usize::MAX)] {
            let changes = StructureChanges {
                language: old.language,
                scopes: StructuralRecords::Full(vec![]),
                selections: StructuralRecords::Replace {
                    start,
                    end,
                    items: vec![],
                },
                opaque_starts: StructuralRecords::Full(vec![]),
                protected: StructuralRecords::Full(vec![]),
                brackets: StructuralRecords::Full(vec![]),
            };
            compare(
                StructurePublication::Changes { changes },
                source.clone(),
                Some(old),
                100_000,
            );
        }
    }
}
