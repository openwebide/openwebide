//! Public component output for packages using the optional parser toolkit.
use super::SyntaxAnalysis;
use crate::{highlight::Language, plugins::languages as protocol};

impl SyntaxAnalysis {
    pub fn language_data(
        &self,
        language: &str,
        scope_id: impl Fn(Language) -> String,
    ) -> Option<protocol::SyntaxAnalysis> {
        if self.record_count() > protocol::MAX_RECORDS {
            return None;
        }
        let data = protocol::SyntaxAnalysis {
            highlights: self.highlights().map(|rows| {
                rows.iter()
                    .map(|row| {
                        let mut end = 0;
                        row.iter()
                            .map(|token| {
                                end += token.text.len();
                                protocol::TokenSpan {
                                    end,
                                    kind: token.kind,
                                }
                            })
                            .collect()
                    })
                    .collect()
            }),
            folds: self.folds().to_vec(),
            structure: self
                .structure()
                .map(|structure| structure.language_data(language, &scope_id)),
        };
        protocol::validate_analysis(&data, self.source(), language).ok()?;
        Some(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        editor::{SyntaxDocument, SyntaxStatus, syntax_contracts::LANGUAGE_CASES},
        highlight::language_from_path,
    };

    #[test]
    fn every_existing_language_exports_source_owned_component_data_with_crlf_and_unicode() {
        for &(path, source, _) in LANGUAGE_CASES {
            for source in [source.to_string(), source.replace('\n', "\r\n")] {
                let language = language_from_path(path);
                let id = format!("{language:?}").to_ascii_lowercase();
                let mut document = SyntaxDocument::new(language).unwrap();
                let (status, analysis) = document.prepare(&source, 4, || true);
                assert!(matches!(status, SyntaxStatus::Ready { .. }), "{path}");
                let analysis = analysis.unwrap();
                let data = analysis
                    .language_data(&id, |language| format!("{language:?}").to_ascii_lowercase())
                    .unwrap_or_else(|| {
                        panic!("{path}: source data failed language protocol validation")
                    });
                protocol::validate_analysis(&data, &source, &id).unwrap();
                assert_eq!(data.folds, analysis.folds());
                assert_eq!(
                    data.highlights.as_ref().unwrap().len(),
                    source.split('\n').count()
                );
            }
        }
    }
}
