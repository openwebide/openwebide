//! Bounded source proof before receiver reconstruction can use source coordinates.
use super::{MAX_STRUCTURE_BYTES, SyntaxSource};
use std::sync::Arc;

const SOURCE_BATCH_BYTES: usize = 8 * 1024;

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

#[cfg(test)]
mod tests {
    use super::*;

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
