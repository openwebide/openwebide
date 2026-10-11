//! Bounded typed reply frames. Standard serde decodes one small frame at a time.
use super::*;
use crate::editor::structure::{StructureFrame, StructureFrameDecoding, StructureFrames};
use crate::editor::syntax::{SYNTAX_PROTOCOL_VERSION, SyntaxReply, SyntaxStatus};
use serde::{Deserialize, Serialize};

pub const MAX_SYNTAX_FRAME_BYTES: usize = 64 * 1024;
pub const SYNTAX_FRAME_VERSION: u32 = 1;
const TEXT_BYTES: usize = 8 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum SourceHeader {
    Full {
        bytes: usize,
    },
    Replace {
        start: usize,
        end: usize,
        bytes: usize,
    },
}
impl SourceHeader {
    fn bytes(&self) -> usize {
        match self {
            Self::Full { bytes } | Self::Replace { bytes, .. } => *bytes,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    source: SourceHeader,
    folds: usize,
    structure: bool,
    rows: Option<usize>,
    base_ticket: Option<u32>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Part {
    Begin {
        status: SyntaxStatus,
        analysis: Option<Header>,
    },
    Source(String),
    Folds(Vec<FoldRange>),
    Structure(StructureFrame),
    Rows(Vec<TokenRowData>),
    Row {
        spans: usize,
    },
    Spans(Vec<(usize, TokenKind)>),
    End,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    version: u32,
    framing: u32,
    ticket: u32,
    sequence: u32,
    part: Part,
}
/// A publisher never exposes a partial frame; adapter scheduling owns the yields.
#[derive(Debug)]
pub enum SyntaxFrameProgress {
    Pending,
    Frame(String),
    Complete,
}
pub struct SyntaxReplyFrames {
    reply: SyntaxReply,
    admission: Option<RecordAdmission>,
    structure: Option<StructureFrames>,
    phase: usize,
    offset: usize,
    row: usize,
    spans: usize,
    large: bool,
    sequence: u32,
    bytes: usize,
}
impl SyntaxReplyFrames {
    pub fn new(reply: SyntaxReply) -> Option<Self> {
        if reply.version != SYNTAX_PROTOCOL_VERSION
            || !valid_status(reply.status, reply.analysis.is_some())
        {
            return None;
        }
        let admission = match &reply.analysis {
            Some(data) => {
                if source_text(&data.source).len() > MAX_STRUCTURE_BYTES {
                    return None;
                }
                Some(RecordAdmission::new(data)?)
            }
            None => None,
        };
        Some(Self {
            reply,
            admission,
            structure: None,
            phase: 0,
            offset: 0,
            row: 0,
            spans: 0,
            large: false,
            sequence: 0,
            bytes: 0,
        })
    }
    pub fn advance(&mut self) -> Option<SyntaxFrameProgress> {
        if let Some(admission) = self.admission.as_mut() {
            if !admission.advance(self.reply.analysis.as_ref()?)? {
                return Some(SyntaxFrameProgress::Pending);
            }
            self.admission = None;
        }
        let part = loop {
            match self.phase {
                0 => {
                    let analysis = self.reply.analysis.as_ref().map(|data| Header {
                        source: match &data.source {
                            SyntaxSource::Full(text) => SourceHeader::Full { bytes: text.len() },
                            SyntaxSource::Replace { start, end, text } => SourceHeader::Replace {
                                start: *start,
                                end: *end,
                                bytes: text.len(),
                            },
                        },
                        folds: data.folds.len(),
                        structure: data.structure.is_some(),
                        rows: data.highlights.as_ref().map(Vec::len),
                        base_ticket: data.base_ticket,
                    });
                    self.phase = if analysis.is_some() { 1 } else { 6 };
                    break Part::Begin {
                        status: self.reply.status,
                        analysis,
                    };
                }
                1 => {
                    let text = source_text(&self.reply.analysis.as_ref()?.source);
                    if self.offset == text.len() {
                        self.phase = 2;
                        self.offset = 0;
                        continue;
                    }
                    let end = text.floor_char_boundary(text.len().min(self.offset + TEXT_BYTES));
                    let part = Part::Source(text[self.offset..end].into());
                    self.offset = end;
                    break part;
                }
                2 => {
                    let data = self.reply.analysis.as_mut()?;
                    if self.offset == data.folds.len() {
                        self.structure = data.structure.take().map(StructureFrames::new);
                        self.phase = 3;
                        self.offset = 0;
                        continue;
                    }
                    let end = data.folds.len().min(self.offset + 64);
                    let part = Part::Folds(data.folds[self.offset..end].to_vec());
                    self.offset = end;
                    break part;
                }
                3 => {
                    if let Some(part) = self.structure.as_mut().and_then(StructureFrames::next) {
                        break Part::Structure(part);
                    }
                    self.structure = None;
                    self.phase = 4;
                }
                4 => {
                    let Some(rows) = self.reply.analysis.as_ref()?.highlights.as_ref() else {
                        self.phase = 6;
                        continue;
                    };
                    if self.row == rows.len() {
                        self.phase = 6;
                        continue;
                    }
                    if let TokenRowData::Spans(spans) = &rows[self.row]
                        && spans.len() > 63
                    {
                        if !self.large {
                            self.large = true;
                            break Part::Row { spans: spans.len() };
                        }
                        let end = spans.len().min(self.spans + 64);
                        let part = Part::Spans(spans[self.spans..end].to_vec());
                        self.spans = end;
                        if end == spans.len() {
                            self.row += 1;
                            self.spans = 0;
                            self.large = false;
                        }
                        break part;
                    }
                    let mut budget = 64;
                    let mut batch = Vec::new();
                    while self.row < rows.len() {
                        let cost = match &rows[self.row] {
                            TokenRowData::Spans(spans) => spans.len() + 1,
                            TokenRowData::Previous { .. } => 1,
                        };
                        if cost > budget {
                            break;
                        }
                        budget -= cost;
                        batch.push(rows[self.row].clone());
                        self.row += 1;
                    }
                    break Part::Rows(batch);
                }
                6 => {
                    self.phase = 7;
                    break Part::End;
                }
                _ => return Some(SyntaxFrameProgress::Complete),
            }
        };
        let message = serde_json::to_string(&Frame {
            version: SYNTAX_PROTOCOL_VERSION,
            framing: SYNTAX_FRAME_VERSION,
            ticket: self.reply.ticket,
            sequence: self.sequence,
            part,
        })
        .ok()?;
        self.sequence = self.sequence.checked_add(1)?;
        self.bytes = self.bytes.checked_add(message.len())?;
        if message.len() > MAX_SYNTAX_FRAME_BYTES || self.bytes > MAX_ANALYSIS_MESSAGE_BYTES {
            return None;
        }
        Some(SyntaxFrameProgress::Frame(message))
    }
}
fn source_text(source: &SyntaxSource) -> &str {
    match source {
        SyntaxSource::Full(text) | SyntaxSource::Replace { text, .. } => text,
    }
}
fn valid_status(status: SyntaxStatus, analysis: bool) -> bool {
    matches!(
        (status, analysis),
        (SyntaxStatus::Ready { .. }, true)
            | (
                SyntaxStatus::Cancelled | SyntaxStatus::TooLarge | SyntaxStatus::NeedsSource,
                false
            )
    )
}

/// Transactional frame decoder. No syntax result is exposed before End; callers
/// still run the shared source/coordinate proof before publishing analysis.
pub struct SyntaxReplyFrameDecoding {
    ticket: u32,
    sequence: u32,
    bytes: usize,
    status: Option<SyntaxStatus>,
    header: Option<Header>,
    source: String,
    folds: Vec<FoldRange>,
    structure: StructureFrameDecoding,
    rows: Vec<TokenRowData>,
    large: Option<(usize, Vec<(usize, TokenKind)>)>,
    records: usize,
    phase: usize,
    failed: bool,
}
impl SyntaxReplyFrameDecoding {
    pub fn new(ticket: u32) -> Self {
        Self {
            ticket,
            sequence: 0,
            bytes: 0,
            status: None,
            header: None,
            source: String::new(),
            folds: Vec::new(),
            structure: StructureFrameDecoding::default(),
            rows: Vec::new(),
            large: None,
            records: 0,
            phase: 0,
            failed: false,
        }
    }
    pub fn push(&mut self, message: &str) -> Option<bool> {
        if self.failed {
            return None;
        }
        let result = self.accept(message);
        if result.is_none() {
            self.failed = true;
        }
        result
    }
    fn accept(&mut self, message: &str) -> Option<bool> {
        if message.len() > MAX_SYNTAX_FRAME_BYTES || self.phase == 7 {
            return None;
        }
        self.bytes = self.bytes.checked_add(message.len())?;
        if self.bytes > MAX_ANALYSIS_MESSAGE_BYTES {
            return None;
        }
        let frame: Frame = serde_json::from_str(message).ok()?;
        if frame.version != SYNTAX_PROTOCOL_VERSION
            || frame.framing != SYNTAX_FRAME_VERSION
            || frame.ticket != self.ticket
            || frame.sequence != self.sequence
        {
            return None;
        }
        self.sequence = self.sequence.checked_add(1)?;
        if let Part::Begin { status, analysis } = frame.part {
            if self.phase != 0 || !valid_status(status, analysis.is_some()) {
                return None;
            }
            if let Some(header) = &analysis {
                if header.source.bytes() > MAX_STRUCTURE_BYTES {
                    return None;
                }
                self.records = header.folds.checked_add(header.rows.unwrap_or(0))?;
                if self.records > MAX_ANALYSIS_RECORDS {
                    return None;
                }
            }
            self.status = Some(status);
            self.header = analysis;
            self.phase = 1;
            return Some(false);
        }
        if self.phase == 0 {
            return None;
        }
        let Some(header) = self.header.as_ref() else {
            if !matches!(frame.part, Part::End) {
                return None;
            }
            self.phase = 7;
            return Some(true);
        };
        if self.phase == 1 && self.source.len() == header.source.bytes() {
            self.phase = 2;
        }
        if self.phase == 2 && self.folds.len() == header.folds {
            self.phase = 3;
        }
        if self.phase == 3 && (!header.structure || self.structure.complete()) {
            self.phase = 4;
        }
        if self.phase == 4 && self.rows.len() == header.rows.unwrap_or(0) && self.large.is_none() {
            self.phase = 6;
        }
        match frame.part {
            Part::Source(text) if self.phase == 1 => {
                if text.is_empty()
                    || text.len() > TEXT_BYTES
                    || self.source.len().checked_add(text.len())? > header.source.bytes()
                {
                    return None;
                }
                self.source.push_str(&text);
            }
            Part::Folds(folds) if self.phase == 2 => {
                if folds.is_empty()
                    || folds.len() > 64
                    || self.folds.len().checked_add(folds.len())? > header.folds
                {
                    return None;
                }
                self.folds.extend(folds);
            }
            Part::Structure(part) if self.phase == 3 => {
                self.structure
                    .push(part, MAX_ANALYSIS_RECORDS.checked_sub(self.records)?)?;
            }
            Part::Rows(rows) if self.phase == 4 && self.large.is_none() => {
                let cost = rows.iter().try_fold(rows.len(), |sum, row| {
                    sum.checked_add(match row {
                        TokenRowData::Spans(spans) => spans.len(),
                        TokenRowData::Previous { .. } => 0,
                    })
                })?;
                if rows.is_empty()
                    || cost > 64
                    || self.rows.len().checked_add(rows.len())? > header.rows?
                {
                    return None;
                }
                for row in &rows {
                    self.records = self.records.checked_add(row.records())?;
                }
                self.rows.extend(rows);
            }
            Part::Row { spans } if self.phase == 4 && self.large.is_none() => {
                if spans <= 63 || self.rows.len() >= header.rows? {
                    return None;
                }
                self.records = self.records.checked_add(spans)?;
                if self.records > MAX_ANALYSIS_RECORDS {
                    return None;
                }
                self.large = Some((spans, Vec::new()));
            }
            Part::Spans(spans) if self.phase == 4 => {
                let (count, values) = self.large.as_mut()?;
                if spans.is_empty()
                    || spans.len() > 64
                    || values.len().checked_add(spans.len())? > *count
                {
                    return None;
                }
                values.extend(spans);
                if values.len() == *count {
                    let (_, values) = self.large.take()?;
                    self.rows.push(TokenRowData::Spans(values));
                }
            }
            Part::End if self.phase == 6 => {
                self.phase = 7;
                return Some(true);
            }
            _ => return None,
        }
        let structural = self.structure.record_count().unwrap_or(0);
        if self.records.checked_add(structural)? > MAX_ANALYSIS_RECORDS {
            return None;
        }
        Some(false)
    }
    pub fn finish(self) -> Option<SyntaxReply> {
        if self.failed || self.phase != 7 {
            return None;
        }
        let analysis = match self.header {
            Some(header) => Some(SyntaxAnalysisData {
                source: match header.source {
                    SourceHeader::Full { .. } => SyntaxSource::Full(self.source),
                    SourceHeader::Replace { start, end, .. } => SyntaxSource::Replace {
                        start,
                        end,
                        text: self.source,
                    },
                },
                folds: self.folds,
                structure: if header.structure {
                    Some(self.structure.finish()?)
                } else {
                    None
                },
                highlights: header.rows.map(|_| self.rows),
                base_ticket: header.base_ticket,
            }),
            None => None,
        };
        Some(SyntaxReply {
            version: SYNTAX_PROTOCOL_VERSION,
            ticket: self.ticket,
            status: self.status?,
            analysis,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frames(reply: SyntaxReply) -> Vec<String> {
        let mut encoder = SyntaxReplyFrames::new(reply).unwrap();
        let mut frames = Vec::new();
        loop {
            match encoder.advance().unwrap() {
                SyntaxFrameProgress::Pending => {}
                SyntaxFrameProgress::Frame(frame) => {
                    assert!(frame.len() <= MAX_SYNTAX_FRAME_BYTES);
                    frames.push(frame);
                }
                SyntaxFrameProgress::Complete => return frames,
            }
        }
    }
    fn decode(messages: &[String], ticket: u32) -> Option<SyntaxReply> {
        let mut decoder = SyntaxReplyFrameDecoding::new(ticket);
        for (index, message) in messages.iter().enumerate() {
            if decoder.push(message)? && index + 1 != messages.len() {
                return None;
            }
        }
        decoder.finish()
    }
    fn compare(
        reply: SyntaxReply,
        source: &str,
        previous: Option<(u32, &SyntaxAnalysis)>,
    ) -> Vec<String> {
        let reference = serde_json::to_value(&reply).unwrap();
        let messages = frames(reply);
        let restored = decode(&messages, 43).unwrap();
        assert_eq!(serde_json::to_value(&restored).unwrap(), reference);
        let source = Arc::new(source.to_owned());
        let mut reception = restored
            .prepare_received_shared(43, source.clone(), previous)
            .unwrap();
        while !reception.advance() {}
        let analysis = reception.finish().unwrap().1.unwrap();
        assert!(Arc::ptr_eq(analysis.source_snapshot(), &source));
        messages
    }
    #[test]
    fn typed_frames_preserve_full_and_retained_reply_shapes() {
        for (language, source) in [
            (
                crate::highlight::Language::Rust,
                "fn main() { call(\"文😀\\\"\"); }\r\n".repeat(512),
            ),
            (
                crate::highlight::Language::Html,
                "<script>call();</script><style>a { color: red; }</style>\n".repeat(100),
            ),
            (
                crate::highlight::Language::Markdown,
                "```rust\nfn main() { call(\"文😀\"); }\n```\n".repeat(100),
            ),
        ] {
            let mut service = SyntaxPreparations::default();
            let old = service
                .prepare("frames".to_string(), language, &source, 4, || true)
                .1
                .unwrap();
            let revised = source.replacen("call", "next", 1);
            let current = service
                .prepare("frames".to_string(), language, &revised, 4, || true)
                .1
                .unwrap();
            for previous in [None, Some((42, old.as_ref()))] {
                let reply = SyntaxReply {
                    version: SYNTAX_PROTOCOL_VERSION,
                    ticket: 43,
                    status: SyntaxStatus::Ready { incremental: false },
                    analysis: current.transfer_data_reusing(previous),
                };
                assert!(compare(reply, &revised, previous).len() > 4);
            }
        }
    }
    #[test]
    fn frames_preserve_long_rows_dense_empty_rows_and_escaped_source() {
        for source in [
            "文😀\"\\\t\r".repeat(20_000),
            "\n".repeat(49_000),
            "x".repeat(8193),
            "\0".repeat(MAX_STRUCTURE_BYTES),
        ] {
            let highlights = if source.len() == 8193 {
                Some(vec![TokenRowData::Spans(
                    (1..=8193).map(|end| (end, TokenKind::Plain)).collect(),
                )])
            } else if source.starts_with('\n') {
                Some(
                    source
                        .split('\n')
                        .map(|_| TokenRowData::Spans(vec![]))
                        .collect(),
                )
            } else {
                None
            };
            let reply = SyntaxReply {
                version: SYNTAX_PROTOCOL_VERSION,
                ticket: 43,
                status: SyntaxStatus::Ready { incremental: false },
                analysis: Some(SyntaxAnalysisData {
                    source: SyntaxSource::Full(source.clone()),
                    folds: vec![],
                    structure: None,
                    highlights,
                    base_ticket: None,
                }),
            };
            let messages = compare(reply, &source, None);
            assert!(messages.len() > 2);
            assert!(messages.iter().map(String::len).sum::<usize>() <= MAX_ANALYSIS_MESSAGE_BYTES);
        }
    }
    #[test]
    fn missing_reordered_duplicate_wrong_owner_and_malformed_frames_are_rejected() {
        let reply = SyntaxReply {
            version: SYNTAX_PROTOCOL_VERSION,
            ticket: 43,
            status: SyntaxStatus::Ready { incremental: false },
            analysis: Some(SyntaxAnalysisData {
                source: SyntaxSource::Full("x".repeat(20_000)),
                folds: vec![],
                structure: None,
                highlights: None,
                base_ticket: None,
            }),
        };
        let messages = frames(reply);
        assert!(decode(&messages, 44).is_none());
        let mut wrong_version = serde_json::from_str::<serde_json::Value>(&messages[0]).unwrap();
        wrong_version["framing"] = serde_json::json!(SYNTAX_FRAME_VERSION + 1);
        assert!(
            SyntaxReplyFrameDecoding::new(43)
                .push(&wrong_version.to_string())
                .is_none()
        );
        for count in 0..messages.len() {
            assert!(decode(&messages[..count], 43).is_none());
        }
        let mut reordered = messages.clone();
        reordered.swap(1, 2);
        assert!(decode(&reordered, 43).is_none());
        let mut duplicate = messages.clone();
        duplicate.insert(1, messages[0].clone());
        assert!(decode(&duplicate, 43).is_none());
        for frame in [
            "{}".to_string(),
            "[1]".into(),
            "x".repeat(MAX_SYNTAX_FRAME_BYTES + 1),
        ] {
            let mut decoder = SyntaxReplyFrameDecoding::new(43);
            assert!(decoder.push(&frame).is_none());
            assert!(decoder.push(&messages[0]).is_none());
            assert!(decoder.finish().is_none());
        }
        let mut bad = serde_json::from_str::<serde_json::Value>(&messages[0]).unwrap();
        bad["part"]["Begin"]["analysis"]["folds"] = serde_json::json!(MAX_ANALYSIS_RECORDS + 1);
        let mut decoder = SyntaxReplyFrameDecoding::new(43);
        assert!(decoder.push(&bad.to_string()).is_none());
        let mut decoder = SyntaxReplyFrameDecoding::new(43);
        decoder.push(&messages[0]).unwrap();
        let mut bad = serde_json::from_str::<serde_json::Value>(&messages[1]).unwrap();
        bad["part"]["Source"] = serde_json::json!("x".repeat(TEXT_BYTES + 1));
        assert!(decoder.push(&bad.to_string()).is_none());
    }
    #[test]
    fn control_frames_are_transactional_and_invalid_status_shapes_fail() {
        for status in [
            SyntaxStatus::Cancelled,
            SyntaxStatus::TooLarge,
            SyntaxStatus::NeedsSource,
        ] {
            let reply = SyntaxReply {
                version: SYNTAX_PROTOCOL_VERSION,
                ticket: 43,
                status,
                analysis: None,
            };
            let messages = frames(reply);
            assert_eq!(messages.len(), 2);
            assert_eq!(decode(&messages, 43).unwrap().status, status);
        }
        assert!(
            SyntaxReplyFrames::new(SyntaxReply {
                version: SYNTAX_PROTOCOL_VERSION,
                ticket: 43,
                status: SyntaxStatus::Ready { incremental: false },
                analysis: None
            })
            .is_none()
        );
    }
}
