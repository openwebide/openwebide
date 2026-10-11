//! One preparation service used behind synchronous and worker transport adapters.
use super::{
    MAX_ANALYSIS_MESSAGE_BYTES, SyntaxAnalysisData, SyntaxPreparations, SyntaxSource, SyntaxStatus,
};
use crate::{editor::MAX_STRUCTURE_BYTES, highlight::Language};

pub const SYNTAX_PROTOCOL_VERSION: u32 = 6;
pub const SYNTAX_BATCH_MS: u32 = 100;
/// Main-thread parsing must yield frequently enough for input and visible paint.
pub const SYNTAX_INPUT_BUDGET_MS: u32 = 12;
pub const MAX_SYNTAX_REQUEST_BYTES: usize = MAX_STRUCTURE_BYTES * 6 + 8192;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxRequest {
    pub version: u32,
    pub ticket: u32,
    pub document: String,
    pub language: Language,
    pub source: SyntaxSource,
    pub tab_width: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ticket: Option<u32>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxReply {
    pub version: u32,
    pub ticket: u32,
    pub status: SyntaxStatus,
    pub analysis: Option<SyntaxAnalysisData>,
}

impl SyntaxRequest {
    pub fn new(
        ticket: u32,
        document: String,
        language: Language,
        source: &str,
        tab_width: usize,
        previous: Option<(u32, &super::SyntaxAnalysis)>,
    ) -> Self {
        Self {
            version: SYNTAX_PROTOCOL_VERSION,
            ticket,
            document,
            language,
            tab_width,
            source: SyntaxSource::publication(source, previous.map(|(_, value)| value.source())),
            base_ticket: previous.map(|(ticket, _)| ticket),
        }
    }
}

fn decode_request(message: &str) -> Option<SyntaxRequest> {
    if message.len() > MAX_SYNTAX_REQUEST_BYTES {
        return None;
    }
    let request: SyntaxRequest = serde_json::from_str(message).ok()?;
    (request.version == SYNTAX_PROTOCOL_VERSION
        && !request.document.is_empty()
        && request.document.len() <= 4096
        && (1..=16).contains(&request.tab_width))
    .then_some(request)
}
fn control_reply(ticket: u32, status: SyntaxStatus) -> Option<String> {
    serde_json::to_string(&SyntaxReply {
        version: SYNTAX_PROTOCOL_VERSION,
        ticket,
        status,
        analysis: None,
    })
    .ok()
}
struct SyntaxTask {
    ticket: u32,
    key: String,
    language: Language,
    tab_width: usize,
    source: Option<std::sync::Arc<String>>,
    resolution: Option<super::transfer::SourceResolution>,
    source_length: usize,
    previous: Option<(u32, std::sync::Arc<super::SyntaxAnalysis>)>,
    change: Option<crate::editor::TextChange>,
    document: super::SyntaxDocument,
    publication: Option<super::transfer::SourcePublication>,
    prepared: Option<(SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>)>,
    reply_source: Option<SyntaxSource>,
    metadata: Option<super::transfer::AnalysisTransferPreparation>,
    encoding: Option<super::transfer::ReplyPublication>,
    ready_reply: Option<(SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>)>,
    encoded_reply: Option<(String, bool)>,
    full_retry: bool,
}
enum Start {
    Task(Box<SyntaxTask>),
    Reply(String),
}

impl SyntaxPreparations<String> {
    /// The synchronous adapter drives the same source-bound task without yielding.
    pub fn handle_message(
        &mut self,
        message: &str,
        mut should_continue: impl FnMut() -> bool,
    ) -> Option<String> {
        let request = decode_request(message)?;
        match self.begin_request(request, &mut should_continue)? {
            Start::Reply(reply) => Some(reply),
            Start::Task(mut task) => {
                let result = task
                    .advance(&mut should_continue, &mut || false)
                    .expect("synchronous syntax task does not yield");
                task.finish(self, result)
            }
        }
    }
    fn begin_request(
        &mut self,
        request: SyntaxRequest,
        should_continue: &mut impl FnMut() -> bool,
    ) -> Option<Start> {
        let previous = self
            .previous_publication(&request.document)
            .filter(|(ticket, _)| request.base_ticket == Some(*ticket));
        if matches!(request.source, SyntaxSource::Replace { .. }) {
            request.base_ticket?;
            if previous.is_none() {
                return control_reply(request.ticket, SyntaxStatus::NeedsSource).map(Start::Reply);
            }
        }
        let base_source = previous.as_ref().map(|(_, analysis)| analysis.source());
        let length = request.source.result_length(base_source)?;
        if length > MAX_STRUCTURE_BYTES {
            self.remove(&request.document);
            return control_reply(request.ticket, SyntaxStatus::TooLarge).map(Start::Reply);
        }
        if !should_continue() {
            self.remove(&request.document);
            return control_reply(request.ticket, SyntaxStatus::Cancelled).map(Start::Reply);
        }
        let change = match &request.source {
            SyntaxSource::Full(_) => None,
            SyntaxSource::Replace { start, end, text } => Some(crate::editor::TextChange {
                range: *start..*end,
                new_end: start.checked_add(text.len())?,
            }),
        };
        let resolution = request.source.prepare_resolution(
            previous
                .as_ref()
                .map(|(_, analysis)| analysis.source_snapshot().clone()),
        )?;
        let document = self.take_document(&request.document, request.language)?;
        Some(Start::Task(Box::new(SyntaxTask {
            ticket: request.ticket,
            key: request.document,
            language: request.language,
            tab_width: request.tab_width,
            source: None,
            resolution: Some(resolution),
            source_length: length,
            previous,
            change,
            document,
            publication: None,
            prepared: None,
            reply_source: None,
            metadata: None,
            encoding: None,
            ready_reply: None,
            encoded_reply: None,
            full_retry: false,
        })))
    }
}
impl SyntaxTask {
    fn advance(
        &mut self,
        should_continue: &mut impl FnMut() -> bool,
        should_yield: &mut impl FnMut() -> bool,
    ) -> Option<(SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>)> {
        if self.ready_reply.is_none() {
            let result = self.advance_analysis(should_continue, should_yield)?;
            self.metadata = result.1.as_ref().map(|analysis| {
                super::transfer::AnalysisTransferPreparation::new(
                    analysis,
                    self.previous
                        .as_ref()
                        .map(|(ticket, old)| (*ticket, old.as_ref())),
                    self.reply_source.take(),
                )
            });
            if self.metadata.is_none() {
                self.encoding = Some(super::transfer::ReplyPublication::new(reply_data(
                    self.ticket,
                    result.0,
                    None,
                )));
            }
            self.ready_reply = Some(result);
        }
        loop {
            if !should_continue() {
                return Some((SyntaxStatus::Cancelled, None));
            }
            if should_yield() {
                return None;
            }
            if let Some(work) = &mut self.metadata {
                let result = self.ready_reply.as_ref()?;
                let analysis = result.1.as_ref()?;
                let previous = (!self.full_retry)
                    .then_some(self.previous.as_ref())
                    .flatten()
                    .map(|(ticket, old)| (*ticket, old.as_ref()));
                if !work.advance(analysis, previous) {
                    continue;
                }
                let data = self.metadata.take()?.finish(analysis, previous);
                if data.is_none() && !self.full_retry {
                    self.full_retry = true;
                    self.metadata = Some(super::transfer::AnalysisTransferPreparation::new(
                        analysis, None, None,
                    ));
                    continue;
                }
                self.encoding = Some(super::transfer::ReplyPublication::new(reply_data(
                    self.ticket,
                    result.0,
                    data,
                )));
                continue;
            }
            if !self.encoding.as_mut()?.advance(1) {
                continue;
            }
            if let Some(encoded) = self.encoding.take()?.finish() {
                self.encoded_reply = Some(encoded);
                return self.ready_reply.take();
            }
            // Preserve the wire's single full-publication retry after overflow.
            if self.full_retry {
                return self.ready_reply.take();
            }
            self.full_retry = true;
            let result = self.ready_reply.as_ref()?;
            if let Some(analysis) = &result.1 {
                self.metadata = Some(super::transfer::AnalysisTransferPreparation::new(
                    analysis, None, None,
                ));
            } else {
                self.encoding = Some(super::transfer::ReplyPublication::new(reply_data(
                    self.ticket,
                    result.0,
                    None,
                )));
            }
        }
    }

    fn advance_analysis(
        &mut self,
        should_continue: &mut impl FnMut() -> bool,
        should_yield: &mut impl FnMut() -> bool,
    ) -> Option<(SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>)> {
        if let Some(resolution) = &mut self.resolution {
            loop {
                if !should_continue() {
                    return Some((SyntaxStatus::Cancelled, None));
                }
                if resolution.is_ready() {
                    break;
                }
                if should_yield() {
                    return None;
                }
                resolution.advance(crate::highlight::LEXICAL_BATCH_BYTES);
            }
            self.source = self.resolution.take().unwrap().finish();
        }
        let change = self
            .previous
            .as_ref()
            .zip(self.change.as_ref())
            .map(|((_, analysis), change)| (analysis.source_snapshot(), change));
        if self.prepared.is_some() {
            return self.advance_publication(should_continue, should_yield);
        }
        let result = self.document.prepare_resolved_cooperative(
            self.source.as_ref()?.clone(),
            self.tab_width,
            should_continue,
            should_yield,
            change,
        )?;
        if let Some(analysis) = &result.1 {
            self.publication = Some(super::transfer::SourcePublication::new(
                analysis.source().len(),
                self.previous.is_some(),
            ));
            self.prepared = Some(result);
            self.advance_publication(should_continue, should_yield)
        } else {
            Some(result)
        }
    }

    fn advance_publication(
        &mut self,
        should_continue: &mut impl FnMut() -> bool,
        should_yield: &mut impl FnMut() -> bool,
    ) -> Option<(SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>)> {
        loop {
            if !should_continue() {
                return Some((SyntaxStatus::Cancelled, None));
            }
            if should_yield() {
                return None;
            }
            let source = self.prepared.as_ref()?.1.as_ref()?.source();
            let previous = self
                .previous
                .as_ref()
                .map(|(_, analysis)| analysis.source());
            if self.publication.as_mut()?.advance(
                source,
                previous,
                crate::highlight::LEXICAL_BATCH_BYTES,
            ) {
                self.reply_source = self.publication.take()?.finish();
                return self.prepared.take();
            }
        }
    }
    fn finish(
        mut self,
        service: &mut SyntaxPreparations<String>,
        result: (SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>),
    ) -> Option<String> {
        let (status, prepared) = result;
        let (message, published) = match self.encoded_reply.take() {
            Some(encoded) => encoded,
            None if prepared.is_none() => (control_reply(self.ticket, status)?, false),
            None => return None,
        };
        if let Some(prepared) = prepared {
            if published {
                self.document.publication = Some((self.ticket, prepared));
            }
            service.install_document(self.key, self.language, self.document, self.source_length);
        }
        Some(message)
    }
}
#[cfg(test)]
fn shape_reply_data(
    ticket: u32,
    status: SyntaxStatus,
    prepared: Option<&std::sync::Arc<super::SyntaxAnalysis>>,
    previous: Option<&(u32, std::sync::Arc<super::SyntaxAnalysis>)>,
    source: Option<SyntaxSource>,
) -> SyntaxReply {
    let analysis = prepared.and_then(|value| {
        let previous = previous.map(|(ticket, analysis)| (*ticket, analysis.as_ref()));
        source
            .map_or_else(
                || value.transfer_data_reusing(previous),
                |source| value.transfer_data_with_source(previous, source),
            )
            .or_else(|| value.transfer_data())
    });
    reply_data(ticket, status, analysis)
}
fn reply_data(
    ticket: u32,
    mut status: SyntaxStatus,
    analysis: Option<SyntaxAnalysisData>,
) -> SyntaxReply {
    if matches!(status, SyntaxStatus::Ready { .. }) && analysis.is_none() {
        status = SyntaxStatus::TooLarge;
    }
    SyntaxReply {
        version: SYNTAX_PROTOCOL_VERSION,
        ticket,
        status,
        analysis,
    }
}

/// Bounded message admission and parser continuation shared above the worker runtime.
#[derive(Default)]
pub struct SyntaxWorker {
    service: SyntaxPreparations<String>,
    queued: std::collections::VecDeque<SyntaxRequest>,
    active: Option<Box<SyntaxTask>>,
}
impl SyntaxWorker {
    pub fn has_work(&self) -> bool {
        self.active.is_some() || !self.queued.is_empty()
    }
    pub fn retained_request_bytes(&self) -> usize {
        self.active.as_ref().map_or(0, |task| task.source_length)
            + self.queued.iter().map(request_bytes).sum::<usize>()
    }
    pub fn enqueue(&mut self, message: &str) -> Option<String> {
        let request = decode_request(message)?;
        let source_bytes = match &request.source {
            SyntaxSource::Full(source) => source.len(),
            SyntaxSource::Replace { text, .. } => text.len(),
        };
        if source_bytes > MAX_STRUCTURE_BYTES {
            self.service.remove(&request.document);
            return control_reply(request.ticket, SyntaxStatus::TooLarge);
        }
        if self.queued.len() + usize::from(self.active.is_some()) >= super::MAX_SYNTAX_DOCUMENTS
            || self
                .retained_request_bytes()
                .saturating_add(request_bytes(&request))
                > super::MAX_SYNTAX_SOURCE_BYTES
        {
            self.service.remove(&request.document);
            return control_reply(request.ticket, SyntaxStatus::Cancelled);
        }
        self.queued.push_back(request);
        None
    }
    pub fn advance(
        &mut self,
        mut should_continue: impl FnMut() -> bool,
        mut should_yield: impl FnMut() -> bool,
    ) -> Option<String> {
        if self.active.is_none() {
            let request = self.queued.pop_front()?;
            match self.service.begin_request(request, &mut should_continue)? {
                Start::Reply(reply) => return Some(reply),
                Start::Task(task) => {
                    let queued_bytes = self.queued.iter().map(request_bytes).sum::<usize>();
                    if queued_bytes.saturating_add(task.source_length)
                        > super::MAX_SYNTAX_SOURCE_BYTES
                    {
                        return control_reply(task.ticket, SyntaxStatus::Cancelled);
                    }
                    self.active = Some(task);
                }
            }
        }
        let result = self
            .active
            .as_mut()?
            .advance(&mut should_continue, &mut should_yield)?;
        self.active.take()?.finish(&mut self.service, result)
    }
}
fn request_bytes(request: &SyntaxRequest) -> usize {
    request.document.len()
        + match &request.source {
            SyntaxSource::Full(source) => source.len(),
            SyntaxSource::Replace { text, .. } => text.len(),
        }
}

impl SyntaxReply {
    /// Read transport metadata without scanning an analysis payload. Shared
    /// producers serialize version/ticket first; routing inspects at most 256
    /// bytes. This is not reply validation: callers must still use `receive`.
    pub fn routing_ticket(message: &str) -> Option<u32> {
        if message.len() > MAX_ANALYSIS_MESSAGE_BYTES {
            return None;
        }
        struct Ticket<'a>(&'a mut Option<u32>);
        impl<'de> serde::de::Visitor<'de> for Ticket<'_> {
            type Value = ();

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a syntax reply object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                while let Some(key) = map.next_key::<String>()? {
                    if key == "ticket" {
                        *self.0 = Some(map.next_value()?);
                        // Deliberately stop before status/analysis. The complete
                        // decoder validates their syntax and ownership later.
                        return Err(serde::de::Error::custom("routing ticket extracted"));
                    }
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
                Ok(())
            }
        }
        let mut ticket = None;
        let prefix = &message.as_bytes()[..message.len().min(256)];
        let mut decoder = serde_json::Deserializer::from_slice(prefix);
        let _ = serde::Deserializer::deserialize_map(&mut decoder, Ticket(&mut ticket));
        ticket
    }

    pub fn receive(
        message: &str,
        ticket: u32,
        source: &str,
    ) -> Option<(SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>)> {
        Self::receive_reusing(message, ticket, source, None)
    }
    pub fn receive_reusing(
        message: &str,
        ticket: u32,
        source: &str,
        previous: Option<(u32, &super::SyntaxAnalysis)>,
    ) -> Option<(SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>)> {
        Self::receive_source(
            message,
            ticket,
            || std::sync::Arc::new(source.to_owned()),
            previous,
        )
    }

    /// Retain the facade's immutable snapshot after validating the worker's source.
    pub fn receive_shared(
        message: &str,
        ticket: u32,
        source: std::sync::Arc<String>,
        previous: Option<(u32, &super::SyntaxAnalysis)>,
    ) -> Option<(SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>)> {
        Self::receive_source(message, ticket, || source, previous)
    }

    fn receive_source(
        message: &str,
        ticket: u32,
        source: impl FnOnce() -> std::sync::Arc<String>,
        previous: Option<(u32, &super::SyntaxAnalysis)>,
    ) -> Option<(SyntaxStatus, Option<std::sync::Arc<super::SyntaxAnalysis>>)> {
        if message.len() > MAX_ANALYSIS_MESSAGE_BYTES {
            return None;
        }
        let reply: Self = serde_json::from_str(message).ok()?;
        if reply.version != SYNTAX_PROTOCOL_VERSION || reply.ticket != ticket {
            return None;
        }
        let analysis = match (reply.status, reply.analysis) {
            (SyntaxStatus::Ready { .. }, Some(data)) => {
                Some(data.validate_shared(source(), previous)?)
            }
            (
                SyntaxStatus::Cancelled | SyntaxStatus::TooLarge | SyntaxStatus::NeedsSource,
                None,
            ) => None,
            _ => return None,
        };
        Some((reply.status, analysis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_during_reply_encoding_never_publishes_partial_analysis() {
        let mut service = SyntaxPreparations::default();
        let source = "fn main() { let value = 1; }\n".repeat(64);
        let other = serde_json::to_string(&SyntaxRequest::new(
            40,
            "other".into(),
            Language::Rust,
            &source,
            4,
            None,
        ))
        .unwrap();
        assert!(service.handle_message(&other, || true).is_some());
        let request = SyntaxRequest::new(43, "owner".into(), Language::Rust, &source, 4, None);
        let Start::Task(mut task) = service.begin_request(request, &mut || true).unwrap() else {
            panic!("valid source must create a task");
        };
        let result = task.advance_analysis(&mut || true, &mut || false).unwrap();
        assert!(result.1.is_some());
        let data = shape_reply_data(
            task.ticket,
            result.0,
            result.1.as_ref(),
            task.previous.as_ref(),
            task.reply_source.take(),
        );
        task.encoding = Some(super::super::transfer::ReplyPublication::new(data));
        task.ready_reply = Some(result);
        assert!(!task.encoding.as_mut().unwrap().advance(1));
        assert!(task.encoded_reply.is_none());
        let result = task.advance(&mut || false, &mut || false).unwrap();
        assert_eq!(result.0, SyntaxStatus::Cancelled);
        assert!(result.1.is_none());
        let message = task.finish(&mut service, result).unwrap();
        assert_eq!(
            SyntaxReply::receive(&message, 43, &source).unwrap().0,
            SyntaxStatus::Cancelled
        );
        assert!(service.previous_publication(&"owner".to_owned()).is_none());
        assert!(service.previous_publication(&"other".to_owned()).is_some());
        assert_eq!(service.len(), 1);
    }

    #[test]
    fn cancellation_during_metadata_preparation_never_changes_publication_owners() {
        let mut service = SyntaxPreparations::default();
        let source = "fn main() { let value = 1; }\n".repeat(2000);
        let request = |ticket, owner: &str| {
            SyntaxRequest::new(ticket, owner.into(), Language::Rust, &source, 4, None)
        };
        service
            .handle_message(
                &serde_json::to_string(&request(1, "other")).unwrap(),
                || true,
            )
            .unwrap();
        let Start::Task(mut task) = service
            .begin_request(request(2, "owner"), &mut || true)
            .unwrap()
        else {
            panic!("expected preparation task")
        };
        let result = task.advance_analysis(&mut || true, &mut || false).unwrap();
        let analysis = result.1.as_ref().unwrap();
        let mut metadata = super::super::transfer::AnalysisTransferPreparation::new(
            analysis,
            None,
            task.reply_source.take(),
        );
        assert!(!metadata.advance(analysis, None));
        task.metadata = Some(metadata);
        task.ready_reply = Some(result);
        assert!(task.encoding.is_none());
        let cancelled = task.advance(&mut || false, &mut || false).unwrap();
        assert_eq!(cancelled.0, SyntaxStatus::Cancelled);
        let message = task.finish(&mut service, cancelled).unwrap();
        assert_eq!(
            SyntaxReply::receive(&message, 2, &source).unwrap().0,
            SyntaxStatus::Cancelled
        );
        assert!(service.previous_publication(&"owner".into()).is_none());
        assert!(service.previous_publication(&"other".into()).is_some());
        assert_eq!(service.len(), 1);
    }

    #[test]
    fn reply_routing_is_bounded_and_does_not_replace_validation() {
        for ticket in [0, 1, u32::MAX] {
            for status in [
                SyntaxStatus::Cancelled,
                SyntaxStatus::TooLarge,
                SyntaxStatus::NeedsSource,
                SyntaxStatus::Ready { incremental: false },
            ] {
                let message = control_reply(ticket, status).unwrap();
                assert_eq!(SyntaxReply::routing_ticket(&message), Some(ticket));
            }
        }
        for message in [
            r#" { "version":6, "ticket":42, "analysis":null }"#,
            r#"{"ticket":42,"version":6}"#,
            r#"{"analysis":{"ticket":7},"ticket":42}"#,
            r#"{"\u0074icket":42}"#,
        ] {
            assert_eq!(SyntaxReply::routing_ticket(message), Some(42));
        }
        for message in [
            r#"{"ticket":-1}"#,
            r#"{"ticket":4294967296}"#,
            r#"{"ticket":"42"}"#,
            r#"[{"ticket":42}]"#,
            r#"{"analysis":{"ticket":42}}"#,
        ] {
            assert_eq!(SyntaxReply::routing_ticket(message), None);
        }
        // A large or malformed tail cannot be scanned during routing. Complete
        // validation must still reject it before publishing any analysis.
        let message = format!(
            "{{\"version\":6,\"ticket\":42,{}",
            "x".repeat(2 * 1024 * 1024)
        );
        assert_eq!(SyntaxReply::routing_ticket(&message), Some(42));
        assert!(SyntaxReply::receive(&message, 42, "source").is_none());
        let late = format!("{{\"analysis\":\"{}\",\"ticket\":42}}", "x".repeat(1024));
        assert_eq!(SyntaxReply::routing_ticket(&late), None);
        let duplicate =
            r#"{"version":6,"ticket":42,"ticket":7,"status":"Cancelled","analysis":null}"#;
        assert_eq!(SyntaxReply::routing_ticket(duplicate), Some(42));
        assert!(SyntaxReply::receive(duplicate, 42, "source").is_none());
    }

    #[test]
    fn short_worker_slices_make_progress_through_parser_injections_and_final_structure() {
        for ending in ["\n", "\r\n"] {
            let source = format!(
                "<script>{ending}{}{}</script>",
                " ".repeat(100_000),
                format!("call(1);{ending}").repeat(4000)
            );
            let request =
                SyntaxRequest::new(1, "short-slices".into(), Language::Html, &source, 4, None);
            let message = serde_json::to_string(&request).unwrap();
            let mut worker = SyntaxWorker::default();
            assert!(worker.enqueue(&message).is_none());
            let mut turns = 0;
            let reply = loop {
                turns += 1;
                assert!(turns < 10_000);
                let mut checks = 0;
                if let Some(reply) = worker.advance(
                    || true,
                    || {
                        checks += 1;
                        checks >= 2
                    },
                ) {
                    break reply;
                }
                assert!(worker.has_work());
            };
            assert!(turns > 100);
            let (status, analysis) = SyntaxReply::receive(&reply, 1, &source).unwrap();
            assert_eq!(status, SyntaxStatus::Ready { incremental: false });
            assert_eq!(
                analysis
                    .as_ref()
                    .unwrap()
                    .structure()
                    .unwrap()
                    .brackets
                    .len(),
                8000
            );
            let expected = SyntaxPreparations::default()
                .handle_message(&message, || true)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&reply).unwrap(),
                serde_json::from_str::<serde_json::Value>(&expected).unwrap()
            );
        }
    }

    #[test]
    fn warm_worker_source_reconstruction_yields_before_parser_and_releases_cancelled_work() {
        for ending in ["\n", "\r\n"] {
            for cancel in [false, true] {
                let source = format!(
                    "/*{}*/{ending}{}fn original() {{}}{ending}",
                    "文😀 ".repeat(20_000),
                    ending.repeat(20_000)
                );
                let mut worker = SyntaxWorker::default();
                let initial =
                    SyntaxRequest::new(1, "resolved".into(), Language::Rust, &source, 4, None);
                let initial_message = serde_json::to_string(&initial).unwrap();
                let initial_reply = worker
                    .service
                    .handle_message(&initial_message, || true)
                    .unwrap();
                let previous = SyntaxReply::receive(&initial_reply, 1, &source)
                    .unwrap()
                    .1
                    .unwrap();
                let changed = format!("// 文😀{ending}{source}");
                let request = SyntaxRequest::new(
                    2,
                    "resolved".into(),
                    Language::Rust,
                    &changed,
                    4,
                    Some((1, &previous)),
                );
                assert!(matches!(request.source, SyntaxSource::Replace { .. }));
                let message = serde_json::to_string(&request).unwrap();
                assert!(worker.enqueue(&message).is_none());
                let mut checks = 0;
                assert!(
                    worker
                        .advance(
                            || true,
                            || {
                                checks += 1;
                                checks >= 2
                            }
                        )
                        .is_none()
                );
                let task = worker.active.as_ref().unwrap();
                assert!(
                    task.source.is_none(),
                    "an incomplete source cannot reach parsing"
                );
                assert!(!task.resolution.as_ref().unwrap().is_ready());
                assert_eq!(task.document.text.as_str(), source);
                assert_eq!(worker.retained_request_bytes(), changed.len());
                let reply = worker.advance(|| !cancel, || false).unwrap();
                assert!(!worker.has_work());
                assert_eq!(worker.retained_request_bytes(), 0);
                let (status, prepared) =
                    SyntaxReply::receive_reusing(&reply, 2, &changed, Some((1, &previous)))
                        .unwrap();
                if cancel {
                    assert_eq!(status, SyntaxStatus::Cancelled);
                    assert!(prepared.is_none());
                } else {
                    assert!(matches!(status, SyntaxStatus::Ready { .. }));
                    let (_, expected) = super::super::SyntaxDocument::new(Language::Rust)
                        .unwrap()
                        .prepare(&changed, 4, || true);
                    assert_eq!(
                        serde_json::to_value(prepared.unwrap().transfer_data()).unwrap(),
                        serde_json::to_value(expected.unwrap().transfer_data()).unwrap()
                    );
                }
                let recovered = SyntaxRequest::new(
                    3,
                    "resolved".into(),
                    Language::Rust,
                    "fn recovered() {}",
                    4,
                    None,
                );
                assert!(
                    worker
                        .enqueue(&serde_json::to_string(&recovered).unwrap())
                        .is_none()
                );
                let reply = worker.advance(|| true, || false).unwrap();
                assert!(
                    SyntaxReply::receive(&reply, 3, "fn recovered() {}")
                        .unwrap()
                        .1
                        .is_some()
                );
            }
        }
    }

    #[test]
    fn synchronous_and_yielding_message_adapters_publish_identical_results() {
        for (language, source) in [
            (Language::Sql, "SELECT '文😀';\r\n".repeat(1_000)),
            (
                Language::Rust,
                "fn main() {\r\n call(\"文😀\");\r\n}\r\n".repeat(300),
            ),
            (
                Language::Html,
                "<script>call(\"文😀\");</script><style>a {color:red}</style>".to_owned(),
            ),
            (
                Language::Markdown,
                "**文😀** and `code`.\r\n\r\n".repeat(1_000),
            ),
        ] {
            let mut synchronous = SyntaxPreparations::default();
            let mut worker = SyntaxWorker::default();
            let mut previous = None::<(u32, std::sync::Arc<super::super::SyntaxAnalysis>)>;
            for (index, text) in [source.clone(), source.replacen("文😀", "changed 文😀", 1)]
                .into_iter()
                .enumerate()
            {
                let ticket = u32::try_from(index + 1).unwrap();
                let request = SyntaxRequest::new(
                    ticket,
                    "document".into(),
                    language,
                    &text,
                    4,
                    previous
                        .as_ref()
                        .map(|(ticket, analysis)| (*ticket, analysis.as_ref())),
                );
                let message = serde_json::to_string(&request).unwrap();
                let expected = synchronous.handle_message(&message, || true).unwrap();
                assert!(worker.enqueue(&message).is_none());
                let mut batches = 0;
                let actual = loop {
                    let mut checks = 0;
                    let reply = worker.advance(
                        || true,
                        || {
                            checks += 1;
                            checks >= 8
                        },
                    );
                    batches += 1;
                    assert!(batches < 10_000);
                    if let Some(reply) = reply {
                        break reply;
                    }
                    assert!(worker.has_work());
                };
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&actual).unwrap(),
                    serde_json::from_str::<serde_json::Value>(&expected).unwrap()
                );
                let (status, analysis) = SyntaxReply::receive_reusing(
                    &actual,
                    ticket,
                    &text,
                    previous
                        .as_ref()
                        .map(|(ticket, analysis)| (*ticket, analysis.as_ref())),
                )
                .unwrap();
                assert!(matches!(status, SyntaxStatus::Ready { .. }));
                previous = Some((ticket, analysis.unwrap()));
                assert!(!worker.has_work());
                assert_eq!(worker.retained_request_bytes(), 0);
            }
        }
    }

    #[test]
    fn pending_source_publication_cancels_without_installing_a_partial_base() {
        for ending in ["\n", "\r\n"] {
            let source = format!(
                "const VALUE: &str = \"{}\";{ending}",
                "文😀".repeat(149_000)
            );
            let request =
                SyntaxRequest::new(900, "publication".into(), Language::Rust, &source, 4, None);
            let message = serde_json::to_string(&request).unwrap();
            let mut worker = SyntaxWorker::default();
            assert!(worker.enqueue(&message).is_none());
            let mut turns = 0;
            loop {
                let mut checks = 0;
                assert!(
                    worker
                        .advance(
                            || true,
                            || {
                                checks += 1;
                                checks >= 2
                            }
                        )
                        .is_none()
                );
                turns += 1;
                assert!(turns < 10_000);
                if worker
                    .active
                    .as_ref()
                    .is_some_and(|task| task.publication.is_some())
                {
                    break;
                }
            }
            let task = worker.active.as_ref().unwrap();
            let prepared = task.prepared.as_ref().unwrap().1.as_ref().unwrap();
            let weak = std::sync::Arc::downgrade(prepared.source_snapshot());
            assert!(task.reply_source.is_none());
            assert!(
                worker
                    .service
                    .previous_publication(&"publication".to_owned())
                    .is_none()
            );
            let mut checks = 0;
            assert!(
                worker
                    .advance(
                        || true,
                        || {
                            checks += 1;
                            checks >= 2
                        }
                    )
                    .is_none()
            );
            let reply = worker.advance(|| false, || false).unwrap();
            let reply: SyntaxReply = serde_json::from_str(&reply).unwrap();
            assert_eq!(reply.status, SyntaxStatus::Cancelled);
            assert!(reply.analysis.is_none());
            assert!(!worker.has_work());
            assert!(weak.upgrade().is_none());
            assert!(
                worker
                    .service
                    .previous_publication(&"publication".to_owned())
                    .is_none()
            );
            assert!(worker.enqueue(&message).is_none());
            let reply = worker.advance(|| true, || false).unwrap();
            let (status, prepared) = SyntaxReply::receive(&reply, 900, &source).unwrap();
            assert!(matches!(status, SyntaxStatus::Ready { .. }));
            assert_eq!(prepared.unwrap().source(), source);
        }
    }

    #[test]
    fn worker_queue_limits_cancellation_and_resync_keep_reply_contracts() {
        let mut worker = SyntaxWorker::default();
        for ticket in 0..u32::try_from(super::super::MAX_SYNTAX_DOCUMENTS).unwrap() {
            let request = SyntaxRequest::new(
                ticket,
                format!("document-{ticket}"),
                Language::Rust,
                "fn main() {}",
                4,
                None,
            );
            assert!(
                worker
                    .enqueue(&serde_json::to_string(&request).unwrap())
                    .is_none()
            );
        }
        let excess = SyntaxRequest::new(
            100,
            "excess".into(),
            Language::Rust,
            "fn main() {}",
            4,
            None,
        );
        let reply = worker
            .enqueue(&serde_json::to_string(&excess).unwrap())
            .unwrap();
        assert_eq!(
            SyntaxReply::receive(&reply, 100, "fn main() {}").unwrap().0,
            SyntaxStatus::Cancelled
        );
        assert!(worker.retained_request_bytes() <= super::super::MAX_SYNTAX_SOURCE_BYTES);
        assert!(worker.advance(|| true, || true).is_none());
        let reply = worker.advance(|| false, || false).unwrap();
        assert_eq!(
            SyntaxReply::receive(&reply, 0, "fn main() {}").unwrap().0,
            SyntaxStatus::Cancelled
        );
        while worker.has_work() {
            worker.advance(|| false, || false);
        }
        assert_eq!(worker.retained_request_bytes(), 0);
        let request = SyntaxRequest {
            source: SyntaxSource::Replace {
                start: 0,
                end: 0,
                text: "new".into(),
            },
            base_ticket: Some(1),
            ..excess
        };
        assert!(
            worker
                .enqueue(&serde_json::to_string(&request).unwrap())
                .is_none()
        );
        let reply = worker.advance(|| true, || false).unwrap();
        assert_eq!(
            SyntaxReply::receive(&reply, 100, "new").unwrap().0,
            SyntaxStatus::NeedsSource
        );
    }

    #[test]
    fn resolved_deltas_and_oversized_sources_obey_worker_admission_limits() {
        let source = "x".repeat(MAX_STRUCTURE_BYTES);
        let mut worker = SyntaxWorker::default();
        let initial = SyntaxRequest::new(1, "base".into(), Language::Plain, &source, 4, None);
        let reply = worker
            .service
            .handle_message(&serde_json::to_string(&initial).unwrap(), || true)
            .unwrap();
        assert!(matches!(
            SyntaxReply::receive(&reply, 1, &source).unwrap().0,
            SyntaxStatus::Ready { .. }
        ));
        let delta = SyntaxRequest {
            ticket: 2,
            source: SyntaxSource::Replace {
                start: 0,
                end: 0,
                text: String::new(),
            },
            base_ticket: Some(1),
            ..initial
        };
        assert!(
            worker
                .enqueue(&serde_json::to_string(&delta).unwrap())
                .is_none()
        );
        for ticket in 3..6 {
            let request = SyntaxRequest::new(
                ticket,
                format!("queued-{ticket}"),
                Language::Plain,
                &source,
                4,
                None,
            );
            assert!(
                worker
                    .enqueue(&serde_json::to_string(&request).unwrap())
                    .is_none()
            );
        }
        let reply = worker.advance(|| true, || true).unwrap();
        assert_eq!(
            SyntaxReply::receive(&reply, 2, &source).unwrap().0,
            SyntaxStatus::Cancelled
        );
        assert!(worker.retained_request_bytes() <= super::super::MAX_SYNTAX_SOURCE_BYTES);
        while worker.has_work() {
            worker.advance(|| false, || false);
        }
        let oversized = SyntaxRequest::new(
            6,
            "oversized".into(),
            Language::Plain,
            &(source + "x"),
            4,
            None,
        );
        let reply = worker
            .enqueue(&serde_json::to_string(&oversized).unwrap())
            .unwrap();
        assert_eq!(
            SyntaxReply::receive(&reply, 6, "unused").unwrap().0,
            SyntaxStatus::TooLarge
        );
        assert!(!worker.has_work());
    }
    fn request(source: &str) -> SyntaxRequest {
        SyntaxRequest {
            version: SYNTAX_PROTOCOL_VERSION,
            ticket: 42,
            document: "1:main.rs".into(),
            language: Language::Rust,
            source: SyntaxSource::Full(source.into()),
            tab_width: 4,
            base_ticket: None,
        }
    }
    #[test]
    fn source_request_spans_match_all_languages_and_resync_stale_or_evicted_bases() {
        for &(path, body, _) in crate::editor::syntax_contracts::LANGUAGE_CASES {
            let source = format!("{body}{}", "\n".repeat(1000));
            let language = crate::highlight::language_from_path(path);
            let mut service = SyntaxPreparations::default();
            let first = SyntaxRequest::new(42, path.into(), language, &source, 4, None);
            let response = service
                .handle_message(&serde_json::to_string(&first).unwrap(), || true)
                .unwrap();
            let old = SyntaxReply::receive(&response, 42, &source)
                .unwrap()
                .1
                .unwrap();
            let revised = source.replace("文😀", "🦀 changed");
            let update =
                SyntaxRequest::new(43, path.into(), language, &revised, 4, Some((42, &old)));
            assert!(
                matches!(update.source, SyntaxSource::Replace { .. }),
                "{path}"
            );
            let delta = serde_json::to_string(&update).unwrap();
            let full = SyntaxRequest::new(43, path.into(), language, &revised, 4, None);
            assert!(
                delta.len() * 2 < serde_json::to_string(&full).unwrap().len(),
                "{path}"
            );
            let response = service.handle_message(&delta, || true).unwrap();
            let prepared = SyntaxReply::receive_reusing(&response, 43, &revised, Some((42, &old)))
                .unwrap()
                .1
                .unwrap();
            let mut direct = SyntaxPreparations::default();
            let expected = direct
                .prepare(path.to_string(), language, &revised, 4, || true)
                .1
                .unwrap();
            assert_eq!(prepared.folds(), expected.folds(), "{path}");
            assert_eq!(prepared.highlights(), expected.highlights(), "{path}");
            assert_eq!(
                prepared
                    .structure()
                    .map(|value| serde_json::to_value(value.transfer_data()).unwrap()),
                expected
                    .structure()
                    .map(|value| serde_json::to_value(value.transfer_data()).unwrap()),
                "{path}"
            );
            // The worker advanced to ticket 43; a discarded UI reply must resync.
            for evicted in [false, true] {
                if evicted {
                    service.remove(&path.to_string());
                }
                let response = service.handle_message(&delta, || true).unwrap();
                assert_eq!(
                    SyntaxReply::receive_reusing(&response, 43, &revised, Some((42, &old)))
                        .unwrap()
                        .0,
                    SyntaxStatus::NeedsSource
                );
                let response = service
                    .handle_message(&serde_json::to_string(&full).unwrap(), || true)
                    .unwrap();
                assert!(
                    SyntaxReply::receive(&response, 43, &revised)
                        .unwrap()
                        .1
                        .is_some()
                );
            }
        }
    }

    #[test]
    fn resolved_nonminimal_replacements_match_fresh_analysis_for_all_languages() {
        for &(path, body, _) in crate::editor::syntax_contracts::LANGUAGE_CASES {
            for ending in ["\n", "\r\n"] {
                let source = format!("{body}{ending}{}", ending.repeat(300));
                let replacement = format!("{ending}{}", body.replace("文😀", "🦀 changed"));
                let revised = format!("{replacement}{}", &source[body.len()..]);
                let language = crate::highlight::language_from_path(path);
                let mut service = SyntaxPreparations::default();
                let initial = SyntaxRequest::new(42, path.into(), language, &source, 4, None);
                let response = service
                    .handle_message(&serde_json::to_string(&initial).unwrap(), || true)
                    .unwrap();
                let retained = SyntaxReply::receive(&response, 42, &source)
                    .unwrap()
                    .1
                    .unwrap();
                let mut update = SyntaxRequest::new(
                    43,
                    path.into(),
                    language,
                    &revised,
                    4,
                    Some((42, &retained)),
                );
                update.source = SyntaxSource::Replace {
                    start: 0,
                    end: body.len(),
                    text: replacement,
                };
                let response = service
                    .handle_message(&serde_json::to_string(&update).unwrap(), || true)
                    .unwrap();
                let result =
                    SyntaxReply::receive_reusing(&response, 43, &revised, Some((42, &retained)))
                        .unwrap()
                        .1
                        .unwrap();
                let mut fresh = SyntaxPreparations::default();
                let expected = fresh
                    .prepare(path.to_owned(), language, &revised, 4, || true)
                    .1
                    .unwrap();
                assert_eq!(result.folds(), expected.folds(), "{path} {ending:?}");
                assert_eq!(
                    result.highlights(),
                    expected.highlights(),
                    "{path} {ending:?}"
                );
                assert_eq!(
                    result
                        .structure()
                        .map(|value| serde_json::to_value(value.transfer_data()).unwrap()),
                    expected
                        .structure()
                        .map(|value| serde_json::to_value(value.transfer_data()).unwrap()),
                    "{path} {ending:?}"
                );
                assert_eq!(retained.source(), source);
                assert_eq!(result.source(), revised);
            }
        }
    }

    #[test]
    fn malformed_source_requests_and_reconstructed_limits_do_not_publish() {
        let source = format!("{}文😀\r\n", "prefix ".repeat(30));
        let mut service = SyntaxPreparations::default();
        let first = request(&source);
        let response = service
            .handle_message(&serde_json::to_string(&first).unwrap(), || true)
            .unwrap();
        let old = SyntaxReply::receive(&response, 42, &source)
            .unwrap()
            .1
            .unwrap();
        let mut update = SyntaxRequest::new(
            43,
            first.document.clone(),
            first.language,
            &source,
            4,
            Some((42, &old)),
        );
        for (start, end) in [
            (source.find('文').unwrap() + 1, source.len()),
            (2, 1),
            (0, usize::MAX),
        ] {
            update.source = SyntaxSource::Replace {
                start,
                end,
                text: "".into(),
            };
            assert!(
                service
                    .handle_message(&serde_json::to_string(&update).unwrap(), || true)
                    .is_none()
            );
        }
        update.source = SyntaxSource::Replace {
            start: 0,
            end: 0,
            text: "".into(),
        };
        update.base_ticket = None;
        assert!(
            service
                .handle_message(&serde_json::to_string(&update).unwrap(), || true)
                .is_none()
        );
        update.base_ticket = Some(42);
        update.source = SyntaxSource::Replace {
            start: 0,
            end: 0,
            text: "x".repeat(MAX_STRUCTURE_BYTES),
        };
        let response = service
            .handle_message(&serde_json::to_string(&update).unwrap(), || true)
            .unwrap();
        assert_eq!(
            SyntaxReply::receive(&response, 43, &source).unwrap().0,
            SyntaxStatus::TooLarge
        );
        assert!(service.is_empty());
    }

    #[test]
    fn large_worker_updates_transfer_changed_spans_and_share_unchanged_rows() {
        let source = "SELECT name, value FROM items;\r\n".repeat(1000);
        let mut service = SyntaxPreparations::default();
        let mut input = request(&source);
        input.language = Language::Sql;
        let first = service
            .handle_message(&serde_json::to_string(&input).unwrap(), || true)
            .unwrap();
        let old = SyntaxReply::receive(&first, 42, &source)
            .unwrap()
            .1
            .unwrap();
        let revised = source.replacen("value", "revised_value", 1);
        input.source = SyntaxSource::Full(revised.clone());
        input.ticket = 43;
        input.base_ticket = Some(42);
        let delta = service
            .handle_message(&serde_json::to_string(&input).unwrap(), || true)
            .unwrap();
        let wire: serde_json::Value = serde_json::from_str(&delta).unwrap();
        assert_eq!(wire["analysis"]["source"]["text"], "revised_");
        assert_eq!(wire["analysis"]["highlights"].as_array().unwrap().len(), 2);
        assert_eq!(wire["analysis"]["highlights"][1]["count"], 1000);
        assert_eq!(
            wire["analysis"]["source"]["start"],
            source.find("value").unwrap()
        );
        let updated = SyntaxReply::receive_reusing(&delta, 43, &revised, Some((42, &old)))
            .unwrap()
            .1
            .unwrap();
        input.ticket = 44;
        input.base_ticket = None;
        let full = service
            .handle_message(&serde_json::to_string(&input).unwrap(), || true)
            .unwrap();
        assert!(
            delta.len() * 2 < full.len(),
            "changed-span publication must materially reduce this workload"
        );
        assert_eq!(
            updated.highlights(),
            SyntaxReply::receive(&full, 44, &revised)
                .unwrap()
                .1
                .unwrap()
                .highlights()
        );
        assert!(
            old.highlights()
                .unwrap()
                .iter()
                .skip(1)
                .zip(updated.highlights().unwrap().iter().skip(1))
                .all(|(old, new)| std::sync::Arc::ptr_eq(old, new))
        );
    }

    #[test]
    fn row_references_preserve_paint_and_reject_missing_stale_or_mismatching_bases() {
        for (language, source) in [
            (
                Language::Rust,
                "fn main() {\r\n let text = \"文😀\";\r\n}\r\n",
            ),
            (Language::Json, "{\r\n \"name\": \"文😀\"\r\n}\r\n"),
            (
                Language::Sql,
                "/* first\r\nstill comment */\r\nSELECT '文😀';\r\n",
            ),
        ] {
            let mut service = SyntaxPreparations::default();
            let mut input = request(source);
            input.language = language;
            let first = service
                .handle_message(&serde_json::to_string(&input).unwrap(), || true)
                .unwrap();
            let old = SyntaxReply::receive(&first, 42, source).unwrap().1.unwrap();
            let revised = source.replace("文😀", "😀 changed");
            input.source = SyntaxSource::Full(revised.clone());
            input.ticket = 43;
            input.base_ticket = Some(42);
            let next = service
                .handle_message(&serde_json::to_string(&input).unwrap(), || true)
                .unwrap();
            let value: serde_json::Value = serde_json::from_str(&next).unwrap();
            assert_eq!(value["analysis"]["base_ticket"], 42);
            assert!(
                value["analysis"]["highlights"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row.get("reuse").is_some())
            );
            let new = SyntaxReply::receive_reusing(&next, 43, &revised, Some((42, &old)))
                .unwrap()
                .1
                .unwrap();
            assert!(std::sync::Arc::ptr_eq(
                &old.highlights().unwrap()[0],
                &new.highlights().unwrap()[0]
            ));
            let mut direct = SyntaxPreparations::default();
            let expected = direct
                .prepare("direct".to_string(), language, &revised, 4, || true)
                .1
                .unwrap();
            assert_eq!(new.highlights(), expected.highlights());
            assert!(old.matches_source(source));
            assert!(SyntaxReply::receive(&next, 43, &revised).is_none());
            assert!(SyntaxReply::receive_reusing(&next, 43, source, Some((42, &old))).is_none());
            assert!(SyntaxReply::receive_reusing(&next, 43, &revised, Some((41, &old))).is_none());
            assert!(SyntaxReply::receive_reusing(&next, 44, &revised, Some((42, &old))).is_none());
            for replacement in [
                serde_json::json!({"reuse":usize::MAX,"count":1}),
                serde_json::json!({"reuse":1,"count":1}),
                serde_json::json!({"reuse":0,"count":1,"extra":true}),
            ] {
                let mut invalid = value.clone();
                invalid["analysis"]["highlights"][0] = replacement;
                assert!(
                    SyntaxReply::receive_reusing(
                        &invalid.to_string(),
                        43,
                        &revised,
                        Some((42, &old))
                    )
                    .is_none()
                );
            }
            let mut invalid = value;
            invalid["analysis"]
                .as_object_mut()
                .unwrap()
                .remove("base_ticket");
            assert!(
                SyntaxReply::receive_reusing(&invalid.to_string(), 43, &revised, Some((42, &old)))
                    .is_none()
            );
            // A stale/discarded base or LRU eviction returns a complete standalone result.
            for evicted in [false, true] {
                if evicted {
                    service.remove(&input.document);
                }
                input.ticket += 1;
                let full = service
                    .handle_message(&serde_json::to_string(&input).unwrap(), || true)
                    .unwrap();
                assert!(
                    SyntaxReply::receive(&full, input.ticket, &revised)
                        .unwrap()
                        .1
                        .is_some()
                );
            }
        }
    }

    #[test]
    fn message_contract_matches_direct_preparation_and_rejects_wrong_tickets_and_sources() {
        let source = "fn main() {\r\n call(\"文😀\");\r\n}";
        let mut direct = SyntaxPreparations::default();
        let (_, expected) =
            direct.prepare("1:main.rs".to_string(), Language::Rust, source, 4, || true);
        let mut worker = SyntaxPreparations::default();
        let input = serde_json::to_string(&request(source)).unwrap();
        let output = worker.handle_message(&input, || true).unwrap();
        let (status, result) = SyntaxReply::receive(&output, 42, source).unwrap();
        assert_eq!(status, SyntaxStatus::Ready { incremental: false });
        assert_eq!(expected.unwrap().highlights(), result.unwrap().highlights());
        assert!(SyntaxReply::receive(&output, 43, source).is_none());
        assert!(SyntaxReply::receive(&output, 42, &source.replace("main", "xxxx")).is_none());
        let output = worker.handle_message(&input, || false).unwrap();
        assert!(matches!(
            SyntaxReply::receive(&output, 42, source),
            Some((SyntaxStatus::Cancelled, None))
        ));
        let output = worker
            .handle_message(
                &serde_json::to_string(&request(&"x".repeat(MAX_STRUCTURE_BYTES + 1))).unwrap(),
                || panic!("byte admission precedes cancellation and parser preparation"),
            )
            .unwrap();
        assert!(matches!(
            SyntaxReply::receive(&output, 42, ""),
            Some((SyntaxStatus::TooLarge, None))
        ));
        let mut invalid = request(source);
        invalid.version += 1;
        assert!(
            worker
                .handle_message(&serde_json::to_string(&invalid).unwrap(), || true)
                .is_none()
        );
        assert!(worker.handle_message("{}", || true).is_none());
    }
}
