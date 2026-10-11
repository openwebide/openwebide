# Editor roadmap checkpoint history

Historical implementation notes and results, preserved from the roadmap at
`4fb3c637`. Pending requirements are tracked in [the current roadmap](../roadmap.md);
these checkpoint results do not establish completion of the editor goal.

**Current measured bottleneck:** the merged production build preserves complete
source in all 24 boundary samples, but styled wrapped near-1-MiB edits take
233–242 ms to paint and 1.145–1.167 s to finish geometry. Cold complete geometry
takes 1.14–3.53 s across the workloads; peak Chrome PSS is 645–820 MiB
([results](merged-production-boundaries.jsonl)).
The next performance investigation is wrapped long-line preparation and replay;
these measurements do not complete the requirements below.

**Remaining implementation**:

- [ ] **Parser-aware ordinary input and decorations:** verify admitted
  HTML/Markdown input costs and budget fallbacks without delaying ordinary input;
  finish retained-fold/current-source ownership verification. The candidate
  skips lexical scans for unconditional literal input and requests editing
  structure without generating highlights/folds, sharing transactions, parser
  phases and source guards. It now exposes the runtime input clock so readiness
  and explicit expiry/fallback behavior can be tested deterministically in both
  modes. The browser's default clock and 12 ms budget are unchanged. All 632
  core tests, 128 current native frontend tests and strict native/WASM lint pass;
  all twelve focused browser contracts pass
  ([receipt](input-clock-focused-browser.json)). All 190 editor browser
  contracts, including both font matrices, pass
  ([receipt](input-clock-full-browser.json)); physical
  readiness/latency are not established by those contracts. That checkpoint's
  hosted CI passed all five jobs
  ([receipt](grouped-ci-7ff2462.json)).
  The earlier full candidate run passed 76 tests, failed held Markdown pairing
  and left 112 tests unexecuted
  ([record](input-structure-full-failure.json)). Instrumented
  diagnostics observed a cold Markdown preparation cancelling at 20 ms, then
  warm preparations of 0–1 ms
  ([record](input-preparation-diagnostic.json)). Changing
  readiness-test clocks does not resolve cold latency or guarantee pairing
  before context is available. Native debug fallback edit medians improved from
  50–96 ms to below 0.5 ms
  ([comparison](typing-preparation-native-comparison.json));
  real browser/release latency and hard per-phase bounds remain open.
  The native/WASM input harness now passes strict lint and all
  16 source/selection checks per output variant
  ([first release samples](input-preparation-release-samples.json)).
  Single samples show cold browser preparation exceeding the budget and a
  near-2-MiB Rust fallback edit around 8 ms; repeat and measure actual facade/view
  costs before claiming latency completion. Explicit structural actions use cooperative preparation
  with request-time cancellation.
  Oversized direct preparation now rejects before grammar construction, matching
  the worker's existing byte-admission ordering and releasing only the affected
  cached document. Its 633 native core tests and strict native/WASM library lint
  pass. This avoids unnecessary parser setup above the syntax limit;
  admitted-source preparation and physical cold readiness still need improvement.
  Editing-context preparation now uses the same cooperative source admission,
  comparison and row-index phases as background analysis, with continuation
  checks between batches. Its 634 core tests, 128 native frontend tests, strict
  native/WASM lint and twelve focused debug-WASM both-mode browser contracts pass
  ([receipt](input-bounded-preparation.json)). Repeat release
  and physical latency measurements before crediting the responsiveness gate.
  Both follow-on checkpoints passed all five hosted jobs
  ([early admission](grouped-ci-211574d.json),
  [budgeted input](grouped-ci-81d2d78.json)).

- [ ] **Cold startup and native input:** remove remaining full-source shaping in
  touch and unsupported-layout fallbacks. Desktop startup now uses bounded native
  input for wrapped and nonuniform rows, with exactly measured origin paint while
  complete document extents prepare. Finish production startup and memory costs
  across admitted boundary workloads; finish touch pointer selection and
  source-owned caret/selection while preserving composition mappings and complete-native fallback
  when bounded geometry cannot be proved. Basic direct and queued keyboard
  motion now uses editor admission instead of the separate 2 MiB structure
  budget; measured neighborhoods retain source and validate only measured rows.
  Go-to-line now uses retained logical rows and sparse character coordinates for
  current prepared documents, with source-only fallback when no current document
  exists. All 638 core tests and strict native core/WASM frontend lint pass;
  the release both-mode navigation and stale source/project regression passes
  ([receipt](indexed-navigation-browser.json)). Hosted CI
  for `cf66d9e` passes all five jobs
  ([receipt](grouped-ci-cf66d9e.json)).
  Neutral fallback now uses a complete current source-owned document index
  instead of allocating a second plain-token table; no-index callers retain
  cooperative preparation. Logical readiness is separate from geometry proof:
  unmeasured oversized rows retain complete native input, while source-owned
  measured prefixes and paragraph coverage permit partial paint.
  Deferred paint subscribes to memoized viewport readiness, and unchanged neutral
  frames reuse their HTML. The complete 193-test editor/worker browser inventory,
  129 native frontend tests and strict native/WASM lints pass; the previously
  failing 55-test group passes twice without viewport diagnostics
  ([receipt](indexed-neutral-browser.json)). Its production
  comparison exposes a wrapped row-limit regression: median input-to-paint rises
  from 247 ms to 1.12 s, with greater memory use
  ([record](indexed-neutral-production-comparison.jsonl)).
  The follow-up retains current source-owned native mappings through empty
  geometry viewports, while stale ownership and preparation errors restore
  complete native fallback. All 194 current editor/worker browser contracts,
  129 native frontend tests and strict native/WASM lints pass
  ([receipt](indexed-neutral-native-retention-browser.json)).
  All 16 current production samples preserve complete source after edits.
  Wrapped row-limit input improves from a 247 ms control median to 47 ms;
  memory returns near the control range
  ([comparison](indexed-neutral-native-retention-production.jsonl)).
  Cold startup still takes 1.20–1.28 s, and unwrapped input establishes no gain.
  Styled-boundary, overall memory and CI reliability gates remain open.
  Complete projection/native normalization, long word/context traversal and
  remaining synchronous selection publication still require bounded preparation.
- [ ] **Tabbed, wrapped and bidirectional layout:** extend bounded preparation
  beyond eligible source-monotonic wrapped paragraphs with complete-word seams;
  finish bidirectional visual-run windows, fine long-row paint and incremental glyph
  measurement. Eligible wrapped paragraphs now use bounded probes with retained
  paint-run clipping and exact overlap validation. Unsupported complete-layout
  fallbacks and touch native input shaping still run synchronously. Cold row
  geometry now enumerates text nodes cooperatively with lazy sibling traversal
  and source revalidation; synchronous pointer/neighborhood queries and complete
  DOM installation still require bounded preparation. Establish exact browser
  geometry against the complete renderer;
  retained DOM, canvas widths and approximate Rust advances are insufficient.
- [ ] **Incremental paragraph updates:** wrapped prefixes and reconnecting
  source/style-proved suffixes now reuse completed probes in yielding batches,
  with fresh terminal dimensions. Changed incoming prefixes can reconnect cached
  tails at whole words using their already measured geometry and extents, including
  hanging glyph ranges. Native range reads are now batched, with independent
  per-range browser oracles; this does not resolve probe reflow costs. Whole-probe
  and exact differential DOM-retention experiments do not remove that cost. Finish
  nonreconnecting wrapped updates: the
  near-limit beginning-edit trace matches source/run boundaries but rejects changed
  incoming wrapping phases through first-row overflow and dense overlap; earlier
  measurements required 70 fresh probes and 1.1–1.2 seconds. Current-source
  repeated probes and preserved Unicode span phase now reduce complete geometry
  to 373–389 ms in four isolated both-mode samples, while viewport paint remains
  236–256 ms. Reduce the remaining visible-input and startup costs. New
  exact overlap samples have no shared visual-row word starts, and nonadjacent
  probe markup/layout identities also remain unique. Removing absolute source
  metadata from detached diagnostic signatures still produces no identical probes
  in either mode ([receipt](normalized-probe-identity.json));
  those earlier seams did not support a whole-probe identity cache. Current-source
  reuse retains matching span topology and the full incoming-overlap proof.
  Publish source-owned current
  viewport coverage during long-row preparation without claiming complete extents
  or fabricating a caret at a partial endpoint; retain nonorigin and failed-proof
  fallbacks. The shared core now exposes conservative measured coverage with
  endpoint rejection. The shared facade now retains coverage under the same
  source/style ownership policy as completed prefixes, with shared anchors and
  composition fallback. Eligible origin viewports now publish proved partial paint before complete
  extents, with independent full-renderer checks, failed-proof fallback and
  partial-to-complete cache invalidation in both modes. Covered positive scroll
  offsets also paint without advertising complete document heights. Four near-limit
  beginning-edit production repeats show styled viewport paint in 253–260 ms,
  while full geometry still takes 1.18–1.24 seconds; cold viewport paint takes
  1.57–1.62 seconds. Broader boundary latency and memory measurements, arbitrary
  nonorigin coverage and the remaining full measurement costs are still unfinished.
  Current release measurements after borrowed request publication still show
  246–261 ms wrapped input paint and 1.18–1.27 s complete geometry in both modes
  ([eight production samples](borrowed-request-production.jsonl)).
  A separate instrumented input run attributes 856 ms to bounds reads across
  71 probes, compared with 1.4 ms of paragraph rendering
  ([profile summary](borrowed-request-layout-profile.json));
  reduce fresh browser shaping/reflow rather than treating source-copy removal
  as completion of this gate.
  Preparation now yields a frame immediately after first publishing proved
  viewport coverage, before continuing background probes. Five focused release
  browser contracts pass in both modes
  ([receipt](coverage-frame-browser.json)). Four current
  production samples show 231–247 ms viewport paint and 1.11–1.14 s complete
  geometry ([samples](coverage-frame-production.jsonl));
  these remain above the responsiveness goal. Its full release browser run passed
  133 tests, failed the initial tabbed horizontal paint bound, and left 57
  unexecuted ([failure](coverage-frame-full-failure.json)).
  The same frozen test passes alone; resolve the timing/order dependency and
  verify the complete inventory and hosted CI before crediting this gate.
  The diagnostic full rerun passed 188 tests but failed the wrapped cursor
  range-count bound, leaving both font matrices unexecuted
  ([record](horizontal-diagnostic-full-failure.json)).
  Distinguish concurrent cold preparation from cursor work and resolve that
  failure without weakening measurement bounds or deadlines.
  Hosted CI for the frame-yield checkpoint also fails the cursor bound in Remote
  mode with 2,504 reads ([receipt](grouped-ci-f86dd7b-failure.json));
  its other four jobs pass. Diagnostics identify 4,110 background height reads
  and 553 other reads in the failing Local sample
  ([record](reply-routing-diagnostic-failure.json)). The
  cursor-cost fixture now waits for complete geometry under its existing deadline,
  retains the 2,000 total-read limit and additionally requires zero background
  reads during the key. The rebuilt full inventory passes all 191 editor tests
  plus the worker routing regression (192 unique tests), including both font
  matrices ([receipt](reply-routing-full-browser.json)).
  Checkpoint `93ec875` also passes all five hosted jobs
  ([receipt](grouped-ci-93ec875.json)). Cold layout costs
  and the historical horizontal-paint failure cause remain open.
  The diagnostic-only follow-on passed all five hosted jobs
  ([receipt](grouped-ci-f5ad590.json)); it predates the cursor
  setup change and does not establish that failure's resolution. Extend
  shifted suffix reuse to changed long-token and plain-run boundaries; avoid repeated prefix
  segmentation for over-limit styled run tables. Eligible styled measurement paths
  now prepare run tables in yielding batches, including uncapped unwrapped
  measurement fallback after the retained cache limit. Paragraph paint-boundary
  validation and exact source-anchor mapping now advance in cooperative batches
  before wrapped and unwrapped browser probes, with complete-only publication.
  Unwrapped preparation now
  builds the complete table once, retaining it only within the existing cache limit;
  it no longer restarts after a capped initial pass. Finish other initial
  run-table construction and unsupported-boundary fallbacks that still segment
  complete rows; avoid repeated over-limit fallback reconstruction. Cooperative
  styled-run preparation now suspends inside large Unicode graphemes using
  bounded forward/context chunks, preserving the original paint boundaries;
  synchronous coordinate, glyph-anchor and complete-layout construction still
  require bounded preparation. Completed visual indexes now retain sparse long-cluster
  boundaries so caret and paint-boundary queries skip their interiors and avoid
  lookahead through an already indexed large grapheme; initial index construction
  now exposes a resumable builder using the same bounded Unicode scanner as paint
  segmentation. Native/row/admission index construction now also has a complete-only
  cooperative document builder. Normal and lossy workspace reads now prepare
  admission and complete indexes cooperatively through the shared editor facade,
  with cancellation and draft protection. Recovery hydration now prepares saved
  and draft indexes before atomic publication, and disk checks classify recovery
  without rebuilding indexes; clean changed-disk reloads prepare cooperatively.
  Explicit recovery-review reloads now also prepare complete indexes before
  publishing, retain shared source allocations and preserve over-limit read-only
  viewing. Immutable editor sources now share cached admission decisions, using
  completed document summaries or cooperative rejection results. Finish remaining
  synchronous document callers, recovery validation/copies, metadata capacity
  growth, external borrowed-version allocation and final publication costs. Shared
  save preparation and acknowledgements now retain the immutable written source,
  preserving late-write ownership without duplicating its bytes. Prepared
  documents now share their immutable source with the saved baseline; standalone
  edits detach the baseline only when necessary to preserve in-place source edits.
  Clean recovery snapshots now capture source directly without building indexes;
  recovery validation counts lines only when saved folds require it.
  Live recovery disk checks now compare committed document sources without
  materializing persistence records, and clean disk reload preparation moves the
  owned read source
  ([both-adapter contracts](live-disk-recovery-browser.jsonl)).
  Admission scans and persistence serialization remain work.
  Recovered undo steps now retain the prepared saved/draft sources instead of
  duplicating both full files; ordinary typing history keeps owned replacement
  spans ([complete editor contracts](shared-recovery-history-browser.jsonl)).
  Recovery capture, record clones and hydration preparation now share immutable
  saved/draft sources; clean buffers without a document share their existing source.
  Final recovery admission and fold row counts now reuse complete source-owned
  indexes. Canonical JSON decoding now borrows encoded field bytes before
  creating the owned decoded source. Encoding, complete JSON/network buffers,
  final normalization costs still require work. Recovered full-source undo/redo
  now publishes retained complete source/index versions instead of copying source
  and rebuilding coordinates. The saved index remains retained until that history
  step is released; common-prefix/suffix comparison, fold rebasing and frontend
  publication/normalization still require bounded preparation and memory evidence.
  Recovery metadata validation now checks borrowed selection positions and ordered
  fold ranges directly, without cloning and sorting them; fold validation retains
  only an ancestor-end stack bounded by nesting depth. Restoration still owns and
  normalizes its final selections and fold state.
- [ ] **Incremental syntax and structure:** finish larger retained-container reuse,
  warm semantic list assembly, paint-table iteration, shifted suffix metadata,
  parser context/selection-list extraction and final paint publication. Finish
  larger fenced-code workloads and changed-source plain row-table reconstruction
  and validation. Bound cache-missing fallback scans, whole-row capacity growth,
  scratch allocation and remaining final publication work. Synchronous document
  indexes still shift/splice in place; immutable retained tables must remain exact.
  Edits now retain preceding rows with proved unchanged complete newline endings
  and following rows when the replacement preserves their line boundary or moves
  them to the document start;
  changed long rows still rebuild their coordinate and visual indexes.
  Row admission summaries, UTF-16 totals and native checkpoints now share the
  visual character pass; remaining synchronous construction still needs bounded
  preparation rather than relying on fewer full-row passes.
- [ ] **Source ownership and storage:** finish external parser snapshots,
  remaining changed-revision paint/transport comparisons, metadata materialization
  and transport serialization, message decoding, diff shaping and native-text
  materialization.
  Editor text equality and motion/decoration/guide ownership checks now accept
  borrowed references to the same immutable source without scanning bytes;
  external values retain complete equality validation. All 129 native frontend
  tests and strict native/WASM lint pass; all three release both-mode motion,
  decoration and guide ownership contracts pass
  ([receipt](source-equality-browser.json)). Complete hosted
  CI and measured production latency remain open.
  The browser worker client now routes replies through a shared core header
  reader capped at 256 bytes, avoiding a full payload parse solely for the ticket.
  Shared producers serialize routing metadata first; complete facade validation
  remains required. All 637 native core tests and strict native/WASM lint pass;
  the worker routing regression and full 191-test editor inventory pass
  ([receipt](reply-routing-full-browser.json)). Remaining
  complete decoding/publication costs are still open. Browser request source comparison, copying and JSON escaping
  now advance in bounded Rust batches through the shared facade. All 191 release
  editor browser contracts, including both font matrices, pass
  ([receipt](request-publication-full-browser.json)); measure
  actual publication/input latency and memory before crediting responsiveness.
  That request-publication checkpoint also passed all five hosted jobs
  ([receipt](grouped-ci-f208bde.json)); newer checkpoints and
  the remaining production/device gates still require verification.
  The borrowed-source follow-on also passed all five hosted jobs
  ([receipt](grouped-ci-6ef5108.json)); this does not resolve
  the later intermittent horizontal-paint failure.
  Request encoding now borrows retained source ranges without an intermediate
  plaintext copy, sharing full/delta selection with replies. Its 636 core tests,
  strict native/WASM lint and all 191 release browser contracts pass
  ([receipt](borrowed-request-full-browser.json)); measure
  actual memory effects and await hosted candidate CI.
  Header and capacity allocation, JavaScript string conversion, message decoding and
  oversized-message fallback still need bounded preparation. Combined row scans retain the geometry contracts, but
  current production repeats still show 125–178 ms cold source-publication tasks
  and 64–69 ms input tasks; these
  source/controller costs remain responsiveness targets. Current `9dce60c` production
  verifies full source in all 24 both-mode boundary samples
  ([record](source-equality-production.jsonl)). Wrapped
  row-count input still takes 241–247 ms versus 17–19 ms unwrapped; its
  instrumented sample is mostly idle before one cheap measurement batch
  ([profile](wrapped-row-count-profile.json)). Resolve the
  neutral readiness path while preserving complete indexed source ownership.
  Bound remaining
  initial capacity allocation, folded/bounded projection assembly, retained
  row/coordinate copies, changed indentation-guide copies and storage suffix
  byte/coordinate shifts. Long boundary rows still scan for admission. Revisit
  measured storage candidates with the actual viewport/worker access pattern,
  preserving exact coordinates, immutable retained views and cancellation.

**Remaining completion gates**:

- [ ] **Responsiveness and memory:** remove the remaining input, cold-paint, wrapped
  layout and process-memory stalls. Repeat admitted byte, row-count and long-line
  boundary workloads in both modes, including formerly unresponsive wrapped cases
  and Linux Chrome PSS. Require actual near-1-MiB String styling through load,
  scrolling and input, plus repeated startup-scroll, initial-shaping and beginning
  edit samples. Existing multi-second results do not satisfy this gate.
  Current production repeats cover both modes with wrapping on/off: near-byte-limit
  input takes 169–210 ms and rejected Rust long-line input 109–154 ms, with roughly
  741–858 MiB peak Chrome PSS ([record](rejected-source-production.jsonl)).
  Long-line input improves over the preceding 674–713 ms cohort, but cold startup
  still takes 1.45–3.95 seconds; these scrolled native-window samples do not verify
  complete source after input or clear the responsiveness/memory gate.
  Plain documents and same-owner parser-rejected documents now paint edits during
  pending worker analysis; grammar retries and recovery remain active. Unfinished
  wrapped geometry restarts when syntax preparation advances. Reduce remaining
  source, layout and memory costs while preserving grammar paint coalescing,
  localized row reuse, cancellation and bounded terminal fallback.
  The source-sharing checkpoint additionally verifies complete source after beginning
  edits in 16 fresh both-mode release samples, including actual near-1-MiB String
  styling ([record](shared-saved-source-production.jsonl)). Styled
  wrapped input paints in 251–262 ms but complete geometry still takes 1.21–1.25
  seconds; cold startup reaches 3.98 seconds. Reduce those remaining costs.
  Neutral rendering borrows immutable projection rows with a stable cache identity;
  fallback completion remains a separate readiness condition. Verify styled-boundary
  and beginning-edit production workloads before crediting broader improvements.
  Remaining wrapped geometry yields to visible
  paint and input only after current source-owned viewport coverage is accepted.
  Both-mode held-task/cancellation and scheduler-fallback verification pass
  ([receipt](background-geometry-owned-browser.json)); final
  production samples verify full source and 134–159 ms viewport paint, with
  387–416 ms complete geometry
  ([results](background-priority-production.jsonl)). Final-source
  geometry, overlap fallback, superseding-source cancellation and held-task
  contracts pass in both modes
  ([receipt](background-priority-final-browser.json)); strict
  WASM lint and formatting pass. Geometry proof and completion requirements
  remain unchanged.
- [ ] **Exact geometry and fallbacks:** retain complete-renderer extent/anchor/hit
  comparisons, font/feature/whitespace matrices, Unicode/caret mapping and failed-proof
  fallback contracts. Current painted coverage and exact retained caret anchors,
  including wrapped endpoints and adjacent same-row boundaries, avoid complete
  movement probes. Unpainted sparse gaps, ambiguous soft-wrap boundaries and
  unsupported layouts still require complete-renderer fallback.
  Improvements must preserve source/account/project ownership,
  pending edits, themes, supported previews, agent context and both adapter contracts.
- [ ] **Reliable CI and release/PWA checks:** pass the complete native/WASI/WASM,
  platform and release checks on the combined editor/plugin tree. The combined
  debug browser run fails wrapped exact-row-cache readiness and cannot establish
  completion ([record](editor-plugin-debug-failure.json)).
  The combined optimized artifact passes all 194 editor/worker browser contracts
  with unchanged assertions and deadlines
  ([receipt](editor-plugin-release-browser.json)). Ordinary
  component fixtures lacked shipped CSS font assets; the explicit font matrices
  load them separately. Repeat ordinary contracts with required production fonts.
  An expanded near-limit replay candidate fails its positive-reuse assertion with
  Monaspace on a second beginning insertion; complete geometry finishes
  ([failure](monaspace-wrapped-replay-regression.json)). Current
  hosted CI for `7968226d`, including the paired-plugin error assertion
  correction, passes all five jobs
  ([receipt](grouped-ci-7968226.json)). The newer font-harness
  checkpoint `13fe1da0` also passes all five jobs
  ([receipt](grouped-ci-13fe1da.json)). The first
  current-source replay candidate failed the second beginning insertion
  ([failure](current-source-probe-reuse-failure.json)).
  Selecting the next complete word after a Unicode span edge resolves the focused
  release regression in both modes and widths
  ([receipt](current-source-probe-reuse-browser.json));
  658 native core tests and strict native/WASM lint pass. All 170 component
  contracts pass with actual font files served, using
  disjoint runs within unchanged deadlines. Production complete geometry improves
  to 373–389 ms, while viewport paint remains 236–256 ms
  ([samples](current-source-wrapped-production.jsonl)).
  All 194 editor/worker browser contracts, 658 native core tests, 129 native
  frontend tests, final ownership regression and strict WASM lint pass. Merged
  main `1b271c81` passes all five hosted jobs
  ([receipt](grouped-ci-1b271c8.json)). Hosted CI for the newer background-priority checkpoint remains pending;
  visible-input/cold-start performance gates remain open.
  The embedded-comment
  checkpoint `cc019c1` passed all five hosted jobs
  ([receipt](grouped-ci-cc019c1.json)). Selection-capability
  checkpoint `71997bc` passed all 184 editor browser tests locally and all five
  hosted jobs ([receipt](grouped-ci-71997bc.json)). New structural navigation passes all 187 editor browser contracts and 128
  native frontend tests ([receipt](structural-navigation-full-browser.json));
  that checkpoint (`8cb111c`) passed all five hosted jobs
  ([receipt](grouped-ci-8cb111c.json)). The subsequent embedded-input checkpoint
  passes the complete 188-test editor inventory
  ([receipt](held-typing-full-browser.json)) and passed all five
  hosted jobs ([receipt](grouped-ci-ed1c8ca.json)). The subsequent
  preparation checkpoints also passed all five jobs
  ([7ff2462](grouped-ci-7ff2462.json),
  [211574d](grouped-ci-211574d.json),
  [81d2d78](grouped-ci-81d2d78.json)). These successful
  checkpoints do not close the outstanding production and reliability gates.

  Investigate intermittent hosted readiness failures before declaring reliability.
  `9d4f888` published the typed parser-budget fallback but did not finish paint
  within the existing deadline, followed by five other failures
  ([receipt](grouped-ci-9d4f888.json)). Earlier wrapped-startup
  and parser-fallback failures at `3afca61` and `72b7a3e` also have unproven causes
  and possible fixture contamination. Subsequent green checkpoints and isolated
  local passes do not establish those causes. Keep parser/paint/geometry timeout
  diagnostics and repeat near-limit Linux/x64 readiness, complete hosted suites
  and production responsiveness/memory checks with unchanged readiness deadlines
  and paint/probe limits.

  Preserve complete compiled-artifact inventories, independently checked disjoint
  groups of at most 64 editor tests, the 300-second browser deadline, separate
  font matrices and every other UI partition. Repeat merged editor/plugin and
  subsequent implementation checkpoints, including warm highlight bursts and
  wrapped-row reuse, file-switch cancellation, cold queued-input source/selection
  ownership, host-plugin focus/layout and explicit Output/terminal targeting.
  Validate bounded browser-setup retries after upstream Chrome download HTTP 502
  failures. Historical checkpoint counts, receipts and measured results live in
  [performance evidence](../editor-performance.md); they are not the current inventory.
- [ ] **Physical Chrome/Edge PWA input:** verify real input-method commit/cancel,
  Unicode and LF/CRLF undo/redo, multiple-cursor clipboard behavior and touch input
  without rewriting unrelated text. Automated CDP composition is supporting evidence,
  not physical input-method verification.
- [ ] **Folder permissions and recovery:** expand permission/error regressions and
  verify real local directory-handle permission loss/regrant and reload recovery.
  Existing held-write contracts cover failures and stale account/folder/bridge/project
  results, background saves, newer edits and a clean disk reload while a save waits.
  Late acknowledgements reconcile active and retained snapshot dirty state with
  the actual written source
  ([failure/pass evidence](late-save-reload-browser.jsonl)).
  The disposable recovery check has no
  native local folder handle. Coordinate with Offline & error-state recovery below.
- [ ] **Accessibility and integration:** verify keyboard focus/Tab escape, assistive
  technology, touch, theme integration and PWA loading across the completed editor,
  including the cold/fallback paths. File-specific input names, current Tab guidance
  and live Ctrl+M announcements have browser contracts in both modes. Release-app
  checks verify trusted forward/backward Tab escape and accessible input names with
  pending syntax, bounded native input and LF/CRLF sources. Physical assistive-
  technology verification remains. Evaluate whether the current projected native
  input and source-paint surface adequately supports the richer multiple-selection view.

