//! Exact source-delta selection and UTF-8 copying across publication batches.
use super::super::SyntaxSource;
use crate::editor::TextChangePreparation;
use std::ops::Range;

/// The caller retains the same immutable current/base sources through completion.
pub(in crate::editor::syntax) struct SourceSelection {
    comparison: TextChangePreparation,
    compare: bool,
    range: Range<usize>,
    replacement: Option<(usize, usize)>,
}
impl SourceSelection {
    pub fn new(length: usize, has_previous: bool) -> Self {
        Self {
            comparison: TextChangePreparation::default(),
            compare: has_previous,
            range: 0..length,
            replacement: None,
        }
    }

    /// Return the byte budget left after selecting the exact source range.
    pub fn advance(&mut self, source: &str, previous: Option<&str>, mut budget: usize) -> usize {
        if self.compare {
            let previous = previous.expect("publication retains its base");
            budget -= self.comparison.advance(previous, source, budget);
            if !self.comparison.is_complete() {
                return budget;
            }
            let (start, end, new_end) = self.comparison.change().map_or((0, 0, 0), |change| {
                (change.range.start, change.range.end, change.new_end)
            });
            // Preserve the existing envelope threshold, including no-op deltas.
            if (new_end - start).saturating_add(96) < source.len() {
                self.range = start..new_end;
                self.replacement = Some((start, end));
            }
            self.compare = false;
        }
        budget
    }
    pub fn is_ready(&self) -> bool {
        !self.compare
    }
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
    pub fn replacement(&self) -> Option<(usize, usize)> {
        self.replacement
    }
}

pub(in crate::editor::syntax) struct SourcePublication {
    selection: SourceSelection,
    offset: usize,
    output: String,
    complete: bool,
}
impl SourcePublication {
    pub fn new(length: usize, has_previous: bool) -> Self {
        Self {
            selection: SourceSelection::new(length, has_previous),
            offset: 0,
            output: String::new(),
            complete: false,
        }
    }
    /// At least four bytes guarantee UTF-8 copy progress. Capacity allocation
    /// remains a runtime primitive, while comparisons and copies charge bytes.
    pub fn advance(&mut self, source: &str, previous: Option<&str>, budget: usize) -> bool {
        if self.complete {
            return true;
        }
        let budget = self.selection.advance(source, previous, budget);
        if !self.selection.is_ready() {
            return false;
        }
        let part = &source[self.selection.range()];
        let end = part.floor_char_boundary(part.len().min(self.offset.saturating_add(budget)));
        self.output.push_str(&part[self.offset..end]);
        self.offset = end;
        self.complete = end == part.len();
        self.complete
    }

    pub fn finish(self) -> Option<SyntaxSource> {
        self.complete.then_some(match self.selection.replacement() {
            Some((start, end)) => SyntaxSource::Replace {
                start,
                end,
                text: self.output,
            },
            None => SyntaxSource::Full(self.output),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::text_change;

    fn reference(source: &str, previous: Option<&str>) -> SyntaxSource {
        if let Some(previous) = previous {
            let (start, end, text) = text_change(previous, source).map_or((0, 0, ""), |change| {
                (
                    change.range.start,
                    change.range.end,
                    &source[change.range.start..change.new_end],
                )
            });
            if text.len().saturating_add(96) < source.len() {
                return SyntaxSource::Replace {
                    start,
                    end,
                    text: text.into(),
                };
            }
        }
        SyntaxSource::Full(source.into())
    }
    #[test]
    fn publication_batches_match_full_and_delta_policy_at_utf8_boundaries() {
        for ending in ["\n", "\r\n"] {
            let old = format!(
                "header{ending}{}{ending}tail",
                "文😀 e\u{301} ".repeat(20_000)
            );
            let changed = [
                old.clone(),
                old.replacen("header", "new😀", 1),
                old.replace("tail", "文"),
                old.replace("文", "言"),
                String::new(),
                "small😀".into(),
            ];
            for source in &changed {
                for previous in [None, Some(old.as_str()), Some("")] {
                    let expected = serde_json::to_value(reference(source, previous)).unwrap();
                    for budget in [4, 7, 8192] {
                        let mut work = SourcePublication::new(source.len(), previous.is_some());
                        if !source.is_empty() {
                            assert!(!work.advance(source, previous, 0));
                        }
                        let mut turns = 0;
                        while !work.advance(source, previous, budget) {
                            turns += 1;
                            assert!(turns < 200_000);
                        }
                        assert_eq!(
                            serde_json::to_value(work.finish().unwrap()).unwrap(),
                            expected
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn unfinished_source_cannot_publish() {
        let mut work = SourcePublication::new(1000, false);
        assert!(!work.advance(&"x".repeat(1000), None, 4));
        assert!(work.finish().is_none());
    }
}
