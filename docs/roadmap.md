# Roadmap

What's left, grouped by how soon it's coming: **Next** (queued up), **Later**
(planned, not yet started). Finished work —
phases 1 through 14, telemetry, hardening, streaming, `/test`, database-backed
theme and prompt history, frontend performance & polish, app branding and the chat
welcome, database-backed agent skills and skill import/export — moved to [CHANGELOG.md](../CHANGELOG.md).
[Host administration](host-administration.md), structured agent questions and
[Monitors](monitors.md) are implemented, with an active-only status-line flyout;
their shipped behavior is in the changelog.
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
| Structure and languages | Syntax highlighting, parser-backed folding, bracket matching and structural navigation; extensible Rust/WASM language support including Rust, TypeScript/TSX, Python, JavaScript/JSX, Java, C#, C++, PHP, Shell, C, Go, HTML, CSS, JSON/JSONC, YAML/YML, TOML, INI/EditorConfig, XML ecosystem configs and Markdown with inline/fenced code. Pending and unsupported source stays plain. |
| Navigation and review | Find/Replace; line numbers; horizontal scrolling and linked split scrolling; Edit/Inline/Split diffs; supported previews and Markdown change gutters/word differences; pending-edit review and agent context. |
| Tabs, appearance and recovery | Tab context actions; consistent editor action-menu icons and labels; five Monaspace families; texture healing and ligature toggles enabled by default; retained caret/selection/scroll and database-backed editor recovery. Real folder-permission recovery still needs device verification. |
| Selection and browser input | Multiple/rectangular selections and clipboard transactions; scoped native windows with bounded desktop startup for unwrapped, wrapped and nonuniform views, including long tabbed/bidirectional rows; automated Chromium composition and pointer checks. The recorded line-end caret bug is fixed and user-verified in the localhost PWA. |
| Preparation and ownership | Rust/WASM worker and cooperative fallback; retained source/token/structure allocations; bounded worker delta reconstruction/reply-source publication and final structure metadata, parsed/lexical region reconciliation and retained fallback collection; bounded eligible unwrapped and wrapped paragraph probes, retained paint-run clipping and cooperative sampled cold-row glyph measurements, exact overlap validation, exact carets from retained anchors/current painted coverage and conservative complete-layout fallback. Current-file progress is shown during preparation. Matching trusted font notifications retain current in-flight work and ordinary-row replay after edits. |

**Remaining implementation**:

- [ ] **Parser-aware command readiness:** extend the cooperative Reindent readiness
  policy to other parser-aware commands where pending analysis can lose their
  intended behavior. Use shared structure/capability information for embedded
  language actions rather than relying solely on the outer file language. Cover
  deferred completion, cancellation and stale source/account/selection in both
  modes. Reindent now passes completion, HTML/Markdown bodies, menu, capacity and
  cancellation contracts; its evidence is in [performance verification](editor-performance.md).
  Embedded comment actions now share that preparation path, with request-time
  ownership and newer-command cancellation; all 183 editor browser contracts and
  128 native frontend tests pass. Selection-specific comment capabilities now
  reuse complete analysis, with mixed-selection and stale-account checks passing
  in both modes ([focused evidence](editor-performance/selection-capabilities-focused-browser.json)).
  All 184 current editor browser tests, including both font matrices, pass
  ([complete evidence](editor-performance/selection-capabilities-full-browser.json)). Finish
  the readiness policy for other structural commands without delaying ordinary input.

- [ ] **Cold startup and native input:** remove remaining full-source shaping in
  touch and unsupported-layout fallbacks. Desktop startup now uses bounded native
  input for wrapped and nonuniform rows, with exactly measured origin paint while
  complete document extents prepare. Finish production startup and memory costs
  across admitted boundary workloads; finish touch pointer selection and
  source-owned caret/selection while preserving composition mappings and complete-native fallback
  when bounded geometry cannot be proved. Basic direct and queued keyboard
  motion now uses editor admission instead of the separate 2 MiB structure
  budget; measured neighborhoods retain source and validate only measured rows.
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
  incoming wrapping phases through first-row overflow and dense overlap; the latest
  repeated measurements still require 70 fresh probes and 1.1–1.2 seconds. New
  exact overlap samples have no shared visual-row word starts, and nonadjacent
  probe markup/layout identities also remain unique. Publish source-owned current
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
  nonorigin coverage and the remaining full measurement costs are still unfinished. Extend
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
  ([both-adapter contracts](editor-performance/live-disk-recovery-browser.jsonl)).
  Admission scans and persistence serialization remain work.
  Recovered undo steps now retain the prepared saved/draft sources instead of
  duplicating both full files; ordinary typing history keeps owned replacement
  spans ([complete editor contracts](editor-performance/shared-recovery-history-browser.jsonl)).
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
  materialization. Browser request source publication and oversized-message fallback
  still run synchronously. Combined row scans retain the geometry contracts, but
  current production repeats still show 125–178 ms cold source-publication tasks
  and 64–69 ms input tasks; these
  source/controller costs remain responsiveness targets. Bound remaining
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
  741–858 MiB peak Chrome PSS ([record](editor-performance/rejected-source-production.jsonl)).
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
  styling ([record](editor-performance/shared-saved-source-production.jsonl)). Styled
  wrapped input paints in 251–262 ms but complete geometry still takes 1.21–1.25
  seconds; cold startup reaches 3.98 seconds. Reduce those remaining costs.
  Neutral rendering borrows immutable projection rows with a stable cache identity;
  fallback completion remains a separate readiness condition. Verify styled-boundary
  and beginning-edit production workloads before crediting broader improvements.
- [ ] **Exact geometry and fallbacks:** retain complete-renderer extent/anchor/hit
  comparisons, font/feature/whitespace matrices, Unicode/caret mapping and failed-proof
  fallback contracts. Current painted coverage and exact retained caret anchors,
  including wrapped endpoints and adjacent same-row boundaries, avoid complete
  movement probes. Unpainted sparse gaps, ambiguous soft-wrap boundaries and
  unsupported layouts still require complete-renderer fallback.
  Improvements must preserve source/account/project ownership,
  pending edits, themes, supported previews, agent context and both adapter contracts.
- [ ] **Reliable CI and release/PWA checks:** pass the complete native/WASI/WASM,
  platform, browser and release-app checks reliably.
  CI run `38045611820` failed wrapped startup readiness, highlight burst generations,
  localized wrapped-row reuse and file-switch cancellation assertions. The identical
  50-test group at `0bd6bbd` passes on macOS and Linux with four and two CPUs
  ([records](editor-performance/current-ci-group-browser.jsonl)). All 173 current
  editor tests, including both font matrices, also pass in the checked Linux
  runner ([record](editor-performance/parser-budget-browser.jsonl));
  `0bd6bbd`, `121fbbb` and `c42bc47` also passed all five CI jobs
  ([first receipt](editor-performance/grouped-ci-0bd6bbd.json),
  [second receipt](editor-performance/grouped-ci-121fbbb.json),
  [third receipt](editor-performance/grouped-ci-c42bc47.json)); repeated complete
  verification and the production responsiveness/memory gates remain required.
  CI run `38051791475` at `3afca61` subsequently failed wrapped startup,
  long wrapped cursor measurement and horizontal fragment assertions
  ([receipt](editor-performance/grouped-ci-3afca61.json)). Its exact 50-test
  selection passes with two CPUs on the current Linux arm64 build
  ([record](editor-performance/ci-3afca61-group-current.jsonl)); this does not
  prove the hosted x64 failure is fixed. Investigate the first startup failure
  and any subsequent probe contamination before declaring CI reliable.
  The subsequent `87df822` checkpoint passed all five CI jobs
  ([receipt](editor-performance/grouped-ci-87df822.json)), before the initial
  native-input lifecycle change. Binding now retries when document preparation
  finishes after mounting; all 173 editor tests pass with two CPUs in Linux arm64
  ([record](editor-performance/native-startup-retry-browser.jsonl)). This does not
  prove the hosted startup failure's cause; preserve unchanged readiness deadlines
  and verify the current checkpoint on hosted CI.
  The input-readiness checkpoint `3017d5a` then passed all five CI jobs
  ([receipt](editor-performance/grouped-ci-3017d5a.json)). Neutral rendering
  identity changes pass all 173 editor browser tests with unchanged paint/probe
  count limits ([record](editor-performance/neutral-render-identity-browser.jsonl)).
  The neutral-identity checkpoint `e2ec3f5` passed all five CI jobs
  ([receipt](editor-performance/grouped-ci-e2ec3f5.json)). Rejected-source early
  paint and wrapped geometry restart pass all 174 editor browser tests on two-CPU
  Linux arm64 with unchanged readiness deadlines and paint/probe limits
  ([record](editor-performance/rejected-source-browser.jsonl)); current hosted
  verification, repeated reliability and the broader gates remain open.
  The rejected-source checkpoint `43f66c9` passed all five hosted CI jobs
  ([receipt](editor-performance/grouped-ci-43f66c9.json)).
  Prepared-source baseline sharing passes all 469 native core tests, 128 native
  frontend tests and 174 editor browser tests with both font matrices
  ([record](editor-performance/shared-saved-source-browser.jsonl)). Its `8a5f1e3`
  checkpoint passed all five hosted CI jobs
  ([receipt](editor-performance/grouped-ci-8a5f1e3.json)).
  Immutable save payload/acknowledgement sharing additionally passes 470 native
  core tests, 128 native frontend tests and 30 selected browser checks, including
  both-adapter save ownership and recovery failures
  ([record](editor-performance/immutable-save-source-browser.jsonl)); its
  `dc49196` checkpoint passed all five hosted CI jobs
  ([receipt](editor-performance/grouped-ci-dc49196.json)).
  The late-save dirty-state checkpoint `0ec8cbe` and direct disk-comparison
  checkpoint `988f700` also passed all five hosted jobs
  ([first receipt](editor-performance/grouped-ci-0ec8cbe.json),
  [second receipt](editor-performance/grouped-ci-988f700.json)). The recovered
  undo-source checkpoint `72b7a3e` subsequently failed the editor browser job
  ([receipt](editor-performance/grouped-ci-72b7a3e.json)): the first failure was
  the typed parser-budget fallback readiness assertion, followed by cursor-probe,
  horizontal-fragment and geometry readiness failures. Their cause and any
  contamination from the first failed test remain unproven. Timeout diagnostics
  now distinguish parser status, pending work and painted state without changing
  deadlines or assertions. The source-sharing checkpoint `ba33f5d` subsequently
  passed all five jobs ([receipt](editor-performance/grouped-ci-ba33f5d.json));
  this does not establish the earlier failure's cause. The final-index validation
  checkpoint `2301c26` also passed all five jobs
  ([receipt](editor-performance/grouped-ci-2301c26.json)). The subsequent navigation,
  decoding and metadata-validation checkpoints also passed all five jobs
  ([navigation](editor-performance/grouped-ci-3d7eb5d.json),
  [decoding](editor-performance/grouped-ci-2b1e482.json),
  [validation](editor-performance/grouped-ci-be926f5.json)). Current Reindent,
  reveal-retry and prepared-index undo checkpoint `9d4f888` failed its hosted
  browser job ([receipt](editor-performance/grouped-ci-9d4f888.json)). The first
  failure published the typed parser-capacity fallback but did not finish paint
  readiness within the existing deadline; five subsequent editor checks also
  failed. The cause and possible fixture contamination remain unproven. The
  subsequent embedded-comment checkpoint still requires complete hosted
  verification; earlier green checkpoints do not prove these failures' causes.
  Validate bounded browser setup retries after the upstream Chrome download HTTP 502 failure. Repeat near-limit Linux
  readiness and the complete suites; a local run or one green checkpoint is insufficient.
  Cold queued-input checks explicitly defer neighborhood paint while preserving
  source and selection comparisons; verify the complete suite on CI as well.
  Host-plugin fixtures and explicit Output targeting preserve the memory/skill and
  terminal contracts; verify focus/layout behavior in the complete browser partition.
  Warm burst checks now wait for published parser/fallback paint and exact token-owned
  dimensions; three Linux repetitions pass the strict burst and both-mode row-reuse
  assertions, and the expanded 149-test Linux editor component partition passes.
  The browser setup correction (`3956122`) passed all five CI jobs; repeat complete
  CI for the merged editor and plugin checkpoints and retain the boundary gates.
  The scoped crop-failure fixture checkpoint (`11150a0`) passed all five jobs,
  including the complete editor and other UI partitions and both font matrices;
  repeat verification for subsequent implementation checkpoints remains required.
  Later checkpoints exhausted the combined 149-test browser deadline without an
  individual assertion failure. CI now inventories current compiled artifacts and
  checks complete, disjoint groups of at most 64 editor tests, preserving the
  300-second browser deadline and separate font matrices. All 167 editor tests
  pass in the official Linux runner. The grouping checkpoint (`2eab2c2`) passed
  all five CI jobs, including every browser group and both font matrices. The
  following-row checkpoint (`54c814e`) repeated that complete success; full CI
  for subsequent implementations remains required.
  Current checkpoints and measured results are in [performance evidence](editor-performance.md).
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
  ([failure/pass evidence](editor-performance/late-save-reload-browser.jsonl)).
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

The compact app/editor rows, universal search, logo drawer, single status footer,
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

### Process execution: MCP client & headless browser

The rest of the Phase 11 bridge work that
isn't built yet — the bridge's PTY sessions, `run_command` tool, and
terminal pane are done and in the changelog, but:

- **Model Context Protocol (MCP) client:** the bridge spawning configured
  MCP servers (`~/.openwebide/mcp.json` / `.openwebide/mcp.json`) as host
  child processes over `stdio`, translating `tools/list` into the agent's
  `ToolDefinition` schema, and forwarding `tools/call`.
- **Deferred tool loading (`tool_search`):** once MCP servers push the tool count up, keep the core
  file/search/shell tools always loaded and, when the remaining schemas would exceed a share of the
  connection's context limit, send only their names plus a `tool_search` tool (keyword or `select:`
  lookup) that loads the matching full definitions for the next turn. Off below the threshold, so
  small tool sets and small-context local models pay nothing extra.
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

- Finish executable plugin integration on the public Rust SDK and versioned
  WASM component contract. The SDK, host runtime and isolated source preparation
  foundation are merged; invocation transport and authenticated general host
  capability dispatch remain unfinished. Verify Linux/container build isolation
  and provision compiler prerequisites before enabling executable installation
  there; Windows source-build isolation remains unsupported. Publisher
  CI builds artifacts; releases record artifact location, checksum, pinned source
  and host API compatibility. Validate and execute components only on bridge hosts.
  Provide an authoring example and reusable build/validation workflow for custom
  marketplaces.
- Replace transitional first-party tool-group feature switches with plugin-owned
  implementations, starting with Web, then Memory, Scheduling and Skill Authoring.
  Require the same public SDK, interfaces and privileges as community plugins:
  no first-party identity dispatch, built-in feature proxy or fallback. Keep host
  primitives general and feature policy in plugin code. Verify local/remote
  execution, cancellation, failures, disablement and updates before declaring
  these behavior migrations complete.
- Extend the existing skills and optional platform tool-group contributions with
  arbitrary host tool handlers and MCP servers. Add versioned tool schemas,
  runtime contracts, capability grants and dependency/configuration validation.
  Build on the MCP client and [database-backed project skills](agent-skills.md);
  keep discovery and context loading bounded through deferred tool loading.
  Add context/run hooks when needed. Core workspace tools remain built in.
- Add host-side update checks while all clients are closed, extending the existing
  Notify, Automatic compatible and Off policies. Persist check state and update
  availability, bound polling/retries, and surface results in the existing Plugins
  indicator and update controls when a client reconnects.
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
- Let plugins contribute top-level panels through a versioned declarative UI
  schema inspired by Block Kit. Declare stable panel IDs, titles, built-in icons
  and host view/action handlers in the package manifest. Host handlers return
  bounded UI documents composed of supported text, layout, status, form, button,
  list/table and code/diff components; the core Rust/Leptos renderer uses existing
  shared components, theme tokens, accessibility and responsive panel layouts.
  Plugins provide data and action IDs, not arbitrary browser HTML/CSS/JavaScript,
  WASM UI code or client-side expressions. Keep UI schema compatibility distinct
  from package manifest compatibility and validate documents before rendering.
- Route panel interactions through the same shared plugin facade and host bridge
  as other contributions. Send validated inputs with the owning user/project,
  panel-instance and view revision; dispatch only declared actions, apply normal
  capability/approval rules, and return or stream replacement view revisions.
  Prevent duplicate side effects and stale replies after navigation, account or
  project switches, disablement and updates. Define loading/error/disconnected
  states, subscription cleanup and bounded update rates; retain core navigation
  when a plugin fails. Namespaced dynamic panels use the shared panel registry;
  user layout/visibility and durable plugin configuration/state stay in the
  database. Verify the same panel/action contracts through local and remote host
  adapters, including mobile clients. Use a GitHub pull-request panel as a
  first-party reference, with inspectable actions and approval before publishing.
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
- Add agent-facing tools to search/browse catalogs, inspect plugins, create and
  validate packages, install, configure, enable/disable, update, remove and roll
  back plugins, and manage marketplace sources. Extend the existing Plugins
  controls to new contribution types, sharing validation, ownership, revision
  checks and mutation-approval policy. Plugin instructions and handlers remain
  subject to granted tool capabilities.
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
this preparation; publication and release-host verification remain outstanding.
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
- **Release rehearsal:** run the Release workflow's nonpublishing manual build and
  verify its native Linux amd64/arm64 images and Linux/macOS bridge archives. Exercise
  download-only Compose, rootless Quadlet (including registry auto-updates), HTTPS,
  database backup/restore/upgrade and both workspace modes. CI validates generated
  Quadlet services; actual systemd service and update-timer checks remain hands-on.
- **Prerelease publication:** the prepared pipeline also accepts `-alpha.N`, `-beta.N`
  and `-rc.N`, publishes versioned images plus the matching rolling channel, and marks
  GitHub Releases as prereleases. Validate a first prerelease install and make GHCR
  public before using alpha/beta distribution for feedback; this does not complete 1.0.
- **First stable publication:** move shipped changelog entries to `[1.0.0]`, bump workspace,
  Spin and lockfile versions together, and publish `v1.0.0` after rehearsal passes.
  The prepared workflow runs CI, merges native platform images into
  `ghcr.io/openwebide/openwebide:v1.0.0` and `latest`, and publishes changelog-derived
  notes, archives, install files and checksums using `GITHUB_TOKEN`. Make the GHCR
  package public and verify anonymous pulls and clean installs on both architectures.
  The workspace currently remains `0.1.0`; a prerelease version can be adopted before
  the stable-release gates pass.
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
