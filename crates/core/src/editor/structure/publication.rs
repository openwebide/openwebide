//! Source-bound preparation of structural wire lists across bounded record batches.
use super::{StructuralRecords, Structure, StructureChanges, StructureData, StructurePublication};

struct Records<T> {
    phase: u8,
    prefix: usize,
    suffix: usize,
    offset: usize,
    output: Vec<T>,
    result: Option<StructuralRecords<T>>,
}
impl<T> Default for Records<T> {
    fn default() -> Self {
        Self {
            phase: 0,
            prefix: 0,
            suffix: 0,
            offset: 0,
            output: Vec::new(),
            result: None,
        }
    }
}
impl<T: Clone + PartialEq> Records<T> {
    fn advance(&mut self, current: &[T], previous: Option<&[T]>) -> bool {
        let old = previous.unwrap_or_default();
        let mut budget = 64;
        while budget > 0 && self.phase < 3 {
            match self.phase {
                0 => {
                    let equal = if previous.is_some() && self.prefix < current.len().min(old.len())
                    {
                        budget -= 1;
                        current[self.prefix] == old[self.prefix]
                    } else {
                        false
                    };
                    if equal {
                        self.prefix += 1;
                    } else {
                        self.phase = 1;
                    }
                }
                1 => {
                    let equal = if previous.is_some()
                        && self.suffix < (current.len() - self.prefix).min(old.len() - self.prefix)
                    {
                        budget -= 1;
                        current[current.len() - self.suffix - 1] == old[old.len() - self.suffix - 1]
                    } else {
                        false
                    };
                    if equal {
                        self.suffix += 1;
                    } else {
                        self.phase = 2;
                        self.offset = if self.prefix + self.suffix > 4 {
                            self.prefix
                        } else {
                            0
                        };
                    }
                }
                _ => {
                    let patch = self.prefix + self.suffix > 4;
                    let limit = current.len() - if patch { self.suffix } else { 0 };
                    let end = self.offset.saturating_add(budget).min(limit);
                    self.output.extend_from_slice(&current[self.offset..end]);
                    budget -= end - self.offset;
                    self.offset = end;
                    if end == limit {
                        self.result = Some(if patch {
                            StructuralRecords::Replace {
                                start: self.prefix,
                                end: old.len() - self.suffix,
                                items: std::mem::take(&mut self.output),
                            }
                        } else {
                            StructuralRecords::Full(std::mem::take(&mut self.output))
                        });
                        self.phase = 3;
                    }
                }
            }
        }
        self.phase == 3
    }
    fn take(&mut self) -> StructuralRecords<T> {
        self.result.take().expect("complete structural records")
    }
}

#[derive(Default)]
pub(in crate::editor) struct StructurePublicationPreparation {
    phase: usize,
    scopes: Records<(std::ops::Range<usize>, crate::highlight::Language)>,
    selections: Records<std::ops::Range<usize>>,
    opaque_starts: Records<usize>,
    protected: Records<(std::ops::Range<usize>, bool, super::RegionKind)>,
    brackets: Records<(usize, char, Option<usize>)>,
}
impl StructurePublicationPreparation {
    pub fn advance(&mut self, current: &Structure, previous: Option<&Structure>) -> bool {
        let previous = previous.filter(|old| old.language == current.language);
        let done = match self.phase {
            0 => self
                .scopes
                .advance(&current.scopes, previous.map(|old| old.scopes.as_slice())),
            1 => self.selections.advance(
                &current.selection_ranges,
                previous.map(|old| old.selection_ranges.as_slice()),
            ),
            2 => self.opaque_starts.advance(
                &current.opaque_starts,
                previous.map(|old| old.opaque_starts.as_slice()),
            ),
            3 => self.protected.advance(
                &current.protected,
                previous.map(|old| old.protected.as_slice()),
            ),
            4 => self.brackets.advance(
                &current.brackets,
                previous.map(|old| old.brackets.as_slice()),
            ),
            _ => return true,
        };
        if done {
            self.phase += 1;
        }
        self.phase == 5
    }
    pub fn finish(mut self, current: &Structure) -> Option<StructurePublication> {
        if self.phase != 5 {
            return None;
        }
        let changes = StructureChanges {
            language: current.language,
            scopes: self.scopes.take(),
            selections: self.selections.take(),
            opaque_starts: self.opaque_starts.take(),
            protected: self.protected.take(),
            brackets: self.brackets.take(),
        };
        if changes.scopes.is_patch()
            || changes.selections.is_patch()
            || changes.opaque_starts.is_patch()
            || changes.protected.is_patch()
            || changes.brackets.is_patch()
        {
            return Some(StructurePublication::Changes { changes });
        }
        fn full<T>(records: StructuralRecords<T>) -> Vec<T> {
            let StructuralRecords::Full(items) = records else {
                unreachable!("patches returned above")
            };
            items
        }
        Some(StructurePublication::Full(StructureData {
            language: changes.language,
            scopes: full(changes.scopes),
            selections: full(changes.selections),
            opaque_starts: full(changes.opaque_starts),
            protected: full(changes.protected),
            brackets: full(changes.brackets),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    #[derive(serde::Serialize)]
    struct Record {
        value: usize,
        #[serde(skip)]
        operations: Rc<Cell<usize>>,
    }
    impl Clone for Record {
        fn clone(&self) -> Self {
            self.operations.set(self.operations.get() + 1);
            Self {
                value: self.value,
                operations: self.operations.clone(),
            }
        }
    }
    impl PartialEq for Record {
        fn eq(&self, other: &Self) -> bool {
            self.operations.set(self.operations.get() + 1);
            self.value == other.value
        }
    }
    #[test]
    fn structural_comparison_and_copy_share_one_fixed_record_budget() {
        let operations = Rc::new(Cell::new(0));
        for (prefix, suffix, changed) in [
            (0, 0, 0),
            (0, 0, 400),
            (2, 2, 1),
            (3, 2, 1),
            (400, 400, 1),
            (800, 0, 0),
        ] {
            let old: Vec<_> = (0..prefix + suffix + changed)
                .map(|value| Record {
                    value,
                    operations: operations.clone(),
                })
                .collect();
            let mut current: Vec<_> = old.iter().take(prefix).cloned().collect();
            current.extend((0..changed).map(|value| Record {
                value: value + 10000,
                operations: operations.clone(),
            }));
            current.extend(old.iter().skip(prefix + changed).cloned());
            for previous in [None, Some(old.as_slice())] {
                let expected = previous.map_or_else(
                    || StructuralRecords::Full(current.clone()),
                    |old| StructuralRecords::publication(&current, old),
                );
                let mut work = Records::default();
                loop {
                    operations.set(0);
                    let complete = work.advance(&current, previous);
                    assert!(operations.get() <= 64, "{} operations", operations.get());
                    if complete {
                        break;
                    }
                }
                assert_eq!(
                    serde_json::to_vec(&work.take()).unwrap(),
                    serde_json::to_vec(&expected).unwrap()
                );
            }
        }
    }
}
