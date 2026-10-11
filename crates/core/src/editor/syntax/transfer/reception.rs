//! Bounded source proof before receiver reconstruction can use source coordinates.
use super::{MAX_STRUCTURE_BYTES, SyntaxSource};
use std::sync::Arc;

const SOURCE_BATCH_BYTES: usize = 8 * 1024;

pub(in crate::editor::syntax) struct RecordAdmission {
    count: usize,
    index: usize,
}
impl RecordAdmission {
    pub fn new(data: &super::SyntaxAnalysisData) -> Option<Self> {
        let count = data
            .folds
            .len()
            .checked_add(
                data.structure
                    .as_ref()
                    .map_or(0, super::StructurePublication::record_count),
            )?
            .checked_add(data.highlights.as_ref().map_or(0, Vec::len))?;
        (count <= super::MAX_ANALYSIS_RECORDS).then_some(Self { count, index: 0 })
    }
    pub fn advance(&mut self, data: &super::SyntaxAnalysisData) -> Option<bool> {
        let rows = data.highlights.as_deref().unwrap_or(&[]);
        let end = rows.len().min(self.index + 64);
        while self.index < end {
            self.count = self.count.checked_add(rows[self.index].records())?;
            if self.count > super::MAX_ANALYSIS_RECORDS {
                return None;
            }
            self.index += 1;
        }
        Some(self.index == rows.len())
    }
}

pub(in crate::editor::syntax) struct SourceValidation {
    wire: SyntaxSource,
    expected: Arc<String>,
    previous: Option<Arc<String>>,
    phase: usize,
    offset: usize,
    written: usize,
    line_count: usize,
    valid: bool,
}

impl SourceValidation {
    pub fn new(
        wire: SyntaxSource,
        expected: Arc<String>,
        previous: Option<Arc<String>>,
    ) -> Option<Self> {
        if expected.len() > MAX_STRUCTURE_BYTES
            || wire.result_length(previous.as_deref().map(String::as_str))? != expected.len()
        {
            return None;
        }
        Some(Self {
            wire,
            expected,
            previous,
            phase: 0,
            offset: 0,
            written: 0,
            line_count: 1,
            valid: true,
        })
    }

    /// Compare and count logical lines in at most 8 KiB per unit. Byte slices
    /// deliberately permit a chunk boundary inside a UTF-8 scalar: no string
    /// coordinate or partial source is exposed by this proof.
    pub fn advance(&mut self) -> bool {
        if !self.valid || self.phase == 3 {
            return true;
        }
        let mut remaining = SOURCE_BATCH_BYTES;
        while self.phase < 3 {
            let part = match &self.wire {
                SyntaxSource::Full(source) => {
                    if self.phase > 0 {
                        self.phase = 3;
                        break;
                    }
                    source.as_bytes()
                }
                SyntaxSource::Replace { start, end, text } => match self.phase {
                    0 => &self.previous.as_ref().expect("validated base").as_bytes()[..*start],
                    1 => text.as_bytes(),
                    _ => &self.previous.as_ref().expect("validated base").as_bytes()[*end..],
                },
            };
            let end = part.len().min(self.offset + remaining);
            let bytes = &part[self.offset..end];
            let next = self.written + bytes.len();
            if self.expected.as_bytes().get(self.written..next) != Some(bytes) {
                self.valid = false;
                return true;
            }
            self.line_count += bytes.iter().filter(|&&byte| byte == b'\n').count();
            remaining -= bytes.len();
            self.written = next;
            self.offset = end;
            if end == part.len() {
                self.phase += 1;
                self.offset = 0;
            } else {
                return false;
            }
            if remaining == 0 {
                return self.phase == 3;
            }
        }
        true
    }

    pub fn finish(self) -> Option<(Arc<String>, usize)> {
        (self.valid && self.phase == 3).then_some((self.expected, self.line_count))
    }

    pub fn completed_lines(&self) -> Option<usize> {
        (self.valid && self.phase == 3).then_some(self.line_count)
    }
}

pub(in crate::editor::syntax) struct AnalysisReconstruction<'a> {
    pub(super) source: Arc<String>,
    pub(super) folds: Vec<crate::editor::FoldRange>,
    pub(super) structure: Option<Arc<crate::editor::Structure>>,
    pub(super) structural: Option<super::StructureReception<'a>>,
    pub(super) rows: Option<TokenReconstruction>,
}
impl AnalysisReconstruction<'_> {
    pub fn advance(&mut self) -> Option<bool> {
        if let Some(structural) = self.structural.as_mut() {
            if !structural.advance()? {
                return Some(false);
            }
            self.structure = Some(Arc::new(self.structural.take()?.finish()?));
            return Some(false);
        }
        self.rows
            .as_mut()
            .map_or(Some(true), TokenReconstruction::advance)
    }
    pub fn finish(self) -> Option<Arc<super::SyntaxAnalysis>> {
        if self.structural.is_some() {
            return None;
        }
        let highlights = match self.rows {
            Some(rows) => Some(Arc::new(rows.finish()?)),
            None => None,
        };
        Some(Arc::new(super::SyntaxAnalysis {
            source: self.source,
            folds: self.folds,
            structure: self.structure,
            highlights,
        }))
    }
}

pub(super) struct TokenReconstruction {
    data: Vec<super::TokenRowData>,
    declared_index: usize,
    declared_rows: usize,
    input: Option<std::vec::IntoIter<super::TokenRowData>>,
    source: Arc<String>,
    previous: Option<Arc<crate::highlight::TokenRows>>,
    line_count: usize,
    records: usize,
    output: crate::highlight::TokenRows,
    reuse: Option<std::ops::Range<usize>>,
    row: Option<ReceiveRow>,
    line_start: usize,
    scan: usize,
    line_end: Option<usize>,
    complete: bool,
}
struct ReceiveRow {
    spans: std::vec::IntoIter<(usize, crate::highlight::TokenKind)>,
    retained: Option<crate::highlight::TokenRow>,
    index: usize,
    start: usize,
    pending: Option<ReceiveToken>,
    output: Vec<crate::highlight::Token>,
}
struct ReceiveToken {
    end: usize,
    kind: crate::highlight::TokenKind,
    offset: usize,
    text: String,
}
impl ReceiveRow {
    fn spans(spans: Vec<(usize, crate::highlight::TokenKind)>) -> Self {
        Self {
            spans: spans.into_iter(),
            retained: None,
            index: 0,
            start: 0,
            pending: None,
            output: Vec::new(),
        }
    }
    fn retained(row: crate::highlight::TokenRow) -> Self {
        Self {
            spans: Vec::new().into_iter(),
            retained: Some(row),
            index: 0,
            start: 0,
            pending: None,
            output: Vec::new(),
        }
    }
    fn advance(&mut self, line: &str, records: &mut usize, bytes: &mut usize) -> Option<bool> {
        while *records > 0 && *bytes > 0 {
            *records -= 1;
            if self.pending.is_none() {
                let token = match &self.retained {
                    Some(row) => match row.get(self.index) {
                        Some(token) => {
                            Some((self.start.checked_add(token.text.len())?, token.kind))
                        }
                        None => None,
                    },
                    None => self.spans.next(),
                };
                let Some((end, kind)) = token else {
                    return Some(self.start == line.len()).filter(|valid| *valid);
                };
                if end < self.start
                    || !line.is_char_boundary(end)
                    || (self.retained.is_none() && end == self.start && !line.is_empty())
                {
                    return None;
                }
                self.pending = Some(ReceiveToken {
                    end,
                    kind,
                    offset: self.start,
                    text: String::new(),
                });
            }
            let token = self.pending.as_mut()?;
            let end = if self.retained.is_some() {
                token.end.min(token.offset + *bytes)
            } else {
                line.floor_char_boundary(token.end.min(token.offset + *bytes))
            };
            if end == token.offset && end < token.end {
                return Some(false);
            }
            let length = end - token.offset;
            if let Some(row) = &self.retained {
                let old = row.get(self.index)?;
                let old_start = token.offset - self.start;
                if line.as_bytes().get(token.offset..end)
                    != old.text.as_bytes().get(old_start..old_start + length)
                {
                    return None;
                }
            } else {
                token.text.push_str(&line[token.offset..end]);
            }
            *bytes -= length;
            token.offset = end;
            if end == token.end {
                let token = self.pending.take()?;
                self.start = token.end;
                self.index += 1;
                if self.retained.is_none() {
                    self.output.push(crate::highlight::Token {
                        kind: token.kind,
                        text: token.text,
                    });
                }
            } else {
                return Some(false);
            }
        }
        Some(false)
    }
    fn finish(self) -> crate::highlight::TokenRow {
        self.retained
            .unwrap_or_else(|| crate::highlight::share_token_row(self.output))
    }
}
impl TokenReconstruction {
    pub fn new(
        data: Vec<super::TokenRowData>,
        source: Arc<String>,
        line_count: usize,
        records: usize,
        previous: Option<Arc<crate::highlight::TokenRows>>,
    ) -> Self {
        Self {
            data,
            declared_index: 0,
            declared_rows: 0,
            input: None,
            source,
            previous,
            line_count,
            records,
            output: Vec::new(),
            reuse: None,
            row: None,
            line_start: 0,
            scan: 0,
            line_end: None,
            complete: false,
        }
    }
    pub fn advance(&mut self) -> Option<bool> {
        if self.complete {
            return Some(true);
        }
        let mut records = 64;
        let mut bytes = SOURCE_BATCH_BYTES;
        if self.input.is_none() {
            while self.declared_index < self.data.len() && records > 0 {
                self.declared_rows = self
                    .declared_rows
                    .checked_add(self.data[self.declared_index].row_count())?;
                self.declared_index += 1;
                records -= 1;
            }
            if self.declared_index < self.data.len() {
                return Some(false);
            }
            if self.declared_rows != self.line_count
                || self.declared_rows > super::MAX_ANALYSIS_RECORDS
            {
                return None;
            }
            self.output = Vec::with_capacity(self.line_count);
            self.input = Some(std::mem::take(&mut self.data).into_iter());
        }
        while records > 0 && bytes > 0 {
            if self.row.is_none() {
                records -= 1;
                if self.reuse.as_ref().is_some_and(std::ops::Range::is_empty) {
                    self.reuse = None;
                }
                let row = if let Some(range) = self.reuse.as_mut() {
                    let retained = self.previous.as_ref()?.get(range.start)?.clone();
                    range.start += 1;
                    self.records = self.records.checked_add(retained.len())?.checked_add(1)?;
                    ReceiveRow::retained(retained)
                } else {
                    match self.input.as_mut()?.next() {
                        Some(super::TokenRowData::Spans(spans)) => {
                            self.records = self.records.checked_add(spans.len())?.checked_add(1)?;
                            ReceiveRow::spans(spans)
                        }
                        Some(super::TokenRowData::Previous { reuse, count }) => {
                            if count == 0 {
                                return None;
                            }
                            let end = reuse.checked_add(count)?;
                            self.previous.as_ref()?.get(reuse..end)?;
                            self.reuse = Some(reuse..end);
                            continue;
                        }
                        None => {
                            if self.output.len() != self.line_count
                                || self.line_start <= self.source.len()
                            {
                                return None;
                            }
                            self.complete = true;
                            return Some(true);
                        }
                    }
                };
                if self.records > super::MAX_ANALYSIS_RECORDS || self.line_start > self.source.len()
                {
                    return None;
                }
                self.row = Some(row);
            }
            if self.line_end.is_none() {
                let end = self.source.len().min(self.scan + bytes);
                let part = &self.source.as_bytes()[self.scan..end];
                if let Some(newline) = part.iter().position(|&byte| byte == b'\n') {
                    bytes -= newline + 1;
                    self.scan += newline;
                    self.line_end = Some(self.scan);
                } else {
                    bytes -= part.len();
                    self.scan = end;
                    if end == self.source.len() {
                        self.line_end = Some(end);
                    } else {
                        return Some(false);
                    }
                }
            }
            let end = self.line_end?;
            let line = self.source.get(self.line_start..end)?;
            if !self.row.as_mut()?.advance(line, &mut records, &mut bytes)? {
                return Some(false);
            }
            self.output.push(self.row.take()?.finish());
            self.line_start = end + 1;
            self.scan = self.line_start;
            self.line_end = None;
        }
        Some(false)
    }
    pub fn finish(self) -> Option<crate::highlight::TokenRows> {
        self.complete.then_some(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstructed_blank_rows_share_tokens_without_replacing_retained_owners() {
        use super::super::TokenRowData;
        use crate::highlight::{Token, TokenKind};
        for plain in [false, true] {
            let count = 49_001;
            let source = Arc::new("\n".repeat(count - 1));
            let spans = if plain {
                vec![(0, TokenKind::Plain)]
            } else {
                vec![]
            };
            let mut work = TokenReconstruction::new(
                vec![TokenRowData::Spans(spans); count],
                source,
                count,
                0,
                None,
            );
            while !work.advance().unwrap() {}
            let rows = work.finish().unwrap();
            assert_eq!(rows.len(), count);
            assert!(rows.iter().all(|row| Arc::ptr_eq(row, &rows[0])));
            assert_eq!(rows[0].len(), usize::from(plain));
            let old: crate::highlight::TokenRow = Arc::from(vec![Token {
                kind: TokenKind::Plain,
                text: String::new(),
            }]);
            let mut retained = TokenReconstruction::new(
                vec![TokenRowData::Previous { reuse: 0, count: 1 }],
                Arc::new(String::new()),
                1,
                0,
                Some(Arc::new(vec![old.clone()])),
            );
            while !retained.advance().unwrap() {}
            assert!(Arc::ptr_eq(&retained.finish().unwrap()[0], &old));
        }
    }

    #[test]
    fn record_admission_matches_original_limits_and_bounds_dense_headers() {
        use super::super::{MAX_ANALYSIS_RECORDS, SyntaxAnalysisData, TokenRowData};
        use crate::highlight::TokenKind;
        for rows in [
            vec![TokenRowData::Spans(vec![]); MAX_ANALYSIS_RECORDS],
            vec![TokenRowData::Spans(vec![]); MAX_ANALYSIS_RECORDS + 1],
            vec![TokenRowData::Spans(vec![
                (0, TokenKind::Plain);
                MAX_ANALYSIS_RECORDS
            ])],
            vec![TokenRowData::Spans(vec![
                (0, TokenKind::Plain);
                MAX_ANALYSIS_RECORDS + 1
            ])],
            vec![TokenRowData::Previous {
                reuse: 0,
                count: usize::MAX,
            }],
        ] {
            let data = SyntaxAnalysisData {
                source: SyntaxSource::Full(String::new()),
                folds: vec![],
                structure: None,
                highlights: Some(rows),
                base_ticket: None,
            };
            let reference = data.record_count() <= MAX_ANALYSIS_RECORDS;
            let Some(mut work) = RecordAdmission::new(&data) else {
                assert!(!reference);
                continue;
            };
            let mut units = 1;
            let accepted = loop {
                match work.advance(&data) {
                    Some(false) => units += 1,
                    Some(true) => break true,
                    None => break false,
                }
            };
            assert_eq!(accepted, reference);
            if data.highlights.as_ref().unwrap().len() > 64 {
                assert_eq!(units, data.highlights.as_ref().unwrap().len().div_ceil(64));
            }
        }
    }

    fn compare_rows_with_original(
        source: &str,
        data: Vec<super::super::TokenRowData>,
        previous: Option<&super::super::SyntaxAnalysis>,
    ) -> usize {
        use super::super::{SyntaxAnalysisData, TokenRowData};
        let wire = SyntaxAnalysisData {
            source: SyntaxSource::Full(source.into()),
            folds: vec![],
            structure: None,
            highlights: Some(data),
            base_ticket: previous.map(|_| 42),
        };
        let source = Arc::new(source.to_owned());
        let lines = source.split('\n').count();
        let reference = wire.clone().validate_verified_reference(
            source.clone(),
            lines,
            previous.map(|old| (42, old)),
        );
        let mut work = wire
            .prepare_reconstruction(source.clone(), lines, previous.map(|old| (42, old)))
            .unwrap();
        let mut units = 0;
        let actual = loop {
            units += 1;
            match work.advance() {
                Some(false) => {}
                Some(true) => break work.finish(),
                None => break None,
            }
        };
        assert_eq!(actual.is_some(), reference.is_some());
        if let (Some(actual), Some(reference)) = (actual, reference) {
            assert_eq!(actual.highlights(), reference.highlights());
            assert!(Arc::ptr_eq(actual.source_snapshot(), &source));
            if let Some(previous) = previous {
                // A single retained range uses the exact old immutable row owners.
                let retained = SyntaxAnalysisData {
                    source: SyntaxSource::Full(String::new()),
                    folds: vec![],
                    structure: None,
                    highlights: Some(vec![TokenRowData::Previous {
                        reuse: 0,
                        count: lines,
                    }]),
                    base_ticket: Some(42),
                };
                let mut work = retained
                    .prepare_reconstruction(source.clone(), lines, Some((42, previous)))
                    .unwrap();
                while let Some(false) = work.advance() {}
                if let Some(restored) = work.finish() {
                    assert!(
                        restored
                            .highlights()
                            .unwrap()
                            .iter()
                            .zip(previous.highlights().unwrap().iter())
                            .all(|(a, b)| Arc::ptr_eq(a, b))
                    );
                }
            }
        }
        units
    }

    #[test]
    fn token_reconstruction_matches_original_rows_ranges_and_failures() {
        use super::super::TokenRowData;
        use crate::highlight::{Token, TokenKind};
        for source in ["", "文😀", "a\r\n文😀\nlast", "last\r", "\n\n"] {
            let rows = source
                .split('\n')
                .map(|line| {
                    Arc::from(vec![Token {
                        kind: TokenKind::Plain,
                        text: line.into(),
                    }])
                })
                .collect::<Vec<crate::highlight::TokenRow>>();
            let previous = super::super::SyntaxAnalysis {
                source: Arc::new(source.into()),
                folds: vec![],
                structure: None,
                highlights: Some(Arc::new(rows)),
            };
            let spans = source
                .split('\n')
                .map(|line| TokenRowData::Spans(vec![(line.len(), TokenKind::Plain)]))
                .collect::<Vec<_>>();
            compare_rows_with_original(source, spans.clone(), None);
            compare_rows_with_original(
                source,
                vec![TokenRowData::Previous {
                    reuse: 0,
                    count: source.split('\n').count(),
                }],
                Some(&previous),
            );
            for end in 0..source.split('\n').next().unwrap().len() + 2 {
                let mut corrupt = spans.clone();
                corrupt[0] = TokenRowData::Spans(vec![(end, TokenKind::Plain)]);
                compare_rows_with_original(source, corrupt, None);
            }
            for (reuse, count) in [(0, 0), (0, usize::MAX), (usize::MAX, 1), (1, 1), (0, 1)] {
                compare_rows_with_original(
                    source,
                    vec![TokenRowData::Previous { reuse, count }],
                    Some(&previous),
                );
                compare_rows_with_original(
                    source,
                    vec![TokenRowData::Previous { reuse, count }],
                    None,
                );
            }
            compare_rows_with_original(source, vec![], None);
            let mut extra = spans.clone();
            extra.push(TokenRowData::Spans(vec![]));
            compare_rows_with_original(source, extra, None);
            let mut corrupt_previous = previous.clone();
            corrupt_previous.highlights = Some(Arc::new(vec![
                Arc::from(vec![Token {
                    kind: TokenKind::Plain,
                    text: "mismatch".into()
                }]);
                source.split('\n').count()
            ]));
            compare_rows_with_original(
                source,
                vec![TokenRowData::Previous {
                    reuse: 0,
                    count: source.split('\n').count(),
                }],
                Some(&corrupt_previous),
            );
        }
    }

    #[test]
    fn long_tokens_dense_rows_and_partial_reconstruction_remain_bounded() {
        use super::super::{SyntaxAnalysisData, TokenRowData};
        use crate::highlight::{Token, TokenKind};
        for source in ["文😀".repeat(40_000), "\n".repeat(49_000), "x".repeat(8193)] {
            let spans = source
                .split('\n')
                .map(|line| TokenRowData::Spans(vec![(line.len(), TokenKind::Plain)]))
                .collect::<Vec<_>>();
            let units = compare_rows_with_original(&source, spans.clone(), None);
            assert!(units >= source.len().div_ceil(SOURCE_BATCH_BYTES));
            assert!(units > 2);
            let previous = super::super::SyntaxAnalysis {
                source: Arc::new(source.clone()),
                folds: vec![],
                structure: None,
                highlights: Some(Arc::new(
                    source
                        .split('\n')
                        .map(|line| {
                            Arc::from(vec![Token {
                                kind: TokenKind::Plain,
                                text: line.into(),
                            }])
                        })
                        .collect(),
                )),
            };
            let units = compare_rows_with_original(
                &source,
                vec![TokenRowData::Previous {
                    reuse: 0,
                    count: source.split('\n').count(),
                }],
                Some(&previous),
            );
            assert!(units > 2);
            let wire = SyntaxAnalysisData {
                source: SyntaxSource::Full(String::new()),
                folds: vec![],
                structure: None,
                highlights: Some(spans),
                base_ticket: None,
            };
            let mut work = wire
                .prepare_reconstruction(Arc::new(source.clone()), source.split('\n').count(), None)
                .unwrap();
            assert_eq!(work.advance(), Some(false));
            assert!(work.finish().is_none());
        }
        let source = "x".repeat(8193);
        let spans = (1..=source.len())
            .map(|end| (end, TokenKind::Plain))
            .collect();
        assert!(compare_rows_with_original(&source, vec![TokenRowData::Spans(spans)], None) >= 128);
    }

    #[test]
    fn bounded_source_proof_matches_original_validation() {
        for old in ["", "文😀", "a\r\n文😀\nlast", "last\r"] {
            let positions = old
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(old.len()))
                .collect::<Vec<_>>();
            for &start in &positions {
                for &end in positions.iter().filter(|&&end| end >= start) {
                    for text in ["", "new", "😀文\r\n"] {
                        let wire = SyntaxSource::Replace {
                            start,
                            end,
                            text: text.into(),
                        };
                        let source = format!("{}{text}{}", &old[..start], &old[end..]);
                        for expected in [
                            source.clone(),
                            format!("{source}x"),
                            source.replace('文', "字"),
                        ] {
                            let expected = Arc::new(expected);
                            let old_analysis = super::super::SyntaxAnalysis {
                                source: Arc::new(old.into()),
                                folds: vec![],
                                structure: None,
                                highlights: None,
                            };
                            let reference =
                                wire.clone().validate(expected.clone(), Some(&old_analysis));
                            let actual = SourceValidation::new(
                                wire.clone(),
                                expected.clone(),
                                Some(old_analysis.source_snapshot().clone()),
                            )
                            .and_then(|mut work| {
                                while !work.advance() {}
                                work.finish()
                            });
                            assert_eq!(actual.is_some(), reference.is_some());
                            if let Some((shared, lines)) = actual {
                                assert!(Arc::ptr_eq(&shared, &expected));
                                assert_eq!(lines, expected.split('\n').count());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn long_sources_yield_and_never_expose_incomplete_or_invalid_proofs() {
        let source = Arc::new("文😀\r\n".repeat(20_000));
        let mut work =
            SourceValidation::new(SyntaxSource::Full(source.to_string()), source.clone(), None)
                .unwrap();
        assert!(!work.advance());
        assert!(work.finish().is_none());
        let mut work =
            SourceValidation::new(SyntaxSource::Full(source.to_string()), source.clone(), None)
                .unwrap();
        let mut batches = 1;
        while !work.advance() {
            batches += 1;
        }
        assert!(batches >= source.len().div_ceil(SOURCE_BATCH_BYTES));
        let (shared, lines) = work.finish().unwrap();
        assert!(Arc::ptr_eq(&source, &shared));
        assert_eq!(lines, 20_001);
        for wire in [
            SyntaxSource::Replace {
                start: 1,
                end: 3,
                text: String::new(),
            },
            SyntaxSource::Replace {
                start: 3,
                end: 1,
                text: String::new(),
            },
            SyntaxSource::Replace {
                start: 0,
                end: source.len() + 1,
                text: String::new(),
            },
        ] {
            assert!(SourceValidation::new(wire, source.clone(), Some(source.clone())).is_none());
        }
    }
}
