//! Bounded decoding of the existing recovery wire format, before validation.
use super::{
    DocumentRecovery, EditorRecovery, EditorRecoveryFile, EditorRecoveryRecord, FoldRange,
    MAX_DOCUMENT_BYTES, MAX_RECOVERY_FILES, RecoveryFile, RecoveryRecord, Selection,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use std::{borrow::Cow, collections::VecDeque, sync::Arc};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncodedDocumentRecovery<'a> {
    #[serde(borrow)]
    text: Cow<'a, str>,
    #[serde(borrow)]
    saved: Cow<'a, str>,
    selections: Vec<Selection>,
    collapsed: Vec<FoldRange>,
}

pub(super) struct RecoveryTextDecoding<T> {
    encoded: T,
    offset: usize,
    bytes: Vec<u8>,
    utf8: Option<crate::utf8::Utf8TextPreparation>,
    result: Option<Result<String, String>>,
}
impl<T: AsRef<str>> RecoveryTextDecoding<T> {
    pub fn new(encoded: T) -> Self {
        let mut result = (encoded.as_ref().len() > MAX_DOCUMENT_BYTES.div_ceil(3) * 4)
            .then(|| Err("Recovered text exceeds document size limit".into()));
        let mut bytes = Vec::new();
        if result.is_none()
            && let Err(error) = bytes
                .try_reserve_exact((encoded.as_ref().len().div_ceil(4) * 3).min(MAX_DOCUMENT_BYTES))
        {
            result = Some(Err(error.to_string()));
        }
        Self {
            encoded,
            offset: 0,
            bytes,
            utf8: None,
            result,
        }
    }
    /// Decode at most 8 KiB of base64 or validate at most 8 KiB of UTF-8 per unit.
    pub fn advance(&mut self, budget: usize) -> bool {
        for _ in 0..budget {
            if self.result.is_some() {
                break;
            }
            if let Some(utf8) = &mut self.utf8 {
                if utf8.advance(1) {
                    self.result = Some(
                        self.utf8
                            .take()
                            .unwrap()
                            .finish()
                            .unwrap()
                            .map_err(|e| e.to_string()),
                    );
                }
                continue;
            }
            let encoded = self.encoded.as_ref();
            let end = (self.offset + 8192).min(encoded.len());
            // Base64 is ASCII; byte slices also safely reject non-ASCII input.
            let chunk = &encoded.as_bytes()[self.offset..end];
            if end < encoded.len() && chunk.contains(&b'=') {
                self.result = Some(Err(
                    "Invalid base64 padding before end of recovered text".into()
                ));
                continue;
            }
            let mut decoded = [0; 6144];
            match STANDARD.decode_slice(chunk, &mut decoded) {
                Ok(count) if self.bytes.len() + count <= MAX_DOCUMENT_BYTES => {
                    self.bytes.extend_from_slice(&decoded[..count]);
                    self.offset = end;
                    if end == encoded.len() {
                        self.utf8 = Some(crate::utf8::Utf8TextPreparation::new(std::mem::take(
                            &mut self.bytes,
                        )));
                    }
                }
                Ok(_) => {
                    self.result = Some(Err("Recovered text exceeds document size limit".into()));
                }
                Err(error) => self.result = Some(Err(error.to_string())),
            }
        }
        self.result.is_some()
    }
    pub fn finish(self) -> Option<Result<String, String>> {
        self.result
    }
}

/// Holds encoded sources privately and publishes a record only after every
/// source is decoded. Metadata validation remains the shared recovery policy.
pub struct RecoveryRecordDecoding<'a> {
    files: VecDeque<RecoveryFile<EncodedDocumentRecovery<'a>>>,
    record: EditorRecoveryRecord,
    active: Option<ActiveDocument<'a>>,
    result: Option<Result<(), String>>,
}
struct ActiveDocument<'a> {
    file: EditorRecoveryFile,
    selections: Vec<Selection>,
    collapsed: Vec<FoldRange>,
    text: RecoveryTextDecoding<Cow<'a, str>>,
    saved: RecoveryTextDecoding<Cow<'a, str>>,
}
impl<'a> RecoveryRecordDecoding<'a> {
    pub fn new(encoded: RecoveryRecord<EncodedDocumentRecovery<'a>>) -> Self {
        let result = if encoded.revision < 0 {
            Some(Err(
                "Invalid editor recovery revision returned by server".into()
            ))
        } else if encoded.state.files.len() > MAX_RECOVERY_FILES {
            Some(Err("Too many open files for editor recovery".into()))
        } else {
            None
        };
        Self {
            record: EditorRecoveryRecord {
                revision: encoded.revision,
                state: EditorRecovery {
                    format: encoded.state.format,
                    root: encoded.state.root,
                    selected: encoded.state.selected,
                    files: Vec::new(),
                },
            },
            files: encoded.state.files.into(),
            active: None,
            result,
        }
    }
    pub fn advance(&mut self, budget: usize) -> bool {
        for _ in 0..budget {
            if self.result.is_some() {
                break;
            }
            if let Some(ActiveDocument { text, saved, .. }) = &mut self.active {
                if !text.advance(1) {
                    continue;
                }
                if text.result.as_ref().is_some_and(Result::is_err) {
                    self.result = Some(Err(text.result.take().unwrap().unwrap_err()));
                    continue;
                }
                if !saved.advance(1) {
                    continue;
                }
                let ActiveDocument {
                    mut file,
                    selections,
                    collapsed,
                    text,
                    saved,
                } = self.active.take().unwrap();
                match text
                    .finish()
                    .unwrap()
                    .and_then(|text| saved.finish().unwrap().map(|saved| (text, saved)))
                {
                    Ok((text, saved)) => {
                        file.document = Some(DocumentRecovery {
                            text: Arc::new(text),
                            saved: Arc::new(saved),
                            selections,
                            collapsed,
                        });
                        self.record.state.files.push(file);
                    }
                    Err(error) => self.result = Some(Err(error)),
                }
                continue;
            }
            let Some(file) = self.files.pop_front() else {
                self.result = Some(Ok(()));
                continue;
            };
            let plain = EditorRecoveryFile {
                path: file.path,
                document: None,
                scroll: file.scroll,
                read_only: file.read_only,
            };
            if let Some(document) = file.document {
                self.active = Some(ActiveDocument {
                    file: plain,
                    selections: document.selections,
                    collapsed: document.collapsed,
                    text: RecoveryTextDecoding::new(document.text),
                    saved: RecoveryTextDecoding::new(document.saved),
                });
            } else {
                self.record.state.files.push(plain);
            }
        }
        self.result.is_some()
    }
    pub fn finish(self) -> Option<Result<EditorRecoveryRecord, String>> {
        self.result.map(|result| result.map(|()| self.record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(encoded: &str, budget: usize) -> Result<String, String> {
        let mut preparation = RecoveryTextDecoding::new(encoded);
        assert!(preparation.finish().is_none());
        preparation = RecoveryTextDecoding::new(encoded);
        while !preparation.advance(budget) {}
        preparation.finish().unwrap()
    }

    #[test]
    fn bounded_base64_and_utf8_decode_match_standard_at_chunk_seams() {
        let sources = [
            String::new(),
            "a".repeat(6143),
            "a".repeat(6144),
            "a".repeat(6145),
            format!("{}😀\r\n{}", "a".repeat(8191), "é".repeat(10_000)),
        ];
        for source in sources {
            let encoded = STANDARD.encode(source.as_bytes());
            for budget in [1, 3, usize::MAX] {
                assert_eq!(decode(&encoded, budget), Ok(source.clone()));
            }
        }
        for invalid in ["a", "====", "YQ==Yg==", "YWJj$", "YR==", "😀"] {
            assert!(STANDARD.decode(invalid).is_err());
            assert!(decode(invalid, 1).is_err());
        }
        let internal_padding = format!("{}YQ=={}", "YWJj".repeat(2047), "YWJj");
        assert!(decode(&internal_padding, 1).is_err());
        assert!(decode(&STANDARD.encode([0xff]), 1).is_err());
    }

    #[test]
    fn record_decode_publishes_only_complete_wire_equivalent_sources_in_both_modes() {
        for mode in [crate::WorkspaceMode::Local, crate::WorkspaceMode::Remote] {
            let source = format!("{}😀\r\n", "x".repeat(20_000));
            let document = super::super::Document::new(source).recovery();
            let record = EditorRecoveryRecord {
                revision: 7,
                state: EditorRecovery {
                    root: Some(super::super::EditorRecoveryRoot {
                        mode,
                        path: (mode == crate::WorkspaceMode::Remote).then(|| "/project".into()),
                    }),
                    selected: Some("test.txt".into()),
                    files: vec![EditorRecoveryFile {
                        path: "test.txt".into(),
                        document: Some(document),
                        scroll: super::super::RecoveryScroll {
                            top: 35.0,
                            left: 17.0,
                        },
                        read_only: true,
                    }],
                    ..EditorRecovery::default()
                },
            };
            let json = serde_json::to_string(&record).unwrap();
            let encoded = serde_json::from_str(&json).unwrap();
            let mut decoding = RecoveryRecordDecoding::new(encoded);
            assert!(!decoding.advance(1));
            assert!(decoding.finish().is_none());
            for budget in [1, 3, usize::MAX] {
                let mut decoding =
                    RecoveryRecordDecoding::new(serde_json::from_str(&json).unwrap());
                while !decoding.advance(budget) {}
                let decoded = decoding.finish().unwrap().unwrap();
                assert_eq!(decoded, record);
                decoded.state.validate().unwrap();
            }
        }
    }

    #[test]
    fn encoded_sources_borrow_json_and_preserve_escaped_field_compatibility() {
        let json = r#"{"text":"YQ==","saved":"Yg==","selections":[],"collapsed":[]}"#;
        let encoded: EncodedDocumentRecovery<'_> = serde_json::from_str(json).unwrap();
        assert!(matches!(encoded.text, Cow::Borrowed(_)));
        assert!(matches!(encoded.saved, Cow::Borrowed(_)));
        let range = json.as_ptr() as usize..json.as_ptr() as usize + json.len();
        assert!(range.contains(&(encoded.text.as_ptr() as usize)));
        assert!(range.contains(&(encoded.saved.as_ptr() as usize)));
        let escaped = json.replace("YQ==", r"\u0059Q==");
        let encoded: EncodedDocumentRecovery<'_> = serde_json::from_str(&escaped).unwrap();
        assert!(matches!(encoded.text, Cow::Owned(_)));
        assert_eq!(decode(&encoded.text, 1), Ok("a".into()));
        assert!(
            serde_json::from_str::<EncodedDocumentRecovery<'_>>(
                &json.replace("[]}", "[],\"extra\":true}")
            )
            .is_err()
        );
    }
}
