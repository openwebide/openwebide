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

mod custom;

const MAX_DOCUMENTS: usize = 16;
const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct LanguageGrammar {
    /// Use Plain with a custom provider for grammars outside the built-in set.
    /// Parsed contexts and declared bracket pairs supply structural metadata.
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
    delegate_embeddings: bool,
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
            delegate_embeddings: false,
        })
    }
    /// Independent families delegate embedded bodies to installed providers.
    pub fn new_delegated(
        definitions: Vec<LanguageDefinition>,
        grammars: BTreeMap<String, LanguageGrammar>,
    ) -> Result<Self, String> {
        if grammars.values().any(|grammar| {
            grammar.provider.is_none()
                && !matches!(
                    grammar.language,
                    SyntaxLanguage::Sql | SyntaxLanguage::Plain
                )
        }) {
            return Err("A declared language grammar is not enabled in this component".into());
        }
        let mut engine = Self::new(definitions, grammars)?;
        engine.delegate_embeddings = true;
        Ok(engine)
    }

    pub fn definitions(&self) -> &[LanguageDefinition] {
        &self.definitions
    }

    pub fn handle(&mut self, input: LanguageInput) -> Result<LanguageOutput, String> {
        self.handle_with(input, |_, _, _| Ok(()))
    }

    /// Refine complete parsed data using the reconstructed source, for example
    /// to recover unfinished literals in a package's own grammar. Host bounds
    /// and validation still apply; unavailable sources never invoke the callback.
    pub fn handle_with(
        &mut self,
        input: LanguageInput,
        refine: impl FnOnce(&str, &str, &mut protocol::SyntaxAnalysis) -> Result<(), String>,
    ) -> Result<LanguageOutput, String> {
        protocol::validate_input(&input)?;
        match input {
            LanguageInput::Close { version, document } => {
                self.documents.remove(&document);
                Ok(LanguageOutput::Closed { version, document })
            }
            LanguageInput::Analyze(request) => self.analyze(request, refine),
        }
    }

    fn analyze(
        &mut self,
        request: AnalysisRequest,
        refine: impl FnOnce(&str, &str, &mut protocol::SyntaxAnalysis) -> Result<(), String>,
    ) -> Result<LanguageOutput, String> {
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
        // Markdown's block and inline grammars implement one language. Only
        // fenced bodies cross the installed-provider boundary.
        let markdown = self.delegate_embeddings && grammar.language == SyntaxLanguage::Markdown;
        let Some(mut syntax) = previous.map(|document| document.syntax).or_else(|| {
            if self.delegate_embeddings && !markdown {
                SyntaxDocument::with_delegated_provider(grammar.language, grammar.provider)
            } else {
                SyntaxDocument::with_provider(grammar.language, grammar.provider)
            }
        }) else {
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
            let Some(mut data) = analysis.and_then(|analysis| {
                analysis.language_data(&request.language, |language| {
                    if markdown && language == SyntaxLanguage::MarkdownInline {
                        return request.language.clone();
                    }
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
            if self.delegate_embeddings
                && grammar
                    .provider
                    .is_some_and(|provider| provider.injection.is_some())
            {
                if markdown && let Some(structure) = &mut data.structure {
                    structure
                        .scopes
                        .retain(|scope| scope.language != request.language);
                }
                let scopes = syntax.language_regions(|node, source| {
                    if markdown && node.kind() == "inline" {
                        return None;
                    }
                    openwebide_core::editor::language_injection(grammar.language, node, source)
                })?;
                let composition = protocol::LanguageComposition::new(&source.source);
                for scope in scopes {
                    composition.replace(&mut data, &scope, None)?;
                }
                protocol::validate_analysis(&data, &source.source, &request.language)?;
            }
            refine(&source.source, &request.language, &mut data)?;
            protocol::validate_analysis(&data, &source.source, &request.language)?;
            if grammar.language == SyntaxLanguage::Plain && grammar.provider.is_some() {
                let definition = self
                    .definitions
                    .iter()
                    .find(|definition| definition.id == request.language)
                    .unwrap();
                if custom::brackets(&source.source, &mut data, &definition.configuration).is_none()
                {
                    response.status = AnalysisStatus::TooLarge;
                    return Ok(output);
                }
                protocol::validate_analysis(&data, &source.source, &request.language)?;
            }
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
