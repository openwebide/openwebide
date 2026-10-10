//! Shared primitive measurements, excluding frontend publication and DOM paint.
use openwebide_core::{
    editor::{Document, Selection, SyntaxPreparations, SyntaxStatus},
    highlight::Language,
};
use std::sync::Arc;

#[derive(Debug)]
pub struct InputMeasurement {
    pub workload: &'static str,
    pub decorations: bool,
    pub bytes: usize,
    pub status: SyntaxStatus,
    pub context: bool,
    pub paired: bool,
    pub document_ms: f64,
    pub preparation_ms: f64,
    pub edit_ms: f64,
}

/// Each workload starts with a fresh parser cache. Fixture generation and
/// selection setup are untimed; document construction includes admission,
/// allocation and indexes. Preparation uses the production 12 ms policy.
/// Outcomes are recorded, including cancellation, rather than assumed ready.
pub fn measure(clock: impl Fn() -> f64, decorations: bool) -> Vec<InputMeasurement> {
    let mut measurements = Vec::new();
    for bytes in [64usize * 1024, 512 * 1024, 2 * 1024 * 1024, 2_600_000] {
        for (workload, language, prefix, pattern, suffix) in [
            (
                "html-code",
                Language::Html,
                "<script>\n",
                "function run() {} // 文😀 padding ................................\n",
                "</script>",
            ),
            (
                "html-bodies",
                Language::Html,
                "",
                "<script>function run() {} // 文😀 padding ................\n</script>\n",
                "",
            ),
            (
                "markdown-code",
                Language::Markdown,
                "```javascript\n",
                "function run() {} // 文😀 padding ................................\n",
                "```",
            ),
            (
                "rust-code",
                Language::Rust,
                "",
                "fn run() {} // 文😀 padding ........................................\n",
                "",
            ),
        ] {
            let count = bytes.saturating_sub(prefix.len() + suffix.len()) / pattern.len();
            let source = Arc::new(format!("{prefix}{}{suffix}", pattern.repeat(count)));
            let at = source.rfind("{}").unwrap() + 1;
            let started = clock();
            let mut document = Document::for_editor(source.as_str()).unwrap();
            let document_ms = clock() - started;
            document.set_selections(vec![Selection::caret(at)]).unwrap();
            let saved = document.shared_text();
            let mut cache = SyntaxPreparations::<u8>::default();
            let started = clock();
            let deadline = started + 12.0;
            let (status, analysis) = if decorations {
                cache.prepare_shared(1, language, source.clone(), 4, || clock() <= deadline)
            } else {
                cache.prepare_input_shared(1, language, source.clone(), || clock() <= deadline)
            };
            let preparation_ms = clock() - started;
            let structure = analysis.as_ref().and_then(|analysis| analysis.structure());
            let started = clock();
            if let Some(structure) = structure {
                document
                    .type_character_with_context('(', structure)
                    .unwrap();
            } else {
                document.type_character('(', language).unwrap();
            }
            let edit_ms = clock() - started;
            let inserted = document.text().len() - source.len();
            assert_eq!(&document.text()[..at], &source[..at]);
            assert_eq!(&document.text()[at + inserted..], &source[at..]);
            assert!(matches!(&document.text()[at..at + inserted], "(" | "()"));
            assert_eq!(saved.as_str(), source.as_str());
            assert!(document.undo());
            assert_eq!(document.text(), source.as_str());
            assert_eq!(document.selections(), &[Selection::caret(at)]);
            measurements.push(InputMeasurement {
                workload,
                decorations,
                bytes: source.len(),
                status,
                context: structure.is_some(),
                paired: inserted == 2,
                document_ms,
                preparation_ms,
                edit_ms,
            });
        }
    }
    measurements
}

pub fn print(record: &InputMeasurement) {
    println!(
        "{},{},{},{:?},{},{},{:.3},{:.3},{:.3}",
        record.workload,
        record.decorations,
        record.bytes,
        record.status,
        record.context,
        record.paired,
        record.document_ms,
        record.preparation_ms,
        record.edit_ms,
    );
}

#[cfg(all(test, target_arch = "wasm32"))]
mod browser {
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn shared_input_preparation_workloads() {
        run(false);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn shared_full_preparation_workloads() {
        run(true);
    }

    fn run(decorations: bool) {
        let clock = js_sys::Function::new_no_args("return performance.now()");
        for record in super::measure(
            || clock.call0(&js_sys::global()).unwrap().as_f64().unwrap(),
            decorations,
        ) {
            wasm_bindgen_test::console_log!("{record:?}");
        }
    }
}
