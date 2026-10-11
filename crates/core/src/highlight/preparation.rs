//! Cooperative plain-row preparation; source and publication policy stay shared.
use super::{LexicalPreparation, LexicalRow, Token, plain_token_row};
use std::sync::Arc;

pub(super) struct LexicalRowPreparation {
    scan: usize,
    extent: Option<(usize, bool)>,
    candidates: [Option<usize>; 2],
    validated: usize,
    copied: usize,
    text: Option<String>,
}

impl LexicalPreparation {
    /// Scan, validate and copy within a byte budget, publishing only complete
    /// rows. Zero returned rows can still mean progress within a long row.
    /// Tiny budgets admit one UTF-8 scalar or candidate comparison pair.
    pub fn advance_bounded(&mut self, max_rows: usize, max_bytes: usize) -> usize {
        if max_rows == 0 || max_bytes == 0 {
            return 0;
        }
        let mut completed = 0;
        let mut remaining = max_bytes.saturating_sub(self.advance_source_comparison(max_bytes));
        if self.pending_change.is_some() {
            return 0;
        }
        while !self.complete && completed < max_rows && remaining > 0 {
            if self.pending_row.is_none() {
                if let Some((index, next, newline)) = self.indexed_row() {
                    let tokens = self.previous.as_ref().unwrap().tokens[index].clone();
                    self.publish_bounded_row(next, newline, tokens, Some(index));
                    completed += 1;
                    remaining -= 1;
                    continue;
                }
                #[cfg(test)]
                {
                    self.boundary_scans += 1;
                }
            }
            let mut row = self.pending_row.take().unwrap_or(LexicalRowPreparation {
                scan: self.next,
                extent: None,
                candidates: [None; 2],
                validated: self.next,
                copied: self.next,
                text: None,
            });
            if row.extent.is_none() {
                let end = self.source.len().min(row.scan.saturating_add(remaining));
                let newline = self.source.as_bytes()[row.scan..end]
                    .iter()
                    .position(|byte| *byte == b'\n');
                let next = newline.map_or(end, |at| row.scan + at + 1);
                remaining -= next - row.scan;
                row.scan = next;
                if newline.is_some() || next == self.source.len() {
                    row.extent = Some((next, newline.is_some()));
                    row.candidates = self.bounded_reuse_candidates(next);
                }
            }
            if let Some((next, newline)) = row.extent {
                let candidates = row.candidates.iter().flatten().count();
                if candidates > 0 && row.validated < next && remaining > 0 {
                    let cost = (remaining / candidates).max(1).min(next - row.validated);
                    let previous = self.previous.as_ref().unwrap();
                    let offset = row.validated - self.next;
                    for candidate in &mut row.candidates {
                        if let Some(index) = *candidate {
                            let start = previous.rows[index].start + offset;
                            if previous.source.as_bytes()[start..start + cost]
                                != self.source.as_bytes()[row.validated..row.validated + cost]
                            {
                                *candidate = None;
                            }
                        }
                    }
                    row.validated += cost;
                    remaining = remaining.saturating_sub(cost.saturating_mul(candidates));
                }
                if row.validated == next
                    && let Some(index) = row.candidates.into_iter().flatten().next()
                {
                    let tokens = self.previous.as_ref().unwrap().tokens[index].clone();
                    self.publish_bounded_row(next, newline, tokens, Some(index));
                    completed += 1;
                    continue;
                }
                if row.candidates.iter().all(Option::is_none) {
                    let mut end = next - usize::from(newline);
                    if self.normalize_crlf
                        && newline
                        && self.source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r')
                    {
                        end -= 1;
                    }
                    let text = row
                        .text
                        .get_or_insert_with(|| String::with_capacity(end - self.next));
                    if remaining > 0 {
                        let mut to = end.min(row.copied.saturating_add(remaining));
                        while !self.source.is_char_boundary(to) {
                            to += 1;
                        }
                        text.push_str(&self.source[row.copied..to]);
                        remaining = remaining.saturating_sub(to - row.copied);
                        row.copied = to;
                    }
                    if row.copied == end {
                        let tokens = plain_token_row(row.text.take().unwrap());
                        self.retokenized_rows += 1;
                        self.publish_bounded_row(next, newline, tokens, None);
                        completed += 1;
                        continue;
                    }
                }
            }
            self.pending_row = Some(row);
        }
        completed
    }

    /// Compare raw bytes cooperatively; round only the finished boundaries to
    /// valid UTF-8 positions before exposing them to indexed row reuse.
    pub(super) fn advance_source_comparison(&mut self, budget: usize) -> usize {
        let Some(mut comparison) = self.pending_change.take() else {
            return 0;
        };
        let old = &self
            .previous
            .as_ref()
            .expect("retained comparison source")
            .source;
        let new = &self.source;
        let cost = comparison.advance(old, new, budget);
        if comparison.is_complete() {
            self.source_change = comparison.change().cloned();
            self.unchanged = self.source_change.is_none();
            self.complete = self.unchanged;
        } else {
            self.pending_change = Some(comparison);
        }
        cost
    }

    fn bounded_reuse_candidates(&self, end: usize) -> [Option<usize>; 2] {
        let Some(previous) = &self.previous else {
            return [None; 2];
        };
        let shifted = if self.source.len() >= previous.source.len() {
            self.next
                .checked_sub(self.source.len() - previous.source.len())
        } else {
            self.next
                .checked_add(previous.source.len() - self.source.len())
        };
        [Some(self.next), shifted].map(|start| {
            let index = previous
                .rows
                .binary_search_by_key(&start?, |row| row.start)
                .ok()?;
            let row = &previous.rows[index];
            (row.end - row.start == end - self.next).then_some(index)
        })
    }

    fn publish_bounded_row(
        &mut self,
        next: usize,
        newline: bool,
        tokens: Arc<[Token]>,
        reused: Option<usize>,
    ) {
        if let Some(index) = reused {
            self.previous_row = index + 1;
        }
        self.contexts.push(LexicalRow {
            start: self.next,
            end: next,
        });
        self.rows.push(tokens);
        self.next = next;
        self.complete = !newline;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::{Language, highlight_lines, share_token_rows};

    #[cfg(feature = "editor-parser")]
    #[test]
    fn validated_replacements_reuse_only_the_exact_compatible_base() {
        let source = Arc::new(format!(
            "header\r\n{}\r\ntail",
            "文😀e\u{301} ".repeat(20_000)
        ));
        for normalize in [false, true] {
            let make = |source| {
                if normalize {
                    LexicalPreparation::for_textarea(source, Language::Sql)
                } else {
                    LexicalPreparation::new(source, Language::Sql)
                }
            };
            let mut prior = make(source.clone());
            while !prior.is_complete() {
                prior.advance(128, usize::MAX);
            }
            let prior = Arc::new(prior.finish_snapshot().unwrap());
            let revised = Arc::new(source.replacen("header", "revised", 1));
            let change = crate::editor::text_change(&source, &revised).unwrap();
            for trusted in [false, true] {
                let base = if trusted {
                    source.clone()
                } else {
                    Arc::new(source.as_ref().clone())
                };
                let mut job =
                    make(revised.clone()).reuse_validated_change(prior.clone(), &base, &change);
                assert_eq!(job.pending_change.is_some(), !trusted);
                while !job.is_complete() {
                    job.advance_bounded(128, 4096);
                }
                let snapshot = job.finish_snapshot().unwrap();
                assert!(Arc::ptr_eq(&snapshot.source, &revised));
                assert_eq!(snapshot.retokenized_rows(), 1);
                assert!(Arc::ptr_eq(&snapshot.tokens()[1], &prior.tokens()[1]));
                let expected = if normalize {
                    revised.replace("\r\n", "\n")
                } else {
                    revised.to_string()
                };
                assert_eq!(
                    snapshot.tokens().as_ref(),
                    &share_token_rows(highlight_lines(&expected, Language::Sql))
                );
            }
            let equal = Arc::new(source.as_ref().clone());
            let job = make(equal.clone()).reuse_validated_change(
                prior.clone(),
                &source,
                &crate::editor::TextChange {
                    range: 0..0,
                    new_end: 0,
                },
            );
            assert!(job.is_complete());
            let equal = job.finish_snapshot().unwrap();
            assert!(Arc::ptr_eq(equal.tokens(), prior.tokens()));
            assert!(Arc::ptr_eq(&equal.rows, &prior.rows));
            let mut incompatible = LexicalPreparation::new(revised.clone(), Language::Plain)
                .reuse_validated_change(prior, &source, &change);
            assert!(incompatible.previous.is_none());
            while !incompatible.is_complete() {
                incompatible.advance_bounded(128, 4096);
            }
            assert_eq!(
                incompatible.finish().unwrap(),
                share_token_rows(highlight_lines(&revised, Language::Plain))
            );
        }
    }

    #[test]
    fn cooperative_source_comparison_matches_exact_unicode_changes() {
        let variants = [
            "",
            "a",
            "ab",
            "文😀e\u{301}",
            "文😃e\u{301}",
            "文😀\r\n",
            "\n文😀",
            "文😀tail",
        ];
        for old in variants {
            let mut prior = LexicalPreparation::new(Arc::new(old.into()), Language::Sql);
            while !prior.is_complete() {
                prior.advance(128, usize::MAX);
            }
            let prior = Arc::new(prior.finish_snapshot().unwrap());
            for new in variants {
                for budget in [1, 2, 7, 64] {
                    let source = Arc::new(new.to_string());
                    let mut job = LexicalPreparation::new(source.clone(), Language::Sql)
                        .reuse_cooperative(prior.clone());
                    assert!(!job.is_complete());
                    assert!(job.pending_change.is_some());
                    let mut turns = 0;
                    while job.pending_change.is_some() {
                        assert!(job.advance_source_comparison(budget) <= budget);
                        turns += 1;
                        assert!(turns < 100);
                    }
                    assert_eq!(job.source_change, crate::editor::text_change(old, new));
                    while !job.is_complete() {
                        job.advance_bounded(2, budget);
                    }
                    let snapshot = job.finish_snapshot().unwrap();
                    assert!(Arc::ptr_eq(&source, &snapshot.source));
                    assert_eq!(
                        snapshot.tokens().as_ref(),
                        &share_token_rows(highlight_lines(new, Language::Sql))
                    );
                    if old == new {
                        assert!(Arc::ptr_eq(snapshot.tokens(), prior.tokens()));
                        assert!(Arc::ptr_eq(&snapshot.rows, &prior.rows));
                    }
                }
            }
        }
    }

    #[test]
    fn long_source_comparison_yields_before_row_publication_and_can_restart() {
        let source = Arc::new(format!(
            "header\r\n{}\r\nfooter",
            "文😀e\u{301} ".repeat(40_000)
        ));
        let mut prior = LexicalPreparation::for_textarea(source.clone(), Language::Sql);
        while !prior.is_complete() {
            prior.advance(128, usize::MAX);
        }
        let prior = Arc::new(prior.finish_snapshot().unwrap());
        let current = Arc::new(source.replace("header", "changed"));
        let mut job = LexicalPreparation::for_textarea(current.clone(), Language::Sql)
            .reuse_cooperative(prior.clone());
        assert_eq!(job.advance_bounded(128, 4096), 0);
        assert!(job.pending_change.is_some());
        assert!(job.rows.is_empty());
        assert!(!job.is_complete());
        assert!(
            LexicalPreparation::for_textarea(current.clone(), Language::Sql)
                .reuse_cooperative(prior.clone())
                .finish_snapshot()
                .is_none()
        );
        while !job.is_complete() {
            job.advance_bounded(128, 4096);
        }
        let snapshot = job.finish_snapshot().unwrap();
        assert_eq!(snapshot.retokenized_rows(), 1);
        assert!(Arc::ptr_eq(&snapshot.tokens()[1], &prior.tokens()[1]));
        let reset = LexicalPreparation::for_textarea(current, Language::Sql)
            .reuse_cooperative(prior.clone())
            .reuse_cooperative(prior.clone());
        assert!(reset.pending_change.is_some());
        let reset = reset.reuse_cooperative(Arc::new(snapshot));
        assert!(reset.is_complete());
        assert!(reset.pending_change.is_none());
    }

    #[test]
    fn bounded_long_rows_preserve_unicode_newlines_and_complete_publication() {
        let source = Arc::new(format!(
            "{}\r\n\n{}\nend\r",
            "文😀e\u{301}\t words ".repeat(10_000),
            "second 文😀 ".repeat(8_000)
        ));
        for normalize in [false, true] {
            for budget in [7, 4096, 64 * 1024] {
                let mut job = if normalize {
                    LexicalPreparation::for_textarea(source.clone(), Language::Sql)
                } else {
                    LexicalPreparation::new(source.clone(), Language::Sql)
                };
                assert_eq!(job.advance_bounded(0, budget), 0);
                assert_eq!(job.advance_bounded(1, 0), 0);
                assert_eq!(job.advance_bounded(1, budget), 0);
                assert!(
                    !job.is_complete(),
                    "a long row must retain cooperative work"
                );
                let mut turns = 1;
                while !job.is_complete() {
                    job.advance_bounded(2, budget);
                    turns += 1;
                    assert!(turns < 200_000);
                }
                let snapshot = job.finish_snapshot().unwrap();
                let expected = if normalize {
                    source.replace("\r\n", "\n")
                } else {
                    source.to_string()
                };
                assert!(Arc::ptr_eq(&source, &snapshot.source));
                assert_eq!(
                    snapshot.tokens().as_ref(),
                    &share_token_rows(highlight_lines(&expected, Language::Sql))
                );
            }
        }
        let mut interrupted = LexicalPreparation::new(source, Language::Sql);
        interrupted.advance_bounded(1, 4096);
        assert!(interrupted.finish_snapshot().is_none());
    }

    #[test]
    fn bounded_validation_reuses_long_rows_inside_disjoint_edits() {
        let body = "文😀e\u{301}\t words ".repeat(10_000);
        let mut old = LexicalPreparation::new(Arc::new(format!("a\n{body}\nz")), Language::Sql);
        while !old.is_complete() {
            old.advance(128, usize::MAX);
        }
        let old = Arc::new(old.finish_snapshot().unwrap());
        let mut job = LexicalPreparation::new(Arc::new(format!("b\n{body}\ny")), Language::Sql)
            .reuse(old.clone());
        assert_eq!(job.advance_bounded(1, 4096), 1);
        assert_eq!(job.advance_bounded(1, 4096), 0);
        assert!(!job.is_complete());
        while !job.is_complete() {
            job.advance_bounded(1, 4096);
        }
        let next = job.finish_snapshot().unwrap();
        assert_eq!(next.retokenized_rows(), 2);
        assert!(Arc::ptr_eq(&old.tokens()[1], &next.tokens()[1]));
        assert_eq!(next.tokens()[0][0].text, "b");
        assert_eq!(next.tokens()[2][0].text, "y");
    }

    #[test]
    fn synchronous_driver_can_finish_a_partially_prepared_row() {
        let source = Arc::new(format!("{}\r\ntail\n", "文😀e\u{301}".repeat(10_000)));
        let mut job = LexicalPreparation::for_textarea(source.clone(), Language::Plain);
        assert_eq!(job.advance_bounded(1, 31), 0);
        assert_eq!(job.advance(1, 31), 1);
        while !job.is_complete() {
            job.advance(128, usize::MAX);
        }
        assert_eq!(
            job.finish().unwrap(),
            share_token_rows(highlight_lines(
                &source.replace("\r\n", "\n"),
                Language::Plain
            ))
        );
    }
}
