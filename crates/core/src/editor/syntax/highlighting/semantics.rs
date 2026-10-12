//! Private span normalization shared by scheduled paint and synchronous consumers.
use crate::editor::{Structure, structure::ordering::MetadataOrder};
use crate::highlight::TokenKind;
use std::ops::Range;

pub(super) const SEMANTIC_BATCH_RECORDS: usize = 1024;

type Spans = Vec<(Range<usize>, TokenKind)>;

pub(super) struct SemanticPreparation {
    values: Spans,
    outer: usize,
    coverage: Vec<(Range<usize>, usize)>,
    order: MetadataOrder,
    phase: Phase,
    read: usize,
    write: usize,
    maximum_end: usize,
    previous_end: Option<usize>,
}
#[derive(Clone, Copy)]
enum Phase {
    Coverage,
    CoverageOrder,
    CoverageEnds,
    Outer,
    Order,
    Protected,
    Complete,
}
impl SemanticPreparation {
    pub(super) fn new(values: Spans, outer: usize) -> Self {
        Self {
            values,
            outer,
            coverage: Vec::new(),
            order: MetadataOrder::new(),
            phase: Phase::Coverage,
            read: 0,
            write: 0,
            maximum_end: 0,
            previous_end: None,
        }
    }
    /// One unit constructs, compares, moves or validates one fixed span record.
    /// Allocation growth remains a separate cost. No partial span list escapes.
    pub(super) fn advance(
        &mut self,
        context: &Structure,
        bodies: usize,
        mut body: impl FnMut(usize) -> Option<Range<usize>>,
        records: usize,
    ) -> Option<bool> {
        for _ in 0..records {
            match self.phase {
                Phase::Coverage => {
                    if self.read < bodies {
                        if let Some(range) = body(self.read) {
                            self.coverage.push((range, 0));
                        }
                        self.read += 1;
                    } else {
                        self.phase = Phase::CoverageOrder;
                        self.read = 0;
                    }
                }
                Phase::CoverageOrder => {
                    if self
                        .order
                        .step(&mut self.coverage, |(r, _)| (r.start, r.end))
                    {
                        self.phase = Phase::CoverageEnds;
                        self.order = MetadataOrder::new();
                    }
                }
                Phase::CoverageEnds => {
                    if let Some((range, end)) = self.coverage.get_mut(self.read) {
                        self.maximum_end = self.maximum_end.max(range.end);
                        *end = self.maximum_end;
                        self.read += 1;
                    } else {
                        self.phase = Phase::Outer;
                        self.read = 0;
                    }
                }
                Phase::Outer => {
                    if let Some((range, _)) = self.values.get(self.read) {
                        // Prefix maximum ends also handle nested/overlapping and
                        // zero-width body records with the original strict edges.
                        let covered = self.read < self.outer
                            && self
                                .coverage
                                .partition_point(|(body, _)| body.start < range.end)
                                .checked_sub(1)
                                .is_some_and(|index| self.coverage[index].1 > range.start);
                        if !covered {
                            self.values.swap(self.read, self.write);
                            self.write += 1;
                        }
                        self.read += 1;
                    } else {
                        self.values.truncate(self.write);
                        self.read = 0;
                        self.write = 0;
                        self.phase = Phase::Order;
                    }
                }
                Phase::Order => {
                    if self.order.step(&mut self.values, |(r, _)| (r.start, r.end)) {
                        self.phase = Phase::Protected;
                    }
                }
                Phase::Protected => {
                    if let Some((range, _)) = self.values.get(self.read) {
                        let covered = context
                            .protected
                            .partition_point(|(r, _, _)| r.start <= range.start)
                            .checked_sub(1)
                            .and_then(|index| context.protected.get(index))
                            .is_some_and(|(protected, _, _)| range.end <= protected.end);
                        if !covered {
                            if self.previous_end.is_some_and(|end| end > range.start) {
                                return None;
                            }
                            self.previous_end = Some(range.end);
                            self.values.swap(self.read, self.write);
                            self.write += 1;
                        }
                        self.read += 1;
                    } else {
                        self.values.truncate(self.write);
                        self.phase = Phase::Complete;
                    }
                }
                Phase::Complete => return Some(true),
            }
        }
        Some(matches!(self.phase, Phase::Complete))
    }
    pub(super) fn finish(self) -> Option<Spans> {
        matches!(self.phase, Phase::Complete).then_some(self.values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::structure::RegionKind;
    use crate::highlight::Language;

    fn reference(
        mut values: Spans,
        outer: usize,
        bodies: &[Option<Range<usize>>],
        context: &Structure,
    ) -> Option<Spans> {
        let mut index = 0;
        values.retain(|(range, _)| {
            let keep = index >= outer
                || !bodies
                    .iter()
                    .flatten()
                    .any(|body| range.start < body.end && range.end > body.start);
            index += 1;
            keep
        });
        values.sort_by_key(|(r, _)| (r.start, r.end));
        values.retain(|(range, _)| {
            context
                .protected
                .partition_point(|(r, _, _)| r.start <= range.start)
                .checked_sub(1)
                .and_then(|i| context.protected.get(i))
                .is_none_or(|(r, _, _)| range.end > r.end)
        });
        (!values
            .windows(2)
            .any(|pair| pair[0].0.end > pair[1].0.start))
        .then_some(values)
    }

    fn drive(
        values: Spans,
        outer: usize,
        bodies: &[Option<Range<usize>>],
        context: &Structure,
        records: usize,
    ) -> Option<Spans> {
        let mut work = SemanticPreparation::new(values, outer);
        let mut turns = 0;
        loop {
            let lookups = std::cell::Cell::new(0);
            let result = work.advance(
                context,
                bodies.len(),
                |index| {
                    lookups.set(lookups.get() + 1);
                    bodies[index].clone()
                },
                records,
            );
            assert!(lookups.get() <= records);
            turns += 1;
            assert!(turns < 2_000_000);
            match result {
                Some(true) => return work.finish(),
                Some(false) => assert!(!matches!(work.phase, Phase::Complete)),
                None => return None,
            }
        }
    }

    #[test]
    fn indexed_coverage_matches_linear_oracle_for_nested_missing_and_empty_bodies() {
        let context = Structure::new(&"x".repeat(128), Language::Plain);
        for bodies in [
            vec![],
            vec![None],
            vec![Some(10..30), Some(20..22), Some(40..60)],
            vec![Some(40..60), None, Some(20..22), Some(10..30)],
            vec![Some(10..10), Some(20..20), Some(30..32)],
        ] {
            for width in [1, 2, 4] {
                let mut values = (0..128)
                    .step_by(width)
                    .map(|start| (start..(start + width).min(128), TokenKind::Keyword))
                    .collect::<Spans>();
                let outer = values.len();
                values.extend([(11..12, TokenKind::Number), (41..42, TokenKind::String)]);
                let expected = reference(values.clone(), outer, &bodies, &context);
                for records in [1, 7, 256] {
                    assert_eq!(
                        drive(values.clone(), outer, &bodies, &context, records),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn reversed_span_order_and_opaque_regions_match_original_normalization() {
        let mut context = Structure::new(&"x".repeat(8192), Language::Plain);
        context.protected = vec![
            (200..500, true, RegionKind::String),
            (2000..2100, true, RegionKind::BlockComment),
        ];
        let values = (0..4096)
            .rev()
            .map(|i| {
                (
                    i * 2..i * 2 + 1,
                    if i % 2 == 0 {
                        TokenKind::Keyword
                    } else {
                        TokenKind::Number
                    },
                )
            })
            .collect::<Spans>();
        let expected = reference(values.clone(), 0, &[], &context).unwrap();
        for records in [1, 7, 256] {
            assert_eq!(
                drive(values.clone(), 0, &[], &context, records).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn overlap_rejection_happens_after_protected_and_injected_filtering() {
        let mut context = Structure::new(&"x".repeat(64), Language::Plain);
        let values = vec![
            (2..10, TokenKind::Keyword),
            (4..8, TokenKind::Number),
            (20..25, TokenKind::String),
        ];
        for protected in [vec![], vec![(0..12, true, RegionKind::String)]] {
            context.protected = protected;
            for bodies in [vec![], vec![Some(1..11)]] {
                let expected = reference(values.clone(), 2, &bodies, &context);
                assert_eq!(drive(values.clone(), 2, &bodies, &context, 1), expected);
            }
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::super::super::SyntaxDocument;
    use crate::editor::SyntaxStatus;
    use crate::highlight::Language;
    use std::sync::Arc;

    #[test]
    fn collection_and_semantic_yields_remain_private_and_cancel_or_replace_exact_source() {
        for (collection, replace) in [(true, false), (true, true), (false, false), (false, true)] {
            let source =
                Arc::new("<script>call(1);</script><style>a { color:red; }</style>\n".repeat(250));
            let mut document = SyntaxDocument::new(Language::Html).unwrap();
            for turn in 0..10_000 {
                let mut checks = 0;
                let result = document.prepare_cooperative(
                    source.clone(),
                    4,
                    || true,
                    || {
                        checks += 1;
                        checks >= 2
                    },
                );
                assert!(result.is_none());
                assert!(document.prepared.is_none());
                if document.paint_progress.as_ref().is_some_and(|work| {
                    if collection {
                        work.collection_pending()
                    } else {
                        work.semantic_pending()
                    }
                }) {
                    break;
                }
                assert!(turn < 9999);
            }
            assert!(document.structure().is_none());
            assert!(document.folds().is_empty());
            if replace {
                let replacement = "fn replacement() { call(); }";
                let (_, analysis) = document.prepare(replacement, 4, || true);
                let analysis = analysis.unwrap();
                assert_eq!(analysis.source_snapshot().as_str(), replacement);
                assert_eq!(
                    analysis.highlights().unwrap()[0]
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>(),
                    replacement
                );
            } else {
                let (status, analysis) = document
                    .prepare_cooperative(source, 4, || false, || false)
                    .unwrap();
                assert_eq!(status, SyntaxStatus::Cancelled);
                assert!(analysis.is_none());
            }
            assert!(document.painting.is_none());
            assert!(document.paint_progress.is_none());
        }
    }
}
