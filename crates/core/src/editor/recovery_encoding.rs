//! Bounded encoding of the existing recovery JSON/base64 wire format.
use super::{EditorRecoveryRecord, recovery_body_limit};
use base64::{Engine, engine::general_purpose::STANDARD};

#[derive(Clone, Copy)]
enum Phase {
    Header,
    File,
    Text,
    Saved,
    Selections,
    Folds,
    FileEnd,
    End,
}

/// Callers validate the immutable record before encoding. Only complete JSON is
/// exposed; source encoding and metadata arrays use bounded append steps.
pub struct RecoveryRecordEncoding<'a> {
    record: &'a EditorRecoveryRecord,
    body: String,
    encoded_bytes: usize,
    phase: Phase,
    file: usize,
    next: usize,
    result: Option<Result<(), String>>,
}
impl<'a> RecoveryRecordEncoding<'a> {
    fn empty(record: &'a EditorRecoveryRecord) -> Self {
        Self {
            record,
            body: String::new(),
            encoded_bytes: 0,
            phase: Phase::Header,
            file: 0,
            next: 0,
            result: None,
        }
    }
    pub fn new(record: &'a EditorRecoveryRecord) -> Self {
        // Validated records have at most 64 files and bounded paths/metadata.
        // Reserve once so later bounded appends never copy a growing full body.
        let mut capacity = 256_usize;
        if let Some(root) = &record.state.root {
            capacity = capacity.saturating_add(
                root.path
                    .as_ref()
                    .map_or(0, |path| path.len().saturating_mul(6)),
            );
        }
        capacity = capacity.saturating_add(
            record
                .state
                .selected
                .as_ref()
                .map_or(0, |path| path.len().saturating_mul(6)),
        );
        for file in &record.state.files {
            capacity = capacity
                .saturating_add(256)
                .saturating_add(file.path.len().saturating_mul(6));
            if let Some(document) = &file.document {
                capacity = capacity
                    .saturating_add(document.text.len().div_ceil(3).saturating_mul(4))
                    .saturating_add(document.saved.len().div_ceil(3).saturating_mul(4))
                    .saturating_add(document.selections.len().saturating_mul(64))
                    .saturating_add(document.collapsed.len().saturating_mul(96));
            }
        }
        let mut body = String::new();
        let result = body
            .try_reserve_exact(capacity.min(recovery_body_limit()))
            .err()
            .map(|error| Err(error.to_string()));
        Self {
            body,
            result,
            ..Self::empty(record)
        }
    }
    /// A unit encodes at most 6 KiB of source or 64 selection/fold records.
    pub fn advance(&mut self, budget: usize) -> bool {
        for _ in 0..budget {
            if self.result.is_some() {
                break;
            }
            let previous = self.body.len();
            if let Err(error) = self.step() {
                self.result = Some(Err(error));
            }
            self.encoded_bytes = self
                .encoded_bytes
                .saturating_add(self.body.len() - previous);
            if self.encoded_bytes > recovery_body_limit() {
                self.result = Some(Err("Editor recovery body exceeds its size limit".into()));
            }
        }
        self.result.is_some()
    }
    fn step(&mut self) -> Result<(), String> {
        match self.phase {
            Phase::Header => {
                self.body.push_str("{\"revision\":");
                self.json(&self.record.revision)?;
                self.body.push_str(",\"state\":{\"format\":");
                self.json(&self.record.state.format)?;
                self.body.push_str(",\"root\":");
                self.json(&self.record.state.root)?;
                self.body.push_str(",\"selected\":");
                self.json(&self.record.state.selected)?;
                self.body.push_str(",\"files\":[");
                self.phase = Phase::File;
            }
            Phase::File => {
                if self.file == self.record.state.files.len() {
                    self.phase = Phase::End;
                    return Ok(());
                }
                if self.file > 0 {
                    self.body.push(',');
                }
                self.body.push_str("{\"path\":");
                self.json(&self.record.state.files[self.file].path)?;
                self.body.push_str(",\"document\":");
                if self.record.state.files[self.file].document.is_some() {
                    self.body.push_str("{\"text\":\"");
                    self.phase = Phase::Text;
                } else {
                    self.body.push_str("null");
                    self.phase = Phase::FileEnd;
                }
            }
            Phase::Text | Phase::Saved => {
                let document = self.record.state.files[self.file]
                    .document
                    .as_ref()
                    .unwrap();
                let source = if matches!(self.phase, Phase::Text) {
                    &document.text
                } else {
                    &document.saved
                };
                // Non-final blocks are multiples of three bytes, so concatenated
                // base64 blocks have exactly the original padding/canonical form.
                let end = self.next.saturating_add(6144).min(source.len());
                let mut encoded = [0; 8192];
                let count = STANDARD
                    .encode_slice(&source.as_bytes()[self.next..end], &mut encoded)
                    .map_err(|error| error.to_string())?;
                self.body
                    .push_str(std::str::from_utf8(&encoded[..count]).unwrap());
                self.next = end;
                if end == source.len() {
                    self.next = 0;
                    if matches!(self.phase, Phase::Text) {
                        self.body.push_str("\",\"saved\":\"");
                        self.phase = Phase::Saved;
                    } else {
                        self.body.push_str("\",\"selections\":[");
                        self.phase = Phase::Selections;
                    }
                }
            }
            Phase::Selections | Phase::Folds => {
                let document = self.record.state.files[self.file]
                    .document
                    .as_ref()
                    .unwrap();
                let count = if matches!(self.phase, Phase::Selections) {
                    document.selections.len()
                } else {
                    document.collapsed.len()
                };
                let end = self.next.saturating_add(64).min(count);
                for index in self.next..end {
                    if index > 0 {
                        self.body.push(',');
                    }
                    if matches!(self.phase, Phase::Selections) {
                        self.json(&document.selections[index])?;
                    } else {
                        self.json(&document.collapsed[index])?;
                    }
                }
                self.next = end;
                if end == count {
                    self.next = 0;
                    if matches!(self.phase, Phase::Selections) {
                        self.body.push_str("],\"collapsed\":[");
                        self.phase = Phase::Folds;
                    } else {
                        self.body.push_str("]}");
                        self.phase = Phase::FileEnd;
                    }
                }
            }
            Phase::FileEnd => {
                self.body.push_str(",\"scroll\":");
                self.json(&self.record.state.files[self.file].scroll)?;
                self.body.push_str(",\"read_only\":");
                self.json(&self.record.state.files[self.file].read_only)?;
                self.body.push('}');
                self.file += 1;
                self.phase = Phase::File;
            }
            Phase::End => {
                self.body.push_str("]}}");
                self.result = Some(Ok(()));
            }
        }
        Ok(())
    }
    fn json(&mut self, value: &impl serde::Serialize) -> Result<(), String> {
        let encoded = serde_json::to_string(value).map_err(|error| error.to_string())?;
        self.body.push_str(&encoded);
        Ok(())
    }
    pub fn finish(self) -> Option<Result<String, String>> {
        self.result.map(|result| result.map(|()| self.body))
    }
}

/// Incremental wire-format output for adapters that retain immutable byte parts.
/// Callers validate the immutable record before encoding, as with the complete
/// encoder. Chunks are JSON prefixes, not independently valid records. A caller must see
/// successful exhaustion before publishing/sending any of them. Errors are
/// terminal, and the total body limit applies across all drained chunks.
pub struct RecoveryRecordChunks<'a> {
    encoding: RecoveryRecordEncoding<'a>,
    done: bool,
}
impl<'a> RecoveryRecordChunks<'a> {
    pub fn new(record: &'a EditorRecoveryRecord) -> Self {
        Self {
            encoding: RecoveryRecordEncoding::empty(record),
            done: false,
        }
    }
}
impl Iterator for RecoveryRecordChunks<'_> {
    type Item = Result<String, String>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let complete = self.encoding.advance(1);
        if complete {
            self.done = true;
            if let Some(Err(error)) = &self.encoding.result {
                return Some(Err(error.clone()));
            }
        }
        Some(Ok(std::mem::take(&mut self.encoding.body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Document, EditorRecoveryFile, EditorRecoveryRoot, RecoveryScroll};
    #[test]
    fn drained_chunks_match_complete_encoding_and_count_the_entire_body() {
        for mode in [crate::WorkspaceMode::Local, crate::WorkspaceMode::Remote] {
            let mut record = EditorRecoveryRecord::default();
            record.state.root = Some(EditorRecoveryRoot {
                mode,
                path: Some("root文".into()),
            });
            record.state.files.push(EditorRecoveryFile {
                path: "a\"文.rs".into(),
                document: Some(
                    Document::new(format!("{}😀\r\n{}", "a".repeat(6143), "文".repeat(20000)))
                        .recovery(),
                ),
                scroll: RecoveryScroll::default(),
                read_only: false,
            });
            let mut chunks = RecoveryRecordChunks::new(&record);
            assert_eq!(chunks.encoding.body.capacity(), 0);
            let mut body = String::new();
            while let Some(chunk) = chunks.next() {
                let chunk = chunk.unwrap();
                assert!(chunk.len() <= 8192 + 32);
                body.push_str(&chunk);
                assert!(chunks.encoding.body.is_empty());
                assert_eq!(chunks.encoding.encoded_bytes, body.len());
            }
            assert_eq!(body, serde_json::to_string(&record).unwrap());
            assert!(chunks.next().is_none());
        }
        // A drained prefix cannot reset the cumulative wire-size admission.
        let record = EditorRecoveryRecord::default();
        let mut chunks = RecoveryRecordChunks::new(&record);
        chunks.encoding.encoded_bytes = recovery_body_limit();
        assert!(chunks.next().unwrap().is_err());
        assert!(chunks.next().is_none());
    }

    #[test]
    fn metadata_batches_preserve_serde_arrays_without_body_growth() {
        use crate::editor::{FoldRange, Selection};
        let mut document = Document::new("line\n".repeat(2001));
        document.set_fold_ranges(
            (0..1000)
                .map(|index| FoldRange {
                    start_line: index * 2,
                    end_line: index * 2 + 1,
                })
                .collect(),
        );
        document.fold_state_mut().collapse_all();
        document
            .set_selections((0..130).map(Selection::caret).collect())
            .unwrap();
        let mut record = EditorRecoveryRecord::default();
        record.state.files.push(EditorRecoveryFile {
            path: "a.rs".into(),
            document: Some(document.recovery()),
            scroll: RecoveryScroll::default(),
            read_only: false,
        });
        let drained: String = RecoveryRecordChunks::new(&record)
            .map(Result::unwrap)
            .collect();
        assert_eq!(drained, serde_json::to_string(&record).unwrap());
        let mut encoding = RecoveryRecordEncoding::new(&record);
        let capacity = encoding.body.capacity();
        while !encoding.advance(1) {
            assert_eq!(encoding.body.capacity(), capacity);
        }
        assert_eq!(encoding.body.capacity(), capacity);
        assert_eq!(
            encoding.finish().unwrap().unwrap(),
            serde_json::to_string(&record).unwrap()
        );
    }

    #[test]
    fn bounded_encoding_matches_serde_exactly_across_source_seams_and_both_modes() {
        for mode in [crate::WorkspaceMode::Local, crate::WorkspaceMode::Remote] {
            for source in [
                String::new(),
                "a".repeat(6143),
                "a".repeat(6144),
                "a".repeat(6145),
                format!("{}😀\r\n{}", "a".repeat(6143), "文\t\"\\".repeat(3000)),
            ] {
                let mut record = EditorRecoveryRecord {
                    revision: 7,
                    ..EditorRecoveryRecord::default()
                };
                record.state.root = Some(EditorRecoveryRoot {
                    mode,
                    path: Some("a\"\\文".into()),
                });
                record.state.selected = Some("a\"文.rs".into());
                record.state.files = vec![
                    EditorRecoveryFile {
                        path: "a\"文.rs".into(),
                        document: Some(Document::new(source).recovery()),
                        scroll: RecoveryScroll {
                            top: 35.5,
                            left: -0.0,
                        },
                        read_only: true,
                    },
                    EditorRecoveryFile {
                        path: "other.rs".into(),
                        document: None,
                        scroll: RecoveryScroll::default(),
                        read_only: false,
                    },
                ];
                let expected = serde_json::to_string(&record).unwrap();
                let drained: String = RecoveryRecordChunks::new(&record)
                    .map(Result::unwrap)
                    .collect();
                assert_eq!(drained, expected);
                for budget in [1, 8, usize::MAX] {
                    let mut encoding = RecoveryRecordEncoding::new(&record);
                    assert!(!encoding.advance(1));
                    while !encoding.advance(budget) {}
                    let encoded = encoding.finish().unwrap().unwrap();
                    assert_eq!(encoded, expected);
                    assert_eq!(
                        serde_json::from_str::<EditorRecoveryRecord>(&encoded).unwrap(),
                        record
                    );
                }
            }
        }
        let record = EditorRecoveryRecord::default();
        let mut encoding = RecoveryRecordEncoding::new(&record);
        assert!(!encoding.advance(1));
        assert!(encoding.finish().is_none());
    }
}
