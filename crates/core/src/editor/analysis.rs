//! Immutable source-bound analysis consumed without compiling language engines.
use super::{FoldRange, Structure};
use crate::plugins::languages as protocol;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SyntaxStatus {
    Ready { incremental: bool },
    TooLarge,
    Cancelled,
    NeedsSource,
}

#[derive(Clone, Debug)]
pub struct SyntaxAnalysis {
    pub(in crate::editor) source: Arc<String>,
    pub(in crate::editor) folds: Vec<FoldRange>,
    pub(in crate::editor) structure: Option<Arc<Structure>>,
    pub(in crate::editor) highlights: Option<Arc<crate::highlight::TokenRows>>,
}
impl SyntaxAnalysis {
    pub fn matches_source(&self, source: &str) -> bool {
        std::ptr::eq(self.source.as_str(), source) || self.source.as_str() == source
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn source_snapshot(&self) -> &Arc<String> {
        &self.source
    }
    pub fn folds(&self) -> &[FoldRange] {
        &self.folds
    }
    pub fn structure(&self) -> Option<&Arc<Structure>> {
        self.structure.as_ref()
    }
    pub fn highlights(&self) -> Option<&Arc<crate::highlight::TokenRows>> {
        self.highlights.as_ref()
    }

    pub fn from_language_data(
        source: Arc<String>,
        language: &str,
        data: protocol::SyntaxAnalysis,
        definitions: &[protocol::LanguageDefinition],
    ) -> Result<Self, String> {
        protocol::validate_analysis(&data, &source, language)?;
        let highlights = data.highlights.map(|rows| {
            let rows = rows
                .into_iter()
                .zip(source.split('\n'))
                .map(|(spans, row)| {
                    let mut start = 0;
                    spans
                        .into_iter()
                        .map(|span| {
                            let token = crate::highlight::Token {
                                text: Arc::new(row[start..span.end].into()),
                                kind: span.kind,
                            };
                            start = span.end;
                            token
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            Arc::new(crate::highlight::share_token_rows(rows))
        });
        let structure = match data.structure {
            Some(data) => Structure::from_language_data(source.clone(), data, definitions)?,
            None => Structure::from_language_configuration(source.clone(), language, definitions)?,
        };
        Ok(Self {
            source,
            folds: data.folds,
            structure: Some(Arc::new(structure)),
            highlights,
        })
    }
}
