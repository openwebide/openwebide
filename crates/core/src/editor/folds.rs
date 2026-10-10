//! Logical line ranges shared by parser, indentation and explicit-region providers.

/// The header remains visible; subsequent logical lines through `end_line` fold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FoldRange {
    pub start_line: usize,
    pub end_line: usize,
}

/// Validate already ordered persisted ranges without cloning and sorting them.
/// The ancestor stack retains only end lines, bounded by nesting depth.
pub(super) fn are_normalized(ranges: &[FoldRange], line_count: usize) -> bool {
    let mut ancestors = Vec::new();
    let mut previous_start = None;
    for range in ranges {
        if range.start_line >= range.end_line
            || range.end_line >= line_count
            || previous_start.is_some_and(|start| start >= range.start_line)
        {
            return false;
        }
        previous_start = Some(range.start_line);
        while ancestors.last().is_some_and(|end| *end < range.start_line) {
            ancestors.pop();
        }
        if ancestors.last().is_some_and(|end| range.end_line > *end) {
            return false;
        }
        ancestors.push(range.end_line);
    }
    true
}

/// Keep one control per header and nested/disjoint ranges only.
pub fn normalize_folds(mut ranges: Vec<FoldRange>, line_count: usize) -> Vec<FoldRange> {
    ranges.retain(|range| range.start_line < range.end_line && range.end_line < line_count);
    ranges.sort_unstable_by_key(|range| (range.start_line, std::cmp::Reverse(range.end_line)));
    ranges.dedup_by_key(|range| range.start_line);
    let mut ancestors: Vec<FoldRange> = Vec::new();
    ranges.retain(|range| {
        while ancestors
            .last()
            .is_some_and(|parent| parent.end_line < range.start_line)
        {
            ancestors.pop();
        }
        if ancestors
            .last()
            .is_some_and(|parent| range.end_line > parent.end_line)
        {
            return false;
        }
        ancestors.push(*range);
        true
    });
    ranges
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldCommand {
    Toggle(usize),
    Collapse { recursive: bool },
    Expand { recursive: bool },
    CollapseAll,
    ExpandAll,
    Reveal(usize),
}

/// Collapse state is independent of presentation and retained per source document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FoldState {
    ranges: Vec<FoldRange>,
    collapsed: std::collections::BTreeSet<usize>,
    // Source anchors survive multiple edits while a provider refresh is pending.
    // These ranges are never exposed to the view before the provider validates them.
    pending: Vec<FoldRange>,
    // Presentation survives pending analysis without making obsolete folds actionable.
    indicators: Vec<usize>,
}

pub(super) struct FoldRebase {
    collapsed: Vec<(usize, usize)>,
    indicators: Vec<usize>,
}

impl FoldState {
    pub fn ranges(&self) -> &[FoldRange] {
        &self.ranges
    }
    pub fn indicator_headers(&self) -> impl Iterator<Item = usize> + '_ {
        self.ranges
            .iter()
            .map(|range| range.start_line)
            .chain(self.indicators.iter().copied())
    }
    pub fn indicator_collapsed(&self, header: usize) -> bool {
        self.collapsed.contains(&header)
    }
    pub fn set_ranges(&mut self, ranges: Vec<FoldRange>, line_count: usize) {
        self.ranges = normalize_folds(ranges, line_count);
        self.pending.clear();
        self.indicators.clear();
        self.collapsed.retain(|line| {
            self.ranges
                .binary_search_by_key(line, |range| range.start_line)
                .is_ok()
        });
    }
    pub fn collapsed_at(&self, header: usize) -> Option<FoldRange> {
        if !self.collapsed.contains(&header) {
            return None;
        }
        self.ranges
            .binary_search_by_key(&header, |range| range.start_line)
            .ok()
            .map(|index| self.ranges[index])
    }
    pub fn toggle(&mut self, header: usize) -> bool {
        if self
            .ranges
            .binary_search_by_key(&header, |range| range.start_line)
            .is_err()
        {
            return false;
        }
        if !self.collapsed.remove(&header) {
            self.collapsed.insert(header);
        }
        true
    }
    pub fn collapse_all(&mut self) {
        self.collapsed
            .extend(self.ranges.iter().map(|range| range.start_line));
    }
    pub fn expand_all(&mut self) {
        self.collapsed.clear();
        self.pending.clear();
    }
    pub fn collapse_at(&mut self, line: usize, recursive: bool) -> bool {
        let Some(range) = self
            .ranges
            .iter()
            .rev()
            .find(|range| range.start_line <= line && line <= range.end_line)
            .copied()
        else {
            return false;
        };
        self.collapsed.insert(range.start_line);
        if recursive {
            self.collapsed.extend(
                self.ranges
                    .iter()
                    .filter(|child| {
                        range.start_line <= child.start_line && child.end_line <= range.end_line
                    })
                    .map(|child| child.start_line),
            );
        }
        true
    }
    pub fn expand_at(&mut self, line: usize, recursive: bool) -> bool {
        let candidates = if self.ranges.is_empty() {
            &self.pending
        } else {
            &self.ranges
        };
        let Some(range) = candidates
            .iter()
            .rev()
            .find(|range| {
                (recursive || self.collapsed.contains(&range.start_line))
                    && range.start_line <= line
                    && line <= range.end_line
            })
            .copied()
        else {
            return false;
        };
        self.collapsed.remove(&range.start_line);
        if recursive {
            self.collapsed.retain(|header| {
                !candidates.iter().any(|child| {
                    child.start_line == *header
                        && range.start_line <= child.start_line
                        && child.end_line <= range.end_line
                })
            });
        }
        true
    }
    /// Search and navigation reveal every ancestor hiding the requested line.
    pub fn reveal(&mut self, line: usize) -> bool {
        let before = self.collapsed.len();
        let candidates = if self.ranges.is_empty() {
            &self.pending
        } else {
            &self.ranges
        };
        self.collapsed.retain(|header| {
            !candidates.iter().any(|range| {
                range.start_line == *header && range.start_line < line && line <= range.end_line
            })
        });
        self.collapsed.len() != before
    }
    /// Native input reveals selected source lines before the browser edits them,
    /// including a collapsed header. Disjoint folds remain collapsed.
    pub fn reveal_lines(&mut self, first: usize, last: usize) -> bool {
        let before = self.collapsed.len();
        let candidates = if self.ranges.is_empty() {
            &self.pending
        } else {
            &self.ranges
        };
        self.collapsed.retain(|header| {
            !candidates.iter().any(|range| {
                range.start_line == *header && range.start_line <= last && first <= range.end_line
            })
        });
        self.collapsed.len() != before
    }

    /// Capture unaffected collapsed boundaries before applying known edits.
    pub(super) fn prepare_rebase(
        &self,
        lines: &[super::lines::Line],
        edits: &[super::Edit<&str>],
    ) -> FoldRebase {
        let candidates = if self.ranges.is_empty() {
            &self.pending
        } else {
            &self.ranges
        };
        let mapped = |offset: usize, end: bool| {
            let mut result = offset;
            for edit in edits {
                if edit.range.end <= offset
                    && !(end && edit.range.is_empty() && edit.range.start == offset)
                {
                    result = result - edit.range.len() + edit.text.len();
                }
            }
            result
        };
        let collapsed = self
            .collapsed
            .iter()
            .filter_map(|header| {
                let range = candidates.get(
                    candidates
                        .binary_search_by_key(header, |range| range.start_line)
                        .ok()?,
                )?;
                let first = lines.get(*header)?.start;
                let last = lines.get(range.end_line)?;
                if edits.iter().any(|edit| {
                    if edit.range.is_empty() {
                        first < edit.range.start && edit.range.start < last.end
                    } else {
                        edit.range.start < last.end && first < edit.range.end
                    }
                }) {
                    return None;
                }
                Some((mapped(first, false), mapped(last.body_end, true)))
            })
            .collect();
        let indicators = self
            .indicator_headers()
            .filter_map(|header| {
                let offset = lines.get(header)?.start;
                let mut result = offset;
                for edit in edits {
                    if edit.range.end <= offset {
                        result = result - edit.range.len() + edit.text.len();
                    } else if edit.range.start <= offset {
                        result = result - (offset - edit.range.start) + edit.text.len();
                        break;
                    }
                }
                Some(result)
            })
            .collect();
        FoldRebase {
            collapsed,
            indicators,
        }
    }

    pub(super) fn finish_rebase(&mut self, boundaries: FoldRebase, lines: &[super::lines::Line]) {
        let row = |offset| {
            lines
                .partition_point(|line| line.start <= offset)
                .saturating_sub(1)
        };
        self.indicators = boundaries.indicators.into_iter().map(row).collect();
        self.indicators.sort_unstable();
        self.indicators.dedup();
        self.pending = boundaries
            .collapsed
            .into_iter()
            .map(|(first, last)| FoldRange {
                start_line: row(first),
                end_line: row(last),
            })
            .collect();
        self.collapsed = self.pending.iter().map(|range| range.start_line).collect();
        self.ranges.clear();
    }

    /// Edited folds open; unaffected headers follow line insertions/removals.
    /// Ranges are invalidated until a provider publishes candidates for the new text.
    pub fn rebase(&mut self, old: &str, new: &str) {
        let Some(change) = super::text_change(old, new) else {
            return;
        };
        let edits = [super::Edit {
            text: &new[change.range.start..change.new_end],
            range: change.range,
        }];
        let boundaries = self.prepare_rebase(&super::lines::lines(old), &edits);
        self.finish_rebase(boundaries, &super::lines::lines(new));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn persisted_range_validation_matches_normalization_exhaustively() {
        // Include invalid/reversed ranges, duplicate headers, nesting, crossings,
        // touching endpoints and unsorted input. The existing normalizer is the
        // independent acceptance oracle for persisted metadata.
        for length in 0..=4_u32 {
            for mut encoded in 0..16_usize.pow(length) {
                let mut ranges = Vec::new();
                for _ in 0..length {
                    ranges.push(super::FoldRange {
                        start_line: encoded % 4,
                        end_line: (encoded / 4) % 4,
                    });
                    encoded /= 16;
                }
                for lines in 0..=5 {
                    assert_eq!(
                        super::are_normalized(&ranges, lines),
                        super::normalize_folds(ranges.clone(), lines) == ranges,
                        "{ranges:?}, {lines} lines"
                    );
                }
            }
        }
    }

    use super::*;

    #[test]
    fn indicators_rebase_while_ranges_wait_for_authoritative_analysis() {
        let mut state = FoldState::default();
        state.set_ranges(
            vec![FoldRange {
                start_line: 1,
                end_line: 3,
            }],
            5,
        );
        state.rebase(
            "prefix\nheader\nbody\nclose\nlast",
            "prefix\n\nheader\nbody\nclose\nlast",
        );
        assert!(state.ranges().is_empty());
        assert_eq!(state.indicator_headers().collect::<Vec<_>>(), [2]);
        assert!(!state.toggle(2));
        state.rebase(
            "prefix\n\nheader\nbody\nclose\nlast",
            "header\nbody\nclose\nlast",
        );
        assert_eq!(state.indicator_headers().collect::<Vec<_>>(), [0]);
        state.set_ranges(vec![], 4);
        assert!(state.indicator_headers().next().is_none());
    }
    #[test]
    fn recursive_commands_and_navigation_preserve_unrelated_collapsed_ranges() {
        let ranges = vec![
            FoldRange {
                start_line: 0,
                end_line: 8,
            },
            FoldRange {
                start_line: 2,
                end_line: 5,
            },
            FoldRange {
                start_line: 10,
                end_line: 12,
            },
        ];
        let mut state = FoldState::default();
        state.set_ranges(ranges, 14);
        assert!(state.collapse_at(0, true));
        assert!(state.collapsed_at(2).is_some());
        state.toggle(10);
        state.toggle(0);
        assert!(state.expand_at(0, true));
        assert!(state.collapsed_at(2).is_none());
        assert!(state.collapsed_at(10).is_some());
        state.collapse_all();
        assert!(state.reveal(4));
        assert!(state.collapsed_at(0).is_none());
        assert!(state.collapsed_at(2).is_none());
        assert!(state.collapsed_at(10).is_some());
        assert!(!state.reveal(10));
        assert!(!state.toggle(9));
    }

    #[test]
    fn navigation_during_pending_refresh_cannot_rehide_its_target() {
        let old = "prefix\nheader\nbody\nclose\nlast";
        let new = format!("zero\n{old}");
        let mut state = FoldState::default();
        state.set_ranges(
            vec![FoldRange {
                start_line: 1,
                end_line: 3,
            }],
            5,
        );
        state.collapse_all();
        state.rebase(old, &new);
        assert!(state.ranges().is_empty());
        assert!(state.reveal(3));
        state.set_ranges(
            vec![FoldRange {
                start_line: 2,
                end_line: 4,
            }],
            6,
        );
        assert!(state.collapsed_at(2).is_none());
    }

    #[test]
    fn document_edits_and_grouped_undo_rebase_headers_without_reusing_stale_ranges() {
        use crate::editor::{Document, Edit, Selection};
        let source = "prefix\nheader\nbody\nclose\nlast";
        let mut document = Document::new(source);
        document.fold_state_mut().set_ranges(
            vec![FoldRange {
                start_line: 1,
                end_line: 3,
            }],
            5,
        );
        document.fold_state_mut().collapse_all();
        document
            .apply(
                vec![Edit::replace(0..0, "zero\n")],
                vec![Selection::caret(0)],
                Some(1),
            )
            .unwrap();
        assert_eq!(document.projection().text(), document.text());
        assert!(document.fold_state().ranges().is_empty());
        document
            .apply(
                vec![Edit::replace(0..0, "one\n")],
                vec![Selection::caret(0)],
                Some(1),
            )
            .unwrap();
        document.fold_state_mut().set_ranges(
            vec![FoldRange {
                start_line: 3,
                end_line: 5,
            }],
            7,
        );
        assert!(document.fold_state().collapsed_at(3).is_some());
        assert!(document.undo());
        assert_eq!(document.text(), source);
        document.fold_state_mut().set_ranges(
            vec![FoldRange {
                start_line: 1,
                end_line: 3,
            }],
            5,
        );
        assert!(document.fold_state().collapsed_at(1).is_some());
        assert!(document.redo());
        document.fold_state_mut().set_ranges(
            vec![FoldRange {
                start_line: 3,
                end_line: 5,
            }],
            7,
        );
        assert!(document.fold_state().collapsed_at(3).is_some());
        let body = document.text().find("body").unwrap();
        document
            .apply(
                vec![Edit::replace(body..body + 4, "changed")],
                vec![Selection::caret(body)],
                None,
            )
            .unwrap();
        document.fold_state_mut().set_ranges(
            vec![FoldRange {
                start_line: 3,
                end_line: 5,
            }],
            7,
        );
        assert!(document.fold_state().collapsed_at(3).is_none());
    }

    #[test]
    fn fold_providers_merge_without_crossing_or_duplicate_header_controls() {
        let folds = normalize_folds(
            vec![
                FoldRange {
                    start_line: 0,
                    end_line: 10,
                },
                FoldRange {
                    start_line: 0,
                    end_line: 5,
                },
                FoldRange {
                    start_line: 2,
                    end_line: 4,
                },
                FoldRange {
                    start_line: 3,
                    end_line: 7,
                },
                FoldRange {
                    start_line: 8,
                    end_line: 9,
                },
                FoldRange {
                    start_line: 11,
                    end_line: 12,
                },
                FoldRange {
                    start_line: 12,
                    end_line: 12,
                },
                FoldRange {
                    start_line: 13,
                    end_line: 20,
                },
            ],
            14,
        );
        assert_eq!(
            folds,
            vec![
                FoldRange {
                    start_line: 0,
                    end_line: 10
                },
                FoldRange {
                    start_line: 2,
                    end_line: 4
                },
                FoldRange {
                    start_line: 8,
                    end_line: 9
                },
                FoldRange {
                    start_line: 11,
                    end_line: 12
                },
            ]
        );
    }
}
