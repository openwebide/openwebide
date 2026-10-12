//! Borrowed paint spans for current text while grammar preparation is pending.
use super::{Token, TokenKind};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaintToken<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
}

#[derive(Clone, Debug)]
struct TokenWindow<'a> {
    tokens: &'a [Token],
    front: usize,
    back: usize,
    front_byte: usize,
    back_byte: usize,
    range: Range<usize>,
    strip_cr: bool,
    whole: bool,
}
fn text(tokens: &[Token], index: usize, strip_cr: bool) -> &str {
    let body = tokens[index].text.as_str();
    if strip_cr && index + 1 == tokens.len() {
        body.strip_suffix('\r').unwrap_or(body)
    } else {
        body
    }
}
impl<'a> TokenWindow<'a> {
    fn clipped(&self, index: usize, start: usize, end: usize) -> Option<PaintToken<'a>> {
        let token = &self.tokens[index];
        let body = text(self.tokens, index, self.strip_cr);
        let left = start.max(self.range.start);
        let right = end.min(self.range.end);
        (left < right).then(|| PaintToken {
            kind: token.kind,
            text: &body[left - start..right - start],
        })
    }
}
impl<'a> TokenWindow<'a> {
    fn step(&mut self, backwards: bool) -> Option<Option<PaintToken<'a>>> {
        if self.front == self.back {
            return None;
        }
        let index = if backwards {
            self.back -= 1;
            self.back
        } else {
            let index = self.front;
            self.front += 1;
            index
        };
        if self.whole {
            return Some(Some(PaintToken {
                kind: self.tokens[index].kind,
                text: text(self.tokens, index, self.strip_cr),
            }));
        }
        let length = text(self.tokens, index, self.strip_cr).len();
        let (start, end) = if backwards {
            let end = self.back_byte;
            self.back_byte -= length;
            (self.back_byte, end)
        } else {
            let start = self.front_byte;
            self.front_byte += length;
            (start, self.front_byte)
        };
        Some(self.clipped(index, start, end))
    }
}
#[derive(Clone)]
pub struct RowPaintIter<'a> {
    prefix: TokenWindow<'a>,
    middle: Option<PaintToken<'a>>,
    suffix: TokenWindow<'a>,
    remaining: Option<usize>,
}
impl<'a> RowPaintIter<'a> {
    /// Visit at most one underlying token, including a hidden or empty token.
    /// Outer None means exhausted; Some(None) is a bounded skipped-token step.
    pub fn next_step(&mut self, backwards: bool) -> Option<Option<PaintToken<'a>>> {
        if self.remaining == Some(0) {
            return None;
        }
        let step = self.step(backwards);
        if matches!(step, Some(Some(_)))
            && let Some(remaining) = &mut self.remaining
        {
            *remaining -= 1;
        }
        step
    }
    fn step(&mut self, backwards: bool) -> Option<Option<PaintToken<'a>>> {
        if backwards {
            if let Some(step) = self.suffix.step(true) {
                return Some(step);
            }
            if let Some(token) = self.middle.take() {
                return Some(Some(token));
            }
            self.prefix.step(true)
        } else {
            if let Some(step) = self.prefix.step(false) {
                return Some(step);
            }
            if let Some(token) = self.middle.take() {
                return Some(Some(token));
            }
            self.suffix.step(false)
        }
    }
}
impl<'a> Iterator for RowPaintIter<'a> {
    type Item = PaintToken<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(step) = self.next_step(false) {
            if step.is_some() {
                return step;
            }
        }
        None
    }
}
impl DoubleEndedIterator for RowPaintIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        while let Some(step) = self.next_step(true) {
            if step.is_some() {
                return step;
            }
        }
        None
    }
}

/// Paint unchanged prefix/suffix spans with their prepared colors, and only the
/// changed island plainly. All text is borrowed; no source or token table is copied.
#[derive(Clone, Debug)]
pub struct RowPaintTokens<'a> {
    tokens: &'a [Token],
    strip_cr: bool,
    size: usize,
    prefix: usize,
    suffix: usize,
    middle: Option<PaintToken<'a>>,
    length: usize,
    whole: bool,
}
/// Reconciliation metadata for one exact immutable token row and source body.
/// Hosts must retain and verify both owners before supplying it to another read.
#[derive(Clone, Debug)]
pub struct RowPaintMatch {
    size: usize,
    prefix: usize,
    suffix: usize,
    middle: bool,
    length: usize,
    whole: bool,
}
impl<'a> RowPaintTokens<'a> {
    pub fn retained_match(&self) -> RowPaintMatch {
        RowPaintMatch {
            size: self.size,
            prefix: self.prefix,
            suffix: self.suffix,
            middle: self.middle.is_some(),
            length: self.length,
            whole: self.whole,
        }
    }
    /// Restore a match only with the same token/source owners and CR policy.
    pub fn matched(
        tokens: &'a [Token],
        source: &'a str,
        strip_cr: bool,
        matched: &RowPaintMatch,
    ) -> Self {
        Self {
            tokens,
            strip_cr,
            size: matched.size,
            prefix: matched.prefix,
            suffix: matched.suffix,
            middle: matched.middle.then(|| PaintToken {
                kind: TokenKind::Plain,
                text: &source[matched.prefix..source.len() - matched.suffix],
            }),
            length: matched.length,
            whole: matched.whole,
        }
    }
    pub fn prepared(tokens: &'a [Token], strip_cr: bool) -> Self {
        Self::windows(tokens, strip_cr, 0, 0, 0, None, true)
    }
    pub fn current(tokens: &'a [Token], source: &'a str, strip_cr: bool) -> Self {
        let size: usize = (0..tokens.len())
            .map(|index| text(tokens, index, strip_cr).len())
            .sum();
        let bytes = || {
            tokens
                .iter()
                .enumerate()
                .flat_map(|(index, _)| text(tokens, index, strip_cr).bytes())
        };
        let mut prefix = bytes()
            .zip(source.bytes())
            .take_while(|(old, new)| old == new)
            .count();
        while !source.is_char_boundary(prefix) {
            prefix -= 1;
        }
        if prefix == size && prefix == source.len() {
            return Self::prepared(tokens, strip_cr);
        }
        let limit = (size - prefix).min(source.len() - prefix);
        let mut suffix = bytes()
            .rev()
            .zip(source.bytes().rev())
            .take(limit)
            .take_while(|(old, new)| old == new)
            .count();
        while !source.is_char_boundary(source.len() - suffix) {
            suffix -= 1;
        }
        let middle = &source[prefix..source.len() - suffix];
        Self::windows(
            tokens,
            strip_cr,
            size,
            prefix,
            suffix,
            (!middle.is_empty()).then_some(PaintToken {
                kind: TokenKind::Plain,
                text: middle,
            }),
            false,
        )
    }
    fn windows(
        tokens: &'a [Token],
        strip_cr: bool,
        size: usize,
        prefix: usize,
        suffix: usize,
        middle: Option<PaintToken<'a>>,
        whole: bool,
    ) -> Self {
        let mut result = Self {
            tokens,
            strip_cr,
            size,
            prefix,
            suffix,
            middle,
            length: 0,
            whole,
        };
        result.length = if whole {
            tokens.len()
        } else {
            result.iter_with_length(None).count()
        };
        result
    }
    pub fn iter(&self) -> RowPaintIter<'a> {
        self.iter_with_length(Some(self.length))
    }
    fn iter_with_length(&self, remaining: Option<usize>) -> RowPaintIter<'a> {
        let window = |range: Range<usize>, whole: bool| TokenWindow {
            tokens: self.tokens,
            front: 0,
            back: if range.is_empty() && !whole {
                0
            } else {
                self.tokens.len()
            },
            front_byte: 0,
            back_byte: self.size,
            range,
            strip_cr: self.strip_cr,
            whole,
        };
        RowPaintIter {
            prefix: window(0..self.prefix, self.whole),
            middle: self.middle,
            suffix: window(self.size - self.suffix..self.size, false),
            remaining,
        }
    }
    pub fn len(&self) -> usize {
        self.length
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_steps_preserve_clipping_and_mixed_direction_iteration() {
        let tokens: Vec<_> = (0..3000)
            .flat_map(|_| ["a", "", "文😀", "b"])
            .map(|body| Token {
                kind: TokenKind::String,
                text: std::sync::Arc::new(body.into()),
            })
            .collect();
        for source in ["x", "a changed", "x文😀b", "a文😀b"] {
            let paint = RowPaintTokens::current(&tokens, source, false);
            let mut expected = Vec::new();
            for range in [0..paint.prefix, paint.size - paint.suffix..paint.size] {
                if range.start == paint.size - paint.suffix {
                    expected.extend(paint.middle);
                }
                let mut position = 0;
                for token in &tokens {
                    let end = position + token.text.len();
                    let left = position.max(range.start);
                    let right = end.min(range.end);
                    if left < right {
                        expected.push(PaintToken {
                            kind: token.kind,
                            text: &token.text[left - position..right - position],
                        });
                    }
                    position = end;
                }
            }
            let mut iter = paint.iter();
            let mut front = 0;
            let mut back = expected.len();
            let mut backwards = false;
            loop {
                let before = [
                    iter.prefix.front,
                    iter.prefix.back,
                    iter.suffix.front,
                    iter.suffix.back,
                ];
                let step = iter.next_step(backwards);
                let after = [
                    iter.prefix.front,
                    iter.prefix.back,
                    iter.suffix.front,
                    iter.suffix.back,
                ];
                assert!(
                    before
                        .iter()
                        .zip(after)
                        .map(|(a, b)| a.abs_diff(b))
                        .sum::<usize>()
                        <= 1
                );
                match step {
                    Some(Some(token)) if backwards => {
                        back -= 1;
                        assert_eq!(token, expected[back]);
                        backwards = false;
                    }
                    Some(Some(token)) => {
                        assert_eq!(token, expected[front]);
                        front += 1;
                        backwards = true;
                    }
                    Some(None) => {}
                    None => break,
                }
            }
            assert_eq!(front, back);
        }
    }
    #[test]
    fn retained_matches_preserve_exact_spans_and_native_cr_policy() {
        let tokens = fixture();
        for current in [
            "const value = \"文😀\";",
            "coxst value = \"文😀\";",
            "",
            "文😀",
            "const value = \"文😀\";\r",
        ] {
            for strip_cr in [false, true] {
                let expected = RowPaintTokens::current(&tokens, current, strip_cr);
                let matched = expected.retained_match();
                let restored = RowPaintTokens::matched(&tokens, current, strip_cr, &matched);
                assert_eq!(restored.len(), expected.len());
                assert_eq!(
                    restored.iter().collect::<Vec<_>>(),
                    expected.iter().collect::<Vec<_>>()
                );
            }
        }
    }
    fn fixture() -> Vec<Token> {
        vec![
            Token {
                kind: TokenKind::Keyword,
                text: std::sync::Arc::new("const".into()),
            },
            Token {
                kind: TokenKind::Plain,
                text: std::sync::Arc::new(" value = ".into()),
            },
            Token {
                kind: TokenKind::String,
                text: std::sync::Arc::new("\"文😀\"".into()),
            },
            Token {
                kind: TokenKind::Punct,
                text: std::sync::Arc::new(";\r".into()),
            },
        ]
    }
    #[test]
    fn retained_spans_borrow_unchanged_colors_and_current_changed_text() {
        let tokens = fixture();
        let current = "coxst value = \"文😀\";";
        let paint = RowPaintTokens::current(&tokens, current, true);
        let spans = paint.iter().collect::<Vec<_>>();
        assert_eq!(
            spans[0],
            PaintToken {
                kind: TokenKind::Keyword,
                text: "co"
            }
        );
        assert_eq!(
            spans[1],
            PaintToken {
                kind: TokenKind::Plain,
                text: "x"
            }
        );
        assert_eq!(
            spans[2],
            PaintToken {
                kind: TokenKind::Keyword,
                text: "st"
            }
        );
        assert_eq!(spans[0].text.as_ptr(), tokens[0].text.as_ptr());
        assert_eq!(spans[1].text.as_ptr(), current[2..3].as_ptr());
        assert!(
            spans
                .iter()
                .any(|span| span.kind == TokenKind::String && span.text == "\"文😀\"")
        );
    }
    #[test]
    fn every_unicode_edit_preserves_forward_reverse_and_mixed_iteration() {
        let tokens = fixture();
        let source = "const value = \"文😀\";";
        let positions: Vec<_> = source
            .char_indices()
            .map(|(at, _)| at)
            .chain([source.len()])
            .collect();
        for &start in &positions {
            for &end in positions.iter().filter(|end| **end >= start) {
                for inserted in ["", "x", "文😀", "e\u{301}", "\t"] {
                    let current = format!("{}{inserted}{}", &source[..start], &source[end..]);
                    let paint = RowPaintTokens::current(&tokens, &current, true);
                    let spans: Vec<_> = paint.iter().collect();
                    assert_eq!(
                        spans.iter().map(|span| span.text).collect::<String>(),
                        current
                    );
                    assert_eq!(paint.len(), spans.len());
                    assert_eq!(
                        paint.iter().rev().collect::<Vec<_>>(),
                        spans.iter().copied().rev().collect::<Vec<_>>()
                    );
                    let mut mixed = paint.iter();
                    if let Some(first) = spans.first() {
                        assert_eq!(mixed.next(), Some(*first));
                        if spans.len() > 1 {
                            assert_eq!(mixed.next_back(), spans.last().copied());
                            assert_eq!(mixed.collect::<Vec<_>>(), spans[1..spans.len() - 1]);
                        } else {
                            assert!(mixed.next_back().is_none());
                        }
                    } else {
                        assert!(mixed.next().is_none());
                    }
                }
            }
        }
    }
    #[test]
    fn prepared_rows_keep_empty_tokens_and_cr_normalization() {
        let tokens = [
            Token {
                kind: TokenKind::Plain,
                text: std::sync::Arc::new(String::new()),
            },
            Token {
                kind: TokenKind::Comment,
                text: std::sync::Arc::new("// hi\r".into()),
            },
        ];
        let paint = RowPaintTokens::prepared(&tokens, true);
        assert_eq!(paint.len(), 2);
        assert_eq!(
            paint.iter().map(|span| span.text).collect::<String>(),
            "// hi"
        );
        assert_eq!(paint.iter().next_back().unwrap().text, "// hi");
        let empty = [tokens[0].clone()];
        assert_eq!(RowPaintTokens::prepared(&empty, false).iter().count(), 1);
    }
}
