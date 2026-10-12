//! Complete-only byte/style comparison for immutable paragraph replay candidates.
use crate::highlight::{PaintToken, RowPaintIter, RowPaintTokens};

const BYTE_CHUNK: usize = 8192;

pub struct PaintStyleComparison<'a> {
    old: RowPaintIter<'a>,
    new: RowPaintIter<'a>,
    length: Option<RowPaintIter<'a>>,
    first_pair: bool,
    pending_old: Option<PaintToken<'a>>,
    length_left: usize,
    new_length: usize,
    suffix: bool,
    normalize_cr: bool,
    pair: Option<(&'a str, &'a str, usize)>,
    common: usize,
    done: bool,
}

impl<'a> PaintStyleComparison<'a> {
    pub fn prefix(old: RowPaintTokens<'a>, new: RowPaintTokens<'a>) -> Self {
        Self::new(old, new, false, false)
    }
    pub fn suffix(old: RowPaintTokens<'a>, new: RowPaintTokens<'a>, normalize_cr: bool) -> Self {
        Self::new(old, new, true, normalize_cr)
    }
    fn new(
        old: RowPaintTokens<'a>,
        new: RowPaintTokens<'a>,
        suffix: bool,
        normalize_cr: bool,
    ) -> Self {
        Self {
            old: old.iter(),
            new: new.iter(),
            length: suffix.then(|| new.iter()),
            first_pair: true,
            pending_old: None,
            length_left: new.len(),
            new_length: 0,
            suffix,
            normalize_cr,
            pair: None,
            common: 0,
            done: false,
        }
    }
    fn text(&self, token: PaintToken<'a>, last: bool) -> &'a str {
        if self.normalize_cr && last {
            token.text.strip_suffix('\r').unwrap_or(token.text)
        } else {
            token.text
        }
    }
    /// Each unit visits at most one underlying token (including hidden tokens)
    /// and compares at most 8 KiB of byte pairs. Identical immutable slices need
    /// no byte traversal. Partial comparisons are never published.
    pub fn advance(&mut self, units: usize) -> bool {
        for _ in 0..units {
            if self.done {
                break;
            }
            if let Some(length) = self.length.as_mut() {
                if let Some(step) = length.next_step(false) {
                    let Some(token) = step else {
                        continue;
                    };
                    self.length_left -= 1;
                    self.new_length += self.text(token, self.length_left == 0).len();
                } else {
                    self.length = None;
                }
                continue;
            }
            if self.pair.is_none() {
                if self.pending_old.is_none() {
                    match self.old.next_step(self.suffix) {
                        Some(token) => self.pending_old = token,
                        None => {
                            self.done = true;
                            break;
                        }
                    }
                    continue;
                }
                let new = match self.new.next_step(self.suffix) {
                    Some(Some(token)) => token,
                    Some(None) => continue,
                    None => {
                        self.done = true;
                        break;
                    }
                };
                let old = self.pending_old.take().unwrap();
                let a = self.text(old, self.suffix && self.first_pair);
                // Reverse traversal strips CR only on the first token. The
                // forward prefix intentionally preserves the original policy.
                let b = self.text(new, self.suffix && self.first_pair);
                self.first_pair = false;
                if old.kind != new.kind || (a.len() > 512) != (b.len() > 512) {
                    self.done = true;
                    break;
                }
                if a.as_ptr() == b.as_ptr() && a.len() == b.len() {
                    self.common += a.len();
                    continue;
                }
                self.pair = Some((a, b, 0));
            }
            let (a, b, scanned) = self.pair.unwrap();
            let limit = a.len().min(b.len());
            let end = (scanned + BYTE_CHUNK).min(limit);
            let equal = if self.suffix {
                a.as_bytes()[a.len() - end..a.len() - scanned]
                    .iter()
                    .rev()
                    .zip(b.as_bytes()[b.len() - end..b.len() - scanned].iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count()
            } else {
                a.as_bytes()[scanned..end]
                    .iter()
                    .zip(&b.as_bytes()[scanned..end])
                    .take_while(|(a, b)| a == b)
                    .count()
            };
            self.common += equal;
            if equal != end - scanned {
                self.done = true;
                break;
            }
            if end == limit {
                self.pair = None;
                if a.len() != b.len() {
                    self.done = true;
                }
            } else {
                self.pair = Some((a, b, end));
            }
        }
        self.done
    }
    pub fn finish(self) -> Option<usize> {
        self.done.then(|| {
            if self.suffix {
                self.new_length - self.common
            } else {
                self.common
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::{Token, TokenKind};
    use std::sync::Arc;

    #[test]
    fn hidden_window_seeking_yields_before_publishing_a_suffix() {
        let tokens = vec![
            Token {
                kind: TokenKind::String,
                text: Arc::new("a".into())
            };
            2000
        ];
        let old = RowPaintTokens::prepared(&tokens, false);
        let new = RowPaintTokens::current(&tokens, "a", false);
        let mut comparison = PaintStyleComparison::suffix(old, new, false);
        assert!(!comparison.advance(8));
        assert_eq!(comparison.common, 0);
        while !comparison.advance(8) {}
        assert_eq!(comparison.finish(), Some(0));
    }

    fn oracle(old: &[Token], new: &[Token], suffix: bool, normalize: bool) -> usize {
        let text = |token: &Token, last: bool| {
            let bytes = token.text.as_bytes();
            bytes.len() - usize::from(suffix && normalize && last && bytes.last() == Some(&b'\r'))
        };
        let mut common = 0;
        let new_length: usize = new
            .iter()
            .enumerate()
            .map(|(i, token)| text(token, i + 1 == new.len()))
            .sum();
        let old_iter: Box<dyn Iterator<Item = (usize, &Token)>> = if suffix {
            Box::new(old.iter().enumerate().rev())
        } else {
            Box::new(old.iter().enumerate())
        };
        let new_iter: Box<dyn Iterator<Item = (usize, &Token)>> = if suffix {
            Box::new(new.iter().enumerate().rev())
        } else {
            Box::new(new.iter().enumerate())
        };
        for ((i, a), (j, b)) in old_iter.zip(new_iter) {
            let a_len = text(a, i + 1 == old.len());
            let b_len = text(b, j + 1 == new.len());
            if a.kind != b.kind || (a_len > 512) != (b_len > 512) {
                break;
            }
            let a = &a.text.as_bytes()[..a_len];
            let b = &b.text.as_bytes()[..b_len];
            let same = if suffix {
                a.iter()
                    .rev()
                    .zip(b.iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count()
            } else {
                a.iter().zip(b.iter()).take_while(|(a, b)| a == b).count()
            };
            common += same;
            if same != a_len || same != b_len {
                break;
            }
        }
        if suffix { new_length - common } else { common }
    }
    #[test]
    fn resumable_style_comparison_matches_original_policy() {
        let sources = [
            "".into(),
            "words\r".into(),
            "é".repeat(12000),
            format!("{}ê", "é".repeat(11999)),
            "x".repeat(512),
            "x".repeat(513),
        ];
        for a in &sources {
            for b in &sources {
                for suffix in [false, true] {
                    for normalize in [false, true] {
                        for budget in [1, 3, 64] {
                            let old = [
                                Token {
                                    kind: TokenKind::String,
                                    text: Arc::new(a.clone()),
                                },
                                Token {
                                    kind: TokenKind::Plain,
                                    text: Arc::new("tail\r".into()),
                                },
                            ];
                            let new = [
                                Token {
                                    kind: TokenKind::String,
                                    text: Arc::new(b.clone()),
                                },
                                Token {
                                    kind: TokenKind::Plain,
                                    text: Arc::new("tail\r".into()),
                                },
                            ];
                            let rows = |tokens| RowPaintTokens::prepared(tokens, false);
                            let mut preparation = if suffix {
                                PaintStyleComparison::suffix(rows(&old), rows(&new), normalize)
                            } else {
                                PaintStyleComparison::prefix(rows(&old), rows(&new))
                            };
                            assert!(!preparation.advance(0));
                            while !preparation.advance(budget) {}
                            assert_eq!(
                                preparation.finish(),
                                Some(oracle(&old, &new, suffix, normalize))
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn incomplete_comparisons_stay_private_and_shared_slices_skip_byte_work() {
        let old = [Token {
            kind: TokenKind::String,
            text: Arc::new("文😀".repeat(10000)),
        }];
        let new = [Token {
            text: Arc::new((*old[0].text).clone()),
            ..old[0].clone()
        }];
        let mut pending = PaintStyleComparison::prefix(
            RowPaintTokens::prepared(&old, false),
            RowPaintTokens::prepared(&new, false),
        );
        assert!(!pending.advance(1));
        assert!(pending.finish().is_none());
        let mut shared = PaintStyleComparison::prefix(
            RowPaintTokens::prepared(&old, false),
            RowPaintTokens::prepared(&old, false),
        );
        assert!(shared.advance(4));
        assert_eq!(shared.finish(), Some(old[0].text.len()));
    }
}
