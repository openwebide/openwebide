//! Complete-only reply encoding across bounded text/record batches.
use super::{MAX_ANALYSIS_MESSAGE_BYTES, SyntaxSource, TokenRowData};
use crate::editor::{
    structure::StructureSerialization,
    syntax::{
        SyntaxReply,
        json::{self, JsonArray},
    },
};

const REPLY_TEXT_BATCH_BYTES: usize = 8 * 1024;

pub(in crate::editor::syntax) struct ReplyPublication {
    reply: SyntaxReply,
    message: String,
    phase: usize,
    source_offset: usize,
    array: JsonArray,
    structure: StructureSerialization,
    row: usize,
    spans: JsonArray,
    failed: bool,
    field_started: bool,
}
impl ReplyPublication {
    pub fn new(reply: SyntaxReply) -> Self {
        Self {
            reply,
            message: String::new(),
            phase: 0,
            source_offset: 0,
            array: JsonArray::default(),
            structure: StructureSerialization::default(),
            row: 0,
            spans: JsonArray::default(),
            failed: false,
            field_started: false,
        }
    }
    /// One unit escapes at most 8 KiB source bytes or serializes 64 fixed-size
    /// records. Metadata construction and String capacity growth remain separate.
    pub fn advance(&mut self, units: usize) -> bool {
        for _ in 0..units {
            if self.phase == 6 || self.failed {
                break;
            }
            self.step();
            self.failed |= self.message.len() > MAX_ANALYSIS_MESSAGE_BYTES;
        }
        self.failed || self.phase == 6
    }
    fn step(&mut self) {
        if self.phase == 0 {
            #[derive(serde::Serialize)]
            struct Header {
                version: u32,
                ticket: u32,
                status: crate::editor::SyntaxStatus,
            }
            json::value(
                &mut self.message,
                &Header {
                    version: self.reply.version,
                    ticket: self.reply.ticket,
                    status: self.reply.status,
                },
            );
            self.message.pop();
            self.message.push_str(",\"analysis\":");
            let Some(data) = &self.reply.analysis else {
                self.message.push_str("null}");
                self.phase = 6;
                return;
            };
            self.message.push_str("{\"source\":");
            match &data.source {
                SyntaxSource::Full(_) => self.message.push('"'),
                SyntaxSource::Replace { start, end, .. } => {
                    self.message.push_str("{\"start\":");
                    json::value(&mut self.message, start);
                    self.message.push_str(",\"end\":");
                    json::value(&mut self.message, end);
                    self.message.push_str(",\"text\":\"");
                }
            }
            self.phase = 1;
            return;
        }
        let data = self
            .reply
            .analysis
            .as_ref()
            .expect("analysis phases retain their data");
        let done = match self.phase {
            1 => {
                let text = match &data.source {
                    SyntaxSource::Full(text) | SyntaxSource::Replace { text, .. } => text,
                };
                let done = json::text(
                    &mut self.message,
                    text,
                    &mut self.source_offset,
                    REPLY_TEXT_BATCH_BYTES,
                );
                if done {
                    self.message.push('"');
                    if matches!(data.source, SyntaxSource::Replace { .. }) {
                        self.message.push('}');
                    }
                }
                done
            }
            2 => {
                if self.array.is_new() {
                    self.message.push_str(",\"folds\":");
                }
                self.array.advance(&mut self.message, &data.folds)
            }
            3 => {
                if !self.field_started {
                    self.message.push_str(",\"structure\":");
                    self.field_started = true;
                }
                if let Some(structure) = &data.structure {
                    self.structure.advance(&mut self.message, structure)
                } else {
                    self.message.push_str("null");
                    true
                }
            }
            4 => {
                if !self.field_started {
                    self.message.push_str(",\"highlights\":");
                    self.field_started = true;
                    if data.highlights.is_some() {
                        self.message.push('[');
                    }
                }
                if let Some(rows) = &data.highlights {
                    let mut budget = 64;
                    while self.row < rows.len() && budget > 0 {
                        if self.spans.is_new() && self.row > 0 {
                            self.message.push(',');
                        }
                        let done = match &rows[self.row] {
                            TokenRowData::Previous { .. } => {
                                json::value(&mut self.message, &rows[self.row]);
                                budget -= 1;
                                true
                            }
                            TokenRowData::Spans(spans) => {
                                let before = self.spans.offset();
                                let done =
                                    self.spans.advance_budget(&mut self.message, spans, budget);
                                budget -= (self.spans.offset() - before).max(1);
                                done
                            }
                        };
                        if !done {
                            break;
                        }
                        self.row += 1;
                        self.spans = JsonArray::default();
                    }
                    if self.row == rows.len() {
                        self.message.push(']');
                        true
                    } else {
                        false
                    }
                } else {
                    self.message.push_str("null");
                    true
                }
            }
            5 => {
                if let Some(ticket) = data.base_ticket {
                    self.message.push_str(",\"base_ticket\":");
                    json::value(&mut self.message, &ticket);
                }
                self.message.push_str("}}");
                true
            }
            _ => unreachable!("private reply phases are exhaustive"),
        };
        if done {
            self.phase += 1;
            self.array = JsonArray::default();
            self.field_started = false;
        }
    }
    pub fn finish(self) -> Option<(String, bool)> {
        (!self.failed && self.phase == 6).then_some((self.message, self.reply.analysis.is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        editor::{SYNTAX_PROTOCOL_VERSION, SyntaxPreparations, SyntaxStatus},
        highlight::{Language, TokenKind},
    };
    use std::sync::Arc;

    #[test]
    fn reply_batches_match_serde_bytes_for_full_and_retained_publications() {
        for (language, source) in [
            (
                Language::Rust,
                (r#"fn main() { let text = "文😀"; }"#.to_owned() + "\n").repeat(160),
            ),
            (
                Language::Html,
                "<main><script>let x = 1;</script><style>p { color: red; }</style></main>\n"
                    .repeat(8),
            ),
            (
                Language::Markdown,
                "# Heading\n\ntext 文😀\n\n```rust\nfn main() {}\n```\n".repeat(8),
            ),
        ] {
            let mut cache = SyntaxPreparations::default();
            let old = cache
                .prepare_shared("owner", language, Arc::new(source.clone()), 4, || true)
                .1
                .unwrap();
            for changed in [
                source.clone(),
                format!("\n{source}"),
                source.replace("main", "changed"),
            ] {
                let (_, analysis) =
                    cache.prepare_shared("owner", language, Arc::new(changed.clone()), 4, || true);
                let analysis = analysis.unwrap();
                for previous in [None, Some((42, old.as_ref()))] {
                    let reply = SyntaxReply {
                        version: SYNTAX_PROTOCOL_VERSION,
                        ticket: 43,
                        status: SyntaxStatus::Ready { incremental: false },
                        analysis: analysis.transfer_data_reusing(previous),
                    };
                    assert!(reply.analysis.is_some());
                    let expected = serde_json::to_string(&reply).unwrap();
                    let mut job = ReplyPublication::new(reply);
                    let mut turns = 0;
                    while !job.advance(1) {
                        turns += 1;
                        assert!(turns < 100_000);
                    }
                    assert!(turns > 8);
                    let (message, published) = job.finish().unwrap();
                    assert!(published);
                    assert_eq!(message, expected);
                    assert!(
                        SyntaxReply::receive_reusing(&message, 43, &changed, previous)
                            .unwrap()
                            .1
                            .is_some()
                    );
                }
            }
        }
        for status in [
            SyntaxStatus::Cancelled,
            SyntaxStatus::TooLarge,
            SyntaxStatus::NeedsSource,
        ] {
            let reply = SyntaxReply {
                version: SYNTAX_PROTOCOL_VERSION,
                ticket: 7,
                status,
                analysis: None,
            };
            let expected = serde_json::to_string(&reply).unwrap();
            let mut job = ReplyPublication::new(reply);
            assert!(job.advance(1));
            assert_eq!(job.finish().unwrap(), (expected, false));
        }
    }

    #[test]
    fn single_row_and_escaped_source_encoding_have_independent_batch_cursors() {
        let text = ["\0", "\"", "\\", "文😀"].concat().repeat(4000);
        let reply = SyntaxReply {
            version: SYNTAX_PROTOCOL_VERSION,
            ticket: 1,
            status: SyntaxStatus::Ready { incremental: false },
            analysis: Some(super::super::SyntaxAnalysisData {
                source: SyntaxSource::Full(text),
                folds: Vec::new(),
                structure: None,
                highlights: Some(vec![TokenRowData::Spans(
                    (1..=8193).map(|end| (end, TokenKind::Plain)).collect(),
                )]),
                base_ticket: None,
            }),
        };
        let expected = serde_json::to_string(&reply).unwrap();
        let mut job = ReplyPublication::new(reply);
        let mut source_batches = 0;
        let mut row_batches = 0;
        loop {
            let phase = job.phase;
            let before = job.source_offset;
            let spans = job.spans.offset();
            let done = job.advance(1);
            assert!(job.source_offset - before <= REPLY_TEXT_BATCH_BYTES);
            if phase == 1 {
                source_batches += 1;
            }
            if phase == 4 && job.row == 0 {
                assert!(job.spans.offset() - spans <= 64);
                row_batches += 1;
            }
            if done {
                break;
            }
        }
        assert!(source_batches > 2);
        assert!(row_batches > 100);
        assert_eq!(job.finish().unwrap().0, expected);
    }
}
