//! Shared preparation retention policy, independent of worker or UI runtimes.
use super::{SyntaxAnalysis, SyntaxDocument, SyntaxStatus};
use crate::highlight::Language;
use std::{collections::VecDeque, sync::Arc};

pub const MAX_SYNTAX_DOCUMENTS: usize = 8;
pub const MAX_SYNTAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

pub struct SyntaxPreparations<K> {
    entries: VecDeque<(K, Language, SyntaxDocument, usize)>,
}
impl<K> Default for SyntaxPreparations<K> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
}
impl<K: Eq> SyntaxPreparations<K> {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn retained_source_bytes(&self) -> usize {
        self.entries.iter().map(|entry| entry.3).sum()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn remove(&mut self, key: &K) {
        self.entries.retain(|entry| &entry.0 != key);
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.entries.retain(|entry| keep(&entry.0));
    }

    /// Read an existing complete analysis without parsing, touching LRU order or
    /// comparing the complete source. Other allocations require fresh preparation.
    pub fn cached_analysis(
        &self,
        key: &K,
        language: Language,
        source: &Arc<String>,
        tab_width: usize,
    ) -> Option<Arc<SyntaxAnalysis>> {
        let entry = self
            .entries
            .iter()
            .find(|entry| &entry.0 == key && entry.1 == language)?;
        let (width, analysis) = entry.2.prepared.as_ref()?;
        (*width == tab_width && Arc::ptr_eq(analysis.source_snapshot(), source))
            .then(|| analysis.clone())
    }

    pub(super) fn previous_publication(&self, key: &K) -> Option<(u32, Arc<SyntaxAnalysis>)> {
        self.entries
            .iter()
            .find(|entry| &entry.0 == key)?
            .2
            .publication
            .clone()
    }
    pub fn prepare(
        &mut self,
        key: K,
        language: Language,
        source: &str,
        tab_width: usize,
        should_continue: impl FnMut() -> bool,
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        self.prepare_with(key, language, source.len(), |document| {
            document.prepare(source, tab_width, should_continue)
        })
    }

    pub fn prepare_shared(
        &mut self,
        key: K,
        language: Language,
        source: Arc<String>,
        tab_width: usize,
        should_continue: impl FnMut() -> bool,
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        self.prepare_with(key, language, source.len(), |document| {
            document.prepare_shared(source, tab_width, should_continue)
        })
    }

    fn prepare_with(
        &mut self,
        key: K,
        language: Language,
        source_len: usize,
        prepare: impl FnOnce(&mut SyntaxDocument) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>),
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        let Some(mut document) = self.take_document(&key, language) else {
            return (SyntaxStatus::Cancelled, None);
        };
        let result = prepare(&mut document);
        if result.1.is_some() {
            self.install_document(key, language, document, source_len);
        }
        result
    }

    pub(super) fn take_document(&mut self, key: &K, language: Language) -> Option<SyntaxDocument> {
        let old = self
            .entries
            .iter()
            .position(|entry| &entry.0 == key)
            .and_then(|index| self.entries.remove(index));
        match old {
            Some((_, old_language, document, _)) if old_language == language => Some(document),
            _ => SyntaxDocument::new(language),
        }
    }

    pub(super) fn install_document(
        &mut self,
        key: K,
        language: Language,
        document: SyntaxDocument,
        source_len: usize,
    ) {
        while self.entries.len() >= MAX_SYNTAX_DOCUMENTS
            || self.retained_source_bytes().saturating_add(source_len) > MAX_SYNTAX_SOURCE_BYTES
        {
            self.entries.pop_front();
        }
        self.entries
            .push_back((key, language, document, source_len));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_analysis_requires_exact_prepared_ownership_without_changing_retention() {
        let mut cache = SyntaxPreparations::default();
        let source = Arc::new("fn main() { call(); }\n".to_owned());
        assert!(
            cache
                .cached_analysis(&1, Language::Rust, &source, 4)
                .is_none()
        );
        let (_, prepared) = cache.prepare_shared(1, Language::Rust, source.clone(), 4, || true);
        let prepared = prepared.unwrap();
        let cached = cache
            .cached_analysis(&1, Language::Rust, &source, 4)
            .unwrap();
        assert!(Arc::ptr_eq(&cached, &prepared));
        let bytes = cache.retained_source_bytes();
        assert!(
            cache
                .cached_analysis(&2, Language::Rust, &source, 4)
                .is_none()
        );
        assert!(
            cache
                .cached_analysis(&1, Language::Python, &source, 4)
                .is_none()
        );
        assert!(
            cache
                .cached_analysis(&1, Language::Rust, &source, 8)
                .is_none()
        );
        let replacement = Arc::new(source.as_ref().clone());
        assert!(
            cache
                .cached_analysis(&1, Language::Rust, &replacement, 4)
                .is_none()
        );
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.retained_source_bytes(), bytes);
        cache.remove(&1);
        assert!(
            cache
                .cached_analysis(&1, Language::Rust, &source, 4)
                .is_none()
        );
    }

    #[test]
    fn retention_is_bounded_lru_and_cancelled_entries_release_sources() {
        let mut cache = SyntaxPreparations::default();
        for key in 0..MAX_SYNTAX_DOCUMENTS {
            cache.prepare(key, Language::Rust, "fn main() {}", 4, || true);
        }
        assert_eq!(cache.len(), MAX_SYNTAX_DOCUMENTS);
        assert_eq!(
            cache
                .prepare(0, Language::Rust, "fn main() {}", 4, || true)
                .0,
            SyntaxStatus::Ready { incremental: true }
        );
        cache.prepare(99, Language::Rust, "fn main() {}", 4, || true);
        assert_eq!(
            cache
                .prepare(1, Language::Rust, "fn main() {}", 4, || true)
                .0,
            SyntaxStatus::Ready { incremental: false }
        );
        assert!(cache.retained_source_bytes() <= MAX_SYNTAX_SOURCE_BYTES);
        cache.prepare(1, Language::Rust, "fn main() {}", 4, || false);
        assert_eq!(cache.len(), MAX_SYNTAX_DOCUMENTS - 1);
        cache.retain(|key| *key == 99);
        assert_eq!(cache.len(), 1);
        cache.remove(&99);
        assert!(cache.is_empty());
    }
    #[test]
    fn aggregate_sources_and_line_counts_have_explicit_fallbacks() {
        let mut cache = SyntaxPreparations::default();
        let source = "x".repeat(super::super::MAX_STRUCTURE_BYTES);
        for key in 0..6 {
            let (status, result) = cache.prepare(key, Language::Plain, &source, 4, || true);
            assert!(matches!(status, SyntaxStatus::Ready { .. }));
            assert!(result.is_some());
            assert!(cache.retained_source_bytes() <= MAX_SYNTAX_SOURCE_BYTES);
        }
        assert_eq!(cache.len(), MAX_SYNTAX_SOURCE_BYTES / source.len());
        let (status, result) = cache.prepare(7, Language::Rust, &"\n".repeat(50_000), 4, || true);
        assert_eq!(status, SyntaxStatus::TooLarge);
        assert!(result.is_none());
        assert_eq!(cache.len(), 4);
    }
}
