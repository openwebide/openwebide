//! Source-coordinate composition for independently installed embedded providers.
use super::*;
use std::ops::Range;

/// Reuse the immutable source's row index across all embedded results.
pub struct LanguageComposition<'a> {
    source: &'a str,
    rows: Vec<Range<usize>>,
}
impl<'a> LanguageComposition<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut start = 0;
        let rows = source
            .split('\n')
            .map(|row| {
                let range = start..start + row.len();
                start += row.len() + 1;
                range
            })
            .collect();
        Self { source, rows }
    }

    /// Clear the owner's analysis within a delegated body, or replace it with
    /// validated child data. Delimiters and containing selections stay owned by
    /// the outer language. Missing providers leave a plain, unconfigured scope.
    pub fn replace(
        &self,
        analysis: &mut SyntaxAnalysis,
        scope: &LanguageScope,
        child: Option<&SyntaxAnalysis>,
    ) -> Result<(), String> {
        let range = scope.range;
        let body = self
            .source
            .get(range.start..range.end)
            .ok_or("Embedded language range is outside its source")?;
        if !super::identifier(&scope.language) {
            return Err("Embedded language identity is invalid".into());
        }
        if let Some(child) = child {
            validate_analysis(child, body, &scope.language)?;
        }
        let first_row = self
            .rows
            .partition_point(|row| row.start <= range.start)
            .saturating_sub(1);
        let last_row = self
            .rows
            .partition_point(|row| row.start <= range.end)
            .saturating_sub(1);
        if analysis.highlights.is_none() && child.is_some_and(|child| child.highlights.is_some()) {
            analysis.highlights = Some(
                self.rows
                    .iter()
                    .map(|row| {
                        if row.is_empty() {
                            vec![]
                        } else {
                            vec![TokenSpan {
                                end: row.len(),
                                kind: TokenKind::Plain,
                            }]
                        }
                    })
                    .collect(),
            );
        }
        if let Some(highlights) = &mut analysis.highlights {
            for (row_index, row_highlights) in highlights
                .iter_mut()
                .enumerate()
                .take(last_row + 1)
                .skip(first_row)
            {
                let row = &self.rows[row_index];
                let from = range.start.max(row.start).min(row.end) - row.start;
                let to = range.end.min(row.end).max(row.start) - row.start;
                if from >= to {
                    continue;
                }
                let previous = &*row_highlights;
                let mut next = Vec::new();
                let mut start = 0;
                for span in previous {
                    if start < from {
                        append(&mut next, span.end.min(from), span.kind);
                    }
                    start = span.end;
                }
                if let Some(spans) = child
                    .and_then(|child| child.highlights.as_ref())
                    .and_then(|rows| rows.get(row_index - first_row))
                {
                    let shift = if row_index == first_row { from } else { 0 };
                    for span in spans {
                        append(&mut next, (shift + span.end).min(to), span.kind);
                    }
                } else {
                    append(&mut next, to, TokenKind::Plain);
                }
                for span in previous {
                    if span.end > to {
                        append(&mut next, span.end, span.kind);
                    }
                }
                *row_highlights = next;
            }
        }
        analysis.folds.retain(|fold| {
            self.rows[fold.start_line].start < range.start
                || self.rows[fold.end_line].end > range.end
        });
        if let Some(child) = child {
            analysis
                .folds
                .extend(child.folds.iter().map(|fold| FoldRange {
                    start_line: first_row + fold.start_line,
                    end_line: first_row + fold.end_line,
                }));
            analysis
                .folds
                .sort_unstable_by_key(|fold| (fold.start_line, fold.end_line));
            analysis.folds.dedup();
        }
        let Some(structure) = &mut analysis.structure else {
            return Err("Embedded language analysis requires an outer structure".into());
        };
        let inside = |offset| range.start <= offset && offset < range.end;
        structure.brackets.retain(|bracket| !inside(bracket.offset));
        for bracket in &mut structure.brackets {
            if bracket.partner.is_some_and(inside) {
                bracket.partner = None;
            }
        }
        structure.opaque_starts.retain(|offset| !inside(*offset));
        structure
            .selections
            .retain(|selection| selection.start < range.start || selection.end > range.end);
        let mut protected = Vec::new();
        for region in &structure.protected {
            if region.range.end <= range.start || region.range.start >= range.end {
                protected.push(region.clone());
                continue;
            }
            if region.range.start < range.start {
                protected.push(ProtectedRegion {
                    range: TextRange {
                        start: region.range.start,
                        end: range.start,
                    },
                    complete: false,
                    kind: region.kind,
                });
            }
            if region.range.end > range.end {
                protected.push(ProtectedRegion {
                    range: TextRange {
                        start: range.end,
                        end: region.range.end,
                    },
                    complete: region.complete,
                    kind: region.kind,
                });
            }
        }
        structure.protected = protected;
        let mut scopes = Vec::new();
        for previous in &structure.scopes {
            if previous.range.end <= range.start || previous.range.start >= range.end {
                // Equal empty scopes must still be replaced, not duplicated.
                if previous.range != range {
                    scopes.push(previous.clone());
                }
                continue;
            }
            if previous.range.start < range.start {
                scopes.push(LanguageScope {
                    range: TextRange {
                        start: previous.range.start,
                        end: range.start,
                    },
                    language: previous.language.clone(),
                });
            }
            if previous.range.end > range.end {
                scopes.push(LanguageScope {
                    range: TextRange {
                        start: range.end,
                        end: previous.range.end,
                    },
                    language: previous.language.clone(),
                });
            }
        }
        let shifted = |range: TextRange| TextRange {
            start: scope.range.start + range.start,
            end: scope.range.start + range.end,
        };
        let mut offset = range.start;
        if let Some(child_structure) = child.and_then(|child| child.structure.as_ref()) {
            for nested in &child_structure.scopes {
                let nested_range = shifted(nested.range);
                if offset < nested_range.start {
                    scopes.push(LanguageScope {
                        range: TextRange {
                            start: offset,
                            end: nested_range.start,
                        },
                        language: scope.language.clone(),
                    });
                }
                scopes.push(LanguageScope {
                    range: nested_range,
                    language: nested.language.clone(),
                });
                offset = nested_range.end;
            }
            structure.selections.extend(
                child_structure
                    .selections
                    .iter()
                    .map(|range| shifted(*range)),
            );
            structure.opaque_starts.extend(
                child_structure
                    .opaque_starts
                    .iter()
                    .map(|offset| range.start + offset),
            );
            structure
                .protected
                .extend(
                    child_structure
                        .protected
                        .iter()
                        .map(|region| ProtectedRegion {
                            range: shifted(region.range),
                            complete: region.complete,
                            kind: region.kind,
                        }),
                );
            structure
                .brackets
                .extend(child_structure.brackets.iter().map(|bracket| Bracket {
                    offset: range.start + bracket.offset,
                    character: bracket.character,
                    partner: bracket.partner.map(|offset| range.start + offset),
                }));
        }
        if offset < range.end
            || (range.start == range.end
                && child
                    .and_then(|child| child.structure.as_ref())
                    .is_none_or(|structure| structure.scopes.is_empty()))
        {
            scopes.push(LanguageScope {
                range: TextRange {
                    start: offset,
                    end: range.end,
                },
                language: scope.language.clone(),
            });
        }
        scopes.sort_by_key(|scope| (scope.range.start, scope.range.end));
        structure.scopes = scopes;
        structure
            .selections
            .sort_by_key(|range| (range.start, range.end));
        structure.selections.dedup();
        structure.opaque_starts.sort_unstable();
        structure.opaque_starts.dedup();
        structure
            .protected
            .sort_by_key(|region| (region.range.start, region.range.end));
        structure.brackets.sort_by_key(|bracket| bracket.offset);
        Ok(())
    }
}
fn append(spans: &mut Vec<TokenSpan>, end: usize, kind: TokenKind) {
    if spans.last().is_some_and(|span| span.end >= end) || end == 0 {
        return;
    }
    if let Some(previous) = spans.last_mut()
        && previous.kind == kind
    {
        previous.end = end;
    } else {
        spans.push(TokenSpan { end, kind });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn analysis(source: &str, language: &str, kind: TokenKind) -> SyntaxAnalysis {
        SyntaxAnalysis {
            highlights: Some(
                source
                    .split('\n')
                    .map(|row| {
                        if row.is_empty() {
                            vec![]
                        } else {
                            vec![TokenSpan {
                                end: row.len(),
                                kind,
                            }]
                        }
                    })
                    .collect(),
            ),
            folds: vec![],
            structure: Some(Structure {
                language: language.into(),
                scopes: vec![],
                selections: vec![],
                opaque_starts: vec![],
                protected: vec![],
                brackets: vec![],
            }),
        }
    }
    #[test]
    fn embedded_unicode_crlf_colors_and_structure_replace_only_the_owned_body() {
        let source = "<x>(文😀)\r\nz</x>";
        let start = source.find('(').unwrap();
        let end = source.find("</x>").unwrap();
        let body = &source[start..end];
        let scope = LanguageScope {
            range: TextRange { start, end },
            language: "custom".into(),
        };
        let mut parent = analysis(source, "html", TokenKind::Keyword);
        let structure = parent.structure.as_mut().unwrap();
        structure.protected.push(ProtectedRegion {
            range: TextRange {
                start: 0,
                end: source.len(),
            },
            kind: RegionKind::Text,
            complete: true,
        });
        structure.selections.push(TextRange {
            start: 0,
            end: source.len(),
        });
        structure.brackets = vec![
            Bracket {
                offset: start,
                character: '(',
                partner: Some(end - 1),
            },
            Bracket {
                offset: end - 1,
                character: 'z',
                partner: Some(start),
            },
        ];
        let composition = LanguageComposition::new(source);
        composition.replace(&mut parent, &scope, None).unwrap();
        validate_analysis(&parent, source, "html").unwrap();
        assert!(parent.structure.as_ref().unwrap().brackets.is_empty());
        let mut child = analysis(body, "custom", TokenKind::String);
        let close = body.find(')').unwrap();
        child.structure.as_mut().unwrap().brackets = vec![
            Bracket {
                offset: 0,
                character: '(',
                partner: Some(close),
            },
            Bracket {
                offset: close,
                character: ')',
                partner: Some(0),
            },
        ];
        child
            .structure
            .as_mut()
            .unwrap()
            .protected
            .push(ProtectedRegion {
                range: TextRange {
                    start: 1,
                    end: close,
                },
                kind: RegionKind::String,
                complete: true,
            });
        child.structure.as_mut().unwrap().opaque_starts.push(1);
        composition
            .replace(&mut parent, &scope, Some(&child))
            .unwrap();
        validate_analysis(&parent, source, "html").unwrap();
        let structure = parent.structure.as_ref().unwrap();
        assert_eq!(structure.scopes, vec![scope]);
        assert_eq!(
            structure.selections,
            vec![TextRange {
                start: 0,
                end: source.len()
            }]
        );
        assert_eq!(structure.brackets[0].offset, start);
        assert_eq!(structure.brackets[0].partner, Some(start + close));
        assert_eq!(structure.opaque_starts, vec![start + 1]);
        assert_eq!(
            parent.highlights.as_ref().unwrap()[0],
            vec![
                TokenSpan {
                    end: start,
                    kind: TokenKind::Keyword
                },
                TokenSpan {
                    end: source.find('\n').unwrap(),
                    kind: TokenKind::String
                }
            ]
        );
        assert_eq!(
            parent.highlights.as_ref().unwrap()[1],
            vec![
                TokenSpan {
                    end: 1,
                    kind: TokenKind::String
                },
                TokenSpan {
                    end: 5,
                    kind: TokenKind::Keyword
                }
            ]
        );
        let scope = structure.scopes[0].clone();
        composition.replace(&mut parent, &scope, None).unwrap();
        validate_analysis(&parent, source, "html").unwrap();
        assert!(parent.structure.as_ref().unwrap().brackets.is_empty());
    }
    #[test]
    fn nested_scopes_flatten_without_overlaps_and_reject_wrong_utf8_boundaries() {
        let source = "a文😀z";
        let mut parent = analysis(source, "outer", TokenKind::Plain);
        let scope = LanguageScope {
            range: TextRange {
                start: 1,
                end: source.len() - 1,
            },
            language: "middle".into(),
        };
        let mut child = analysis("文😀", "middle", TokenKind::String);
        child
            .structure
            .as_mut()
            .unwrap()
            .scopes
            .push(LanguageScope {
                range: TextRange { start: 3, end: 7 },
                language: "inner".into(),
            });
        let composition = LanguageComposition::new(source);
        composition
            .replace(&mut parent, &scope, Some(&child))
            .unwrap();
        validate_analysis(&parent, source, "outer").unwrap();
        assert_eq!(
            parent.structure.as_ref().unwrap().scopes,
            vec![
                LanguageScope {
                    range: TextRange { start: 1, end: 4 },
                    language: "middle".into()
                },
                LanguageScope {
                    range: TextRange { start: 4, end: 8 },
                    language: "inner".into()
                }
            ]
        );
        let previous = parent.clone();
        assert!(
            composition
                .replace(
                    &mut parent,
                    &LanguageScope {
                        range: TextRange { start: 2, end: 4 },
                        language: "wrong".into()
                    },
                    None
                )
                .is_err()
        );
        assert_eq!(parent, previous);
    }
}
