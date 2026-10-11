//! Cooperative materialization of the existing source/structure/token wire representation.
use super::{
    MAX_ANALYSIS_RECORDS, SourcePublication, SyntaxAnalysisData, SyntaxSource, TokenRowData,
};
use crate::{
    editor::{SyntaxAnalysis, structure::StructurePublicationPreparation},
    highlight::TokenRows,
};

#[derive(Default)]
struct Row {
    candidate: usize,
    token: usize,
    byte: usize,
    end: usize,
    spans: Vec<(usize, crate::highlight::TokenKind)>,
}
impl Row {
    fn advance(
        &mut self,
        index: usize,
        lines: &TokenRows,
        previous: Option<&TokenRows>,
    ) -> Option<TokenRowData> {
        let line = &lines[index];
        let shifted = previous.and_then(|old| {
            if lines.len() >= old.len() {
                index.checked_sub(lines.len() - old.len())
            } else {
                index.checked_add(old.len() - lines.len())
            }
        });
        let candidates = [Some(index), shifted];
        let mut tokens = 64;
        let mut bytes = 8192;
        while self.candidate < 2 && tokens > 0 && bytes > 0 {
            let reuse = candidates[self.candidate];
            let old = reuse.and_then(|reuse| previous?.get(reuse));
            if let Some(old) = old.filter(|old| old.len() == line.len()) {
                if std::sync::Arc::ptr_eq(line, old) || self.token == line.len() {
                    return Some(TokenRowData::Previous {
                        reuse: reuse.unwrap(),
                        count: 1,
                    });
                }
                let current = &line[self.token];
                let earlier = &old[self.token];
                if current.kind == earlier.kind && current.text.len() == earlier.text.len() {
                    let end = self.byte.saturating_add(bytes).min(current.text.len());
                    if current.text.as_bytes()[self.byte..end]
                        == earlier.text.as_bytes()[self.byte..end]
                    {
                        bytes -= end - self.byte;
                        self.byte = end;
                        if end == current.text.len() {
                            self.token += 1;
                            self.byte = 0;
                            tokens -= 1;
                        }
                        continue;
                    }
                }
            }
            self.candidate += 1;
            self.token = 0;
            self.byte = 0;
            tokens -= 1;
        }
        if self.candidate < 2 {
            return None;
        }
        while self.token < line.len() && tokens > 0 {
            let token = &line[self.token];
            self.end += token.text.len();
            self.spans.push((self.end, token.kind));
            self.token += 1;
            tokens -= 1;
        }
        (self.token == line.len()).then(|| TokenRowData::Spans(std::mem::take(&mut self.spans)))
    }
}

pub(in crate::editor::syntax) struct AnalysisTransferPreparation {
    phase: usize,
    count_row: usize,
    records: usize,
    failed: bool,
    source: Option<SyntaxSource>,
    source_work: Option<SourcePublication>,
    folds: Vec<crate::editor::FoldRange>,
    structure_work: Option<StructurePublicationPreparation>,
    structure: Option<crate::editor::structure::StructurePublication>,
    highlights: Vec<TokenRowData>,
    highlight_records: usize,
    row: usize,
    row_work: Row,
    reused: bool,
}
impl AnalysisTransferPreparation {
    pub fn new(
        current: &SyntaxAnalysis,
        previous: Option<(u32, &SyntaxAnalysis)>,
        source: Option<SyntaxSource>,
    ) -> Self {
        let records = current
            .folds
            .len()
            .saturating_add(
                current
                    .structure
                    .as_ref()
                    .map_or(0, |value| value.record_count()),
            )
            .saturating_add(current.highlights.as_ref().map_or(0, |lines| lines.len()));
        let source_work = source
            .is_none()
            .then(|| SourcePublication::new(current.source.len(), previous.is_some()));
        Self {
            phase: 0,
            count_row: 0,
            records,
            failed: false,
            source,
            source_work,
            folds: Vec::new(),
            structure_work: current
                .structure
                .as_ref()
                .map(|_| StructurePublicationPreparation::default()),
            structure: None,
            highlights: Vec::new(),
            highlight_records: 0,
            row: 0,
            row_work: Row::default(),
            reused: false,
        }
    }
    /// One unit visits/copies at most 64 fixed records or compares 8 KiB token text.
    /// Retained inputs must remain the same immutable snapshots for every call.
    pub fn advance(
        &mut self,
        current: &SyntaxAnalysis,
        previous: Option<(u32, &SyntaxAnalysis)>,
    ) -> bool {
        if self.failed || self.phase == 5 {
            return true;
        }
        let old = previous.map(|(_, analysis)| analysis);
        match self.phase {
            0 => {
                if let Some(lines) = &current.highlights {
                    let end = self.count_row.saturating_add(64).min(lines.len());
                    for line in &lines[self.count_row..end] {
                        self.records = self.records.saturating_add(line.len());
                    }
                    self.count_row = end;
                    if self.records > MAX_ANALYSIS_RECORDS {
                        self.failed = true;
                        return true;
                    }
                    if end != lines.len() {
                        return false;
                    }
                }
                if self.records > MAX_ANALYSIS_RECORDS {
                    self.failed = true;
                    return true;
                }
                self.phase = 1;
            }
            1 => {
                if let Some(work) = &mut self.source_work {
                    if !work.advance(current.source(), old.map(SyntaxAnalysis::source), 8192) {
                        return false;
                    }
                    self.source = self.source_work.take().and_then(SourcePublication::finish);
                }
                self.reused = matches!(self.source, Some(SyntaxSource::Replace { .. }));
                self.phase = 2;
            }
            2 => {
                let end = self.folds.len().saturating_add(64).min(current.folds.len());
                self.folds
                    .extend_from_slice(&current.folds[self.folds.len()..end]);
                if end == current.folds.len() {
                    self.phase = 3;
                }
            }
            3 => {
                if let Some(work) = &mut self.structure_work {
                    let structure = current
                        .structure
                        .as_deref()
                        .expect("structure preparation has input");
                    if !work.advance(structure, old.and_then(|value| value.structure.as_deref())) {
                        return false;
                    }
                    self.structure = self
                        .structure_work
                        .take()
                        .and_then(|work| work.finish(structure));
                    self.reused |= self
                        .structure
                        .as_ref()
                        .is_some_and(super::StructurePublication::needs_base);
                }
                self.phase = 4;
            }
            _ => {
                if let Some(lines) = &current.highlights
                    && self.row < lines.len()
                {
                    let Some(data) = self.row_work.advance(
                        self.row,
                        lines,
                        old.and_then(|value| value.highlights.as_deref()),
                    ) else {
                        return false;
                    };
                    self.highlight_records = self.highlight_records.saturating_add(data.records());
                    if let TokenRowData::Previous { reuse, count } = data {
                        self.reused = true;
                        match self.highlights.last_mut() {
                            Some(TokenRowData::Previous {
                                reuse: start,
                                count: existing,
                            }) if start.checked_add(*existing) == Some(reuse) => {
                                *existing += count;
                            }
                            _ => self
                                .highlights
                                .push(TokenRowData::Previous { reuse, count }),
                        }
                    } else {
                        self.highlights.push(data);
                    }
                    self.row += 1;
                    self.row_work = Row::default();
                    if self.row < lines.len() {
                        return false;
                    }
                }
                self.phase = 5;
            }
        }
        self.phase == 5
    }
    pub fn finish(
        self,
        current: &SyntaxAnalysis,
        previous: Option<(u32, &SyntaxAnalysis)>,
    ) -> Option<SyntaxAnalysisData> {
        if self.failed || self.phase != 5 {
            return None;
        }
        let records = self
            .folds
            .len()
            .saturating_add(
                self.structure
                    .as_ref()
                    .map_or(0, super::StructurePublication::record_count),
            )
            .saturating_add(self.highlights.len())
            .saturating_add(self.highlight_records);
        if records > MAX_ANALYSIS_RECORDS {
            return None;
        }
        Some(SyntaxAnalysisData {
            source: self.source?,
            folds: self.folds,
            structure: self.structure,
            highlights: current.highlights.as_ref().map(|_| self.highlights),
            base_ticket: self
                .reused
                .then(|| previous.expect("reused previous publication").0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        editor::SyntaxPreparations,
        highlight::{Language, Token, TokenKind},
    };
    use std::sync::Arc;

    fn complete(
        current: &SyntaxAnalysis,
        previous: Option<(u32, &SyntaxAnalysis)>,
    ) -> Option<SyntaxAnalysisData> {
        let mut work = AnalysisTransferPreparation::new(current, previous, None);
        let mut steps = 0;
        while !work.advance(current, previous) {
            steps += 1;
            assert!(steps < 200_000);
        }
        work.finish(current, previous)
    }

    #[test]
    fn metadata_batches_match_original_full_and_delta_policy() {
        for (language, source) in [
            (
                Language::Rust,
                (r#"fn main() { let s = "文😀"; }"#.to_string() + "\n").repeat(200),
            ),
            (
                Language::Html,
                "<script>let x = 1;</script><style>p {color:red}</style>\n".repeat(100),
            ),
            (
                Language::Markdown,
                "# Header\n\ntext 文😀\n\n```rust\nfn main() {}\n```\n".repeat(100),
            ),
            (Language::Yaml, "key: value\nlist: [one, two]\n".repeat(200)),
            (Language::Sql, "SELECT 1;\n".repeat(200)),
        ] {
            let mut cache = SyntaxPreparations::default();
            let prepare = |cache: &mut SyntaxPreparations<usize>, key, source: String| {
                let source = Arc::new(source);
                loop {
                    if let Some(result) = cache.prepare_cooperative(
                        key,
                        language,
                        source.clone(),
                        4,
                        || true,
                        || false,
                    ) {
                        break result.1.unwrap_or_else(|| {
                            panic!("missing {language:?} analysis: {:?}", result.0)
                        });
                    }
                }
            };
            let old = prepare(&mut cache, 0, source.clone());
            for changed in [
                source.clone(),
                format!("\n{source}"),
                source.replace("main", "new_name"),
                source[..source.floor_char_boundary(source.len() / 2)].to_string(),
            ] {
                let current = prepare(&mut cache, 1, changed);
                for previous in [None, Some((7, old.as_ref()))] {
                    let source = SyntaxSource::publication(
                        current.source(),
                        previous.map(|(_, old)| old.source()),
                    );
                    let expected = current.transfer_data_with_source_reference(previous, source);
                    assert_eq!(
                        serde_json::to_vec(&complete(&current, previous)).unwrap(),
                        serde_json::to_vec(&expected).unwrap(),
                        "{language:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn giant_equal_token_comparisons_yield_before_finishing_metadata() {
        let source = Arc::new("文😀".repeat(40_000));
        let analysis = || SyntaxAnalysis {
            source: source.clone(),
            folds: Vec::new(),
            structure: None,
            highlights: Some(Arc::new(vec![Arc::from([Token {
                text: source.as_str().into(),
                kind: TokenKind::Plain,
            }])])),
        };
        let old = analysis();
        let current = analysis();
        let previous = Some((9, &old));
        let mut work = AnalysisTransferPreparation::new(&current, previous, None);
        let mut comparisons = 0;
        while !work.advance(&current, previous) {
            if work.phase == 4 && work.row_work.byte > 0 {
                comparisons += 1;
            }
            assert!(work.phase < 5);
        }
        assert!(comparisons > 20);
        let expected = current.transfer_data_with_source_reference(
            previous,
            SyntaxSource::publication(current.source(), Some(old.source())),
        );
        assert_eq!(
            serde_json::to_vec(&work.finish(&current, previous)).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
    }

    #[test]
    fn compressed_empty_rows_preserve_exact_transmitted_record_limit() {
        let rows = MAX_ANALYSIS_RECORDS;
        let source = Arc::new("\n".repeat(rows - 1));
        let current = SyntaxAnalysis {
            source,
            folds: Vec::new(),
            structure: None,
            highlights: Some(Arc::new(vec![Arc::from([]); rows])),
        };
        assert!(complete(&current, Some((1, &current))).is_none());
        assert!(complete(&current, None).is_some());
        assert!(
            current
                .transfer_data_with_source_reference(
                    Some((1, &current)),
                    SyntaxSource::publication(current.source(), Some(current.source()))
                )
                .is_none()
        );
    }
}
