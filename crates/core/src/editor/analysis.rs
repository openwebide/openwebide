//! Immutable source-bound analysis consumed without compiling language engines.
use super::{FoldRange, Structure};
use crate::plugins::languages as protocol;
use std::sync::Arc;

/// Foreground editor work retains the same bounded input deadline whether
/// analysis arrives from a host component or a parser test fixture.
pub const SYNTAX_INPUT_BUDGET_MS: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SyntaxStatus {
    Ready { incremental: bool },
    TooLarge,
    Cancelled,
    NeedsSource,
}

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct SyntaxAnalysis {
    pub(in crate::editor) source: Arc<String>,
    pub(in crate::editor) folds: Vec<FoldRange>,
    pub(in crate::editor) structure: Option<Arc<Structure>>,
    pub(in crate::editor) highlights: Option<Arc<crate::highlight::TokenRows>>,
}
impl SyntaxAnalysis {
    pub fn from_language_configuration(
        source: Arc<String>,
        language: &str,
        definitions: &[protocol::LanguageDefinition],
    ) -> Result<Self, String> {
        let structure =
            Structure::from_language_configuration(source.clone(), language, definitions)?;
        Ok(Self {
            source,
            folds: vec![],
            structure: Some(Arc::new(structure)),
            highlights: None,
        })
    }
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
        Self::from_language_data_reusing(source, language, data, definitions, None)
    }

    /// Reuse immutable paint rows only after their complete text and token
    /// categories match the newly validated result. Structure is always current.
    pub fn from_language_data_reusing(
        source: Arc<String>,
        language: &str,
        data: protocol::SyntaxAnalysis,
        definitions: &[protocol::LanguageDefinition],
        previous: Option<&Self>,
    ) -> Result<Self, String> {
        protocol::validate_analysis(&data, &source, language)?;
        let highlights = data.highlights.map(|rows| {
            let previous = previous.filter(|previous| previous.highlights.is_some());
            let alignment = previous.and_then(|previous| {
                let change = super::text_change(previous.source(), &source)?;
                let before = source[..change.range.start]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count();
                let old_end = before
                    + previous.source[change.range.clone()]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count();
                let new_end = before
                    + source[change.range.start..change.new_end]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count();
                Some((before, old_end, new_end))
            });
            let rows = rows
                .into_iter()
                .zip(source.split('\n'))
                .enumerate()
                .map(|(index, (spans, row))| {
                    let old_index = alignment.map_or(index, |(before, old_end, new_end)| {
                        if index >= before && index >= new_end {
                            old_end + index - new_end
                        } else {
                            index
                        }
                    });
                    if let Some(old) =
                        previous.and_then(|previous| previous.highlights.as_ref()?.get(old_index))
                    {
                        let mut start = 0;
                        if old.len() == spans.len()
                            && old.iter().zip(&spans).all(|(token, span)| {
                                let matches = token.kind == span.kind
                                    && token.text.as_str() == &row[start..span.end];
                                start = span.end;
                                matches
                            })
                        {
                            return old.clone();
                        }
                    }
                    let mut start = 0;
                    crate::highlight::share_token_row(
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
                            .collect(),
                    )
                })
                .collect::<Vec<_>>();
            if let Some(old) = previous.and_then(Self::highlights)
                && old.len() == rows.len()
                && old.iter().zip(&rows).all(|(a, b)| Arc::ptr_eq(a, b))
            {
                old.clone()
            } else {
                Arc::new(rows)
            }
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
