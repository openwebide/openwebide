//! Optional parser toolkit linked into ordinary language components, never the
//! browser or bridge. Packages own definitions and select their grammar bindings.
pub use openwebide_core::editor::{SYNTAX_PROVIDERS, SyntaxProvider, syntax_provider};
use openwebide_core::{
    editor::{SyntaxDocument, SyntaxStatus},
    plugins::languages::{self as protocol, LanguageDocument},
};
pub use openwebide_plugin_sdk::languages::SyntaxLanguage;
use protocol::{
    AnalysisRequest, AnalysisStatus, LanguageDefinition, LanguageInput, LanguageOutput,
};
use std::{collections::BTreeMap, sync::Arc};

const MAX_DOCUMENTS: usize = 16;
const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct LanguageGrammar {
    pub language: SyntaxLanguage,
    pub provider: Option<SyntaxProvider>,
}

#[cfg(test)]
mod tests;
impl LanguageGrammar {
    pub fn standard(language: SyntaxLanguage) -> Self {
        Self {
            language,
            provider: syntax_provider(language),
        }
    }
}

struct Document {
    source: LanguageDocument,
    syntax: SyntaxDocument,
    tick: u64,
}
pub struct LanguageSyntax {
    definitions: Vec<LanguageDefinition>,
    grammars: BTreeMap<String, LanguageGrammar>,
    documents: BTreeMap<String, Document>,
    tick: u64,
}
impl LanguageSyntax {
    pub fn new(
        definitions: Vec<LanguageDefinition>,
        grammars: BTreeMap<String, LanguageGrammar>,
    ) -> Result<Self, String> {
        protocol::validate_definitions(&definitions)?;
        if grammars
            .keys()
            .any(|id| !definitions.iter().any(|definition| &definition.id == id))
        {
            return Err("Every grammar binding requires a declared language".into());
        }
        Ok(Self {
            definitions,
            grammars,
            documents: BTreeMap::new(),
            tick: 0,
        })
    }
    pub fn definitions(&self) -> &[LanguageDefinition] {
        &self.definitions
    }

    pub fn handle(&mut self, input: LanguageInput) -> Result<LanguageOutput, String> {
        protocol::validate_input(&input)?;
        match input {
            LanguageInput::Close { version, document } => {
                self.documents.remove(&document);
                Ok(LanguageOutput::Closed { version, document })
            }
            LanguageInput::Analyze(request) => self.analyze(request),
        }
    }

    fn analyze(&mut self, request: AnalysisRequest) -> Result<LanguageOutput, String> {
        let Some(grammar) = self.grammars.get(&request.language).copied() else {
            return Ok(protocol::analysis_response(
                &request,
                AnalysisStatus::Unsupported,
            ));
        };
        let previous = self.documents.get(&request.document);
        let Some(source) =
            LanguageDocument::apply(previous.map(|document| &document.source), &request)?
        else {
            return Ok(protocol::analysis_response(
                &request,
                AnalysisStatus::NeedsSource,
            ));
        };
        let previous = self
            .documents
            .remove(&request.document)
            .filter(|previous| previous.source.language == request.language);
        let Some(mut syntax) = previous
            .map(|document| document.syntax)
            .or_else(|| SyntaxDocument::with_provider(grammar.language, grammar.provider))
        else {
            return Err("Language grammar could not be initialized".into());
        };
        let (status, analysis) = syntax.prepare_shared(
            Arc::new(source.source.to_string()),
            request.tab_width,
            || true,
        );
        let status = match status {
            SyntaxStatus::Ready { incremental } => AnalysisStatus::Ready { incremental },
            SyntaxStatus::TooLarge => AnalysisStatus::TooLarge,
            SyntaxStatus::Cancelled => return Err("Syntax preparation was cancelled".into()),
            SyntaxStatus::NeedsSource => AnalysisStatus::NeedsSource,
        };
        let mut output = protocol::analysis_response(&request, status);
        let LanguageOutput::Analyzed(response) = &mut output else {
            unreachable!()
        };
        if matches!(status, AnalysisStatus::Ready { .. }) {
            let Some(data) = analysis.and_then(|analysis| {
                analysis.language_data(&request.language, |language| {
                    self.grammars
                        .iter()
                        .find(|(_, grammar)| grammar.language == language)
                        .map_or_else(
                            || format!("{language:?}").to_ascii_lowercase(),
                            |(id, _)| id.clone(),
                        )
                })
            }) else {
                response.status = AnalysisStatus::TooLarge;
                return Ok(output);
            };
            response.analysis = Some(data);
            self.tick = self.tick.wrapping_add(1);
            self.documents.insert(
                request.document.clone(),
                Document {
                    source,
                    syntax,
                    tick: self.tick,
                },
            );
            self.evict();
        }
        Ok(output)
    }

    fn evict(&mut self) {
        while self.documents.len() > MAX_DOCUMENTS
            || self
                .documents
                .values()
                .map(|document| document.source.source.len())
                .sum::<usize>()
                > MAX_SOURCE_BYTES
        {
            let oldest = self
                .documents
                .iter()
                .min_by_key(|(_, document)| document.tick)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.documents.remove(&oldest);
            } else {
                break;
            }
        }
    }
}
