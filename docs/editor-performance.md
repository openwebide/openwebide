# Editor performance measurements

Run the same Rust workloads on the native host and in Chrome/WASM:

```sh
cargo run -p openwebide-editor-bench --features candidates --release
CHROMEDRIVER=<matching-driver> cargo test -p openwebide-editor-bench --features candidates --target wasm32-unknown-unknown --release -- --nocapture
```

The browser command needs the matching `wasm-bindgen-test-runner` on PATH and Chrome configuration described in [editor controls](editor.md). Copy the runner configuration to `tools/editor-bench/webdriver.json`; each package’s test runner reads its own working directory. CI runs both workloads. Timings are observations, not pass/fail thresholds; correctness checks and native/WASM builds are required. Setup and final text verification are outside timed edits.

Each edit workload performs 50 insert/delete or document edit/Undo pairs (100 edits) at the middle of a Unicode/CRLF buffer. Document history stores replacement deltas. Candidate rope runs compare isolated edits and edits that also materialize the complete display string after every operation. Queries repeat 100 times. `document_100_textarea_queries` exercises actual CRLF-aware browser offsets; raw UTF-16 candidate queries measure their indexing primitives and are not interchangeable with textarea offsets.

Rope candidates are optional dependencies of this measurement tool and are absent from application runtime dependencies. [Crop](https://docs.rs/crop/0.4.3/crop/struct.Rope.html) exposes byte edits and an optional UTF-16 metric. [Ropey](https://docs.rs/ropey/1.6.1/ropey/struct.Rope.html) uses character edits with byte/character conversion. Ropey enables SIMD while disabling extra Unicode/bare-CR line separators to match the editor’s LF/CRLF line semantics.

## Recorded comparison

One local observation on 2026-10-06, Rust 1.98.1, macOS arm64, Chrome 154.0.8037.98. All numbers are total milliseconds for 100 operations. Repeat on deployment hardware before choosing thresholds.

| Workload | Bytes | Native ms | Browser WASM ms |
| --- | ---: | ---: | ---: |
| `document_100_edits` | 2,097,144 | 4.888 | 3.145 |
| `crop_100_edits` | 2,097,144 | 0.004 | 0.015 |
| `ropey_100_edits` | 2,097,144 | 0.009 | 0.060 |
| `crop_100_edits_with_view` | 2,097,144 | 6.833 | 5.565 |
| `ropey_100_edits_with_view` | 2,097,144 | 8.512 | 6.860 |
| `document_100_textarea_queries` | 2,097,144 | 77.741 | 120.825 |
| `document_100_edits` | 16,777,194 | 51.277 | 24.380 |
| `crop_100_edits` | 16,777,194 | 0.005 | 0.055 |
| `ropey_100_edits` | 16,777,194 | 0.013 | 0.060 |
| `crop_100_edits_with_view` | 16,777,194 | 110.173 | 45.565 |
| `ropey_100_edits_with_view` | 16,777,194 | 131.001 | 55.185 |
| `document_100_textarea_queries` | 16,777,194 | 579.388 | 964.545 |

Full records: [native CSV](editor-performance/native-storage.csv), [browser CSV](editor-performance/browser-storage.csv).

Both ropes make isolated edits and indexed queries much cheaper. In this workload, recreating the full display string makes candidate edits slower than the current document. Retain current production storage while implementing worker/viewport rendering; choose storage against the resulting access pattern rather than adding a mirrored rope alongside a full string. Whole-source reference conversions remain in the comparison; production document queries use the incremental index described below.

## Source-change comparison

The shared minimal-edit comparison now compares 64-byte chunks before adjusting
only differing edges to UTF-8 character boundaries. The same helper serves native
input, transactions, fold updates, parser edits, lexical reuse and worker deltas.
The benchmark repeats 100 middle/late insert comparisons outside allocation and
checks the exact resulting span. Measurements on this machine (2026-10-07):

| Source | Position | Native before / after (ms) | Chrome WASM before / after (ms) |
| --- | --- | --- | --- |
| 2,097,144 bytes | Middle | 163.221 / 7.613 | 189.630 / 57.620 |
| 2,097,144 bytes | End | 156.038 / 5.948 | 186.885 / 57.505 |
| 16,777,194 bytes | Middle | 1184.709 / 53.540 | 1545.795 / 447.260 |
| 16,777,194 bytes | End | 1198.077 / 59.841 | 1497.915 / 429.395 |

These are observations from consecutive runs, not latency thresholds or proof of
viewport memory bounds. Comparison remains linear in unchanged text. Full records:
[native before](editor-performance/native-source-change-before.csv),
[native after](editor-performance/native-source-change-after.csv),
[browser before](editor-performance/browser-source-change-before.csv),
[browser after](editor-performance/browser-source-change-after.csv).

## Sequential lexical row reuse

Unchanged source rows now try the exact next retained row position before an
indexed search. The validated UTF-8 source replacement still proves unchanged
prefix/suffix bytes; raw changed rows, incoming multiline state, terminal rows,
normalization settings and cooperative budgets retain their existing checks.
A changed row resumes the retained table through indexed lookup. Exhaustive
Unicode/CRLF/EOF edits compare with fresh tokens; work-count checks require one
indexed recovery across a 3,001-row update, including multiline state propagation.
Source comparison and complete table publication remain linear.

Run the same primitive workload in native and browser release builds:

```sh
cargo run -p openwebide-editor-bench --release -- --lexical
CHROMEDRIVER=<matching-driver> cargo test -p openwebide-editor-bench --target wasm32-unknown-unknown --release shared_lexical_workloads -- --nocapture
```

Each record times twenty complete preparations against a retained lexical base,
including source comparison, token-row/context-table assembly and publication.
Fixtures and fresh-token verification are outside timing. Source contains Unicode
Rust strings and LF or CRLF; begin/middle/end insertions retokenize exactly one row.
The 65,516/2,097,150-byte fixtures use LF; 65,527/2,097,140-byte fixtures use CRLF.
Consecutive observations on this machine (2026-10-08, Rust 1.98.1, macOS arm64,
Chrome 154):

| Ending / insertion | Source bytes | Native before / after ms | Chrome WASM before / after ms |
| --- | ---: | ---: | ---: |
| LF / begin | 2,097,150 | 92.875 / 28.443 | 66.805 / 30.035 |
| LF / middle | 2,097,150 | 90.224 / 32.722 | 60.900 / 29.590 |
| LF / end | 2,097,150 | 90.649 / 26.239 | 61.835 / 27.870 |
| CRLF / begin | 2,097,140 | 88.327 / 24.737 | 64.255 / 27.005 |
| CRLF / middle | 2,097,140 | 89.235 / 23.991 | 58.555 / 26.930 |
| CRLF / end | 2,097,140 | 100.010 / 23.413 | 60.765 / 30.490 |

These are single paired primitive runs, not application input-to-paint latency,
percentiles or proof that remaining long-row stalls are resolved. Full records:
[native before](editor-performance/lexical-row-before-native.csv),
[native after](editor-performance/lexical-row-after-native.csv),
[browser before](editor-performance/lexical-row-before-browser.csv),
[browser after](editor-performance/lexical-row-after-browser.csv).

## Complete native-input comparison

Complete textarea replacements and duplicate composition commits compare borrowed
byte chunks between CR/LF normalization boundaries. Forward and reverse chunks
retain complete UTF-8 characters and keep CRLF pairs together. The shared input
facade returns original source byte offsets without constructing normalized copies.
The benchmark checks an exact middle insertion and repeats comparison 100 times;
document construction and candidate allocation stay outside the timed operation.

Consecutive runs on this machine (2026-10-07):

| Source bytes | Ending | Native before / after (ms) | Chrome WASM before / after (ms) |
| --- | --- | --- | --- |
| 2,047,212 | LF | 278.608 / 73.755 | 493.465 / 116.835 |
| 2,097,144 | CRLF | 276.221 / 106.637 | 496.785 / 143.945 |
| 16,377,737 | LF | 2235.934 / 592.272 | 3975.460 / 983.465 |
| 16,777,194 | CRLF | 2299.756 / 925.233 | 4016.480 / 1150.195 |

These observations do not establish event-to-paint latency or memory bounds.
Comparison remains linear; native text generation and source/selection publication
are separate costs. The workload now emits 57 records (75 with storage candidates).
Full records: [native before](editor-performance/native-input-before.csv),
[native after](editor-performance/native-input-after.csv),
[browser before](editor-performance/browser-input-before.csv),
[browser after](editor-performance/browser-input-after.csv).

## Incremental coordinates and shared projections

The document now maintains logical-line and raw/native UTF-16 prefixes across edit
transactions, grouped Undo/Redo and IME previews. Updates re-scan the changed rows
and shift suffix coordinates; queries binary-search a row and scan only within it.
Line/comment/reindent/selection commands reuse those rows. Folded projections share
immutable text, normalized textarea text and visible-row coordinates until source
or folds change. Read-only view preparation does not change document identity.

One observation on the same native host and Chrome version on 2026-10-06:

| Workload | Bytes | Native ms | Browser WASM ms |
| --- | ---: | ---: | ---: |
| `document_100_edits` | 2,097,144 | 5.850 | 5.270 |
| `document_100_textarea_queries` | 2,097,144 | 98.997 | 124.210 |
| `document_indexed_100_textarea_queries` | 2,097,144 | 0.004 | 0.010 |
| `document_cold_projection` | 2,097,144 | 3.209 | 3.035 |
| `document_1000_warm_projections` | 2,097,144 | 0.007 | 0.015 |
| `document_100_edits` | 16,777,194 | 69.710 | 42.660 |
| `document_100_textarea_queries` | 16,777,194 | 767.601 | 1025.380 |
| `document_indexed_100_textarea_queries` | 16,777,194 | 0.013 | 0.025 |
| `document_cold_projection` | 16,777,194 | 29.591 | 24.435 |
| `document_1000_warm_projections` | 16,777,194 | 0.008 | 0.015 |

Full records: [native CSV](editor-performance/native-index.csv),
[browser CSV](editor-performance/browser-index.csv). The indexed query workload
performs 100 paired byte-to-native/native-to-byte queries; the legacy reference
performs 100 byte-to-native queries. Cold projection includes its first allocation;
warm access clones shared handles 1,000 times. Setup is outside the timed section,
and browser clock resolution limits precision for the shortest workloads.

This removes whole-prefix coordinate scans on ordinary short-line files and repeated
projection allocations. It does not bound long-line scans, avoid full String copies
on edits, or eliminate suffix-coordinate shifts. Native textarea input still owns
full projected text. These microbenchmarks do not establish input/scroll latency,
total memory use or full-editor large-file limits.

## Shared document coordinate tables

An unfolded projection now shares the document index's coordinate table rather
than assembling another table of row handles. Edits update the unique table in
place; retained views and composition/document snapshots detach on mutation.
Folded and bounded projections keep their own visible coordinates without spare
growth capacity. Core contracts verify allocation sharing, immutable retained
views, native offsets, undo/redo and folded reconstruction for Unicode, LF, CRLF
and lone CR. Browser contracts exercise the same editor facade in both modes.

One observation on 2026-10-07 on the same macOS arm64 host and Chrome version;
other verification processes were running, so these are workload observations,
not an isolated before/after performance comparison:

| Workload | Bytes | Native ms | Browser WASM ms |
| --- | ---: | ---: | ---: |
| `document_100_edits` | 2,097,144 | 4.091 | 4.220 |
| `document_cold_projection` | 2,097,144 | 2.942 | 3.080 |
| `document_1000_warm_projections` | 2,097,144 | 0.009 | 0.015 |
| `document_100_edits` | 16,777,194 | 31.322 | 33.890 |
| `document_cold_projection` | 16,777,194 | 24.568 | 25.865 |
| `document_1000_warm_projections` | 16,777,194 | 0.009 | 0.015 |

Full records: [native CSV](editor-performance/native-shared-coordinates.csv),
[browser CSV](editor-performance/browser-shared-coordinates.csv). These records
precede the subsequent visible-row cache: unfolded row tables now prepare lazily,
share the document index and update affected rows plus shifted suffixes after
edits. Retained tables detach, growth reserves 256 rows of headroom, and major
deletions release excess capacity. Cold/folded row assembly, native CRLF
normalization and retained-table detachment still visit large containers; these
measurements do not establish memory bounds, input latency or completion of the
editor performance roadmap.

## Sparse glyph coordinates and cached eligibility

Long logical rows share immutable grapheme/UTF-16 checkpoints across the document
and folded views, approximately every 512 source bytes. Paint and cursor probes
scan from a known cluster boundary instead of retaining a coordinate per glyph.
Unchanged rows retain the same allocation; edited rows rebuild it. The initial
scan also caches whether horizontal fragments are safe for the source. DOM visual
boundaries and styled row shaping are still measured again; this does not finish
long-line rendering or total-memory work.

The shared native/browser workload compares complete glyph arrays with sparse
coordinates for Unicode, combining marks and tabs. It checks 1,000 distributed
lookups and byte-to-glyph inverses, including EOF, and requires estimated sparse
metadata retention below one eighth of the complete array allocation. One macOS
arm64 / Chrome 148 observation on 2026-10-06:

| Workload | Source bytes | Native ms | Browser WASM ms |
| --- | ---: | ---: | ---: |
| Complete glyph construction | 1,048,572 | 9.701 | 14.315 |
| Sparse glyph construction and eligibility | 1,048,572 | 17.889 | 20.770 |
| Sparse glyph queries ×1,000 | 1,048,572 | 2.277 | 2.380 |
| Source eligibility scans ×100 | 1,048,572 | 981.864 | 1047.545 |
| Cached eligibility queries ×100 | 1,048,572 | <0.001 | 0.005 |

Construction is slower than the complete coordinate array because it also checks
horizontal eligibility. Sparse queries trade direct array access for bounded
Unicode scanning; repeated eligibility queries avoid reclassifying the source.
An indivisible grapheme can exceed checkpoint spacing, so the admitted source-line
limit still bounds that exceptional scan. Timings are observations, not latency
gates. Raw records: [native CSV](editor-performance/native-glyph.csv) and
[browser CSV](editor-performance/browser-glyph.csv).

## Long wrapped lines

The existing both-mode Unicode/CRLF regression keeps two cursors deep inside the same large wrapped line. It checks exact Down/Up restoration, unchanged source/history and fewer than 2,000 DOM range measurements. Paint now splits oversized tokens into Unicode-safe text runs, retaining complete grapheme clusters and escaped source. In the local debug browser fixture, splitting runs reduced elapsed fixture time from 9.05 seconds to 1.27 seconds; a Down press used 225 ranges in approximately 130 ms. This is a regression observation, not a claim of final large-file responsiveness.

Syntax preparation now runs in a Rust/WASM worker, with bounded shared cache retention, validated coordinates and coalesced requests. The built-worker Chrome check exercises all providers, incremental Unicode/CRLF, a UI event during preparation and oversized-source fallback. These correctness checks do not measure total editor memory or latency.

Remaining work: bounded cold and fine wrapped viewport paint, incremental input/source access, responsiveness and memory validation at the admission boundaries, and real-device verification. The production baselines below establish observations rather than completing that validation. Storage microbenchmarks do not prove those items complete.

## Production view and process memory

`python3 tools/measure-editor-view.py` runs the built app against disposable
Spin/SQLite accounts and fresh Chrome process trees. It hydrates the same saved
editor source in local and remote modes, inserts text through Chrome's native input
path, scrolls to 70% of the document, and waits for current source/layout paint.
Wrapped readiness additionally requires an exact height table. Local cases do not
grant an OS directory handle; these measure editor behavior rather than filesystem
permissions. Each source contains Unicode, and multi-line sources retain CRLF.

The harness records load-to-paint, input-to-paint, scroll-to-paint, frame intervals,
main-thread long tasks, main WASM committed memory before/after input, DOM size, and
Chrome process-tree RSS at 200 ms intervals. Linux additionally reports apportioned
PSS when every owned process's `smaps_rollup` is readable. Summed RSS counts shared
pages repeatedly; it is not unique resident memory. Main WASM allocation excludes
worker instances; process-tree measurements include their hosting renderer.

Samples
can miss brief peaks. These are complete application workloads, including recovery,
background preparation and deferred saves, rather than isolated core operations.
Input timing includes the native event through paint observation; cold timing also
includes navigation and recovery transport. Results are single observations, not
percentiles or pass/fail latency budgets.

On 2026-10-06, macOS arm64 and Chrome 148, the built geometry checkpoint `645c91f`
(with concurrent branding/welcome working changes) produced these observations.
The raw records identify the compiled JS module hash, browser version and checkout.

| Workload | Mode | Wrap | Cold ms | Input ms | Scroll ms | Main WASM after input MiB | Sampled peak summed RSS GiB |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| 58 KB / 1,001 rows | Local | Off | 550 | 37 | 31 | 22.8 | 1.02 |
| 58 KB / 1,001 rows | Remote | Off | 525 | 36 | 6 | 22.9 | 1.02 |
| 2 MiB / 36,158 rows | Local | Off | 1,035 | 254 | 124 | 89.0 | 1.34 |
| 2 MiB / 36,158 rows | Remote | Off | 1,027 | 265 | 124 | 91.0 | 1.38 |
| 100,000 rows | Local | Off | 896 | 202 | 134 | 46.1 | 1.38 |
| 100,000 rows | Remote | Off | 1,177 | 200 | 131 | 46.1 | 1.40 |
| Near 8 MiB byte limit | Local | Off | 1,541 | 358 | 148 | 239.9 | 1.66 |
| Near 8 MiB byte limit | Remote | Off | 1,631 | 331 | 145 | 234.2 | 1.68 |
| 58 KB / 1,001 rows | Local | On | 694 | 167 | 26 | 25.9 | 1.10 |
| 58 KB / 1,001 rows | Remote | On | 721 | 174 | 24 | 26.1 | 1.10 |
| Near 1 MiB single-line limit | Local | On | 2,122 | 447 | — | 48.4 | 3.76 |
| Near 1 MiB single-line limit | Remote | On | 2,159 | 402 | — | 49.1 | 4.39 |

Full records: [unwrapped JSONL](editor-performance/production-view-unwrapped.jsonl),
[wrapped JSONL](editor-performance/production-view-wrapped.jsonl). To repeat:

```bash
NO_COLOR=true spin build
CHROMEDRIVER=<driver> CHROME=<chrome> python3 tools/measure-editor-view.py \
  --cases small medium line-limit byte-limit
CHROMEDRIVER=<driver> CHROME=<chrome> python3 tools/measure-editor-view.py \
  --cases small long-line --wrap
```

An earlier separate 2 MiB wrapped local attempt timed out waiting for WebDriver
to observe cold paint. It did not establish a completed latency or memory result,
and remote completion at that size was not checked. The near-1 MiB wrapped line
also produced frame stalls above 1.5 seconds and several GiB of sampled summed
RSS despite a bounded final DOM. A bounded warm row count alone therefore does
not prove bounded shaping, cold layout, native input or overall memory.

These results contradict treating the current admission limits as validated
responsiveness limits. Finish bounded cold measurement/paint, finer rendering
within very long wrapped rows, and incremental input/source access; then repeat
these cases with memory sampling, including Linux PSS, and verify the previously
unresponsive wrapped workloads before closing the roadmap item.


## Cold measurement batches

The batched-layout working tree on 2026-10-06 (checkout `f628df9`, concurrent
branding changes, built JS `openwebide-frontend-1866e76a7cfeee5f.js`) completed
both previously unverified 2 MiB wrapped cases. The same Chrome 148/macOS harness
used bounded temporary row DOM, task yields and a frame turn every eight batches.
These are single observations, including concurrent host workloads; summed RSS
retains the shared-page caveat above.

| Workload | Mode | Cold ms | Input ms | Scroll ms | Main WASM after input MiB | Sampled peak summed RSS GiB |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 58 KB / 1,001 rows | Local | 711 | 91 | 17 | 24.4 | 1.08 |
| 58 KB / 1,001 rows | Remote | 762 | 107 | 32 | 24.6 | 1.09 |
| 2 MiB / 36,158 rows | Local | 2,366 | 2,579 | 89 | 105.4 | 2.02 |
| 2 MiB / 36,158 rows | Remote | 2,616 | 2,628 | 89 | 104.6 | 2.03 |

Full records: [batched wrapped JSONL](editor-performance/production-view-cold-batches.jsonl).
The larger cases still produce frame stalls around 533 ms and rebuild the complete
height table after input. Progressing preparation accepts ordinary native typing
and retains queued arrows, but ordered edits/IME/clipboard after a queued arrow
still need work. Incremental height reuse, finer shaping within long logical rows,
native input/source access and Linux PSS/device validation remain open. Completing
these cases does not validate the admission boundaries as responsiveness limits.


## Incremental styled-row reuse

The follow-on working tree at checkout `166ba6b`, built JS
`openwebide-frontend-9099131cd6992d5e.js`, reuses exact unchanged styled rows after
edits. The same Chrome 148/macOS workload on 2026-10-06 produced:

| Workload | Mode | Cold ms | Input ms | Scroll ms | Main WASM after input MiB | Sampled peak summed RSS GiB |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 2 MiB / 36,158 rows | Local | 2,375 | 315 | 89 | 102.6 | 1.81 |
| 2 MiB / 36,158 rows | Remote | 2,673 | 303 | 87 | 103.4 | 1.82 |

Full records: [incremental wrapped JSONL](editor-performance/production-view-row-reuse.jsonl).
Localized and disjoint edits reuse unchanged paint only when text, token styles,
line endings, font/width and ownership match. Identical rows with conflicting
previous heights are remeasured. Browser contracts count actual measurement DOM
for single-row changes, insertion/deletion, undo and two distant edits, and verify
full remeasurement after font invalidation in both modes.

Input-to-paint improves from roughly 2.6 seconds to 303–315 ms. Cold preparation
still takes 2.4–2.7 seconds, with frame stalls around 550 ms; native input retains
full projected text. The long-line/byte/row admission boundaries, Linux PSS and
real-device workflows still need the remaining validation described above. These
single observations establish improvement, not final responsiveness limits.

## Sparse long-line coordinates

The follow-on working tree at checkout `9392227` on 2026-10-06 adds immutable
checkpoints approximately every 512 bytes inside long logical rows. Document
native-UTF-16/byte and character-column queries binary-search a checkpoint, then
scan only its tail. Fold projections share the same row indexes, and transactions
rebuild the edit region while retaining indexes outside it. Short rows allocate no
checkpoint array. The same macOS arm64 host and Chrome 148 ran these release
workloads; setup and correctness assertions are outside timed queries.

| Workload | Bytes | Native ms | Browser WASM ms |
| --- | ---: | ---: | ---: |
| Long-line document construction | 65,522 | 0.124 | 0.120 |
| Full-prefix reference, 100 native queries | 65,522 | 3.699 | 4.910 |
| Indexed document, 100 paired byte/native queries | 65,522 | 0.071 | 0.170 |
| Cold folded projection | 65,522 | 0.054 | 0.055 |
| Indexed projection, 100 paired byte/native queries | 65,522 | 0.072 | 0.160 |
| Indexed character columns, 100 queries | 65,522 | 0.003 | 0.010 |
| Long-line document construction | 1,048,574 | 1.895 | 1.835 |
| Full-prefix reference, 100 native queries | 1,048,574 | 56.506 | 76.030 |
| Indexed document, 100 paired byte/native queries | 1,048,574 | 0.005 | 0.015 |
| Cold folded projection | 1,048,574 | 0.739 | 0.840 |
| Indexed projection, 100 paired byte/native queries | 1,048,574 | 0.004 | 0.010 |
| Indexed character columns, 100 queries | 1,048,574 | 0.001 | 0.010 |

Full records: [native CSV](editor-performance/native-long-coordinates.csv),
[browser CSV](editor-performance/browser-long-coordinates.csv). Queries repeat
at one fixed position three quarters into Unicode/tab/standalone-CR text ending
in CRLF; checkpoint alignment differs between sizes. Tiny timings approach browser
clock resolution and are observations rather than thresholds. Reference queries
are one direction while indexed queries include both directions. These measurements
do not establish native input/shaping, fine wrapped paint, memory or admission-boundary
responsiveness. Grapheme indexing for browser geometry still scans a complete line.


## Destination-verified long-line scrolling

The measurement harness now scrolls horizontally for an unwrapped single logical
row, and vertically for wrapped or multiline source. Single-row readiness requires
fragment offsets covering the destination and current document paint ownership.
Earlier single-row scroll readings lacked that destination check; their wrapped
scroll values above are unverified, and an unwrapped single-row vertical scroll
would be a no-op. Historical raw records are retained.

On 2026-10-06, macOS arm64 / Chrome 148, checkpoint `26bbd18` with the concurrent
branding/welcome edits compiled into module `f5175cf8ffc15d3` produced these single
observations for 1,048,572 source bytes:

| Mode | Wrap | Cold ms | Native input ms | Scroll ms | Main WASM after input MiB | Sampled peak summed RSS GiB |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Local | Off | 1,602 | 768 | 38 | 46.6 | 1.61 |
| Remote | Off | 1,609 | 434 | 43 | 50.8 | 1.58 |
| Local | On | 3,849 | 1,460 | 359 | 47.7 | 3.57 |
| Remote | On | 4,433 | 1,467 | 367 | 49.6 | 4.15 |

The horizontal runs moved 4,406,358 pixels; wrapped runs moved 340,803 pixels.
Raw records: [horizontal JSONL](editor-performance/production-horizontal-anchors.jsonl)
and [wrapped JSONL](editor-performance/production-wrapped-destination.jsonl).
Native input, cold shaping and wrapped scrolling still have substantial stalls;
these observations do not validate the admission limits or complete the editor
performance goal. Linux PSS and actual local folder permissions remain unmeasured
here. Repeat with `--cases long-line` and additionally `--wrap` for the wrapped case.

### Wrapped source anchors

A subsequent [wrapped-anchor observation](editor-performance/production-wrapped-anchors.jsonl)
uses the same destination-verified 1 MiB source and both modes, with the new
wrapped source-slicing implementation in the worktree. It ran before the explicit
bidi eligibility guard was added; this workload contains source-monotonic text.
The built app includes the concurrent branding/welcome changes described above.

| Mode | Cold ready (ms) | Input to paint (ms) | Destination scroll to paint (ms) | Peak summed Chrome RSS (GiB) |
| --- | ---: | ---: | ---: | ---: |
| Local | 4302.3 | 1651.9 | 51.5 | 4.03 |
| Remote | 4682.7 | 1711.8 | 60.8 | 4.51 |

Scroll readiness improved in these individual observations, but initial/input
work and process memory remain costly. These are single observations, not
percentiles or admission validation; Linux PSS and full boundary/device gates
remain outstanding. The final guard retains full-paragraph probes for bidi text.

### Cold height anchors and syntax reuse

The [cold-anchor observation](editor-performance/production-cold-anchors.jsonl)
uses the same destination-verified wrapped 1 MiB source in both modes. The built
worktree app includes cold height-anchor publication, provenance-checked transfer
across equivalent styled syntax results, stable file tabs and the concurrent
branding/welcome changes. Its module identifier is recorded in the raw results;
`checkoutHead` identifies the parent commit rather than the uncommitted build.
No other owned browser test ran concurrently.

| Mode | Cold ready (ms) | Input to paint (ms) | Destination scroll to paint (ms) | Peak summed Chrome RSS (GiB) |
| --- | ---: | ---: | ---: | ---: |
| Local | 2701.5 | 1075.5 | 54.7 | 4.31 |
| Remote | 2768.4 | 488.7 | 54.9 | 4.54 |

An [earlier observation before syntax transfer](editor-performance/production-cold-anchors-before-transfer.jsonl)
recorded 382 ms destination scroll in local mode and 58.7 ms remotely. Equivalent
syntax results could discard anchors while reusing heights, leaving no cold probe
that would repopulate them. The final policy preserves only identical styled rows
with valid previous height provenance; browser contracts verify transfer and stale
publication rejection, including font loading with unchanged computed metrics.

These individual observations support further investigation, not percentiles or
admission validation. Complete cold row shaping, long-line input and multi-GiB
summed Chrome RSS remain costly. Summed RSS can double-count shared pages; Linux
PSS and full boundary/device gates remain outstanding.

### Source slices before HTML generation

The shared facade now selects bounded source from retained styled anchors before
HTML generation and parsing. Browser contracts in both modes audit the largest
source row passed to HTML escaping on unseen horizontal/wrapped intervals and
verify it stays within 64 KiB, preserving Unicode/native hits and Find. An injected
partial measurement failure restores complete source, drops the rejected anchors
and obtains fresh bounded paint on the next probe. Equivalent styled syntax can
retain unwrapped geometry within the same font/layout epoch without a height
table; font loading still invalidates it.

The final [horizontal observation](editor-performance/production-source-slices-horizontal.jsonl)
and [wrapped observation](editor-performance/production-source-slices-wrapped.jsonl)
use the same destination-verified 1 MiB workload, module `d4e04563118f0285`.
The build contains the current worktree and concurrent branding/welcome changes;
`checkoutHead` records its parent commit. Browser tests were terminal before these
sequential runs started.

| Layout | Mode | Cold ready (ms) | Input to paint (ms) | Destination scroll to paint (ms) | Peak summed Chrome RSS (GiB) |
| --- | --- | ---: | ---: | ---: | ---: |
| Horizontal | Local | 1676.7 | 455.4 | 22.8 | 1.55 |
| Horizontal | Remote | 1282.1 | 429.3 | 367.7 | 1.57 |
| Wrapped | Local | 2631.2 | 850.0 | 455.8 | 4.59 |
| Wrapped | Remote | 3164.6 | 497.5 | 458.4 | 4.63 |

Earlier samples of the same source-slicing build before the unwrapped reuse fix
are retained for comparison: [horizontal](editor-performance/production-source-slices-horizontal-before-reuse.jsonl)
recorded 715.5/371.6 ms destination scroll; [wrapped](editor-performance/production-source-slices-before-unwrapped-reuse.jsonl)
recorded 35.7/39.1 ms. The initial height-provenance requirement excluded unwrapped
rows from equivalent syntax reuse; the shared contract now covers that correction.
These single observations remain inconsistent immediately after readiness and do
not prove stable startup scroll latency. Trace late syntax/font/layout changes and
uncached probes before claiming that gate complete. Full initial shaping, input
costs, process memory, Linux PSS, percentiles and admission/device gates remain
outstanding; summed RSS can double-count shared pages.

### Font provenance across syntax and layout reconciliation

The [before trace](editor-performance/production-layout-provenance-before.jsonl)
records full 748,980-native-unit paint probes after worker replies, alongside
bounded paint. Complete-row bounding-box reads took roughly 330–425 ms. No font
loading events occurred. These traces instrument browser primitives and perturb
timing; they establish probe type and ordering, not uninstrumented latency.

The shared facade regression reproduced a specific ordering bug: equivalent syntax
paint carried anchors into a new scope, then height reconciliation rejected them
against the older height-cache scope. Independent font provenance now retains
exact styled rows through ordinary layout reconciliation. Actual font changes,
source/read/account changes and stale callback publication remain protected.
The same contract failed before this fix and passes in both modes afterward.

The [after trace](editor-performance/production-layout-provenance-after.jsonl)
retains only bounded paint after the initial height probes, including after
worker replies. Wrapped cold shaping still runs twice in these observations;
new input still needs full height measurement. No font events or trace truncation
occurred. `--trace` caps each diagnostic array at 256 records and marks truncation;
normal benchmark runs do not replace browser measurement or worker primitives.
No source text or worker payload is stored in diagnostic records.

Uninstrumented [horizontal](editor-performance/production-font-provenance-horizontal.jsonl)
and [wrapped](editor-performance/production-font-provenance-wrapped.jsonl)
observations use the same destination-verified 1 MiB workload in both modes.
The built worktree includes concurrent branding/welcome changes; the raw module
identifier describes the build, while `checkoutHead` identifies its parent commit.
All owned browser tests were terminal before these sequential runs started.

| Layout | Mode | Cold ready (ms) | Input to paint (ms) | Destination scroll to paint (ms) | Peak summed Chrome RSS (GiB) |
| --- | --- | ---: | ---: | ---: | ---: |
| Horizontal | Local | 1655.1 | 466.8 | 23.5 | 1.55 |
| Horizontal | Remote | 1660.4 | 452.2 | 23.3 | 1.56 |
| Wrapped | Local | 2745.0 | 515.8 | 36.5 | 4.63 |
| Wrapped | Remote | 2714.0 | 442.4 | 36.3 | 4.66 |

This removes the reproduced cache-ordering failure and extra full paint probes
in these runs. It does not prove percentiles or complete admission gates. Duplicate
initial height shaping, cold native input layout, full String/input costs,
bidirectional windows, Linux PSS, boundary and device verification remain open.
Summed Chrome RSS can double-count shared pages.

### Settled font readiness and duplicate cold measurement

The [original-scope before trace](editor-performance/production-font-ready-before.jsonl)
records two startup height probes in each mode with identical styles and source,
read and account revisions. The second probe advances font provenance from zero
to one despite no font loading events. The viewport observer unconditionally
invalidated font geometry when the already-resolved `document.fonts.ready`
promise completed. The both-mode browser regression reproduces this only after
waiting for animation frames, and fails before the fix.

The observer now invalidates on loading completion or failure, including loads
in progress when it is installed, rather than settled readiness. The
[after trace](editor-performance/production-font-ready-after.jsonl) records one
startup height probe per mode, retains font generation zero and has no font
events or trace truncation. Measurement probes record their original immutable
scope so diagnostic timing cannot mislabel jobs with newer state.

`--trace` also records `preparationBatches`: Rust HTML rendering (source bytes),
DOM installation (HTML bytes), row layout/width reads, and source-geometry
sampling/publication (row counts). Each phase includes elapsed milliseconds;
the layout and geometry totals sum per-row work. The optional adapter callback
is installed only by the measurement harness; ordinary preparation does not
read a timing clock or invoke diagnostics. Records retain the probe's immutable
account/read/source/view/font/layout scope and stop at 256 batches, marking
truncation. Querying nodes, plan bookkeeping and yielding are outside these
phase totals, so they are not an end-to-end preparation duration.

Traces record
style and revision attributes, never source text. These instrumented observations
perturb timings and establish probe counts rather than latency thresholds.
Both builds include concurrent branding/welcome changes; module identifiers
identify the builds and `checkoutHead` their parent commit. Initial full-row
shaping, native textarea/input and full String costs, bidirectional windows,
Linux PSS, percentiles and boundary/device gates remain outstanding.

### Repeated Linux boundary baseline

`tools/measure-editor-view-linux.sh` runs the same built Rust/WASM editor harness
in a disposable Linux container, with matching distribution Chromium/ChromeDriver,
DejaVu, Noto CJK and color emoji fonts. It reuses the existing built WASM files;
there is no second Cargo target directory. Every repetition creates fresh browser,
account and runtime state. The launcher requires numeric peak and final Chrome
PSS, runs by immutable image ID, and records that ID, the browser version, CPU
count and cgroup memory/CPU limits alongside the parent checkout and app module.

```bash
NO_COLOR=true spin build
tools/measure-editor-view-linux.sh \
  --cases medium line-limit byte-limit long-line --wrap --repeat 3
tools/measure-editor-view-linux.sh \
  --cases medium line-limit byte-limit long-line --repeat 3
```

The container has a four-CPU quota, a 10 GiB memory limit and 1 GiB shared memory.
These are observations on a Linux arm64 Docker Desktop VM, not deployment-host
percentiles or phone/device checks. The same source/destination readiness checks
cover local and remote editor behavior; local folder permissions are not exercised.
PSS apportions shared Chrome pages; it excludes the Spin runtime and ChromeDriver.
The 200 ms sampler can miss brief peaks. Three repetitions establish observed
variation, not reliable tail percentiles or pass/fail responsiveness budgets.

On 2026-10-06, Chromium 154.0.8037.92 on Linux arm64 completed all 48
Unicode-capable boundary runs. Each table entry summarizes three fresh runs.
Cold and scroll values are medians; input shows median and observed range; PSS
is the largest sampled peak among those runs. Both image exports share the same
runtime configuration and package layers; build attestation IDs differ.
The built worktree includes concurrent branding/welcome changes; module
identifiers identify the actual build and the checkout its parent commit.

| Layout | Workload | Mode | Cold median ms | Input median (min–max) ms | Scroll median ms | Largest peak Chrome PSS GiB |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| Wrapped | 2 MiB | Local | 3943 | 500 (484–529) | 178 | 1.34 |
| Wrapped | 2 MiB | Remote | 4188 | 461 (457–498) | 177 | 1.34 |
| Wrapped | 100,000 rows | Local | 3702 | 397 (349–412) | 270 | 1.43 |
| Wrapped | 100,000 rows | Remote | 4045 | 335 (332–346) | 253 | 1.37 |
| Wrapped | Near 8 MiB | Local | 4217 | 644 (610–661) | 320 | 2.06 |
| Wrapped | Near 8 MiB | Remote | 5575 | 579 (577–622) | 322 | 2.25 |
| Wrapped | Near 1 MiB line | Local | 1763 | 556 (528–556) | 48 | 1.00 |
| Wrapped | Near 1 MiB line | Remote | 1766 | 496 (485–568) | 42 | 1.02 |
| Unwrapped | 2 MiB | Local | 1736 | 440 (346–451) | 193 | 0.95 |
| Unwrapped | 2 MiB | Remote | 1880 | 397 (381–456) | 354 | 0.97 |
| Unwrapped | 100,000 rows | Local | 1518 | 397 (388–399) | 275 | 1.07 |
| Unwrapped | 100,000 rows | Remote | 1539 | 282 (266–287) | 270 | 1.09 |
| Unwrapped | Near 8 MiB | Local | 2170 | 473 (468–664) | 413 | 1.28 |
| Unwrapped | Near 8 MiB | Remote | 2348 | 472 (454–474) | 439 | 1.29 |
| Unwrapped | Near 1 MiB line | Local | 1623 | 530 (518–545) | 26 | 1.03 |
| Unwrapped | Near 1 MiB line | Remote | 1654 | 538 (509–553) | 34 | 1.04 |

These baseline input timings start at `input`. The current harness starts at
`beforeinput` to include edit preparation; records label this as
`inputTimingStart`. Timings from the two clocks are not directly comparable.

Raw records: [wrapped](editor-performance/production-linux-wrapped-boundaries.jsonl),
[unwrapped](editor-performance/production-linux-unwrapped-boundaries.jsonl).
Earlier exploratory runs without a CJK font are excluded from this baseline;
font availability materially changed long-line shaping and memory. These font
fixture changes are not production editor performance improvements.

The completed runs provide Linux PSS evidence and repeated byte/row/long-line
boundary observations in both modes. They still show costly input and cold paint;
current admission limits remain unvalidated as responsiveness limits. Initial
shaping, bounded native input/source access, bidirectional visual-run windows,
more repetitions for tail latency and real-device/input-method/permission checks
remain required before closing the full editor roadmap item.

On 2026-10-06, the browser-owned insertion commit path passed twelve production
Linux runs (2 MiB, near 8 MiB and near 1 MiB line; both modes and layouts). These
are one-run correctness and PSS observations, not tail estimates. Input timing
starts at `beforeinput`, including preparation, so it cannot be compared directly
with the earlier `input` baseline. Large-file latency remains substantial.

| Layout | Case | Mode | Beforeinput to paint ms | Peak Chrome PSS GiB |
| --- | --- | --- | ---: | ---: |
| wrapped | medium | local | 522 | 1.32 |
| wrapped | medium | remote | 454 | 1.32 |
| wrapped | byte-limit | local | 644 | 2.06 |
| wrapped | byte-limit | remote | 620 | 2.17 |
| wrapped | long-line | local | 1024 | 1.00 |
| wrapped | long-line | remote | 973 | 1.03 |
| unwrapped | medium | local | 437 | 0.92 |
| unwrapped | medium | remote | 467 | 0.99 |
| unwrapped | byte-limit | local | 480 | 1.27 |
| unwrapped | byte-limit | remote | 496 | 1.29 |
| unwrapped | long-line | local | 823 | 0.98 |
| unwrapped | long-line | remote | 823 | 1.03 |

Raw records: [wrapped native commit](editor-performance/production-linux-native-commit-wrapped.jsonl),
[unwrapped native commit](editor-performance/production-linux-native-commit-unwrapped.jsonl).

The following trusted single-cursor value-retention checkpoint also passed all
twelve workloads in both modes/layouts on 2026-10-06. Browser contracts verify zero
full DOM-value reads in the successful trusted input handler, while synthetic
inputs and multiple cursors still reconcile. These one-run `beforeinput` timings
range from 388 to 1034 ms; they do not establish a latency improvement or
validate responsiveness limits. Preparation, source publication, initial shaping
and full native textarea layout remain expensive.

Raw records: [wrapped native retention](editor-performance/production-linux-native-retain-wrapped.jsonl),
[unwrapped native retention](editor-performance/production-linux-native-retain-unwrapped.jsonl).

## Existing-buffer transactions

On 2026-10-06, the shared engine validated borrowed proposed pieces before mutation
and applied edits/grouped history to the existing String. Allocation contracts
verify retained capacity for repeated typing/undo/redo and at most 64 KiB growth
headroom above a transaction's peak size. Merged row contexts preserve unchanged
interior indexes; randomized Unicode/CRLF transactions match complete replacement
and history, and rejected selections leave the document unchanged. Fold APIs share
precise boundary rebasing and retain unrelated collapsed ranges through refresh.

All 63 shared storage observations passed on native macOS arm64 (Rust 1.98.1) and
Chrome 148.0.7778.97/browser WASM. These are isolated one-run observations, not
percentiles or evidence of an overall speedup. The 16 MiB fixture uses the
unrestricted document API, outside the full editor's 8 MiB admission boundary.

| 100 middle edit/Undo operations | Native ms | Browser WASM ms |
| --- | ---: | ---: |
| 65,520 bytes | 0.885 | 0.740 |
| 2,097,144 bytes | 7.803 | 3.900 |
| 16,777,194 bytes | 26.279 | 31.715 |

Raw storage records: [native](editor-performance/native-buffer-transactions.csv),
[browser WASM](editor-performance/browser-buffer-transactions.csv).

The final capped-growth production build also passed twelve Linux Chromium 154
runs across 2 MiB, near 8 MiB and near 1 MiB line workloads, both modes and layouts.
Each has numeric apportioned Chrome PSS and the same compiled application module.
The build includes concurrent branding/welcome work; raw parent-checkout and module
identities describe the actual worktree build. Uncapped-reserve exploratory runs
are excluded.

One-run `beforeinput`-to-paint timings range from 412 to 1018 ms. Removing the
replacement candidate does not establish responsiveness: full proposed-text
admission scans, String suffix shifts, full buffer/projection/IME publication,
initial shaping and native textarea layout remain.

Raw production records: [wrapped](editor-performance/production-linux-buffer-transactions-wrapped.jsonl),
[unwrapped](editor-performance/production-linux-buffer-transactions-unwrapped.jsonl).

## Cold layout candidate check

The first cold logical-row probe still shapes a complete paragraph. Before
replacing it, `tools/measure-editor-layout.py` compares Parley 0.11.1 in native
Rust and browser WASM with DOM and canvas geometry. Parley provides shaping,
line breaking and bidirectional layout; see its
[layout description](https://github.com/linebender/parley/blob/main/doc/concept.md).

The isolated probe uses the same supplied Monaspace Neon v1.400 variable TTF in
each engine, 13 px text, 19.5 px line height, wrapping at 300 px, and texture
healing/ligature settings enabled and disabled. Parley has only that registered
font, with system-font discovery disabled to match the WASM environment. The
browser supplies its normal fallback fonts. Tab-size policy is supplied to the
DOM; Parley and canvas receive the original tab characters without an additional
tab-stop implementation. These are integration gaps to resolve, rather than
evidence that the libraries cannot support those features.

Recorded unwrapped widths with features enabled, in CSS pixels:

| Case | DOM | Rust/WASM Parley | Canvas |
| --- | ---: | ---: | ---: |
| ASCII source | 241.8125 | 241.8000 | 241.7981 |
| `a\tb\tc` | 72.5469 | 40.3000 | 40.2997 |
| Combining mark, CJK and emoji | 93.7500 | 80.6000 | 93.7395 |
| Mixed LTR/RTL | 175.2969 | 177.3200 | 175.2493 |
| 990,000-byte ASCII paragraph | 7,979,337 | 7,933,542 | 7,957,381.5 |

The large paragraph requires further precision investigation: its width differs
substantially even with the same font data. Wrapped heights match in this sample,
which does not establish matching glyph/caret positions. The Parley browser
measurement took 513 ms for the long unwrapped paragraph versus 45.65 ms for the
bare DOM row. These are single observations, not latency percentiles. The Rust
measurement includes constructing font/layout contexts and shaping; the DOM
measurement covers layout after text installation with a loaded font. Neither
measures the application renderer, styled-span parsing, input, or process memory.

Chrome 154 reports no extended canvas index, cluster or selection-rectangle
methods. Registering separate font faces with feature descriptors does change
canvas pixels, but canvas width still differs from DOM width. Future canvas
editing methods are described in the
[Chromium proposal](https://groups.google.com/a/chromium.org/g/blink-dev/c/Wf1iK1bc_00/m/JDtlgk-eAwAJ);
they cannot be assumed available in the browsers tested here.

**Decision:** keep the current DOM geometry adapter while developing bounded cold
preparation. A different shaper needs matching tab stops, explicit fallback-font
ownership, source/glyph precision, and a painter using the same geometry. Passing
this comparison probe only proves the candidate runs and produces measurements;
it does not complete cold rendering, bidi windows, native input, or either-mode
performance gates. Experimental dependencies stay outside the production
workspace, and the main Cargo lockfile is unchanged.

Raw records: [layout comparison](editor-performance/layout-candidate-parley.jsonl).
The tool freezes candidate dependencies, uses the repository's current WASM
bindings and lint policy, builds only in its shared `target/`, and removes its
temporary source directory afterward. With matching wasm-bindgen tools and Chrome
available, reproduce using the
[upstream font](https://github.com/githubnext/monaspace/blob/v1.400/fonts/Variable%20Fonts/Monaspace%20Neon/Monaspace%20Neon%20Var.ttf):

```bash
CHROMEDRIVER=/path/to/chromedriver \
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=/path/to/wasm-bindgen-test-runner \
python3 tools/measure-editor-layout.py \
  --font /path/to/MonaspaceNeon.ttf --output /tmp/editor-layout.jsonl --lint
```


### Production cold-preparation phases

[Phase records](editor-performance/production-cold-phases.jsonl) cover the small
and 1 MiB Unicode long-line fixtures, wrapped and unwrapped, in both modes on
macOS Chrome 154. The recorded module identifies the release bundle built from
the working changes on parent `a9c4397`; these are instrumented single runs,
not percentiles or Linux PSS evidence. The checked-in records omit the existing
per-row `layoutProbes` array; batch phases and all other result fields remain.

For the long line, cold row layout/width reads took 2,687–3,668 ms per batch.
HTML generation took roughly 12–13 ms and DOM installation roughly 2 ms in the
unwrapped runs. Source geometry sampling took roughly 27–45 ms. Font/layout
scope changes caused repeated cold full-row measurements; input subsequently
repeated full-row layout despite using a bounded native input window. Load to
paint ranged from 11.6–17.8 seconds, and input to paint from 2.9–3.1 seconds.
These phase timings include the opt-in primitive hooks and do not account for
all startup/native-input work. Small-fixture row diagnostics hit their existing
256-record cap; batch phase records did not hit their separate cap.

**Next action:** target full-paragraph browser shaping and repeated layout scopes,
then repeat the same workloads without tracing. Merely speeding HTML assembly
or making its allocation cooperative cannot address the measured multi-second
layout calls. Exact source/glyph/caret geometry and wrapped extents remain
required; this evidence does not complete the cold-shaping gate.

The measurement harness now recognizes source-scoped bounded native bindings
and scrolls the active source scroll surface, following the production adapter's
selection rule. It records native binding/length separately instead of requiring
the textarea to hold the full file. Full-source editing correctness remains
covered by the shared component contracts, rather than inferred from this
performance probe.


### Pending/plain row reuse

The shared row comparator now represents a single `Plain` token and a borrowed
pending row by their exact rendered body. Both HTML paths call `paint_text` once;
CRLF normalization follows the renderer. Multiple token runs stay distinct,
because changing text/span boundaries can change glyph shaping. Row guides,
endings, whitespace, font/layout metrics, file/read/source/account ownership
still participate in the existing cache and publication checks. No transport or
DOM-measurement implementation was added.

[After records](editor-performance/production-plain-row-reuse.jsonl) repeat the
1 MiB Unicode paragraph in both modes, wrapped and unwrapped, against the
previous [phase baseline](editor-performance/production-cold-phases.jsonl).
All four cases record three full-row batches instead of four across load,
scroll and input: identical plain syntax no longer adds a redundant measurement.
The recorded module identifies the bundle from working changes on parent
`2431484`; per-row `layoutProbes` are omitted as in the baseline. These are
instrumented single observations, not latency percentiles. Each remaining full
layout still takes seconds, and font transitions and actual edits still require
new geometry; this does not complete initial shaping or incremental long-row
measurement.

The both-mode browser contract checks retained glyph anchors and completed
measurement plans after pending/plain resolution, while rejecting different
plain token boundaries. Existing stale source/read/account/font checks remain.
All 366 browser component tests pass with the change. Next, address first cold
paragraph shaping, font-transition duplication and actual long-row edits.


### Font settling before cold row probes

Cold measurement reads the primary computed CSS family and waits for its
registered faces through the browser's
[FontFace loading primitive](https://www.w3.org/TR/css-font-loading-3/#font-face-load).
It checks registered face status directly: family availability checks can succeed
through fallback and do not establish that the primary face has loaded. The
configured Monaspace families each register one variable face. Glyph shaping
still uses the full computed style of the existing DOM probe; font loading
supplies no substitute metrics.
Already available fonts proceed immediately. Pending loads race a 250 ms timer,
with the budget defined in shared Rust core. Failed or stalled requests preserve
browser fallback and the existing native editing surface.

After an actual wait, a frame and task let font notifications invalidate old
measurement tickets. Source/account/font/layout ownership and computed metrics
are checked again before allocating the probe. Font changes after a timeout
still invalidate fallback geometry through the existing observer. No
measurements or glyph positions are guessed while a font is pending.

The both-mode browser regression controls font completion, failure and an
unresolved request, and replaces source while the load is pending. It checks
that no probe is allocated immediately for pending fonts, old font/source jobs
cannot run after completion, and a request that never resolves still permits
fallback measurement. Initial paragraph shaping and native cold/touch input
remain separate gates.


Measurement identity now includes the registered primary faces, their lifetime
IDs, statuses and style/weight/stretch descriptors, alongside the existing CSS
metrics. A load can therefore obsolete a queued probe even when computed CSS is
unchanged. Preference observation compares CSS separately, so availability
changes are not misclassified as preference changes.

Native loading notifications go through the shared editor facade. If current
source/account-owned measurements already use identical actual font and layout
metrics, their geometry is retained. Missing or different metrics invalidate it.
Explicit/synthetic invalidation and preference changes retain forced refresh.
This handles native notifications delivered after a loaded face was already
used by a completed measurement, while preserving fallback invalidation after
a delayed load. Opt-in traces now record up to 16 face names/statuses per batch
and loading event, within the existing 256-record caps; no source text is added.


[Final production records](editor-performance/production-font-notifications.jsonl)
repeat the 1 MiB Unicode paragraph in both modes, wrapped and unwrapped, using
the bundle identified by its module on parent `c549010`. All four cases now
record one cold full-row batch plus one batch after typing, compared with two
cold/scroll batches plus one input batch in the
[pending/plain baseline](editor-performance/production-plain-row-reuse.jsonl).
Registered-face states and loading events retain provenance; per-row diagnostics
are omitted as in prior records. Unwrapped load to paint was about 8.9 seconds;
wrapped was about 10.5–10.6 seconds. These instrumented single observations
establish removal of redundant work, not latency percentiles or Linux PSS gates.
Initial shaping and long-row input remain multi-second work and are unfinished.

The final implementation passes all 367 browser component tests, including
the eight controlled font-load outcomes across both modes, and all-target WASM
Clippy. Production probes use disposable state; native folder permission,
physical PWA input, Linux PSS and uninstrumented latency gates remain open.

## Fresh Linux boundary observations

On 2026-10-07, the release build at `abc6e4a` completed sixteen fresh-process
Linux Chromium 154 cases: 2 MiB, near 8 MiB, 100,000 rows and a near 1 MiB Unicode
line, wrapped/unwrapped in local/remote projects. Each case retained bounded native
input and completed cold paint, destination scrolling and a trusted insertion.
The container retained the 10 GiB memory/four-core configuration and CJK/emoji
fonts. These are single observations per mode, without probe instrumentation;
input timing starts at `beforeinput`. The following ranges cover the two modes,
not repeated-run percentiles. PSS is the largest sampled peak, including startup.

| Layout | Case | Cold range ms | Input range ms | Scroll range ms | Peak Chrome PSS GiB |
| --- | --- | ---: | ---: | ---: | ---: |
| wrapped | medium | 3793–3985 | 726–745 | 26–34 | 1.23 |
| wrapped | byte-limit | 7486–7915 | 434–473 | 34–36 | 2.15 |
| wrapped | line-limit | 4389–4598 | 1658–1658 | 24–34 | 1.27 |
| wrapped | long-line | 16808–16923 | 4356–4634 | 36–41 | 2.79 |
| unwrapped | medium | 1745–2383 | 72–735 | 12–25 | 0.87 |
| unwrapped | byte-limit | 2387–2434 | 430–457 | 32–35 | 1.07 |
| unwrapped | line-limit | 3451–3504 | 1649–1669 | 25–34 | 0.97 |
| unwrapped | long-line | 11672–11822 | 3749–4130 | 20–22 | 0.83 |

Raw records: [wrapped](editor-performance/production-linux-native-chunks-wrapped.jsonl),
[unwrapped](editor-performance/production-linux-native-chunks-unwrapped.jsonl).
Chrome subprocess exits made one initial unwrapped PSS sample unavailable; that
case retained later, peak and final PSS samples. The records preserve the missing
value instead of substituting summed RSS.

The runs establish functional boundary coverage and current memory observations,
but contradict completion of cold/long-line responsiveness. The long paragraph
still causes multi-second tasks, and row-count input needs profiling. Isolated
phase traces and repeated runs are needed to attribute those costs; these results
cannot be compared directly to older Chrome/build/font observations. Geometry
validation's subsequent removal of temporary normalized source copies is a
separate change, not a measured speedup in these records. Keep cold shaping,
incremental measurement, long-row input and responsiveness verification open.

The opt-in `--trace` harness now also records native textarea value-write lengths
and `scrollWidth`/`scrollHeight` read durations, with the installed binding state
and phase. This distinguishes native layout from styled-row probe timings without
reading/copying the textarea value for diagnostics. A disposable small Linux case
verified all three event kinds and the complete-to-window value transition. The
trace check is instrumentation validation, not an isolated performance sample;
use it to profile long-line and row-count cases next.

## Row-count input scheduling and rules detection

Isolated release traces at parent `94ef898` separated the two remaining costs:
[row input](editor-performance/production-linux-row-input-trace.jsonl) and
[layout/native input](editor-performance/production-linux-layout-input-trace.jsonl).
On 100,000-row input, the native `input` handler took about 16 ms and the changed
row probe about 0.3 ms, but lexical fallback waited one animation frame per eight
128-row batches (roughly 98 frames). A near-1-MiB wrapped paragraph instead spent
about 3.5 seconds measuring its changed full row; that separate cost remains.

Lexical fallback now uses a four-millisecond preparation budget per animation
frame with a 64-batch hard cap, retaining task yields and ownership validation
between batches. Missing/invalid clocks retain the conservative eight-batch
fallback. An intermediate build reduced remote row-count input to about 243 ms,
but local input remained about 1.1 seconds even in isolated repeated runs.
Profiling ownership checks found repeated full-source rules detection when a
loaded configuration entry was unavailable. The shared facade now memoizes rules
and invalidates on source, preferences, loaded configuration, override and file/
project changes. Neither adapter implements its own rules or scheduling policy.

Three fresh-process samples per mode/layout on Linux Chromium 154 used the
working release module `3b3fccc397b4a4fd` on parent `94ef898` with both changes.
Input timing starts at trusted `beforeinput`; the same 10-GiB/four-core container
and CJK/emoji fonts were retained. Medians and observed input ranges follow:

| Layout | Mode | Cold median ms | Input median ms | Input range ms | Peak Chrome PSS GiB |
| --- | --- | ---: | ---: | ---: | ---: |
| Wrapped | Local | 3208 | 244 | 241–249 | 1.24 |
| Wrapped | Remote | 3112 | 244 | 241–250 | 1.27 |
| Unwrapped | Local | 1983 | 238 | 230–238 | 0.97 |
| Unwrapped | Remote | 2005 | 237 | 236–238 | 0.97 |

Raw records: [wrapped](editor-performance/production-linux-lexical-rules-wrapped.jsonl),
[unwrapped](editor-performance/production-linux-lexical-rules-unwrapped.jsonl).
These twelve observations establish the row-count improvement; they do not
establish interactive latency percentiles or resolve cold shaping and long-row
measurement. Local fixtures recover source without a browser directory handle,
so physical native folder permissions remain a separate gate. A later pointer
readiness fix flushes a pending validated source frame before a click; its release
module differs and is not included in these timing records.

Validation passed 438 core tests, all 408 WASM browser tests and strict all-targets
frontend Clippy. The updated release app also passed fourteen trusted pointer
cases (Rust/C#/JSON, short/scrolled and wrapped Rust) and four bounded-native
composition cases (local/remote, LF/CRLF). The cold unwrapped contract specifically
clicks a scrolled source row while full measurements are pending, then verifies
insertion at the source offset and retained whole-file extents.

## Retained paragraph mutation check

`tools/measure-editor-row-update.py` isolates paragraph mutation using the built
stylesheet, Monaspace Neon with texture healing/ligatures enabled, and fresh
Chromium 154 processes. The Linux container retains the 10-GiB/four-core limits
and CJK/emoji fallback fonts. It compares minimal `Text.replaceData`, replacing
text data and rebuilding nodes after initial layout. Each completed case checks
complete text and compares row width/height plus one sampled caret across methods.
These checks do not prove every glyph or end-to-end application input behavior.

The first single-text-node surrogate completed 27 cases, then timed out on the
first wrapped Unicode case even with a 180-second command deadline. Its missing
wrapped result is recorded explicitly. This surrogate does not match the editor's
existing 512-byte, grapheme-preserving `editor-text-run` markup, and its glyph
geometry differs; it must not be used as an application performance estimate.
A separate 4,096-unit, space-boundary split completed one wrapped Unicode sample
in about 4.5 seconds, but still retained a multi-second input task.

The matching plain-run fixture then completed all 36 cases: ASCII/Unicode,
wrapped/unwrapped, three update methods and three repetitions. A midpoint `X`
insertion retained row dimensions and sampled caret positions across methods
within 0.25 CSS pixels. Median elapsed times in milliseconds:

| Source/layout | Minimal edit total | Minimal edit mutation | Minimal edit layout | Set data total | Rebuild total |
| --- | ---: | ---: | ---: | ---: | ---: |
| ASCII, unwrapped | 112 | 110 | 3 | 124 | 947 |
| ASCII, wrapped | 120 | 88 | 32 | 163 | 937 |
| Unicode, unwrapped | 3483 | 3474 | 10 | 3272 | 3762 |
| Unicode, wrapped | 3508 | 3428 | 80 | 3432 | 3986 |

Rebuild totals include JavaScript `Intl.Segmenter` and DOM creation in this
isolated fixture; the application builds markup in Rust. Those totals cannot be
substituted for measured application rebuild costs. More importantly, minimal
mutation still performs multi-second work on Unicode even when the subsequent
rectangle read is short. A retained DOM cache alone therefore does not establish
responsive admitted paragraphs. Keep bounded cold/changed paragraph preparation,
font fallback, bidi windows and application latency/PSS verification open.
Chromium's [inline layout description](https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/core/layout/inline/README.md)
and [LayoutNG overview](https://developer.chrome.com/docs/chromium/layoutng)
explain the paragraph-level shaping/cache boundary; DOM node identity alone is
not evidence that an update is local.

Raw records: [single-node surrogate, including timeout](editor-performance/layout-row-mutation-linux.jsonl),
[space-boundary split](editor-performance/layout-row-mutation-split-wrapped.jsonl),
[matching plain-run fixture](editor-performance/layout-row-mutation-production-runs.jsonl).
The timeout footer on the first record was added after the old harness terminated;
new runs emit timeout records directly. The body mutation/geometry phases of that
failed wrapped command remain unresolved. No PSS was collected by this isolated
probe, and no native folder handle was granted. Existing both-mode application
contracts and their performance gates remain separate.

Reproduce the matching fixture after building the frontend/backend, using the
existing measurement image (no new Cargo target):

```bash
docker run --rm --init --shm-size=1g --memory=10g --cpus=4 \
  --mount type=bind,src="$PWD",dst=/workspace/repos/openwebide \
  --entrypoint python3 openwebide:editor-view-measurements \
  tools/measure-editor-row-update.py --repeat 3 --source ascii unicode \
  --operations production-replace-data production-set-data production-replace-node \
  --timeout 60
```

## Bounded unwrapped paragraph preparation

Eligible source-monotonic unwrapped rows now use styled probes of at most 16 KiB.
The shared editor facade supplies the original renderer's grapheme-safe paint-run
boundaries; the core continuation plan selects overlapping probes and validates
every overlap glyph before publishing sparse document anchors. DOM adapters
measure actual fonts and styles, preserving the same policy in local and remote
projects. Unsupported boundaries, tabbed/bidirectional rows and failed overlap proofs
retain complete measurement. Wrapped rows still use the complete path.

Glyph rectangles are captured near the origin to avoid large-coordinate
precision loss. After capturing local rectangles, the adapter measures final overflow through
normal inline layout at document coordinates. Transforming overflow or translating
an already-rounded slice width can round differently from the original row. No estimated character
widths or relaxed geometry tolerances are used.

Browser contracts compare complete integer scroll extents and every retained
anchor within 0.25 CSS pixels, including a near-1-MiB Unicode row, all five bundled
Monaspace families, texture healing, ligatures, visible whitespace and italic
comments in both workspace modes. An injected overlap failure checks complete
measurement fallback without changing document ownership or source.

This bounds the styled probe, not the entire input pipeline. Initial native
paragraph shaping, source-prefix segmentation, tabbed/wrapped/bidirectional preparation
and incremental reuse within changed paragraphs remain open. Production latency and PSS measurements remain separate from these correctness
contracts.

Release measurements on 2026-10-08 used Chromium 154 on Linux arm64, the existing
measurement image with CJK/emoji fallback fonts, four CPU quotas and a 10 GiB
container limit. Each of three repetitions per mode used a fresh browser/runtime
and a 1,048,572-byte Unicode row. Local cases used editor recovery without a native
folder handle. No build ran concurrently with these six samples. The raw header
records the pre-commit checkout; `appModule` identifies the built frontend.

| Mode | Cold paint median / range (ms) | Input paint median / range (ms) | Scroll paint median (ms) | Peak Chrome PSS median (KiB) | Largest task median (ms) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Local | 11414 / 11179–12006 | 2623 / 2616–2684 | 26 | 850760 | 4052 |
| Remote | 11856 / 11732–11943 | 2657 / 2490–2942 | 20 | 840722 | 3780 |

These rows remain outside the interactive latency goal. Initial native shaping
still causes multi-second tasks, and changed paragraphs repeat their probe work.
Steady-state scroll paint is short, but that does not establish responsive cold
loading or typing. Peak PSS includes the whole Chrome process tree; it is not
Rust/WASM allocation alone.

Raw records: [three repetitions per mode](editor-performance/unwrapped-paragraph-linux.jsonl).

A separate instrumented repetition per mode recorded 185/177 layout probes and
182/174 preparation batches, with no complete-row fallback and no trace truncation.
The largest paragraph-render input was 15,914 bytes; sampled local layout calls
peaked at 15/16.2 ms. The trace includes probe and worker diagnostics, so its
end-to-end timings are separate from the uninstrumented table.
[Raw diagnostic records](editor-performance/unwrapped-paragraph-linux-trace.jsonl).

Reproduce after building the current frontend/backend:

```bash
bash tools/measure-editor-view-linux.sh --cases long-line --repeat 3
bash tools/measure-editor-view-linux.sh --cases long-line --repeat 1 --trace
```

CI runs the font matrix and near-1-MiB complete-geometry oracle in separate browser
runs, retaining the 300-second binary deadline and existing short UI readiness
assertions. This keeps the expanding suite's aggregate duration from hiding
individual geometry failures.

## Resuming paint at indexed run boundaries

Continuation probes now carry the original paint-run boundary already proved by
the shared plan. Plain viewport slices use the existing source index's binary
lookup to make the same declaration. The renderer resumes grapheme segmentation
from that boundary instead of rescanning the token's unused prefix, retaining
original shaping spans even when the selected crop is shorter than 512 bytes.
Styled viewport slices without a proved boundary retain prefix segmentation.

The browser DOM oracle compares aligned and unaligned crops with cloned original
markup, including Unicode, combining clusters and escaped text. Core index checks
compare boundary declarations with the complete renderer's run ranges. Both-mode
geometry contracts still compare complete extents/anchors through the 1 MiB row
limit and all five fonts/features; the broader UI suite passes.

Six fresh release samples on the same Linux/Chromium setup and Unicode source as
the preceding table, three repetitions per mode (2026-10-08):

| Mode | Cold paint median / range (ms) | Input paint median / range (ms) | Scroll paint median (ms) | Peak Chrome PSS median (KiB) | Largest task median (ms) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Local | 10085 / 9720–10654 | 2086 / 2072–2110 | 21 | 823600 | 3581 |
| Remote | 10603 / 9912–11571 | 2085 / 2066–2106 | 22 | 828989 | 3650 |

Input medians decrease by about 20–22 percent against the preceding samples.
These observations still do not satisfy interactive long-row latency: startup
shaping and changed-paragraph preparation remain multi-second work. Tabbed,
wrapped and bidirectional preparation, styled viewport prefix scans and
incremental changed-paragraph reuse remain open. Local cases grant no native
folder handle. The recorded `appModule` identifies the measured release bundle;
the header's checkout is the pre-commit base.

[Raw repeated measurements](editor-performance/aligned-paint-runs-linux.jsonl).

The language/parser browser contract also uses a scoped synthetic clock for its
synchronous facade calls: grammar correctness does not race the real 12 ms
interactive budget on a slow runner. A separate advancing-clock check verifies
deadline cancellation, alongside caller cancellation, oversized source rejection
and recovery. The production parser budget is unchanged; the clock is restored
before assertions or awaits.

A separate instrumented repetition per mode recorded no complete-row fallback or
trace truncation. Input preparation used 71 batches in each mode. Paragraph HTML
generation totalled 12.5/13.0 ms, local layout 794.9/766.7 ms, and geometry plus
final overflow 1234.4/1211.3 ms (local/remote). These diagnostic totals identify
layout/geometry as the next input bottleneck; they do not include initial native
shaping, and instrumented end-to-end timings are not substituted into the table.
[Raw diagnostic records](editor-performance/aligned-paint-runs-linux-trace.jsonl).


## Dense coordinates within bounded continuation probes

Each continuation probe now builds its byte/native-UTF-16 coordinate table once.
Every requested overlap glyph still receives an exact DOM range measurement;
complete-source extents, anchor tolerances, stale-result guards and fallback
conditions are unchanged. Ordinary sparse viewport queries retain their sparse
index. This transient dense table is limited to the existing 16 KiB probe.

The complete-row geometry contracts pass for both adapters, the near-1-MiB
Unicode row and all five Monaspace families/features. Injected inconsistent
geometry still falls back. The current-head browser suite and strict frontend
Clippy checks pass. The Git file status contract now checks the accessible
Modified title and SVG icon introduced by the concurrent navigation checkpoint.

Three release repetitions per adapter on the same Linux/Chromium setup
(2026-10-08):

| Mode | Cold paint median / range (ms) | Input paint median / range (ms) | Scroll paint median (ms) | Peak Chrome PSS median (KiB) | Largest task median (ms) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Local | 9119 / 9024–9417 | 1810 / 1799–1830 | 31 | 835999 | 3348 |
| Remote | 9141 / 9116–9182 | 1826 / 1815–1840 | 29 | 844624 | 3386 |

These samples include the concurrently committed navigation changes at
`cc8c2ae`; they are not an isolated same-checkout comparison against the previous
table. Local cases grant no native folder handle. The recorded `appModule`
identifies the measured release, including the uncommitted coordinate change.
Long-line input and initial shaping remain outside interactive latency targets.
Tabbed/wrapped/bidirectional preparation, incremental changed-paragraph reuse
and remaining physical-input verification are still open.

[Raw repeated measurements](editor-performance/dense-probe-coordinates-linux.jsonl).

A separate instrumented repetition retains all exact range calls and records no
complete-row fallback or trace truncation. Input preparation uses 71 batches per
adapter. Render/layout/geometry-plus-final-overflow totals (ms):

- Local: 11.8 / 697.7 / 909.5.
- Remote: 12.0 / 722.0 / 906.9.

Two layouts per continuation probe remain a substantial cost. These diagnostic
totals exclude initial native shaping and are separate from the uninstrumented
latency table. [Raw diagnostic records](editor-performance/dense-probe-coordinates-linux-trace.jsonl).


## Rejected co-installed local and global overflow probes

A one-row transform candidate was rejected: translating a global-gap row near
the local origin still failed the near-1-MiB complete-geometry contract (the
shared overlap proof rejected preparation). The shorter font matrix passed,
which is insufficient evidence for large-coordinate correctness. No tolerance,
source limit or fallback was relaxed.

The second candidate installed a bounded clone of the local probe with its original
global inline gap before any layout reads. Exact glyph ranges use the first row's
small coordinates, while exact integer overflow uses the clone's original inline
layout. Both are cleared together after the existing source-owned batch. Shared
planning, admission, overlap checks and adapter selection remain unchanged.
The probe remains at most 16 KiB; transient DOM now contains up to two copies.

The near-1-MiB differential oracle, all five Monaspace families/features and
injected-overlap fallback pass in both adapters. Horizontal tab fallback,
scroll extent and native hits pass. The ordinary browser suite passes 12 library,
399 component and 4 integration tests, with the two heavy matrices verified
separately. Strict frontend Clippy and the release build pass.

Three release repetitions per adapter using the same source fixture, browser and
resource limits as the dense-coordinate baseline. The production sources match
that baseline apart from the paired-probe candidate; the earlier baseline captured
the dense-coordinate change before its commit:

| Mode | Cold median (ms) | Input median / range (ms) | Scroll median (ms) | Peak Chrome PSS median (KiB) | Largest task median (ms) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Local | 10164 | 2518 / 2486–2527 | 22 | 823547 | 3401 |
| Remote | 10430 | 2542 / 2542–2587 | 27 | 822375 | 3498 |

The paired-row candidate regresses input latency against the 1810/1826 ms
baseline despite passing exact geometry contracts. It has been removed; production
retains the faster sequential local/global measurement. Correctness alone did not
justify shipping more transient DOM. The `appModule` in these records identifies
the rejected release, whose source was uncommitted over `7c15bde`.
[Raw candidate measurements](editor-performance/paired-probe-candidate-linux.jsonl).

Next, reuse validated measurement prefixes inside changed paragraphs rather than
repeating unchanged continuation probes. Reuse must establish source/run/style
identity, preserve the complete renderer's shaping boundaries, validate fresh
continuation overlaps and reject stale font/layout/project/account ownership.
This is still unimplemented; tabbed/wrapped/bidi preparation and physical-input
verification remain requirements.


## Reusing unchanged prefixes within edited paragraphs

The shared paragraph plan retains exact completed probe records and original paint
run boundaries. On a later source revision, the editor facade proves matching
account/project/file/read/font/metrics/indentation/whitespace ownership and a
source/style prefix. Core replay compares source bytes and run boundaries, then
uses normal target, dimension and overlap validation for each retained record.
The first changed probe is freshly measured with the preceding exact overlap;
failed admission or validation retains complete-row fallback.

Records share immutable rectangle allocations and the facade retains its existing
source projection. Retention is capped at 128 Ki glyph rectangles per paragraph
and eight rows per current environment. Crossing the cap disables retention rather
than changing measurement or source admission. Source/style-identical rows survive
scope updates; account/project/font changes reject reuse. Wrapped, tabbed and bidi
preparation retains its previous path.

All 445 native core tests pass, including changed-prefix equivalence, invalid
records and fresh overlap rejection. Both-mode browser contracts compare reused
geometry with the complete renderer and reject stale account, project, font and
layout scopes. The ordinary suite passes 12 library, 400 component and 4 integration
tests; the near-1-MiB and five-font matrices also pass separately. Two additional
browser policy tests reject changed token styles/span topology, indentation guides
and line-ending paint. All 114 native frontend tests pass. Strict core and frontend
Clippy and the release build pass.

The production harness now supports `--input-position end`. It navigates through
the real Ctrl+End handler, verifies the persisted full-source selection and
unchanged source before typing, and checks the recovered complete source equals
the original plus the inserted character. Timing still begins at `beforeinput`;
navigation and its persistence wait are excluded. The default beginning/native-window
case remains available as a control.

A separate instrumented EOF repetition per adapter recorded one fresh input
preparation batch, 192.9/182.7 ms input paint (local/remote), no complete-row fallback
and no trace truncation. These diagnostic timings are separate from repeated
uninstrumented samples. [Raw EOF trace](editor-performance/paragraph-prefix-end-linux-trace.jsonl).

Initial native shaping, beginning-of-line edits, incremental suffix reuse,
tabbed/wrapped/bidi preparation and physical-input verification remain open.

Three fresh release repetitions per adapter and input position, using the same
near-1-MiB Unicode fixture and Linux/Chromium resource limits (2026-10-08):

| Input | Mode | Cold paint median (ms) | Input median / range (ms) | Peak Chrome PSS median (KiB) | Largest task median (ms) |
| --- | --- | ---: | ---: | ---: | ---: |
| Start | Local | 9218 | 1825 / 1823–2267 | 838348 | 3367 |
| Start | Remote | 9164 | 1828 / 1816–1843 | 838906 | 3389 |
| End | Local | 9326 | 194 / 185–196 | 862510 | 6972 |
| End | Remote | 9418 | 180 / 179–200 | 875106 | 7035 |

The beginning-control medians remain close to the preceding 1810/1826 ms samples;
one local repetition reaches 2267 ms. EOF edits preserve nearly the entire
measured prefix, but their 180–194 ms medians still miss tighter interactive
latency goals. Cold startup remains multi-second work. Input positions exercise
different amounts of changed source; this table does not substitute EOF timings
for beginning or arbitrary interior edits. EOF runs verify the persisted complete
source after input. Their memory peaks include navigation and its recovery save.

[Beginning control records](editor-performance/paragraph-prefix-start-linux.jsonl),
[EOF records](editor-performance/paragraph-prefix-end-linux.jsonl). Header checkout
`f4937a6` is the pre-commit base; `appModule` identifies the release including the
uncommitted prefix implementation. To repeat:

```bash
bash tools/measure-editor-view-linux.sh --cases long-line --repeat 3
bash tools/measure-editor-view-linux.sh --cases long-line --repeat 3 --input-position end
bash tools/measure-editor-view-linux.sh --cases long-line --repeat 1 --input-position end --trace
```


### Font-matrix readiness budget

The docs-only `f4937a6` CI run failed its isolated font matrix after 3.63 s:
`Timed out waiting for font-owned paragraph measurements`. All other jobs passed;
the preceding implementation run was entirely green. This is the debug-WASM
matrix with two long string/comment source paragraphs, not a production latency assertion.
The then-current 10 KB grammar cutoff could make their rendered tokens plain;
the stronger styled contract is described below. Its
ownership/preparation wait now uses the same explicit 30-second boundary-size
budget as the near-1-MiB oracle. Source/font ownership and every exact geometry
comparison remain unchanged. The 300-second whole-run deadline and ordinary
3-second short UI readiness checks remain. Release measurements above continue
to expose unresolved latency independently of this correctness budget.

Two fresh isolated browser runs pass the unchanged full font/feature/whitespace
matrix in 17.20 and 16.75 s overall; strict frontend Clippy passes.


### Rejected early native bootstrap

An isolated early-binding experiment tried installing the exact bounded native
context before complete unwrapped source shaping. The existing both-adapter cold
contract holds complete geometry preparation pending: 12,000 Unicode/CRLF rows
include a wide row at source row 9,000. On the first local case, source scroll
height/width remained 230/327 px while the bounded textarea measured 14,259/326 px
(11,559 native characters). The unchanged assertion requires complete source
height above 100,000 px before far pointer/scroll/edit operations. The experiment
failed there; source-mapped input alone does not supply whole-document geometry.

An additional all-rows `source_paint_eligible` filter made the test pass by
bypassing early binding: short rows are outside that predicate's long-paragraph
eligibility. That pass is not evidence for the experiment. Numeric pending-scroll
requests do not repair missing extents. The prototype was removed; production
retains complete initial native shaping until an exact early-extent path passes
the original cold contract. Initial native shaping remains on the roadmap.


### Layout-candidate advance precision

The isolated native/WASM candidate now sums the same shaped glyph advances in
`f32` and `f64`, without changing shaping, wrapping or the production renderer.
Both strict native/WASM lint checks and all 10 native/20 browser records pass.
The supplied font hash matches the preceding candidate sample. For the
990,000-byte unwrapped ASCII fixture (features enabled), native and WASM agree:

| Measurement | Width (CSS px) |
| --- | ---: |
| Parley layout | 7,933,542 |
| Glyph advances summed in `f32` | 7,928,103.5 |
| Same glyph advances summed in `f64` | 7,979,400.4154 |
| Browser DOM | 7,979,337 |

The identical advance sequence differs by 51,296.9154 px solely from accumulation
precision. Parley's published width differs from the naive `f32` sum as well;
this check does not attribute every internal layout error to that one sum. The
`f64` result still differs from DOM by 63.4154 px and does not establish glyph,
caret or wrapped geometry parity. No substitute extent is published.

Glyph-ID-zero counts are 0 for ASCII, 2 for the tab fixture, 2 for the combining/
CJK/emoji fixture and 8 for mixed LTR/RTL text. These counts expose the candidate's
missing font/tab integration; they do not identify a specific missing character
or prove browser fallback equivalence. The bare long-row browser sample takes
540 ms for Rust layout and 49.625 ms for DOM layout, with the same timing-scope
limitations as the preceding probe. Production latency remains unproved.

[Raw advance diagnostics](editor-performance/layout-candidate-advances.jsonl).
Reproduce with the existing layout tool command above; each record now includes
`advances`. Exact early source extents, tab stops, explicit fallback font ownership,
a painter with matching geometry and full native-input contracts remain required.


### Prepared long-line styles and viewport run reuse

Prepared grammar rows no longer inherit the lexical renderer's 10,000-byte
plain-line cutoff. Grammar source/work limits remain unchanged (including
2 MiB/50,000-row preparation admission and bounded metadata); unavailable or
cancelled grammar still uses the independently limited lexical fallback. The
shared row assembler preserves exact source text and token-piece reuse. Native
contracts cover long Unicode strings in Rust, the requested ten GitHub languages,
JSX/TSX, HTML and CSS, plus unavailable/oversized analysis and lexical fallback.

Eligible styled viewport slices reuse the completed paragraph's immutable run
boundaries, using binary search rather than segmenting a long token's unused
prefix. The facade requires the same document version, source/token/guide
allocations, account/project/read/font/metric scope and whitespace/indentation
settings. Layout reconciliation alone may retain those source-owned boundaries.
Slices include the preceding original run before exact DOM range cropping and
retain the 64 KiB cap. Uncached or unsupported scopes keep the original path.

DOM range cloning omits its common ancestor. Cropping now preserves the original
inline token/run wrappers, retaining color and italic style, while rebuilding
absolute fragment geometry instead of nesting the previous fragment's position.
Existing anchor and every-glyph checks, complete-source fallback, scroll extents
and source/native mapping remain.

A 162,016-byte Unicode/CRLF Rust string fixture runs through the shared syntax
service in both adapters and scrolls to 30%, 70% and 98% of the document width.
It requires actual String tokens, source-exact fragments, retained token wrappers,
unchanged complete scroll extent and native source, and pointer hits in the
painted interval. A test-only counter checks actual segmentation stays below
64 KiB plus one run of lookahead, rather than counting only escaped output.
Ownership contracts reject replacement documents even with identical bytes,
new token/guide allocations, account/project/read/font/style changes and stale
view revisions.

The five-family/feature/whitespace matrix now also uses the shared syntax service
and requires actual long String and Comment tokens before comparing exact
geometry with complete production paint. Earlier source-shaped matrix samples
could pass with plain fallback; they do not prove long-token color/italic parity.
Two fresh strengthened matrix runs pass in 17.42 and 17.40 s; the isolated
near-1-MiB oracle passes in 22.04 s. The preceding docs-only
`c3dd901` CI font matrix timed out at its existing 30-second readiness boundary;
readiness now names the exact mode/font/settings case and separates grammar
preparation from font/geometry ownership. No readiness deadline or geometry
assertion is relaxed.

This change does not prove production latency or memory improvement. Initial
native shaping, uncached styled slices, tabbed/wrapped/bidi preparation and
incremental suffix measurements remain open; release-app measurements must
continue to distinguish those cases.

Validation: 447 core native tests, 114 frontend native tests, 15 WASM library /
401 ordinary component / 4 integration contracts, plus the two isolated matrices
(422 unique browser tests). Strict core and WASM frontend Clippy and the complete
Trunk release/PWA build pass. These are correctness checks, not production
latency samples.


### Styled release boundary and rejected DOM-call batching

`styled-long-line` supplies a 1,048,567-byte valid Rust constant containing a
Unicode string, below the admitted 1 MiB logical-row boundary. Unlike a neutral
long-line fixture, it requires an actual `.tok-string` in the current source-owned
paint at initial readiness, after scrolling and after native input. Its snapshot
records `styledString`. Both production adapters retain that styling; these
recovery-based local samples still do not exercise an OS directory handle.

Three fresh uninstrumented Linux/Chromium repetitions per adapter compare the
verified renderer with a rejected measurement-only candidate under the same
4-CPU/10-GiB container limits. The candidate performs every original DOM range
read in one JavaScript primitive, with endpoints and all geometry/overlap policy
still in Rust. A differential browser test matches scalar glyph rectangles bit
for bit across Unicode, multi-node text, tabs and italic spans, rejects invalid
source/endpoints, and retains empty-target behavior. All four paragraph contracts
(including both modes, fonts/features, near-limit complete-geometry comparison,
changed-prefix reuse and failed-overlap fallback), the 16-test WASM library and
strict frontend lint pass. Correctness does not establish performance benefit.

| Renderer | Mode | Cold paint median (ms) | Beginning input median / range (ms) | Scroll median (ms) | Peak Chrome PSS median (KiB) | Largest task median (ms) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Verified scalar | Local | 9211 | 1731 / 1724–1746 | 21.5 | 834044 | 3510 |
| Verified scalar | Remote | 9279 | 1755 / 1752–1772 | 28.3 | 830076 | 3427 |
| Rejected batch | Local | 9232 | 1735 / 1734–1744 | 26.4 | 822550 | 3411 |
| Rejected batch | Remote | 9471 | 1743 / 1706–1746 | 24.5 | 835406 | 3536 |

There is no material input improvement across these samples; cold startup stays
multi-second. Three repetitions are not a latency distribution or a general
regression/performance guarantee. Source/probe and worker policies are unchanged.
The candidate source was removed, and the verified production release rebuilt.
[Rejected patch](editor-performance/range-batch-rejected.patch),
[baseline records](editor-performance/range-batch-before-linux.jsonl),
[candidate records](editor-performance/range-batch-after-linux.jsonl). Header
checkout `41c3df9` describes the measurement worktree; `appModule` separates the
verified `58db9da3b934f406` bundle from the rejected `1fb4488f17965066` candidate.

A separate instrumented scalar repetition records actual styled readiness,
9556/9187 ms cold paint and 1784/1766 ms beginning input (local/remote). Its 71
input probe batches spend about 1632/1616 ms combined in paragraph layout and
geometry, with only 12/12 ms in source rendering; both traces are untruncated.
These diagnostic timings include instrumentation and are separate from the
repeated uninstrumented samples. [Raw styled trace](editor-performance/styled-long-line-linux-trace.jsonl).

The next optimization must reduce actual preparation/layout work or provide an
exact incremental geometry path. Bulk calls alone do not complete initial native
shaping, changed suffix reuse, tabbed/wrapped/bidi preparation or memory gates.
Reproduce the scalar samples with:

```bash
bash tools/measure-editor-view-linux.sh --cases styled-long-line --repeat 3
bash tools/measure-editor-view-linux.sh --cases styled-long-line --repeat 1 --trace
```

## Exact-origin suffix candidate check

An unshipped suffix-replay experiment required identical source byte offsets,
paint-run boundaries, dimensions, measured origin and every incoming overlap
rectangle. Core synthetic geometry and the plain Unicode browser case passed.
The styled browser case exposed a whole-paragraph overflow mismatch that also
occurs with fresh preparation of the original and changed source, so exact
reconnection alone does not prove the existing rounded scroll extent.

Fixture: `const VALUE: &str = "a` followed by 12,000 repetitions of
`word 文😀  ` and `";`, default Monaspace settings, 340 px local workspace pane.
Replace the first string character `a` with `z`; require actual grammar String
paint and compare every anchor and scroll extent against the complete renderer.
Original fresh, changed retained and changed fresh preparation all reported
1,028,352 px overflow against 1,028,351 px complete overflow. The final glyph
left edge was 1,028,343.390625 px against 1,028,343.375 px, a 1/64 px difference
that still crosses the overflow rounding boundary. Anchor tolerance and the
extent oracle were unchanged. The failure occurs in the local styled case;
this failing run did not reach the remote styled case.

Keeping the viewport row width instead of the zero-width overflow probe did not
remove the failure. Both experiments were removed from production. The next
measurement/reuse work must establish complete overflow rounding, not infer it
from local overlap agreement. Native shaping and shifted/wrapped/bidi reuse
remain open. [Rejected patch](editor-performance/suffix-reconnection-rejected.patch),
[oracle output](editor-performance/suffix-reconnection-failure.txt).

### Fractional global offset correction

The original fresh-source counterexample is now a dedicated browser regression
in both workspace modes, still comparing every glyph and the rounded scroll
extent against the complete production renderer. It failed before the fix with
the same 1,028,352 versus 1,028,351 px mismatch.

The DOM adapter keeps the fractional origin in its own inline-block spacer and
adds integer spacers of at most 1,048,576 px, rather than assigning the whole
large fractional origin to one CSS length. Local glyph measurement, overlap
validation and the complete-renderer oracle are unchanged. The focused optimized
Chrome run passes both adapters (1.83 seconds). The full optimized-WASM suite
passes all 424 checks: 422 ordinary checks, the font matrix (14.66 seconds) and
the near-1-MiB matrix (21.18 seconds). Strict frontend WASM lint and formatting
also pass. This establishes the correction
for the regression fixture; it does not ship or prove suffix reconnection,
wrapped/tabbed/bidi preparation or initial native shaping.

### Exact-origin suffix replay

After the fractional-offset correction, the original suffix candidate passes
plain and grammar-styled browser geometry in both workspace modes. A first
String character replacement (`a` to `z`) reuses more than two suffix probes,
while original fresh, changed retained and changed fresh preparation each match
the complete production renderer's scroll extent and every glyph anchor. The
focused ownership run took 5.02 seconds; that is a contract-test duration, not
an application latency measurement.

The shared core replays only records with identical byte/run positions,
dimensions, measured origin and every incoming overlap rectangle. The facade
holds immutable source/style proof and rechecks current ownership before replay.
Captured font, layout, read, pending-source, account, project and file changes
reject reuse; fresh owner-scoped requests remain eligible. Subpixel/whole-pixel
origin shifts and changed lengths require fresh measurement. Rectangle storage
remains shared and retained limits are unchanged. No translated suffix geometry
is admitted; insertion/deletion, shifted suffixes, initial native shaping and
wrapped/tabbed/bidi incremental preparation remain open.

Verification: 448 native core tests, 114 native frontend tests and all 426 browser
checks pass (424 ordinary checks plus the font and near-limit matrices). The
matrices took 14.85 and 24.40 seconds locally. Strict native core and WASM frontend
lint, formatting and the Trunk/PWA release build pass; these timings do not
establish application latency.

## Browser CI module footprint

The `faf1040` CI run passed all 421 ordinary browser checks and the separate
font matrix, then ChromeDriver timed out waiting for the renderer while loading
the final boundary-test run (`missing field chunk`, 30-second renderer timeout).
The test runner polls output with a synchronous WebDriver script; its 300-second
whole-run budget does not prevent that lower-level command timeout.

Browser CI now runs the same tests with `--release`, retaining all source/geometry
assertions, separate matrices and existing readiness/whole-run deadlines. Local
optimized-WASM verification passed 421 ordinary checks plus both matrices. The
components module is 48,723,270 bytes (46.47 MiB) instead of 260,821,407 bytes
(248.74 MiB) in debug mode, measured after restoring the same production source. Ordinary
checks took 118.50 seconds, the font matrix 12.64 seconds and the near-limit
check 18.88 seconds in this local run;
these timings do not prove Linux CI reliability. The full CI run must verify the
profile change on its own Chrome/host. Native tests and strict debug-target lint
remain unchanged.

The optimized-profile CI run `37767455776` passed all 421 ordinary checks and
the font matrix, then failed the near-1-MiB test's unchanged 30-second readiness
deadline (`admitted paragraph measurements`). The boundary geometry comparison
was not reached. Optimized module loading alone therefore does not establish CI
reliability; investigate the preparation delay without relaxing its geometry
or readiness assertions.

The next full CI run (`37769663927`, fractional-offset checkpoint) passed all five
jobs. Its browser job passed the ordinary suite, font matrix, near-1-MiB contract
and browser storage measurements. The near-limit test took 69.16 seconds across
both adapters and differential oracles, retaining each unchanged 30-second
readiness deadline. This is one Linux success; the previous readiness timeout
still requires repeat verification before claiming reliable CI.

## Original styled runs without completed geometry

Original-run boundaries now have a separate source-owned cache, available before
paragraph geometry finishes and for styled rows that retain complete preparation.
The DOM adapter calls the editor facade; shared policy builds exact token/run
boundaries once, caps retained tables at eight rows and 16,384 ends per row, and
keeps the existing uncached path when retention limits do not fit. Paragraph
plans and completed measurements share the same immutable boundary allocation.
The cache does not publish dimensions, estimated glyph anchors or partial layout.

The browser regression clears both paragraph measurements and run metadata, then
visits three new horizontal positions in a grammar-styled Unicode/CRLF row. The
first table build segments the long token; later paints segment zero source bytes
for run-table construction and share its allocation. Actual cropped HTML still
segments its small selected runs, with the unchanged 65,536-byte source-paint and
65,536 + 1,024-byte segmentation ceilings. It checks exact UTF-16 source text,
String wrappers, unchanged complete scroll width and pointer hits in each slice.
Both workspace modes pass the focused run (0.96 seconds). Stale font, layout,
view, read, pending-source, account, project and file scopes cannot replace the
current table; paragraph preparation reuses it without another segmentation pass.

This is source-work and geometry-contract evidence, not application latency
percentiles. First-build segmentation, over-limit tables, cold native shaping
and tabbed/wrapped/bidi preparation still require further work.

Verification: 449 native core tests, all 426 browser checks, strict native core
and WASM frontend lint, formatting and the Trunk/PWA release build pass. The font matrix took 14.84 seconds;
the near-limit contract took 21.52 seconds locally. Neither this metadata change
nor the earlier suffix change establishes CI reliability: run `37771757040`
passed four jobs and the ordinary/font browser checks, then again hit the
unchanged near-limit readiness deadline before the differential oracle.

The near-limit test now logs opt-in row-probe phase/scope totals and native/font
readiness every five seconds, retaining all deadlines and geometry assertions.
Its local sample at 5,832 ms had 748,980 native UTF-16 units installed, loaded Neon
fonts, no row probes yet and no bounded-native declaration. That points to cold
startup/native layout before row preparation; it is not proof of the Linux
failure's cause. The next CI failure can show whether probes started, were
cancelled, or fell back to complete measurement.

### Native installation and in-flight font notifications

CI run `37774838140` again passed four jobs and the ordinary/font browser
contracts, then failed the unchanged near-limit readiness deadline. Its first
diagnostic report arrived at 28,768 ms with the complete 748,980-unit native
value, loaded Neon faces and no row probes. CI reliability remains unproved.

The [native installation trace](editor-performance/native-install-before.jsonl)
separates text installation, selection restoration and browser layout reads.
Text installation took less than 0.5 ms and selection restoration less than
0.02 ms. The first scroll-surface width read during native scroll restoration
forced 3,803 ms of layout in Local and 3,145 ms in Remote. These are single Mac
Chrome observations, not Linux timings or latency percentiles. Combining scroll
updates and sizing an empty textarea before installing text did not improve
startup in their local samples; neither production candidate is retained.

The same trace records a disconnected 16-probe job followed by a new job with
an advanced font/layout generation. Previously a trusted font notification
always invalidated in-flight measurements because only completed geometry could
establish identical metrics. The shared facade now retains each current ticket's
original paint environment. A matching notification can retain that job without
publishing partial geometry. Missing provenance, changed face availability/CSS
metrics and stale source/view/read/account/project/file ownership still invalidate;
synthetic notifications retain their explicit refresh behavior.

The both-mode ownership regression verifies matching/changed/unknown metrics,
immutable ticket provenance, replacement-ticket cleanup, and font, layout, view,
read, source, pending-edit, account, project and file invalidation. All 428 browser
checks pass: 426 ordinary checks, the font matrix (15.37 seconds), and the
near-limit differential contract (21.59 seconds). Those elapsed samples do not
prove startup latency or CI reliability; the complete-native layout remains.
The 114 native frontend tests, strict WASM frontend lint, formatting and
Trunk/PWA release build also pass.


### Source-owned native input before initial long-row layout

CI checkpoints `37781861651` and `37782390116` both passed all five jobs after
retaining matching in-flight font measurements. These repeated successes do not
complete the broader Linux latency and memory gates.

The next startup candidate installs the existing source-owned native context
before the first native value when the initial row exceeds 65,536 bytes and the
projection is uniform, unwrapped and free of unsupported long-row tabs or bidi.
The shared facade retains source selections and a pending-geometry state; the DOM
adapter does not publish the native window's dimensions as complete extents.
Pointer selection waits for current source paint and measured extents. Automatic
scroll clamping cannot overwrite the saved source scroll during this interval.
Failed measurement restores complete native input; a composing input method keeps
its installed window until commit/cancel before that fallback.

The [window installation sample](editor-performance/native-install-window.jsonl)
records the unchanged near-1-MiB plain-text Unicode paragraph in Mac Chrome. Both modes installed
8,776 UTF-16 units, rather than 748,980. Current measured paint was ready at 1,863 ms
Local and 1,704 ms Remote. The maximum scroll-surface width read was 26.895 ms Local
and 5.550 ms Remote, compared with the earlier 3,803/3,145 ms reads. The strengthened
contract rejects any initial native value above 12 KiB and compares the resulting
complete source extents/anchors/hit geometry with the complete renderer. That run
passed in 13.21 seconds, including both modes and their complete-renderer oracles.
These are individual observations, not latency percentiles or Linux evidence. This
plain-text boundary test does not prove near-limit styled Rust String behavior.

Cold short-first-row, wrapped, tabbed, bidi and coarse-pointer views retain their
complete-native capability fallback. Initial paint-run construction and paragraph
geometry still visit the long source; this candidate does not complete the cold
startup or bounded-layout roadmap items. All 429 browser checks pass: 427 ordinary checks, the font matrix (15.75 seconds),
and the final near-limit differential contract (13.50 seconds). The near-limit
readiness predicate now requires both visible paint and completed native geometry
within the unchanged 30-second deadline, including when a newer measurement job
supersedes an earlier visible frame. Composition-safe failure and saved-scroll
regressions pass in both modes. Native frontend tests (114), strict WASM frontend
lint, formatting and the Trunk/PWA release build pass. The aggregate invocation
passed all runtime checks but failed dependent-crate resolution during doctests;
a separate current-artifact doctest invocation passed. This startup checkpoint
still needs Linux CI verification.


### Restored long-row startup and admitted Rust String verification

Startup CI `37787470871` passed all five jobs. Its Linux browser job passed 427
ordinary checks (190.69 seconds), the font matrix (41.67 seconds), and the
near-limit plain-text contract (44.29 seconds). Those are complete test-run times,
including both modes and their renderer oracles; passing output suppresses the
per-mode trace. They do not establish Linux startup percentiles or memory bounds.

The shared startup eligibility policy now also admits a restored source caret in
a later long row. Uniform, unwrapped, supported geometry remains required; short
initial selections preserve their existing complete-native cold-frame fallback.
The DOM regression records every native value installation, requires a maximum of
12 KiB, verifies the original source selection and 24,500-pixel horizontal scroll,
and compares the later row with complete-renderer geometry in both modes. The
shared failure/ownership regression also covers this later-row selection.

The styled viewport contract now uses a Rust string literal within 128 bytes of
the 1-MiB line limit, with headroom for a beginning edit. It loads Monaspace Neon
explicitly, requires prepared Rust String tokens, compares initial and edited
paragraph geometry with the complete renderer, and retains the original cached
and uncached scroll, source-slice, paint/segmentation budget and stale-owner checks.
This replaces the earlier 6,000-repeat workload; an initial test incorrectly used
the separate 8-MiB document cap and was corrected to the existing 1-MiB line cap.

The [focused Mac Chrome sample](editor-performance/styled-limit-neon.jsonl)
passed in 16.46 seconds. Initial styled paint was ready
at 1,424.705 ms Local and 1,424.915 ms Remote; complete paint after a beginning edit
was ready after 1,291.275/1,263.180 ms. These are single load-to-paint and
transaction-to-paint observations, not synchronous input latency, repeated
percentiles, Linux evidence or completion of the responsiveness/memory gate.
Broader cold startup, wrapped/touch layout, incremental shifted-source preparation,
source ownership, memory and physical input verification remain open.


The combined browser run initially passed the new editor contracts but exposed
two branch-menu regressions in the incorporated UI changes: cached options were
hidden on reopening while background discovery ran, and a ready open menu was
unmounted on a later loading transition. The shared dropdown now retains a
surface once displayed until it closes; the shared branch picker can present
cached choices during discovery. The existing checkout, stale-result and retained
menu contracts pass unchanged in both modes. Strict lint also caught two modal/tab
test numeric comparisons; infallible conversion and exact bit comparisons retain
their assertions. These integration corrections do not relax editor readiness or
geometry requirements.


Final combined verification passes all 439 browser checks: 437 ordinary checks
(140.66 seconds), the font matrix (15.16 seconds) and the near-limit plain-text
contract (14.22 seconds). The 114 native frontend tests, strict WASM frontend lint,
formatting and Trunk/PWA release build also pass. Linux CI for this combined
checkpoint is still pending; the previously green startup checkpoint does not
verify these later changes.


### Styled transaction timing and bounded metadata attempts

Combined checkpoint CI `37792246345` passed all five jobs, including the full-size
styled Rust contract and restored-caret/shared-menu regressions. This does not
verify the subsequent metadata change or complete the remaining Linux PSS,
wrapped-layout, physical input and repeated responsiveness requirements.

The [split transaction sample](editor-performance/styled-transaction-split.jsonl)
uses the same near-1-MiB styled Rust String, loaded Monaspace Neon, both workspace
modes and unchanged complete-renderer comparisons. The synchronous source
transaction took 32.725 ms Local and 32.425 ms Remote; transaction-to-complete-paint
was 1,290.325/1,263.765 ms. Initial styled paint took 1,427.890/1,388.395 ms. The
focused run passed in 16.84 seconds. These individual observations distinguish
source mutation from downstream layout; they do not establish input percentiles,
whole-app main-thread responsiveness or an absence of other source-copy costs.

Capped styled-run construction previously extended the entire long token before
checking the retained run limit. It now retains no boundary beyond the budget
and segments at most one additional run to detect overflow, instead of scanning
the whole remaining token. A budget exhausted before a long token skips that
token's segmentation entirely. Tests cover zero,
small and production-cap budgets, including an admitted row with a nearly full
short-token prefix and a final long token. Unicode/grapheme and normalized CRLF
boundaries match complete construction when the budget fits; insufficient budgets
return no partial run table. The existing complete-layout fallback remains.

This bounds the rejected metadata attempt. Accepted initial rows still require
complete original run boundaries, and over-limit/unsupported complete preparation
and repeated-prefix fallback work remain on the roadmap. The retained cap itself,
readiness deadlines, exact geometry tolerances and ownership checks are unchanged.


Verification passes all 441 browser checks: 439 ordinary checks (including the
18 WASM unit cases), the font matrix (15.36 seconds) and the near-limit plain-text
contract (13.10 seconds). Both-mode load/scroll/edit geometry and ownership
contracts remain unchanged. The 114 native frontend tests, strict WASM frontend
lint, formatting and Trunk/PWA release build pass. The UI-only run-budget tests
execute in WASM; the native frontend test set remains 114. Linux CI for this
metadata checkpoint `37795367204` passed all five CI jobs, including Linux browser and release checks. Repeated responsiveness and physical-device gates remain open.


### Retained paragraph replay allocations and boundary validation

Warm prefix and exact-offset suffix replay now share each retained immutable
rectangle allocation directly after the existing measurement validation. Previously,
replay allocated a temporary rectangle array and then replaced it with the retained
one. Target, overlap, finite-dimension, source and retention-budget checks remain
unchanged; target and next-overlap collections still allocate.

Prefix replay checks each newly extended run-boundary interval once. Earlier
intervals remain proven because the original and replacement run tables are
immutable during the plan. Regression coverage changes a later boundary both by
inserting a boundary and by moving one without changing the boundary count: each
case stops replay at the first affected probe, after preserving two valid records.
Existing fresh-layout equality, exact suffix reconnection and failed-proof tests
remain required. This does not implement shifted suffix reuse or improve cold
startup; no new latency or memory percentile is claimed.

Verification passes 452 all-feature core tests and 114 native frontend tests,
strict core/all-feature and WASM frontend lint, formatting and the Trunk/PWA
release build. All 444 browser checks pass: 442 ordinary checks (18 unit,
420 component and four integration), the font matrix (15.77 seconds) and the
near-limit plain-text contract (13.22 seconds). The component suite took 140.92
seconds. Both-mode complete-renderer and fallback comparisons remain unchanged.
These local checks do not establish Linux PSS or physical input readiness; CI
for paragraph replay checkpoint `37797735516` passed all five jobs.


### Probe-scoped sparse anchor lookup

Paragraph target construction and validated record commitment previously filtered
the entire sparse line-anchor iterator for every probe. The shared visual index
now binary-searches the probe's glyph interval and iterates only the matching
checkpoints, preserving the complete iterator's terminal glyph and duplicate
semantics. Geometry sampling, overlap proofs, original paint boundaries and
failure fallback are unchanged. Initial plan admission still counts complete
anchors; this change addresses repeated per-probe scans.

A core contract compares range lookup with the complete anchor iterator filtered
independently, including empty/single-glyph text, checkpoint edges, combining
clusters, Unicode, the admitted maximum line size, empty/reversed ranges and
bounds beyond EOF. Both-mode complete-renderer browser contracts remain the
integration gate. No startup, paint or memory improvement is quantified here.


Verification passes 453 all-feature core tests, 114 native frontend tests, strict
core/all-feature and WASM frontend lint, formatting and Trunk/PWA release build.
All 444 browser checks pass: 442 ordinary checks (component suite 141.37 seconds),
the font matrix (15.26 seconds) and the near-limit plain-text contract (13.35
seconds). Both-mode load/scroll/edit geometry, failed-proof and stale ownership
assertions are unchanged. Both Linux CI runs for this checkpoint, `37799023743`
and `37799025459`, passed all five jobs. The broader responsiveness/device gates
remain separate requirements.


### Parser spans from resolved worker source replacements

The shared syntax service now passes a validated replacement span to the same
parser preparation implementation used by direct preparation. It uses the span
only when the parser's cached source is the exact immutable allocation retained
by the advertised worker publication. The resolver constructs the new source
from that base before preparation; a separate equal-content base cannot authorize
the shortcut. Full requests, changed languages/caches and mismatched bases retain
complete source comparison. Cancellation, request limits, missing-base resync,
line-table updates, injection parsing and publication validation are unchanged.

This removes duplicate unchanged-content comparison and changed-span discovery
inside eligible worker parser updates. Source reconstruction, initial/changed
admission scans and reply/publication source comparisons still traverse source;
this is not a complete source-ownership or memory result. Tests compare broad
(nonminimal) worker replacements with fresh language analysis under LF/CRLF,
including folds, token rows and structural data. A parser-level contract checks
that exact retained bases use the supplied broad span while equal-content distinct
allocations use the independently discovered minimal span, preserving old snapshots.


Verification passes 455 all-feature core tests, 114 native frontend tests,
strict core/all-feature and WASM frontend lint, formatting and Trunk/PWA release
build. All 444 browser checks pass: 442 ordinary checks (component suite 142.15
seconds), the font matrix (15.69 seconds) and the near-limit plain-text contract
(13.67 seconds). Existing worker cancellation/resync, both-mode complete geometry
and fallback assertions remain intact. These correctness checks do not quantify
parser scan savings or complete the remaining performance/device gates. CI
`37800600841` subsequently passed all five jobs for that syntax-span checkpoint.


### Bounded startup beyond long initial selections

The shared input facade now permits its existing initial native window for every
eligible unwrapped uniform projection. The initial caret and first source row
no longer have to intersect a long row. Short tabbed/Unicode rows retain measured
complete row geometry; unsupported long tabbed/bidirectional rows, wrapping and
nonuniform projections still use the complete-native fallback. The same pending
geometry, source ownership, scroll guard and composition-aware failure handling
remain in place.

The expanded runtime contract checks a restored long-row caret, a short initial
caret before a later long row and 4,000 short Unicode/combining/tabbed CRLF rows
in both modes, including a fractional CSS line height to exercise DOM quantization. Native value installation stays within 12 KiB through preparation.
Long-row anchors/extents retain the complete-renderer comparison. The short-row
case compares complete native dimensions with measured source dimensions; its
rows fit the viewport, so the complete input's minimum width already includes
trailing padding. The initial test incorrectly added trailing padding to that
minimum a second time; the corrected comparison independently asserts that the
complete short rows fit. The expanded four-case startup contract passes in 1.51 seconds.
No admission-boundary percentile, wrapped startup or physical-input claim follows
from this observation. Full-suite verification remains required.


Broad regression verification found that installing a native window early removed
cold whole-file native dimensions. A source-owned vertical extent now uses actual
fixed unwrapped row boxes, with no complete horizontal-width claim before glyph
measurement. Uniform unwrapped source/measurement rows have fixed line-height
boxes; the adapter checks support, reads the DOM's quantized row height and CSS
padding, and the shared facade validates row counts, finite bounds and current
source ownership. Wrapped/nonuniform rows retain their existing layout path.
The scroll adapter admits vertical and horizontal dimensions independently, and
current source-surface scrolling can preserve user movement during preparation;
clamped native scroll echoes remain guarded. A partial vertical extent preserves
the saved horizontal position until complete glyph widths are known. Scrolling
outside the installed window into a cold, unpaintable viewport restores complete
native input; active composition retains its mapping.

The original viewport and paused-preparation contracts pass (0.64 and 0.99
seconds), as do source/failure ownership and prepared-input contracts (0.10 and
0.47 seconds). All 456 all-feature core and 114 native frontend tests pass, with
strict core and WASM frontend lint and formatting. Full browser-suite and
release/PWA verification remain pending for this checkpoint.


The broad startup regression run passed 420 component checks but failed five
(144.22 seconds): fold-scroll and edit-scrollbar horizontal extents, cold
unwrapped horizontal extents, repeated wrapped probe count, and Explorer/Changes
row spacing. These failures keep the checkpoint unshipped. Diagnose each in a
fresh browser before attributing later failures to leaked test state. The
subsequent plain-fallback change also requires refreshed verification.


### Parser-only colors and configuration grammars — development checks

Removed the heuristic code tokenizer, including its use for gaps in prepared
parser paint. Cooperative fallback retains immutable plain rows and exact source
boundaries without comment-state guesses. The shared parser classifies grammar
terminals, semantic roles and opaque contexts; embedded parsers own their source
ranges. Markdown prose receives no code colors, while code spans, markers, links
and declared fenced-code grammars supply prepared styles.

The registry now includes JSON/JSONC, YAML/YML, TOML, INI/EditorConfig, XML build
configuration and Markdown block/inline grammars. Common manifest/settings aliases
include csproj/props/targets, NuGet.Config, pom.xml, setup.cfg, npmrc and env files.
LICENSE/NOTICE/COPYING stay plain. Config selectors depend only on the immediate parent; opaque grammar contexts
own complete strings/comments, including Markdown code spans. This avoids a
whole-ancestor walk per token and permits existing subtree color reuse. Incremental
semantic assembly and broader nested injection performance remain open.

All 460 all-feature core tests and strict core lint pass. Migrated color regressions
exercise actual parser results instead of the removed tokenizer. New fixture
contracts check LF/CRLF, folds, source preservation, incremental/fresh equivalence
and transfer validation; Markdown independently checks plain prose and Rust fence
keyword/function/number colors. The shared configuration/prose browser contract passes in Chromium in both modes
(0.50 seconds), and strict WASM frontend lint passes. The parser-disabled core
build passes all 375 tests with plain diff fallback. Repeated current-source contracts pass: configuration/prose 0.52 seconds, plain
fallback/ownership 1.05 seconds, embedded HTML 0.09 seconds, font/whitespace matrix
15.77 seconds and near-limit plain geometry 13.71 seconds. All 114 native frontend
tests, formatting and the Trunk release/PWA hook pass. The complete browser suite
and startup extent regressions remain open; these targeted results do not establish
whole-goal completion or reliably passing Linux CI.


A later language-policy pass preserves XML's markup quote/comment behavior,
uses INI line comments and keeps Markdown inline scopes from receiving code quote
pairs. New marker/pair regressions bring the all-feature core suite to 462 passing
tests. The independent Explorer/Changes density failure is fixed by reusing the
same responsive `compact-tree` class; its both-mode browser contract passes in
0.02 seconds. These follow-on edits require refreshed full-browser and release
verification; the four startup/layout failures remain under investigation.


The latest native runs pass 463 all-feature core tests, 378 parser-disabled core
tests and 114 frontend tests. Strict core and WASM frontend lint pass. A broader
browser run before the follow-on fixes passed 416 component contracts and failed
10; several failures involved obsolete fallback-color expectations, YAML root
fold boundaries and startup tests assuming complete horizontal measurements in
the initial partial-height frame. These are being checked independently; no full
browser or current Linux CI pass is claimed yet. Pending horizontal wheel input
now records a scoped user request above the browser adapter, preserving it against
clamped width echoes until complete measured extents become available. Source,
view/account, finite-value and extent-limit rejection remain required.


### Parser-only/configuration colors and eligible unwrapped startup checkpoint

Final local verification on October 8 passes all 430 ordinary component
contracts (142.40 seconds), all 18 WASM unit tests and four other WASM integration
contracts. The separate font/feature/whitespace matrix passes in 15.79 seconds,
and the near-limit plain geometry matrix passes in 17.24 seconds with the original
0.25-pixel glyph comparison and 30-second readiness budget. Both workspace modes
remain covered. The current branch also passes 464 all-feature core tests,
114 native frontend tests, strict core/frontend WASM lint, formatting and diff
checks. The release/PWA build passed for the production changes in this checkpoint.

Focused fresh-browser contracts pass pending horizontal requests (0.75 seconds),
cold source scrolling/pointers/edits (1.24), restored selection/scroll (0.32),
wrapped/unwrapped wheel modes and stale ownership (0.97), repeated wrapped layout
(0.96), localized wrapped reuse (0.50), fold-scroll retention (0.75), scrollbar
layout, configuration/prose, worker cache/transfer ownership, plain terminal
fallback and palette focus. Earlier layout/counter/focus failures no longer occur
in the full passing run after correcting the initial width contracts.

Eligible uniform rows prove height before width; user wheel requests survive
clamped width echoes and apply when measured width is available. Prepared-view
restoration and scrollbar tests require measured extents before setting native
positions. Complete physical width is compared with an independent full DOM
source paint using the production CSS: Chromium's overflowing textarea omits its
16-pixel right padding from scrollWidth. This preserves the editor's end padding
rather than loosening the oracle. Complete native height and exact glyph/source
comparisons remain independent. No wrapped/bidirectional startup, physical IME,
Linux memory percentile or whole-goal completion claim follows from these passes.
Linux/runtime and physical-input evidence remain separate roadmap gates.

### Parser-only colors: production worker follow-up (2026-10-08)

CI exposed an overlapping TypeScript primitive-type span: its named type wrapper
and anonymous `number` terminal both selected the same bytes. The shared provider
now gives the wrapper sole ownership and excludes anonymous type terminals from
string-context classification. Regression fixtures cover TypeScript/TSX, C#,
Java, C/C++ and Go, including fresh/incremental equivalence; the numeric
TypeScript fixture also participates in both-mode browser contracts.

The rebuilt production worker passes 15 code-language providers, six config/prose
languages, one unsupported/plain fallback, six nested incremental/fresh metadata
comparisons, Unicode/CRLF source deltas, compact row/structural patches, eviction
resynchronization and oversize fallback. All 465 core tests and strict core lint
pass. The first ordinary UI run found file-tab vertical overflow and a later
focus failure. Including the classic scroll track and the shared one-pixel bottom
border in the strip's height fixes the overflow; the subsequent full run passes
all 430 ordinary components, 18 WASM unit tests and the adapter checks. The
font/feature/whitespace matrix passes in 16.29 seconds with the unchanged exact
geometry oracle. Focused tests cover current-file progress while preparation is
paused and its removal after terminal plain fallback in both workspace modes.

The final release/PWA build succeeds. One direct worker launch reported a generic
module-load event before preparation; the same assets then loaded in an
instrumented worker and passed two fresh direct checks with all protocol
assertions retained. This does not close the reliable-CI or runtime completion
gates; worker load failures now report an explicit error rather than a null value.

The separate near-limit geometry matrix passes in 13.84 seconds. A subsequent
status-line placement check passes in both modes (0.72 seconds): the preparation
spinner uses a fixed final slot, and the Output button retains its exact position
when preparation finishes.

Linux CI for `6db638b` passes the backend/native, Windows bridge, Docker/HTTPS
and production frontend checks, but its ordinary UI run exposes a touch-capability
file-tab overflow (`scroll=61`, `client=36`). Local Chrome touch emulation
reproduces the same 61-pixel overflow. Touch/phone dropdown sizing must exclude
visually hidden context-menu triggers, and phone tab rows must include their
border around 44-pixel controls. The regression now checks those hidden triggers
and actual vertical overflow in both layouts and workspace modes.

The touch-emulated regression passes after the exclusions and phone row-border
correction (0.07 seconds), including hidden-trigger height, actual scroll extents,
stable tab nodes and both workspace modes.

### Large Markdown inline regions (2026-10-08)

Markdown paragraphs retain independent included-range trees but reuse one parser
per embedded language. Inline prose no longer consumes the 64 code-body slots;
source size, visited-node, progress and published-record bounds remain enforced.
This avoids allocating a parser for each paragraph without joining unrelated
paragraphs or carrying an unfinished inline construct into the next paragraph.

The native regression prepares 1,000 Unicode paragraphs with inline code and
emphasis, independent unmatched backticks and a Rust fence, using LF and CRLF.
It compares complete transferred structure, folds and paint against fresh parses
before and after a middle-paragraph edit, checks two retained embedded parsers,
and verifies cancellation releases the pool and permits clean recovery.
All 467 all-feature core tests passed; strict core linting passed. The release
worker verifier also passed its enlarged 1,000-paragraph CRLF fixture, including
incremental source reconstruction and reused paint rows, together with its
existing grammar, eviction, size and structural-delta contracts.

This checkpoint still reparses each embedded body on a source revision. Skipping
unchanged inline trees, larger fenced-code workloads and warm metadata assembly
remain editor-goal work; this is not a general bounded-latency completion claim.

The no-worker frontend path retains its existing 12 ms synchronous parse budget;
large documents may publish plain terminal fallback there. Large-file UI grammar
contracts therefore use the shared worker transport with the real Rust syntax
service, while the release verifier above exercises the actual module worker.
The highlighted-click contract also uses that transport for its 1,000-function
fixture, retaining the original caret/line-end geometry assertions and readiness
deadline rather than depending on the removed naive fallback colors.

The large-Markdown browser publication regression passed in 0.40 s across both
workspace modes, validating inline-code colors on all 1,000 paragraphs before
and after the middle-paragraph edit. Strict WASM frontend linting also passed.
The highlighted-click worker contract passed in 1.14 s with both modes and both
one-function and 1,000-function sources. Final strict core and WASM frontend
checks passed after the added regressions.

### Reusing unchanged embedded syntax trees (2026-10-08)

Unchanged included-range bodies retain their trees and metadata when an edit
leaves their source bytes intact. Prefix trees keep their coordinates; suffix
trees receive the exact source edit only when their range coordinates shift.
Reuse requires matching language and full byte/point range from the new outer
parse. New or intersecting bodies are parsed, and stale/reclassified bodies are
discarded. Both matching and retention observe cancellation.

Native regressions compare complete publications against fresh parses for LF and
CRLF, Unicode, a changed middle paragraph, inserted/deleted paragraphs, insertion
exactly after a paragraph and a fenced-language change. Test-only counts prove
that the 1,000-paragraph edit parses one body, insertion parses one new body and
deletion of an independent paragraph parses none. All 468 core tests passed.

The preceding checkpoint's Linux production-worker CI exposed a separate cold
parse deadline issue: the first 1,000-paragraph Markdown request returned
Cancelled at the unchanged 100 ms worker deadline; the next request completed.
That regression remains intact. This reuse change does not claim to resolve
bounded cold continuation or the remaining CI reliability gate.

Final development verification passed: 468 all-feature core tests, strict core
and WASM frontend linting, 18 WASM unit tests, 435 ordinary browser component
tests (143.27 s), four adapter integration checks and the rebuilt production
worker verifier. The two separate heavy font/boundary geometry matrices were not
rerun for this parser-only change; their assertions remain in CI. The local
worker passed the cold 1,000-paragraph request, which does not erase the Linux
CI cold-deadline failure recorded above.

### Cooperative parser continuation (2026-10-08)

The Rust syntax service now retains source-bound outer and embedded parser
progress across work batches. Expiry of the unchanged 100 ms worker batch asks
for a yield, not cancellation; the module-worker adapter schedules the next task
and resumes the same included range. The synchronous message adapter drives that
same engine without yielding. Explicit cancellation, replacement source, total
parser progress, source/node/record limits and wire validation remain separate
from the time-slice decision. Partial syntax is never published.

The core message queue bounds pending/active request count and source bytes.
Admission also checks the resolved size of source deltas before making them
active. A task retains its exact immutable source allocation; resumed calls avoid
repeating line admission for that allocation. Parser reset/destruction happens
before unfinished source buffers are released.

Native regressions force many slices, compare complete fresh and incremental
publications across both message adapters, prove completed paragraphs parse only
once, interrupt/resume a fenced Rust body, cancel that body and recover with a
new source, verify supersession releases source allocations, retain the total
progress limit across yields, and check count/byte limits, oversize and resync
responses. All 476 all-feature core tests and strict core linting passed.
The isolated browser suite passed 18 WASM unit tests, 434 ordinary component
tests (145.51 s), four adapter integration checks and doctests. Strict WASM
frontend linting passed. The two heavy geometry matrices retain their CI gates;
this parser change does not alter layout measurement.

The release worker verifier keeps its original cold 1,000-paragraph Markdown
fixture and all readiness/source/reuse assertions. An additional module-worker
probe advances a test-only clock and instruments real task scheduling, without
changing the production deadline. It passed 1,063 forced yields and compared
Unicode/CRLF source, token colors, structure and folds across cold, incremental
and fresh parses. The normal release worker also passed its grammar, eviction,
source-delta, structural-delta and size-fallback contracts.

Selection traversal, retained-region matching and final metadata/paint assembly
still complete synchronously within their source/node/record admission limits;
they are not proved to meet a 100 ms wall-clock bound. Bounded selection and
publication, cold native input/layout, physical-device checks and full CI
reliability remain in the editor goal. The preceding e0174b7 checkpoint achieved
one complete green CI run; this change still needs its own CI evidence.

### Resumable embedded-language selection (2026-10-08)

The shared syntax engine now retains injection selection's next descendant index,
selected ranges and cumulative node/code-body counters. It reconstructs a cursor
on the same immutable tree at each batch and checks cancellation/yield every 256
nodes. No borrowed node/cursor or partial syntax publication crosses a batch.
The synchronous and worker message adapters use this same traversal.

Native regressions compare resumable selection with an independent complete tree
walk for 1,000 Unicode/CRLF Markdown paragraphs and nested HTML script/style
bodies. They require exact node counts and ranges, at most 256 visited nodes per
forced batch, cancellation without advancing state and a node budget that does
not reset across batches. The complete core suite passed 477 tests.

Retained-body matching and final metadata/paint assembly remain synchronous;
this change does not prove a 100 ms wall-clock bound for the complete preparation
pipeline. Cold layout, process memory, physical device checks and reliable CI
remain completion gates.

### Resumable retained-body matching (2026-10-08)

Retained embedded syntax trees are now matched one body at a time in the shared
preparation loop, rather than in a synchronous bulk pass. The pending task owns
unmatched bodies, changed bodies and the exact-range reuse map; the same batch
and cancellation checks precede each next body. After matching completes, parsing
uses the unchanged source/range/provider rules and publishes only complete analysis.

The native regression drives a middle-paragraph edit through more than 100
matching batches for both LF and CRLF Unicode Markdown. It requires bounded
per-batch body progress, exactly one affected paragraph parse, fresh publication
equality, cancellation while retained bodies remain, source-buffer release and
correct recovery. All 478 core tests and strict core/WASM frontend linting
passed. The rebuilt release worker passed its normal grammar/delta/fallback
checks and 1,189 forced scheduler yields with exact source, structure, folds and
paint equality. Metadata/paint assembly and each individual Tree-sitter edit
remain synchronous; a total preparation wall-clock bound is still unproved.

### Ordered embedded-scope checks (2026-10-08)

Fallback protected ranges and opaque-start points now locate their candidate
embedded scope with binary search over the existing source-ordered scope table.
Injection selection already validates ordering and nonoverlap, so range ends are
monotonic, including adjacent and empty bodies. The original overlap and point
containment predicates still decide exclusion. This removes repeated complete
scope scans without changing context semantics or allocating a second index.

The native oracle compares indexed decisions with the previous complete scan for
empty tables, duplicate empty scopes, touching ranges, contained/enclosing/empty
queries and a 1,000-body table. All 479 core tests and strict core/WASM frontend linting passed. The completed
release bundle passed the normal grammar/delta/fallback verifier and 1,189 forced
yields with exact source, context, fold and paint publication equality. The first
check launched before bundle publication used the prior bundle and is excluded
from this evidence. List construction, lexical fallback scans and final metadata
publication are still synchronous and unbounded by a time slice.

Checkpoint b1dc2b4 completed CI run 37845943338 with all five jobs green, including
the browser suite and release worker. Reliability remains a completion gate for
subsequent editor changes; the b930042 and b16d7b4 runs are still in progress.

### Retained embedded fallback contexts (2026-10-08)

Unchanged embedded bodies now retain relative fallback protected ranges and
opaque-start positions alongside their validated syntax trees. Prefix bodies
keep the allocation directly; shifted suffix bodies map it from their new byte
start when assembling the source-bound result. Intersecting/replacement bodies
clear the cache before parsing and scan their own source again. The cache stores
only context metadata, with no additional source snapshot or unused bracket table.
The outer fallback scan and final list assembly still run on every changed source.

A 1,000-paragraph Unicode Markdown regression covers both LF and CRLF. A middle
paragraph edit allocates one new fallback cache, prefix insertion allocates only
its new body, and prefix deletion allocates none. Each revision must match fresh
source, structure, paint and fold publication exactly. Existing synchronous and
yielding message contracts exercise the same core preparation implementation.
All 480 core tests, strict core/WASM frontend linting and the release build passed.
The completed release worker passed its normal grammar/delta/fallback checks and
1,189 forced scheduler yields with exact fresh/incremental publication equality.
The combined 43444fa scheduled-task/assistance base passed 481 core tests and
strict core linting with this change applied.

Full CI completed successfully for b1dc2b4, b930042, b16d7b4 and e39c8b7. These
checkpoints provide repeated baseline evidence; the changed implementation still
needs its own full CI run, and completion requires the remaining performance,
geometry, device/input, recovery and accessibility work.

### Resumable lexical fallback scanning (2026-10-08)

The complete scanner and syntax worker now use one retained lexical state machine.
It carries comment depth, raw-string hash prefixes/closing matches, quote/escape
state, regex classes, template holes and bracket links across batches. Worker
preparation scans outer and newly parsed embedded source in 8 KiB batches before
installing source-bound trees/metadata. A completed embedded parse is retained
while its fallback scan yields; unchanged bodies retain their prior contexts.
Explicit cancellation discards incomplete scanning with the pending source.

The regex expression-position check uses the retained last non-whitespace offset
and bounded keyword-suffix checks, avoiding repeated prefix scans. Normal UTF-8
characters, escapes and fixed delimiters are atomic; a batch can overrun by at
most four bytes. YAML scalar lookahead still processes a complete region and does
not satisfy that byte bound. Final context, paint, fold and transfer assembly are
still synchronous; no total 100 ms wall-clock guarantee is claimed.

Before extraction, 864 synthetic cases captured the original scanner's exact
protected ranges, opaque starts and bracket links across 24 languages, LF/CRLF,
malformed/truncated input and nested constructs. Each golden case is checked at
1/2/3/7/64-byte and complete budgets. Native near-1-MiB Unicode Rust and fenced
Markdown literals require many fallback batches, retained completed trees, exact
fresh publication, cancellation/source release and recovery. Separate contracts
retain source/bracket caps across yields and forbid incomplete publication.

These are scanner/worker contracts. Main editor load/scroll/input, exact geometry,
Linux process memory, physical IME/touch, permission recovery and accessibility
remain full-goal completion gates.

Validation passed: 484 all-feature core tests, 383 no-default-feature core tests,
strict core and WASM frontend linting, and the finalized release bundle. The real
production worker check compares fresh/cooperative publication and paints a
1,043,013-byte Unicode source with a String span exceeding 1,000,000 bytes. It
reports 1,538 forced paragraph yields and 14 large-literal yields; those are worker
scheduling evidence, not a main-thread input or PSS measurement.

The preceding 477fe58 checkpoint passed four CI jobs but failed the browser child
approval fixture. Its automatic task-name request consumed the scripted child
file-edit completion. The fixture now supplies naming separately, retaining the
manual approval, inherited-file and durable nested-history assertions.
The corrected ordinary Chrome suite passed: 18 WASM unit tests, 444 component
contracts and four adapter integrations. The two unchanged heavy paragraph
geometry matrices remain separate CI gates. Full CI for this checkpoint is still
required; prior checkpoint green runs do not substitute for that result.

### Resumable YAML scalars (2026-10-08)

YAML header validation and scalar bodies now retain lexical/row state across the
same 8 KiB fallback batches. They track ASCII indentation and Unicode whitespace
without whole-row trimming or prefix/newline searches. A rejected header or a
nonblank dedented row replays through the ordinary shared scanner, restoring its
row context. No extra source snapshot or text copy is retained. Budget accounting
includes bytes revisited during replay and zero-byte state transitions; every
batch, including YAML, has at most four bytes of atomic delimiter/UTF-8 overrun.
Final metadata and paint assembly remain synchronous.

Before replacing YAML lookahead, 192 additional fixed cases captured its exact
ranges across indentation, indicators, comments, invalid/no-newline headers,
LF/CRLF, blank rows and Unicode whitespace. The existing 864-case scanner oracle
and these YAML cases match at 1/2/3/7/64-byte and complete budgets. Near-1-MiB
headers, scalars, whitespace rows and rejected-header replay remain bounded.
Native standalone/fenced YAML additionally exercises scanner yields, exact fresh
publication, cancellation/source release and recovery in the same syntax engine.

Validation passed: 485 all-feature and 384 no-default-feature core tests, strict
core/WASM frontend linting and the finalized release build. The production worker
check paints a String span exceeding 1,000,000 bytes in a 1,043,026-byte YAML source,
requires 14 forced yields, and matches source/highlight/structure/fold publication
against fresh preparation. The retained Rust literal still passes its styling and
14-yield gate. The full check reports 1,552 cooperative yields. These are worker
contracts; main-editor load/scroll/input, PSS and the other full-goal gates remain.

### Parsed bracket traversal (2026-10-08)

Final parsed bracket linking now traverses ordered opaque-region and language-body
boundaries directly. It skips an entire known literal or an unbracketed language
span instead of decoding every character and querying protected ranges. Each
embedded-scope boundary still clears bracket ancestry, including when an opaque
region crosses it. Scope validation matches wire reconstruction before jumping
through source offsets. The bracket cap and pairing rules are unchanged; no
source snapshot or copy is added.

The original lexical oracle checks final bracket results across all admitted
captured cases. A near-1-MiB Unicode literal crossing an embedded scope examines
only its two exposed parentheses and preserves unmatched ancestry. Near-1-MiB
Markdown prose examines only an embedded two-character Rust body. Separate
contracts cover adjacent bodies, unbracketed bodies, empty scopes, invalid UTF-8
boundaries, overlap/out-of-source ranges and the exact bracket cap.

All five CI jobs completed successfully for e061d68 (run 37856267663), including
the child-task fixture correction and the two separate heavy geometry matrices.
The subsequent YAML checkpoint's CI remains active. These checkpoints do not
complete the remaining editor performance, storage, device/input, recovery and
accessibility work. Final metadata/paint/fold assembly remains synchronous.

Validation passed: all 488 core tests, strict core and WASM frontend linting, and
the finalized release build. The actual production worker retains exact
fresh/incremental source, highlights, structure and folds; both near-1-MiB Rust
and YAML sources contain String spans exceeding 1,000,000 bytes and satisfy their
14-yield gates. The complete check still reports 1,552 cooperative yields.
No main-editor latency or process-memory completion claim follows from this
traversal/worker evidence. Full CI for this checkpoint remains required.

### Retained exact caret anchors (2026-10-08)

Home/End navigation now asks the shared editor facade for a retained exact caret
boundary before constructing a complete movement neighborhood. Sparse anchors
never interpolate missing glyphs. Source/account/project, view, font, pending-edit
and layout ownership remain required; wrapped/bidirectional or missing geometry
retains the complete-renderer fallback. The DOM adapter only translates the
row-relative measured rectangle into viewport coordinates. Other sparse movement
positions still need bounded exact geometry; this is not a general navigation
performance completion claim.

The benchmark now distinguishes native-window start from verified document
beginning. Beginning/end cases navigate with trusted Chrome input and require
source-owned persisted selections plus the complete saved source before and after
insertion, rather than checking only a native input fragment.

The following single pair of instrumented Linux runs uses a 1,048,567-byte styled
Rust String, 4 CPUs, 10 GiB and the existing arm64 measurement image. Both bundles
were built from base 3714b44: before module f4db4ce92e67c02d, after module
1c363e9f2efa8035 with this draft. The same base backend artifact was pinned during
measurement to avoid replacement by concurrent development builds. Both runs
retain String styling through load, scrolling and beginning insertion and verify
complete source recovery. Local uses a database recovery fixture with no native
directory handle; these runs do not prove folder permissions or filesystem writes.

| Mode / version | Cold paint ms | Scroll paint ms | Beginning input paint ms | Peak Chrome PSS KiB | Largest task ms | Full navigation probes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Local before | 4079.3 | 21.4 | 2647.6 | 812530 | 8489 | 2 |
| Local after | 3857.9 | 19.6 | 2556.2 | 760204 | 101 | 0 |
| Remote before | 4057.8 | 25.4 | 2694.7 | 800223 | 8109 | 2 |
| Remote after | 3856.7 | 17.5 | 2550.4 | 797945 | 109 | 0 |

[Before trace](editor-performance/retained-caret-before-linux-trace.jsonl) and
[after trace](editor-performance/retained-caret-after-linux-trace.jsonl) are
untruncated. Before navigation shaped two complete 748,983-UTF-16-unit rows per
mode; after navigation created no measurement probes. These instrumented samples
are not a latency distribution or a general memory guarantee. Cold load and
beginning-edit repaint remain multi-second and fail the full responsiveness gate.

Validation passed: 489 all-feature and 385 no-default-feature core tests; the full
ordinary Chrome suite (18 unit, 445 component and four adapter tests), separate
font/feature/whitespace geometry matrix, source-ownership caret regression, strict
core lint and the release build. Endpoint pixel checks compare retained geometry
against independent collapsed browser ranges across all five Monaspace families.
The release worker check reports 1,552 cooperative yields and exact publication
for near-1-MiB Rust and YAML String spans. The separate near-limit geometry matrix and final strict WASM lint also pass.
Explicit account invalidation passes alongside seven other scope changes in both
modes. This checkpoint's CI remains required.

All five CI jobs passed for e061d68 (37856267663), 47c5cbb (37857374089) and
3714b44 (37858160328). Those results do not replace this draft's CI or the full
editor completion gates.

### Current painted caret coverage (2026-10-08)

Caret reveal now first maps the source byte into source-owned UTF-16 paint via
`EditorActions`, then measures a collapsed browser range within actual painted
coverage. Missing coverage, stale account/file/source/style, a mismatched DOM
projection revision or an invalid rectangle preserves the existing retained-anchor
and complete-renderer fallbacks. Whole rendered rows receive the same paint scope
as windowed fragments. Retained paint now explicitly records the wrap preference,
so a wrap toggle rejects old geometry before deferred layout refresh.

The styled Rust navigation regression drives Home and 30 Right/Left moves through
combining characters and emoji in both modes. A synchronous mutation audit sees
no hidden row-probe allocations; selections return to byte zero and complete
source stays unchanged. The harness supplies actual Rust syntax-service replies
through its deferred transport, since its page does not host the release worker.
Independent release-worker validation reports the same 1,552 cooperative yields,
exact fresh/incremental publication and near-1-MiB Rust/YAML String styling.
Trusted release Chromium input also passes commit/cancel, undo/redo and source
preservation for LF/CRLF in both modes. These are engine checks, not physical IME,
touch, installed PWA or local directory-permission evidence.

[Repeated Linux measurements](editor-performance/painted-caret-linux-repeat.jsonl)
use the finalized c24fc4781b9b96d2 bundle, based on f1cf98b plus this draft, the
same pinned 3714b44 backend, existing arm64 image and 4-CPU/10-GiB container budget.
All six runs use the actual 1,048,567-byte styled Rust String, require verified
byte-zero selection before insertion and verify the complete saved document
through load, scrolling and insertion. Three samples per mode are descriptive,
not a percentile or a general memory guarantee. Component verification was also
running on the host; the container budget does not imply exclusive host resources.
Local recovery fixtures have no native directory handles.

| Mode | Cold paint median (range), ms | Scroll paint median (range), ms | Beginning input median (range), ms | Peak Chrome PSS range, KiB | Largest task range, ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Local | 3942.4 (3904.1–3994.9) | 18.3 (17.1–21.7) | 2644.3 (2614.4–2686.2) | 760987–792826 | 100–109 |
| Remote | 4151.3 (3712.2–4608.2) | 28.5 (26.2–30.1) | 2761.9 (2663.5–2893.5) | 761344–807202 | 115–122 |

Cold/start-of-document repaint and process memory remain completion work. The
previous trace shows beginning repaint still preparing 72 paragraph probes;
shifted suffix reuse must preserve original paint-run boundaries and exact new
glyph targets, not simply translate widths or assume unchanged source offsets.
Validation passed: 115 native frontend tests, strict WASM frontend lint, the full
Chrome suite (18 unit, 446 component and four adapter tests), the separate five-family
font/feature/whitespace matrix and near-limit geometry matrix, format checks and
the finalized release build. This checkpoint's CI remains required.


### Shifted styled paragraph replay (2026-10-08)

Unchanged styled suffix probes now map exact source bytes and grapheme positions
across insertions/deletions. Original paint-run endpoints supply bounded anchors;
replay checks source slices, normalized run boundaries and every incoming browser
overlap rectangle before translating retained rectangles. Replay yields after a
bounded batch. Translated interior probes publish no scroll extent: the terminal
probe freshly measures actual browser rounding. Shifted tabs and failed proofs
use fresh probes; plain/changed long-token boundaries remain implementation work.

Pending plain preparation retains the last styled candidate without permitting
stale geometry publication. Matching trusted font notifications preserve the
measured font identity across source edits, while account/project/read/font and
CSS changes still reject stale candidates. The browser regression exercises ASCII,
emoji and combining-prefix insertion plus deletion in both modes, comparing every
published anchor, endpoint caret and complete extent to the full browser renderer.

[Repeated Linux samples](editor-performance/shifted-paragraph-linux-repeat.jsonl)
use release module 1fe4ba3678588de8, based on 40cab32 plus this draft, the existing
arm64 measurement image and 4-CPU/10-GiB container budget. The earlier pinned
backend artifact was removed by build cleanup; these samples use a newly built
40cab32/draft backend pinned at SHA-256
`0f0129f66166019510c4c4ac8ea68e90852faad3b67a35c83528b9b51e462799`.
The host also ran component checks, so resources were not exclusive. Each of the
six samples retains actual String styling in the 1,048,567-byte Rust source through
load, scrolling and beginning insertion, verifying byte-zero selection and the
complete saved document. Local fixtures contain no native directory handle and
do not establish folder permissions. Three samples per mode are descriptive,
not a latency distribution or a general memory guarantee.

| Mode | Cold paint median (range), ms | Scroll paint median (range), ms | Beginning input median (range), ms | Peak Chrome PSS range, KiB | Largest task range, ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Local | 4014.3 (3669.4–5001.1) | 20.8 (17.7–27.7) | 539.2 (505.5–540.2) | 751452–761699 | 102–104 |
| Remote | 3892.5 (3726.9–4647.8) | 30.2 (17.0–31.6) | 520.1 (509.4–558.4) | 704946–740267 | 118–125 |

The [untruncated Linux trace](editor-performance/shifted-paragraph-linux-trace.jsonl)
records three actual sliced input layout probes per mode, versus 72 in the prior
trace. Fresh input probes shape 11,385 and 4,631 UTF-16 units; the visible paint
probe shapes 386. Cold preparation still shapes 72–80 probes. Both trace runs
verify the complete saved source.

The preceding checkpoint's beginning-input medians were 2644.3/2761.9 ms. These
observations support substantial reduction in repaint work, but cold load,
half-second input and process memory still fail the full responsiveness goal.
Validation so far passes 492 all-feature core tests, 388 no-default-feature core
tests, 115 native frontend tests, strict core/WASM lint and the release build.
The actual production worker retains exact fresh/incremental source, structure
and near-limit Rust/YAML String spans with 1,552 cooperative yields. The full
ordinary Chrome suite passes 18 unit, 447 component and four adapter tests.
Both separate complete-renderer font/feature/whitespace and near-limit geometry
matrices pass. Trusted release input checks pass commit/cancel, undo/redo and
complete LF/CRLF source preservation in both modes. These checks do not establish
physical IME, installed PWA, touch, assistive-technology or native folder permission
behavior. All five CI jobs passed for 2e934df (37866467373).

All five jobs passed for both previous caret checkpoints: f1cf98b (37861220328)
and 40cab32 (37862828415). They do not replace verification of this draft.


### Ordered source coordinate queries (2026-10-08)

Shifted replay now evaluates exact old/new glyph positions in ordered queries,
sharing cluster traversal between dense targets and using retained checkpoints
across sparse gaps. Relative old glyph numbers remain candidates: every old/new
source byte must still match the measured source shift, alongside the original
source/run, font, scope, overlap and fresh-terminal-extent checks. The shared Rust
coordinate API rejects unordered, mismatched-length and over-budget requests;
duplicates and EOF retain exact byte/UTF-16 semantics. Unicode tests compare dense,
sparse and repeated queries to complete cluster coordinates, including emoji,
regional indicators, combining sequences and Indic conjuncts.

The exact 2e934df [baseline](editor-performance/ordered-positions-before-linux-repeat.jsonl)
(module ab421393ed5b99d) and [new query samples](editor-performance/ordered-positions-after-linux-repeat.jsonl)
(module b95be28cdbd84bcd) use the same newly rebuilt 2e934df backend, pinned outside
Cargo cleanup at SHA-256
`e15661c00f9b8d42efb10b5287ff2cc28c2549649534854a36851d84b213b0f1`.
A shared target cleanup removed the previous pin after the baseline frontend build;
measurement resumed only after rebuilding and preserving the backend. No new Docker
image was created. Both versions use the existing arm64 image and 4-CPU/10-GiB
container budget. Concurrent host builds/tests mean resources were not exclusive.

All twelve samples use the actual 1,048,567-byte Rust String, retain String styling
through load/scroll/input, verify byte-zero selection before insertion and verify
the complete saved source afterward. Local fixtures contain no native directory
handles. Three samples per mode/version are descriptive, not percentiles or a
memory/permission guarantee.

| Mode / version | Cold paint median (range), ms | Scroll paint median (range), ms | Beginning input median (range), ms | Peak Chrome PSS range, KiB | Largest task range, ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Local before | 4155.8 (4138.4–4436.5) | 24.2 (23.0–28.0) | 555.0 (544.9–638.7) | 750373–764653 | 103–108 |
| Local after | 3864.0 (3797.8–4274.6) | 20.4 (17.3–23.6) | 143.3 (139.4–162.9) | 715479–759498 | 108–112 |
| Remote before | 4177.0 (4053.9–4190.7) | 27.2 (24.4–27.4) | 555.0 (552.1–556.4) | 721500–763088 | 117–127 |
| Remote after | 3936.3 (3910.0–4125.4) | 25.6 (17.3–27.7) | 147.1 (145.9–147.2) | 716743–759972 | 115–128 |

The [untruncated new trace](editor-performance/ordered-positions-after-linux-trace.jsonl)
still records three sliced input probes per mode; their first-to-last span is
43.8/42.5 ms, versus 453.3/422.6 ms in the preceding shifted-replay trace. This
supports reduction of coordinate-validation work, not less geometry coverage.
Cold preparation still measures 80/72 probes and remains multi-second. Largest
cold tasks and process memory remain completion work; this does not finish the
responsiveness, wrapped/bidirectional, ownership, device, recovery or accessibility
gates.

Validation so far passes 495 all-feature and 391 no-default-feature core tests,
strict core/WASM lint, both complete-browser suffix regressions, the release build
and the actual production worker checks (1,552 cooperative yields with exact
near-limit Rust/YAML String publication). The full ordinary Chrome suite passes
18 unit, 450 component and four adapter tests. Both separate font/feature/whitespace
and near-limit geometry matrices pass, as do trusted release composition,
undo/redo and full LF/CRLF source checks in both modes. These are supporting engine
checks, not physical IME, touch, assistive-technology or native folder-permission
verification. This checkpoint's CI remains required.


### Editor verification follow-ups (2026-10-09 UTC)

Main checkpoint `87fd6cb`, which contains the ordered-coordinate improvements
and accessible native-input labels, passed all five jobs in
[CI run 37872165452](https://github.com/openwebide/openwebide/actions/runs/37872165452).
The preceding `3f36586` run failed its UI suite. Its first failure was the pointer
tooltip contract; later menu/composer focus failures followed. A local release run
reproduced the tooltip and three later failures (454 passed, four failed), whereas
the two tooltip contracts passed in isolation.

The pointer fixture omitted the app stylesheet: its popup participated in page
layout instead of being a fixed overlay. With production styles restored, the
complete ordinary release suite passes 18 unit, 458 component and four adapter
contracts, including every formerly failing case. Assertions and timeouts remain
unchanged. The two dedicated font/limit matrices are separate from this ordinary
suite; their preceding passes remain historical evidence.

The next cold-input checkpoint retains tab presence in each immutable long-row
coordinate index. Admission reuses that fact instead of rescanning the source;
its unsupported-layout rules remain unchanged. All 497 core tests, strict
core/WASM lint and the existing both-mode initial-input admission/failure/ownership
contract pass. This change has no new performance samples and does not complete
wrapped/nonuniform startup or the responsiveness/memory gate.

Paragraph preparation now reuses the same immutable tab fact rather than scanning
the row again. The 13 paragraph unit contracts and strict core/WASM lint pass.
The ordinary release suite passes 18 unit, 458 component and four adapter tests,
including tabbed horizontal paint, complete extents, native hits, failed proofs
and stale ownership in both modes. An initial isolated debug run failed the cold-
anchor audit; baseline debug/release and the candidate debug repeat passed, as did
the complete candidate release suite. This is not evidence that the timing risk
has been eliminated. The combined command's final documentation stage failed
with missing shared-crate artifacts; a separate documentation check rebuilt its
dependencies and passed (zero documentation examples). No new
startup, input or memory performance samples accompany this change.
Both dedicated release matrices also pass from the same retained artifact:
font/features/whitespace geometry and near-limit complete-row geometry, each
covering local and remote adapters.

Styled paragraph anchor construction now uses ordered exact byte-boundary queries,
sharing grapheme traversal between nearby runs and resuming sparse gaps from the
immutable coordinate checkpoints. Interior grapheme bytes remain excluded, query
budgets remain bounded, and over-budget run tables retain the sparse path. The
Unicode contract compares against complete segmentation, including every byte,
duplicate/terminal boundaries, sparse queries, oversized clusters and rejected
inputs. All 337 editor core tests and strict core/WASM lint pass. Six release
browser paragraph contracts pass: shifted suffixes, changed prefixes/stale scopes,
failed overlaps, complete rounding, font/features/whitespace and near-limit rows.
Both-mode geometry coverage remains intact. This is an algorithmic change without
new timing or memory samples; cold/wrapped/unsupported layout gates stay open.

The bounded styled-run cache now retains unavailable-table results for its exact
source/style scope. Repeated warm queries skip the capped attempt; complete
paragraph construction still runs when metadata cannot be retained. The new
both-mode facade contract distinguishes these paths by segmented source bytes,
checks lossless complete-plan availability and rejects another project's result.
A browser unit contract covers stale account/file/read/epoch scopes, changed view
and styles, the eight-row retention bound and retry after eviction. Both pass,
as does the existing near-limit styled horizontal-paint contract in both modes.
Strict WASM lint passes. Uncapped construction still segments full rows, and no
new timing/memory samples or completion-gate claims accompany this increment.

Grammar-free SQL preparation now retains its plain-row job across cooperative
worker tasks. Both drivers use the same scanner; yielded jobs publish no partial
analysis, retain their original completion status and permit synchronous completion.
The 500 parser-enabled and 394 minimal core tests pass, including synchronous/
worker wire parity, LF/CRLF row equality, unchanged-row reuse, tab-width reuse,
mid-job cancellation and source replacement. Both-mode Chromium contracts pass
for SQL publication with LF/CRLF and stale file/project/account publication guards.
Strict core and WASM lint pass. Whole oversized rows and final fold/record/message
assembly remain synchronous; this has no new timing or memory samples and does
not complete the bounded-publication or cold-layout gates.

The accessible-input checkpoint also passes eight release-app cases: both modes,
LF/CRLF and pending-worker/bounded-native input. Chromium exposes the file-specific
input name and current keyboard description; trusted Ctrl+M, Tab and Shift+Tab
move focus in both directions without changing text or selection. Commit/cancel,
undo/redo and recovered source remain lossless. These disposable local drafts have
no native directory handles. Physical input devices, installed PWAs, screen
readers, touch, folder permissions and the remaining editor gates stay open.

### Bounded plain-row scanning and copy (2026-10-09)

The shared plain-row job now retains unfinished boundary scanning, raw-row reuse
validation and UTF-8 text copying between byte-budgeted advances. Only complete
rows enter its token table; cancelled jobs cannot publish a partial snapshot.
Retained indexed rows keep their token allocations, and unchanged rows inside
disjoint edits validate in bounded chunks before reusing their old tokens.
The worker and browser fallback call this same job. The initial browser batch
allows a small file's scan plus copy; subsequent batches use the ordinary budget.
The synchronous whole-row API can still finish an already-started cooperative row.

All 504 parser-enabled and 397 minimal core tests pass, including Unicode/CRLF,
tiny budgets, disjoint-edit reuse, mixed drivers and cancellation inside a long
SQL row. Strict core/WASM lint passes. Chromium contracts pass in both modes for
long-row SQL worker publication and terminal/ordinary browser fallback, including
stale scope rejection. Whole-row capacity allocation, source-change comparison,
final context/token publication and serialization remain; these results do not
establish the overall responsiveness, memory, tabbed/wrapped layout or device gates.

The ordinary optimized browser run passes 19 unit tests, 460 component contracts
and four adapter integrations. The two separate heavy font/paragraph-limit
matrices were excluded from that ordinary run; this is not a claim that the full
editor completion gates or all release/device verification are satisfied.

### Cooperative retained plain-source comparison (2026-10-09)

Warm plain-source reuse no longer performs a synchronous whole-document text
comparison before entering the bounded row job. Both syntax-worker and browser
fallback callers retain a prefix/suffix comparison in that shared job, charge
compared bytes to its advance budget and round only completed replacement
boundaries to UTF-8 scalar positions. Identical source allocations still finish
immediately; equal text in a different allocation validates cooperatively and
shares the complete retained row/token tables. Indexed row reuse begins only
after the exact replacement is known. The synchronous API drains the same state.

All 507 parser-enabled and 399 minimal core tests pass. Regressions compare the
result with the existing synchronous text-change algorithm across empty,
insertion/deletion, Unicode, combining and CRLF cases at tiny budgets. Long-source
checks prove unfinished comparison publishes no rows, shares converged tokens,
resets on replacement and cancels in the worker without publishing stale analysis.
Strict core and WASM frontend lint passes. Both optimized Chromium cooperative
contracts pass: cold/warm SQL worker publication with LF/CRLF, and ordinary or
terminal browser fallback reuse with stale source/read/account/project rejection
in both workspace modes. The full browser suite and performance/device gates
are not established by these two contracts.

Parser source-change comparisons outside this plain-row job, whole-row capacity
allocation, final metadata assembly/publication and transport serialization remain.
This change does not establish the full responsiveness, memory, geometry or device
completion gates.

### Cooperative comparison before parser updates (2026-10-09)

Parser-backed cooperative preparation now uses the same resumable exact UTF-8
comparison as retained plain-row preparation. Unresolved warm sources retain the
old tree and immutable base while prefix/suffix validation yields between bounded
advances. Completed replacement boundaries enter the existing shared parser update
path; resolver-provided deltas bypass comparison only for the exact ready base
allocation. Unfinished comparison exposes no new analysis, contexts or folds.
Supersession, cancellation and synchronous updates release its retained progress.

All 510 parser-enabled and 399 minimal core tests pass, as does strict core/WASM
lint. Rust, TypeScript and admitted Markdown fixtures match fresh complete analysis
after resumed LF/CRLF updates. Exact-base delta admission and source release have
regressions. A large repeated-word Markdown paragraph hit the existing parser
cancellation limit before source comparison; it remains outside this improvement's
evidence and the broader Markdown/preparation gate stays open.
All three optimized cooperative Chromium contracts pass in both workspace modes:
cold/warm parser preparation on an approximately 820-KiB LF/CRLF source, SQL
worker preparation and ordinary/terminal plain fallback reuse with stale scopes.

Admission scans, changed row/index assembly, synchronous source-change callers,
final metadata publication and transport materialization remain. These contracts
do not establish the full performance, memory, wrapped/tabbed layout or device
completion gates.

CI run `37889078284` for the preceding checkpoint passed native/backend, frontend
lint/build, Windows bridge and Docker, but its ordinary browser component run
exhausted the 300-second harness deadline while contracts were still running.
The workflow now partitions ordinary tests with complementary `editor::` filters
across every target and retains the two separate expensive matrices. No test or
readiness deadline is removed or relaxed; YAML and extracted shell syntax validate.
Linux CI must verify this partition before reliability is established.
The first complete local optimized run passed 462 component contracts, including
both expensive editor matrices, but its keyboard-tooltip fixture lost its popup
when focus scrolled the growing harness page. The focus-only fixture now stays
in the viewport and prevents native focus scrolling; both tooltip fixtures now
settle stylesheet/layout changes in a shared setup before dispatching events.
Pointer/scroll dismissal assertions and readiness limits remain unchanged.
The non-editor partition passes all 327 component contracts, six unit tests and
four adapter integrations after that fixture correction.
The original run passed all 19 unit tests
but stopped before adapter integrations on that fixture failure.
The complementary editor partition passes 134 component contracts and 13 unit
tests in 92.64 seconds; the non-editor component partition completes in 117.82
seconds. Combined ordinary coverage is 461 component contracts, all 19 unit
tests and four adapter integrations. Both expensive matrices passed in the first
complete run with the same production parser code. These are local results;
the partitioned Linux workflow still needs its own successful run.

### Cooperative parser-free entry and replacement reuse (2026-10-09)

Cooperative preparation now compares warm parser-free sources with the same
resumable UTF-8 job used for parser-backed files. The entry equality check no
longer synchronously traverses an unresolved retained plain source. Its validated
replacement passes into plain-row preparation, where only the exact compatible
retained base may bypass a second comparison. Other base allocations still take
ordinary cooperative comparison; source settings/language mismatches take fresh
preparation. Existing synchronous callers remain synchronous.

All 511 parser-enabled and 399 minimal core tests pass, with strict core and
WASM frontend lint. The fresh-analysis comparison matrix now includes SQL and
plain documents alongside Rust, TypeScript and admitted Markdown, with LF/CRLF.
Exact-base row reuse, equal text in distinct allocations, normalization, incompatible
languages and warm cancellation retain regression coverage. All three optimized
cooperative Chromium contracts pass, including a larger SQL document with a
630-kB first row, warm updates and exact raw-source publication in both modes.

Admission, row/index assembly, paint/transport comparisons, final publication,
geometry, process memory and physical input/device gates remain open. These
results do not establish full editor completion or Linux CI reliability.

### Cooperative syntax admission (2026-10-09)

The worker coordinator and shared preparation engine now use one source-owned
admission scanner. Each advance inspects at most the supplied raw-byte budget;
UTF-8 and CRLF boundaries need no copies. It retains newline counts across task
yields and preserves the existing 2 MiB byte cap and rejection at 50,000 LF bytes.
Borrowed synchronous admission drains the same scanner. Browser admission yields
before request serialization and rechecks the request ticket, source/read scope
and account generation before sending. Core admission hides old contexts/folds
until the admitted source enters the existing shared comparison/parser facade.
Cancellation or supersession discards its owned pending source.

All 514 parser-enabled and 399 minimal core tests pass; strict parser-enabled
core lint passes. Native regressions cover independent declared byte/newline
boundaries, zero budgets, bounded cursor movement, Unicode, LF/CRLF, source
ownership, cancellation and replacement. Later-phase parser/comparison fixtures
explicitly complete admission first so their original progress assertions remain
meaningful. A repeated-word Markdown source still hits the existing parser
progress limit; fresh-analysis equivalence uses the admitted long paragraph
fixture, without removing that larger Markdown gate.

Strict release WASM frontend lint also passes. All five optimized cooperative
Chromium contracts pass in 6.51 seconds in both modes, including source/read/account
invalidation before transport and rejection without sending requests. The UI
capacity boundary remains intact: files beyond the editor byte cap never receive
a syntax key or worker request; admitted files with 50,000 LF bytes receive syntax
TooLarge without transport. The core scanner independently verifies the larger
syntax byte boundary. Changed
row/index assembly, paint/transport comparisons, final publication, exact geometry,
process memory and physical input/device gates remain open.

The preceding parser-comparison and parser-free checkpoints both completed all
five Linux/platform CI jobs successfully: runs `37892302443` and `37893661498`.
Their partitioned UI jobs retain every contract and the existing readiness
deadlines. This proves two complete checkpoint runs; near-limit performance,
release/PWA repeats and physical device gates remain separate requirements.

### Cooperative parser row replacement (2026-10-09)

Changed row replacement now has one borrowed synchronous scanner and a source-owned
cooperative driver. Each advance reads its supplied raw-byte budget and emits at
most 256 rows. Unicode and CRLF may straddle advances; relative row bodies and
trailing empty/suffix rows match the complete line iterator. The parser facade
retains its exact base, source and validated replacement while scanning yields,
rather than repeating prefix/suffix comparison on each continuation. Completed
rows supply the existing parser InputEdit exactly once. Canceled/replaced work or
a synchronous update releases pending rows without publishing partial structure.
Pending parse resumption remains tied to its exact immutable source allocation.

Scanner oracle tests cover zero budgets, empty/interior/suffix rows, long Unicode
rows, CRLF and row-limit boundaries, plus owned-source release. Shared parser
regressions check cold/warm long boundary rows and a 40,000-row comment source,
full index equality before parsing/publication, exact-base deltas, source changes,
cancellation and synchronous takeover. Later parser-specific fixtures explicitly
complete row preparation first while retaining their existing parser assertions.

The wide 40,000-comment source exposes a separate final-highlighting stall:
sampling finds repeated Tree-sitter parent lookup across siblings in built-in
classification and retained-part construction. Its eventual analysis also exceeds
the existing record budget. That fixture must match fresh rejection, while proving
row index construction itself; it does not prove responsive or complete styling
of that dense source. Keep this measured workload and the broader final-paint
performance/record-budget gate open. Suffix shifting, Vec allocation/splicing,
final index/context/paint publication and physical geometry/device gates remain.

All 518 parser-enabled and 399 minimal core tests pass, with strict core lint.
The complete parser run takes 215.98 seconds because the preserved dense-comment
fixture exercises that final-highlighting bottleneck and matches fresh TooLarge
rejection. This duration does not satisfy the responsiveness gate. Optimized
browser contracts all pass in 6.00 seconds in both modes, including a 49,000-row
LF/CRLF source through actual worker messages and complete fresh-analysis equality.
Strict release WASM frontend lint also passes. The preceding admission checkpoint
completed all five CI jobs successfully in run `37897135708`.

### Cursor-provided syntax parents (2026-10-09)

Syntax traversal now carries the immediate parent alongside each node, using
owned traversal ancestry rather than repeated Node.parent searches from the tree
root. Color selectors accept that node/parent pair; built-in code, configuration
and Markdown colors retain their existing type-wrapper and parent-field rules.
Custom document-dependent selectors retain their declared cache policy and may
ignore the parent. Retained context/fold/color parts use the same supplied parent
kind for construction and candidate validation. Visit charging and source/tree
identity rules remain unchanged. External ancestry needed by interpolation owners
and parent-owned fold headers still uses its existing guarded path.

All 519 parser-enabled core tests pass in 4.47 seconds, compared with the preceding
215.98-second run. The same 40,000-comment LF/CRLF workload, full fresh-analysis
comparison and record-budget rejection remain in the suite. A first run of the
unchanged 518 contracts completed in 7.35 seconds before adding the new oracle.
The added grammar matrix checks actual parent node identity and classification
agreement at every traversed node and retained frontier for all 22 registered
grammars, including embedded Markdown and fenced languages. Existing dependency, field-role, document-dependent and cache invalidation
contracts remain intact. These are unoptimized local native whole-suite timings,
not Linux/PWA rendering, memory or full responsiveness proof.

All 399 minimal core tests and strict core/WASM frontend lint also pass. All six
optimized cooperative Chromium contracts pass in 6.53 seconds in both workspace
modes, including dense admitted rows and warm parser/plain-source replacement.
Dense
source record budgets, final metadata/paint assembly, actual styled near-limit
rendering, geometry, memory and physical device gates remain open.


### Cooperative parser index publication (2026-10-09)

Changed-row scanning now feeds a retained publication job. It copies the old
prefix, translates replacement coordinates and shifts retained suffix coordinates
in batches of at most 256 rows, observing cancellation/yield between batches.
The old index stays intact until the entire replacement is ready; publication
swaps the completed Vec once before constructing the parser InputEdit. Work is
bound to the exact old/new source allocations. Cancellation, source supersession
and synchronous takeover discard partial publication. Both workspace adapters
use the same syntax-worker/facade implementation. Coordinate translation policy
is shared with synchronous LineEdit application, which retains its in-place
primitive to avoid copying unchanged prefixes during ordinary document edits.

This removes an uninterrupted parser suffix-shift/splice pass. It still copies
the complete index over multiple batches and may grow Vec capacity during a
batch. Allocator behavior, synchronous document indexes, final context/paint
assembly and transport serialization remain open performance gates.

Publication regressions compare a complete line-iterator oracle at every Unicode
edit boundary with empty, LF, CRLF and multi-row replacements. Batches include
zero/one-row budgets, large unchanged prefixes and suffixes, and discarded jobs.
Parser regressions cover cancellation, supersession, synchronous takeover and
fresh-analysis equality after completed publication. Existing embedded-body
phase tests explicitly prime index publication before testing their own yield
boundaries, preserving their parser assertions.

All 528 parser-enabled and 405 minimal core contracts pass. Strict core lint
passes. The expanded worker browser fixture retains one worker through a
49,000-row cold load and a beginning-of-file edit, for LF/CRLF in each workspace
mode, with exact fresh-analysis comparison and no partial frontend publication.
All six optimized cooperative Chromium contracts pass in 6.68 seconds.
Strict release WASM component-test lint also passes. These results do not prove
the full editor responsiveness, memory or physical device gates.

CI run `37902004036` finished with native, WASM, Windows and Docker jobs green,
but two local-bridge browser fixtures failed: discovery missed its expected completion count, while compaction had no
room for a summary after tool schemas expanded. Both fixtures still use an
8,192-token model budget. The CI completion gate remains open until fixture corrections and
the complete browser suite are verified.

The two CI fixtures now use a 32,768-token model capacity. Compaction history
increases proportionally from 4,000 to 16,000 repeated entries, so both plain and
tool-enabled cases still exceed capacity and must compact. Discovery adds an
explicit no-error assertion. All 24 local-bridge Chromium contracts pass in
19.03 seconds, retaining discovery fallback/timeout and compaction persistence
assertions. No production model limits or test deadlines changed. Full CI on
the new checkpoint remains to be verified.


### Cooperative worker source reconstruction (2026-10-09)

Validated worker replacement requests now retain an owned reconstruction job
before parsing. Prefix, inserted text and suffix append in at most 64-KiB batches
without splitting UTF-8 scalars. The exact published base allocation stays owned
until completion; no partial String reaches admission, parsing or publication.
Cancellation drops the partial source and the normal worker control reply remains
unchanged. Full snapshots move their existing String allocation without another
copy. The synchronous message driver drains the same job; borrowed SyntaxSource
resolution uses the same reconstruction policy without copying its base.

Total resolved bytes remain charged against the existing worker queue cap before
reconstruction starts, including when only a small insertion was transmitted.
Ranges and UTF-8 boundaries validate against the exact ticketed publication before
allocation. Request/reply schemas and protocol version stay unchanged.

This bounds source copying between worker tasks. Initial String capacity allocation,
JSON decoding/encoding, frontend source-delta validation, paint snapshots and
final metadata publication remain open gates. Budgets smaller than four bytes
can stop before an indivisible UTF-8 scalar; production batches are 64 KiB.

Independent splice-oracle tests cover every Unicode edit boundary, LF/CRLF,
empty replacements, zero and small budgets, owned-base release, invalid ranges
and full-source allocation identity. Shared worker regressions stop during a
large delta before parser entry, verify byte accounting, cancel or complete to
exact fresh analysis, then recover on a full snapshot. A separate large
prefix/insertion/suffix fixture requires over 100 bounded copy batches and
checks every phase against exact splicing. All 532 parser-enabled core contracts
and strict core lint pass. All six optimized cooperative Chromium contracts
pass in 6.97 seconds across both workspace modes, including explicitly
asserted warm replacement requests. Strict release WASM component-test lint
also passes. Index-publication CI `37904746814` and source-reconstruction CI
`37906056594` subsequently completed all five jobs successfully. The wider
physical/performance and full-goal CI gates remain open.


### Cooperative final bracket publication (2026-10-09)

Final grammar contexts now feed an owned bracket-linking continuation before
analysis publication. It retains the immutable source, bracket stack, source
position and ordered scope/region cursors, with at most 256 charged operations
between cancellation/yield checks. Scalar visits, skipped regions/languages and
metadata cursor advances each charge work, so repeated empty boundaries cannot
hide an unbounded inner loop. Opaque bodies still skip directly to their next
body boundary, and bracket ancestry resets between embedded languages. The
existing 65,536-bracket cap and malformed-range fallback remain unchanged.

Synchronous structure queries and message preparation drain the same scanner.
Pending worker structures and folds remain unavailable until complete;
embedded-language queries retain the root-language fallback. Continuations
retain the original cold/incremental status across resumes. Cancellation, source replacement and synchronous takeover discard or
complete the owned work without publishing partial contexts.

Metadata extraction, context-list construction, sorting/validation, semantic paint
assembly and transport serialization still run outside this continuation. This
checkpoint does not establish complete responsiveness or memory behavior.

All 536 parser-enabled and 405 minimal core contracts pass, with strict core
lint after the scheduler correction below. Captured
lexical bracket oracles also run through one/seven/256-operation batches across
languages. Additional tests cover dense empty scope/region cursor budgets, exact
bracket limits, owned-source release, embedded HTML/JavaScript completion,
cancellation, source supersession and synchronous takeover with LF/CRLF.
All seven optimized cooperative Chromium contracts pass in 7.63 seconds
in both workspace modes, with strict release WASM component-test lint. The
index-publication
checkpoint completed all five CI jobs successfully in run `37904746814`,
including the corrected bridge fixtures. Source-reconstruction CI run
`37906056594` also completed all five CI jobs successfully.

The added embedded-language browser fixture exposed starvation with short
slices: nested entry checks could repeatedly yield before delegated parse or
selection work, and parsed-body fallback had another duplicated pre-work check.
Delegated phases now own their entry check; already parsed fallback bodies use
the outer boundary, while newly completed parses retain a post-parse check.
A native worker regression runs the unchanged 100,000-space/4,000-call HTML/JS
source through two-check slices with LF/CRLF and matches the complete synchronous
reply, including all 8,000 linked brackets. Parser/record caps and runtime/test
deadlines remain unchanged. The unchanged browser fixture now passes through
cold and warm worker requests in each workspace mode with LF/CRLF, retaining
exact fresh-analysis comparison and all 8,000 paired-bracket assertions.

### Cooperative final structure metadata (2026-10-09)

The owned final structure job now validates scopes, protected ranges and selection
coordinates, stably orders protected/selection/opaque metadata and deduplicates
selection/opaque lists across bounded batches before bracket linking. Each
metadata operation examines, compares or moves a constant number of records.
Invalid metadata remains rejected after yielding; unfinished structures remain
hidden, and cancellation or source replacement drops their exact source owner.

Already ordered lists need only a bounded linear check. Nearly ordered parser
ranges use adjacent stable moves without scratch tables. This adaptive path has
an eight-operations-per-record limit, then uses an indexed stable merge fallback
with bounded initialization, merge, permutation and record moves. Scratch capacity
allocation remains outside the record budget. Earlier context collection and
reconciliation/sorting, final paint/fold assembly and transport still need bounded
work; this checkpoint does not complete those requirements.

All 558 parser-enabled and 422 minimal core tests pass (4.36/4.96 seconds),
including the unchanged short-slice completion limit. Independent tests compare
stable sort/dedup results and lexical bracket oracles through one/seven/256-unit
batches with LF/CRLF. Other tests cover reversed order, equal-key stability,
constant comparisons per step, nearly ordered enclosing ranges without scratch
tables, persistent invalid-range rejection and source release. Strict core and release WASM component-test lint
pass. All seven optimized Chromium cooperative contracts pass in 8.52 seconds
through both workspace modes, including cold/warm embedded scopes, cancellation,
stale results, worker transport and browser fallback.

A refreshed production baseline, before this metadata change, admits a truly
styled 1,048,567-byte string and verifies complete-source beginning edits in both
modes. [Raw Linux traces](editor-performance/structure-metadata-baseline-linux.jsonl)
record one fresh browser/runtime per mode under the existing four-CPU/10-GB
container limits. Local/remote load-to-paint was 7,447/6,535 ms, horizontal
scroll-to-paint 28/23 ms and beginning-edit-to-paint 162/342 ms. Peak Chrome PSS
was 714,384/694,216 KiB, with 350/200-ms maximum frames. Complete source and
String styling were preserved; these timing/memory results still fail the goal's
responsiveness gate. These are instrumented single samples, not repeated release
proof or evidence that final metadata ordering fixes the long-row shaping cost.

### DOM rectangle batching experiment (2026-10-09; rejected)

A thin DOM binding batched the existing Range queries while Rust retained source,
glyph and overlap policy. Exact scalar/batched rectangle bits matched across
all Monaspace families, ligature settings, Unicode, nested spans, fractional
positions, reordered targets and failure cases. The optimized browser oracle
passed in 0.10 seconds, and release WASM component-test lint passed.

The production experiment did not demonstrate a reliable performance or memory
benefit. Both bundles used editor checkpoint `47c26d8`, the same admitted styled
1,048,567-byte string, four-CPU/10-GB Linux container limits and two fresh
browser/runtime samples per workspace mode. All eight completed samples preserved
String styling and verified complete-source beginning edits. Local baseline
load-to-paint was 4,971/4,761 ms versus candidate 7,129/7,853 ms; remote baseline
was 4,442/4,361 ms versus candidate 4,787/4,956 ms. Cold geometry work also
remained expensive: local baseline 2,391/2,354 ms versus 2,852/3,590 ms; remote
baseline 2,051/2,047 ms versus 2,246/2,396 ms. Peak PSS stayed around
659–690 MiB, without a consistent reduction across modes. Host compilation
activity was not controlled across these groups, so this does not establish
an intrinsic batching regression. A subsequent quiet repeat produced no valid
editor sample because the shared WASI artifact had been cleaned before Spin
readiness. The binding is not shipped.

[Baseline traces](editor-performance/paragraph-rectangle-batching-baseline-linux.jsonl),
[candidate traces](editor-performance/paragraph-rectangle-batching-candidate-linux.jsonl)
and the [rejected patch](editor-performance/paragraph-rectangle-batching-rejected.patch)
retain the experiment. The next candidate should target native layout work:
intermediate DOM probes currently force global overflow layout, although the
shared plan already accepts extent-free intermediate records and requires a
fresh terminal extent. Exact overlap and complete-renderer proofs must remain.

All five CI jobs passed for checkpoint `47c26d8` in run `37912907218`, including
Docker HTTPS transport and the full frontend browser suite. This remains a
checkpoint result; the goal's near-limit, physical-input and complete release
gates are still outstanding.

### Terminal-only paragraph extents (2026-10-09)

The shared Rust paragraph plan now exposes whether the current live probe needs
a document overflow extent. Intermediate records may omit it, as retained suffix
records already could; the terminal record must supply a fresh extent. The DOM
adapter still measures every requested glyph and validates exact overlap, but
only builds global-coordinate gaps and reads overflow for the terminal probe.
No geometry approximations, admission caps or fallback limits changed. Both
workspace modes use this plan and adapter.

Production bundles from the same `a89117e` checkout were measured with and without
this change, using the same frozen WASI backend, Linux Chromium 154 image, four
CPU quota and 10 GiB container ceiling. Each cohort has two fresh samples per
mode of the genuinely String-styled 1,048,567-byte row, with a verified beginning
edit and process-tree PSS. Freezing the runtime prevents build cleanup from
invalidating a sample. [Baseline traces](editor-performance/terminal-paragraph-extents-baseline-linux.jsonl)
and [candidate traces](editor-performance/terminal-paragraph-extents-candidate-linux.jsonl)
retain all measurements; their checkout header identifies the mounted root repo,
while the frontend bundle comes from the isolated `a89117e` worktree.

| Mode/sample | Baseline load / cold geometry (ms) | Candidate load / cold geometry (ms) | Baseline / candidate input (ms) | Baseline / candidate peak PSS (KiB) |
| --- | --- | --- | --- | --- |
| local/1 | 4148 / 1979 | 4004 / 1182 | 140 / 136 | 675591 / 687753 |
| local/2 | 5416 / 2086 | 3221 / 1094 | 137 / 144 | 686211 / 693189 |
| remote/1 | 4021 / 1906 | 3241 / 1090 | 129 / 136 | 711274 / 707380 |
| remote/2 | 4034 / 1892 | 3402 / 1181 | 138 / 140 | 693090 / 700752 |

Cold geometry work decreased about 40–45% in these samples. Input and peak memory
show no consistent improvement, and the remaining startup cost still takes
seconds. These small instrumented cohorts do not clear the full responsiveness,
memory or physical-input release gates.

Validation: 560 parser-enabled and 424 minimal core tests passed, including
missing terminal extents, exact completed geometry, invalid dimensions and
altered overlap. Strict core and optimized WASM component-test lint passed.
Optimized Chromium near-limit and full font/feature/whitespace contracts passed
in both modes. The production bundle also built successfully.
The final optimized Chromium regression run passed all six paragraph contracts
(30.42 s), covering final global overflow rounding, near-limit complete-row
geometry, altered-overlap fallback, changed-prefix/stale-scope rejection,
unchanged suffix reuse and the font/feature/whitespace matrix in both modes.

### Cooperative parsed-region reconciliation (2026-10-09)

The final structure job now retains parsed/lexical reconciliation before metadata
validation and bracket linking. It builds and stably orders parsed coverage,
merges recognized regions, filters lexical fallbacks by ordered lookup, groups
and stably orders interpolation holes, emits their protected pieces and opaque
boundaries, orders the combined list and merges overlaps across batches. Unclaimed
hole-owner allocations are also released one owner per step. Ordered lookups remain
logarithmic, and vector/hash capacity growth remains runtime allocation work.

Synchronous callers drain this same Rust job; worker and browser fallback callers
use its existing 256-unit slices. Partial reconciliation cannot publish a structure.
The previous stable overlap/closed-marker semantics and source allocation remain
unchanged. No analysis limit, parser budget or fallback deadline was raised.

An independent copy of the previous synchronous algorithm checks reversed coverage,
overlapping and equal-key interpolation holes, lexical precedence, unclaimed owners,
closed markers and exact final bracket/opaque/selection tables at budgets 1, 7 and
256 with LF/CRLF and Unicode. An unfinished job cannot publish and releases its
source. All 562 parser-enabled core tests passed (4.20 s), and strict all-target
core lint passed.

Context extraction from retained parser subtrees, fallback metadata collection,
initial/scratch allocation, fold/color/paint assembly and serialization remain
separate unbounded work. This checkpoint alone does not prove complete large-file
responsiveness, process-memory behavior or the physical-input release gates.
Validation also passed 424 parser-free core tests (3.63 s), six optimized
cooperative browser contracts (4.89 s) in both modes, the mixed heredoc/nested
interpolation editing and paint contract (0.35 s) in both modes, and strict
optimized WASM component-test lint. All five CI jobs passed for prior checkpoint
`53b9092` in run `37918744325`; the complete release gates remain open.

### Cooperative retained fallback collection (2026-10-09)

The shared structure preparation job now retains the outer fallback metadata and
up to 64 embedded metadata allocations. It copies and filters outer protected
regions and opaque positions, then shifts embedded records into document coordinates
one record per budget unit before parsed-region reconciliation. Ordered scope
queries use the existing monotonic-end lookup, now shared with the collection job.
Synchronous callers drain the same job; worker and browser fallback use its existing
256-unit batches. Source metadata remains immutable and incomplete collection
cannot publish contexts. No limits or timeouts changed.

The independent collection oracle compares full-vector filtering/extension against
budgets 1, 7 and 256, adjacent embedded scopes, LF/CRLF, Unicode and alternating closed
markers. Each advance emits no more records than its budget. Cancellation releases
both source and retained metadata allocations. All 564 parser-enabled core tests
passed (4.23 s); strict core all-target lint passed. The large-literal scan test now
recognizes post-parse structure progress while retaining its scan-byte, cancellation
and complete-analysis assertions. Browser adapter verification passed all six optimized cooperative contracts
(5.06 s) and the mixed heredoc/nested-interpolation editing and paint contract
(0.34 s), each in both modes. All 424 parser-free core tests passed (3.87 s),
and strict optimized WASM component-test lint passed.

Parser context extraction, cache-missing fallback scans, vector/scratch allocation,
fold/color/paint assembly, transport materialization and the full responsiveness,
memory and physical PWA gates remain. This checkpoint does not clear them.

All five CI jobs passed for checkpoint `2d3637c` in run `37920845353`, repeating
the green native/WASI/WASM/platform/browser/Docker checks after `53b9092`. This
remains checkpoint evidence; the goal's complete release and PWA gates are open.
The roadmap now keeps shipped preparation details in its implemented baseline
and performance evidence, and lists only unfinished work under implementation.

### Cooperative worker reply-source publication (2026-10-09)

The shared source-publication policy now advances the existing resumable UTF-8
comparison before selecting the exact minimal source delta or full source, then
copies the selected body across byte-budgeted tasks. Synchronous publication
drains that same policy; the syntax worker retains the completed analysis and
its exact previous publication through 8-KiB comparison/copy batches. The wire
shape, no-op delta behavior and 96-byte envelope threshold are unchanged.
No parser admission limit, record cap, progress budget or timeout was raised.

The worker cannot install a publication base until the source body finishes and
the complete reply passes existing shaping checks. Cancellation during this phase
releases the retained analysis/source and emits no partial data. An independent
copy of the prior source policy checks full/no-op/changed/deleted sources,
multibyte UTF-8 and combining characters with LF/CRLF at budgets 4, 7 and 8192.
The worker cancellation test interrupts a truly near-limit literal publication,
checks source release and missing partial bases, then verifies full recovery.
All 567 parser-enabled core tests passed (4.21 s), and strict core all-target lint
passed. All six optimized cooperative browser contracts passed (5.05 s) in
both modes; 424 parser-free core tests passed (3.91 s), and strict optimized
WASM component-test lint passed.

Record-count traversal, structure/token metadata materialization, paint-source
comparisons, JSON encoding/decoding, message-size fallback publication and runtime
capacity growth remain unbounded. Browser request construction still drains the
shared source policy synchronously. This change does not establish the full
responsiveness, memory, geometry or physical PWA release gates.

## Cooperative cold-row glyph measurement

Cold height probes retain the complete styled row, but sampled glyph geometry now
advances through the shared `RowGeometryPreparation` in batches of at most 128
exact DOM range reads. The browser yields between batches, checks the immutable
source/account/project/layout/font scope before continuing, and publishes only a
complete table accepted by the existing horizontal or wrapped geometry validator.
Synchronous viewport queries drain the same policy without introducing a second
geometry contract. Invalid or superseded jobs discard their partial table.

This does not bound complete-row HTML installation, initial browser shaping,
DOM-node enumeration, source comparisons or synchronous viewport queries. It does
not enable bidirectional slicing or remove the complete-layout fallback. The
existing anchor/rectangle limits are unchanged. Geometry timing around this async
pass includes time spent yielding; it is wall time, not isolated CPU measurement.
This checkpoint makes no startup, input-latency or memory improvement claim.

Verification includes a direct complete-renderer rectangle comparison and actual
browser-yield cancellation for wrapped and unwrapped styled/tabbed Unicode text,
using loaded Monaspace Neon/Argon fonts and enabled/disabled healing and ligatures,
plus the existing both-mode cold preparation and wrapped fragment regressions.

Validation for this checkpoint: 569 parser-enabled and 426 minimal core tests;
strict native core and optimized WASM component-test Clippy; the new optimized
Chrome contract (0.82 s), cold superseded preparation (3.68 s), wrapped fragment
scroll/Find and bidi fallback (1.52 s), and six paragraph geometry regressions
(33.27 s). Browser contracts exercise both workspace modes. These fixture timings
are regression observations, not production performance measurements.

## Bounded wrapped paragraph preparation (2026-10-09)

Eligible source-monotonic wrapped paragraphs now prepare across at most 16-KiB
styled probes. Continuations begin after complete words at grapheme boundaries,
clip retained original paint runs, and preserve the measured horizontal origin.
Each dense overlap must match actual glyph rectangles within the existing 0.25-px
geometry tolerance; vertical translation comes from the same overlap glyph.
Only completed validated geometry publishes through the shared editor facade.
Source/account/project/font/layout cancellation discards unfinished tables.
Bidirectional source, oversized unbroken words and failed proofs retain complete
layout. Existing source, node, anchor and paint-run caps remain unchanged.
Initial run-table construction, native input shaping and other roadmap work remain.

The first prototype rejected the unchanged styled fixture because original paint
run ends supplied no useful space seams. Exact retained-run clipping resolved that
restriction; the fixture, tolerance and admission limits were not relaxed.
Browser comparisons cover loaded five-family Monaspace fonts, healing, ligatures,
whitespace and both workspace modes (80 combinations, 27.96 s), every retained
anchor and full extents. Actual source replacement during a wrapped probe publishes
no old geometry and renders the replacement correctly (0.79 s, both modes).

### Paired production Linux observations

Baseline is detached `3ab579e`; candidate is this change built independently from
that head plus the wrapped implementation. Both use the same frozen WASI backend
and Spin manifest, reused image
`sha256:e1a372cac8e6c496251a7ea371631a8beb560d1c5c6cddd1b70a270da19bda8c`,
Linux ARM64 Chromium 154.0.8037.92, four CPUs, 10-GiB memory and 1-GiB shared memory.
No builds ran during measurements. Two fresh samples per mode used
`styled-long-line --wrap --input-position beginning --repeat 2 --trace --require-pss`.
Actual source is 1,048,567 bytes with verified String styling, caret zero before
input, complete source after input and the destination fragment after scrolling.
Baseline module is `openwebide-frontend-5d701a5ce1b456e1.js`; candidate module is
`openwebide-frontend-8bef8f071f0c7940.js`. The harness checkout-head field is identical
because candidate edits were still uncommitted when measured; these module hashes
and separate production build directories distinguish the bundles.

| Bundle | Mode/sample | Load ms | Input ms | Scroll ms | Max frame ms | Peak Chrome PSS KiB |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| baseline | local 1 | 24764.9 | 4575.5 | 41.6 | 14016 | 4320642 |
| baseline | local 2 | 23527.2 | 4485.7 | 44.6 | 13099.5 | 2831816 |
| baseline | remote 1 | 23086.8 | 4624.1 | 35.4 | 12849.6 | 2920372 |
| baseline | remote 2 | 23968.0 | 4697.5 | 37.6 | 13166.2 | 2860049 |
| candidate | local 1 | 17542.8 | 1352.3 | 41.5 | 14749.4 | 2849647 |
| candidate | local 2 | 17127.6 | 1544.8 | 45.5 | 13666.2 | 2812789 |
| candidate | remote 1 | 15995.5 | 1427.3 | 50.6 | 13249.5 | 3031482 |
| candidate | remote 2 | 16187.3 | 1312.3 | 50.3 | 13332.8 | 2855416 |

Both bundles retain the same 547,721-px complete scroll height and 264-px width.
All four candidate traces contain 146 paragraph render/layout/geometry batches,
with a maximum 16,382 source bytes per probe and no trace truncation, rather
than complete-row cold DOM shaping. Load and beginning edit wall time improve in
these samples, but maximum frames remain about 13–15 seconds and PSS remains large.
This does **not** satisfy responsiveness or memory completion gates. Initial full
native source shaping is still a severe startup cost; small timing differences and
PSS variation across four fresh runs are not evidence of a memory improvement.

Final regression checks: all 572 parser-enabled and 429 minimal core tests pass;
strict native core all-target Clippy passes. Optimized browser checks cover cold
superseded preparation (3.68 s), wrapped fragment scroll/Find and bidi fallback
(1.68 s), and six existing paragraph geometry/fallback contracts (31.85 s).
The wrapped fragment contract now requires bounded cold probes instead of the
previous complete-row implementation; its source, extent and caret assertions
remain in place. Browser contracts exercise both workspace modes.
The independent DOM-clone markup oracle and strict optimized WASM component-test
Clippy also pass. These checks verify the checkpoint, not the remaining full goal.

## Cold native input for long tabbed and bidirectional rows

The shared editor input facade now admits bounded surrounding text for every
uniform unwrapped projection. Native input needs exact source coordinates and
selection ownership; it does not require the paint slicing policy to admit the
row. Fixed unwrapped row boxes prove vertical extent independently of tabs and
bidirectional shaping. Horizontal extent remains pending until the existing
bounded or complete-layout measurement finishes. This also removes the initial
whole-document scan for long-row paint eligibility from input admission.

The complete-layout fallback is unchanged. Current measurement failure restores
complete native input, while composition retains its installed mapping through
commit/cancel and stale source/account/project results cannot release another
context. Wrapped/nonuniform initial native input and source-owned touch input
remain unresolved. This checkpoint makes no near-limit production responsiveness
or PSS improvement claim: unsupported paragraph shaping still runs in full.

The browser startup contract audits every production textarea value assignment
before readiness and requires at most 12,288 UTF-16 units; the ownership contract
also checks the 12-KiB source-byte cap. It preserves restored selection/scroll
and exact full extents. It adds long tabbed and mixed bidirectional Unicode rows
to both workspace modes. Overflowing Chromium textareas omit right padding from
their reported width: the first new oracle observed source width 580,337 px,
native width 580,321 px and complete source-paint width 580,337 px. Width is
therefore compared against the existing independent complete source-paint oracle;
height remains checked against a complete native control. No tolerance is raised.

Verification: the expanded optimized Chrome startup contract passes (2.47 s),
scrolled cold native input/source edits and pointer mapping pass (1.30 s), and
long tabbed horizontal fragments/native hits pass (1.34 s). The 32-case ownership
matrix passes (0.12 s), including Unicode edits before measurement, tabbed
composition commit and bidirectional composition cancel after measurement failure,
plus stale account/file rejection. Strict optimized WASM component-test Clippy,
formatting and whitespace checks pass. These are regression timings, not controlled
production latency measurements or physical input-method verification.

## Bounded native startup for wrapped and nonuniform rows

The shared editor input facade admits bounded desktop surrounding text before
wrapped or nonuniform layout completes. The shared core measurement plan exposes
at most 128 exactly measured origin rows, stopping at the first missing height or
width. The DOM adapter publishes that prefix once it covers the viewport (or the
batch/document ends); the shared facade validates the preparation ticket, source,
syntax revision, account, project, read, font, layout and whitespace preference.

Early origin paint does not publish complete document dimensions or pointer-ready
geometry. Restored distant scroll waits for the complete table. Wrapped fragment
paint can use already measured row geometry, while complete measurements and
native context completion retain the existing publication gate. Failure restores
complete native input; active composition keeps its installed mapping until
commit/cancel. Touch still uses complete native input.

Regression coverage audits all textarea assignments for a 1,048,567-byte styled
wrapped source in both workspace modes, with a 12-KiB source-byte bound and exact
complete-renderer geometry. This test uses the real Rust syntax service through
controlled test transport; it does not establish production worker latency. The
cold-origin test requires visible measured paint before complete geometry, rejects
partial document extents and verifies a source edit cancels the old ticket. The
ownership matrix adds wrapped composition commit/cancel and lone-carriage-return
rows; preparation provenance also rejects syntax and whitespace changes.

The complete source-paint oracle normalizes lone carriage returns as line breaks,
matching native input and the existing projection contract. Short nonuniform rows
compare width against the larger of complete paint and the scroll viewport; a
nonoverflowing file cannot have a scroll width narrower than its viewport. Height
still uses an independent complete native control. Tolerances and admission limits
are unchanged.

No new controlled production latency or memory improvement is claimed by this
checkpoint. The near-limit responsiveness/PSS, unsupported shaping and physical
Chrome/Edge PWA input and touch checks remain on the roadmap.

Verification: 573 parser-enabled core tests and 430 minimal core tests pass,
alongside strict native core all-target and optimized WASM component-test Clippy.
Twelve optimized Chrome contracts pass in both modes, covering the near-limit
styled startup audit, restored unwrapped geometry, preparation provenance,
cold origin/edit cancellation, failure/composition ownership, wrapped styled
geometry and chunk cancellation, fragment scroll/Find, native pointer/edit
mapping, wrapped row windows, repeated-row sharing and localized row reuse.
Formatting and whitespace checks pass. Exact-head CI is checked separately.

### Repeated production check of bounded wrapped startup

Fresh paired samples compare the earlier wrapped bundle
`openwebide-frontend-8bef8f071f0c7940.js` with pushed `8890d1d` bundle
`openwebide-frontend-b536ded1cf465ba0.js`. Both use the same frozen backend,
Spin manifest, Linux ARM64 Chromium 154.0.8037.92 and existing image
`sha256:e1a372cac8e6c496251a7ea371631a8beb560d1c5c6cddd1b70a270da19bda8c`,
with four CPUs, 10-GiB memory and 1-GiB shared memory. No builds ran during
measurements. Each sample has a fresh account/runtime/browser and a 1,048,567-byte
String source, exact source caret zero before beginning input, complete source
verified afterward and prepared String styling through scroll and input.
The checkout header identifies the mounted root at `8890d1d`; module hashes
identify the independently mounted frontend bundles.

[Baseline traces](editor-performance/wrapped-cold-native-baseline-linux.jsonl) and
[candidate traces](editor-performance/wrapped-cold-native-candidate-linux.jsonl)
retain every sample and all probe, worker, font and native events without truncation.

| Bundle | Mode/sample | Load ms | Input ms | Scroll ms | Cold max frame ms | Overall max frame ms | Peak Chrome PSS KiB |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline | local 1 | 18994.7 | 1656.4 | 47.2 | 16082.6 | 16082.6 | 2996265 |
| baseline | local 2 | 17634.5 | 1645.4 | 55.0 | 14882.7 | 14882.7 | 2804888 |
| baseline | remote 1 | 18500.4 | 1688.9 | 44.6 | 15249.3 | 15249.3 | 2932197 |
| baseline | remote 2 | 17720.0 | 1722.3 | 53.4 | 14832.8 | 14832.8 | 2822613 |
| candidate | local 1 | 2721.9 | 1463.5 | 64.2 | 183.4 | 4833.1 | 1084287 |
| candidate | local 2 | 2652.1 | 1408.6 | 37.9 | 150.0 | 4566.4 | 1104233 |
| candidate | remote 1 | 2788.4 | 1370.3 | 38.8 | 183.3 | 4699.8 | 1107400 |
| candidate | remote 2 | 2795.0 | 1404.1 | 48.4 | 183.3 | 4549.9 | 1107348 |

Both bundles retain 547,721-px complete scroll height and 264-px width. The
candidate installs an 8,782-unit native window at readiness. Startup and peak
PSS materially improve in these repeated samples, but completion is still unproven:
load and input remain above a second, and the overall frame maximum includes a
remaining 4.5–4.8-second navigation stall. The trace attributes that stall to an
unscoped, unsliced 748,983-unit movement probe when Ctrl+Home reveals the omitted
wrapped origin. It is real application navigation, not a geometry oracle injected
by this harness. This evidence makes retained wrapped caret navigation the next
implementation target; it does not close the responsiveness or memory gates.

### Exact retained wrapped caret navigation

The shared core geometry now exposes exact wrapped endpoints and adjacent
same-row boundaries. The existing EditorActions facade validates the current
source, metrics and geometry orientation before reusing those coordinates in
both modes. Sparse gaps and soft-wrap affinity retain browser fallback. The thin
DOM adapter aligns current source paint with the actual scroll position before
reading coordinates: queued Home/End commands can otherwise observe the previous
scroll transform before its scroll event arrives. The regression checks both
source selection and actual endpoint visibility, including consecutive Home/End.

The production candidate `openwebide-frontend-7e2b607e36e267e5.js` uses the same
frozen backend, image, resource limits and quiet measurement conditions as the
preceding comparison. The mounted checkout remains `8890d1d` with this pending
checkpoint; the independently mounted candidate bundle contains the caret fix.
Each beginning/end sample verifies exact source caret zero/1,048,567 and complete
source after input, with real String styling and unchanged complete dimensions.

[Beginning traces](editor-performance/wrapped-retained-caret-beginning-linux.jsonl)
and [end traces](editor-performance/wrapped-retained-caret-end-linux.jsonl) retain
all four samples each without truncation.

| Input position | Mode/sample | Load ms | Input ms | Scroll ms | Overall max frame ms | Peak Chrome PSS KiB |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| beginning | local 1 | 2732.8 | 1223.2 | 36.9 | 200.1 | 701667 |
| beginning | local 2 | 2772.5 | 1332.3 | 43.2 | 166.7 | 727389 |
| beginning | remote 1 | 2709.7 | 1291.4 | 51.7 | 183.4 | 724273 |
| beginning | remote 2 | 2667.3 | 1277.5 | 42.5 | 183.3 | 711157 |
| end | local 1 | 2685.7 | 1231.6 | 45.8 | 166.6 | 730673 |
| end | local 2 | 2769.8 | 1239.7 | 35.2 | 166.7 | 713708 |
| end | remote 1 | 2833.9 | 1216.1 | 41.1 | 183.3 | 709929 |
| end | remote 2 | 2722.4 | 1249.1 | 43.9 | 183.4 | 736614 |

The previous 748,983-unit navigation probe is absent. Beginning navigation needs
no measurement probe; end navigation uses only a bounded 979-unit probe. Across
all samples the largest probe is 11,706 units. Overall frame maxima fall from
4.5–4.8 seconds in the preceding beginning samples to 166.6–200.1 ms here, and
peak Chrome PSS falls from 1,084,287–1,107,400 to 701,667–736,614 KiB (see raw
samples for exact values). These observations close this navigation regression,
not the full responsiveness/memory gate: startup still takes 2.7–2.8 seconds and
input 1.2–1.3 seconds. Unsupported geometry and physical PWA checks remain open.

Verification: 573 parser-enabled and 430 minimal core tests pass, with strict
native core and optimized WASM component-test Clippy and a release Trunk build.
Optimized Chrome contracts pass for retained source/account/project/font/layout
ownership, painted caret movement and Home/End visibility, independent primary
caret/selection geometry, and wrapped fragment scroll/Find in both modes.
Formatting and whitespace checks pass. Exact-head CI is tracked separately.

### Reusing completed wrapped paragraph probes

Wrapped and unwrapped layout now share the retained ParagraphMeasurements cache
and the EditorActions replay facade. Geometry policies stay in Rust core; the DOM
adapter supplies actual styled rectangles and yields between suffix batches.
Wrapped replay verifies exact source bytes, original clipped paint-run boundaries,
byte/glyph mapping, scoped font/layout provenance and dense incoming overlap.
Original paint-run anchors can survive source shifts. A changed incoming gap can
translate the first visual row only with dense proof across its line break and
reconnection through the normal fresh-probe gate. Overflow-sensitive changed-gap
probes and missing anchors fall back to fresh measurement. The terminal probe is
always fresh, so cached intermediate geometry cannot authorize terminal extents.
The existing quarter-pixel overlap tolerance and 128-K retained-rectangle budget
are unchanged.

The browser regression preserves the complex Unicode/operator workload whose
wrapping phase may not reconnect after a prefix insertion. It also exercises the
actual production String pattern, requiring positive shifted suffix reuse where
its measured phase reconnects. Both workloads compare every retained anchor and
complete dimensions against independently laid-out complete source in both modes.
The existing five-family/healing/ligature/whitespace matrix still passes.

The optimized production candidate `openwebide-frontend-d2e8c2ef5615904a.js` was
measured with the same frozen backend, image, Chromium version and four-CPU/
10-GiB/1-GiB-shm limits as the preceding retained-caret checkpoint. No local builds
ran during these samples. The mounted checkout header is `39c1ba2`; the separate
candidate bundle contains this pending replay checkpoint. The actual source is
1,048,567 bytes with prepared String styling, exact beginning/end source caret,
complete source verified after input and unchanged 547,721-by-264-px dimensions.

[Beginning traces](editor-performance/wrapped-replay-beginning-linux.jsonl) and
[end traces](editor-performance/wrapped-replay-end-linux.jsonl) retain every sample
without truncation.

| Input position | Mode/sample | Load ms | Input ms | Scroll ms | Overall max frame ms | Input probes | Peak Chrome PSS KiB |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| beginning | local 1 | 2855.6 | 1943.7 | 47.9 | 183.3 | 70 | 730008 |
| beginning | local 2 | 3856.1 | 1568.3 | 72.2 | 183.3 | 70 | 696786 |
| beginning | remote 1 | 3292.9 | 1340 | 49.1 | 183.3 | 70 | 731560 |
| beginning | remote 2 | 2788.9 | 1292.7 | 45 | 183.3 | 70 | 731057 |
| end | local 1 | 2760.0 | 123.1 | 39.4 | 216.8 | 2 | 727682 |
| end | local 2 | 2629.3 | 120.8 | 67.3 | 149.9 | 2 | 746818 |
| end | remote 1 | 2758.3 | 147 | 42.2 | 166.7 | 2 | 725203 |
| end | remote 2 | 2842.5 | 119.7 | 44.8 | 183.3 | 2 | 727670 |

End edits now require two fresh input probes and 119.7–147.0 ms, compared with
1,216.1–1,249.1 ms in the preceding end samples. The near-limit beginning insertion
still requires 70 fresh probes and 1,292.7–1,943.7 ms; the smaller browser regression's
reconnecting suffix does not establish near-limit reconnection. The production
trace therefore proves a substantial end-edit improvement, not a general wrapped
editing or startup improvement. Startup remains 2.6–3.9 seconds, and peak Chrome
PSS remains around 0.7 million KiB. Nonreconnecting layout, synchronous prefix
replay and remaining source/run-table work remain responsiveness targets; full
memory and physical-device gates stay open.

Verification: 575 parser-enabled and 432 minimal core tests pass, including
source/style/overlap rejection, changed-origin line-break proof, overflow rejection
and the requirement for a fresh terminal extent. Strict native core and optimized
WASM component-test Clippy, release Trunk build, formatting and whitespace checks
pass. Six optimized Chrome filters cover wrapped edit replay, the five-family
matrix, unwrapped suffix replay with stale scopes, painted caret movement,
chunk cancellation and bounded near-limit native startup in both modes.
Exact-head CI is checked separately; all five jobs and Project site passed for
the preceding `39c1ba2` checkpoint.


### Retained prefix replay batches

Both core paragraph plans now expose bounded prefix replay. The shared editor
facade captures the immutable source/style proof once, while its batch calls
reject a replaced editor scope before resuming. Wrapped and unwrapped DOM
preparation yield between at most eight retained probes, using the existing
measurement batch limit. Terminal probes still measure complete dimensions
freshly; no geometry tolerances, rectangle limits or fallback rules changed.
The synchronous entry point uses the same policy for existing complete-renderer
contracts. Initial proof preparation and individual probe replay remain work;
yielding batches alone does not establish a latency or memory improvement.

Core validation passes 587 parser-enabled tests and strict Clippy, including
zero-budget calls, repeated bounded continuation, exact geometry and fresh
terminal extents. Final optimized WASM component-test Clippy passes. Optimized Chrome contracts pass for wrapped full-renderer geometry, captured-prefix
batch limits and stale-account rejection, unwrapped shifted suffix geometry, and
wrapped chunk cancellation in both modes. The production Trunk bundle also passes.
Quiet production traces use the same existing Linux ARM64 Chromium image,
4 CPUs, 10 GiB memory, 1 GiB shared memory and frozen backend as the preceding
wrapped-replay samples. No local builds or browser checks ran during measurement.
The candidate module is `openwebide-frontend-41a1adbc2012954e.js`. Both positions
have two repetitions per adapter, actual 1,048,567-byte source, exact source caret,
complete source after input, String styling, and complete 547,721 × 264 extents.

Raw traces: [beginning](editor-performance/prefix-batch-beginning-linux.jsonl) and
[end](editor-performance/prefix-batch-end-linux.jsonl).

| Position | Mode / repetition | Load ms | Input ms | Scroll ms | Largest frame ms | Fresh input probes | Peak Chrome PSS KiB |
|---|---|---:|---:|---:|---:|---:|---:|
| beginning | local 1 | 2564.8 | 1197.5 | 42.5 | 166.5 | 70 | 746169 |
| beginning | local 2 | 2409.0 | 1131.1 | 39.9 | 150.0 | 70 | 733485 |
| beginning | remote 1 | 2611.1 | 1206.8 | 40.6 | 150.0 | 70 | 734750 |
| beginning | remote 2 | 2536.6 | 1190.9 | 41.8 | 150.0 | 70 | 756080 |
| end | local 1 | 2587.0 | 117.1 | 42.9 | 216.7 | 2 | 741791 |
| end | local 2 | 2397.9 | 121.4 | 34.9 | 166.7 | 2 | 736379 |
| end | remote 1 | 2489.5 | 125.6 | 36.8 | 166.7 | 2 | 738816 |
| end | remote 2 | 2429.6 | 113.8 | 30.7 | 149.9 | 2 | 725316 |

These samples validate source/geometry preservation and retain the two-fresh-probe
end path. They do not demonstrate a general wrapped-edit latency or memory win:
beginning edits still require full reflow, and startup remains costly. The newer
bundle also includes the intervening plugin integration, so differences from the
preceding traces do not isolate prefix batching. Nonreconnecting geometry,
individual replay cost, initial proof/run-table preparation and physical-device
verification remain open.

### Cooperative styled run preparation checkpoint

Eligible wrapped and unwrapped measurements now prepare original styled run
boundaries through one shared Rust continuation. It retains Unicode segmentation
between batches of 64 token/run operations, preserving adjacent plain-token merges,
CR normalization and the existing 16,384-run/eight-row cache limits. Synchronous
callers drain the same implementation. The editor facade checks source, read,
project, account and layout ownership before and after each yield, rejects disposed
owners and publishes only complete tables or a scoped unavailable result.

Validation passes 589 parser-enabled core tests, strict core and optimized WASM
frontend Clippy, formatting, and the production Trunk/PWA build. Optimized Chrome
checks pass for yielding and stale-owner rejection, unavailable-table caching,
wrapped edits and unwrapped shifted suffixes against complete geometry in both
workspace modes. Additional both-mode contracts pass for bounded styled wrapped
geometry (24.50 s), cancellation (0.58 s), and near-limit bounded native startup
(10.58 s).

No production latency or memory improvement is claimed for this checkpoint.
The quiet Linux beginning/end input traces below compare with the preceding
prefix-batch baseline. A single indivisible grapheme can exceed the ordinary run
size; unsupported and uncapped synchronous construction paths, nonreconnecting
wrapped reflow, startup/memory and physical-device completion gates remain open.

Quiet Linux production samples for this checkpoint use the same frozen backend,
Chromium image, 4 CPUs, 10 GiB memory and 1 GiB shared memory as the preceding
prefix-batch baseline. No local builds or browser checks ran during measurement.
Both positions retain actual 1,048,567-byte String styling, exact boundary carets,
complete source after input and complete 547,721 × 264 extents, without truncated
traces. The candidate module is `openwebide-frontend-3115e25d7ada20e1.js`.
Raw traces: [beginning](editor-performance/paint-runs-beginning-linux.jsonl)
and [end](editor-performance/paint-runs-end-linux.jsonl).

| Position | Mode / repetition | Load ms | Input ms | Scroll ms | Largest frame ms | Fresh input probes | Peak Chrome PSS KiB |
|---|---|---:|---:|---:|---:|---:|---:|
| beginning | local 1 | 2384.5 | 1109.0 | 40.7 | 166.7 | 70 | 726093 |
| beginning | local 2 | 2454.4 | 1187.4 | 45.4 | 150.0 | 70 | 735538 |
| beginning | remote 1 | 2494.5 | 1224.6 | 39.9 | 150.0 | 70 | 725524 |
| beginning | remote 2 | 2377.2 | 1145.5 | 42.4 | 150.0 | 70 | 738874 |
| end | local 1 | 2409.4 | 129.2 | 30.0 | 150.0 | 2 | 729530 |
| end | local 2 | 2362.5 | 117.4 | 33.5 | 150.1 | 2 | 724644 |
| end | remote 1 | 2444.8 | 125.0 | 32.5 | 149.9 | 2 | 715041 |
| end | remote 2 | 2451.2 | 125.8 | 35.8 | 150.0 | 2 | 699650 |

Beginning input remains 1109.0–1224.6 ms, compared with 1131.1–1206.8 ms in
the preceding baseline. End input remains 117.4–129.2 ms, compared with
113.8–125.6 ms. These samples show no consistent latency improvement. Beginning
edits still measure 70 fresh probes; their traced browser bounds calls total
762.8–832.7 ms. Changed long-token run boundaries and nonreconnecting wrapped
reflow remain optimization targets. The PSS samples do not establish a general
memory improvement. The bundle also includes the intervening status ordering and
browser-fixture fixes, so the comparison does not isolate run preparation alone.

Full CI for the browser-fixture/status-ordering checkpoint `3564938` completed
successfully in run `38006010538`, including the complete browser partition.
The subsequent cooperative-run checkpoint `a02d543` also passed all five jobs in
run `38007304962`. These are repeated complete checkpoint results; the remaining
performance, physical input and recovery gates stay open.

### Uncapped unwrapped run preparation

The shared editor facade now uses the same yielding run continuation for the
uncapped unwrapped measurement fallback. Rejected retained tables still permit
complete paragraph preparation, with the same original boundaries, tab policy
and conservative layout fallback. Over-limit tables remain owned by the returned
measurement plan rather than expanding the eight-row/16,384-run retained cache.
The bounded and uncapped paths share source/account/project/read/layout checks
before and after yields. Synchronous comparison callers retain the same core
segmentation implementation.

Repeated over-limit construction, complete plan validation and other synchronous
initialization paths remain work. This change makes no wall-clock latency or
memory improvement claim.

Final optimized Chrome checks pass for uncapped yielding, unchanged negative
cache limits and rejection after source/read/project/account/owner changes
(0.03 s), the existing bounded preparation contract (0.03 s), both-mode uncapped
preparation and capped-rescan avoidance (0.05 s), failed-overlap complete fallback
(0.39 s), and shifted styled suffix geometry against the complete renderer
(4.24 s). These times are test execution, excluding compilation and browser
startup. Strict optimized WASM frontend library/test Clippy, formatting and the
production Trunk/PWA build pass. Full CI for this new checkpoint remains required.

### Near-limit wrapped suffix rejection diagnosis

A temporary opt-in trace adjacent to the existing core rejection gates identifies
why the beginning edit still measures 70 fresh probes. The diagnostic bundle
(`openwebide-frontend-1b473f491ddb72d9.js`) uses checkpoint `5dfff38` plus this
[diagnostic patch](editor-performance/wrapped-suffix-diagnostic.patch). The patch
is archived for reproduction and is not applied to the shipped editor. It adds
rejection labels without changing validation, limits, geometry or fallback rules.
Four core wrapped geometry/replay/failure contracts pass with the trace applied.

The existing Linux image, frozen backend, 4 CPUs, 10 GiB memory and 1 GiB shared
memory match the preceding samples. No builds or other browser checks ran during
measurement. Both adapters preserve actual 1,048,567-byte String styling, source
caret zero, complete source after input and complete 547,721 × 264 extents.
The [raw trace](editor-performance/wrapped-suffix-rejections-linux.jsonl) has one
repetition per adapter and no truncated records; it diagnoses failure rather
than establishing latency or memory improvement. Input paint takes 1130.6 ms
locally and 1157.9 ms remotely.

Both adapters report the same input rejection counts: 59 translated-first-row
overflows and eight dense-overlap failures, plus the initial empty-overlap and
fresh-terminal cases. No source, run-boundary or missing-probe rejection occurs.
The source and original run boundaries reconnect, but the new incoming gap
changes wrapping beyond the accepted first-row translation. Removing a rejection
would discard the exact geometry contract. The next layout work must support
that changed wrapping phase with measured, source/style-proved geometry rather
than concentrate only on segmentation or relax overlap tolerances.

### Fresh incoming-prefix reconnection

The shared Rust wrapped-paragraph plan can replace a cached incoming prefix with
its already measured current geometry, then retain the cached tail after a common
complete word starts a visual row and every subsequent overlap glyph proves the
same measured vertical translation. Original source, run-boundary and glyph
mapping checks remain in place. Cached overflow-sensitive records still require
fresh layout; tolerances, limits and fresh terminal measurements are unchanged.
The ordinary record gate validates the reconciled probe before it can publish.

Fresh glyph ranges may hang beyond a row box without widening its scroll extent.
The prefix keeps the extents already captured by the preceding fresh probe rather
than inferring width from those ranges. Missing prefix coverage, invalid rectangles
or nonreconnecting overlap retain the existing fresh-measurement path. Test-only
counters require the new path to execute in each workspace mode; they add no
production state or UI.

Optimized Chrome passes the positive-path/full-renderer comparison at seven widths
in both modes (5.44 s), the complete 16-test wrapped partition including font,
feature, whitespace, edits and cancellation contracts (59.75 s), and failed-overlap
complete fallback (0.39 s). Test times exclude compilation/browser startup. All
590 core tests, strict core and optimized WASM frontend library/test Clippy,
formatting and the production Trunk/PWA build pass. Temporary diagnosis code was
removed before final verification. The preceding checkpoints `5dfff38` and
`40bbcd9` each passed all five full CI jobs (runs `38008817667` and `38009617391`);
this new implementation still needs its own full CI result.

The production candidate (`openwebide-frontend-f0c38b80f3d1c716.js`, built from
`40bbcd9` plus this change) was measured twice per adapter and edit position with
the same existing Linux image, frozen backend, 4 CPUs, 10 GiB memory and 1 GiB
shared memory. No local builds or browser tests ran during measurement. The
[beginning traces](editor-performance/measured-prefix-beginning-linux.jsonl) and
[end traces](editor-performance/measured-prefix-end-linux.jsonl) preserve actual
1,048,567-byte String styling, exact source carets, complete source after input,
complete 547,721 × 264 extents and untruncated traces with Linux Chrome PSS.
Beginning edits still need 70 fresh probes and 1136.0–1192.2 ms; end edits retain
two fresh probes and take 113.5–123.0 ms. Chrome PSS is 695,425–701,608 KiB across
the samples. This extends proved reuse but does not resolve the principal
near-limit beginning-edit stall or establish a latency/memory improvement. The
remaining responsiveness, physical PWA input, folder recovery and accessibility
gates stay open.


### Batched native paragraph ranges and portable browser contracts

The browser adapter now reads exact native Range rectangles in one call per
paragraph probe. Rust still selects and validates source/UTF-16 endpoints,
text-node ownership and targets, constructs the resulting geometry and controls
fallback. The adapter returns the same first positive-height rectangle as the
individual reads; limits, density, tolerances and cancellation are unchanged.
The wrapped full-renderer oracle keeps independent individual native reads.

The new browser contract compares exact rectangles across both modes, all five
Monaspace families, healing/ligature profiles, wrapped/unwrapped Unicode, combining
marks split across text nodes, tabs and bidirectional content. Empty targets,
invalid endpoints, mismatched source and hidden rows retain their failure semantics.
The 11-test paragraph partition, 16-test wrapped partition, strict optimized WASM
library/test Clippy and production Trunk/PWA build pass.

Checkpoint `57d4cbb` failed CI run `38013770042` solely in the positive measured-prefix
browser test: a beginning edit did not preserve the changed wrapping phase into
the overlap with Linux fonts. The revised fixture edits immediately before a
nearby String boundary while preserving its original run endpoints. It retains
the positive-path assertion and complete-geometry comparisons at all seven widths
in both modes. It passes macOS Chrome and the same compiled WASM in Linux Chrome,
with both [normal fonts](editor-performance/range-batch-linux-contracts.jsonl)
and a [DejaVu-only font configuration](editor-performance/range-batch-linux-minimal-fonts.jsonl).
These two Linux runs also pass the new range contract. All five full CI jobs
pass for repaired checkpoint `aa6cdea` in [run 38016719224](https://github.com/openwebide/openwebide/actions/runs/38016719224),
including the complete browser partition and production editor input/pointer checks.
Repeated CI and the remaining editor completion gates remain required.

For cross-host reproduction, generate web bindings from the built component-test
WASM with `wasm-bindgen --target web --out-name tests --out-dir DIR COMPONENTS.wasm`.
Run `python3 tools/check-editor-components-browser.py DIR FILTER [FILTER ...]`.
The tool uses the standard wasm-bindgen test context and actual raw test exports.
Every requested filter must match at least one export; a valid filter paired with
a nonexistent contract now [fails before execution](editor-performance/component-missing-filter-linux.jsonl),
rather than reporting a partial pass. The [valid two-filter run](editor-performance/component-strict-filters-linux.jsonl)
executes and passes both contracts.
The existing Linux measurement image accepts read-only repository and binding
mounts with `--entrypoint python3`; no second Cargo target or image is needed.
For the reduced-font run, mount
[component-minimal-fontconfig.xml](editor-performance/component-minimal-fontconfig.xml)
and point `FONTCONFIG_FILE` to it. Both recorded runs select
`measured_wrapped_prefix_reconnection_matches_complete_geometry_in_both_modes`
and `paragraph_batches_match_individual_native_ranges_and_failures_in_both_modes`.

The production candidate (`openwebide-frontend-fc98197b1f2505ae.js`, based on
`57d4cbb`) was measured twice per adapter and edit position using the same Linux
image, frozen backend, 4 CPUs, 10 GiB memory and 1 GiB shared memory. No builds or
other browser checks ran during measurement. The
[beginning traces](editor-performance/range-batch-beginning-linux.jsonl) and
[end traces](editor-performance/range-batch-end-linux.jsonl) preserve actual
1,048,567-byte String styling, exact source carets, complete source after input,
547,721 × 264 extents, untruncated traces and Linux Chrome PSS.
Beginning edits take 1081.7–1108.8 ms with the same 70 fresh probes; end edits take
120.2–132.1 ms with two fresh probes. Range counts are unchanged. Beginning
bounding-box/reflow time remains 764.2–784.0 ms, dominating native range reads
at 109.1–115.8 ms. The samples do not establish a consistent overall latency or
memory improvement. The principal reflow stall and remaining editor gates stay open.


### Probe DOM retention experiments

Two disposable browser-only experiments use the same production range-batch bundle,
Linux image, frozen backend and resource limits above. They change only hidden
measurement probes, do not modify the shipped adapter and run one beginning-edit
sample per mode. Reproduction scripts are archived with the results; their
JavaScript hooks are experimental DOM primitives, not replacement editor policy.
No builds or other local browser measurements ran concurrently. These samples
preserve actual 1,048,567-byte String styling, complete source after input,
exact beginning caret and 547,721 × 264 extents.

The [whole-probe script](editor-performance/retained-probe-experiment.py) retains
text children only when the incoming rendered inner markup is identical, while
updating source metadata and the measured incoming gap. The
[results](editor-performance/retained-probe-experiment-linux.jsonl) show zero
identical-probe hits in either mode; beginning edits take 1226.1 and 1263.4 ms.
Parsing and comparison add work without avoiding layout for this workload.

The [differential script](editor-performance/differential-probe-experiment.py)
keeps equal subtrees, updates changed text/attributes in place and requires every
resulting DOM to serialize exactly to the incoming rendered HTML before native
measurement. The [results](editor-performance/differential-probe-experiment-linux.jsonl)
show 3910 equal-subtree hits and eight node replacements in each mode, but
beginning edits still take 1114.4 and 1116.9 ms, with 779.4 and 776.6 ms in
bounding-box/reflow reads. Preserving DOM subtrees does not eliminate the principal
changed-gap layout work. Neither experiment is adopted; they are diagnostic
samples, not full geometry contracts or a responsiveness gate pass. The next
implementation must address exact layout/reconnection across changed wrapping
phases rather than rely on retained DOM alone.


A separate [probe-size microbenchmark](editor-performance/probe-size-experiment.py)
clones the real hidden-probe font/style and measures fresh wrapped String markup
at 2, 4, 8 and 16 KiB with three incoming gap positions. The
[raw results](editor-performance/probe-size-experiment-linux.jsonl) include the
production run and 24 native reflow samples per size. After the first three
samples, mean reflow times are 1.41, 2.70, 5.50 and 11.02 ms respectively: cost
scales approximately with text bytes. Smaller probes would add dense-overlap and
scheduling work without eliminating the total shaping cost; these diagnostic
results do not justify changing the probe cap or accepting approximate geometry.
This synthetic native primitive check is not a both-mode full-renderer contract.


### Cooperative cold text-node enumeration

The Rust browser primitive now walks text nodes lazily, queuing one sibling at a
time instead of materializing every child of a wide parent before its visit
budget applies. Synchronous queries share the same traversal and original 65,536
visited-node limit. Cold row geometry advances it in existing 128-operation
geometry batches with task/frame yields, cancellation and root-connectivity
checks. Source is revalidated after enumeration before those UTF-16 offsets can
produce geometry. The existing `EditorActions` scopes and shared core geometry
policy remain the entry points for both workspace adapters.

The 70 KiB fragmented-row regression compares every text-node identity, order,
UTF-16 offset and length with the original eager traversal, including combining
marks split between nodes, emoji, empty text nodes and an adjacent root sibling.
It requires admitted long-row geometry to match the complete native renderer,
observes cancellation during an actual yield, rejects a disconnected root and
rejects source replacement during enumeration. It passes in both modes on macOS
Chrome (2.45 s). The existing font/glyph/cancellation contract passes (0.81 s).
The same compiled WASM passes the [13-contract Linux selection](editor-performance/cooperative-dom-linux-contracts.jsonl),
including both traversal contracts and the paragraph font/feature/whitespace,
limits, origin rounding, source reuse and failed-overlap fallbacks (95.36 s).
Strict optimized WASM library/test Clippy, formatting and production Trunk/PWA
build pass. These are correctness checks, not large-file latency measurements;
changed-wrap reflow, synchronous pointer queries, DOM installation and other
remaining editor gates are still open.

Repeated CI run `38017353270` for checkpoint `2cf0b1b` passes four jobs but fails
three existing editor probe-count assertions (`measure_highlight_bursts`,
`localized_wrapped_edits_reuse_exact_row_heights_in_both_modes`, and
`cold_repeated_wrapped_rows_share_layout_and_preserve_far_edits_in_both_modes`).
The application code is identical to green checkpoint `aa6cdea`. Font-notification
and preparation timing need investigation; the strict reuse-count assertions
remain unchanged. A single green CI run has not satisfied the reliability gate.


### Retained row font ownership

Matching font notifications now consult the ordinary row replay cache as well as
the paragraph cache through the shared `EditorActions::font_measurements_changed`
entry point. Source edits intentionally invalidate current geometry while
retaining replay candidates; a notification arriving before replacement
preparation previously discarded the ordinary cache despite unchanged metrics.
Both caches use the same file/project, pending epoch, read revision, account
generation and font epoch ownership checks. Changed or missing metrics still
require invalidation. The browser geometry adapter remains shared by both modes.

The regression fails before this change and passes after it in macOS Chromium,
including seven stale-scope cases in each workspace mode. Both retained-row and
in-flight ownership contracts also pass in [Linux Chromium](editor-performance/retained-row-font-linux-contracts.jsonl).
Strict optimized WASM library/test lint, formatting, production Trunk/PWA build
and the existing settled-font browser contract pass. Linux reproduction of
the three existing CI probe-count failures confirms timing remains an open
verification issue. An attempted highlighted-token warm-up condition timed out
and was discarded; existing performance assertions and deadlines are unchanged.
The full editor goal and CI reliability gate remain open.


### Prepared-paint burst measurements

The three previously failing Linux component tests now pass together in three
fresh browser sessions (2.25, 2.27 and 2.23 seconds), using the same compiled WASM
as the passing macOS Chrome burst check (1.49 seconds).
[Raw repetition results](editor-performance/burst-prepared-paint-linux-repetitions.jsonl)
include both-mode localized wrapped edits and repeated-row replay. Strict optimized
WASM library/test Clippy, formatting and whitespace checks pass.

A current-token identity check alone was insufficient: it still reproduced
`[2, 1, 1, 1, 1]` burst probes, followed by the localized test counting 131 rows.
In the worker-disabled component environment, the initial budgeted parser can
leave plain fallback installed, then complete syntax during the first edit. That
legitimate style transition remeasures rows and mixes cold preparation into the
warm-edit benchmark. A failed WASM assertion also leaves its mounted view active,
which contaminates the following global probe observer.

The burst fixture now injects the existing deferred transport and produces replies
with the real shared `SyntaxPreparations` parser. It waits for published syntax or
terminal fallback and dimensions tied to the exact token allocation before
counting edits. During each burst it rejects obsolete requests and publishes the
coalesced current source through the production facade. The 1,000/10,000-line
sources, visible-generation limits, probe limits and deadlines are unchanged.
No production parser limits, geometry or fallback policy changed; these warm-edit
results do not establish cold-start latency. The separate font-cache fix preserves
ordinary-row replay across matching notifications.

All five CI jobs passed for `1b499c4` in run `38018986189`. All five jobs also passed for the
font-cache checkpoint `574d2a5` in run `38020653214`. The corrected fixture
checkpoint `644ece9` passes the complete [147-test Linux editor component partition](editor-performance/prepared-paint-linux-editor-partition.jsonl)
with CI's two existing matrix exclusions and unchanged 300-second run deadline
(157.26 seconds). Its full CI run `38021534434` remains under observation; the
complete editor and reliable-CI gates remain open.


### Changed-phase word-start proof (2026-10-09)

A second [diagnostic patch](editor-performance/wrapped-prefix-proof-diagnostic.patch)
records the exact old/current glyph pairs considered by prefix reconnection,
without changing any admission or replay rule. The bundle
`openwebide-frontend-d2b75278f3616306.js` uses `338c7bd` plus that temporary patch.
All five native wrapped paragraph contracts pass (0.34 s), and the diagnostic
Trunk/PWA build passes. The patch is archived and removed from production source.

The [observer script](editor-performance/wrapped-prefix-proof-measure.py) also
compares complete probe child markup, incoming gaps, row style, font epoch,
prepared-paint flag and computed probe CSS across nonadjacent probes. It only
observes already requested bounds and reads markup; it does not retain or modify
DOM. These observations include measurement overhead and are not latency wins.

The [Linux samples](editor-performance/wrapped-prefix-proof-linux.jsonl) use the
existing image, frozen backend (SHA recorded), 4 CPUs, 10 GiB memory and 1 GiB
shared memory, with one fresh runtime/browser per mode and no concurrent builds.
Both preserve actual 1,048,567-byte String styling, caret zero before input,
complete source after input and 547,721-pixel height/264-pixel scroll width. Input
paint takes 1110.8 ms locally and 1186.2 ms remotely; traces are untruncated.

Each mode's eight sampled incoming-overlap proofs contains **zero shared
source glyphs at visual-row starts**. The first probe shifts the incoming origin
from 72.625 to 85.625 pixels. A glyph starting the current next row at zero is
still at 195.53125 pixels on the cached first row; a cached next-row start is at
48.359375 pixels in the current row. This rules out treating the incoming prefix
as a small replaceable tail in these samples: the line-breaking phase remains
different throughout the measured overlap. The observer also finds 149 distinct
probe markup/layout identities and no within-phase or prior-phase matches in
either mode. Nonadjacent identical-probe caching does not address this workload.

The next implementation must expose exact **current viewport coverage** while
complete paragraph extents continue preparing, rather than wait for full-row
completion before publishing an origin viewport. Such coverage must carry current
source/font/layout/account ownership, reject requests outside measured anchors,
and never turn a partial height into complete source extents or synthesize a
caret at its temporary endpoint. Nonorigin windows, composition, cancellation
and failed native proofs must preserve complete fallback. The source-owned
facade and shared core remain the entry points for both workspace adapters.
Full changed-paragraph layout work, responsiveness and all remaining completion
gates are still open.


### Reboot checkpoint: measured coverage primitive

The shared core can expose conservative origin coverage from committed paragraph
anchors without reporting complete row extents. ASCII and Unicode regression
coverage rejects uncovered ranges, invalid heights and temporary endpoint carets;
complete measurement still preserves the actual EOF caret. All six native wrapped
paragraph contracts pass. This primitive is not connected to browser publication
yet and makes no new responsiveness claim.

Resume with facade ownership and independent browser-renderer validation before
activating partial paint. The unfinished integration and historical draft stashes
are preserved locally under the ignored `ai_docs/pre-reboot-drafts/` directory;
they are investigation material, not changes to apply wholesale.


### Partial coverage ownership (2026-10-09)

The shared editor facade now binds partial wrapped coverage to the same origin
measurement policy as completed prefixes. Geometry stays separate from complete
row tables, and queries share immutable anchors instead of cloning them. Queries
return paint provenance so the browser can compare its actual current metrics.
Composition hides coverage and rejects publication.

The new ownership contract checks ASCII and Unicode sources in both modes across
15 scope changes (60 combinations), including replacement tickets, font/layout,
read, source, folding, syntax, whitespace, wrapping, account, project, file,
completion and composition changes. Five mismatched paint identities cannot
overwrite retained coverage. Old completion does not erase a replacement ticket.
The existing 24-case completed-prefix ownership contract still passes.

Both contracts pass in macOS Chrome. The same final compiled WASM passes in the
existing Linux Chrome image in 0.45 seconds; exact artifact and source hashes,
image identity and results are in the [ownership evidence](editor-performance/partial-coverage-ownership-linux.jsonl).
Optimized WASM library/tests Clippy passes with `-D warnings`, and formatting
checks pass. Browser coverage publication, independent full-renderer parity and
production latency/PSS evidence remain outstanding; this checkpoint claims no
new input responsiveness or complete editor-goal verification.

Earlier full CI checkpoint `644ece9` (run 38021534434) is now confirmed successful
across all five jobs, in addition to the previously confirmed `338c7bd` result.
The latest checkpoint CI runs remain pending at the time of this verification.

### Partial wrapped origin publication checkpoint

Eligible origin viewports now publish only native-measured partial coverage while
complete paragraph geometry continues preparing. Failed crop proofs discard that
coverage and wait for full geometry; uncovered carets and temporary endpoints are
rejected. Replacement geometry invalidates affected fragment-cache windows so
provisional heights cannot survive the complete publication.

All 149 contracts in the Linux editor partition passed in 183.97 seconds,
including the held-continuation publication regression, independent full-renderer
oracle and failed-proof fallback in both workspace modes. The 17 wrapped/unwrapped
macOS browser contracts also passed; cache allocation/recency tests and strict
WASM lint passed. See [Linux results](editor-performance/partial-coverage-publication-linux.jsonl).
This checkpoint does not establish production boundary latency/PSS improvements;
those measurements and the full editor completion gates remain on the roadmap.
CI for `3956122` completed successfully across all five jobs.


### Partial coverage extents and positive viewport offsets

Partial paragraph paint no longer advertises complete document height. The editor
uses the same shared facade and measured coverage in both workspace modes, and
accepts a positive scroll offset only when the complete visible viewport plus
overscan lies within proved geometry. Completed-prefix selection and uncovered
viewport fallback remain unchanged. The held-continuation regression covers cold
origin, 2-pixel edit offsets and failed crop proofs at 19.5 pixels, retaining the
independent complete-renderer, source ownership, endpoint and complete-height checks.

Production Ctrl+Home leaves a 2-pixel scroll offset in this Linux browser. The old
origin-only selector therefore delayed visible input paint until complete geometry.
The [before-offset evidence](editor-performance/partial-coverage-milestones-before-offset-linux.jsonl)
records first styled input paint at 1.26–1.37 seconds. Four fresh
[after-offset repeats](editor-performance/partial-coverage-milestones-after-offset-linux.jsonl)
(two per workspace mode) show styled viewport paint at 253–260 ms while complete
geometry takes 1.18–1.24 seconds. All four early input paints precede complete
extents. Cold viewport paint takes 1.57–1.62 seconds, with complete geometry at
2.54–2.60 seconds. Peak Chrome PSS is 708,631–739,275 KiB. These are
1,048,567-byte styled Rust paragraphs with exact beginning caret and edited-source
verification; local-mode measurements use recovery data rather than a real granted
folder handle. The measurement now separates visible paint from complete geometry
and requires complete source extents for its original completion gate. Artifact
hashes, image identity and resource limits accompany the evidence.

CI run `38027092539` passed native/backend, Windows, Docker and production frontend
jobs, including isolated Linux compiler preparation, but failed the UI partition.
Its first failure was the new held-continuation fixture observing plain fragments
before String styling. The waiter now requires actual String styling within the
existing deadline; no geometry, source or fallback assertion was removed. The final
regression passes in macOS Chrome in 2.70 seconds. Full Linux partition and new
checkpoint CI results are tracked below. Full geometry costs, broader boundary
workloads, physical PWA input and the complete editor goal remain open.

The final compiled WASM passes all 149 Linux editor-partition contracts with the
existing two font-matrix exclusions and unchanged 300-second deadline; see
[final partition evidence](editor-performance/partial-coverage-final-linux.jsonl).
Strict optimized WASM lint, formatting and Python compilation also pass. The
previous two-contract Linux check is retained as
[focused offset evidence](editor-performance/partial-coverage-offset-linux.jsonl).
A successful new full CI run remains required.


### One-pass uncapped unwrapped run preparation

The shared `EditorActions::prepare_paragraph_measurements_cooperatively` facade
now prepares a fresh styled table once with the existing cooperative core iterator.
It retains the completed table only when it fits the existing cache limit; otherwise
it publishes the same scoped negative retention entry and passes the ephemeral
complete table to the measurement plan. Existing negative entries still allow
complete preparation. This removes the initial capped pass followed by a restart
without enlarging the retained cache or changing browser geometry contracts.
Both filesystem modes use this facade and the same DOM measurement adapter.

The fresh/negative-cache regression checks one preparation, yielding before
publication, exact comparison with the previous synchronous plan, and source,
read, project, account and disposal cancellation. All nine shared row-preparation
regressions pass in macOS Chrome (0.09 seconds). The both-mode component contract
now requires exactly one complete segmentation on the first request as well as
subsequent requests, while retaining bounded negative metadata and stale-project
rejection. Strict optimized WASM library/test lint also passes.

The raw-bindings Linux harness cannot run the mixed library artifact: module
initialization invokes an unrelated native notification test before the WASM
browser harness can start. That attempt is not evidence of a passing library
partition; the supported WASM runner provides the library result above, and Linux
verification uses the component artifact. This checkpoint makes no new latency
or process-memory claim. Repeated over-limit requests still reconstruct an
ephemeral full table; remaining paragraph costs and full editor gates stay open.

The updated both-mode component contract passes in macOS Chrome (0.05 seconds).
All 149 Linux editor-partition contracts also pass with the existing font-matrix
exclusions and unchanged deadline; see
[one-pass Linux evidence](editor-performance/one-pass-unwrapped-runs-linux.jsonl).
Full CI for the new checkpoint remains required.


### Cooperative paragraph anchor setup

Paragraph paint-run order/character-boundary validation and exact source-anchor
mapping now advance in batches through `ParagraphAnchorPreparation` in the shared
Rust core. An opaque complete result authorizes the existing wrapped or unwrapped
plan; a partial, invalid or mismatched-layout result cannot authorize probes.
Original synchronous constructors drain the same preparation policy, preserving
complete-renderer fallback and caller contracts. Small tables retain original
paint-run anchors, including omission of interior Unicode cluster boundaries;
over-budget tables retain the original sparse checkpoint policy.

Both browser measurement paths call the shared editor facade, which revalidates
source/read/project/account ownership around every yield. Cached segmentation is
not repeated during anchor setup. The DOM adapter receives the complete plan and
checks its current metrics/target before measuring; no adapter implements a second
validation or mapping policy. Plain boundary collection, bounded sparse-table
finalization and unrelated full-source fallbacks remain separate remaining costs.

Twenty-four native paragraph contracts pass, including an independent complete
Unicode-coordinate oracle at multiple budgets and rejection of partial, invalid
and wrong-layout setup. Native core Clippy passes with warnings denied. The new
facade regression suspends after segmentation is cached and checks completion
and source/read/project/account/disposal cancellation for both paragraph layouts.
Browser, Linux partition and new checkpoint CI results are tracked below. This
change makes no measured latency/PSS claim and does not complete the editor goal.

All ten shared row-preparation regressions pass in macOS Chrome (0.13 seconds),
including cached-setup cancellation. Twelve paragraph component contracts pass
in 63.16 seconds, including the complete-renderer and font/feature/whitespace
matrices in both workspace modes. All 149 Linux editor-partition contracts pass
in 171.90 seconds with the existing exclusions and deadline;
[artifact-bound evidence](editor-performance/cooperative-paragraph-anchors-linux.jsonl)
records the compiled binding hash. Strict optimized WASM lint passes.

CI run `38029031142` for the previous viewport checkpoint passed four jobs but
failed first in `source_slice_measurement_failure_restores_full_source_in_both_modes`.
Its three-second full-source fallback wait timed out; later tests also failed and
the partition reached its overall deadline. The fixture now restores the Range
hook before its failure assertion and reports whether the injected failure fired,
visible source bytes and slice ownership. Its deadline and complete-source/native
fallback assertions remain unchanged. This diagnostic edit followed the Linux
partition above; its targeted checks and full CI remain required.

The diagnostic fixture passes in macOS Chrome in 0.88 seconds. Its four-contract
Linux neighborhood (loaded-font switch, near-limit styled horizontal windows,
injected source-slice failure and prepared native-window editing) passes under a
two-CPU limit with all existing deadlines/assertions retained; see
[constrained Linux evidence](editor-performance/slice-failure-diagnostics-two-cpu-linux.jsonl).
Final strict WASM lint also passes. This does not reproduce or resolve the CI-only
timeout; the expanded diagnostics must establish the next CI failure state if it
recurs. CI reliability and the complete editor goal remain open.


### Production source-publication task timing and scoped failure injection

Four fresh production repeats after cooperative anchor setup pass source/caret,
actual String styling, complete geometry, scrolling and Linux PSS checks in both
workspace modes. First styled input viewport paint is 251–274 ms, complete input
geometry is 1.22–1.26 seconds, cold styled viewport paint is 1.55–1.60 seconds and
complete cold geometry is 2.49–2.56 seconds. Peak Chrome PSS is
693,793–736,502 KiB. These results do not establish a material improvement over
the previous cohort. [Task-timed evidence](editor-performance/cooperative-anchor-production-tasks-linux.jsonl)
records exact frontend/backend/script hashes and image/resource constraints.
The same local recovery-handle limitation and frozen backend snapshot apply.

The measurement tool now records bounded long-task start/duration, observation
time, phase and current editor scope. The four ordinary runs show 140–147 ms
cold tasks during first source publication and 64–75 ms input tasks before
geometry completion. A separate, explicitly labeled
[diagnostic CPU profile](editor-performance/cooperative-anchor-cpu-profile-linux.jsonl)
includes profiler overhead and cannot serve as a normal latency/PSS baseline.
Correlating its monotonic navigation clock with task start times shows
`refresh_editor_scroll`/`set_editor_scroll_position`, string transfer into WASM
and anonymous WASM work in the cold task; some of the same WASM stacks recur
after native input. Stripped production symbols do not establish exact named
Rust-function attribution. Source publication/indexing and scroll-refresh paths
remain the next measured candidates, rather than declaring total background
geometry costs resolved. Python compilation and injected JavaScript syntax checks
pass. The current production Trunk/PWA build also passes.

Both checkpoint CI runs `38029581778` and `38030710930` passed four jobs but failed
the source-slice failure fixture first. The latter's diagnostics prove injection
fired while visible source still began at zero (770 of 210,000 bytes). The fixture
previously accepted an origin anchor as setup readiness and could corrupt a
background preparation probe before reaching its requested offscreen viewport.
It now requires complete current-scope geometry/extents, verifies actual scroll
positions, excludes background height probes and requires a positive injected
source offset. All setup/fallback deadlines remain three seconds, and exact
full-source restoration, native source preservation and subsequent bounded paint
assertions remain in place. The held-continuation contract separately retains
partial-coverage failure semantics. The corrected fixture passes on macOS in
0.91 seconds. The unchanged Linux editor partition passes all 149 tests in
166.57 seconds, with its existing two font-matrix exclusions and 300-second
deadline; see [full output](editor-performance/complete-source-slice-failure-linux.jsonl).
Strict optimized WASM library/test lint and formatting checks pass. A new full
CI run remains required; the editor and CI reliability gates stay open.

### Batched viewport sizing reads

The browser viewport primitive now reads the client width/height together before
changing input dimensions, reads any required native fallback extents before
changing either source extent, and retains the existing resize-observer settling
and source/account/view ownership checks. Complete source dimensions still avoid
native dimension reads; bounded native windows still retain prior source extents
while replacement geometry prepares. The feature facade and both workspace
adapters remain unchanged.

The existing both-mode extent contract now compares shrinking and growing
viewports with an independent complete native renderer, with wrapping enabled
and disabled, exact source checks and its unchanged three-second waits. It
passes on macOS in 1.20 seconds. The same Linux editor partition passes all 149
tests in 176.34 seconds, retaining the two existing font-matrix exclusions and
300-second deadline; see [full output](editor-performance/batched-scroll-refresh-linux.jsonl).
Strict optimized WASM library/test lint and the production Trunk/PWA build pass.

[Four fresh production repeats](editor-performance/batched-scroll-production-linux.jsonl)
pass source/caret, actual near-1-MiB String styling, complete geometry, scrolling
and Linux PSS checks in both modes. Cold styled viewport paint is 1.52–1.61
seconds and complete cold geometry is 2.51–2.57 seconds. Input viewport paint is
248–257 ms and complete input geometry is 1.20–1.22 seconds. Peak Chrome PSS is
683,929–744,991 KiB. Cold source-publication tasks still take 136–150 ms and input
tasks 60–72 ms. These observations do not establish a material latency/memory
improvement or completion of source/controller responsiveness work. The same
frozen backend, local recovery-handle limitation and measurement image/resource
constraints apply; exact artifacts are recorded in the evidence header. Full CI,
physical-device input and the remaining editor implementation gates remain open.

### Reuse of proved unchanged preceding-row indexes

The shared source index's conservative edit envelope formerly rebuilt the
preceding row even when its complete LF/CRLF ending preceded the replacement.
`LineIndex::update` now excludes that proved unchanged row from reconstruction,
retaining its admission summary and exact immutable native/visual coordinates.
Edits inside an ending and unterminated rows retain the conservative scan.
Parser context envelopes are unchanged; no workspace-mode branch or second
implementation was added. Document construction and changed long-row indexes
still run synchronously.

All 252 native editor tests pass, including exhaustive replacements at Unicode,
LF, CRLF and standalone-CR boundaries against fresh complete indexes, and exact
retained-coordinate checks across edits after long rows. Retained snapshots
remain unchanged. Strict optimized core library/test lint passes, as does the
production Trunk/PWA build. The unchanged Linux editor partition passes all 149
tests in 179.02 seconds, retaining both existing font-matrix exclusions and its
300-second deadline; see [full output](editor-performance/retained-preceding-row-editor-linux.jsonl).
Strict optimized frontend WASM library/test lint and formatting checks pass.

The production measurement tool adds a near-1-MiB styled first row followed by a
short editable row. Wrapped crop movement is proved from measured offsets even
when scrolling remains inside the same logical row of a multiline file. Native
Ctrl+End, exact recovered source/caret before input and full resulting source
after input are verified. Actual String styling, complete geometry, scrolling
and Linux PSS checks remain required. With unchanged four-CPU/10-GiB limits,
the previous `b846c17` frontend's four fresh local/remote samples show following-row
input viewport/complete geometry in 105.5–112.0 ms; the new frontend's four fresh
samples show 78.4–90.0 ms. Cold paint does not improve: it is 1.51–1.58 seconds
before and 1.56–1.62 seconds after. Peak Chrome PSS is 729,933–742,401 KiB before
and 734,243–742,695 KiB after, with no established memory improvement.
[Before evidence](editor-performance/retained-preceding-row-before-linux.jsonl)
and [after evidence](editor-performance/retained-preceding-row-after-linux.jsonl)
record exact frontend/backend/script hashes. These observations support the
following-row improvement, not removal of cold source-publication tasks or
changed-long-row costs. The frozen backend, local recovery-handle limitation and
existing image apply to both cohorts. Python compilation and executing all eight
production measurements verify the updated measurement script.

The earlier crop-failure fixture checkpoint `11150a0` passed all five CI jobs in
[run 38033623746](https://github.com/openwebide/openwebide/actions/runs/38033623746).
The UI log confirms 375 other components, 149 editor components, both independent
font matrices, frontend unit/integration partitions and storage measurements pass;
the editor partition takes 228.24 seconds. Both previously failing crop-failure
and early wrapped-paint fixtures pass. This establishes that checkpoint's full
CI result; subsequent implementation checkpoints still require repeated full CI.

### Combined row-coordinate character pass and complete browser grouping

Source-row indexing now collects raw UTF-16 totals, admission summaries and
native checkpoints during the visual index's character traversal. Short or
visual-over-limit rows use one character pass for those summaries/checkpoints;
projection windows retain the existing short-row fast path. Visual grapheme
checkpoints, plain paint boundaries, CRLF inverse offsets and shared immutable
coordinates remain unchanged. Constructors still run synchronously: fewer passes
do not establish bounded initial construction or changed-row preparation.

All 253 native editor tests pass, including independent byte-admission/UTF-16
oracles at ordinary, Unicode/CRLF and byte-limit boundaries and visual-index
rejection above the structure limit. Existing independent coordinate queries,
index edits, history, composition and retained-snapshot contracts also pass.
Strict optimized core and frontend WASM library/test lint, formatting and the
current production Trunk/PWA build pass.

The `b846c17` and `7ea1367` full CI runs each passed four jobs; their combined
149-test editor browser run exceeded its 300-second deadline without an
individual assertion failure. The previously failing crop/partial-paint and
new resize contracts passed. CI now uses
`tools/run-editor-browser-tests.py`: Cargo supplies current test artifacts, the
official runner inventories their editor tests, and independently listed
selections must equal every planned group. Selection collisions, omissions,
duplicates and missing independent matrices fail the plan. Ordinary groups have
at most 64 tests; round-robin assignment distributes expensive fixtures. Every
test keeps its readiness assertions and each browser keeps the 300-second
deadline. The existing two font matrices still run independently.

[Official Linux evidence](editor-performance/combined-row-official-browser-linux.jsonl)
proves all 167 editor tests execute: ordinary component groups pass 50/50/49 tests
in 99.38/33.65/37.00 seconds, font matrices pass in 22.06/14.97 seconds, and 16
editor unit tests pass in 0.18 seconds. Artifacts/script hashes, official runner
version/checksum provenance, existing image and unchanged four-CPU/10-GiB limits
are recorded. This verifies the actual grouping/runner; full CI with the new
grouping subsequently passed all five jobs at `2eab2c2`. The
[complete CI receipt](editor-performance/grouped-ci-2eab2c2.json) includes every
planned group, both font matrices, the ordinary UI partition and browser storage
workloads. Later implementation checkpoints still require complete verification.
The next checkpoint, `54c814e`, also passed all five jobs with the same complete
selection and unchanged browser limits; its [receipt](editor-performance/grouped-ci-54c814e.json)
records the repeated execution of every group and both matrices.

Fresh production cohorts compare the previous `7ea1367` frontend with the
combined-pass frontend, four local/remote repetitions each. No host build
process was observed at one-second intervals during either cohort (25/24
observations); the earlier known-contended run is excluded from comparisons.
Exact full source/caret, actual near-1-MiB String styling, scrolling, complete
geometry and Linux PSS checks pass. Cold styled viewport paint is 1.52–1.70
seconds before and 1.49–1.56 seconds after. Before cold source-publication tasks
are 139–149 ms; three after tasks are 125–129 ms and one is 178 ms. That outlier
prevents claiming consistently improved blocking latency. Input viewport paint
is 245–269 ms before and 247–273 ms after; complete input geometry is
1.18–1.26 seconds before and 1.18–1.26 seconds after. Peak Chrome PSS is
672,942–738,142 KiB before and 732,281–742,660 KiB after, with no established
memory improvement. These measurements do not complete responsiveness gates.
[Before](editor-performance/combined-row-before-linux.jsonl),
[after](editor-performance/combined-row-after-linux.jsonl) and
[host observations before](editor-performance/combined-row-before-host.json)/
[after](editor-performance/combined-row-after-host.json) retain exact artifacts
and limitations. Both cohorts include the same trace/host-observation overhead;
the frozen backend snapshot and absence of a real local directory handle remain.

## Paint segmentation within large graphemes

Styled run preparation now advances a Unicode cursor through source and backward
context chunks of at most 512 bytes. Each scan step caps cursor calls and forward/context
progress; a large indivisible grapheme can suspend before emitting its run. Small
chunk overlap retains sequential regional-indicator parity. Original 512-byte
grapheme-safe run boundaries, token merging, CR normalization, caps and complete-only
publication stay exact. This shared primitive serves the existing editor facade
without selecting workspace modes.

All 256 native editor tests pass with `PROPTEST_CASES=1024`, including fresh
whole-string segmentation comparisons and the retained mixed-Unicode failing seed.
The same final WASM artifact passes all 16 editor library tests in macOS and Linux
Chrome. A giant combining/emoji cluster must suspend without publishing any run;
source/read/project/account/disposal changes reject that suspended preparation.
The browser boundary contract includes a flag pair crossing byte 512 and CR
normalization. [Runner evidence](editor-performance/chunked-paint-browser.jsonl)
records exact source/artifact hashes, selection, limits and complete console output.
These are scoped correctness/yielding checks, not production responsiveness or
memory measurements. Synchronous coordinate/anchor construction, other complete
fallbacks and the full release/device gates remain unfinished.

## Indexed long-cluster queries

The completed visual index now retains the byte end and exact starting
byte/UTF-16/glyph coordinates of clusters longer than 512 bytes, during its
existing construction pass. Original paint checkpoints remain separate and
unchanged. Anchor/interior queries skip indexed cluster interiors; caret lookahead
stops at their known start instead of materializing the next large cluster.
Retained-memory accounting includes the sparse metadata allocation. Initial
coordinate construction and other whole-source preparation remain synchronous.

All 257 shared editor tests pass. Combining, emoji-join and Indic clusters with
prefix lengths around the 512-byte seams compare every glyph coordinate and
original paint boundary with complete segmentation, including interior and EOF
queries. Strict optimized core/frontend WASM lint passes. The same final WASM
artifact passes all 16 editor library tests in macOS and Linux Chrome, including
exact giant-cluster coordinate comparisons through the existing editor tests.
[Browser evidence](editor-performance/long-cluster-query-browser.jsonl) records
source/artifact hashes, unchanged limits and complete runner output. These scoped
checks do not establish full production latency or memory gates.

## Resumable visual-index construction

Paint segmentation and visual-index construction now share the same bounded
Unicode source/context scanner. The ordinary constructor consumes the resumable
builder completely; cooperative callers can advance it in scan-unit budgets and
publish only its completed index. Source visitation occurs once in forward byte
order, including when one large cluster suspends across tasks; backward Unicode
context does not repeat the character visitor. Paint seams, native/glyph totals,
long-cluster metadata, tab detection and source-paint admission stay exact.

All 258 shared editor tests pass, including independent complete segmentation and
exact character-visit comparisons across budgets, Unicode context, EOF, RTL and
CR/tab fixtures. Strict optimized core/frontend WASM lint passes. All 16 editor
library tests pass on macOS and Linux Chrome; the giant-cluster fixture explicitly
advances the new builder across yielding tasks before checking complete coordinates.
[Browser evidence](editor-performance/cooperative-visual-browser.jsonl) records
exact source/artifact hashes and complete runner output. This is preparation
infrastructure: native/row/admission indexing, workspace-read and recovery
integration still need cooperative construction/publication. Existing document
calls remain synchronous, and production latency/memory gates are unfinished.

## Cooperative row and document indexes

Native checkpoints and admission summaries now have complete-only row preparation.
Eligible visual rows share the bounded Unicode traversal; short/unsupported visual
rows advance through bounded character chunks. Logical row discovery scans at most
8 KiB per unit, then advances that row's coordinates before adding its exact raw
and normalized offsets. EOF empty rows, CRLF and standalone-CR admission semantics
remain identical. Ordinary index constructors consume the same implementation.
`DocumentPreparation` exposes the completed index as a source-sharing document;
it cannot expose partial row/native/visual mappings.

All 260 native editor tests pass, including independent logical-row discovery,
byte admission, UTF-16/native conversion and complete segmentation comparisons
across budgets and oversized visual fallback. Strict optimized core/frontend WASM
lint passes. All 16 editor library tests pass in macOS and Linux Chrome; the giant
fixture now prepares a complete document through yielding tasks, checks source
sharing/clean state and compares exact glyph coordinates. [Browser evidence](editor-performance/cooperative-document-browser.jsonl)
records source/artifact hashes and complete output. Workspace reads and recovery
still need to await this builder with ownership cancellation and draft protection.
Metadata vector growth, saved-source allocation and final publication costs remain
responsiveness/memory targets; these primitive checks do not complete those gates.

## Cooperative incoming files

The shared editor facade now prepares incoming files before publishing their
source. Allocation-free admission advances through at most 8 KiB per unit, rejects
byte limits immediately, and retains CRLF/standalone-CR and first-error ordering.
Normal and lossy workspace reads await admission and complete document indexes;
both filesystem adapters use the same preparation and publication policy. Every
yield checks account, project, read revision, root guard, source revision and draft
ownership. Publication installs the completed document and source together;
over-limit sources retain the existing bounded read-only view. Unchanged files
retain their existing selections, folds, history and source allocation.

Recovery and other synchronous document callers still need this integration.
Capacity memoization, metadata growth, saved-source allocation and final equality
checks/publication remain responsiveness targets; these changes do not establish
production latency or memory gates.

All 261 native editor tests pass, including exact admission comparisons across
Unicode/CRLF byte seams and first-error precedence. Strict optimized core and
frontend WASM lint passes. All 19 editor library browser tests pass on macOS and
Linux Chrome; the extended file-switching test passes through both filesystem
adapters on both platforms, including normal and lossy prepared reads.
[Browser evidence](editor-performance/cooperative-read-browser.jsonl) records
source/artifact hashes and complete output. Assertions, the three-second readiness
limit and the 300-second browser runner timeout remain unchanged.

## Cooperative recovery hydration and disk verification

Recovery prepares saved and draft indexes with the same shared editor admission
and document builder as incoming file reads. Final restoration shares edit history
policy with ordinary transactions, installs the prepared draft index without
rebuilding it, and retains one-step undo against the original saved source. Clean
records reuse their saved index. Workspace hydration collects complete documents
before publishing project tabs, buffers, scroll positions and selected content
atomically. Ownership checks reject newer editor activity, compositions, reads,
account changes, root/handle/bridge changes and disposed state across yields;
final snapshot validation also protects inactive project state.

Disk verification now uses shared recovery classification without constructing
throwaway documents. Changed clean disk sources use cooperative preparation with
retained document source/revision checks before replacement. Explicit recovered
file review reloads still construct synchronously. Record validation, admission
memoization, source/history copies, final comparisons and publication remain
responsiveness/memory work; physical folder permission and PWA checks remain
unproven. These changes do not complete the production latency/memory gates.

All 263 native editor tests pass, including comparisons with transaction-based
restoration, undo/redo, folds, source-index sharing and invalid prepared documents.
Native workspace recovery tests and strict optimized core/frontend WASM lint pass.
All 20 editor library tests and 17 inventoried recovery/read component contracts
pass on macOS and Linux Chrome. The new hydration fixture explicitly yields during
saved and draft preparation in both project modes, checks complete-only source
publication and rejects account/root/read/source/epoch/disposal changes. Existing
real-adapter hydration, conflicts, permission retry, review, save and file-read
contracts remain covered without changing their readiness or runner deadlines.
[Browser evidence](editor-performance/cooperative-recovery-browser.jsonl) records
exact artifacts, source hashes, selection inventory and complete output. The
[preceding complete CI run](editor-performance/grouped-ci-81a6516.json) also passed
all five jobs; current changes still require their full CI run.

## Cooperative explicit review reloads

Explicit recovered-file reloads now use the same complete-only admission/index
preparation as incoming reads and hydration. Cheap account/root/read-source and
composition guards run across yields; complete recovery snapshot validation runs
before publication. The reviewed disk is read again after preparation so external
changes during that window preserve the draft and require another review. Reloads
install the completed document and share its source with active content/buffers.
Over-limit sources keep bounded read-only viewing without allocating document
indexes. Clean changed-disk reloads also share their prepared source with buffers.

Recovery validation, final disk/source comparisons, remaining document callers,
metadata growth, saved-source/history allocation and production latency/memory
checks remain unfinished. Physical PWA and folder-permission checks remain open.

Strict optimized frontend WASM lint passes. All 20 editor library tests and the
17 inventoried recovery/read component contracts pass in macOS and Linux Chrome,
including both real filesystem adapters. Extended review assertions verify shared
document/buffer source and fresh undo history; admission checks retain typed
capacity failures. Existing stale disk/editor/account/cancel, hydration, permission
retry, save and normal/lossy read contracts remain covered without changing any
readiness or runner deadlines. [Browser evidence](editor-performance/cooperative-review-browser.jsonl)
records source/artifact hashes, selection inventory and complete output.

## Production boundary measurements and neutral paint

The release bundle at `121fbbb` was measured in isolated Linux Chromium
154.0.8037.92, with four CPUs, 10 GiB container memory and 1 GiB shared memory.
The frontend WASM SHA-256 is
`0082cabf3ffcbb39e924507e66a2c230217e889e8b90a0918abfb4f69f4f6aec`;
the backend WASM SHA-256 is
`d6c06e54fb13fad2de610eca312726c538be8af03ecbdf10aa320b55eff02aa9`.
[Unwrapped](editor-performance/current-production-boundaries-unwrapped.jsonl)
and [wrapped](editor-performance/current-production-boundaries-wrapped.jsonl)
records contain three repetitions per case and project mode, 36 samples total.
Every sample passes the strict peak/final Chrome PSS checks and unchanged
rendering deadlines. Some intermediate or pre-load snapshots remain unavailable
and are explicitly null; the records do not imply continuous memory coverage.
Local cases use database recovery fixtures and do not prove filesystem permissions.

The cases contain 8,388,416 bytes, 99,999 short lines, and a 1,048,572-byte
Unicode line. Near-byte-limit input takes 157–199 ms; long-line input takes
707–789 ms. Peak Chrome PSS across these cases is roughly 688–875 MiB.
The input occurs at the current scrolled native window; complete-source input
verification is explicitly false in these measurement records. These observations
do not complete the responsiveness/memory or physical PWA gates.

The [wrapped long-line profile](editor-performance/current-production-long-line-profile.jsonl)
shows that neutral text paint waits for a pending worker reply: input-to-paint is
706 ms, with about 549 ms of main-thread idle time in that interval. Profiling
adds overhead and is diagnostic evidence rather than a normal timing baseline.
An experimental change to update neutral paint before worker completion was
withheld: the complete browser group failed highlight coalescing, localized
wrapped-row reuse and file-switch cancellation assertions. Removing retention
also caused multi-second near-byte-limit input and increased process memory.
The remaining fix must distinguish genuine neutral documents from temporary
neutral paint for grammar-supported documents, while retaining the bounded
terminal-fallback mapping. Existing production retention remains unchanged.

Memory sampling retries an incomplete process-tree snapshot up to three times,
without repeating editor operations. Persistent missing PSS remains a failure
under `--require-pss`; partial process coverage is never reported as complete.
Four deterministic sampler tests cover complete descendants, process retirement,
persistent unreadable children and platforms without `/proc`.

## Pending plain-document paint

The shared editor facade now allows current plain-document paint while a worker
reply is pending. It continues to retain grammar-supported generations, including
temporary neutral grammar paint, and keeps terminal fallback's bounded native
mapping until complete row preparation. No local/remote feature path was added.
A deferred-reply regression verifies source, native mapping and visible new text
before reply publication in both project modes, wrapped and unwrapped. All 172
editor tests pass across six independently inventoried Linux browser runs, including
both font matrices and the coalescing, row-reuse and file-switch contracts that
rejected the earlier experiment ([evidence](editor-performance/neutral-paint-browser.jsonl)).
All 128 native frontend library tests and strict optimized frontend WASM lint pass.

The production frontend WASM SHA-256 is
`bb485a2f9dda9bfbcd5b18c0e86c2c08c5440dab9b89b6a601e0edf72b8f37c6`.
The backend and container constraints match the preceding baseline. The complete
Rust boundary matrix retains three repetitions per case/mode/wrapping setting
([unwrapped](editor-performance/neutral-paint-rust-unwrapped.jsonl),
[wrapped](editor-performance/neutral-paint-rust-wrapped.jsonl)); all 36 samples
pass peak/final PSS checks and existing rendering deadlines. Rust long-line input
still takes 714–771 ms. Near-byte-limit input takes 155–221 ms. Those costs remain
open; this change does not complete the responsiveness/memory gate.

The measurement tool's new `--file-name` option selects the actual production
language policy without changing the source workload or default Rust fixture.
The 1,048,572-byte Unicode line was also measured as `measure.txt`, with three
repetitions per mode and wrapping setting for each release bundle.

| Plain-file input | Baseline median / range (ms) | Current median / range (ms) |
| --- | ---: | ---: |
| Unwrapped | 143.2 / 140.1–146.4 | 126.0 / 123.8–150.2 |
| Wrapped | 151.3 / 149.2–165.7 | 151.2 / 143.6–158.0 |

Records: baseline [unwrapped](editor-performance/plain-neutral-before-unwrapped.jsonl)
and [wrapped](editor-performance/plain-neutral-before-wrapped.jsonl); current
[unwrapped](editor-performance/plain-neutral-after-unwrapped.jsonl) and
[wrapped](editor-performance/plain-neutral-after-wrapped.jsonl). Peak PSS spans
roughly 652–724 MiB across these runs, with no demonstrated memory improvement.
The wrapped timing difference is immaterial. These are scrolled native-window
input measurements, with complete-source verification explicitly false. Deferred
worker tests prove early neutral publication; production timings do not prove
that every large wrapped input avoids worker or geometry waits.

## Parser budget rejection and worker reply diagnostics

The Rust Unicode long-line worker trace shows fixed parser work-budget exhaustion,
previously reported as cancellation. The shared `parse_step` now returns `TooLarge`
for that limit and preserves `Cancelled` for stopped work; the check limit remains
4,096 and cumulative across yields. Explicit cancellation takes precedence over
budget exhaustion. Both synchronous and cooperative drivers use this policy.
Rejected source remains editable through the existing neutral fallback, and a
small valid replacement restores grammar preparation. No transport or mode policy
was duplicated. Trace-only reply diagnostics now retain status, analysis presence,
highlight-row count and message UTF-16 length, without copying source into logs.

The [preceding production trace](editor-performance/parser-budget-before-worker.jsonl)
contains `Cancelled` replies in both modes. The
[current trace](editor-performance/parser-budget-worker.jsonl) contains `TooLarge`
for cold and input requests in both modes, with successful paint/input and
peak/final Chrome PSS checks. The current frontend WASM SHA-256 is
`63a420c2c678783b02c973f2b429faa091f3c21357bb846adef13893f79c6f06`;
the backend and container constraints match the preceding measurements.
Input-to-paint remains 714–719 ms, including roughly 575–585 ms between worker
request and reply. Tracing adds overhead; these are diagnosis samples rather
than a latency improvement claim. Avoiding repeated rejected parsing, neutral
layout identity invalidation and wrapped/memory costs remains open.

All 468 native core tests, all 173 editor browser tests (including both font
matrices), strict native core lint and strict optimized core/frontend WASM lint
pass. [Verification evidence](editor-performance/parser-budget-browser.jsonl)
includes exact artifacts, source hashes, complete native output and six independently
inventoried browser runs. The new browser regression checks rejection, complete
source retention, native mapping, explicit cancellation and recovery in both modes.
The [preceding checkpoint](editor-performance/grouped-ci-c42bc47.json) passed
all five CI jobs; current full CI remains required.

The subsequent [plain-paint CI run](editor-performance/grouped-ci-3afca61.json)
failed wrapped startup readiness, the long wrapped cursor probe bound and a
horizontal fragment cleanup assertion. The exact 50-test selection passes on
the current arm64 artifact with two CPUs and unchanged deadlines
([record](editor-performance/ci-3afca61-group-current.jsonl)). Hosted x64 failure
remains unresolved. Check whether the first startup timeout leaves probe/state
behind and contaminates later assertions; the local selection pass does not
prove either cause or a fix. Current CI and reliable repeated verification remain
required.


## Initial native context readiness

Initial input binding now distinguishes a document still being prepared from an
unneeded binding. The shared editor facade supplies this result; the component
retries on document publication and stops after binding or a terminal fallback.
Both workspace modes use the same lifecycle and retain source/selection ownership.
The existing ownership contract now checks missing-document readiness before
preparation in both modes. Near-limit startup tests also log readiness metadata
once near their unchanged three-second deadline to diagnose future failures.

All 173 editor browser tests pass on Linux arm64 with two CPUs, unchanged
deadlines and both font matrices ([record](editor-performance/native-startup-retry-browser.jsonl)).
All 128 native frontend tests, strict optimized frontend WASM lint and the
production frontend build pass. This does not establish the cause of the previous
hosted x64 startup failure. The preceding checkpoint `87df822` passed all five
CI jobs ([receipt](editor-performance/grouped-ci-87df822.json)) before this
lifecycle change. Current hosted verification and repeated reliability checks
remain required; responsiveness, memory and physical PWA gates remain open.


## Neutral rendering identity

The shared editor facade now supplies one neutral rendering identity for plain
fallback rows, which borrow text from the current immutable projection. Completing
the fallback token table no longer changes geometry identity when the row styling
is unchanged. Grammar token identities remain distinct. The component separately
retains complete-row readiness and uses preparation state for initial immediate
paint, preserving pending-source retention and file-switch paint cancellation.
Source, project, account, font and projection ownership checks still apply.

The full 173-test editor browser suite passes on two-CPU Linux arm64, including
both font matrices, burst coalescing, localized wrapped-row reuse, bounded native
input, source retention and file-switch cancellation
([record](editor-performance/neutral-render-identity-browser.jsonl)). The warm-cache
contract compares the rendering identity that owns measured geometry; its readiness
requirement and all paint/probe count limits remain unchanged. The existing
fallback contract checks identity before and after completion in both modes.
All 128 native frontend tests, strict optimized frontend WASM lint and the
production build pass.

The preceding `3017d5a` checkpoint passed all five hosted CI jobs
([receipt](editor-performance/grouped-ci-3017d5a.json)), following the green
`87df822` checkpoint. This cache change still needs current hosted CI; repeated
release/PWA verification and the broader responsiveness/memory gates remain open.

Sixteen fresh production samples cover near-byte-limit and 1-MiB Unicode long-line
files, both modes, wrapping on/off and two repetitions per combination
([record](editor-performance/neutral-render-identity-production.jsonl)). Peak/final
Chrome PSS checks pass in every sample. Near-byte-limit input takes 148–177 ms;
long-line input takes 674–713 ms, with roughly 740–860 MiB peak Chrome PSS across
these samples. These are scrolled native-window measurements; complete-source
after-input verification is explicitly false. They do not establish a latency or
memory improvement and still fail the broader responsiveness gate. Repeated
parser rejection waits, styled boundaries, beginning edits and physical PWA/input
verification remain required.


## Rejected-source paint and wrapped geometry restart

The shared editor facade now publishes neutral edits while retrying grammar
analysis after a typed `TooLarge` result. The prior rejection must match project,
path, epoch, read revision, account and tab width; cancelled results and foreign
ownership cannot enable early paint. Grammar analysis still runs and a later valid
source recovers real highlighting. Local and remote projects use this same policy.

The first candidate exposed a wrapped geometry stall: completing syntax preparation
invalidated an unfinished measurement job, but unchanged neutral token identity
prevented its replacement from starting. The component's in-flight batch key now
includes the facade's preparation revision. Existing completed neutral geometry
remains reusable, and the source/syntax ownership checks remain intact. A regression
holds animation frames across the worker reply and proves geometry completes with
exact complete source and native mapping in both modes.

All 174 editor browser tests pass on two-CPU Linux arm64, including both font
matrices and the new restart regression
([record](editor-performance/rejected-source-browser.jsonl)). The literal-context
contract now retries the optional bounded syntax query within its existing
three-second readiness deadline; all literal/code-region and color assertions
remain unchanged. All 128 native frontend tests, strict optimized frontend WASM
lint, production build and four measurement-memory tests pass. Trace-only failure
diagnostics capture bounded geometry/runtime metadata and recovery source length
without logging document text or extending readiness deadlines.

The preceding `e2ec3f5` checkpoint passed all five hosted CI jobs
([receipt](editor-performance/grouped-ci-e2ec3f5.json)). Current hosted CI and
repeated release/PWA, styled-boundary, beginning-edit, responsiveness and memory
checks remain required.

Sixteen fresh release samples cover near-byte-limit and 1-MiB Unicode long-line
Rust fixtures, both modes, wrapping on/off and two repetitions per combination
([record](editor-performance/rejected-source-production.jsonl)). Every sample
passes the existing peak/final Chrome PSS requirement. Near-byte-limit input takes
168.5–209.9 ms; long-line input takes 108.5–154.0 ms, compared with 674–713 ms in
the preceding neutral-identity cohort. Peak PSS is 741–858 MiB. Startup remains
1.45–3.95 seconds; byte-limit latency and memory show no material improvement.
These are scrolled native-window observations, with complete-source-after-input
verification explicitly false and no actual String styling. They do not validate
styled boundaries, beginning edits, physical PWA/FSA permissions or the full
responsiveness/memory gate. Measurement limits and readiness deadlines are unchanged.


## Sharing prepared source with the saved baseline

The shared core document now retains its saved baseline as the same immutable
source allocation used by complete document preparation. Initial publication and
marking the current version saved no longer allocate another full source string;
clean dirty-state checks can prove equality by source identity. Acknowledging an
external write still checks exact bytes and retains the version actually written,
so a late acknowledgement cannot mark newer typing clean. External versions that
match neither retained source still require their own baseline allocation.

The first candidate broke the existing unused-projection contract by forcing a
source copy on a standalone document's first edit. The corrected edit path releases
the unused projection, then detaches only the saved baseline if it is the sole
other owner and no weak source handles exist. The mutable source keeps its buffer
and insertion headroom. External snapshots and composition retain the original
source and use the existing source-detach path. This defers a necessary baseline
copy to the first standalone edit; it does not eliminate source copies from edits
with retained versions. No adapter or UI mode branch is involved.

Four fresh styled wrapped beginning-edit samples at `43f66c9` retain actual String
paint and verify exact complete recovered source after input
([baseline](editor-performance/styled-baseline-43f66c9.jsonl)). Viewport paint takes
225–247 ms and complete geometry 1.10–1.88 seconds. A bounded [diagnostic trace](editor-performance/styled-baseline-trace-43f66c9.jsonl)
still observes 71 input probe entries, with 69 paragraph layout batches; this
change targets source allocation and does not claim to fix wrapped probe reflow.
Current source-sharing regression tests cover prepared-source identity, detached
edits, retained saved versions, late acknowledgements and undo. All 469 native
core tests pass, including unchanged projection, history, recovery and composition
contracts. All 174 editor browser tests (both font matrices included), 128 native
frontend tests and strict native-core/optimized frontend WASM lint pass
([record](editor-performance/shared-saved-source-browser.jsonl)).

Sixteen fresh release samples cover actual near-1-MiB String styling and
near-8-MiB files, both modes, wrapping on/off and two repetitions per combination
([record](editor-performance/shared-saved-source-production.jsonl)). Every sample
verifies source caret zero before insertion, exact complete recovered source after
insertion and the existing observed peak/final Chrome PSS requirement. Resource
limits remain four CPUs, 10 GiB container memory and 1 GiB shared memory; source
checks do not prove physical FSA permissions or real IME input.

| Fixture | Wrapping | Startup ms | Input viewport ms | Input complete geometry ms |
| --- | --- | --- | --- | --- |
| Styled long line | On | 2588–2638 | 251–262 | 1206–1247 |
| Styled long line | Off | 3343–3980 | 148–151 | 148–152 |
| Byte limit | On | 1513–1788 | 170–203 | 170–203 |
| Byte limit | Off | 1553–1675 | 141–182 | 141–182 |

In the wrapped styled cohort, cold committed WASM memory is 32.31 MiB, versus
33.44–34.56 MiB in the preceding same-fixture baseline; after input it is 40.94 MiB,
versus 42.06–43.13 MiB. These are linear-memory high-water observations, not
per-object allocation measurements. The core identity regression directly proves
that the prepared saved baseline shares source. Observed peak Chrome PSS across
all current samples is 697–854 MiB; variation does not establish a process-memory
improvement. Wrapped styled complete geometry remains over one second, startup
remains multi-second, and no latency improvement is established. The byte-limit
beginning edits additionally verify complete source, so their positions differ
from the preceding scrolled-window cohort. Full responsiveness, memory, unsupported
shaping and physical PWA release gates remain open.


## Immutable source through asynchronous saves

`EditorActions::prepare_save` now returns the same `EditorText` source retained by
the normalized document and workspace buffer. The shared workspace save workflow
borrows its bytes for `Workspace::write`, then acknowledges its immutable source
allocation through `Document::mark_saved_source`. Previously converting the result
to `String` copied the full source, and acknowledging a late write could allocate
a second saved-version copy. Local filesystem and remote HTTP primitives retain
the same contracts; no UI or mode-specific save policy was added.

Acknowledgement records the version actually written, including when newer edits
have detached the current source. Existing account, workspace-root, bridge, project
and file-switch ownership checks remain unchanged. Borrowed external disk versions
still use the exact-byte `mark_saved_version` path. Source identity regressions
cover prepared save payloads, late acknowledgements, undo and composition
cancellation. This is an allocation/lifetime change; it makes no new whole-app
latency or process-memory claim. Full responsiveness and physical-device gates
remain open.

The preceding rejected-source checkpoint `43f66c9` passed all five hosted CI jobs
([receipt](editor-performance/grouped-ci-43f66c9.json)). The baseline-sharing
checkpoint `8a5f1e3` also passed all five hosted CI jobs
([receipt](editor-performance/grouped-ci-8a5f1e3.json)); the current save change
at `dc49196` also passed all five hosted CI jobs
([receipt](editor-performance/grouped-ci-dc49196.json)). Later implementation
checkpoints still require their own hosted verification.

All 470 native core tests, 128 native frontend tests, strict native-core lint
(including tests) and optimized frontend WASM lint pass. Thirty selected browser
checks pass on two-CPU Linux arm64, including six editor save/recovery contracts
and the source-allocation regression
([record](editor-performance/immutable-save-source-browser.jsonl)). The save
ownership contract holds writes across root/file/account/bridge/project changes,
write failure and newer typing in both local filesystem and remote HTTP adapters.
Formatting and recovery-conflict checks retain their original assertions and
three-second readiness deadline. This selected run does not replace full hosted
CI, production timing or physical-device verification.

## Late save after a clean disk reload

A held-write regression reproduced a clean-state mismatch: reloading newer disk
content while an older save waited left the active editor marked clean after the
older write replaced the file. The document and retained buffer correctly differed
from the acknowledged source, but active and project-snapshot dirty flags did not.
The shared save completion now reconciles each matching view with the exact source
written, retaining account, root, project and file ownership guards.

The unchanged regression fails on the preceding implementation at local case 8
and passes after the correction. Ten scenarios per adapter cover the active reload,
switching away after reload, newer typing, write failure and ownership changes.
All 29 selected save-related browser checks, 128 native frontend tests and strict
optimized frontend WASM lint including tests pass
([failure/pass record](editor-performance/late-save-reload-browser.jsonl)).
This is a shared-state correction, not a performance measurement or physical
folder-permission verification; those roadmap gates remain open.

## Disk verification without persistence snapshots

The shared document now classifies fresh disk reads directly from its committed
source and baseline, using the same comparison and admission policy as persisted
recovery records. Composition previews remain excluded. Both automatic recovery
verification and the pre-save check use this path, avoiding two full-source copies
and persistence metadata capture on each verification. Completed writes reuse the
document source for saved acknowledgement and buffer publication; clean reloads
move the owned disk read into cooperative preparation instead of cloning it.

All 471 native core tests, 128 native frontend tests, native core lint including
tests, optimized frontend WASM lint including tests and 21 selected browser checks
pass ([record](editor-performance/live-disk-recovery-browser.jsonl)). The new
both-adapter contract checks clean reload and already-completed writes, shared
document/buffer/active source allocations, saved baselines and undo behavior.
The core contract compares live and persisted decisions across clean/dirty state,
composition, late saved baselines, missing files and capacity rejection.
Admission scans, persistence serialization, whole-app timing/memory and physical
permission/input verification remain separate unfinished gates.

## Prepared sources retained by recovered undo history

Recovery's single undo step now retains the complete prepared saved and draft
source allocations. It no longer clones both full files during final history
publication. Ordinary editing transactions keep owned replacement spans; their
source bytes are moved into history, with no new shared allocation per edit.
History grouping, byte accounting, eviction and replacement application stay in
the shared core engine. Both representations compare by exact text.

The new core contract checks the recovered transaction's source allocation
identity, its unchanged byte charge, later edits and independent snapshot
undo/redo. Existing parity contracts compare prepared recovery with ordinary
transaction restoration, including selections, folds, revision and saved state.
All 472 native core tests, 128 native frontend tests, strict native/core and
optimized frontend WASM lint, and all 175 editor browser tests pass
([record](editor-performance/shared-recovery-history-browser.jsonl)). The browser
inventory covers every editor test in six disjoint runs, including both font
matrices, with unchanged readiness and runner deadlines.

This removes the recovery-specific source copies; it is not a whole-app memory
or latency claim. Edits can still detach a source retained by immutable history.
At this checkpoint, recovery payload capture/preparation copies, admission
validation, full-source undo publication and physical-device gates remained
unfinished; the following section records the subsequent payload-sharing work.


## Immutable recovery payloads

Recovery capture, queued record clones and hydration preparation now retain the
same immutable saved and draft sources as the document. Clean buffers without a
document retain their existing source for both fields. Decoding wraps the owned
UTF-8 result without another full-source copy. The encoded JSON/storage format,
capacity checks, committed composition boundary, stale-scope guards and independent
saved/draft versions remain unchanged. Recovery transport still encodes and decodes
complete source strings; admission and final metadata validation remain work.

The core allocation contract checks pointer identity through capture, cloning and
restoration, exact encoded JSON compatibility, and later independent edits. Both
workspace adapters check source identity during recovery capture and cooperative
hydration. All 473 core, 213 backend/storage and 128 native frontend tests pass,
as do native, WASI and optimized frontend WASM lint. The complete frozen editor
inventory passes all 175 browser checks in six disjoint runs with unchanged limits
([verification](editor-performance/shared-recovery-payload-verification.json)).
A subsequent test-only change adds timeout diagnostics to the parser fallback
contract; its exact 51-test hosted selection passes locally on two-CPU Linux arm64
([diagnostic verification](editor-performance/shared-recovery-payload-diagnostic.json)).
This is not proof of a hosted x64 fix. The preceding `72b7a3e` checkpoint failed
its frontend browser job after a parser-budget fallback readiness timeout; later
probe/geometry failures may be related, but the evidence does not establish that
([CI receipt](editor-performance/grouped-ci-72b7a3e.json)). The preceding `0ec8cbe`
and `988f700` checkpoints passed all five jobs. Repeated current CI remains required.

The uninstrumented production frontend completed 32 beginning-of-source samples:
byte and line admission boundaries, plain and grammar-highlighted long lines,
both modes, wrapping enabled/disabled, two repetitions each
([complete samples and manifest](editor-performance/shared-recovery-payload-production.jsonl),
[ranges](editor-performance/shared-recovery-payload-production-summary.json)).
Every sample verified exact complete source after input and caret offset zero
before input, with complete renderer PSS snapshots. The backend is the unchanged
historical control artifact, and local cases use database recovery fixtures;
these measurements do not verify physical directory permissions. Multiple earlier
allocation changes separate this cohort from the earlier control, so these are
pipeline measurements, not an isolated causal comparison of payload sharing.

Cold viewport paint ranges from 1.11 to 4.44 seconds. The wrapped styled long-line
case paints the input viewport in 230–266 ms and completes geometry in
1.12–1.18 seconds; wrapped line-limit input takes 245–249 ms. Peak renderer PSS
ranges from approximately 684 to 823 MiB. These remain responsiveness and memory
work, despite successful source/caret checks. Byte-limit beginning-of-source
samples at this checkpoint retained the earlier nonzero vertical scroll
position; the later keyboard-motion section records its correction. Full-source undo publication, transport encoding,
cold/fallback geometry and physical PWA/IME/permission verification remain open.


## Final recovery validation from complete indexes

Final recovery restoration now uses the already prepared documents' source-owned
admission summaries and the draft's complete LF row count. It still validates
selection boundaries, fold order/overlap, source identity, capacity and fresh
preparation state. Mismatched prepared sources retain the original source-scan
validation and error precedence; they cannot prove validity for another payload.
Standalone CR continues to affect editor admission without becoming an LF fold
row. The persisted record validator retains its existing source-based behavior.

A new shared core regression compares ordinary and prepared recovery across
byte, line and individual-line capacity rejection, invalid Unicode selections,
invalid/crossing folds, mixed line endings, competing errors and mismatched
prepared sources. This removes repeated admitted-source scans from final
hydration, without changing persistence policy. Initial record validation,
metadata normalization, transport serialization and full-source undo publication
remain unfinished; no whole-app latency or memory improvement is claimed here.

All 474 native core and 128 native frontend tests pass. Strict native/core,
optimized frontend WASM including tests and backend WASI lint pass. All 15
recovery-related component checks and 20 editor library browser checks pass on
two-CPU Linux arm64 with unchanged deadlines
([verification](editor-performance/indexed-recovery-validation.json)). The browser
contracts include cooperative hydration cancellation, stale account/project guards,
disk conflicts, save failure/retry and source identity through both real adapters.
This is targeted verification of the changed path, not another complete editor
inventory or a hosted CI result; those wider gates remain open.


## Keyboard motion across admitted files

Boundary-size production measurements exposed a correctness gap: navigation used
the 2 MiB structure-analysis budget even for an admitted 8 MiB editor document.
The recorded beginning caret could already be zero before Ctrl+Home, masking the
rejected key while the viewport remained at the earlier scroll position. The
smaller virtualized-row fixture passes before the change; the larger admitted
fixture fails to reveal the document-end caret, and the shared core regression
fails with `SelectionError::TooLarge`.

Direct and queued basic motion now use the document's indexed editor admission.
Logical row descriptors come from cached source-owned projection/index metadata;
horizontal and boundary motion no longer computes an unused display column.
Direct grapheme cursors avoid segmenting the complete prefix on every arrow key.
Measured neighborhoods retain the immutable source allocation, validate grapheme
positions only in requested logical rows and use prepared sparse coordinates for
long rows. The browser's warm and cold adapters use the same shared constructor.
Fold, source, composition, queue order and measurement identity checks remain.

New core contracts cover admitted files above the structure budget, large folded
views, queued sparse neighborhoods and exact source allocation identity. Complete
Unicode segmentation independently checks direct motion at every character boundary
in representative combining, emoji, regional-indicator and Indic fixtures.
Sparse layout validation is checked against complete segmentation for large Unicode
rows, CRLF interiors and a large indivisible grapheme; a dense admitted caret list
crosses the existing sparse-query batch limit without dropping positions.
The new browser contract covers both workspace modes and wrapping settings,
virtualized origin/end rows, extended selection and queued page movement.

This does not complete all responsiveness work: initial projection/native CRLF
normalization, long word/context traversal, synchronous selection publication,
unsupported geometry/touch fallbacks and physical-device verification remain open.
Structural selection commands retain their separate analysis budget. Current
full hosted verification is still required.

All 479 native core tests, 128 native frontend tests, native/core lint, optimized
frontend WASM lint including tests and backend WASI lint pass. The complete frozen
editor inventory passes all 176 browser checks in six disjoint two-CPU Linux arm64
runs with unchanged readiness, runner and probe limits
([verification and before/after identities](editor-performance/admitted-keyboard-motion-browser.json)).
The admitted large-file browser regression passes in both modes with wrapping
on/off, including source-end/origin viewport reveal, directional selection and
queued page motion. The preceding source-sharing checkpoint `ba33f5d` passed all
five hosted jobs ([receipt](editor-performance/grouped-ci-ba33f5d.json)); current
motion changes still require their own complete and repeated CI verification.

The uninstrumented production build passes eight byte-boundary beginning-input
samples: two repetitions per workspace mode with wrapping on/off
([complete samples and manifest](editor-performance/admitted-keyboard-motion-production.jsonl)).
Every sample retains the complete source, proves caret offset zero and has input
scroll coordinates `(top: 2, left: 0)`, replacing the preceding cohort's remote
viewport position far down the file. Cold viewport paint still takes
1.43–1.54 seconds, input viewport paint 152–171 ms and peak renderer PSS
approximately 775–825 MiB. These are correctness/remaining-cost measurements,
not an isolated memory-improvement claim. The backend remains the unchanged
historical control; local database recovery fixtures do not verify physical folder
permissions. This checkpoint fixes navigation and removes repeated-prefix layout
validation/source copies; the broader responsiveness/device gates remain open.


## Recovery decoding without encoded-field copies

The shared recovery codec now asks the JSON reader for a string view and decodes
that view directly into the owned UTF-8 source. Canonical base64 fields contain
no JSON escapes, so slice/string readers can lend the original input bytes instead
of allocating an intermediate encoded `String`. Readers which already own a
value, and escaped JSON fields, remain supported through the same visitor.
Encoded-length and decoded-length limits, base64 validation and invalid UTF-8
rejection remain in the shared codec; the serialized/storage format is unchanged.
HTTP persistence in both workspace modes and database recovery use this codec.

The allocation contract records the exact borrowed field addresses and lengths
inside the real JSON input, verifies source/control/Unicode equality and checks
that decoded sources remain independently owned after the input and original
payload are dropped. Compatibility checks include owned JSON values, escaped
fields and wrong field types; existing malformed base64, UTF-8, metadata and
capacity contracts remain in place. Test-only tracing records addresses/lengths,
with no production tracing or retained source pointers.

All 481 core, 213 backend/storage and 128 native frontend tests pass, as do strict
native, optimized frontend WASM including tests and backend WASI lint. The 15
recovery-related component contracts and all 20 editor library browser contracts
pass on two-CPU Linux arm64 with unchanged deadlines
([verification](editor-performance/borrowed-recovery-decoding.json)). This targeted
verification covers both real workspace adapters, cancellation, stale ownership,
disk conflict and save/retry behavior; it is not a complete editor inventory or a
whole-app memory/latency measurement. Encoding, JSON/network buffers, decoded
source ownership and remaining publication costs still
require work.

The preceding `2301c26` final-index validation checkpoint passed all five hosted
jobs ([receipt](editor-performance/grouped-ci-2301c26.json)), following the fully
green `ba33f5d` source-sharing checkpoint. These runs do not establish the earlier
parser-fallback failure's cause. Current motion/decoding checkpoints still require
their own complete hosted and repeated verification.

## Recovery metadata checks without normalization copies

Persistence checks now validate borrowed selection offsets directly. They preserve
the count limit and Unicode boundary errors, while restoration still normalizes
and owns its final selections. Persisted folds must already be normalized: a
linear check rejects invalid bounds, unordered/duplicate headers and crossing
ranges, retaining only an ancestor-end stack bounded by nesting depth. It does
not clone or sort the entire fold list. Source-size and selection failures still
precede fold failures, and folds still use LF logical rows.

The existing fold normalizer is an independent oracle for every list of up to
four ranges over four endpoint positions, checked against six document lengths
(419,430 cases). Selection checks cover every offset pair around Unicode and
CRLF source, overlapping/reversed selections and invalid count limits without
mutating the payload. This changes the shared recovery validator used by both
workspace adapters and backend/storage persistence; source decoding, restoration,
history and the storage format are unchanged. Deep ancestor-stack allocation,
final selection/fold normalization and whole-source publication remain costs.

All 483 core and 213 storage/backend tests pass, with strict native core,
frontend WASM including tests and backend WASI lint. The optimized browser suite
passes 15 recovery component contracts and 20 editor library contracts on
two-CPU Linux arm64 with unchanged deadlines
([verification](editor-performance/borrowed-recovery-metadata.json)). These
targeted contracts cover both adapters, stale/account ownership, conflicts and
retry; they do not establish full editor CI reliability or whole-app memory gains.

## Prepared recovery versions during undo and redo

The recovered transaction now retains both complete immutable source/index
versions. Replay compares the current and target text using the existing change
boundaries, rebases folds with those same boundaries, invalidates the derived
projection and switches to the prepared version. It does not copy source or
reconstruct row coordinates. Selection restoration, revisions, composition
guards, saved-baseline ownership and history source-byte budgets remain unchanged.
Ordinary edit transactions do not retain these complete versions.

This trades retention of the saved version's prepared index for avoiding replay
reconstruction; it is not evidence of lower total memory. The recovery step's
source bytes remain charged to the existing history budget, while index retention
is bounded by the admitted source versions. Replacing the redo branch releases
both retained indexes. The first ordinary edit after recovery also detaches the
retained draft index through the existing copy-on-write path; bounding that work
and measuring its cost remain required. Native contracts assert exact source/index identities
through ordinary edits and cloned snapshots, and compare folding/revision/source
behavior with ordinary full-replacement transactions on empty, clean, Unicode,
CRLF, long and middle-change documents. The browser recovery contract asserts
source identity after undo/redo in both workspace modes.

Common-prefix/suffix comparison, fold mapping, frontend publication and native
normalization still need bounded preparation. Full admitted-workload latency and
memory evidence remains required before completing the editor goal.

The first undo verification passed all 484 core tests and strict native/WASM/WASI
lint, but its full editor browser verification was incomplete: the first 52-test
group passed 51 tests and failed the admitted-file document-end navigation
deadline ([failure evidence](editor-performance/prepared-recovery-undo-browser-failure.json)).
The other five groups did not execute. Source selection, scroll, binding and
geometry diagnostics were then added with the existing assertions and deadlines
unchanged. This initial attempt did not qualify the checkpoint for commit.

The same 52-test group reproduced the failure with unchanged deadlines and
failure-only state diagnostics
([diagnostic evidence](editor-performance/navigation-reveal-diagnostic.json)).
In wrapped Remote mode the source selection reached byte 2,305,056 exactly, with
no queued motion, bound native input, no pending geometry and one mounted editor.
Scroll remained at 907,647 of 1,296,638 pixels (the starting 70% position). This
distinguishes a lost viewport reveal from source-motion rejection; the underlying
paint availability timing is not yet proved. A bounded frame retry guarded by
source, selection, project/read/account scope and unchanged scroll intent was added,
including a focused retry/cancellation contract. At that point it was not yet a
verified fix. The preceding navigation checkpoint did pass all five hosted jobs
([receipt](editor-performance/grouped-ci-3d7eb5d.json)); that success does not negate
these local failures or establish reliability.

The guarded retry passes its focused cancellation contract and all 21 editor
library contracts, followed by the complete previously failing 52-test navigation
group. The next 51-test group passed 49 tests and failed parser-backed reindent
(HTML source remained unchanged) and file-switch highlight counts (two extra
publications). The remaining 53 tests did not run
([full-run evidence](editor-performance/navigation-reveal-retry-browser-failure.json)).
Both failing tests pass individually in fresh browsers with the same frozen
artifacts and deadlines
([isolated diagnostics](editor-performance/reindent-file-switch-diagnostic.json));
these are diagnostics, not a substitute for complete verification. Cold command
syntax preparation currently has a 12 ms deadline and falls back to lexical
reindent if no parsed structure is available. The HTML menu also disables
reindent based on its outer language, despite embedded-body support. Parser-aware
command readiness and container capability checks were still implementation work.
Neither parser cancellation nor failed-mount contamination is yet proved as the
shared-group failure's cause. The decoding checkpoint passed all five hosted jobs
([receipt](editor-performance/grouped-ci-2b1e482.json)); that run did not verify the
undo/retry work as a full editor checkpoint.

Cold Reindent now uses a shared facade path which preserves the requested edit
until complete structure is available. The fast path reuses current prepared
structure; otherwise it cooperatively admits the source and asks the same core
`SyntaxWorker` service used by browser workers through a yielding task adapter.
The command applies through the same transaction implementation as immediate
editing commands. It checks source allocation, document revision, all selections,
rules, composition and project/read/account ownership between batches and before
publication. Superseded requests return without editing; unavailable structure
returns a typed error. HTML/Markdown containers expose reindent for their parsed
code bodies while preserving markup and prose.

This avoids cold parser cancellation being interpreted as a lexical no-op for an
explicit reindent request. It does not establish the precise cause of the earlier
shared-group failure. The former semantic contract now invokes the same awaited
facade used by the menu, with its expected output and undo/redo checks retained.
New contracts cover superseded requests in both modes, actual HTML menu execution
and one undoable commit, and admitted editor sources above the structure budget.
The complete optimized editor inventory passes all 181 tests, including both
independent font matrices ([full-run evidence](editor-performance/cold-reindent-full-browser.json)).
The implementation source was then retained while the semantic fixture added
Markdown code-fence reindent, exact prose preservation and undo. The rebuilt
artifact passes both complete groups containing the earlier navigation, reindent
and file-switch failures: 104 tests with unchanged readiness deadlines and CPU
limits ([repeated-group evidence](editor-performance/cold-reindent-repeat-browser.json)).
This is a complete baseline plus repeated current groups, not a second complete
181-test run of the rebuilt artifact. All 484 core tests, strict native/WASM/WASI
lint and formatting also pass. The preceding metadata-validation checkpoint
passed all five hosted jobs ([receipt](editor-performance/grouped-ci-be926f5.json));
the current checkpoint still needs its own hosted CI.

These runs verify the cooperative command and guarded reveal behavior without
establishing the exact cause of every earlier shared-group failure. Other
parser-aware commands still use their existing immediate fallback; request/reply
serialization, source/index publication, command preparation memory, broader
responsiveness and physical-device checks remain work.

## Embedded comment command readiness

Explicit line/block comments now use the same command-preparation facade as
Reindent when the registered language provider declares embedded languages.
Single-language comments retain immediate execution, including SQL without a
grammar and admitted files above the structural analysis budget. This selects
preparation from shared provider capabilities, not filesystem mode or a second
comment implementation. All edits still use the existing core transaction path.

The facade captures source, project/read/account scope, document revision,
selections, indentation and command revision when a request is constructed.
Supersession is checked before its first poll, between preparation batches and
before applying the transaction. Newer commands cancel older pending actions,
including an Undo with no history/text change. UI tasks construct the request
before queuing rather than acquiring scope after the task starts. Workspace reset
invalidates the command revision alongside existing preparation ownership.

Expanded browser contracts cover Reindent and both comment actions, source/file
changes before the first poll, newer unchanged-text commands, existing stale
ownership/selection/composition cases, SQL and above-budget immediate comments,
and actual HTML keyboard/Markdown menu commands with one undoable transaction.
Existing mixed-language comment tests now invoke the awaited facade. The nine
focused contracts pass ([evidence](editor-performance/comment-readiness-focused-browser.json)),
followed by all 183 current editor browser tests, including both independent font
matrices ([complete verification](editor-performance/comment-readiness-full-browser.json)).
The same sources pass 128 native frontend tests, strict WASM lint and formatting.
Current-head hosted CI remains required. Selection-specific menu capabilities and
other structural-command readiness remain roadmap work, alongside the broader
responsiveness, memory, device and release checks.

## Hosted parser fallback failure

The `9d4f888` checkpoint subsequently failed its hosted editor browser partition
([receipt](editor-performance/grouped-ci-9d4f888.json)). The first two editor
groups passed all 105 tests; the third passed 46 and failed six. The two font
matrices and 22 library tests were not reached. The other four CI jobs passed.

The first failure was the existing parser-budget fallback contract. Its diagnostic
showed `TooLarge`, no pending syntax, a bound native input and one mounted editor,
but no ready paint within the unchanged three-second deadline. This proves typed
rejection reached publication; it does not establish why presentation stalled.
Five later layout/navigation checks also failed, and contamination from the first
failed fixture is still unproven. Earlier green runs and isolated local passes
do not close this reliability requirement. Failure-only diagnostics now include
native geometry, fallback publication, measured rows and in-progress row jobs.
The subsequent embedded-comment checkpoint `cc019c1` passed all five hosted jobs
([receipt](editor-performance/grouped-ci-cc019c1.json)). This establishes that
checkpoint's hosted result, without proving why the earlier paint check failed.

## Selection-aware comment capabilities

The shared editor facade now computes line/block comment eligibility from every
selection's start language in completed embedded-language structure. Mixed
selections require support everywhere, matching atomic core comment semantics.
Pending containers retain provisional outer-language eligibility; invoking their
actions still uses cooperative preparation. Capacity-rejected container actions
are disabled. Single-language actions reuse the existing marker registry.

The cache read requires the exact immutable source allocation, language, key and
tab width, without parsing or changing retention order. Core coverage checks
unprepared and mismatched entries, equal bytes with different ownership and
unchanged retention. Browser coverage exercises Python, Rust, JSON and prose,
mixed/reversed selections and stale account ownership, without new worker requests.
The capability check and existing parser-budget fallback both pass in Local and
Remote ([focused evidence](editor-performance/selection-capabilities-focused-browser.json)).
All 628 parser-enabled core tests, 128 native frontend tests, strict core/WASM/WASI
lint and formatting pass. All 184 current editor browser tests pass in complete,
disjoint groups of 22/54/53/53/1/1, including both font matrices
([complete evidence](editor-performance/selection-capabilities-full-browser.json)).
This local success does not establish the earlier CI failure's cause or close
the hosted reliability, responsiveness and physical-device gates.
The selection-capability checkpoint `71997bc` subsequently passed all five hosted
jobs ([receipt](editor-performance/grouped-ci-71997bc.json)); verification of newer
changes and repeated readiness still remain required.

## Explicit structural navigation preparation

Selection expansion and bracket jumps now use the same action ownership and
cooperative syntax preparation as Reindent and embedded comment actions. The
shared action captures source allocation, parser scope, all selections, document
revision, indentation and intent before its future is polled. Keyboard and menu
entry points prepare current syntax before applying navigation; unsupported
grammars retain immediate lexical behavior. Passive bracket decorations keep
their nonblocking fallback. Explicit navigation and selection intent cancels an
older pending action even when the newer command leaves its selection unchanged.

New contracts cover parsed Python suite expansion and bracket pairs, held
background syntax with actual keyboard/menu actions, unsupported plain/SQL
navigation, and pre-poll/source/selection/secondary/file/project/read/epoch/account/
rules/composition/disposal supersession. The cancellation matrix exercises both
expansion and bracket jumps. Existing Reindent/comment contracts remain part of
the focused regression set. The first optimized run passed seven of eight
contracts ([record](editor-performance/structural-navigation-initial-browser.json));
its only failure expected bracket pairing in plain prose, contrary to the existing
core navigation contract. The subsequent run passed the same seven contracts but
exposed the equivalent incorrect expectation for SQL
([record](editor-performance/structural-navigation-fallback-browser.json)). Both
languages are excluded from the existing bracket whitelist. The fixture now
preserves their no-pair behavior while checking immediate lexical selection
expansion. Production behavior remains unchanged by these corrections. All eight
focused contracts now pass
([record](editor-performance/structural-navigation-focused-browser.json)). The complete independently inventoried editor suite subsequently passed all
187 tests in six disjoint runs (22/55/54/54/1/1), including both independent font
matrices ([record](editor-performance/structural-navigation-full-browser.json)).
The runner exited successfully and frozen source/artifact hashes match the current
checkpoint. The production changes pass strict WASM lint, formatting and 128
native frontend tests. Its own hosted CI remains pending. This section does not
claim completion of ordinary-input readiness, responsiveness, hosted reliability
or device gates.
