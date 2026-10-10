//! Incremental source comparison, copying and JSON escaping for worker requests.
use super::{SyntaxAnalysis, SyntaxRequest, SyntaxSource, transfer::SourcePublication};
use std::sync::Arc;

pub const SYNTAX_REQUEST_BATCH_BYTES: usize = 64 * 1024;

pub struct SyntaxRequestPublication {
    source: Arc<String>,
    previous: Option<Arc<SyntaxAnalysis>>,
    publication: Option<SourcePublication>,
    encoded_source: Option<SyntaxSource>,
    offset: usize,
    message: String,
    complete: bool,
}

impl SyntaxRequest {
    pub fn publication_shared(
        ticket: u32,
        document: String,
        language: crate::highlight::Language,
        source: Arc<String>,
        tab_width: usize,
        previous: Option<(u32, Arc<SyntaxAnalysis>)>,
    ) -> SyntaxRequestPublication {
        #[derive(serde::Serialize)]
        struct Header<'a> {
            version: u32,
            ticket: u32,
            document: &'a str,
            language: crate::highlight::Language,
            tab_width: usize,
            #[serde(skip_serializing_if = "Option::is_none")]
            base_ticket: Option<u32>,
        }
        let mut message = serde_json::to_string(&Header {
            version: super::SYNTAX_PROTOCOL_VERSION,
            ticket,
            document: &document,
            language,
            tab_width,
            base_ticket: previous.as_ref().map(|(ticket, _)| *ticket),
        })
        .expect("request header contains serializable primitives");
        message.pop();
        message.push_str(",\"source\":");
        SyntaxRequestPublication {
            publication: Some(SourcePublication::new(source.len(), previous.is_some())),
            source,
            previous: previous.map(|(_, analysis)| analysis),
            encoded_source: None,
            offset: 0,
            message,
            complete: false,
        }
    }
}

impl SyntaxRequestPublication {
    /// Each call compares/copies or escapes at most `budget` UTF-8 source bytes.
    /// Budgets of at least four bytes guarantee progress; allocation and the
    /// small request header remain runtime primitives.
    pub fn advance(&mut self, budget: usize) -> bool {
        if self.complete {
            return true;
        }
        if let Some(publication) = &mut self.publication {
            if !publication.advance(
                &self.source,
                self.previous.as_ref().map(|analysis| analysis.source()),
                budget,
            ) {
                return false;
            }
            let source = self.publication.take().unwrap().finish().unwrap();
            match &source {
                SyntaxSource::Full(_) => self.message.push('"'),
                SyntaxSource::Replace { start, end, .. } => {
                    use std::fmt::Write;
                    write!(
                        self.message,
                        "{{\"start\":{start},\"end\":{end},\"text\":\""
                    )
                    .expect("string formatting cannot fail");
                }
            }
            self.encoded_source = Some(source);
            return false;
        }
        let source = self
            .encoded_source
            .as_ref()
            .expect("published request source");
        let text = match source {
            SyntaxSource::Full(text) | SyntaxSource::Replace { text, .. } => text,
        };
        let end = text.floor_char_boundary(text.len().min(self.offset.saturating_add(budget)));
        let escaped =
            serde_json::to_string(&text[self.offset..end]).expect("source text is serializable");
        self.message.push_str(&escaped[1..escaped.len() - 1]);
        self.offset = end;
        if end == text.len() {
            self.message.push('"');
            if matches!(source, SyntaxSource::Replace { .. }) {
                self.message.push('}');
            }
            self.message.push('}');
            self.complete = true;
        }
        self.complete
    }

    pub fn finish(self) -> Option<String> {
        self.complete.then_some(self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{editor::SyntaxPreparations, highlight::Language};

    #[test]
    fn request_batches_match_wire_policy_for_unicode_escaping_and_retained_deltas() {
        let old = Arc::new("fn main() {}\n// 文😀\\\"\t\r\n".repeat(100));
        let mut cache = SyntaxPreparations::default();
        let (_, previous) =
            cache.prepare_shared("owner".to_owned(), Language::Rust, old.clone(), 4, || true);
        let previous = previous.unwrap();
        for source in [
            old.clone(),
            Arc::new(old.replace("main", "new_name")),
            Arc::new("\u{0000}\"\\文😀\r\n\t".into()),
            Arc::new(String::new()),
        ] {
            for base in [None, Some((7, previous.clone()))] {
                let expected = SyntaxRequest::new(
                    9,
                    "owner:\"文\\".into(),
                    Language::Rust,
                    &source,
                    4,
                    base.as_ref()
                        .map(|(ticket, analysis)| (*ticket, analysis.as_ref())),
                );
                for budget in [4, 7, 8192, SYNTAX_REQUEST_BATCH_BYTES] {
                    let mut job = SyntaxRequest::publication_shared(
                        9,
                        expected.document.clone(),
                        expected.language,
                        source.clone(),
                        4,
                        base.clone(),
                    );
                    assert!(!job.advance(0));
                    let mut turns = 0;
                    while !job.advance(budget) {
                        turns += 1;
                        assert!(turns < 100_000);
                    }
                    let message = job.finish().unwrap();
                    assert_eq!(
                        serde_json::from_str::<serde_json::Value>(&message).unwrap(),
                        serde_json::to_value(&expected).unwrap()
                    );
                    let reply = cache.handle_message(&message, || true).unwrap();
                    let (status, _) =
                        super::super::SyntaxReply::receive(&reply, 9, &source).unwrap();
                    assert!(matches!(
                        status,
                        super::super::SyntaxStatus::Ready { .. }
                            | super::super::SyntaxStatus::NeedsSource
                    ));
                }
            }
        }
        let mut unfinished =
            SyntaxRequest::publication_shared(1, "owner".into(), Language::Rust, old, 4, None);
        assert!(!unfinished.advance(4));
        assert!(unfinished.finish().is_none());
    }
}
