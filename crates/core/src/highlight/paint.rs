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
impl<'a> Iterator for TokenWindow<'a> {
    type Item = PaintToken<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        while self.front < self.back {
            let index = self.front;
            self.front += 1;
            if self.whole {
                return Some(PaintToken {
                    kind: self.tokens[index].kind,
                    text: text(self.tokens, index, self.strip_cr),
                });
            }
            let start = self.front_byte;
            self.front_byte += text(self.tokens, index, self.strip_cr).len();
            if let Some(token) = self.clipped(index, start, self.front_byte) {
                return Some(token);
            }
        }
        None
    }
}
impl DoubleEndedIterator for TokenWindow<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        while self.front < self.back {
            self.back -= 1;
            if self.whole {
                return Some(PaintToken {
                    kind: self.tokens[self.back].kind,
                    text: text(self.tokens, self.back, self.strip_cr),
                });
            }
            let end = self.back_byte;
            self.back_byte -= text(self.tokens, self.back, self.strip_cr).len();
            if let Some(token) = self.clipped(self.back, self.back_byte, end) {
                return Some(token);
            }
        }
        None
    }
}

#[derive(Clone)]
pub struct RowPaintIter<'a>(
    std::iter::Chain<
        std::iter::Chain<TokenWindow<'a>, std::option::IntoIter<PaintToken<'a>>>,
        TokenWindow<'a>,
    >,
);
impl<'a> Iterator for RowPaintIter<'a> {
    type Item = PaintToken<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}
impl DoubleEndedIterator for RowPaintIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back()
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
impl<'a> RowPaintTokens<'a> {
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
            result.iter().count()
        };
        result
    }
    pub fn iter(&self) -> RowPaintIter<'a> {
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
        RowPaintIter(
            window(0..self.prefix, self.whole)
                .chain(self.middle)
                .chain(window(self.size - self.suffix..self.size, false)),
        )
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
    fn fixture() -> Vec<Token> {
        vec![
            Token {
                kind: TokenKind::Keyword,
                text: "const".into(),
            },
            Token {
                kind: TokenKind::Plain,
                text: " value = ".into(),
            },
            Token {
                kind: TokenKind::String,
                text: "\"文😀\"".into(),
            },
            Token {
                kind: TokenKind::Punct,
                text: ";\r".into(),
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
                text: String::new(),
            },
            Token {
                kind: TokenKind::Comment,
                text: "// hi\r".into(),
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
