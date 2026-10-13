use openwebide_core::editor::{Document, Edit, Selection};
use std::hint::black_box;
#[cfg(feature = "input-preparation")]
pub mod input;
#[derive(Clone, Debug)]
pub struct Measurement {
    pub operation: &'static str,
    pub bytes: usize,
    pub milliseconds: f64,
}
/// Measure edit/undo pairs with construction and exact-source verification outside
/// timing. This includes core allocation, admission, history and index publication,
/// but excludes browser input, syntax preparation, shaping and layout.
pub fn measure_edits(clock: impl Fn() -> f64) -> Vec<Measurement> {
    let mut records = Vec::new();
    for (pattern, rows, insert_begin, insert_middle, split_middle) in [
        (
            "word_123; ",
            104856,
            "ascii_insert_begin_20",
            "ascii_insert_middle_20",
            "ascii_split_middle_20",
        ),
        (
            "文😀e\u{301}; ",
            1048576 / 13,
            "mixed_insert_begin_20",
            "mixed_insert_middle_20",
            "mixed_split_middle_20",
        ),
        (
            "line()\n",
            99998,
            "dense_insert_begin_20",
            "dense_insert_middle_20",
            "dense_split_middle_20",
        ),
    ] {
        let source = pattern.repeat(rows);
        let middle = source.floor_char_boundary(source.len() / 2);
        for (label, at, inserted) in [
            (insert_begin, 0, "a"),
            (insert_middle, middle, "a"),
            (split_middle, middle, "\n"),
        ] {
            for _ in 0..3 {
                let mut document = Document::for_editor(source.clone()).unwrap();
                document.set_selections(vec![Selection::caret(at)]).unwrap();
                time(&mut records, &clock, label, source.len(), || {
                    for _ in 0..20 {
                        assert!(
                            document
                                .apply(
                                    vec![Edit::replace(at..at, inserted)],
                                    vec![Selection::caret(at + inserted.len())],
                                    None
                                )
                                .unwrap()
                        );
                        assert!(document.undo());
                    }
                });
                assert_eq!(document.text(), source);
                assert_eq!(
                    document.byte_to_textarea(middle),
                    openwebide_core::editor::byte_to_textarea(&source, middle)
                );
                black_box(&document);
            }
        }
    }
    records
}

fn time(
    records: &mut Vec<Measurement>,
    clock: &impl Fn() -> f64,
    label: &'static str,
    bytes: usize,
    f: impl FnOnce(),
) {
    let start = clock();
    f();
    records.push(Measurement {
        operation: label,
        bytes,
        milliseconds: clock() - start,
    });
}
/// Measure complete changed-source lexical preparation, including comparison and
/// row-table publication. Fixture creation and fresh-token verification are untimed.
pub fn measure_lexical(clock: impl Fn() -> f64) -> Vec<Measurement> {
    use openwebide_core::highlight::{Language, LexicalPreparation};
    use std::sync::Arc;
    let mut records = Vec::new();
    for size in [64 * 1024, 2 * 1024 * 1024] {
        for ending in ["\n", "\r\n"] {
            let pattern = format!("let text = \"文😀\";{ending}");
            let source = Arc::new(pattern.repeat(size / pattern.len()));
            let mut initial = LexicalPreparation::for_textarea(source.clone(), Language::Rust);
            while !initial.is_complete() {
                initial.advance(128, usize::MAX);
            }
            let previous = Arc::new(initial.finish_snapshot().unwrap());
            let middle = source[..source.len() / 2]
                .rfind('\n')
                .map_or(0, |byte| byte + 1);
            for (label, at) in [
                ("lexical_begin_20", 0),
                ("lexical_middle_20", middle),
                ("lexical_end_20", source.len()),
            ] {
                let mut changed = source.as_ref().clone();
                changed.insert_str(at, "changed ");
                let changed = Arc::new(changed);
                let prepare = || {
                    let mut job = LexicalPreparation::for_textarea(changed.clone(), Language::Rust)
                        .reuse(previous.clone());
                    while !job.is_complete() {
                        job.advance(128, 32 * 1024);
                    }
                    job.finish_snapshot().unwrap()
                };
                let expected = openwebide_core::highlight::share_token_rows(
                    openwebide_core::highlight::highlight_lines(
                        &changed.replace("\r\n", "\n"),
                        Language::Rust,
                    ),
                );
                let verified = prepare();
                assert_eq!(verified.tokens().as_ref(), &expected);
                assert_eq!(verified.retokenized_rows(), 1);
                time(&mut records, &clock, label, source.len(), || {
                    for _ in 0..20 {
                        black_box(prepare());
                    }
                });
            }
        }
    }
    records
}

/// Run identical storage workloads with a platform clock supplied by the caller.
/// Ropes are comparison dependencies only; production text storage remains String.
pub fn measure(clock: impl Fn() -> f64) -> Vec<Measurement> {
    let mut records = Vec::new();
    for size in [64 * 1024, 2 * 1024 * 1024, 16 * 1024 * 1024] {
        let pattern = "fn sample() { let text = \"文😀e\u{301}\"; }\r\n";
        let source = pattern.repeat(size / pattern.len());
        let mut at = source.len() / 2;
        while !source.is_char_boundary(at) {
            at -= 1;
        }
        for (label, position) in [
            ("source_change_100", at),
            ("source_change_late_100", source.len()),
        ] {
            let mut changed = source.clone();
            changed.insert_str(position, "changed");
            let expected = openwebide_core::editor::TextChange {
                range: position..position,
                new_end: position + 7,
            };
            assert_eq!(
                openwebide_core::editor::text_change(&source, &changed),
                Some(expected)
            );
            time(&mut records, &clock, label, source.len(), || {
                for _ in 0..100 {
                    black_box(openwebide_core::editor::text_change(
                        black_box(&source),
                        black_box(&changed),
                    ));
                }
            });
        }
        for (label, native_source) in [
            ("native_replacement_lf_100", source.replace("\r\n", "\n")),
            ("native_replacement_crlf_100", source.clone()),
        ] {
            let mut position = native_source.len() / 2;
            while !native_source.is_char_boundary(position) {
                position -= 1;
            }
            // The source offset must not split a normalized CRLF pair.
            if native_source[..position].ends_with('\r')
                && native_source[position..].starts_with('\n')
            {
                position -= 1;
            }
            let native_position = native_source[..position].replace("\r\n", "\n").len();
            let mut value = native_source.replace("\r\n", "\n");
            value.insert(native_position, '🦀');
            let document = Document::new(native_source);
            assert_eq!(
                document.native_replacement(&value),
                Some(Edit::replace(position..position, "🦀"))
            );
            time(&mut records, &clock, label, document.text().len(), || {
                for _ in 0..100 {
                    black_box(document.native_replacement(black_box(&value)));
                }
            });
        }
        let mut doc = Document::new(source.clone());
        time(
            &mut records,
            &clock,
            "document_100_edits",
            source.len(),
            || {
                for _ in 0..50 {
                    doc.apply(
                        vec![Edit::replace(at..at, "x")],
                        vec![Selection::caret(at + 1)],
                        None,
                    )
                    .unwrap();
                    doc.undo();
                }
            },
        );
        assert_eq!(doc.text(), source);
        let mut text = source.clone();
        time(
            &mut records,
            &clock,
            "string_copy_100_edits",
            source.len(),
            || {
                for _ in 0..50 {
                    text = [&text[..at], "x", &text[at..]].concat();
                    text = [&text[..at], &text[at + 1..]].concat();
                    black_box(&text);
                }
            },
        );
        assert_eq!(text, source);
        let expected = source[..at].replace("\r\n", "\n").encode_utf16().count();
        assert_eq!(
            openwebide_core::editor::byte_to_textarea(&source, at).unwrap(),
            expected
        );
        time(
            &mut records,
            &clock,
            "document_100_textarea_queries",
            source.len(),
            || {
                for _ in 0..100 {
                    black_box(
                        openwebide_core::editor::byte_to_textarea(&source, black_box(at)).unwrap(),
                    );
                }
            },
        );
        assert_eq!(doc.byte_to_textarea(at).unwrap(), expected);
        assert_eq!(doc.textarea_to_byte(expected), at);
        time(
            &mut records,
            &clock,
            "document_indexed_100_textarea_queries",
            source.len(),
            || {
                for _ in 0..100 {
                    black_box(doc.byte_to_textarea(black_box(at)).unwrap());
                    black_box(doc.textarea_to_byte(black_box(expected)));
                }
            },
        );
        time(
            &mut records,
            &clock,
            "document_cold_projection",
            source.len(),
            || {
                black_box(doc.projection());
            },
        );
        time(
            &mut records,
            &clock,
            "document_1000_warm_projections",
            source.len(),
            || {
                for _ in 0..1000 {
                    black_box(doc.projection());
                }
            },
        );
        time(
            &mut records,
            &clock,
            "string_100_raw_utf16_queries",
            source.len(),
            || {
                for _ in 0..100 {
                    black_box(source[..black_box(at)].encode_utf16().count());
                }
            },
        );
        #[cfg(feature = "candidates")]
        for materialize in [false, true] {
            let mut crop = crop::Rope::from(source.as_str());
            time(
                &mut records,
                &clock,
                if materialize {
                    "crop_100_edits_with_view"
                } else {
                    "crop_100_edits"
                },
                source.len(),
                || {
                    for _ in 0..50 {
                        crop.insert(at, "x");
                        if materialize {
                            black_box(crop.to_string());
                        }
                        crop.delete(at..at + 1);
                        if materialize {
                            black_box(crop.to_string());
                        }
                    }
                },
            );
            assert_eq!(crop.to_string(), source);
            let mut rope = ropey::Rope::from_str(&source);
            time(
                &mut records,
                &clock,
                if materialize {
                    "ropey_100_edits_with_view"
                } else {
                    "ropey_100_edits"
                },
                source.len(),
                || {
                    for _ in 0..50 {
                        let char_at = rope.byte_to_char(at);
                        rope.insert(char_at, "x");
                        if materialize {
                            black_box(rope.to_string());
                        }
                        rope.remove(char_at..char_at + 1);
                        if materialize {
                            black_box(rope.to_string());
                        }
                    }
                },
            );
            assert_eq!(rope.to_string(), source);
        }
        #[cfg(feature = "candidates")]
        {
            let crop = crop::Rope::from(source.as_str());
            let rope = ropey::Rope::from_str(&source);
            let expected = source[..at].encode_utf16().count();
            assert_eq!(crop.utf16_code_unit_of_byte(at), expected);
            assert_eq!(rope.char_to_utf16_cu(rope.byte_to_char(at)), expected);
            time(
                &mut records,
                &clock,
                "crop_100_raw_utf16_queries",
                source.len(),
                || {
                    for _ in 0..100 {
                        black_box(crop.utf16_code_unit_of_byte(black_box(at)));
                    }
                },
            );
            time(
                &mut records,
                &clock,
                "ropey_100_raw_utf16_queries",
                source.len(),
                || {
                    for _ in 0..100 {
                        black_box(rope.char_to_utf16_cu(rope.byte_to_char(black_box(at))));
                    }
                },
            );
        }
    }
    for size in [64 * 1024, 1024 * 1024] {
        let pattern = "文😀e\u{301}\t\rwords ";
        let source = pattern.repeat(size / pattern.len()) + "\r\n";
        let at = source.floor_char_boundary(source.len() * 3 / 4);
        let expected = openwebide_core::editor::byte_to_textarea(&source, at).unwrap();
        let expected_column = openwebide_core::editor::line_column(&source, at);
        let mut document = None;
        time(
            &mut records,
            &clock,
            "long_line_document_construction",
            source.len(),
            || {
                document = Some(Document::new(source.clone()));
            },
        );
        let document = document.unwrap();
        time(
            &mut records,
            &clock,
            "long_line_reference_100_textarea_queries",
            source.len(),
            || {
                for _ in 0..100 {
                    black_box(
                        openwebide_core::editor::byte_to_textarea(&source, black_box(at)).unwrap(),
                    );
                }
            },
        );
        assert_eq!(document.byte_to_textarea(at).unwrap(), expected);
        assert_eq!(document.textarea_to_byte(expected), at);
        time(
            &mut records,
            &clock,
            "long_line_indexed_100_textarea_queries",
            source.len(),
            || {
                for _ in 0..100 {
                    black_box(document.byte_to_textarea(black_box(at)).unwrap());
                    black_box(document.textarea_to_byte(black_box(expected)));
                }
            },
        );
        let mut projection = None;
        time(
            &mut records,
            &clock,
            "long_line_cold_projection",
            source.len(),
            || {
                projection = Some(document.projection());
            },
        );
        let projection = projection.unwrap();
        assert_eq!(projection.byte_to_textarea(at).unwrap(), expected);
        assert_eq!(projection.textarea_to_byte(expected), at);
        time(
            &mut records,
            &clock,
            "long_line_projection_100_textarea_queries",
            source.len(),
            || {
                for _ in 0..100 {
                    black_box(projection.byte_to_textarea(black_box(at)).unwrap());
                    black_box(projection.textarea_to_byte(black_box(expected)));
                }
            },
        );
        assert_eq!(document.line_column(at), expected_column);
        time(
            &mut records,
            &clock,
            "long_line_indexed_100_line_columns",
            source.len(),
            || {
                for _ in 0..100 {
                    black_box(document.line_column(black_box(at)));
                }
            },
        );
    }
    measure_visual_lines(&mut records, &clock);
    records
}

fn measure_visual_lines(records: &mut Vec<Measurement>, clock: &impl Fn() -> f64) {
    use openwebide_core::editor::{VisualLineIndex, horizontal_paint_bounds, visual_line_offsets};
    for size in [64 * 1024 + 512, 1024 * 1024] {
        let pattern = "文😀e\u{301}\t words ";
        let body = pattern.repeat(size / pattern.len());
        let mut dense = None;
        time(
            records,
            clock,
            "glyph_dense_construction",
            body.len(),
            || {
                dense = visual_line_offsets(&body).ok();
            },
        );
        let dense = dense.unwrap();
        let mut sparse = None;
        time(
            records,
            clock,
            "glyph_sparse_construction",
            body.len(),
            || {
                sparse = VisualLineIndex::new(&body);
            },
        );
        let sparse = sparse.unwrap();
        assert_eq!(sparse.len(), dense.len());
        assert!(
            sparse.retained_bytes() < dense.capacity() * std::mem::size_of::<(usize, usize)>() / 8
        );
        let queries: Vec<_> = (0..1000).map(|i| i * (dense.len() - 1) / 999).collect();
        for &glyph in &queries {
            let (byte, _) = dense[glyph];
            assert_eq!(sparse.at(&body, glyph), Some(dense[glyph]));
            assert_eq!(sparse.index_at_byte(&body, byte), Some(glyph));
        }
        time(
            records,
            clock,
            "glyph_dense_1000_queries",
            body.len(),
            || {
                for &glyph in &queries {
                    black_box(dense[black_box(glyph)]);
                }
            },
        );
        time(
            records,
            clock,
            "glyph_sparse_1000_queries",
            body.len(),
            || {
                for &glyph in &queries {
                    black_box(sparse.at(&body, black_box(glyph)).unwrap());
                }
            },
        );
        time(
            records,
            clock,
            "horizontal_source_100_eligibility",
            body.len(),
            || {
                for i in 0..100 {
                    black_box(horizontal_paint_bounds(&body, f64::from(i * 100), 400.0));
                }
            },
        );
        time(
            records,
            clock,
            "horizontal_cached_100_eligibility",
            body.len(),
            || {
                for i in 0..100 {
                    black_box(sparse.horizontal_paint_bounds(f64::from(i * 100), 400.0));
                }
            },
        );
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod browser {
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn shared_lexical_workloads() {
        let clock = js_sys::Function::new_no_args("return performance.now()");
        for record in
            super::measure_lexical(|| clock.call0(&js_sys::global()).unwrap().as_f64().unwrap())
        {
            wasm_bindgen_test::console_log!(
                "{},{},{:.3}",
                record.operation,
                record.bytes,
                record.milliseconds
            );
            assert!(record.milliseconds.is_finite() && record.milliseconds >= 0.0);
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn shared_storage_workloads() {
        let clock = js_sys::Function::new_no_args("return performance.now()");
        let records = super::measure(|| clock.call0(&js_sys::global()).unwrap().as_f64().unwrap());
        for record in records {
            wasm_bindgen_test::console_log!(
                "{},{},{:.3}",
                record.operation,
                record.bytes,
                record.milliseconds
            );
            assert!(record.milliseconds.is_finite() && record.milliseconds >= 0.0);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod native {
    #[test]
    fn shared_lexical_workloads() {
        let start = std::time::Instant::now();
        let records = super::measure_lexical(|| start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(records.len(), 12);
        assert!(
            records
                .iter()
                .all(|record| record.milliseconds.is_finite() && record.milliseconds >= 0.0)
        );
    }
    #[test]
    fn shared_storage_workloads() {
        let start = std::time::Instant::now();
        let records = super::measure(|| start.elapsed().as_secs_f64() * 1000.0);
        let expected = if cfg!(feature = "candidates") { 75 } else { 57 };
        assert_eq!(records.len(), expected);
        assert!(
            records
                .iter()
                .all(|record| record.milliseconds.is_finite() && record.milliseconds >= 0.0)
        );
    }
}
