# Roadmap

What's left, grouped by how soon it's coming: **Next** (queued up), **Later**
(planned, not yet started). Finished work —
phases 1 through 14, telemetry, hardening, streaming, `/test`, database-backed
theme and prompt history, frontend performance & polish, app branding and the chat
welcome, database-backed agent skills and skill import/export — moved to [CHANGELOG.md](../CHANGELOG.md).
[Host administration](host-administration.md), with automatic deployment-host SSH
addresses and optional advanced connection overrides, structured agent questions and
[Monitors](monitors.md) are implemented, with an active-only status-line flyout;
their shipped behavior is in the changelog. Private host terminal replies use
browser-to-bridge encryption; the Host flyout offers setup navigation and retains
its layout during refresh.
The [Git pane](git.md) includes commit history, staged-only commits and repository
actions, with retained refresh results, primary-model drafting, resizable history
inspection and shared Git-status icons in both project modes.

## Next

### Remaining manual verification

The model setup and code hardening follow-ups are implemented. Native and browser
contracts cover both modes, failures, stale results and fallbacks. Chat and agent
reply budgets now use remaining context capacity per request unless explicitly
limited; compaction keeps a separate planning reserve. Docker checks
cover authenticated WebSocket PTYs, real-model streaming, remote file writes,
host-owned Git workspaces, supervisor shutdown and bridge-disabled SSE fallback.
[Reload recovery](reload-recovery.md) covers mid-run and multi-window checks, plus
the user-confirmed restoration of an OS-picked local folder.

The following checks still require hands-on device or environment testing:

- Browser-close goal continuation on a real paired local host and a real model.
- First-paint theme and both themes visually; terminal dock hide/show.
- Docker browser terminal and Files/Changes interactions.
- Git pane polish on live local/remote repositories: cold primary-model drafting
  and configured transport timeouts, dense file timelines and pointer resizing.
- A phone on the LAN; podman/systemd.
- Editor IME composition, paste and caret behaviour with a real input method.
- Windows runtime process cleanup. The Windows adapter compiles without TLS locally;
  the Windows CI job checks the full TLS build.

### Full code editor: editing, structure and navigation

Build out the existing syntax-highlighted Edit view into a daily-use code editor.
Keep numbered Inline/Split diffs, previews for supported formats (Markdown/images/PDF
and plain-text documents),
including Markdown gutters on changed blocks/items/rows and inline prose differences for Git HEAD and pending agent edits,
Find, pending-edit review and agent editor
context intact. Editing must work without a host language server in both modes.

The everyday editing baseline is implemented. The goal remains open until the
performance, input/device and verification requirements below are finished.
Completed implementation details belong in [editor controls](editor.md),
[performance evidence](editor-performance.md) and the changelog; this section
tracks the remaining work rather than every optimization already shipped.

**Implemented baseline** (shared local/remote behavior, with automated coverage):

| Area | Available now |
| --- | --- |
| Editing and history | Tab/space and indent/outdent; block-aware Enter; paired typing/deletion; grouped undo/redo; line move/duplicate/delete; comments; selected-line reindent; indentation-matching paste; EditorConfig save policies. |
| Structure and languages | Syntax highlighting, parser-backed folding, bracket matching and structural navigation; extensible Rust/WASM language support including Rust, TypeScript/TSX, Python, JavaScript/JSX, Java, C#, C++, PHP, Shell, C, Go, HTML, CSS, JSON/JSONC, YAML/YML, TOML, INI/EditorConfig, XML ecosystem configs and Markdown with inline/fenced code. Pending text uses current source and valid retained spans; unsupported source stays plain. |
| Navigation and review | Find/Replace; line numbers; horizontal scrolling and linked split scrolling; Edit/Inline/Split diffs; supported previews and Markdown change gutters/word differences; pending-edit review and agent context. |
| Tabs, appearance and recovery | Tab context actions; consistent editor action-menu icons and labels; five Monaspace families; texture healing and ligature toggles enabled by default; retained caret/selection/scroll and database-backed editor recovery. Real folder-permission recovery still needs device verification. |
| Selection and browser input | Multiple/rectangular selections and clipboard transactions; scoped native windows with bounded desktop startup for unwrapped, wrapped and nonuniform views, including long tabbed/bidirectional rows; automated Chromium composition and pointer checks. The recorded line-end caret bug is fixed and user-verified in the localhost PWA. |
| Preparation and ownership | Rust/WASM worker and cooperative fallback; retained source/token/structure allocations; bounded worker delta reconstruction/reply-source publication and final structure metadata, parsed/lexical region reconciliation and retained fallback collection; bounded eligible unwrapped and wrapped paragraph probes, retained paint-run clipping and cooperative sampled cold-row glyph measurements, exact overlap validation, exact carets from retained anchors/current painted coverage and conservative complete-layout fallback. Current-file progress is shown during preparation. Matching trusted font notifications retain current in-flight work and ordinary-row replay after edits. |

The latest integrated production matrix at `e3b2ee59` preserves complete source
after verified beginning edits in all twelve local/remote cases: the 8 MiB byte
limit, 100,000-row limit and near-1-MiB styled line, with wrapping on/off. Styled
viewport paint takes 139–181 ms; cold complete geometry across the matrix takes
1.6–3.5 seconds and peak Chrome PSS is about 588–756 MiB
([summary](editor-performance/integrated-boundary-summary.json),
[unwrapped samples](editor-performance/integrated-boundary-unwrapped.jsonl),
[wrapped samples](editor-performance/integrated-boundary-wrapped.jsonl)).
The styled native input handler takes 34–61 ms, including synchronous changed-row
coordinate reconstruction. Paragraph geometry timings include internal task
yields and do not establish individual task stalls. These instrumented samples
confirm remaining work; they are not a matched before/after improvement.

Earlier styled-line observations preserve complete source and a verified
beginning caret in all four Local/Remote samples. Input viewport paint is 167–269 ms
and peak Chrome PSS about 640–683 MiB
([summary](editor-performance/current-styled-production-summary.json),
[samples](editor-performance/current-styled-production.jsonl)). Compiler activity
was absent at launch but present at the post-run check; these are current
observations, not a controlled improvement. The input/memory gates remain open.

**Unresolved measured bottlenecks:** the recorded 16 interleaved production samples
preserve complete edited source in both modes. Near-1-MiB styled long-line edits
take 191–545 ms, cold complete geometry takes 1.1–1.7 seconds across the
row/long-line workloads, and peak Chrome PSS remains 657–730 MiB
([samples](editor-performance/row-planner-production-comparison.jsonl)).
Cooperative row planning is shipped, but these results leave responsiveness and
memory unfinished. Controlled source-size attribution identifies renderer startup as the dominant
process-memory increase: renderer PSS is about 133 MiB before app load, 378 MiB
with 1,000 rows, and 412–421 MiB near the row limit
([samples](editor-performance/source-size-memory-by-role.jsonl)). Profile that
startup cost next; total Chrome PSS includes a substantial browser baseline.
An isolated worker-bundle experiment did not reduce process
memory and increased the WASM download from roughly 27 to 42 MiB; it was rejected
([comparison](editor-performance/rejected-worker-split-comparison.jsonl)).
Earlier paired unwrapped module-sharing samples showed no PSS reduction
([diagnostic](editor-performance/shared-module-diagnostic.jsonl)). The current
worker now accepts the app's compiled module with matching-bundle checks and
versioned PWA bootstraps, retaining URL initialization for clone failures and
standalone clients. Both paths pass all provider/configuration cases; four official
worker contracts and eight geometry/input contracts pass
([receipt](editor-performance/worker-module-browser.json)). Isolated worker
readiness is 16–26 ms with handoff versus 91–133 ms through the URL fallback,
which includes its 25-ms handoff wait
([diagnostic](editor-performance/worker-module-startup-diagnostic.json)). Sixteen
same-artifact full-editor samples preserve complete edited source. Local sampled
peak PSS falls; incomplete early remote snapshots leave the general memory result
inconclusive, and cold editor readiness remains near 1.2 seconds
([comparison](editor-performance/worker-module-production-summary.json)). Profile
native allocations rather than assuming duplicate compilation explains the
renderer increase. A clock-marked native allocation
diagnostic follows the same app renderer from 41.9 MiB before navigation to
276.8 MiB at cold readiness and 283.6 MiB after input; anonymous writable
mappings dominate ([summary](editor-performance/native-allocator-diagnostic.json)).
A staged diagnostic in both modes now measures 124–126 MiB of renderer growth
before the main WASM entry point runs, with no editor or worker mounted. Main
linear capacity at that point is about 20 MiB; app/editor and worker startup add
another 109–114 MiB before readiness
([receipt](editor-performance/startup-memory-attribution.json)). The pre-start
stage includes module loading/compilation/instantiation and page preloads; native
stacks remain mostly unnamed. Investigate that runtime/module-load cost separately
from document allocations. This is attribution, not a shipped memory improvement.

The preceding 24-sample boundary matrix preserves complete edited source, with
byte-limit edits at 59–69 ms and row-limit edits at 32–60 ms
([summary](editor-performance/background-priority-boundary-summary.json)); repeat
the broader workloads on subsequent implementation checkpoints.

A current production input trace in both modes preserves source and caret,
but records about 39 ms inside the native input handler and repeated paragraph
geometry batches before and after syntax readiness. Sampled input time includes
viewport `clientWidth` reads and range layout probes
([diagnostic](editor-performance/current-input-profile-summary.json),
[raw trace](editor-performance/current-input-profile.jsonl)). Instrumentation and
concurrent host builds prevent a controlled latency claim. Attribute/batch the
viewport reads and prove pending-to-prepared geometry reuse next; retain exact
style, overlap, font and stale-owner validation.
Immediate-paint eligibility now precedes native viewport reads. Ineligible large
rows, wrapped views and prepared paint continue through the same scheduled path;
twelve targeted browser contracts and strict WASM lint pass. Duplicate geometry preparation
and other forced layout reads remain unfinished.
Hosted UI run `38211443355` repeated the horizontal-cache timeout with a loaded
font, current native-window stamps and no paint rows/probes. Initial native
geometry now accepts its current source-owned document index while fallback paint
is pending, avoiding that circular wait. Seven local/remote contracts pass,
including source/account/read/project/file/epoch rejection. The complete failing
group plus the new regression passes all 62 contracts with the official runner
on Linux arm64 (two CPUs/four GiB). Production build, eight full/windowed
composition checks, eight touch checks and strict WASM lint pass
([receipt](editor-performance/initial-native-readiness-browser.json)). Hosted
Linux/x64 verification of the combined disposal/runtime/readiness fixes passes
at `67b3a529`; the isolated contribution of this readiness change is not proved.
Paired native-value/selection restorations now update both source scroll axes
through one viewport refresh. Fourteen browser contracts, eight production touch
checks, eight full/windowed composition checks and strict WASM lint pass
([receipt](editor-performance/paired-scroll-browser.json)); other repeated reads
and duplicate geometry preparation remain open.
Fourteen focused local/remote browser contracts and strict WASM lint now pass.
Full hosted UI run `38207691623` exposed a 3-second horizontal-cache readiness
timeout. The failed contract and its complete 61-test group pass locally,
including a two-CPU/four-GiB repeat. Hosted Linux/x64 checkpoint `67b3a529`
subsequently passes this group, both font matrices and all other editor partitions
([investigation](editor-performance/horizontal-cache-ci-investigation.json)). Keep
the existing deadlines, source/probe limits and full inventory while diagnosing it.
The official wasm-bindgen runner also passes the exact 61-test selection with
two CPUs and four GiB. The failed contract now emits one bounded readiness
snapshot near its existing deadline, without forcing layout or changing its
fixture. Use hosted font, viewport-style, scope and probe state to establish the
cause before treating local passes as a CI fix.

Current-source unwrapped replay now uses the shared facade and existing suffix
proof: matching source/style topology and exact incoming measured geometry.
Tabs remain ineligible and terminal extents stay fresh. Eight production samples
preserve full edited source; cold unwrapped geometry improves to 1.35–1.43 seconds
([samples](editor-performance/unwrapped-current-production.jsonl)). Five final
browser regressions, 660 core tests, 129 native frontend tests and strict native
core/WASM frontend lint pass
([receipt](editor-performance/unwrapped-final-browser.json)).
Cold preparation and memory remain unfinished. The prior hosted browser groups
hit their 300-second deadline after the long wrapped replay contract passed;
independent replay partitions now preserve coverage and deadlines. Hosted CI for
the partition fix passes all five jobs
([receipt](editor-performance/grouped-ci-2bdd9a8-success.json)); the newer checkpoints `d26efa9` and `f251d89` each pass all five jobs
([receipts](editor-performance/editor-ci-d26-success.json),
[receipt](editor-performance/editor-ci-f251-success.json)).

Integrated checkpoint `764873b8` passes all five hosted jobs, including the full
browser partition on combined editor/plugin source
([receipt](editor-performance/editor-ci-764-success.json)). Post-frame owner
cancellation now releases wrapped probes before accessing disposed state; two
consecutive affected 58-test groups and an accepted-coverage unmount regression
pass both project modes
([group](editor-performance/wrapped-unmount-group-browser.json),
[repeat](editor-performance/wrapped-unmount-group-browser-repeat.json),
[unmount](editor-performance/wrapped-unmount-browser.json)). The `c353f63c`
checkpoint failed the cold-command cancellation browser test, which assumed
preparation could not finish during its first 12 ms slice. That regression now
uses the shared injectable clock, and `97d1e424` verifies its correction. The later
`307bc045` and `d2238505` checkpoints failed the dense-row browser contract because metadata
prepared only one token row per unit. Small rows now share the existing 64-record/
8 KiB budget; the 2,000-batch bound is unchanged. Checkpoint `764873b8` verifies this correction across all five hosted jobs. Final integrated CI remains a completion gate.

**Remaining implementation** (all items remain part of this editor goal):

Integrated run `38264345670` passed all checks, both platform images and nightly
publication ([receipt](editor-performance/editor-ci-67b-success.json)). Its complete
editor inventory contained 216 contracts across eight browser runs, with unchanged
deadlines. Editor checks run after a failed frontend step, and the runner executes
every verified group while still failing on any failed group. Newer combined source
and a reliable repeat of the full release remain required. Run `38283033218` at
`99212ba5` also passed every check, both images and nightly publication
([receipt](editor-performance/editor-ci-992-success.json)), verifying the host-update
receipt and native frontend import fixes. Current changes still need their own
combined-source CI result.

- [ ] **Parser-aware input and decorations:** finish admitted HTML/Markdown cold
  readiness and release input costs, the 12 ms budget/fallback behavior, and
  retained-fold/current-source ownership. Keep ordinary literal input independent
  of unnecessary lexical scans; preserve pairing once current context is ready.
  Verify explicit structural commands, cancellation and large fenced-code input.
  Current-source paint and valid syntax fragments now publish while analysis is
  pending. Bounded early publication passes six focused browser contracts and
  sixteen production samples; small Remote Markdown input paint is about 17 ms
  ([samples](editor-performance/early-pending-production.jsonl)).
  Main-thread asynchronous parser fallback now yields at the shared 12 ms
  deadline rather than using the dedicated worker’s 100 ms allowance. Controlled
  HTML/Markdown transport contracts verify yielding, cancellation and complete
  analysis across slices; five local/remote UI contracts and strict native core/
  WASM lint pass ([receipt](editor-performance/main-thread-parser-budget.json)).
  Refreshed folds now inherit pending collapse state only when their entire
  rebased range matches, preventing changed Markdown boundaries from hiding
  adjacent sections. All 671 core tests, four local/remote browser contracts and
  strict native/WASM lint pass
  ([receipt](editor-performance/pending-fold-boundary-browser.json)).
  More than 64 embedded code bodies now use the shared source, parser-work,
  aggregate AST-node and record budgets. Cold/warm preparation supports 200 HTML
  script/style bodies and 100 Markdown fences, including LF/CRLF, yielding,
  language-parser reuse and exact changed scopes. New and retained trees charge
  the same aggregate AST-node budget before publication
  ([receipt](editor-performance/many-code-bodies-browser.json)).
  Full physical timing, remaining boundary workloads and
  broader retained decoration ownership remain unfinished.
- [ ] **Cold startup and native input:** bound remaining complete-source shaping,
  projection/native normalization, remaining word/context traversal and synchronous
  selection publication. The shared bounded grapheme scanner now skips general
  segmentation for adjacent printable ASCII scalars at proven boundaries; Unicode
  lookahead and suspended clusters retain the original scanner. All 751 core tests
  and strict core lint pass, including arbitrary Unicode, long clusters and chunk
  seams. The full production build, WASI core lint and eight full/windowed
  LF/CRLF composition/undo/redo/cancel checks pass in both modes.
  Production profiles also identify forced panel-width layout reads. The shared
  browser observer now uses delivered content-box widths to skip height-only
  reads, and publishes only changed native widths. The browser regression verifies
  padded widths, resize, hide/show and cleanup; all 48 WASM library checks and
  four local/remote panel contracts pass
  ([receipt](editor-performance/width-observer-browser.json)). Repeat matched
  production measurements before claiming a latency improvement.
  Coordinate construction now groups printable ASCII into bounded spans, sharing
  one cursor update and bulk native totals while retaining the original visual
  checkpoints, word summaries, eligibility and Unicode/CRLF boundaries. All 766
  core tests, 48 WASM library checks and six local/remote complete-render geometry
  contracts pass with strict native/WASM lint
  ([receipt](editor-performance/ascii-spans-browser.json)). Compiler-checked,
  interleaved native measurements for 20 beginning edit/undo pairs fall from
  648 to 158 ms for ASCII and 691 to 464 ms for mixed Unicode
  ([benchmark](editor-performance/ascii-spans-native-edits.json)). Four same-backend
  production cases preserve source; native input handlers fall from about 35 to
  28–29 ms, but viewport results are mixed and do not establish an overall UI
  improvement ([summary](editor-performance/ascii-spans-production-summary.json)).
  Printable-ASCII single-row edits now retain unaffected category runs and derive
  native/glyph checkpoints arithmetically. Source inspection is limited to inserted
  bytes and nearby uncached category boundaries; cached long runs skip unchanged
  spans. Complete-table regressions compare against fresh indexes across insertion,
  replacement, deletion, thresholds and LF/CRLF/Unicode/tab fallbacks. All 783
  core tests, 134 native frontend tests, 58 WASM library checks and six local/remote
  geometry/fallback contracts pass with strict core/WASM/WASI lint
  ([receipt](editor-performance/incremental-ascii-row-browser.json)). The full
  Spin build and eight full/windowed LF/CRLF composition/undo/redo/cancel checks
  pass in both modes at `d5dba783`
  ([receipt](editor-performance/incremental-ascii-row-production-input.json)).
  Sparse-table allocation/publication, Unicode and row-splitting reuse, and
  broader responsiveness/memory gates remain open.
  ASCII line splitting now prepares retained prefix/suffix coordinates from the
  same row proof and inspects only inserted rows for new line breaks. Initial
  preparation and edit preparation share row-summary append policy, including
  CRLF normalization and the trailing EOF row. Complete-index regressions cover
  beginning/middle/EOF splits, replacements, LF/CRLF and Unicode inserted interior
  rows. All 784 core tests, 134 native frontend tests, 58 WASM library checks and
  six local/remote geometry/fallback contracts pass with strict core/WASM/WASI
  lint ([receipt](editor-performance/ascii-row-split-browser.json)). The full Spin
  build and twelve production input cases pass at `73fa5d0c`: both workspace modes,
  LF/CRLF, full/windowed composition, and explicit long-ASCII insertion/split/
  undo/redo before composition
  ([receipt](editor-performance/ascii-row-split-production-input.json)). Matched
  native release measurements for twenty edit/undo pairs fall from 126–130 ms to
  about 12 ms for ASCII insertion, and from 130 to 97 ms for ASCII split/undo.
  Unicode/dense results vary; mixed split/undo is slower and needs investigation
  ([measurement](editor-performance/retained-ascii-row-native-edits.json)). These
  exclude browser input/layout and establish no whole-editor or memory gain.
  Unicode boundaries, multi-row joins, sparse table allocation/publication and
  broader responsiveness/memory remain open.
  Printable-ASCII proof now latches rejection during coordinate preparation,
  avoiding repeated eligibility comparisons after Unicode/control text. All 787
  current-worktree core tests, 134 frontend tests, 58 WASM library checks and six
  local/remote geometry contracts pass with strict core/WASM/WASI lint
  ([receipt](editor-performance/ascii-proof-latch-browser.json)). A guarded timing
  run was discarded because another compiler became active. The Unicode timing
  regression, fresh production verification and final integrated CI remain open.
  Visual ASCII coordinates now update by word/space/punctuation groups and
  visual-only preparation avoids an unused character traversal. A complete-table
  regression compares every field with full Unicode grapheme traversal at
  budgets 1/8/64. All 767 core tests, 48 WASM library checks and six local/remote
  geometry/fallback contracts pass with strict native/WASM lint
  ([receipt](editor-performance/ascii-groups-browser.json)). Compiler-checked,
  interleaved native samples for 20 edit/undo pairs fall from 155 to 107 ms for
  ASCII and 469 to 447 ms for mixed Unicode
  ([benchmark](editor-performance/ascii-groups-native-edits.json)). The fresh
  production build and eight full/windowed LF/CRLF composition/undo/redo/cancel
  cases pass in both modes ([input receipt](editor-performance/ascii-groups-production-input.json)).
  Four styled-line production cases preserve source with wrapping on/off;
  viewport paint is 119–142 ms, native handlers 25–26 ms and cold complete
  geometry 1.6–4.2 seconds. Peak Chrome PSS is about 650–733 MiB
  ([summary](editor-performance/ascii-groups-production-summary.json)). These
  single instrumented observations do not establish a matched UI/memory gain.
  The full release checks still need repeating; synchronous traversal and the
  broader performance gates remain open.
  Isolated native timings improve for ASCII and mixed text
  ([receipt](editor-performance/ascii-grapheme-scan.json)); whole-editor latency,
  full-row edit rebuilding and startup memory remain open.
  Current instrumented near-MiB styled-line observations preserve source in both
  modes: viewport input paint is 136–139 ms and peak Chrome PSS is 676–681 MiB
  ([profiles](editor-performance/ascii-grapheme-production-profile.jsonl)).
  These are diagnostics, not a matched improvement claim; native input handling
  still takes about 35 ms and paragraph layout/geometry still repeats after input.
  Native value synchronization now retains a browser string
  for the exact immutable normalized text owner and compares the actual textarea
  value in the DOM adapter. Source/file/read/project/account changes release cached
  owners. All 730 core tests, thirteen local/remote browser contracts, four worker
  startup tests, strict native/WASM lint and both full production worker inventories
  pass. Eight full/windowed LF/CRLF composition checks and eight trusted touch
  checks preserve text, history and selection
  ([receipt](editor-performance/native-value-browser.json)). Sixteen serial
  baseline/candidate observations preserve complete source; MiB input remains
  145–175 ms with no general latency/PSS improvement
  ([comparison](editor-performance/native-value-production-summary.json),
  [samples](editor-performance/native-value-production.jsonl)). Integrated release
  checks remain pending;
  initial encoding, native input reads, shaping and normalization remain open.
  Native newline normalization now copies unchanged UTF-8 spans rather than
  decoding every character, sharing the implementation across projections and
  inserted text. All 738 core tests, strict core lint, production build and eight
  full/windowed LF/CRLF composition checks pass in both modes
  ([receipt](editor-performance/native-normalization-spans.json)). Isolated native
  helper timings improve, especially for long Unicode spans; concurrent builds
  and exclusion of browser work prevent a whole-editor latency claim. Initial
  capacity allocation and full-source normalization remain part of startup work.
  Editable one-finger scrolling now forwards through the
  same browser scroll primitive as wheel input; headless Chrome touch/wheel,
  caret and wrapped-geometry contracts pass in both modes. Chromium-generated
  touch gestures, enabled before startup or after desktop preparation, also
  scroll editable and read-only prepared editors without
  changing text or selection ([receipt](editor-performance/trusted-touch-browser.json)).
  Expanded coverage also passes all 16 focused/unfocused combinations in both
  modes ([receipt](editor-performance/focused-touch-browser.json)). Physical-device
  verification, source-owned touch caret/selection and native fallback transitions
  remain open. Boundable native windows now cap bytes and 128 rows plus endpoint
  mapping. Ordinary edits rebind overflowing windows; active composition retains
  its value and reclips on completion. All 716 core tests, nine local/remote
  browser contracts and strict native/WASM lint pass
  ([receipt](editor-performance/native-row-bound-browser.json)). Sixteen production
  beginning-edit cases preserve complete source, including dense rows and embedded
  HTML/Markdown. Dense native values fall from 12,288 to 128 characters; wrapped
  paint remains 78–92 ms, with mixed before/after results and differing backend
  artifacts, so this establishes a bound rather than a general speedup
  ([measurements](editor-performance/native-row-bound-production-summary.json)).
  Complete-source startup, normalization and wrapped input latency remain open.
  Prepared long-row coordinates now retain sparse category runs
  for exact word jumps, including giant Unicode clusters. The full core suite,
  three local/remote browser contracts and strict lint pass; native release
  measurements show 2–8 ms long-run scans becoming sub-0.001 ms queries
  ([receipt](editor-performance/indexed-word-navigation.json)). These exclude
  browser paint and startup. Double-click and word-drag selection now reuse the
  same prepared category steps rather than scanning from the row start. All 522
  core tests, three local/remote browser contracts and strict lint pass
  ([receipt](editor-performance/indexed-word-pointer.json)).
  Cross-row ASCII whitespace now skips per-grapheme classification through
  boundary-proved byte runs, retaining joined Unicode endpoint clusters. All 525
  core tests, four local/remote browser contracts and strict native/WASM lint pass
  ([receipt](editor-performance/cross-row-whitespace-browser.json)). Native
  release traversal measurements improve from 12–36 ms to 0.23–0.25 ms across
  near-limit whitespace runs; these exclude browser paint and startup. Byte scans
  remain linear. Unicode-context and unindexed/noncanonical traversal still need
  bounded work. Preserve
  composition mapping and complete-native fallback when
  bounded geometry cannot be proved. Finish startup and memory measurements
  across admitted boundaries, including unsupported layouts. Repeated scroll getters now resolve the mounted
  surface once; row-limit comparison samples still reach 100 ms, so native
  dimension/layout publication and row-table costs remain
  ([comparison](editor-performance/scroll-reference-line-comparison.jsonl)). Scroll-paint
  synchronization now snapshots viewport reads before CSS writes and skips
  unchanged values; eight scroll/caret/geometry contracts pass in both modes
  ([receipt](editor-performance/scroll-paint-browser.json)). Native range
  measurement and full cold/input costs remain unfinished.
  The shared viewport observer now applies changed viewport dimensions before
  measuring wrapped rows, batches row reads before gutter writes, and skips
  unchanged styles. Eight scroll/caret/wrapped-resize/stale-owner contracts pass
  in both modes ([receipt](editor-performance/viewport-row-layout-browser.json)).
  Proved source extent changes now refresh the scroll surface independently of
  visible paint, fixing stale scrollbar widths after offscreen edits. Paste
  alignment uses complete-source scroll offsets when native text is bounded;
  the two previously failing hosted contracts and six related input/geometry
  contracts pass locally ([receipt](editor-performance/worker-module-browser.json)).
  Twelve production observations retain separate paint/readiness/PSS metrics;
  they do not establish a latency or memory gain
  ([comparison](editor-performance/viewport-row-layout-production-summary.json)).
- [ ] **Tabbed, wrapped and bidirectional geometry:** extend bounded preparation
  beyond eligible source-monotonic paragraphs with complete-word seams. Finish
  bidirectional visual-run windows, fine long-row paint, incremental glyph
  measurements, complete DOM installation and synchronous pointer/neighborhood
  queries. Finish arbitrary nonorigin coverage, failed-proof fallbacks,
  nonreconnecting wrapped updates and changed long-token/plain-run boundaries.
  Native range reads in cold paragraph preparation now yield between 128-glyph
  batches, reject stale font/source/layout ownership, and preserve first paint
  while background geometry is held
  ([receipt](editor-performance/cooperative-paragraph-ranges-browser.json)).
  Dense Unicode offsets now use the shared bounded Rust scanner; DOM text-node
  enumeration and native-reference setup yield before glyph reads, retaining
  complete-only publication and stale-owner rejection in both project modes
  ([receipt](editor-performance/cooperative-paragraph-setup-browser.json)).
  Eight production samples preserve complete edited source
  ([samples](editor-performance/cooperative-paragraph-setup-production.jsonl));
  the repeated remote/wrapped pair also preserves source
  ([repeat](editor-performance/cooperative-paragraph-setup-pair-repeat.jsonl)).
  External build activity limits timing interpretation; the remote/wrapped pair
  reverses direction on repeat. Input remains 206–519 ms in candidate samples;
  no stable latency or memory gain is established.
  Capacity allocation, paragraph layout and synchronous viewport queries remain.
  Preserve exact browser extents and anchors; approximate widths are insufficient.
- [ ] **Incremental syntax, structure and paint:** finish retained-container reuse,
  warm semantic lists, bounded incremental row-key traversal, shifted suffix metadata, parser
  context/selection-list extraction, larger fenced-code workloads, changed plain
  row-table reconstruction and final paint publication. Bound cache-missing scans,
  whole-row capacity growth, scratch allocations and changed long-row coordinate/
  visual index construction. Keep retained immutable tables exact through edits. Indexed measurement keys
  remove the complete paint-descriptor vectors; row-limit WASM memory after input
  drops to about 41 MiB from 54–56 MiB, but full traversal and input spikes remain
  ([samples](editor-performance/indexed-row-comparison.jsonl)).
  Repeated-key grouping and measurement propagation now yield between bounded
  row batches and reject stale owners in both modes
  ([receipt](editor-performance/cooperative-row-browser.json)). Row validation,
  prefix/suffix comparison, retained-dimension copying and duplicate lookup now
  also yield between bounded row batches; layout invalidation restarts planning
  ([receipt](editor-performance/cooperative-row-reuse-browser.json)). Total
  unchanged-row traversal, single-key byte costs, allocations and final row-table
  publication still need bounded work and production measurements.
  Current/retained planning now trusts immutable core-built projection row ranges
  rather than traversing every row body again before reuse and grouping. Core
  invariants cover edits, folded Unicode LF/CRLF and bounded windows; 717 core
  tests, eight ownership/fallback/geometry browser contracts and strict lint pass
  ([receipt](editor-performance/row-key-proof-browser.json)). Eight production
  edits preserve full source in both modes. Wrapped dense paint remains 73–80 ms
  and wrapped HTML paint 21–25 ms; these observations use differing artifacts and
  leave general latency, remaining traversal and allocation work open
  ([samples](editor-performance/row-key-proof-production.jsonl)).
  Token text now has immutable shared ownership across retained pieces and mixed
  paint rows; buffered worker reception moves its completed string into that
  owner without recopying the token. Long Unicode LF/CRLF rows retain identical
  text owners across warm preparation. The 718 core tests, nine selected browser
  contracts and four worker startup/fallback tests pass with strict native/WASM
  lint ([receipt](editor-performance/shared-token-browser.json)). Both production
  startup paths preserve all 15 providers, six config providers, plain fallback,
  six embedded workloads and MiB-sized literal reception. Twenty-four production
  edits preserve complete source. Main-thread WASM memory rises by about 0–320 KiB
  in these serial samples; timings/PSS do not establish an overall improvement
  ([comparison](editor-performance/shared-token-production-summary.json),
  [samples](editor-performance/shared-token-production.jsonl)). Remaining row
  traversal, allocation overhead and complete publication still need bounded work.
  Large pending rows now retain their complete token reconciliation for repeated
  descriptor reads and paragraph HTML probes. An eight-row cache checks exact
  source/token ownership and body identity, releases replaced owners, and leaves
  cheap rows on the direct path. All 752 core tests, 129 native frontend tests,
  46 WASM library checks, six complete-render geometry/fallback contracts and
  eight full/windowed input cases pass with strict native/WASM lint and the full
  production build ([receipt](editor-performance/pending-row-matches-browser.json)).
  Current instrumented input observations are 134–170 ms, with peak Chrome PSS
  about 669–689 MiB ([profiles](editor-performance/pending-row-matches-production-profile.jsonl));
  they do not establish a matched latency/memory improvement.
  Paragraph prefix/suffix style comparisons now share a complete-only resumable
  engine, with 8 KiB byte-pair chunks and immutable-slice shortcuts. Wrapped and
  unwrapped preparation check cancellation and current ownership between batches;
  synchronous callers drive the same policy. All 754 core tests, 47 WASM library
  checks, six local/remote geometry/fallback contracts and eight full/windowed
  input cases pass with strict native/WASM lint and the full production build
  ([receipt](editor-performance/paragraph-style-comparison-browser.json)). First
  token reconciliation, other complete style comparisons, layout repetition and
  the full performance gates remain open. Paragraph comparisons now also use
  bounded underlying-token steps when seeking clipped windows, including hidden
  and empty tokens. Visible-count tracking avoids scanning exhausted windows;
  forward/reverse/mixed iteration retains exact spans. All 756 core tests,
  47 WASM library checks, six local/remote geometry/fallback contracts and eight
  full/windowed input cases pass with strict native/WASM lint and the production
  build ([receipt](editor-performance/token-window-steps-browser.json)). Other
  synchronous iterator consumers and first reconciliation still need bounded work.
  Styled paint-run preparation now uses the same bounded token steps for initial
  fetch and lookahead, keeping hidden-window traversal inside the cooperative
  budget. Complete-only publication preserves plain-token merging, CR handling
  and Unicode boundaries. All 757 core tests, 47 WASM library checks and six
  local/remote geometry/fallback contracts pass with strict native/WASM lint
  ([receipt](editor-performance/paint-run-lookahead-browser.json)). Other
  synchronous consumers, initial token reconciliation and the full performance
  gates remain open.
  Grammar paint now retains private piece/row assembly across cooperative yields,
  with an 8 KiB source-copy budget and independent 256-record construction,
  owner-copy, visitation and row-publication budgets. Synchronous consumers drive
  the same engine. Original parse status survives resumes; partial paint/context
  stays unavailable through cancellation or source takeover. All 722 core tests
  preserve the existing tiny-slice/dense-row progress bounds; eight local/remote
  browser contracts, four worker startup tests and strict native/WASM lint pass
  ([receipt](editor-performance/paint-assembly-browser.json)). Both production
  startup paths preserve the full provider/config/embedded inventory and MiB
  literal reception. Thirty production edits preserve complete source. Dense-row
  input remains near 12 ms; initially slower MiB samples return to 137–166 ms on
  repeat. Serial samples establish no stable latency/PSS improvement
  ([comparison](editor-performance/paint-assembly-production-summary.json),
  [samples](editor-performance/paint-assembly-production.jsonl)). At that checkpoint semantic
  collection/sorting, capacity growth, retained-cache retirement and complete
  final table publication remain unfinished.
  Semantic paint masking, stable ordering and overlap checks now resume in
  1,024-record numeric-metadata steps. Parsed embedded-body coverage uses ordered
  starts and prefix maximum ends instead of a complete body scan per outer span;
  nested, unsorted, absent-tree and zero-width cases match the original linear
  oracle. The structure and paint paths share the same metadata sorter, including
  its four-comparison step bound and adaptive fallback. All 726 core tests, six
  local/remote browser contracts, four worker startup tests and strict native/WASM
  lint pass ([receipt](editor-performance/semantic-normalization-browser.json)).
  Both production startup paths preserve the full language/config/embedded
  inventory and MiB literal reception. Sixteen wrapped HTML/Markdown production
  edits preserve full source with unchanged main-thread WASM memory. Mixed timing
  and PSS samples establish no general gain
  ([comparison](editor-performance/semantic-normalization-production-summary.json),
  [samples](editor-performance/semantic-normalization-production.jsonl)).
  Cached-subtree classification and relative-span rebasing now advance in private
  256-record steps, sharing the frontier policy with structural consumers and
  preserving aggregate visit limits and invalid-cache fallback. Numeric ancestry
  and nearby cursors preserve parent roles without a root lookup per node.
  Classification, cached-span mapping, ancestry cleanup and cache publication
  have independent record budgets, preserving the existing progress limits.
  All 728 core tests, six local/remote browser contracts, four worker startup tests
  and strict native/WASM lint pass
  ([receipt](editor-performance/highlight-collection-browser.json)). Both production
  startup paths preserve the full language/config/embedded inventory and MiB
  literal reception. Eight wrapped HTML/Markdown edits preserve complete source;
  input paint remains 21–77 ms and cold preparation 1.2–1.4 seconds. Historical
  serial comparisons establish no general latency/PSS improvement
  ([comparison](editor-performance/highlight-collection-production-summary.json),
  [samples](editor-performance/highlight-collection-production.jsonl)). Integrated CI
  verification for this checkpoint remains pending. Cursor seeks, capacity growth, retained-cache retirement
  and final publication still need bounded work.
  Reused highlight-cache entries now transfer ownership into the next cache;
  leftover entries retire through the same preparation driver in bounded record
  steps. Invalid-span fallback also rebuilds the current tree's visit count.
  All 730 core tests, six local/remote browser contracts, four worker startup tests,
  strict native/WASM lint and both full production worker inventories pass
  ([receipt](editor-performance/cache-retirement-browser.json)). Eight serial
  baseline/candidate edits across a 2,500-function cache preserve full source and
  grammar colors in both modes. Input paint remains 11–13 ms, syntax readiness
  52–58 ms and cold preparation about 1.2 seconds; no general latency/PSS gain
  is established ([comparison](editor-performance/cache-retirement-production-summary.json),
  [samples](editor-performance/cache-retirement-production.jsonl)). The rejected
  4,000-function trial exceeds existing analysis admission
  ([trial](editor-performance/cache-retirement-rejected-workload.jsonl)); limits
  are unchanged. Integrated CI verification remains pending.
  Iterator scans, table/tree destruction, cancellation disposal, other retained
  caches and final publication remain unfinished.
- [ ] **Source, worker transport and storage:** finish external parser snapshots,
  changed-revision comparisons, header/capacity allocation,
  JavaScript string conversion, complete message decoding/publication,
  oversized-message fallback, diff shaping and native-text materialization.
  In-process structural commands now retain shared source/analysis through the
  cooperative core facade, removing their request/reply JSON round trip and
  duplicate frontend admission pass. Cache limits, cancellation, exact analysis
  and shared source ownership have contracts; navigation, comments and reindent
  are verified in both modes
  ([receipt](editor-performance/in-process-structure-browser.json)). Dedicated
  worker reply JSON encoding now yields between 8 KiB source-string and 64-record
  batches, including nested structure lists and token rows. Full and retained
  publications match independent serde bytes, and cancellation discards private
  partial output ([receipt](editor-performance/cooperative-reply-browser.json)).
  Payload preparation now batches fold copies, structural prefix/suffix comparisons
  and record copies, token-row reuse comparisons and span construction. Long token
  comparisons and fallback source copies yield between 8 KiB chunks; record work
  uses 64-record batches. Independent original-algorithm byte comparisons and
  exact record-limit/cancellation contracts pass
  ([receipt](editor-performance/cooperative-metadata-browser.json)).
  Receiver bracket/language/body queries now use indexed ordered scopes, preserving
  touching and empty-scope boundaries. Fold validation reuses the shared ordered
  validator instead of cloning and sorting. Dense token metadata shares one
  record/text budget across small rows, preserving the existing browser batch bound
  ([receipt](editor-performance/indexed-scopes-browser.json)). Native query-only
  measurements compare against the original linear oracle; browser-wide latency
  and receive reconstruction remain unfinished. Receiver source comparisons and
  logical-line counting now use 8 KiB batches; ordered fold validation budgets
  records and ancestor pops together. The shared receiver retains private proofs
  across adapter yields and rechecks account/project/file/source ownership before
  reconstructing or publishing a result
  ([receipt](editor-performance/cooperative-receiver-browser.json)). Receiver wire
  record admission and declared-row counting now use 64-header batches. Source-line
  scanning, token materialization and retained-row byte validation share 8 KiB
  byte and 64-record work budgets; only complete reconstructed rows are published.
  Initial header accounting and expanded record limits preserve the original policy
  ([receipt](editor-performance/cooperative-token-receiver-browser.json)).
  Structural patch copies now share a 64-record budget across all five lists.
  Ordered coordinate validation and bracket/link/body proofs also yield between
  64-record batches, preserving complete-only publication and expanded limits
  ([receipt](editor-performance/cooperative-structure-receiver-browser.json)).
  A versioned core frame codec now separates source chunks, folds, structural
  descriptors/records, dense rows and long span rows into bounded typed serde
  payloads. The decoder enforces transaction order and limits and exposes a reply
  only after End; decoded replies enter the shared proof pipeline directly
  ([core receipt](editor-performance/syntax-frame-codec-core.json)).
  Live worker publication and routing now use these frames. The cooperative
  adapter returns the same typed payload; both enter the shared proof pipeline
  without complete reply JSON encoding/decoding. Worker documents become delta
  bases only after complete frame publication. Real dedicated-worker full/warm
  Rust, HTML and Markdown contracts, malformed transactions, dropped requests
  and existing local/remote stale-owner contracts are tracked in the
  [transport receipt](editor-performance/live-syntax-frame-transport.json).
  The production-worker oracle and official-runner fixture now understand framed
  replies and explicit child WASM initialization; both failed CI checks are
  reproduced and repaired locally; all five hosted jobs now pass at `1b63c9ab`
  ([CI receipt](editor-performance/editor-ci-1b63-success.json),
  [repair receipt](editor-performance/framed-transport-ci-repair.json)). Twelve
  unwrapped production samples cover dense rows and 250 repeated embedded-language
  HTML/Markdown sections in both modes. Early viewport paint and cold/post-edit syntax readiness
  are distinct metrics; startup long tasks and high process-tree memory remain
  ([baseline](editor-performance/framed-transport-current-production-summary.json),
  [samples](editor-performance/framed-transport-current-production.jsonl)). These
  are current-version observations, not evidence of an improvement.
  Framed receivers now reserve admitted source/fold/row/span and structural
  counts once, using fallible allocations before bounded appends. Full/warm,
  dense/long-row and malformed-header contracts verify stable capacity and
  rejection before over-budget record allocation
  ([receipt](editor-performance/frame-capacity-browser.json)).
  Plain preparation, grammar paint and syntax reception now share immutable
  blank rows, preserving the distinct empty/Plain-token shapes and exact retained
  owners. Dense 49,001-row contracts verify sharing and source ownership
  ([receipt](editor-performance/shared-blank-row-browser.json)). Plain preparation
  also avoids intermediate token vectors. Production comparisons and wrapped
  beginning-edit source checks are recorded separately
  ([measurements](editor-performance/shared-blank-row-production-summary.json)).
  Initial reservation costs, vector/string growth outside frame decoding,
  JavaScript string conversion and per-row immutable allocation/copying still
  require bounded work and production latency/memory measurements. Twelve
  production observations compare against the previous checkpoint without
  establishing an end-to-end latency or process-memory gain
  ([comparison](editor-performance/frame-capacity-production-summary.json)).
  Bound folded/bounded projection assembly, row/coordinate copies, changed guide
  copies, storage suffix byte/coordinate shifts and long-row admission scans.
  Revisit measured storage candidates using actual viewport/worker access patterns,
  preserving immutable versions, exact coordinates and cancellation.
- [ ] **Recovery and history costs:** finish synchronous document callers,
  recovery admission scans, validation/copies, metadata growth, external borrowed
  version allocation, serialization, complete JSON/network buffers, final selection/
  fold normalization and publication. Bound recovered undo/redo prefix/suffix
  comparisons and fold rebasing; verify memory and source ownership. Retain shared
  saved/draft versions and reconcile late acknowledgements with actual written source.
  Recovery document line counting and persisted-fold validation now advance in
  8 KiB source / 64 fold-record-or-ancestor-pop units in the shared Rust engine.
  Loading and document preparation yield between batches and reject lost owners;
  a proof tied to the exact immutable snapshot avoids repeating its validation at
  hydration publication. Existing synchronous callers drive the same policy.
  All 740 core tests, three browser lifecycle/cancellation checks and fifteen
  recovery/cache contracts pass, with no browser console panics. Strict native
  core, WASM frontend and WASI backend lint and the production build pass
  ([receipt](editor-performance/recovery-validation-browser.json)).
  Snapshot capture/guard comparisons, serialization,
  admission, final fold installation and recovered history costs remain open.
  Recovery browser verification also exposed a queued native-text cache effect
  reading disposed workspace signals. Scope reads and cache cleanup now reject
  disposed owners; the lifecycle regression and both-mode recovery contracts
  now pass without the reproduced panics. This does not establish a general
  memory improvement.
  Restored collapse state now assembles privately in 64-record units from the
  immutable validated document proof. Preparation reuses that proof instead of
  recounting/validating folds; final installation avoids sorting already normalized
  ranges. All 742 core tests, four lifecycle/cancellation browser contracts, fifteen
  recovery/cache contracts and the complete 31-test isolated library selection
  pass. Strict native/WASM/WASI lint and production build pass
  ([receipt](editor-performance/recovery-restoration-browser.json)). Source comparisons,
  allocation growth, final history/selection publication, snapshot capture and
  serialization remain unfinished.
  Disk reconciliation now retains committed draft/baseline owners and advances
  source admission and equality in 8 KiB units. Background verification and the
  save facade share the same scheduling path, retry changed owners and reject
  lost scope. The prior synchronous API drives the same core policy. Verification
  passes: 744 core tests, five lifecycle/current-source browser tests, fifteen
  recovery/cache contracts, strict native/WASM/WASI lint and production build
  ([receipt](editor-performance/recovery-disk-browser.json)). Review/snapshot
  comparisons, serialized snapshot UTF-8 decoding, capture/guard checks,
  serialization, allocation growth and other history/publication costs remain open.
  Recovered disk UTF-8 decoding now validates privately in 8 KiB slices and moves
  the validated byte allocation into its complete String. Background checks, save
  verification and review use the same scoped decoder above both workspace
  adapters. Invalid and truncated scalars preserve strict validation errors;
  lost scopes cannot publish text, change recovery checks or consume overwrite
  permits. All 768 core tests, 49 WASM library checks and 23 recovery-related
  browser contracts pass, including cancellation through both real adapters;
  strict native core, WASM frontend and WASI backend lint pass
  ([receipt](editor-performance/recovery-disk-utf8-browser.json)). Serialized
  snapshot decoding, overwrite/guard comparisons, capture, serialization,
  allocation growth and final history/selection publication remain unfinished.
  Explicit overwrite permits now use immutable committed source owners and a
  complete-only comparison in 8 KiB units. Disk reconciliation shares the same
  equality policy. Verification retries edited or acknowledged source owners,
  rejects canceled/disposed/removed owners, and preserves missing versus empty
  disk semantics. All 769 core tests, 50 WASM library checks and 23 recovery-related
  browser contracts pass, including both real workspace adapters. Strict native
  core, WASM frontend and WASI backend lint pass
  ([receipt](editor-performance/recovery-overwrite-browser.json)). Broader review/
  snapshot guard comparisons, capture, serialization, allocation and final
  history/selection publication remain open. The fresh production build now
  passes API validation/stale-save/tombstone checks plus actual edits, worker
  preparation, autosave, Spin restart and reload/new-window restoration in both
  modes; eight full/windowed LF/CRLF composition/undo/redo/cancel cases also pass
  ([receipt](editor-performance/recovery-latest-production.json)). The recovery
  probe now observes blob/module workers and complete framed source deltas;
  its full/delta UTF-8, sequence and cancellation regression passes. Real folder
  permissions, physical input, overall performance and final CI remain open.
  Review disk comparisons now share the 8 KiB complete-only equality policy,
  including missing/empty distinctions. Decoding and comparison batches use
  cheap root/ticket/handle checks instead of repeatedly rebuilding the full
  recovery guard; complete guards still run before publication/application and
  after comparison yields. All 770 core tests, 51 WASM library checks and 23
  recovery-related browser contracts pass through both real adapters, with
  strict native core/WASM frontend/WASI backend lint
  ([receipt](editor-performance/recovery-review-comparison-browser.json)). Full
  guard capture/equality, snapshot decoding, serialization, allocation and final
  history/selection publication remain open; repeat fresh production and final
  CI verification before closing.
  Recovery guard matching now shares candidate capture with persistence and
  reuses the guard's privately validated state: exact equality proves validity
  without repeating line/fold validation or allocating another revision map.
  Changed project/epoch/read/document revisions reject before state capture.
  All 133 native frontend tests pass, including both-mode equivalence with the
  original full-capture decisions across thirteen state changes. All 51 WASM
  library checks, 23 recovery-related browser contracts and strict WASM lint
  pass ([receipt](editor-performance/recovery-guard-browser.json)). Candidate
  capture, fold/selection copying, final field equality and serialization remain
  synchronous; fresh production and final integrated CI are still required.
  Collapsed-range capture now visits collapsed headers directly instead of
  scanning every current range. Recovery, fold refresh and projection sharing
  use the same core iterator; pending indicators cannot authorize a range.
  The 50,000-range regression, 771 core tests, 51 WASM library tests, 45
  recovery/fold browser contracts and strict core/WASM/WASI lint pass
  ([receipt](editor-performance/collapsed-range-capture-browser.json)).
  Large all-collapsed collections still require bounded copying and capture.
  Serialized recovery sources now decode through the same bounded base64/UTF-8
  engine in storage and browser loading. Browser loading borrows unescaped
  encoded fields from the response and checks account ownership between batches.
  Complete JSON parsing, network buffers, initial allocation, snapshot capture
  and serialization still need work. All 774 core tests, 133 native frontend
  tests, 52 WASM library checks and 45 recovery/fold browser contracts pass,
  with strict core/WASM/WASI lint
  ([receipt](editor-performance/recovery-wire-decoding-browser.json)).
  The full Spin build and five production recovery checks also pass: real
  edits, autosave, restart and reload/new-window restoration in both modes
  ([receipt](editor-performance/recovery-wire-production.json)). Physical
  folder/PWA/IME verification and final integrated CI remain pending.
  Guard comparison now borrows committed source, selection and collapsed-fold
  fields through the same source-selection policy as snapshot capture. It uses
  the matching privately validated document cost, avoiding new fold/selection
  vectors. Initial capture, path/record metadata, source/fold equality and
  serialization still have synchronous costs. Both-mode equivalence now covers
  twenty state changes. All 775 core tests, 133 native frontend tests, 52 WASM
  library checks and 45 recovery/fold browser contracts pass with strict
  core/WASM/WASI lint
  ([receipt](editor-performance/borrowed-recovery-guard-browser.json)).
  The full Spin build and five production recovery checks also pass for the
  borrowed guard checkpoint
  ([receipt](editor-performance/borrowed-recovery-guard-production.json)).
  Final integrated release verification remains pending.
  A shared core capture builder now retains source/scalar metadata and copies
  at most 64 collapsed candidates per step. Copy-on-write fold ownership lets
  it reject changed sources, baselines, selections and fold tables without
  retaining a cloned fold tree. Synchronous capture drives the same builder.
  All 777 core and 133 native frontend tests, 52 WASM library checks and
  45 recovery/fold browser contracts pass with strict core/WASM/WASI lint
  ([receipt](editor-performance/recovery-capture-core-browser.json)).
  Frontend snapshot and guard callers
  still need to yield between capture steps. Initial allocation, admission,
  final comparisons and serialization also remain open.
  Autosave now uses the shared cooperative facade for fold capture and record
  validation. Ownership checks reuse source selection and metadata matching;
  completed captures retain scalar ownership through validation. A private
  account-safe capture lock prevents overlapping work without rescheduling
  unchanged snapshots. Save API validation also yields. Native core/frontend
  verification passes. All 53 WASM library checks and 45 recovery/fold browser
  contracts pass, including eight capture/cancellation cases per mode and
  existing slow-write coalescing, with strict core/WASM/WASI lint
  ([receipt](editor-performance/cooperative-autosave-capture-browser.json)).
  The full Spin build and five production autosave recovery checks also pass
  ([receipt](editor-performance/cooperative-autosave-capture-production.json)).
  Final integrated CI and physical verification remain pending.
  Initial loading now prepares its guard through the same cooperative facade,
  checks document membership/revisions across preparation and reuses captured
  occupancy instead of taking another snapshot. All 133 native frontend tests,
  54 WASM library checks and 45 recovery/fold browser contracts pass with
  strict WASM lint
  ([receipt](editor-performance/cooperative-load-guard-browser.json)).
  Review and confirmation now call the same cooperative guard facade. Review
  preparation shares the existing account/project/handle/bridge predicate with
  disk I/O. Confirmation requests carry their own tickets and cannot supersede
  another open dialog or apply an older request after a newer intent. All 54
  WASM library checks and 45 recovery/fold browser contracts pass, including
  superseded confirmations and review cancellation, with strict WASM lint
  ([receipt](editor-performance/cooperative-review-guard-browser.json)).
  The exact folding-history fixture now uses a fixed parser clock after one
  provider-setup failure under host load; deadline contracts stay separate.
  All recovery action capture callers are cooperative. Initial allocation,
  admission, final comparisons and serialization
  remain open. The combined full Spin build and five production recovery
  checks pass at `bbb63749`
  ([receipt](editor-performance/recovery-combined-production.json)).
  Final integrated CI and physical-device gates remain pending.
  Acknowledged recovery records now use immutable shared ownership, avoiding
  fold/selection vector copies when autosave or reactive state retains the
  previous revision. All 134 native frontend tests pass, including a
  10,000-fold retention/replacement regression. All 54 WASM library checks
  and 45 recovery/fold browser contracts pass with strict WASM lint
  ([receipt](editor-performance/shared-acknowledged-record-browser.json)).
  Fresh production verification of this representation, final equality scans
  and serialization remain open.
  Autosave snapshot equality now uses shared core comparison preparation,
  checking source bytes in 8 KiB units and fold/selection records in 64-record
  units. It preserves derived equality and publishes only a complete result;
  superseded timer/account/project scopes discard the result and recapture.
  All 778 core and 134 native frontend tests pass, including fifteen comparison
  cases across both modes and three budgets. All 55 WASM library checks and
  45 recovery/fold browser contracts pass with strict core/WASM/WASI lint
  ([receipt](editor-performance/cooperative-snapshot-comparison-browser.json)).
  The full Spin build and five production recovery checks pass for combined
  acknowledged-record retention and cooperative snapshot comparison
  ([receipt](editor-performance/snapshot-comparison-production.json)).
  Final integrated CI, other guard equality and serialization remain open.
  Recovery saves now use a shared Rust encoding preparation: 6 KiB source
  blocks preserve canonical base64 seams, 64-record metadata batches preserve
  serde arrays, and one reserved body avoids repeated full-buffer growth.
  Account checks run between batches; encoded JSON uses the same HTTP
  authentication/error primitive as ordinary JSON requests. All 780 core and
  134 native frontend tests pass with strict core lint, including byte-for-byte
  wire oracles, Unicode/escaping/seams and metadata capacity. All 56 WASM
  library checks and 45 recovery/fold browser contracts pass with strict
  core/WASM/WASI lint
  ([receipt](editor-performance/cooperative-recovery-encoding-browser.json)).
  Initial allocation, network buffers, backend/storage serialization and guard
  equality remain open; browser body conversion is addressed below.
  The full Spin build and five production recovery checks pass for the
  bounded encoder at `28abc3a8`
  ([receipt](editor-performance/recovery-encoding-production.json)).
  Recovery upload preparation now copies 8 KiB byte pieces into immutable
  Blob parts, yielding and checking account ownership between pieces. The
  final application/json Blob uses the same HTTP primitive. Browser tests
  cover UTF-8 seams, cancellation, MIME type, request body/header preservation
  and acknowledgements. All 134 native frontend tests, 58 WASM library checks
  and 45 recovery/fold browser contracts pass with strict WASM lint
  ([receipt](editor-performance/cooperative-recovery-body-browser.json)).
  The full Spin build and five production recovery checks pass at `98bb84bd`
  ([receipt](editor-performance/recovery-body-production.json)). Final integrated
  CI remains pending for encoding and Blob upload preparation. Initial allocation,
  native Blob assembly, browser/network memory, backend/storage serialization and
  guard equality remain open.

  Recovery upload encoding now streams directly from the shared Rust phase
  machine into immutable byte-backed Blob parts. No complete encoded Rust
  body is reserved or retained on this upload path. Cumulative size admission
  spans drained chunks, and successful exhaustion plus current account ownership
  is required before constructing/sending the final body. The complete-string
  encoder and chunk encoder share the same wire-format implementation.
  All 781 core tests, 134 native frontend tests, 58 WASM library checks and
  45 recovery/fold browser contracts pass with strict core/WASM/WASI lint
  ([receipt](editor-performance/streamed-recovery-upload-browser.json)). The full
  Spin build and five production recovery checks pass at `4fd30bad`
  ([receipt](editor-performance/streamed-recovery-upload-production.json)). Final
  integrated CI, native Blob assembly/network memory and backend/storage
  serialization remain open.

**Remaining completion gates**:

Hosted run `38255429450` attempts every compiled editor partition and reports three
failed browser runs: cached horizontal readiness, wrapped replay renderer
responsiveness, and 15 isolated library tests without an initialized Leptos
executor. The recovery checkpoint guards the reproduced disposed-cache effects;
isolated editor library fixtures now initialize the shared runtime explicitly.
All 30 isolated library contracts pass with the official runner (two CPUs/four
GiB), alongside strict WASM lint
([receipt](editor-performance/isolated-library-runtime.json)). Hosted verification
of the fixture repair and renderer failures now passes at `67b3a529`
([receipt](editor-performance/editor-ci-67b-success.json)).
Keep the full inventory and existing contract/browser deadlines. The unchanged
wrapped replay contract now passes the official runner on Linux arm64 with two
CPUs/four GiB in 87.29 seconds
([receipt](editor-performance/wrapped-replay-current-official.json)); hosted
Linux/x64 verification also passes in the complete `67b3a529` inventory.

- [ ] **Responsiveness and memory:** remove measured input, cold-paint, wrapped
  geometry and process-memory stalls. Repeat admitted byte, row-count and long-line
  boundary workloads in both modes, wrapping on/off, beginning edits and startup/
  scroll shaping. Verify complete source after input, actual near-1-MiB String
  styling and Linux Chrome PSS. Multi-second preparation and the current latency/
  memory results do not satisfy this gate. Preserve source ownership, bounded
  terminal fallback, grammar paint coalescing and localized reuse.
- [ ] **Exact geometry and fallbacks:** preserve complete-renderer extent, anchor
  and hit comparisons; font/family/feature/whitespace matrices; Unicode/caret mapping;
  partial endpoint rejection; sparse gaps; ambiguous wrap boundaries; unsupported
  layouts and failed-proof fallback. Verify source/account/project ownership,
  pending edits, previews, agent context, themes and both adapter contracts.
- [ ] **Reliable CI and integrated release checks:** complete native/WASI/WASM,
  platform and release checks on combined editor/plugin source. Investigate the
  cached-horizontal-paint timeout and disposed-reactive-state failures from release
  run `38255429450`, and verify the subsequent runtime/owner fixes on hosted Linux.
  Running releases now finish while newer pushes replace only the pending build;
  the complete test and platform-image gates still apply before publication.
  Investigate any new readiness/order failures and repeat near-limit Linux/x64
  readiness with parser/paint/geometry diagnostics, unchanged timeouts and probe
  limits. Preserve complete compiled inventories, independently checked disjoint
  groups of at most 64 editor tests, the 300-second browser deadline, separate font
  matrices and every other UI partition. Repeat warm highlight/row reuse,
  file-switch cancellation, cold queued-input source/selection ownership,
  host-plugin focus/layout and explicit Output/terminal targeting. Validate bounded
  setup retries after upstream Chrome download failures. Latest verified checkpoint
  `67b3a529` passes all five check jobs, both platform images and nightly
  publication on integrated editor/plugin/host source, including all 216 editor
  contracts and both font matrices
  ([receipt](editor-performance/editor-ci-67b-success.json)). Repeat this gate for the final
  integrated editor implementation before closing.
  Subsequent checkpoints `607f7c46` and `420e03aa` also pass the complete hosted
  release workflow. The nightly-version change in `915c07b7` passes native,
  frontend, Docker, Windows and both image jobs but fails the release-notes browser
  assertion: nightly notes now have an Unreleased heading. The contract now
  checks that heading while retaining account acknowledgement and dismissal;
  four fresh browser contracts pass, including About in both modes and focus
  restoration ([receipt](editor-performance/nightly-release-notes-browser.json)).
  Hosted release `38304755880` at `17934e1c` subsequently passes all six check
  jobs, both platform images and nightly publication. The full browser inventory
  includes 219 editor checks, both font matrices and 397 other component checks
  ([receipt](editor-performance/editor-ci-179-success.json)), verifying the
  nightly-heading correction on integrated editor/plugin source. Repeat this
  gate for the later ASCII coordinate updates and final editor implementation.
  The subsequent integrated release `38312660109` at `91f8bfd5` also passes
  all six check jobs, both platform images, 219 editor checks, both font matrices,
  397 other component checks and nightly publication
  ([receipt](editor-performance/editor-ci-91f-success.json)). Later recovery
  decoding/comparison changes pass the same gate at `5ba57f8e` in hosted run
  `38318137783`: all six check jobs, both images, 220 editor checks, both font
  matrices, 397 other component checks and nightly publication pass
  ([receipt](editor-performance/editor-ci-5ba-success.json)). Later guard,
  collapsed capture passes the same complete release gate at `cbbb2209` in
  hosted run `38320950198`
  ([receipt](editor-performance/editor-ci-cbbb-success.json)). The integrated
  `923e5112` release also passes all six checks, both platform images, nightly
  publication, 220 editor checks, both font matrices and 397 other component
  checks after bounded wire decoding/capture, cooperative autosave and initial
  guard preparation
  ([receipt](editor-performance/editor-ci-923-success.json)). Hosted run
  `38332700686` at `7f71bbbf` also passes the complete release gate after
  review/confirmation, immutable records and cooperative comparison: 220 editor
  checks, both font matrices, 397 other component checks and nightly publication
  ([receipt](editor-performance/editor-ci-7f71-success.json)). Subsequent encoding
  and Blob upload preparation pass the complete release gate at `98bb84bd`,
  hosted run `38335437540`: 220 editor checks, both font matrices, 397 other
  component checks, both images and nightly publication
  ([receipt](editor-performance/editor-ci-98bb-success.json)). Direct encoder-to-
  Blob streaming and incremental ASCII row indexes still need the final gate.

- [ ] **Physical Chrome/Edge PWA input:** verify real input-method commit/cancel,
  Unicode and LF/CRLF undo/redo, multiple-cursor clipboard transactions and touch
  without rewriting unrelated source. Automated Chromium composition/pointer
  contracts support this gate but do not replace device verification.
- [ ] **Folder permissions and recovery:** expand permission/error contracts and
  verify real local directory-handle loss/regrant and reload recovery. Preserve
  stale account/folder/bridge/project rejection, held/failed/background saves,
  newer edits, disk reload during saves and late acknowledgements. Coordinate with
  Offline & error-state recovery below; a disposable test without a native handle
  cannot establish physical folder recovery.
- [ ] **Accessibility and integration:** verify keyboard focus/Tab escape,
  assistive technology, touch, themes and PWA loading across cold/fallback paths.
  Assess the projected native input/paint surface for richer multiple selection.
  Automated names, Ctrl+M announcements and trusted Tab escape already have
  contracts; physical assistive-technology behavior remains unverified.

Checkpoint-by-checkpoint results, failed experiments and earlier pending claims
are preserved in [checkpoint history](editor-performance/editor-roadmap-checkpoint-history.md)
and [performance evidence](editor-performance.md). They are evidence for specific
checks, not substitutes for the outstanding requirements above.

**Rust/WebAssembly architecture:** keep the editor in Rust/Leptos compiled to WASM;
do not embed CodeMirror, Monaco or another substantial JavaScript editor client.
Those editors are feature references only. Build reusable Rust document, selection,
edit-transaction, undo/history, command and fold-range primitives, with one Leptos
editor component. Evaluate WASM-compatible Rust crates for text storage and syntax
parsing before choosing dependencies. [Native/browser storage measurements](editor-performance.md)
compare the current document with Crop/Ropey edits, display materialization and
UTF-16 indexing; retain current production storage until viewport/worker work
changes its access pattern.

Keep browser glue thin: DOM events, input/IME,
selection, clipboard, measurements and worker transport; editing policy and algorithms
belong in Rust. Preserve native browser input behavior where possible, and evaluate
a richer view for multiple selections beyond the current projected textarea/paint
surface.

Workspace reads/writes, settings and review policy stay in shared facades. Save user
preferences in database settings, never localStorage. Verify touch/IME/accessibility,
Unicode/caret mapping, incremental parsing, large-file responsiveness, theme integration
and PWA loading. Benchmark viewport rendering and establish large-file fallbacks before
claiming completion; run behavioral contracts in both modes.

Completion, diagnostics, hover, symbol navigation, rename, formatting and code actions
belong to the following Code intelligence item, using the editor's extension points.
Minimap, Vim/Emacs emulation and similar extras are optional later work.

Research references: [VS Code editing](https://code.visualstudio.com/docs/editing/codebasics),
[CodeMirror baseline](https://codemirror.net/examples/basic/),
[CodeMirror Tab accessibility](https://codemirror.net/examples/tab/),
[CodeMirror features](https://codemirror.net/),
[Monaco support/architecture](https://github.com/microsoft/monaco-editor),
and [EditorConfig](https://editorconfig.org/).

### Code intelligence: host language services, LSP & caching

Treat the homelab server and paired native bridges as persistent development
hosts. Language services belong to a project on its execution host, rather than
to an editor tab; browsers, phones and agent runs are clients of those services.
Remote projects use the server bridge; local projects use a paired bridge with
access to the selected folder. Language plugins and their runtimes install and
execute on those hosts, never on browser clients.

Build on the Full code editor component and its document/selection/extension APIs.
Bring real-time code intelligence (syntax errors, lint squiggles, tooltips,
autocomplete) into the editor while keeping the core diagnostics engine
100% shared between Remote and Local mode:

- Host lint/analysis providers, including lightweight WASM providers where useful,
  producing a shared `Diagnostic` struct for the editor's squiggle overlay.
  Browser code renders results and implements core editing; it does not execute
  plugin analyzers. Preserve core editing when no suitable host is available.
- Host LSP multiplexed over the Phase 11 bridge
  (`rust-analyzer`, `pyright`, `vtsls`) for cross-file go-to-definition,
  hover, autocomplete, document symbols/outline, references, rename, code actions
  and document/selection formatting when a host toolchain is available.
- One diagnostics UI regardless of whether a diagnostic came from a host linter
  or language server, with matching local/remote feature and failure contracts.
- Manage language-server lifetimes independently of client connections. Keep
  useful servers and project indexes warm across tab closure, device changes and
  brief disconnects, with bounded idle time, memory, process counts and eviction.
  Start services on demand; do not keep every installed language or project
  running indefinitely. Reconnect clients to current project services, recover
  from host/server restarts, and allow plugin updates at controlled service
  boundaries without changing providers beneath in-flight requests.
- Expose diagnostics, symbols, definitions and references through one shared
  language-service facade to editor features and agent tools. Reuse the same
  project configuration, toolchain and pinned plugin versions across clients;
  project/account access and normal approval rules still apply to agent actions.
- Begin caching with server-owned indexing and supported persistent caches,
  rather than duplicating language-server internals. Measure cold/warm latency,
  cache hits and resource cost before adding OpenWebIDE result caches. Preserve
  host artifacts outside user workspaces in persistent host storage; cache
  contents are disposable derived data, not the source of installation settings
  or user preferences, which remain in the database.
- Add bounded, cancellable prefetching for likely files: open tabs, recent edits
  and dependencies identified by providers. Prefer interactive requests over
  speculative work, avoid eagerly opening every project file, and limit indexing
  and prefetch concurrency/resource use. Initially target reusable diagnostics
  and document symbols; only cache context-sensitive features such as completion
  when their complete request identity and invalidation contract are known.
- Reuse validated results across clients for identical project/document state.
  Key results by user/project/host, document content or revision, project and
  dependency state, effective configuration, and plugin/server version. Isolate
  unsaved buffers by client/session identity unless explicitly shared. Invalidate
  after edits, external file changes, branch/dependency/configuration changes or
  provider updates; never publish stale results after ownership or document
  changes. Expensive cross-file analysis should identify the project generation
  it analyzed, not just the active file revision.
- Verify the same service/cache contracts against both host adapters: cold/warm
  use, device reconnect, concurrent clients with different unsaved buffers,
  external edits and branch switches, cancellation, eviction, provider updates,
  host outages and restart recovery. Benchmark interactive latency and bounded
  resource use on a small homelab before claiming prefetching improves performance.

Expose language features through the shared contribution contracts described in
[Plugins & Git marketplaces](#plugins-git-marketplaces), so existing
languages can become bundled plugins and community packages can add languages.
Defer the language-plugin migration until the ongoing Full code editor work is
complete; preserve the shared document/selection/extension APIs as its foundation.

### Editor Git annotations

- Deliver GitLens-style line authorship, commit details and history as a bundled
  first-party plugin and reference implementation of
  [editor contributions](#plugins-git-marketplaces), after the shared editor APIs
  and relevant plugin contracts are available. Navigate to the relevant commit
  or diff through existing [Git pane](git.md) actions.
- Use typed, declarative editor annotations for inline/gutter labels and hover
  details, rendered by core components; document/selection hooks request refreshed
  data from host handlers. Keep the contribution API reusable by other plugins
  rather than hardcoding blame-specific UI or allowing browser plugin code.
- Reuse shared Git orchestration and existing bridge adapters for blame/history
  in both modes. Associate annotations with repository state and document
  revisions, handle unsaved line shifts and uncommitted lines explicitly, and
  discard stale results after file, project, account or host changes. Bound and
  cache host queries, cancel obsolete work, and clear unavailable annotations
  without blocking editing when a host or repository is unavailable.
- Verify annotation placement and navigation in both modes, including unsaved
  edits, branch switches, reconnects and plugin disable/update. Use this package
  to validate independent editor-contribution enablement, theme/accessibility
  behavior and shared action routing; do not mark shipped before the plugin path
  and both host adapters are verified.

### Test discovery, running and debugging

- Discover tests for supported languages and frameworks in both modes. Initially,
  expose inline **Run test** actions in remote projects, in the style of Rider/Visual
  Studio; local projects can show discovered tests with execution unavailable.
- In remote mode, run individual tests, suites or all project tests through the
  existing server-side bridge, with results and failure
  locations linked back to the editor. Stream runner stdout/stderr to the terminal,
  retaining output alongside structured test results. Define supported frameworks
  and runtime requirements explicitly.
- Assess remote debugger integration through Debug Adapter Protocol (DAP): the
  server-side bridge hosts debugger adapters, and the browser provides breakpoints,
  stepping, stack frames and variable inspection. Route debuggee output to the
  terminal. Add inline **Debug test** only for validated runner/debugger combinations,
  including test-process launch/attach, source mapping and clean session shutdown.
  See [DAP architecture](https://microsoft.github.io/debug-adapter-protocol/overview.html).
- **Initial scope exception:** test execution and debugging are remote-only at first.
  A local companion bridge is outside this feature's scope. Keep local execution and
  debugging parity as remaining work requiring a browser-compatible runtime; do not
  mark the overall feature complete when only remote execution ships. Source-based
  discovery uses `Workspace` in both modes; unavailable actions explain the runtime
  limitation.
- Keep discovery, action availability and result handling shared across modes;
  use thin runtime adapters for execution and explain missing capabilities.
  Coordinate editor integration with Code intelligence.

### Selection context menu and agent actions

- Right-click highlighted code to open a shared context menu with editor and
  agent actions, such as explain, refactor or generate tests.
- Carry the selected text, file path and line range into the agent request and
  pending-edit review; reject stale selections after document or project changes.
- Offer the same actions through keyboard and touch controls in both modes.


### Phone device verification

The compact app/editor rows, universal search, logo drawer, responsive workspace status controls,
on-demand pane controls, grouped menus, responsive dialogs, nested file groups, automatic tree density, folder-only creation menus, middle-click tab closing, pointer-stable tab scrolling, tree controls and Git icons/counts are implemented in
both modes, with browser coverage for narrow layouts, focus restoration, keyboard
navigation, inline header search with viewport-bounded results, stable file-tab geometry across preview availability changes and loading locks, contained tab context-menu controls, non-overlapping tree disclosure touch targets, tooltip dismissal during pointer activation and scroll restoration, and review safeguards. Remaining checks need physical devices:

- Verify density, model/approval controls, activity-group touch controls and the
  compact context indicator on real phones in both modes.
- Check virtual-keyboard and IME behavior on iOS/Android so the composer/editor
  space and controls remain accessible as the keyboard opens and closes.
- Check safe-area spacing, the app drawer and Output sheet on real devices.

### Build, test and process output

Prioritize the existing terminal pane as a useful place to follow build, test and
other process output: readable streamed logs, separate runs, exit status, cancellation,
copy/search and file/line links back to the editor. Preserve output during reconnects
and keep runs scoped to their project/session. Use shared output behavior with thin
runtime adapters; local output is available only for browser-supported execution,
while host-native builds/tests initially run in remote mode.

- When a process starts, reveal output at a modest desktop height while honoring
  explicit hide/resize choices. Verify Output sheet focus/keyboard behavior on
  real devices.
- Keep running/failure status visible through the shared footer's Output indicator
  when the pane is hidden. Add output search, separate run histories and file/line
  links.

Prioritize idle output behavior, phone control overflow and activity-group density
on phones alongside the compact-layout work above. Verify both-mode output contracts and
mobile focus/keyboard behavior before considering these refinements complete.

A fully interactive terminal (cursor movement, direct keyboard input and shell/TUI
programs) is optional later work, pending a concrete need beyond process output.

### Process execution: tool discovery and headless browser

Remaining execution-host work:

- **Headless browser via Chrome DevTools Protocol (CDP):** the bridge
  attaching to a host or sidecar Chrome/Chromium instance to give the agent
  `browser_navigate`/`browser_screenshot`/`browser_click`/`browser_type`/
  `browser_console_logs` tools, failing open to `fetch_web_page` when no CDP
  browser is reachable.

## Later

### Plugins & Git marketplaces

Extend agent and editor capabilities through versioned plugin packages that users
and the agent can manage, without requiring changes to the app for each new
capability.

- Finish manual PWA lifecycle checks on server and paired hosts: asynchronous
  install/cancel and failure recovery, cold action caches, compiled-default
  initialization, and install/run/update/disable/uninstall flows.
- Complete remaining Scheduling browser checks for deletion and bounded monitor
  lifecycle, retained legacy task adoption, pending-occurrence handoff, active
  invocation
  pinning and updates during streamed work, interrupted-run recovery, history
  retention and full-quota journaling/recovery. These extend verified production
  SDK flows through both host transports; retain active work and existing data.
- Extend authenticated, run-pinned collection callbacks with further app data
  and workspace primitives. Publish a registry release of the SDK, extending the
  available pinned public Git SDK and verified custom-marketplace authoring flow.
- Extend executable contributions with MCP servers and dependency/configuration
  validation, using the existing versioned schemas and capability grants.
  Build on the MCP client and [database-backed project skills](agent-skills.md);
  keep discovery and context loading bounded through deferred tool loading.
  Add further run hooks when needed, building on the shared read-only SDK context
  planner. Core workspace tools remain built in.
- Extend the existing host-side catalog checks to unattended Automatic activation,
  preserving compatible-version policy, capability review, installation revisions,
  project opt-outs and active-run pins. Notify remains the default. Catalog checks
  already run with clients closed; automatic installation currently requires an
  open client.
- Use one package format with typed contributions rather than mutually exclusive
  plugin types. A package can combine skills, MCP servers/tools and language
  support, declarative panels/editor annotations and eventual editor hooks; validate compatibility,
  configuration and permissions for each
  contribution. Marketplace categories describe what a package offers without
  selecting separate installation or lifecycle implementations. Install packages
  and execute plugin code strictly on bridge hosts, never on browser clients or
  phones. Clients consume plugin metadata, instructions and service results
  through the shared facade/bridge; they do not maintain plugin clones, execute
  plugin WASM modules or install plugin runtimes.
- Extend the docs [panel playground](panel-playground.md) with a visual block
  builder once the schema and renderer are stable. Keep visual editing on the
  existing JSON document, production validation, simulation and export path
  rather than introducing another UI format. The component gallery, editable
  JSON, live preview, theme/width previews and JSON/Rust exports are available.
- Add editor hooks later as typed contributions in the same package format,
  after the shared editor transaction/save APIs are stable. Start with bounded
  host notifications such as document opened/changed/saved/closed; debounce
  high-frequency events and send only declared, authorized document context.
  Distinguish asynchronous notifications from request/response hooks such as
  before-save transformations. Run handlers on the owning bridge host, never
  browser clients; foreground hooks need explicit timeouts, deterministic ordering
  and unavailable-host/error/cancellation behavior. Hook-proposed edits pass
  through shared editor transactions and existing review/approval policy, with
  document revision checks, undo support and protections against recursive
  save/edit events. Reuse language-service formatting/code-action contracts where
  applicable rather than creating competing edit paths. Allow hooks to be
  disabled independently; bound background work and preserve normal editing/save
  on optional hook failure. Verify both host adapters, plugin updates/disablement,
  concurrent edits, disconnects and stale account/project/session results.
- Pair editor hooks with a versioned declarative annotation contribution API for
  source ranges/lines, inline and gutter content, hover details and declared
  navigation actions. The host supplies bounded data tied to document revisions;
  core editor components own rendering, theme tokens, placement, accessibility
  and conflicts between providers. Treat annotations as a presentation contract,
  separate from hook delivery and document edits. Use the bundled
  [Git annotations plugin](#editor-git-annotations) as the first reference package.
- After the ongoing Full code editor work is complete, support language
  contributions for file recognition, syntax highlighting/folding, comment and
  indentation rules, snippets, formatting, host diagnostics and LSP
  configuration. Build on Code intelligence above; language packages may provide
  only a subset of these features and may also include agent skills or tools.
  Use a shared language-service facade for document synchronization, feature
  routing, diagnostics, conflicts and failure handling, with thin host adapters
  for running plugin syntax analysis and servers beside local or remote project
  files. The core editor renders shared results and applies declarative editing
  rules without executing plugin code in the browser. Preserve editing with a
  core plain-text fallback when the host is unavailable. Guard results by document
  revision and account/project/session ownership; define competing-provider
  selection, missing-runtime behavior, server recovery and formatter precedence.
- Move all existing language support behind those same contribution contracts,
  shipped as bundled packages enabled by default to preserve today's experience.
  Distinguish bundled artifacts, installed versions and enablement; record
  installation/configuration in the database and activate language services on
  matching files rather than starting every installed server. Bundled packages
  use the same validation, configuration, conflict and lifecycle rules as
  community packages. Compiled-in grammars can initially register through the
  contract, but externally supplied syntax grammars require a versioned loading
  and runtime contract before claiming full language extensibility.
- Publish first-party reference packages through the official marketplace using
  the community package format and permission rules: a GitHub integration with
  authentication, MCP tools and review/issue skills, extending the existing
  skills-only PR Review plugin; and browser testing with host dependencies
  and artifacts. Add Terraform after
  language contributions are available to demonstrate file recognition, syntax,
  formatting and an existing LSP server. Keep upstream runtime/server versions
  pinned and distinguish OpenWebIDE-maintained packaging from upstream ownership.
  Include the bundled Git annotations package as the reference for editor hooks,
  declarative annotations and navigation through existing Git actions.
- Make language packages discoverable when an unsupported file opens. Publish
  searchable language metadata (file extensions, exact filenames and optional
  content signatures, plus the features supplied) in the official catalog in the
  OpenWebIDE GitHub organization and use the same metadata for custom catalogs.
  Fetch/cache the index and match locally; do not send file contents or project
  paths to catalog hosts. Show a dismissible suggestion with the package's source,
  publisher and supported features, and offer inspection/install without
  automatically installing, enabling or running it. Respect dismissed suggestions,
  avoid repeated prompts, and keep editing usable during catalog outages. Share
  matching and suggestion policy across modes, guard stale discovery results,
  and persist user preferences/dismissals in the database. A cached catalog is
  only a discovery hint; installation still verifies the pinned package release.
- Extend the configured public Git catalog sources with recognized forge file URL
  normalization, custom source disabling and private Git/forge credential adapters.
  Keep one repository/ref/catalog-path record and the same catalog contract for
  every source; credentials stay out of manifests and agent context. Add user
  defaults and project-specific configuration alongside existing per-project
  enablement. When release archives are supported, verify their digests. Optional
  GitHub topic discovery can follow the catalog/direct-install baseline.
- Add user/project configuration to the existing database-backed marketplace,
  installation and enablement records. Preserve source/publisher identity,
  immutable release commits and content digests, explicit activation and cached
  discovery during outages as additional contribution types arrive.
- Extend host preparation to pinned runtime dependencies and language services.
  Opening an unsupported Terraform file can suggest its catalog package;
  installation prepares it on the selected project's bridge and the editor uses
  its services through that bridge. Report missing prerequisites or prepare
  supported pinned runtime artifacts with approval before reporting native
  features ready. Installation does not require a container checkout before a
  local host checkout. Keep dependencies separate from package Git snapshots,
  prepare them in staging, preserve snapshots needed by active jobs/rollback,
  and rebuild host caches from logical database records. Local projects still
  require a paired bridge with folder access; phones viewing remote projects
  consume the server host's services without installing client packages.
- Extend update policies with version/channel constraints and explicit opt-in to
  tracking a development branch. Require renewed approval for expanded
  permissions or changed credential access before activating an update. Extend
  immutable version pinning to runtime dependencies and language-service
  sessions, including safe service transitions and preservation of active jobs.
  Keep catalog refresh separate from package activation.
- Extend run pinning to plugin configuration and dependency versions as those
  contributions arrive. Validate readiness before activation and prevent plugin
  handlers from granting themselves capabilities or changing approval policy.
- Extend the durable dispatcher used by [Monitors](monitors.md) for plugin
  callbacks and continuing skill jobs. Define trigger contracts, persist progress,
  results and pending approvals, and pin each job to its plugin version. Verify
  restart recovery, deduplicated delivery and plugin disable/remove/update while
  jobs are pending through the shared facade in both project modes.
- Extend the existing shared plugin facade and thin local/remote adapters to
  dependency resolution, new runtimes and context contributions. Add matching
  contracts for private repositories and new contribution types, including failed
  preparation, updates/rollback, disablement, outages, concurrent changes and stale
  results after account/project/session changes. Demonstrate an agent adding
  a tool plugin, testing it in both modes, enabling it for a new run and rolling it
  back.

### Multi-user

Only needed once more than one account can exist (today registration closes
after the first account):

- **Admin-only writes** for connections and server/model configuration (they stay
  shared, admin-owned), and admin-only `browse` and remote-project paths
  (`require_admin`).
- **Ownership columns** where rows are per user, and session-data store
  methods that take the `user_id` (or an owned-session token) instead of
  relying on every call site to check.
- **Registration setup token** with a loopback-only default when none is
  set (a Spin variable; verify Spin sets `spin-client-addr`).
- **Per-user bridge terminals:** sessions owned by the `user_id` in the
  bridge token, so one user can't list, attach to or kill another's shells;
  and the bridge's acting-user header limited to the connection's own user.

### Sandboxed tool execution (optional)

The VFS already confines the file tools (`read_file`, `write_file`,
`list_dir`, `search`) to the working directory, but `run_command` and the
git tools spawn real host processes as the user, which the VFS doesn't
cover. Approval modes now allow commands to run without asking. Offer an
opt-in mode that runs the agent's tool
executor in a container or as a restricted user with only the project folder
mounted and optionally no network, while the user's own terminal keeps full
access. Off by default; recommended alongside YOLO mode. Use the existing
shared tool-execution trait.

### Productionization & public release (1.0)

The milestone that marks **1.0** — the first stable version tagged for public use. A hygiene and packaging pass once the hardening sequence, refactors and the main Next features have
landed — before sharing the repo publicly.

Release automation, download-only Compose/Quadlet files, support issue forms,
configuration/upgrade/backup guidance, architecture diagrams and font credits are
prepared. See [releases and upgrades](releases.md). Version/changelog gates,
archive/checksum contracts, workflow lint and Compose configuration checks cover
this preparation. Alpha images and release assets are published; anonymous pull
and release-host verification remain outstanding.
Documentation now has [section overviews](index.md), first-session/workspace guides
and curated site navigation. Complete the remaining task guides and installation
walkthroughs against the final app before public launch.

- **Release gates:** finish the hardening/editor dependencies and the Remaining
  manual verification checks above. Do not declare 1.0 while both-mode contracts,
  device checks or known responsiveness issues remain unresolved.
- **Repo hygiene:** finish the unused-code/dependency/configuration sweep and remove
  confirmed scratch files and stale specs once concurrent development is finished.
  Generated Python caches, browser test configuration and output are now ignored;
  existing native/WASI/WASM fmt/clippy/tests remain release gates.
- **User documentation:** expand model/provider setup, approval modes and agent
  workflows, sessions, Git, commands/output and troubleshooting into task guides.
  Validate them on a clean installation and on both workspace modes; refresh
  editor and mobile walkthroughs after the ongoing UI work lands.
- **Installation and upgrade rehearsal:** exercise download-only Compose and rootless
  Quadlet (including registry auto-updates), database backup/restore/upgrade and both
  workspace modes on release hosts. CI verifies both native image architectures,
  bridge archives, HTTPS transport and generated Quadlet services; actual systemd
  service and update-timer checks remain hands-on.
  Rehearse Settings → Host → Updates on real Compose and user Quadlet installs,
  including host prerequisites, digest pins, failed deployments and reconnects.
  The settings/actions, durable claims and detached SSH worker have shared local/
  remote browser contracts and controlled deployment-fixture coverage.
- **Prerelease installation verification:** make the GHCR package public and verify
  anonymous pulls on both architectures, then validate a clean prerelease install
  before using alpha/preview distribution for feedback; this does not complete 1.0.
- **First stable publication:** move shipped changelog entries to `[1.0.0]`, bump workspace,
  Spin and lockfile versions together, and publish `v1.0.0` after rehearsal passes.
  The prepared workflow runs CI, merges native platform images into
  `ghcr.io/openwebide/openwebide:v1.0.0` and `stable`, and publishes changelog-derived
  notes, archives, install files and checksums using `GITHUB_TOKEN`. Make the GHCR
  package public and verify anonymous pulls and clean installs on both architectures.
  Shared CI checks generated Quadlet services against the `stable` image channel.
  The pinned alpha is `1.0.0-alpha.6`; main builds publish rolling `nightly` images. Alpha publication does not complete
  the stable-release gates.
- **Public launch:** update the [site landing page](project-site.md) and pre-release
  documentation wording for the actual release, then prepare community announcements
  and directory submissions. Support uses the new GitHub issue forms; an optional
  demo remains future launch work.

### Offline & error-state recovery

- Frontend heartbeat to `/api/health` with exponential backoff reconnection.
- Preserving unsaved editor state and draft prompts across connection dropouts.
- Graceful re-authorization flow for local File System Access API directory handles.

### Parking lot

Ideas without a phase yet:

- Multi-model comparison for a single prompt (an Open WebUI classic).
- Bridge child-process environment allow-list with `--pass-env` — parked until the bridge
  is shared; commands inherit the user's environment on purpose (cloud creds, toolchains).
- Private-address egress blocking / an explicit outbound allow-list for the backend —
  parked; conflicts with LAN model hosts and LAN web fetch. Would also need resolve-then-pin
  to stop a hostname rebinding to the metadata address.

## Explicitly out of scope

- **Model installation and lifecycle management:** Pulling, downloading, or
  deleting model weights on disk (e.g. `ollama pull`, GGUF management). Open
  WebIDE is an IDE and agentic client runtime, not a model engine manager.
  Users deploy their own inference engines (Ollama, llama.cpp in Podman
  quadlets, or OpenAI-compatible endpoints) and point Open WebIDE at them via
  connections.
