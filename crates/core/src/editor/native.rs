//! Native input replay policy. Adapters supply source edits and UTF-8 selections.
//! Composition previews edit only the primary range; secondary edits commit together.
use super::native_value::NativeValue;
use super::{Document, Edit, EditError, Selection, text_change};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Source characters with the newline normalization performed by a textarea.
struct TextareaCharacters<'a>(std::str::CharIndices<'a>);
impl<'a> TextareaCharacters<'a> {
    fn new(source: &'a str) -> Self {
        Self(source.char_indices())
    }
}
impl Iterator for TextareaCharacters<'_> {
    type Item = (Range<usize>, char);
    fn next(&mut self) -> Option<Self::Item> {
        let (start, mut ch) = self.0.next()?;
        let mut end = start + ch.len_utf8();
        if ch == '\r' {
            if self.0.as_str().starts_with('\n') {
                self.0.next();
                end += 1;
            }
            ch = '\n';
        }
        Some((start..end, ch))
    }
}
impl DoubleEndedIterator for TextareaCharacters<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let (mut start, mut ch) = self.0.next_back()?;
        let end = start + ch.len_utf8();
        if ch == '\n' && self.0.as_str().ends_with('\r') {
            self.0.next_back();
            start -= 1;
        } else if ch == '\r' {
            ch = '\n';
        }
        Some((start..end, ch))
    }
}

/// Compare complete native text without materializing normalized source copies.
pub fn textarea_value_matches(source: &str, value: &str) -> bool {
    matching_textarea_bytes(source, value, false) == (source.len(), value.len())
}

pub(super) fn textarea_text(source: &str) -> String {
    let mut text = String::with_capacity(source.len());
    for (_, ch) in TextareaCharacters::new(source) {
        text.push(ch);
    }
    text
}

// Raw chunks are safe only when they contain complete characters and no source
// normalization. A reverse chunk must also avoid splitting a CRLF pair.
fn matching_textarea_chunk(source: &str, value: &str, reverse: bool) -> usize {
    let mut length = source.len().min(value.len()).min(64);
    if reverse {
        if let Some(carriage) = source.as_bytes()[source.len() - length..]
            .iter()
            .rposition(|byte| *byte == b'\r')
        {
            length -= carriage + 1;
        }
        if source.as_bytes().get(source.len() - length) == Some(&b'\n')
            && source
                .as_bytes()
                .get(source.len().saturating_sub(length + 1))
                == Some(&b'\r')
        {
            length = length.saturating_sub(1);
        }
    } else if let Some(carriage) = source.as_bytes()[..length]
        .iter()
        .position(|byte| *byte == b'\r')
    {
        length = carriage;
    }
    let start = |text: &str, length| if reverse { text.len() - length } else { length };
    while !source.is_char_boundary(start(source, length))
        || !value.is_char_boundary(start(value, length))
    {
        length -= 1;
    }
    let slice = |text: &str| {
        if reverse {
            text.len() - length..text.len()
        } else {
            0..length
        }
    };
    let source_range = slice(source);
    let source_chunk = &source[source_range.clone()];
    if source_chunk != &value[slice(value)] {
        return 0;
    }
    length
}

fn matching_textarea_bytes(source: &str, value: &str, reverse: bool) -> (usize, usize) {
    let mut source_chars = TextareaCharacters::new(source);
    let mut value_chars = value.chars();
    let (mut source_bytes, mut value_bytes) = (0, 0);
    loop {
        let old = source_chars.0.as_str();
        let new = value_chars.as_str();
        if old.is_empty() || new.is_empty() {
            break;
        }
        let chunk = matching_textarea_chunk(old, new, reverse);
        if chunk > 0 {
            let remaining = |text: &str| {
                if reverse {
                    0..text.len() - chunk
                } else {
                    chunk..text.len()
                }
            };
            source_chars = TextareaCharacters::new(&old[remaining(old)]);
            value_chars = new[remaining(new)].chars();
            source_bytes += chunk;
            value_bytes += chunk;
            continue;
        }
        let pair = if reverse {
            source_chars.next_back().zip(value_chars.next_back())
        } else {
            source_chars.next().zip(value_chars.next())
        };
        let Some(((_, old_char), new_char)) = pair else {
            break;
        };
        if old_char != new_char {
            break;
        }
        source_bytes += old.len() - source_chars.0.as_str().len();
        value_bytes += new_char.len_utf8();
    }
    (source_bytes, value_bytes)
}

fn textarea_change(source: &str, value: &str) -> Option<(Range<usize>, Range<usize>)> {
    let (source_start, value_start) = matching_textarea_bytes(source, value, false);
    if source_start == source.len() && value_start == value.len() {
        return None;
    }
    let (source_suffix, value_suffix) =
        matching_textarea_bytes(&source[source_start..], &value[value_start..], true);
    Some((
        source_start..source.len() - source_suffix,
        value_start..value.len() - value_suffix,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeInputKind {
    Insert,
    DeleteBackward,
    DeleteForward,
    Other,
}
impl NativeInputKind {
    pub fn from_input_type(kind: &str) -> Self {
        match kind {
            "insertText"
            | "insertFromPaste"
            | "insertFromDrop"
            | "insertReplacementText"
            | "insertFromComposition"
            | "insertCompositionText"
            | "insertLineBreak"
            | "insertParagraph" => Self::Insert,
            "deleteContentBackward"
            | "deleteWordBackward"
            | "deleteSoftLineBackward"
            | "deleteHardLineBackward" => Self::DeleteBackward,
            "deleteContentForward"
            | "deleteWordForward"
            | "deleteSoftLineForward"
            | "deleteHardLineForward" => Self::DeleteForward,
            "deleteByCut" => Self::Insert,
            _ => Self::Other,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Composition {
    before: Box<Document>,
    span: Option<Range<usize>>,
    group: Option<u64>,
}
impl Composition {
    pub(super) fn committed_document(&self) -> &Document {
        &self.before
    }
    pub(super) fn mark_saved_snapshot(&mut self, saved: std::sync::Arc<String>) {
        self.before.mark_saved_snapshot(saved);
    }
}
fn left(text: &str, at: usize, count: usize) -> usize {
    text[..at]
        .grapheme_indices(true)
        .rev()
        .nth(count.saturating_sub(1))
        .map_or(0, |(offset, _)| offset)
}
fn right(text: &str, at: usize, count: usize) -> usize {
    text[at..]
        .grapheme_indices(true)
        .nth(count)
        .map_or(text.len(), |(offset, _)| at + offset)
}
fn extent(
    before: &Document,
    candidate: &NativeValue<'_>,
    kind: NativeInputKind,
) -> Result<Range<usize>, EditError> {
    let primary = before.selections[0].range();
    if candidate.inserted_range(&before.text, &primary).is_some() {
        return Ok(primary);
    }
    let change = candidate
        .change(&before.text)
        .ok_or(EditError::UnsupportedNativeInput)?;
    let span = if primary.is_empty()
        && matches!(
            kind,
            NativeInputKind::DeleteBackward | NativeInputKind::DeleteForward
        ) {
        let count = before.text[change.range.clone()].graphemes(true).count();
        if count == 0 {
            return Err(EditError::UnsupportedNativeInput);
        }
        if kind == NativeInputKind::DeleteBackward {
            left(&before.text, primary.start, count)..primary.end
        } else {
            primary.start..right(&before.text, primary.end, count)
        }
    } else {
        change.range.start.min(primary.start)..change.range.end.max(primary.end)
    };
    candidate
        .inserted_range(&before.text, &span)
        .ok_or(EditError::UnsupportedNativeInput)?;
    Ok(span)
}
fn changes(
    before: &Document,
    span: &Range<usize>,
    text: &str,
    primary: Selection,
) -> Result<Vec<(Edit, Selection)>, EditError> {
    let selected = before.selections[0].range();
    let backwards = before.text[span.start..selected.start]
        .graphemes(true)
        .count();
    let forwards = before.text[selected.end..span.end].graphemes(true).count();
    let ranges: Vec<_> = before
        .selections
        .iter()
        .enumerate()
        .map(|(index, selection)| {
            if index == 0 {
                return span.clone();
            }
            let range = selection.range();
            let start = if backwards == 0 {
                range.start
            } else {
                left(&before.text, range.start, backwards)
            };
            let end = if forwards == 0 {
                range.end
            } else {
                right(&before.text, range.end, forwards)
            };
            start..end
        })
        .collect();
    let mut sorted = ranges.clone();
    sorted.sort_by_key(|range| (range.start, range.end));
    for pair in sorted.windows(2) {
        if pair[0].end > pair[1].start || pair[0].start == pair[1].start {
            return Err(EditError::OverlappingEdits);
        }
    }
    let retained = ranges
        .iter()
        .fold(before.text.len(), |size, range| size - range.len());
    let output = text
        .len()
        .checked_mul(ranges.len())
        .and_then(|size| size.checked_add(retained))
        .ok_or(EditError::OutputTooLarge)?;
    if output > super::MAX_DOCUMENT_BYTES {
        return Err(EditError::OutputTooLarge);
    }
    let end = span.start + text.len();
    if primary.anchor < span.start
        || primary.head < span.start
        || primary.anchor > end
        || primary.head > end
    {
        return Err(EditError::InvalidSelection);
    }
    // A primary-only IME preview must not admit a replica which cannot commit.
    // Share full-editor admission with ordinary transactions, without joining it.
    let edits: Vec<_> = sorted
        .into_iter()
        .map(|range| Edit { range, text })
        .collect();
    before.validate_editor_edits(&edits)?;
    Ok(ranges
        .into_iter()
        .enumerate()
        .map(|(index, range)| {
            let selection = if index == 0 {
                Selection {
                    anchor: primary.anchor - span.start,
                    head: primary.head - span.start,
                }
            } else {
                Selection::caret(text.len())
            };
            (Edit::replace(range, text), selection)
        })
        .collect())
}
pub(super) fn native_inserted_text(source: &str, text: &str) -> String {
    let mut inserted = textarea_text(text);
    if inserted.contains('\n')
        && source
            .split_once('\n')
            .is_some_and(|(prefix, _)| prefix.ends_with('\r'))
    {
        inserted = inserted.replace('\n', "\r\n");
    }
    inserted
}

impl Document {
    /// Replay cancellable native text at the existing selections without diffing
    /// a full textarea value. Composition remains owned by native_input.
    pub fn insert_native_text(
        &mut self,
        text: &str,
        group: Option<u64>,
    ) -> Result<bool, EditError> {
        if self.is_composing() {
            return Err(EditError::CompositionActive);
        }
        if text.len() > super::MAX_DOCUMENT_BYTES {
            return Err(EditError::OutputTooLarge);
        }
        let inserted = native_inserted_text(&self.text, text);
        self.replace_selections(&inserted, group)
    }

    /// Convert a complete normalized textarea value to a minimal source edit.
    /// Non-cancellable input and IME share insertion newline policy with direct typing.
    pub fn native_replacement(&self, value: &str) -> Option<Edit> {
        let (range, inserted) = textarea_change(&self.text, value)?;
        Some(Edit::replace(
            range,
            native_inserted_text(&self.text, &value[inserted]),
        ))
    }

    /// Convert proposed textarea UTF-16 selections using borrowed source pieces.
    /// CRLF pairs may straddle the replacement's prefix, insertion and suffix.
    pub fn native_selection_after(
        &self,
        edit: Option<&Edit>,
        selection: Selection,
    ) -> Result<Selection, EditError> {
        Ok(NativeValue::replacement(&self.text, edit)?.native_selection(selection))
    }

    pub fn is_composing(&self) -> bool {
        self.composition.is_some()
    }
    pub fn begin_composition(&mut self, group: Option<u64>) -> bool {
        if self.is_composing() {
            return false;
        }
        self.composition = Some(Box::new(Composition {
            before: Box::new(self.clone()),
            span: None,
            group,
        }));
        true
    }
    pub fn cancel_composition(&mut self) -> bool {
        self.cancel_composition_with(|_, _| ()).is_some()
    }

    /// Restore committed state before publishing, borrowing the discarded preview
    /// and restored document so adapters need not copy both complete sources.
    /// The callback runs only when a composition was active.
    pub fn cancel_composition_with<T>(
        &mut self,
        publish: impl FnOnce(&str, &Self) -> T,
    ) -> Option<T> {
        let composition = self.composition.take()?;
        let preview = std::mem::replace(self, *composition.before);
        Some(publish(preview.text(), self))
    }
    pub fn end_composition(&mut self) -> Result<bool, EditError> {
        let Some(composition) = self.composition.take() else {
            return Ok(false);
        };
        let Composition {
            before,
            span,
            group,
        } = *composition;
        let mut result = *before;
        let revision = self.revision;
        let outcome = (|| {
            let Some(span) = span else {
                return Ok(false);
            };
            let text = NativeValue::plain(&self.text).inserted_text(&result.text, &span)?;
            if result.text == self.text {
                return Ok(false);
            }
            let edits = changes(&result, &span, &text, self.selections[0])?;
            result.apply_grouped_caret_edits(edits, group)
        })();
        if matches!(outcome, Ok(true)) {
            result.revision = revision.wrapping_add(1);
        }
        *self = result;
        outcome
    }
    /// Compatibility entry point for adapters which still provide a full value.
    /// All replay policy is shared with edit-based native input.
    pub fn native_input(
        &mut self,
        candidate: &str,
        after: Selection,
        kind: NativeInputKind,
        group: Option<u64>,
    ) -> Result<bool, EditError> {
        // Reject an oversized compatibility value before copying its insertion.
        if candidate.len() > super::MAX_DOCUMENT_BYTES {
            self.cancel_composition();
            return Err(EditError::OutputTooLarge);
        }
        let edit = text_change(&self.text, candidate).map(|change| {
            let start = change.range.start;
            Edit::replace(change.range, &candidate[start..change.new_end])
        });
        self.native_edit(edit, after, kind, group)
    }

    /// Replay a native source replacement without constructing a full candidate.
    /// IME previews validate eventual secondary edits before publishing any change.
    pub fn native_edit(
        &mut self,
        edit: Option<Edit>,
        after: Selection,
        kind: NativeInputKind,
        group: Option<u64>,
    ) -> Result<bool, EditError> {
        let composition = self.composition.take();
        let outcome = self.native_edit_inner(edit, after, kind, group, composition.as_deref());
        match (outcome, composition) {
            (Err(error), Some(composition)) => {
                *self = *composition.before;
                Err(error)
            }
            (Ok((changed, span)), Some(mut composition)) => {
                if composition.span.is_none() && changed {
                    composition.span = span;
                }
                self.composition = Some(composition);
                Ok(changed)
            }
            (outcome, None) => outcome.map(|(changed, _)| changed),
        }
    }

    fn native_edit_inner(
        &mut self,
        edit: Option<Edit>,
        after: Selection,
        kind: NativeInputKind,
        group: Option<u64>,
        composition: Option<&Composition>,
    ) -> Result<(bool, Option<Range<usize>>), EditError> {
        let candidate = NativeValue::replacement(&self.text, edit.as_ref())?;
        if candidate.len() > super::MAX_DOCUMENT_BYTES {
            return Err(EditError::OutputTooLarge);
        }
        candidate.validate_selection(after)?;
        if let Some(composition) = composition {
            let before = &composition.before;
            let span = match &composition.span {
                Some(span) => span.clone(),
                None => extent(before, &candidate, NativeInputKind::Insert)?,
            };
            let text = candidate.inserted_text(&before.text, &span)?;
            // Reject invalid eventual replicas before publishing the primary preview.
            changes(before, &span, &text, after)?;
            let mut selections = vec![after];
            for selection in before.selections.iter().skip(1) {
                let map = |at| {
                    if at <= span.start {
                        at
                    } else if at >= span.end {
                        span.start + text.len() + at - span.end
                    } else {
                        span.start + text.len()
                    }
                };
                selections.push(Selection {
                    anchor: map(selection.anchor),
                    head: map(selection.head),
                });
            }
            let changed = self.apply(edit.into_iter().collect(), selections, composition.group)?;
            return Ok((changed, Some(span)));
        }
        let unchanged = candidate.matches(0..candidate.len(), &self.text);
        if self.selections.len() == 1 || unchanged {
            let selections = if unchanged && after == self.selections[0] {
                self.selections.clone()
            } else {
                vec![after]
            };
            return self
                .apply(edit.into_iter().collect(), selections, group)
                .map(|changed| (changed, None));
        }
        if kind == NativeInputKind::Other {
            return Err(EditError::UnsupportedNativeInput);
        }
        let span = extent(self, &candidate, kind)?;
        let text = candidate.inserted_text(&self.text, &span)?;
        let edits = changes(self, &span, &text, after)?;
        self.apply_grouped_caret_edits(edits, group)
            .map(|changed| (changed, None))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Document, Edit, TextareaCharacters, native_inserted_text, text_change,
        textarea_value_matches,
    };

    #[test]
    fn borrowed_textarea_changes_match_normalized_diff_and_source_offsets() {
        for source in [
            "",
            "a",
            "\r\n",
            "\r\r\n\n",
            "文😀\r\na\u{301}\rb\n",
            "same same\r\nsame",
        ] {
            let document = Document::new(source);
            let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
            let boundaries: Vec<_> = normalized
                .char_indices()
                .map(|(at, _)| at)
                .chain([normalized.len()])
                .collect();
            for (index, start) in boundaries.iter().copied().enumerate() {
                for end in boundaries[index..].iter().copied() {
                    for inserted in ["", "文😀", "\n", "\r\n", "\r", "same", "e\u{301}\n"] {
                        let mut value = normalized.clone();
                        value.replace_range(start..end, inserted);
                        let expected = text_change(&normalized, &value).map(|change| {
                            let start = super::super::textarea_to_byte(
                                source,
                                normalized[..change.range.start].encode_utf16().count(),
                            );
                            let end = super::super::textarea_to_byte(
                                source,
                                normalized[..change.range.end].encode_utf16().count(),
                            );
                            Edit::replace(
                                start..end,
                                native_inserted_text(
                                    source,
                                    &value[change.range.start..change.new_end],
                                ),
                            )
                        });
                        assert_eq!(
                            document.native_replacement(&value),
                            expected,
                            "source={source:?}, value={value:?}"
                        );
                        assert_eq!(textarea_value_matches(source, &value), normalized == value);
                    }
                }
            }
        }
    }

    #[test]
    fn chunked_textarea_changes_preserve_unicode_and_crlf_at_chunk_edges() {
        for length in [0, 1, 60, 61, 62, 63, 64, 65, 66, 67, 127, 128, 129] {
            for ending in ["", "\n", "\r\n", "\r", "\r\r\n"] {
                let source = format!(
                    "{}文😀{ending}{}😀文{ending}",
                    "x".repeat(length),
                    "y".repeat(length)
                );
                let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
                let document = Document::new(&source);
                let boundaries: Vec<_> = normalized
                    .char_indices()
                    .map(|(at, _)| at)
                    .filter(|at| *at <= 2 || at.abs_diff(length) <= 8 || normalized.len() - at <= 8)
                    .chain([normalized.len()])
                    .collect();
                for (index, &start) in boundaries.iter().enumerate() {
                    for &end in &boundaries[index..] {
                        for inserted in ["", "文", "😀", "\n", "\r\n"] {
                            let mut value = normalized.clone();
                            value.replace_range(start..end, inserted);
                            let expected = text_change(&normalized, &value).map(|change| {
                                let map = |offset| {
                                    super::super::textarea_to_byte(
                                        &source,
                                        normalized[..offset].encode_utf16().count(),
                                    )
                                };
                                Edit::replace(
                                    map(change.range.start)..map(change.range.end),
                                    native_inserted_text(
                                        &source,
                                        &value[change.range.start..change.new_end],
                                    ),
                                )
                            });
                            assert_eq!(
                                document.native_replacement(&value),
                                expected,
                                "source={source:?}, value={value:?}"
                            );
                            assert_eq!(
                                textarea_value_matches(&source, &value),
                                normalized == value
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn textarea_characters_keep_source_spans_in_both_directions() {
        let source = "\r文\r\n😀\n\r\r\n";
        let expected = vec![
            (0..1, '\n'),
            (1..4, '文'),
            (4..6, '\n'),
            (6..10, '😀'),
            (10..11, '\n'),
            (11..12, '\n'),
            (12..14, '\n'),
        ];
        assert_eq!(
            TextareaCharacters::new(source).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            TextareaCharacters::new(source).rev().collect::<Vec<_>>(),
            expected.iter().rev().cloned().collect::<Vec<_>>()
        );
        let mut characters = TextareaCharacters::new(source);
        assert_eq!(characters.next(), Some(expected[0].clone()));
        assert_eq!(characters.next_back(), Some(expected[6].clone()));
        assert_eq!(characters.next_back(), Some(expected[5].clone()));
        assert_eq!(characters.next(), Some(expected[1].clone()));
        assert_eq!(characters.collect::<Vec<_>>(), expected[2..5]);
    }

    #[test]
    fn cancellation_publishes_borrowed_preview_after_restoring_committed_history() {
        let mut document = super::Document::new("文\r\n😀");
        document.insert_native_text("a", None).unwrap();
        let committed = document.clone();
        assert!(document.begin_composition(Some(5)));
        document
            .native_edit(
                Some(super::Edit::replace(1..1, "候補")),
                super::Selection::caret(7),
                super::NativeInputKind::Insert,
                None,
            )
            .unwrap();
        let preview_address = document.text().as_ptr();
        let baseline_address = document
            .composition
            .as_ref()
            .unwrap()
            .before
            .text()
            .as_ptr();
        let result = document.cancel_composition_with(|preview, restored| {
            assert_eq!(preview.as_ptr(), preview_address);
            assert_eq!(restored.text().as_ptr(), baseline_address);
            assert_eq!(preview, "a候補文\r\n😀");
            assert_eq!(restored, &committed);
            assert!(!restored.is_composing());
            restored.selections()[0]
        });
        assert_eq!(result, Some(committed.selections()[0]));
        assert_eq!(document, committed);
        assert!(
            document
                .cancel_composition_with(|_, _| panic!("no active composition"))
                .is_none()
        );
        assert!(document.undo());
        assert_eq!(document.text(), "文\r\n😀");
    }

    #[test]
    fn composition_rejects_eventual_replica_admission_before_publishing_primary() {
        for (source, inserted, limit) in [
            (
                "x".repeat(super::super::MAX_EDITOR_LINE_BYTES - 1),
                "X",
                super::super::EditorLimit::LineBytes,
            ),
            (
                "\n".repeat(super::super::MAX_EDITOR_LINES - 2),
                "\n",
                super::super::EditorLimit::Lines,
            ),
        ] {
            let mut doc = Document::for_editor(source).unwrap();
            let end = doc.text().len();
            doc.set_selections(vec![Selection::caret(0), Selection::caret(end)])
                .unwrap();
            let before = doc.clone();
            // The primary insertion alone is within the admission boundary.
            let mut primary = before.clone();
            primary.set_selections(vec![Selection::caret(0)]).unwrap();
            primary.insert_native_text(inserted, None).unwrap();
            doc.begin_composition(Some(1));
            assert_eq!(
                doc.native_edit(
                    Some(Edit::replace(0..0, inserted)),
                    Selection::caret(inserted.len()),
                    NativeInputKind::Insert,
                    None
                ),
                Err(EditError::Capacity(limit))
            );
            assert_eq!(doc, before);
            assert!(!doc.is_composing());
            assert!(!doc.can_undo());
        }
    }

    #[test]
    fn native_edit_previews_replacement_and_rolls_back_failed_composition_frames() {
        let mut doc = Document::new("foo\r\nfoo");
        let selected = vec![
            Selection { anchor: 0, head: 3 },
            Selection { anchor: 5, head: 8 },
        ];
        doc.set_selections(selected.clone()).unwrap();
        let before = doc.clone();
        doc.begin_composition(Some(4));
        doc.native_edit(
            Some(Edit::replace(0..3, "文")),
            Selection::caret(3),
            NativeInputKind::Insert,
            None,
        )
        .unwrap();
        assert_eq!(doc.text(), "文\r\nfoo");
        doc.native_edit(
            Some(Edit::replace(0..3, "文字")),
            Selection::caret(6),
            NativeInputKind::Insert,
            None,
        )
        .unwrap();
        assert_eq!(doc.text(), "文字\r\nfoo");
        assert!(doc.end_composition().unwrap());
        assert_eq!(doc.text(), "文字\r\n文字");
        assert!(doc.undo());
        assert_eq!(doc.text(), before.text());
        assert_eq!(doc.selections(), selected);
        let before = doc.clone();
        doc.begin_composition(Some(5));
        doc.native_edit(
            Some(Edit::replace(0..3, "文")),
            Selection::caret(3),
            NativeInputKind::Insert,
            None,
        )
        .unwrap();
        assert_eq!(
            doc.native_edit(
                Some(Edit::replace(0..3, "😀")),
                Selection::caret(1),
                NativeInputKind::Insert,
                None
            ),
            Err(EditError::InvalidSelection)
        );
        assert_eq!(doc, before);
        assert!(doc.can_redo());
    }

    #[test]
    fn native_edit_replays_ambiguous_selections_and_unicode_grapheme_deletion() {
        let mut doc = Document::new("fooo\r\nfooo");
        doc.set_selections(vec![
            Selection { anchor: 2, head: 3 },
            Selection { anchor: 8, head: 9 },
        ])
        .unwrap();
        doc.native_edit(
            Some(Edit::replace(3..4, "")),
            Selection::caret(2),
            NativeInputKind::Insert,
            Some(1),
        )
        .unwrap();
        assert_eq!(doc.text(), "foo\r\nfoo");
        assert_eq!(
            doc.selections(),
            &[Selection::caret(2), Selection::caret(7)]
        );
        let mut doc = Document::new("a\u{301}\r\n😀");
        doc.set_selections(vec![Selection::caret(3), Selection::caret(9)])
            .unwrap();
        doc.native_edit(
            Some(Edit::replace(0..3, "")),
            Selection::caret(0),
            NativeInputKind::DeleteBackward,
            None,
        )
        .unwrap();
        assert_eq!(doc.text(), "\r\n");
        assert_eq!(
            doc.selections(),
            &[Selection::caret(0), Selection::caret(2)]
        );
    }

    #[test]
    fn direct_native_text_failure_keeps_editor_state_and_history() {
        use super::*;
        let mut doc =
            Document::for_editor("x".repeat(super::super::MAX_EDITOR_LINE_BYTES)).unwrap();
        doc.set_selections(vec![Selection::caret(0)]).unwrap();
        let before = doc.clone();
        assert!(matches!(
            doc.insert_native_text("x", Some(1)),
            Err(EditError::Capacity(_))
        ));
        assert_eq!(doc, before);
        assert!(!doc.undo());
    }

    #[test]
    fn direct_native_text_preserves_line_endings_grouped_history_and_composition() {
        use super::*;
        for (source, expected) in [
            ("a\r\na", "文\r\n😀a\r\n文\r\n😀a"),
            ("a\na", "文\n😀a\n文\n😀a"),
        ] {
            let mut doc = Document::new(source);
            doc.set_selections(vec![
                Selection::caret(0),
                Selection::caret(source.len() - 1),
            ])
            .unwrap();
            doc.insert_native_text("文\r\n😀", Some(1)).unwrap();
            assert_eq!(doc.text(), expected);
            doc.insert_native_text("!", Some(1)).unwrap();
            assert!(doc.undo());
            assert_eq!(doc.text(), source);
            assert!(doc.redo());
            doc.begin_composition(Some(2));
            let before = doc.clone();
            assert_eq!(
                doc.insert_native_text("X", Some(2)),
                Err(EditError::CompositionActive)
            );
            assert_eq!(doc, before);
        }
    }

    use super::*;
    #[test]
    fn native_insert_uses_the_selected_occurrence_in_ambiguous_repeated_text() {
        let mut doc = Document::new("fooo\r\nfooo");
        let selected = vec![
            Selection { anchor: 2, head: 3 },
            Selection { anchor: 8, head: 9 },
        ];
        doc.set_selections(selected.clone()).unwrap();
        doc.native_input(
            "foo\r\nfooo",
            Selection::caret(2),
            NativeInputKind::Insert,
            Some(1),
        )
        .unwrap();
        assert_eq!(doc.text(), "foo\r\nfoo");
        assert_eq!(
            doc.selections(),
            &[Selection::caret(2), Selection::caret(7)]
        );
        doc.undo();
        assert_eq!(doc.text(), "fooo\r\nfooo");
        assert_eq!(doc.selections(), selected);
    }
    #[test]
    fn backward_delete_maps_graphemes_instead_of_primary_byte_lengths() {
        let mut doc = Document::new("a\u{301}\r\n😀");
        let selected = vec![Selection::caret(3), Selection::caret(9)];
        doc.set_selections(selected.clone()).unwrap();
        doc.native_input(
            "\r\n😀",
            Selection::caret(0),
            NativeInputKind::DeleteBackward,
            None,
        )
        .unwrap();
        assert_eq!(doc.text(), "\r\n");
        assert_eq!(
            doc.selections(),
            &[Selection::caret(0), Selection::caret(2)]
        );
        doc.undo();
        assert_eq!(doc.text(), "a\u{301}\r\n😀");
        assert_eq!(doc.selections(), selected);
    }
    #[test]
    fn composition_previews_only_primary_and_commits_all_in_one_undo_step() {
        let mut doc = Document::new("文 foo\r\nfoo");
        let selected = vec![
            Selection {
                anchor: 12,
                head: 9,
            },
            Selection { anchor: 4, head: 7 },
        ];
        doc.set_selections(selected.clone()).unwrap();
        assert!(doc.begin_composition(Some(4)));
        assert!(!doc.begin_composition(Some(5)));
        for (candidate, caret) in [("文 foo\r\n文", 12), ("文 foo\r\n文字", 15)] {
            doc.native_input(
                candidate,
                Selection::caret(caret),
                NativeInputKind::Insert,
                Some(4),
            )
            .unwrap();
            assert_eq!(doc.text(), candidate);
            assert_eq!(doc.selections()[1], selected[1]);
            assert!(doc.is_composing());
        }
        assert!(doc.end_composition().unwrap());
        assert!(!doc.is_composing());
        assert_eq!(doc.text(), "文 文字\r\n文字");
        assert_eq!(
            doc.selections(),
            &[Selection::caret(18), Selection::caret(10)]
        );
        assert!(doc.undo());
        assert_eq!(doc.text(), "文 foo\r\nfoo");
        assert_eq!(doc.selections(), selected);
        assert!(!doc.can_undo());
        doc.redo();
        assert_eq!(doc.text(), "文 文字\r\n文字");
    }
    #[test]
    fn composition_supports_accent_replacement_before_a_collapsed_caret() {
        let mut doc = Document::new("e\r\n😀");
        doc.set_selections(vec![Selection::caret(1), Selection::caret(7)])
            .unwrap();
        doc.begin_composition(Some(8));
        doc.native_input(
            "é\r\n😀",
            Selection::caret(2),
            NativeInputKind::Insert,
            Some(8),
        )
        .unwrap();
        assert_eq!(doc.text(), "é\r\n😀");
        doc.end_composition().unwrap();
        assert_eq!(doc.text(), "é\r\né");
        doc.undo();
        assert_eq!(doc.text(), "e\r\n😀");
    }
    #[test]
    fn cancelled_or_invalid_composition_restores_redo_and_selection_history() {
        let mut doc = Document::new("ab");
        doc.replace_selections("x", None).unwrap();
        doc.undo();
        doc.set_selections(vec![Selection::caret(0), Selection::caret(2)])
            .unwrap();
        let before = doc.clone();
        doc.begin_composition(Some(10));
        doc.native_input("文ab", Selection::caret(3), NativeInputKind::Insert, None)
            .unwrap();
        assert_eq!(doc.text(), "文ab");
        assert!(doc.cancel_composition());
        assert_eq!(doc, before);
        doc.begin_composition(Some(11));
        assert_eq!(
            doc.native_input("文ab", Selection::caret(1), NativeInputKind::Insert, None),
            Err(EditError::InvalidSelection)
        );
        assert_eq!(doc, before);
        assert!(doc.can_redo());
    }
    #[test]
    fn returning_to_original_text_ends_composition_without_a_history_step() {
        let mut doc = Document::new("foo");
        doc.set_selections(vec![Selection { anchor: 0, head: 3 }])
            .unwrap();
        let before = doc.clone();
        doc.begin_composition(None);
        doc.native_input("bar", Selection::caret(3), NativeInputKind::Insert, None)
            .unwrap();
        doc.native_input("foo", Selection::caret(3), NativeInputKind::Insert, None)
            .unwrap();
        assert!(!doc.end_composition().unwrap());
        assert_eq!(doc, before);
    }
    #[test]
    fn foreign_commands_cannot_mutate_a_composing_document() {
        let mut doc = Document::new("ab");
        doc.begin_composition(Some(1));
        let before = doc.clone();
        for command in [
            super::super::SelectionCommand::Expand,
            super::super::SelectionCommand::Shrink,
            super::super::SelectionCommand::Single,
        ] {
            assert_eq!(
                doc.selection_command(
                    command,
                    crate::highlight::Language::Rust,
                    super::super::Indentation::default()
                ),
                Err(super::super::SelectionError::Edit(
                    EditError::CompositionActive
                ))
            );
            assert_eq!(doc, before);
        }
        assert_eq!(
            doc.replace_selections("x", None),
            Err(EditError::CompositionActive)
        );
        assert_eq!(
            doc.set_selections(vec![Selection::caret(1)]),
            Err(EditError::CompositionActive)
        );
        assert!(!doc.undo());
        assert_eq!(doc, before);
    }
    #[test]
    fn saved_versions_received_during_composition_survive_cancellation() {
        let mut doc = Document::new("a");
        doc.begin_composition(Some(1));
        doc.native_input("xa", Selection::caret(1), NativeInputKind::Insert, None)
            .unwrap();
        doc.mark_saved_version("xa");
        doc.cancel_composition();
        assert_eq!(doc.text(), "a");
        assert!(doc.is_dirty());
        doc.begin_composition(Some(2));
        doc.native_input("xa", Selection::caret(1), NativeInputKind::Insert, None)
            .unwrap();
        doc.mark_saved();
        doc.cancel_composition();
        assert!(doc.is_dirty());
    }
    #[test]
    fn overlapping_native_delete_and_unknown_input_leave_everything_unchanged() {
        let mut doc = Document::new("abcdef");
        doc.set_selections(vec![Selection::caret(3), Selection::caret(4)])
            .unwrap();
        let before = doc.clone();
        assert_eq!(
            doc.native_input(
                "def",
                Selection::caret(0),
                NativeInputKind::DeleteBackward,
                None
            ),
            Err(EditError::OverlappingEdits)
        );
        assert_eq!(doc, before);
        assert_eq!(
            doc.native_input("abcxdef", Selection::caret(4), NativeInputKind::Other, None),
            Err(EditError::UnsupportedNativeInput)
        );
        assert_eq!(doc, before);
    }
}
