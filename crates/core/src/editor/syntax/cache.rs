//! Shared preparation retention policy, independent of worker or UI runtimes.
use super::{MAX_STRUCTURE_BYTES, SyntaxAnalysis, SyntaxDocument, SyntaxStatus};
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

    pub fn prepare_input_shared(
        &mut self,
        key: K,
        language: Language,
        source: Arc<String>,
        should_continue: impl FnMut() -> bool,
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        self.prepare_with(key, language, source.len(), |document| {
            document.prepare_input_shared(source, should_continue)
        })
    }

    /// Continue the shared engine with an immutable in-process source. Hosts
    /// supply cancellation and task deadlines; no wire encoding is required.
    /// Pending documents obey the same retention limits as complete documents.
    pub fn prepare_cooperative(
        &mut self,
        key: K,
        language: Language,
        source: Arc<String>,
        tab_width: usize,
        should_continue: impl FnMut() -> bool,
        should_yield: impl FnMut() -> bool,
    ) -> Option<(SyntaxStatus, Option<Arc<SyntaxAnalysis>>)> {
        let source_len = source.len();
        let mut document = match self.take_admitted_document(&key, language, source_len) {
            Ok(document) => document,
            Err(status) => return Some((status, None)),
        };
        let result = document.prepare_cooperative(source, tab_width, should_continue, should_yield);
        if result
            .as_ref()
            .is_none_or(|(_, analysis)| analysis.is_some())
        {
            self.install_document(key, language, document, source_len);
        }
        result
    }

    fn take_admitted_document(
        &mut self,
        key: &K,
        language: Language,
        source_len: usize,
    ) -> Result<SyntaxDocument, SyntaxStatus> {
        if source_len > MAX_STRUCTURE_BYTES {
            self.remove(key);
            return Err(SyntaxStatus::TooLarge);
        }
        self.take_document(key, language)
            .ok_or(SyntaxStatus::Cancelled)
    }

    fn prepare_with(
        &mut self,
        key: K,
        language: Language,
        source_len: usize,
        prepare: impl FnOnce(&mut SyntaxDocument) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>),
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        let mut document = match self.take_admitted_document(&key, language, source_len) {
            Ok(document) => document,
            Err(status) => return (status, None),
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
    fn cooperative_cache_retains_exact_sources_and_matches_complete_analysis() {
        for (language, text) in [
            (Language::Rust, "fn main() { let value = 1; }\n".repeat(200)),
            (
                Language::Html,
                "<main><script>let value = 1;</script></main>\n".repeat(32),
            ),
            (
                Language::Markdown,
                "# Heading\n\n```rust\nfn main() {}\n```\n".repeat(32),
            ),
        ] {
            let source = Arc::new(text);
            let mut reference = SyntaxPreparations::default();
            let expected = reference.prepare_shared(1, language, source.clone(), 4, || true);
            let mut cache = SyntaxPreparations::default();
            let mut turns = 0;
            let actual = loop {
                let mut checkpoints = 0;
                let result = cache.prepare_cooperative(
                    1,
                    language,
                    source.clone(),
                    4,
                    || true,
                    || {
                        checkpoints += 1;
                        checkpoints > 4
                    },
                );
                if let Some(result) = result {
                    break result;
                }
                turns += 1;
                assert!(turns < 100_000);
                assert_eq!(cache.len(), 1);
                assert_eq!(cache.retained_source_bytes(), source.len());
            };
            assert!(turns > 0);
            assert_eq!(actual.0, expected.0);
            let actual = actual.1.unwrap();
            assert!(Arc::ptr_eq(actual.source_snapshot(), &source));
            assert!(Arc::ptr_eq(
                &cache.cached_analysis(&1, language, &source, 4).unwrap(),
                &actual
            ));
            assert_eq!(
                serde_json::to_value(actual.transfer_data()).unwrap(),
                serde_json::to_value(expected.1.unwrap().transfer_data()).unwrap(),
            );
        }
    }

    #[test]
    fn cooperative_cache_cancellation_admission_and_pending_retention_share_limits() {
        let source = Arc::new("fn main() {}\n".repeat(100));
        let mut cache = SyntaxPreparations::default();
        let saved = cache
            .prepare_shared(99, Language::Rust, source.clone(), 4, || true)
            .1
            .unwrap();
        assert!(
            cache
                .prepare_cooperative(1, Language::Html, source.clone(), 4, || true, || true)
                .is_none()
        );
        let cancelled = cache
            .prepare_cooperative(1, Language::Html, source.clone(), 4, || false, || false)
            .unwrap();
        assert_eq!(cancelled.0, SyntaxStatus::Cancelled);
        assert!(cancelled.1.is_none());
        assert_eq!(cache.len(), 1);
        assert!(Arc::ptr_eq(
            &cache
                .cached_analysis(&99, Language::Rust, &source, 4)
                .unwrap(),
            &saved
        ));
        let oversized = Arc::new("x".repeat(MAX_STRUCTURE_BYTES + 1));
        let rejected = cache
            .prepare_cooperative(
                99,
                Language::Rust,
                oversized,
                4,
                || panic!("size admission precedes cancellation"),
                || panic!("rejected source must not reach parser"),
            )
            .unwrap();
        assert_eq!(rejected.0, SyntaxStatus::TooLarge);
        assert!(rejected.1.is_none());
        assert!(cache.is_empty());
        for key in 0..MAX_SYNTAX_DOCUMENTS + 2 {
            assert!(
                cache
                    .prepare_cooperative(key, Language::Rust, source.clone(), 4, || true, || true)
                    .is_none()
            );
            assert!(cache.len() <= MAX_SYNTAX_DOCUMENTS);
            assert!(cache.retained_source_bytes() <= MAX_SYNTAX_SOURCE_BYTES);
        }
        assert_eq!(cache.len(), MAX_SYNTAX_DOCUMENTS);
        assert_eq!(
            cache.retained_source_bytes(),
            MAX_SYNTAX_DOCUMENTS * source.len()
        );
        assert!(
            !cache
                .entries
                .iter()
                .any(|entry| entry.0 == 0 || entry.0 == 1)
        );
    }

    #[test]
    fn oversized_source_rejects_before_preparation_and_releases_only_its_cached_owner() {
        let mut cache = SyntaxPreparations::default();
        let source = Arc::new("fn main() {}".to_owned());
        for key in [1, 2] {
            assert!(
                cache
                    .prepare_shared(key, Language::Rust, source.clone(), 4, || true)
                    .1
                    .is_some()
            );
        }
        let (status, analysis) =
            cache.prepare_with(1, Language::Python, MAX_STRUCTURE_BYTES + 1, |_| {
                panic!("oversized source must not reach parser preparation")
            });
        assert_eq!(status, SyntaxStatus::TooLarge);
        assert!(analysis.is_none());
        assert!(
            cache
                .cached_analysis(&1, Language::Rust, &source, 4)
                .is_none()
        );
        assert!(
            cache
                .cached_analysis(&2, Language::Rust, &source, 4)
                .is_some()
        );
        assert_eq!(cache.retained_source_bytes(), source.len());

        let oversized = Arc::new("x".repeat(MAX_STRUCTURE_BYTES + 1));
        let results = [
            cache.prepare(3, Language::Rust, &oversized, 4, || {
                panic!("size precedes cancellation")
            }),
            cache.prepare_shared(3, Language::Rust, oversized.clone(), 4, || {
                panic!("size precedes cancellation")
            }),
            cache.prepare_input_shared(3, Language::Rust, oversized, || {
                panic!("size precedes cancellation")
            }),
        ];
        for (status, analysis) in results {
            assert_eq!(status, SyntaxStatus::TooLarge);
            assert!(analysis.is_none());
        }
        assert_eq!(cache.len(), 1);
    }

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
