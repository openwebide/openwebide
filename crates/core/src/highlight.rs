//! Shared paint tokens, file language detection and cooperative plain-source rows.
//!
//! Grammar providers supply colors after preparation. Pending, unavailable and
//! unsupported analysis renders plain text; it never guesses code categories.
//!
//! The guarantee that makes the overlay work: for each line, concatenating the
//! token texts reproduces that line exactly. No character is dropped or
//! altered, so the colored text stays pixel-aligned with the raw text.

mod paint;
pub use paint::{PaintToken, RowPaintIter, RowPaintTokens};

/// The syntactic category of a token, used to pick a display color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TokenKind {
    /// Whitespace, identifiers, and anything unclassified.
    Plain,
    Keyword,
    Type,
    String,
    Char,
    Comment,
    Number,
    Function,
    Operator,
    Punct,
    Attribute,
    Macro,
    Lifetime,
    Boolean,
}

/// One token: a run of source text plus its category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
}

/// Immutable tokens for one source row.
pub type TokenRow = std::sync::Arc<[Token]>;
/// Token rows shared by lexical updates and editor paint consumers.
pub type TokenRows = Vec<TokenRow>;

/// Transfer owned tokens into compact immutable rows without cloning their text.
pub fn share_token_rows(rows: Vec<Vec<Token>>) -> TokenRows {
    rows.into_iter().map(std::sync::Arc::from).collect()
}

/// The language a file is written in, chosen from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    Jsx,
    TypeScript,
    Tsx,
    Java,
    CSharp,
    Php,
    Json,
    Html,
    Css,
    Markdown,
    MarkdownInline,
    Ini,
    Xml,
    Shell,
    Toml,
    Yaml,
    Sql,
    C,
    Cpp,
    Go,
    Plain,
}

/// Pick a language from a file path's extension.
pub fn language_from_path(path: &str) -> Language {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    if matches!(
        name.as_str(),
        ".editorconfig"
            | ".npmrc"
            | ".yarnrc"
            | ".pypirc"
            | "pip.conf"
            | ".gitconfig"
            | ".gitmodules"
    ) {
        return Language::Ini;
    }
    if matches!(
        name.as_str(),
        "cargo.lock" | "poetry.lock" | "pdm.lock" | "uv.lock" | "pipfile" | "rust-toolchain"
    ) {
        return Language::Toml;
    }
    if matches!(name.as_str(), "composer.lock" | "pipfile.lock") {
        return Language::Json;
    }
    if name == ".env" || name.starts_with(".env.") {
        return Language::Ini;
    }
    let ext = crate::file_type::extension(path).unwrap_or_default();
    match ext.as_str() {
        "rs" => Language::Rust,
        "py" => Language::Python,
        "js" | "mjs" | "cjs" => Language::JavaScript,
        "jsx" => Language::Jsx,
        "ts" | "mts" | "cts" => Language::TypeScript,
        "tsx" => Language::Tsx,
        "java" => Language::Java,
        "cs" => Language::CSharp,
        "php" | "phtml" => Language::Php,
        "json" | "jsonc" => Language::Json,
        "html" | "htm" => Language::Html,
        "xml" | "svg" | "csproj" | "fsproj" | "vbproj" | "props" | "targets" | "config"
        | "resx" | "slnx" | "nuspec" | "ruleset" | "runsettings" | "pubxml" => Language::Xml,
        "ini" | "cfg" | "properties" => Language::Ini,
        "css" | "scss" => Language::Css,
        "md" | "markdown" => Language::Markdown,
        "sh" | "bash" | "zsh" => Language::Shell,
        "toml" => Language::Toml,
        "yaml" | "yml" => Language::Yaml,
        "sql" => Language::Sql,
        "c" | "h" => Language::C,
        "cpp" | "cc" | "cxx" | "hpp" => Language::Cpp,
        "go" => Language::Go,
        _ => Language::Plain,
    }
}

/// Long-row regression boundary; grammar admission uses its own source/work limits.
pub const MAX_HIGHLIGHT_LINE_BYTES: usize = 10_000;

/// Preserve plain source rows until grammar-backed colors are available.
///
/// The returned lines line up with `source.split('\n')`, and concatenating a
/// line's token texts reproduces that line exactly.
pub fn highlight_lines(source: &str, language: Language) -> Vec<Vec<Token>> {
    highlight_lines_while(source, language, || true)
        .expect("unconditional highlighting cannot cancel")
}

/// Preserve source rows, publishing no partial paint after cancellation.
pub fn highlight_lines_while(
    source: &str,
    language: Language,
    mut should_continue: impl FnMut() -> bool,
) -> Option<Vec<Vec<Token>>> {
    let mut lines = Vec::new();
    for line in source.split('\n') {
        if !should_continue() {
            return None;
        }
        lines.push(highlight_line(line, language));
    }
    Some(lines)
}

fn highlight_line(line: &str, _language: Language) -> Vec<Token> {
    vec![Token {
        kind: TokenKind::Plain,
        text: line.to_owned(),
    }]
}

pub const LEXICAL_BATCH_ROWS: usize = 128;
pub const LEXICAL_BATCH_BYTES: usize = 64 * 1024;
pub const LEXICAL_BATCHES_PER_FRAME: usize = 8;

/// Yield rendering after a short preparation interval, rather than imposing a
/// frame wait on every few cheap retained-row batches. Invalid clocks retain the
/// conservative fixed-batch schedule; the hard cap also bounds fast-clock work.
pub fn lexical_frame_due(batches: usize, elapsed_ms: f64) -> bool {
    if !elapsed_ms.is_finite() || elapsed_ms < 0.0 {
        return batches >= LEXICAL_BATCHES_PER_FRAME;
    }
    elapsed_ms >= 4.0 || batches >= 64
}

/// Complete plain-source paint and raw row boundaries needed for exact reuse.
#[derive(Debug)]
pub struct LexicalSnapshot {
    source: std::sync::Arc<String>,
    language: Language,
    normalize_crlf: bool,
    rows: std::sync::Arc<Vec<LexicalRow>>,
    tokens: std::sync::Arc<TokenRows>,
    retokenized_rows: usize,
}
impl LexicalSnapshot {
    #[cfg(feature = "editor-parser")]
    pub(crate) fn source_snapshot(&self) -> &std::sync::Arc<String> {
        &self.source
    }
    pub fn tokens(&self) -> &std::sync::Arc<TokenRows> {
        &self.tokens
    }
    pub const fn retokenized_rows(&self) -> usize {
        self.retokenized_rows
    }
}

#[derive(Debug)]
struct LexicalRow {
    start: usize,
    end: usize,
}

/// A source-owned plain-row job that preserves source boundaries across cooperative
/// batches. Callers can discard it on cancellation; unfinished paint is never
/// returned by finish. The synchronous driver admits whole rows; the bounded
/// driver also yields within source comparison, scanning, validation and copying.
pub struct LexicalPreparation {
    source: std::sync::Arc<String>,
    language: Language,
    normalize_crlf: bool,
    next: usize,
    rows: TokenRows,
    contexts: Vec<LexicalRow>,
    previous: Option<std::sync::Arc<LexicalSnapshot>>,
    retokenized_rows: usize,
    complete: bool,
    unchanged: bool,
    source_change: Option<crate::editor::TextChange>,
    previous_row: usize,
    pending_row: Option<preparation::LexicalRowPreparation>,
    pending_change: Option<crate::editor::TextChangePreparation>,
    #[cfg(test)]
    indexed_searches: std::cell::Cell<usize>,
    #[cfg(test)]
    boundary_scans: usize,
}
impl LexicalPreparation {
    pub fn new(source: std::sync::Arc<String>, language: Language) -> Self {
        Self {
            source,
            language,
            normalize_crlf: false,
            next: 0,
            rows: Vec::new(),
            contexts: Vec::new(),
            previous: None,
            retokenized_rows: 0,
            complete: false,
            unchanged: false,
            source_change: None,
            previous_row: 0,
            pending_row: None,
            pending_change: None,
            #[cfg(test)]
            indexed_searches: std::cell::Cell::new(0),
            #[cfg(test)]
            boundary_scans: 0,
        }
    }
    /// Match native textarea newline normalization without copying full source.
    pub fn for_textarea(source: std::sync::Arc<String>, language: Language) -> Self {
        Self {
            normalize_crlf: true,
            ..Self::new(source, language)
        }
    }
    /// Reuse only exact raw rows. Check both
    /// unchanged offsets and the total byte shift, validating every candidate;
    /// insertions, deletions and disjoint edits cannot reuse mismatching context.
    pub fn reuse(mut self, previous: std::sync::Arc<LexicalSnapshot>) -> Self {
        self = self.reuse_cooperative(previous);
        while self.pending_change.is_some() {
            self.advance_source_comparison(usize::MAX);
        }
        self
    }
    /// Retain the previous source without synchronously comparing its text.
    /// Exact unchanged allocations complete immediately; other comparisons
    /// resume within the same byte budget as plain-row preparation.
    pub fn reuse_cooperative(mut self, previous: std::sync::Arc<LexicalSnapshot>) -> Self {
        if previous.language == self.language && previous.normalize_crlf == self.normalize_crlf {
            self.next = 0;
            self.previous_row = 0;
            self.pending_row = None;
            self.pending_change = None;
            self.rows.clear();
            self.contexts.clear();
            self.retokenized_rows = 0;
            self.complete = false;
            self.unchanged = false;
            self.source_change = None;
            if std::sync::Arc::ptr_eq(&self.source, &previous.source) {
                self.unchanged = true;
                self.complete = true;
            } else {
                self.pending_change = Some(crate::editor::TextChangePreparation::default());
            }
            self.previous = Some(previous);
        }
        self
    }
    pub const fn is_complete(&self) -> bool {
        self.complete
    }
    /// Reuse a replacement already validated by shared syntax preparation or
    /// materialized from a resolver delta. Only its exact retained base can
    /// bypass comparison; other bases take ordinary cooperative validation.
    #[cfg(feature = "editor-parser")]
    pub(crate) fn reuse_validated_change(
        self,
        previous: std::sync::Arc<LexicalSnapshot>,
        base: &std::sync::Arc<String>,
        change: &crate::editor::TextChange,
    ) -> Self {
        let trusted = std::sync::Arc::ptr_eq(base, &previous.source)
            && previous.language == self.language
            && previous.normalize_crlf == self.normalize_crlf;
        let mut result = self.reuse_cooperative(previous);
        if trusted {
            result.pending_change = None;
            result.source_change = Some(change.clone());
            result.unchanged = change.range.is_empty() && change.new_end == change.range.start;
            result.complete = result.unchanged;
        }
        result
    }
    /// Whole rows outside the validated replacement retain their raw boundaries.
    /// Terminal rows may only be reused when they still terminate the new source.
    fn indexed_row(&self) -> Option<(usize, usize, bool)> {
        let previous = self.previous.as_ref()?;
        let change = self.source_change.as_ref()?;
        let prefix = self.next < change.range.start;
        let old_start = if prefix {
            self.next
        } else if self.next >= change.new_end {
            self.next
                .checked_sub(change.new_end)?
                .checked_add(change.range.end)?
        } else {
            return None;
        };
        let index = if previous
            .rows
            .get(self.previous_row)
            .is_some_and(|row| row.start == old_start)
        {
            self.previous_row
        } else {
            #[cfg(test)]
            self.indexed_searches.set(self.indexed_searches.get() + 1);
            previous
                .rows
                .binary_search_by_key(&old_start, |row| row.start)
                .ok()?
        };
        let row = &previous.rows[index];
        let end = if prefix {
            if row.end > change.range.start {
                return None;
            }
            row.end
        } else {
            row.end
                .checked_sub(change.range.end)?
                .checked_add(change.new_end)?
        };
        let newline = row.end > row.start && previous.source.as_bytes()[row.end - 1] == b'\n';
        if !newline && end != self.source.len() {
            return None;
        }
        Some((index, end, newline))
    }

    fn reusable_row(&self, end: usize) -> Option<usize> {
        let previous = self.previous.as_ref()?;
        let shifted = if self.source.len() >= previous.source.len() {
            self.next
                .checked_sub(self.source.len() - previous.source.len())
        } else {
            self.next
                .checked_add(previous.source.len() - self.source.len())
        };
        [Some(self.next), shifted]
            .into_iter()
            .flatten()
            .find_map(|start| {
                let index = previous
                    .rows
                    .binary_search_by_key(&start, |row| row.start)
                    .ok()?;
                let row = &previous.rows[index];
                (previous.source[row.start..row.end] == self.source[self.next..end])
                    .then_some(index)
            })
    }
    pub fn advance(&mut self, max_rows: usize, max_bytes: usize) -> usize {
        let mut count = 0;
        let mut bytes = 0;
        if max_bytes == 0 || max_rows == 0 {
            return 0;
        }
        while self.pending_change.is_some() {
            self.advance_source_comparison(usize::MAX);
        }
        if max_rows > 0 && self.pending_row.is_some() {
            let start = self.next;
            let count = self.advance_bounded(1, usize::MAX);
            let cost = self.next - start;
            return count
                + if count < max_rows && cost < max_bytes {
                    self.advance(max_rows - count, max_bytes - cost)
                } else {
                    0
                };
        }
        while !self.complete && count < max_rows {
            let indexed = self.indexed_row();
            let (next, newline) = indexed.map_or_else(
                || {
                    #[cfg(test)]
                    {
                        self.boundary_scans += 1;
                    }
                    let tail = &self.source[self.next..];
                    let newline = tail.find('\n');
                    (
                        self.next + newline.map_or(tail.len(), |end| end + 1),
                        newline.is_some(),
                    )
                },
                |(_, next, newline)| (next, newline),
            );
            let end = next - usize::from(newline);
            let raw = &self.source[self.next..end];
            let cost = next - self.next;
            if count > 0 && cost > max_bytes.saturating_sub(bytes) {
                break;
            }
            let line = if self.normalize_crlf && newline {
                raw.strip_suffix('\r').unwrap_or(raw)
            } else {
                raw
            };
            let reusable =
                indexed.map_or_else(|| self.reusable_row(next), |(index, _, _)| Some(index));
            let tokens = if let Some(index) = reusable {
                let previous = self.previous.as_ref().expect("matched previous row");
                previous.tokens[index].clone()
            } else {
                self.retokenized_rows += 1;
                std::sync::Arc::from(highlight_line(line, self.language))
            };
            // Exact source boundaries allow unchanged row reuse.
            // Changed/new rows recover via indexed lookup.
            if let Some(index) = indexed.map(|(index, _, _)| index).or(reusable) {
                self.previous_row = index + 1;
            }
            self.contexts.push(LexicalRow {
                start: self.next,
                end: next,
            });
            self.rows.push(tokens);
            self.complete = !newline;
            self.next = next;
            count += 1;
            bytes += cost;
        }
        count
    }
    pub fn finish(self) -> Option<TokenRows> {
        if !self.complete {
            return None;
        }
        Some(if self.unchanged {
            self.previous
                .expect("validated unchanged source")
                .tokens
                .as_ref()
                .clone()
        } else {
            self.rows
        })
    }
    pub fn finish_snapshot(self) -> Option<LexicalSnapshot> {
        if self.unchanged {
            let previous = self.previous.expect("validated unchanged source");
            return Some(LexicalSnapshot {
                source: self.source,
                language: self.language,
                normalize_crlf: self.normalize_crlf,
                rows: previous.rows.clone(),
                tokens: previous.tokens.clone(),
                retokenized_rows: 0,
            });
        }
        self.complete.then_some(LexicalSnapshot {
            source: self.source,
            language: self.language,
            normalize_crlf: self.normalize_crlf,
            rows: std::sync::Arc::new(self.contexts),
            tokens: std::sync::Arc::new(self.rows),
            retokenized_rows: self.retokenized_rows,
        })
    }
}

mod preparation;

#[cfg(test)]
mod tests {
    #[test]
    fn lexical_rendering_budget_bounds_work_without_frame_waits_for_each_small_batch() {
        for cost in [0.01, 1.5] {
            let mut elapsed = 0.0;
            let mut batches = 0;
            let mut frames = 0;
            for _ in 0..100_000_usize.div_ceil(super::LEXICAL_BATCH_ROWS) {
                elapsed += cost;
                batches += 1;
                if super::lexical_frame_due(batches, elapsed) {
                    assert!(elapsed <= 4.0 + cost);
                    assert!(batches <= 64);
                    elapsed = 0.0;
                    batches = 0;
                    frames += 1;
                }
            }
            if cost < 0.1 {
                assert!(
                    frames < 20,
                    "cheap retained rows must not wait nearly 100 frames"
                );
            } else {
                assert!(
                    frames > 200,
                    "costly batches must regularly yield rendering"
                );
            }
        }
        for elapsed in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            assert!(!super::lexical_frame_due(
                super::LEXICAL_BATCHES_PER_FRAME - 1,
                elapsed
            ));
            assert!(super::lexical_frame_due(
                super::LEXICAL_BATCHES_PER_FRAME,
                elapsed
            ));
        }
    }

    use super::*;

    #[test]
    fn cooperative_lexical_rows_preserve_context_and_newline_contracts() {
        let source = format!(
            "/* start\r\ninside 文😀\r\n*/ let value = 1;\n{}\r\ntail\r",
            "x".repeat(MAX_HIGHLIGHT_LINE_BYTES + 1)
        );
        for language in [
            Language::Rust,
            Language::TypeScript,
            Language::Tsx,
            Language::JavaScript,
            Language::Jsx,
            Language::Python,
            Language::Java,
            Language::CSharp,
            Language::Cpp,
            Language::Php,
            Language::Shell,
            Language::C,
            Language::Go,
            Language::Html,
            Language::Css,
            Language::Plain,
        ] {
            for normalize in [false, true] {
                let source = std::sync::Arc::new(source.clone());
                let mut job = if normalize {
                    LexicalPreparation::for_textarea(source.clone(), language)
                } else {
                    LexicalPreparation::new(source.clone(), language)
                };
                let mut steps = 0;
                while !job.is_complete() {
                    assert_eq!(job.advance(1, 16), 1);
                    steps += 1;
                }
                assert!(steps > 3, "fixture must cross lexical batch boundaries");
                let expected = if normalize {
                    source.replace("\r\n", "\n")
                } else {
                    source.to_string()
                };
                let snapshot = job.finish_snapshot().unwrap();
                assert!(std::sync::Arc::ptr_eq(&source, &snapshot.source));
                assert_eq!(
                    snapshot.tokens().as_ref(),
                    &share_token_rows(highlight_lines(&expected, language))
                );
            }
        }
        for source in ["", "\n", "\r\n", "\r", "a\n\n"] {
            let mut job =
                LexicalPreparation::new(std::sync::Arc::new(source.to_owned()), Language::Plain);
            while !job.is_complete() {
                job.advance(2, 8);
            }
            assert_eq!(
                job.finish().unwrap(),
                share_token_rows(highlight_lines(source, Language::Plain))
            );
        }
        let mut cancelled = LexicalPreparation::new(
            std::sync::Arc::new("/*\nunfinished\n*/".to_owned()),
            Language::Rust,
        );
        assert_eq!(cancelled.advance(1, 8), 1);
        assert!(
            cancelled.finish().is_none(),
            "partial lexical jobs cannot publish"
        );
    }

    #[test]
    fn indexed_lexical_boundaries_limit_scans_and_keep_context_propagation() {
        use std::sync::Arc;
        for ending in ["\n", "\r\n"] {
            let source =
                Arc::new(format!("head{ending}body 文😀{ending}tail{ending}").repeat(1000));
            let mut old = LexicalPreparation::for_textarea(source.clone(), Language::Rust);
            while !old.is_complete() {
                old.advance(128, usize::MAX);
            }
            let old = Arc::new(old.finish_snapshot().unwrap());
            let changed = Arc::new(source.replacen("body", "updated", 1));
            let mut job = LexicalPreparation::for_textarea(changed.clone(), Language::Rust)
                .reuse(old.clone());
            while !job.is_complete() {
                job.advance(128, usize::MAX);
            }
            assert_eq!(job.boundary_scans, 1);
            assert_eq!(job.indexed_searches.get(), 1);
            let next = job.finish_snapshot().unwrap();
            assert_eq!(next.retokenized_rows(), 1);
            assert_eq!(
                next.tokens().as_ref(),
                &share_token_rows(highlight_lines(
                    &changed.replace("\r\n", "\n"),
                    Language::Rust
                ))
            );
            assert!(Arc::ptr_eq(&next.tokens()[2999], &old.tokens()[2999]));
        }
        let source = Arc::new("/*\ninside\n*/\ntail\n/*\ninside\n*/".to_owned());
        let mut old = LexicalPreparation::new(source.clone(), Language::Rust);
        old.advance(128, usize::MAX);
        let old = Arc::new(old.finish_snapshot().unwrap());
        let changed = Arc::new(source.replacen("*/", "--", 1));
        let mut job = LexicalPreparation::new(changed.clone(), Language::Rust).reuse(old);
        job.advance(128, usize::MAX);
        assert_eq!(job.boundary_scans, 1);
        assert_eq!(job.indexed_searches.get(), 1);
        let next = job.finish_snapshot().unwrap();
        assert_eq!(next.retokenized_rows(), 1);
        assert_eq!(
            next.tokens().as_ref(),
            &share_token_rows(highlight_lines(&changed, Language::Rust))
        );
    }

    #[test]
    fn indexed_lexical_boundaries_match_fresh_rows_through_unicode_crlf_and_eof_edits() {
        use std::sync::Arc;
        for source in [
            "",
            "\n",
            "\r\n",
            "a",
            "a\nb",
            "a\nb\n",
            "文😀\r\nx",
            "/*\nx\n*/",
        ] {
            let mut old =
                LexicalPreparation::for_textarea(Arc::new(source.to_owned()), Language::Rust);
            old.advance(128, usize::MAX);
            let old = Arc::new(old.finish_snapshot().unwrap());
            let positions = source
                .char_indices()
                .map(|(offset, _)| offset)
                .chain([source.len()])
                .collect::<Vec<_>>();
            for &start in &positions {
                for &end in positions.iter().filter(|&&end| end >= start) {
                    for replacement in ["", "x", "\n", "\r\n", "/*", "*/", "文😀"] {
                        let changed = Arc::new(format!(
                            "{}{replacement}{}",
                            &source[..start],
                            &source[end..]
                        ));
                        let mut job =
                            LexicalPreparation::for_textarea(changed.clone(), Language::Rust)
                                .reuse(old.clone());
                        while !job.is_complete() {
                            job.advance(2, 8);
                        }
                        assert_eq!(
                            job.finish_snapshot().unwrap().tokens().as_ref(),
                            &share_token_rows(highlight_lines(
                                &changed.replace("\r\n", "\n"),
                                Language::Rust
                            )),
                            "{source:?} {start}..{end} {replacement:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unchanged_lexical_tables_preserve_source_settings_and_repeated_reuse() {
        use std::sync::Arc;
        let source = Arc::new("/*\r\n文😀\r\n*/\r\n".repeat(1000));
        let prepare = |source: Arc<String>| {
            let mut job = LexicalPreparation::for_textarea(source, Language::Rust);
            while !job.is_complete() {
                job.advance(128, 64 * 1024);
            }
            Arc::new(job.finish_snapshot().unwrap())
        };
        let old = prepare(source.clone());
        for current in [source.clone(), Arc::new(source.as_ref().clone())] {
            let mut job = LexicalPreparation::for_textarea(current.clone(), Language::Rust)
                .reuse(old.clone());
            assert!(job.is_complete());
            assert_eq!(job.advance(128, 64 * 1024), 0);
            let snapshot = job.finish_snapshot().unwrap();
            assert!(Arc::ptr_eq(&snapshot.source, &current));
            assert!(Arc::ptr_eq(&snapshot.rows, &old.rows));
            assert!(Arc::ptr_eq(snapshot.tokens(), old.tokens()));
            assert_eq!(snapshot.retokenized_rows(), 0);
        }
        assert_eq!(
            &LexicalPreparation::for_textarea(source.clone(), Language::Rust)
                .reuse(old.clone())
                .finish()
                .unwrap(),
            old.tokens().as_ref()
        );
        let changed = prepare(Arc::new(source.replace("文😀", "changed")));
        let mut job = LexicalPreparation::for_textarea(source.clone(), Language::Rust)
            .reuse(old.clone())
            .reuse(changed);
        assert!(!job.is_complete());
        while !job.is_complete() {
            job.advance(128, 64 * 1024);
        }
        assert_eq!(job.finish_snapshot().unwrap().tokens(), old.tokens());
        assert!(
            !LexicalPreparation::new(source.clone(), Language::Rust)
                .reuse(old.clone())
                .is_complete()
        );
        assert!(
            !LexicalPreparation::for_textarea(source, Language::Plain)
                .reuse(old)
                .is_complete()
        );
    }

    #[test]
    fn incremental_lexical_rows_reuse_only_matching_source_and_context() {
        fn prepare(
            source: &str,
            previous: Option<std::sync::Arc<LexicalSnapshot>>,
            language: Language,
        ) -> LexicalSnapshot {
            let mut job =
                LexicalPreparation::for_textarea(std::sync::Arc::new(source.to_owned()), language);
            if let Some(previous) = previous {
                job = job.reuse(previous);
            }
            while !job.is_complete() {
                job.advance(3, 40);
            }
            let result = job.finish_snapshot().unwrap();
            assert_eq!(
                result.tokens().as_ref(),
                &share_token_rows(highlight_lines(&source.replace("\r\n", "\n"), language))
            );
            result
        }
        let source = "head\r\n/*\r\ninside 文😀\r\n*/\r\ntail\r\n";
        for language in [
            Language::Rust,
            Language::TypeScript,
            Language::Tsx,
            Language::Python,
            Language::JavaScript,
            Language::Jsx,
            Language::Java,
            Language::CSharp,
            Language::Cpp,
            Language::Php,
            Language::Shell,
            Language::C,
            Language::Go,
            Language::Html,
            Language::Css,
            Language::Plain,
            Language::Sql,
            Language::Json,
            Language::Markdown,
            Language::Toml,
            Language::Yaml,
        ] {
            let old = std::sync::Arc::new(prepare(source, None, language));
            let changed = source.replace("inside 文😀", "inside revised 文😀");
            let next = prepare(&changed, Some(old.clone()), language);
            assert_eq!(next.retokenized_rows(), 1, "{language:?}");
            for row in [0, 1, 3, 4, 5] {
                assert!(
                    std::sync::Arc::ptr_eq(&old.tokens()[row], &next.tokens()[row]),
                    "unchanged row must share its token allocation: {language:?} row={row}"
                );
            }
            assert!(!std::sync::Arc::ptr_eq(&old.tokens()[2], &next.tokens()[2]));
            for changed in [
                format!("new\r\n{source}"),
                source.replace("head\r\n", ""),
                source.replace("/*", "  "),
                source.replace("*/", "  "),
                source.replace("head", "longer head").replace("tail", "x"),
                source.replace("\r\n", "\n"),
                "".into(),
                "\n".into(),
            ] {
                prepare(&changed, Some(old.clone()), language);
            }
            let unchanged = prepare(source, Some(old.clone()), language);
            assert_eq!(unchanged.retokenized_rows(), 0);
            assert!(std::sync::Arc::ptr_eq(&old.rows, &unchanged.rows));
            assert!(std::sync::Arc::ptr_eq(old.tokens(), unchanged.tokens()));
            assert!(
                old.tokens()
                    .iter()
                    .zip(unchanged.tokens().iter())
                    .all(|(old, new)| std::sync::Arc::ptr_eq(old, new))
            );
        }
        let old = std::sync::Arc::new(prepare(source, None, Language::Rust));
        let changed_context = prepare(
            &source.replace("/*", "  "),
            Some(old.clone()),
            Language::Rust,
        );
        assert_eq!(changed_context.retokenized_rows(), 1);
        let different_language = prepare(source, Some(old), Language::Plain);
        assert_eq!(
            different_language.retokenized_rows(),
            source.split('\n').count()
        );
    }

    /// The concatenation of a line's token texts must equal the original line.
    fn rejoins(source: &str, language: Language) {
        let lines = highlight_lines(source, language);
        assert_eq!(lines.len(), source.split('\n').count());
        for (line, tokens) in source.split('\n').zip(&lines) {
            let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
            assert_eq!(joined, line, "tokens must rejoin to the source line");
        }
    }

    #[test]
    fn language_from_path_uses_extension() {
        assert_eq!(language_from_path("src/main.rs"), Language::Rust);
        assert_eq!(language_from_path("a/b/app.py"), Language::Python);
        assert_eq!(language_from_path("index.js"), Language::JavaScript);
        assert_eq!(language_from_path("index.tsx"), Language::Tsx);
        assert_eq!(language_from_path("index.jsx"), Language::Jsx);
        assert_eq!(language_from_path("Main.java"), Language::Java);
        assert_eq!(language_from_path("Main.cs"), Language::CSharp);
        assert_eq!(language_from_path("index.php"), Language::Php);
        assert_eq!(language_from_path("package.json"), Language::Json);
        assert_eq!(language_from_path("index.html"), Language::Html);
        assert_eq!(language_from_path("styles.css"), Language::Css);
        assert_eq!(language_from_path("README.md"), Language::Markdown);
        assert_eq!(language_from_path("deploy.sh"), Language::Shell);
        assert_eq!(language_from_path("Cargo.toml"), Language::Toml);
        assert_eq!(language_from_path("docker-compose.yml"), Language::Yaml);
        assert_eq!(language_from_path("query.sql"), Language::Sql);
        assert_eq!(language_from_path("main.c"), Language::C);
        assert_eq!(language_from_path("main.cpp"), Language::Cpp);
        assert_eq!(language_from_path("server.go"), Language::Go);
        assert_eq!(language_from_path("Makefile"), Language::Plain);
        assert_eq!(language_from_path("src/lib.rs.bak"), Language::Plain);
        for path in ["LICENSE", "LICENSE.txt", "NOTICE", "COPYING"] {
            assert_eq!(language_from_path(path), Language::Plain);
        }
        for path in [
            ".editorconfig",
            ".npmrc",
            ".gitconfig",
            ".gitmodules",
            ".env",
            ".env.local",
            "setup.cfg",
            "gradle.properties",
        ] {
            assert_eq!(language_from_path(path), Language::Ini);
        }
        for path in [
            "NuGet.Config",
            "App.csproj",
            "App.slnx",
            "Package.nuspec",
            "Directory.Build.props",
            "pom.xml",
            "build.targets",
        ] {
            assert_eq!(language_from_path(path), Language::Xml);
        }
        assert_eq!(language_from_path("tsconfig.jsonc"), Language::Json);
        for path in [
            "Cargo.lock",
            "poetry.lock",
            "pdm.lock",
            "uv.lock",
            "Pipfile",
        ] {
            assert_eq!(language_from_path(path), Language::Toml);
        }
        for path in ["composer.lock", "Pipfile.lock"] {
            assert_eq!(language_from_path(path), Language::Json);
        }
    }

    #[test]
    fn plain_documents_never_receive_code_colors() {
        let source = "Copyright (c) 2026 Example\nPermission is hereby granted, free of charge,\n\"AS IS\", WITHOUT WARRANTY; /* prose */ # headings\n文😀";
        let rows = highlight_lines(source, Language::Plain);
        assert!(
            rows.iter()
                .flatten()
                .all(|token| token.kind == TokenKind::Plain)
        );
        assert_eq!(
            rows.iter()
                .map(|row| row
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<String>())
                .collect::<Vec<_>>()
                .join("\n"),
            source
        );
    }

    #[test]
    fn all_languages_rejoin_to_source() {
        for (src, lang) in [
            ("fn main() {}\nlet x = 'a';\n// c\n/* d */", Language::Rust),
            ("def f(x):\n    return x  # comment\n", Language::Python),
            ("const a = \"s\"; // js\n/* b */", Language::JavaScript),
            (
                "type T = { a: number };\nlet b = true;",
                Language::TypeScript,
            ),
            ("{\n  \"key\": 1, \"ok\": true\n}\n", Language::Json),
            ("plain text, no rules\n", Language::Plain),
            ("let s = \"café\"; // €\n", Language::Rust),
            ("s = \"héllo\"  # ✓\n", Language::Python),
        ] {
            rejoins(src, lang);
        }
    }

    #[test]
    fn non_ascii_symbols_never_panic_and_rejoin() {
        let languages = [
            Language::Rust,
            Language::Python,
            Language::JavaScript,
            Language::TypeScript,
            Language::Json,
            Language::Html,
            Language::Css,
            Language::Markdown,
            Language::Shell,
            Language::Toml,
            Language::Yaml,
            Language::Sql,
            Language::C,
            Language::Cpp,
            Language::Go,
            Language::Plain,
        ];
        let samples = [
            "a — b",
            "let x = 1; €",
            "→",
            "°",
            "٣",
            "😀 x",
            "#é",
            "'é'",
            "\"é\"",
            "a→=b",
        ];
        let fixture = "fn main() { let x = 1; } // comment\n";
        for &language in &languages {
            for &sample in &samples {
                rejoins(sample, language);
                rejoins(&format!("{fixture}{sample}{fixture}"), language);
            }
        }
    }

    #[test]
    fn overlong_line_is_single_plain_token() {
        let long_line = "x".repeat(MAX_HIGHLIGHT_LINE_BYTES + 1);
        let toks = highlight_lines(&long_line, Language::Rust);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].len(), 1);
        assert_eq!(toks[0][0].kind, TokenKind::Plain);
        assert_eq!(toks[0][0].text, long_line);

        // Fallback remains plain across comment-like prose and oversized rows.
        let source = format!("/* start\n{long_line}\nstill comment */ let a = 1;");
        let toks = highlight_lines(&source, Language::Rust);
        assert_eq!(toks[1].len(), 1);
        assert_eq!(toks[1][0].kind, TokenKind::Plain);
        assert_eq!(toks[1][0].text, long_line);
        assert_eq!(toks[2].first().unwrap().kind, TokenKind::Plain);
        assert_eq!(toks[2].first().unwrap().text, "still comment */ let a = 1;");
        rejoins(&source, Language::Rust);
    }

    use proptest::prelude::*;

    /// A strategy over all 16 `Language` variants (`Language` has no
    /// `Arbitrary` impl, so enumerate them explicitly).
    fn any_language() -> impl Strategy<Value = Language> {
        prop_oneof![
            Just(Language::Rust),
            Just(Language::Python),
            Just(Language::JavaScript),
            Just(Language::TypeScript),
            Just(Language::Json),
            Just(Language::Html),
            Just(Language::Css),
            Just(Language::Markdown),
            Just(Language::Shell),
            Just(Language::Toml),
            Just(Language::Yaml),
            Just(Language::Sql),
            Just(Language::C),
            Just(Language::Cpp),
            Just(Language::Go),
            Just(Language::Plain),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        // Generalises the fixed-table `all_languages_rejoin_to_source` to
        // arbitrary Unicode input across every language: concatenating a
        // line's token texts must reproduce that line exactly.
        #[test]
        fn highlight_rejoins_arbitrary(source in any::<String>(), language in any_language()) {
            rejoins(&source, language);
        }
    }
}
