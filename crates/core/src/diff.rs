//! Line-level diffing that sees line endings.
//!
//! [`line_lcs`] splits both sides into [`Line`]s that keep their ending
//! (`\n`, `\r\n`, or none), so a CRLF→LF conversion or an added/removed
//! trailing newline shows up as a change even when the visible text is
//! identical. The middle is aligned with a standard LCS table + backtrack
//! (shared with [`compute_word_diff`]'s token LCS), with a cell budget that falls back to
//! delete-all/insert-all for very large middles.

use serde::{Deserialize, Serialize};

mod render;
pub use render::*;
mod paint;
pub use paint::{
    DiffChange, DiffPaint, DiffPaintLine, DiffPaintToken, paint_inline_diff,
    paint_inline_diff_with_tokens, paint_unified_diff, paint_unified_diff_with_tokens,
    unified_diff_sources,
};

/// How a line ends: `\n`, `\r\n`, or nothing (the file's last line, if it
/// has no trailing newline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Lf,
    CrLf,
    None,
}

/// One line of text plus how it ended, borrowed from the source string.
/// Two `Line`s are equal only if both `text` and `ending` match — this is
/// what makes a CRLF/LF or trailing-newline difference show up as a change
/// even when the visible text is identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line<'a> {
    pub text: &'a str,
    pub ending: Ending,
}

/// Split `s` into `Line`s using `split_inclusive('\n')`. An empty string
/// yields zero lines (not one empty line).
pub fn split_lines(s: &str) -> Vec<Line<'_>> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split_inclusive('\n')
        .map(|piece| {
            if let Some(t) = piece.strip_suffix("\r\n") {
                Line {
                    text: t,
                    ending: Ending::CrLf,
                }
            } else if let Some(t) = piece.strip_suffix('\n') {
                Line {
                    text: t,
                    ending: Ending::Lf,
                }
            } else {
                Line {
                    text: piece,
                    ending: Ending::None,
                }
            }
        })
        .collect()
}

/// One line-level edit operation from [`line_lcs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOp<'a> {
    /// Present, unchanged, on both sides.
    Equal(Line<'a>),
    /// Removed from the old side.
    Delete(Line<'a>),
    /// Added on the new side.
    Insert(Line<'a>),
}

/// Above this many DP cells, [`line_lcs`] falls back to delete-all/insert-all
/// for the middle instead of the O(m·n) table (the same policy as
/// `MAX_WORD_DIFF_CELLS` in `compute_word_diff`, just at line granularity).
pub const MAX_LINE_DIFF_CELLS: usize = 4_000_000;

/// Line-level LCS diff between `old` and `new`, comparing each line including
/// its ending. Trims a common prefix/suffix, then runs a standard LCS table +
/// backtrack on the remaining middle, falling back to delete-all-then-insert-all
/// for the middle when `mid_old.len() * mid_new.len() > MAX_LINE_DIFF_CELLS`.
///
/// The returned ops are a minimal edit script: concatenating the lines of every
/// `Equal`/`Delete` op reproduces `old` byte-for-byte, and every `Equal`/`Insert`
/// op reproduces `new`. Within each maximal run of non-`Equal` ops, all `Delete`s
/// precede all `Insert`s, so a consumer can group a run into a "cluster" of
/// removed lines followed by added lines.
pub fn line_lcs<'a>(old: &'a str, new: &'a str) -> Vec<LineOp<'a>> {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);

    // Common prefix, then common suffix bounded to the non-prefix remainder.
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let mid_old = &old_lines[prefix..old_lines.len() - suffix];
    let mid_new = &new_lines[prefix..new_lines.len() - suffix];
    let mid_m = mid_old.len();
    let mid_n = mid_new.len();

    let mid_ops = if mid_m.saturating_mul(mid_n) > MAX_LINE_DIFF_CELLS {
        // Over budget: the minimal script for an unalignable middle is
        // delete-all then insert-all, and the O(m·n) table would be wasteful.
        let mut ops = Vec::with_capacity(mid_m + mid_n);
        for line in mid_old {
            ops.push(LineOp::Delete(*line));
        }
        for line in mid_new {
            ops.push(LineOp::Insert(*line));
        }
        ops
    } else if mid_m == 0 && mid_n == 0 {
        Vec::new()
    } else {
        let mut ops = Vec::with_capacity(mid_m + mid_n);
        let (mut old_index, mut new_index) = (0, 0);
        for (old_match, new_match) in lcs_matches(mid_old, mid_new) {
            ops.extend(
                mid_old[old_index..old_match]
                    .iter()
                    .copied()
                    .map(LineOp::Delete),
            );
            ops.extend(
                mid_new[new_index..new_match]
                    .iter()
                    .copied()
                    .map(LineOp::Insert),
            );
            ops.push(LineOp::Equal(mid_old[old_match]));
            old_index = old_match + 1;
            new_index = new_match + 1;
        }
        ops.extend(mid_old[old_index..].iter().copied().map(LineOp::Delete));
        ops.extend(mid_new[new_index..].iter().copied().map(LineOp::Insert));
        ops
    };

    let mut ops = Vec::with_capacity(prefix + mid_ops.len() + suffix);
    for line in &old_lines[..prefix] {
        ops.push(LineOp::Equal(*line));
    }
    ops.extend(mid_ops);
    for line in &old_lines[old_lines.len() - suffix..] {
        ops.push(LineOp::Equal(*line));
    }
    ops
}

/// Matching indices in source order. Callers trim common edges and enforce their own cell budget.
/// Prefer deleting from the old side on ties, preserving the existing line/word alignment.
fn lcs_matches<T: PartialEq>(old: &[T], new: &[T]) -> Vec<(usize, usize)> {
    let stride = new.len() + 1;
    let mut dp = vec![0u32; (old.len() + 1) * stride];
    for i in 1..=old.len() {
        for j in 1..=new.len() {
            dp[i * stride + j] = if old[i - 1] == new[j - 1] {
                dp[(i - 1) * stride + j - 1] + 1
            } else {
                dp[(i - 1) * stride + j].max(dp[i * stride + j - 1])
            };
        }
    }
    let (mut i, mut j) = (old.len(), new.len());
    let mut matches = Vec::new();
    while i > 0 && j > 0 {
        if old[i - 1] == new[j - 1] {
            matches.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[(i - 1) * stride + j] >= dp[i * stride + j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    matches.reverse();
    matches
}

/// A note for a paired line whose ending differs between old and new, or
/// `None` if the endings match. Any ending transition involving `Ending::None`
/// (a trailing newline added or removed) reports as "no newline at end of
/// file"; a CRLF/LF flip reports the direction.
pub fn ending_note_for(old: Ending, new: Ending) -> Option<&'static str> {
    if old == new {
        return None;
    }
    match (old, new) {
        (Ending::None, _) | (_, Ending::None) => Some("no newline at end of file"),
        (Ending::CrLf, Ending::Lf) => Some("⏎ CRLF → LF"),
        (Ending::Lf, Ending::CrLf) => Some("⏎ LF → CRLF"),
        _ => None,
    }
}

/// A file edit produced by the agent, for diff rendering in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    /// Previous contents; `None` when the file is new.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    pub new: String,
    /// Whether an existing file was overwritten but could not be read as text; `old` is `None`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub old_unavailable: bool,
    /// Backup path of the original file, if overwritten while unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
}

/// Current review state of a persisted project edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditDecision {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedEdit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<crate::RewindFile>,
    pub project_id: i64,
    pub path: String,
    pub revision: i64,
    pub decision: EditDecision,
    pub diff: FileDiff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveEditRequest {
    pub path: String,
    pub revision: i64,
    pub decision: EditDecision,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_keeps_endings() {
        assert_eq!(
            split_lines("a\nb\r\nc"),
            vec![
                Line {
                    text: "a",
                    ending: Ending::Lf
                },
                Line {
                    text: "b",
                    ending: Ending::CrLf
                },
                Line {
                    text: "c",
                    ending: Ending::None
                }
            ]
        );
        assert!(split_lines("").is_empty());
    }

    #[test]
    fn crlf_to_lf_is_one_change_with_note() {
        // Only the ending differs, so the single line is one change; the
        // note reports the CRLF -> LF direction.
        let ops = line_lcs("a\r\n", "a\n");
        assert_eq!(
            ops,
            vec![
                LineOp::Delete(Line {
                    text: "a",
                    ending: Ending::CrLf
                }),
                LineOp::Insert(Line {
                    text: "a",
                    ending: Ending::Lf
                })
            ]
        );
        assert_eq!(
            ending_note_for(Ending::CrLf, Ending::Lf),
            Some("⏎ CRLF → LF")
        );
        assert_eq!(
            ending_note_for(Ending::Lf, Ending::CrLf),
            Some("⏎ LF → CRLF")
        );
        assert_eq!(ending_note_for(Ending::Lf, Ending::Lf), None);
    }

    #[test]
    fn trailing_newline_change_reports_no_newline_note() {
        // Adding a final newline: the last line's ending goes None -> Lf.
        let ops = line_lcs("a\nb", "a\nb\n");
        assert_eq!(
            ops,
            vec![
                LineOp::Equal(Line {
                    text: "a",
                    ending: Ending::Lf
                }),
                LineOp::Delete(Line {
                    text: "b",
                    ending: Ending::None
                }),
                LineOp::Insert(Line {
                    text: "b",
                    ending: Ending::Lf
                })
            ]
        );
        assert_eq!(
            ending_note_for(Ending::None, Ending::Lf),
            Some("no newline at end of file")
        );
        // Removing a final newline: Lf -> None.
        assert_eq!(
            ending_note_for(Ending::Lf, Ending::None),
            Some("no newline at end of file")
        );
    }

    #[test]
    fn inserted_line_at_top_is_one_insert_rest_equal() {
        // The core bug fix: an inserted line near the top must not mark
        // everything downstream as changed.
        let ops = line_lcs("a\nb\nc\nd", "x\na\nb\nc\nd");
        assert_eq!(
            ops,
            vec![
                LineOp::Insert(Line {
                    text: "x",
                    ending: Ending::Lf
                }),
                LineOp::Equal(Line {
                    text: "a",
                    ending: Ending::Lf
                }),
                LineOp::Equal(Line {
                    text: "b",
                    ending: Ending::Lf
                }),
                LineOp::Equal(Line {
                    text: "c",
                    ending: Ending::Lf
                }),
                LineOp::Equal(Line {
                    text: "d",
                    ending: Ending::None
                })
            ]
        );
    }

    #[test]
    fn non_equal_runs_are_dels_then_inss() {
        // A plain replacement backtracks as Insert-then-Delete; the
        // normalization must reorder it to Delete-then-Insert.
        let ops = line_lcs("a", "b");
        assert_eq!(
            ops,
            vec![
                LineOp::Delete(Line {
                    text: "a",
                    ending: Ending::None
                }),
                LineOp::Insert(Line {
                    text: "b",
                    ending: Ending::None
                })
            ]
        );
    }

    #[test]
    fn over_budget_falls_back_to_delete_all_insert_all() {
        // A few thousand lines each, sharing no lines at all, sized so the
        // middle exceeds MAX_LINE_DIFF_CELLS. The correct minimal script is
        // delete-all/insert-all regardless of path, so this exercises the
        // O(m+n) fallback (it must be fast, not the O(m·n) table).
        let n = 2_500;
        let old: String = (0..n).map(|i| format!("old_{i}\n")).collect();
        let new: String = (0..n).map(|i| format!("new_{i}\n")).collect();
        assert!(n * n > MAX_LINE_DIFF_CELLS);

        let start = std::time::Instant::now();
        let ops = line_lcs(&old, &new);
        let elapsed = start.elapsed();

        assert_eq!(ops.len(), 2 * n);
        for (k, op) in ops.iter().enumerate() {
            if k < n {
                assert!(matches!(op, LineOp::Delete(_)), "op {k} should be Delete");
            } else {
                assert!(matches!(op, LineOp::Insert(_)), "op {k} should be Insert");
            }
        }
        // The fallback is O(m+n); the O(m·n) table would be ~6.25M cells.
        // Keep the bound generous so it doesn't flake on slow CI, but tight
        // enough to catch the table actually running.
        assert!(
            elapsed.as_millis() < 2_000,
            "line_lcs took {elapsed:?}; the over-budget fallback should be fast"
        );
    }

    use proptest::prelude::*;

    /// Rebuild the owned text of a `Line` (its text plus the literal bytes of
    /// its ending) so an op list can be checked against the source string.
    fn line_to_string(l: Line) -> String {
        let mut s = String::with_capacity(l.text.len() + 2);
        s.push_str(l.text);
        match l.ending {
            Ending::Lf => s.push('\n'),
            Ending::CrLf => s.push_str("\r\n"),
            Ending::None => {}
        }
        s
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        // The fundamental invariant: applying the ops reconstructs both sides
        // byte-exact. `any::<String>()` exercises empty strings, non-ASCII,
        // and strings with/without a trailing newline.
        #[test]
        fn line_lcs_round_trips(old in any::<String>(), new in any::<String>()) {
            let ops = line_lcs(&old, &new);
            let mut reconstructed_old = String::new();
            let mut reconstructed_new = String::new();
            for op in &ops {
                match op {
                    LineOp::Equal(l) => {
                        let s = line_to_string(*l);
                        reconstructed_old.push_str(&s);
                        reconstructed_new.push_str(&s);
                    }
                    LineOp::Delete(l) => reconstructed_old.push_str(&line_to_string(*l)),
                    LineOp::Insert(l) => reconstructed_new.push_str(&line_to_string(*l)),
                }
            }
            prop_assert_eq!(reconstructed_old, old);
            prop_assert_eq!(reconstructed_new, new);
        }

        // `compute_word_diff`'s two chunk vectors each rejoin to their input.
        #[test]
        fn word_diff_rejoins(old_line in any::<String>(), new_line in any::<String>()) {
            let (old_chunks, new_chunks) = crate::compute_word_diff(&old_line, &new_line);
            let reconstructed_old: String = old_chunks.iter().map(crate::DiffChunk::text).collect();
            let reconstructed_new: String = new_chunks.iter().map(crate::DiffChunk::text).collect();
            prop_assert_eq!(reconstructed_old, old_line);
            prop_assert_eq!(reconstructed_new, new_line);
        }
    }
}
