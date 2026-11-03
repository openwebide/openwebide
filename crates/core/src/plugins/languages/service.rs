//! Document and result admission shared by clients and execution hosts.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MAX_REQUEST_ENVELOPE_BYTES: usize = 2 * MAX_REQUEST_BYTES + 1024 * 1024;

pub fn source_digest(source: &str) -> String {
    Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn validate_input(input: &LanguageInput) -> Result<(), String> {
    let (version, document) = match input {
        LanguageInput::Close { version, document } => (*version, document),
        LanguageInput::Analyze(request) => return validate_analysis_request(request),
    };
    validate_document_identity(version, document)
}

fn validate_analysis_request(request: &AnalysisRequest) -> Result<(), String> {
    if !identifier(&request.language)
        || request.path.len() > 4096
        || request.path.contains('\0')
        || !(1..=32).contains(&request.tab_width)
        || request.source_digest.len() != 64
        || !request
            .source_digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("Invalid language analysis identity or configuration".into());
    }
    let text = match &request.source {
        SourceChange::Full { text } => text,
        SourceChange::Replace {
            base_revision,
            start,
            end,
            text,
        } => {
            if *base_revision >= request.revision || start > end || *end > MAX_SOURCE_BYTES {
                return Err("Invalid language source revision or replacement".into());
            }
            text
        }
    };
    if text.len() > MAX_SOURCE_BYTES {
        return Err("Language source exceeds its byte limit".into());
    }
    validate_document_identity(request.version, &request.document)
}

fn validate_document_identity(version: u32, document: &str) -> Result<(), String> {
    if version != VERSION || !text(document, 256) {
        return Err("Unsupported language protocol or invalid document identity".into());
    }
    Ok(())
}

/// A host retains the exact admitted source alongside a component. The plugin
/// never supplies source bytes used for rendering or source ownership checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageDocument {
    pub revision: u64,
    pub request: u64,
    pub language: String,
    pub path: String,
    pub tab_width: usize,
    pub digest: String,
    pub source: std::sync::Arc<str>,
}

impl LanguageDocument {
    /// None requests a full snapshot after eviction/restart or a missed delta.
    pub fn apply(
        previous: Option<&Self>,
        request: &AnalysisRequest,
    ) -> Result<Option<Self>, String> {
        validate_analysis_request(request)?;
        if previous.is_some_and(|previous| {
            request.request <= previous.request || request.revision < previous.revision
        }) {
            return Err("Language document request is stale".into());
        }
        let source = match &request.source {
            SourceChange::Full { text } => text.clone(),
            SourceChange::Replace {
                base_revision,
                start,
                end,
                text,
            } => {
                let Some(previous) = previous.filter(|previous| {
                    previous.revision == *base_revision
                        && previous.language == request.language
                        && previous.path == request.path
                        && previous.tab_width == request.tab_width
                }) else {
                    return Ok(None);
                };
                if previous.source.get(*start..*end).is_none() {
                    return Err("Language replacement is not on UTF-8 boundaries".into());
                }
                let size = previous.source.len() - (end - start) + text.len();
                if size > MAX_SOURCE_BYTES {
                    return Err("Language source exceeds its byte limit".into());
                }
                let mut source = previous.source.to_string();
                source.replace_range(*start..*end, text);
                source
            }
        };
        if source_digest(&source) != request.source_digest {
            return Err("Language source digest does not match the document".into());
        }
        if previous.is_some_and(|previous| {
            previous.revision == request.revision && previous.digest != request.source_digest
        }) {
            return Err("Language document changed without a new revision".into());
        }
        Ok(Some(Self {
            revision: request.revision,
            request: request.request,
            language: request.language.clone(),
            path: request.path.clone(),
            tab_width: request.tab_width,
            digest: request.source_digest.clone(),
            source: source.into(),
        }))
    }
}

pub fn analysis_response(request: &AnalysisRequest, status: AnalysisStatus) -> LanguageOutput {
    LanguageOutput::Analyzed(Box::new(AnalysisResponse {
        version: VERSION,
        document: request.document.clone(),
        request: request.request,
        revision: request.revision,
        language: request.language.clone(),
        source_digest: request.source_digest.clone(),
        status,
        analysis: None,
    }))
}

pub fn validate_output(
    input: &LanguageInput,
    output: &LanguageOutput,
    source: Option<&str>,
) -> Result<(), String> {
    validate_input(input)?;
    match (input, output) {
        (
            LanguageInput::Close { version, document },
            LanguageOutput::Closed {
                version: actual,
                document: id,
            },
        ) if version == actual && document == id => Ok(()),
        (LanguageInput::Analyze(request), LanguageOutput::Analyzed(response)) => {
            if response.version != request.version
                || response.document != request.document
                || response.request != request.request
                || response.revision != request.revision
                || response.language != request.language
                || response.source_digest != request.source_digest
            {
                return Err("Language result belongs to another document request".into());
            }
            match (&response.status, &response.analysis) {
                (AnalysisStatus::Ready { .. }, Some(analysis)) => {
                    let source = source.ok_or("Language result has no admitted source")?;
                    if source_digest(source) != request.source_digest {
                        return Err("Language result source changed".into());
                    }
                    validate_analysis(analysis, source, &request.language)
                }
                (AnalysisStatus::Ready { .. }, None) | (_, Some(_)) => {
                    Err("Language status and analysis disagree".into())
                }
                _ => Ok(()),
            }
        }
        _ => Err("Language result operation does not match its request".into()),
    }
}

pub fn validate_analysis(
    analysis: &SyntaxAnalysis,
    source: &str,
    language: &str,
) -> Result<(), String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err("Language source exceeds its byte limit".into());
    }
    let rows: Vec<&str> = source.split('\n').collect();
    let mut records = analysis.folds.len();
    if let Some(highlights) = &analysis.highlights {
        if highlights.len() != rows.len() {
            return Err("Language token rows do not match the source".into());
        }
        records = records.saturating_add(highlights.len());
        for (spans, row) in highlights.iter().zip(&rows) {
            records = records.saturating_add(spans.len());
            let mut offset = 0;
            for span in spans {
                if span.end <= offset || span.end > row.len() || !row.is_char_boundary(span.end) {
                    return Err("Language token offsets are invalid".into());
                }
                offset = span.end;
            }
            if offset != row.len() {
                return Err("Language tokens do not cover their source row".into());
            }
        }
    }
    let mut previous_fold = None;
    for fold in &analysis.folds {
        let key = (fold.start_line, fold.end_line);
        if fold.start_line >= fold.end_line
            || fold.end_line >= rows.len()
            || previous_fold.is_some_and(|previous| previous >= key)
        {
            return Err("Language fold ranges are invalid".into());
        }
        previous_fold = Some(key);
    }
    if let Some(structure) = &analysis.structure {
        if structure.language != language {
            return Err("Language structure belongs to another language".into());
        }
        records = records
            .saturating_add(structure.scopes.len())
            .saturating_add(structure.selections.len())
            .saturating_add(structure.opaque_starts.len())
            .saturating_add(structure.protected.len())
            .saturating_add(structure.brackets.len());
        let valid_range = |range: &TextRange| {
            range.start < range.end && source.get(range.start..range.end).is_some()
        };
        if structure.scopes.iter().any(|scope| {
            scope.range.start > scope.range.end
                || source.get(scope.range.start..scope.range.end).is_none()
                || !identifier(&scope.language)
        }) || structure
            .scopes
            .windows(2)
            .any(|pair| pair[0].range.end > pair[1].range.start)
            || structure.selections.iter().any(|range| !valid_range(range))
            || structure
                .protected
                .iter()
                .any(|region| !valid_range(&region.range))
            || structure
                .protected
                .windows(2)
                .any(|pair| pair[0].range.end > pair[1].range.start)
            || structure
                .selections
                .windows(2)
                .any(|pair| (pair[0].start, pair[0].end) >= (pair[1].start, pair[1].end))
            || structure
                .opaque_starts
                .iter()
                .any(|offset| *offset >= source.len() || !source.is_char_boundary(*offset))
            || structure
                .opaque_starts
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err("Language structural source ranges are invalid".into());
        }
        let mut brackets = BTreeMap::new();
        let mut previous = None;
        for bracket in &structure.brackets {
            if previous.is_some_and(|offset| offset >= bracket.offset)
                || source
                    .get(bracket.offset..)
                    .and_then(|text| text.chars().next())
                    != Some(bracket.character)
                || !character(bracket.character)
            {
                return Err("Language bracket source positions are invalid".into());
            }
            previous = Some(bracket.offset);
            brackets.insert(bracket.offset, bracket);
        }
        if structure.brackets.iter().any(|bracket| {
            bracket.partner.is_some_and(|partner| {
                partner == bracket.offset
                    || brackets
                        .get(&partner)
                        .is_none_or(|other| other.partner != Some(bracket.offset))
            })
        }) {
            return Err("Language bracket partners are not reciprocal".into());
        }
    }
    if records > MAX_RECORDS {
        return Err("Language analysis exceeds its record limit".into());
    }
    Ok(())
}
