# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
The workspace is at `0.1.0` with no tags and no releases yet, so everything
to date lives under [Unreleased](#unreleased). See [docs/roadmap.md](docs/roadmap.md)
for what's still ahead.

## [Unreleased]

- Make copied Rust source and embedded SDK inputs readable by the isolated Linux compiler identity, preserving private account-cache permissions during source installation. Verify public Git source compilation and plugin-owned execution in the production image, cancellation during preparation, cache reuse, and server installation with inherited project enablement.

- Allow the selected Apple developer toolchain in the Rust plugin compiler sandbox, supporting hosts with a full Xcode installation as well as Command Line Tools.

- Publish the Rust plugin SDK through a pinned public Git checkout, with a capability reference and source-authoring instructions. Marketplace CI validates catalog content and tests, lints and compiles executable references against that exact SDK; registry publication remains planned.

- Share plugin preparation policy across browser, Spin and native bridge clients. Use short start/status requests, common deadlines and cancellation cleanup for installation, cold action caches and bundled initialization. Reclaim completed results under queue pressure so repeated tool calls do not exhaust preparation capacity.

- Show host preparation progress and a Cancel installation action in Plugins. Installation and frontend cold-cache actions share asynchronous preparation through server and paired transports; cancel pending requests and reject stale account/project results before recording receipts. Bound requests and preparation time, and preserve installed versions when preparation fails. Deployed lifecycle verification remains in progress.

- Add authenticated asynchronous host preparation with start, status and cancel endpoints. Bound queued work and retained results, enforce owner isolation and a preparation deadline, and stop compiler work on cancellation or runtime shutdown.

- Add cancellation to the isolated Rust compiler primitive. Stop dependency retrieval and compilation with their child processes when cancelled, including before process startup.

- Clean up terminal Scheduling runs and event jobs through plugin-owned SDK handlers. Save run history before deletion, retain pending results and active work, and rotate bounded cleanup cursors past blocked records. History retention frees quota before raw cleanup retries; deployment remains in progress.

- Bound Scheduling's completed task history in plugin source. Scan one page and remove at most three old entries per reconciliation tick; keep 128 recent entries plus current task results, active history and legacy records. Persist scan progress and recover interrupted cleanup through scoped SDK revisions. Deployment remains in progress.

- Record Scheduling preflight failures in task history when a conversation or model is unavailable. The Rust plugin advances recurring tasks, stops failed monitors and journals interrupted history writes for bounded reconciliation. Preserve prompts already accepted before configuration changes; deployment remains in progress.

- Recover failed Scheduling completion deliveries from retained terminal callback snapshots, including after raw-run deletion. The Rust plugin saves task state and history before removing the callback; live deliveries remain with their worker. Deployment remains in progress.

- Select real Rust Web, Memory, Scheduling and Skill Authoring implementations for new bundled defaults. Distribution builds compile them through the normal compiler sandbox, verify public exports and embed source/SDK/toolchain-pinned WASM for offline server and paired-host installation. Preserve existing version choices, removals and project opt-outs; existing transitional installations require a reviewed update. Production image/browser rollout remains in progress.

- Route Tasks create, edit, pause/resume and delete, plus monitor mutations, through declared SDK tools in the shared host facade. Preserve projectless scope, titles on enablement and conversation-scoped monitor cancellation. Monitor flyout refreshes only read owned data; local unattended work uses the shared host folder binding. Deployed paired-host verification remains in progress.

- Deliver declared periodic plugin background events through the shared leased queue on both host modes, using current enabled source bindings and sessionless grants. Preserve deadlines across restarts, revoke replaced background actors and reserve recovery capacity even when the user job queue is full. The unlisted Scheduling source repairs interrupted timers, run bookkeeping, deletion and source-version handoffs; retention and deployment remain in progress.

- Project SDK raw-run progress into existing task history through scoped immutable bindings. Preserve legacy history IDs and final snapshots after raw-run cleanup; Scheduling source records bindings through the public SDK. Lifecycle recovery and deployment remain in progress.

- Expose current account model preferences and shared public server metadata through read-only SDK collections, without choosing models or exporting endpoints. The unlisted Scheduling Rust source now owns CRUD, due delivery, recurrence, cancellation and monitor-result handlers; lifecycle recovery and deployment remain in progress.

- Expose existing task and monitor data through scoped SDK collection CRUD with revisions and plugin ownership. Preserve task IDs/history and keep plugin-owned tasks out of legacy dispatch. Add atomic record prerequisites to raw prompt submissions so stale handlers cannot queue new work after an edit or deletion; Scheduling handlers and deployment remain in progress.

- Let executable plugins choose origin-bound or project-scoped durable events, preserving model pins while allowing saved work to outlive its creating chat. Add bounded, read-only conversation metadata through the shared collections SDK; target selection remains in plugin source. Executable Scheduling and deployed defaults remain in progress.

- Deliver declared plugin completion events atomically with terminal raw-run status, including cancellation, preflight failures and interrupted-host recovery. Reserve event capacity on submission, pin the submitting source version, preserve released callbacks during history cleanup and reuse shared leased event delivery. Executable Scheduling and deployed defaults remain in progress.

- Dispatch raw plugin prompts automatically through the shared host runner on server and paired hosts. Renew leases during preparation and execution, release busy claims, retry status acknowledgements and cancel deliveries on lease loss or shutdown. Executable Scheduling remains in progress.

- Add host-bound raw plugin run claims, renewable leases, status acknowledgements and recovery that never replays consumed prompts. Cancel dropped agent preparations and release their reservations; executable Scheduling remains in progress.

- Add capability-gated durable raw plugin prompt submissions with owned conversations, idempotency keys, bounded history, model pins and atomic queue delivery guards. Keep plugin prompts out of browser queue draining; Scheduling remains in progress.

- Deliver durable plugin events automatically through a shared worker with bounded concurrency, lease renewal during preparation/execution, sessionless grants and actor cancellation on lease loss. Executable Scheduling remains in progress.

- Prevent plugin HTTP from bootstrapping bridge credentials or bypassing SDK grants through Open WebIDE control APIs, preserving ordinary public and LAN HTTP access.

- Add capability-gated durable plugin jobs with immutable source snapshots, idempotency keys, bounded pages, renewable leases and terminal-job cleanup. Scope callbacks to the current delivery and stop new claims for disabled plugins. The Scheduling migration remains in progress.

- Prepare pinned Rust plugins on the selected execution host before agent tools, context hooks and event callbacks. Validate source, manifest and digest before execution, supporting cold caches and offline reuse through the same shared workflow.

- Route Memory UI mutations through declared executable-plugin actions, sharing grants, validation, cancellation and stale-result guards with agent execution. Preserve explicit manual editing while automatic Memory context is disabled. Deployment remains in progress.

- Select paired plugin hosts independently of local workspace folder mapping, allowing plugin installation and execution when only the browser can access the project folder. Command and Git operations retain their existing mapping checks.

- Add authenticated plugin execution contexts without chat sessions, sharing project records and collection callbacks with chat runs. Pin selected primary models in local and remote run grants; durable job delivery remains in progress.

- Add declared Rust SDK event handlers through the shared host invocation workflow, with manifest/export matching, bounded payloads and normal capability grants. Durable scheduling and background delivery remain in progress.

- Add capability-gated text completion through configured primary/fast models, with shared input/context/output limits, a 30-second deadline and no tools. Memory Rust source owns its automatic-title prompt, profile fallback and content-derived fallback; UI naming integration and deployment remain in progress.

- Preserve full existing Skills resources during plugin edits, including JSON-escaped data beyond the private-record limit. Bound shared collection pages by serialized bytes and retain the smaller private-record quota.

- Prepare legacy skill tools and catalog context only for enabled API 2 authoring contributions, allowing SDK plugins to own all six skill handlers and context without tool collisions. The unlisted first-party Rust source now implements this behavior; deployment and default migration remain in progress.

- Expose existing Skills UI records through capability-gated collection CRUD, preserving resources, metadata, revisions and plugin provenance. Protect managed and disabled skills and keep authoring policy in plugin code; the executable Skill Authoring migration remains in progress.

- Run read-only Rust SDK context hooks through shared planning before model tool selection in local and remote sessions. Enforce prompt budgets, grant scope and disabling only the plugin's own tools, including when model tools are disabled.

- Route backend SSE tools and child tasks through the shared SDK executor and approval gate, preventing failed SDK tools from falling back to built-in behavior. Expire abandoned host invocations without waiting for another request.

- Add capability-gated CRUD for existing Memory UI records and read-only Rust SDK context hooks on the host protocol. Keep search and context policy in Memory plugin source; default migration and full lifecycle verification remain in progress.

- Compile plugin source in normal Linux containers using an unprivileged compiler launcher with filesystem and syscall restrictions. Isolate dependency retrieval, retain macOS/native Linux build adapters, and package the pinned compiler and launcher. Production verification and first-party migration rollout remain in progress.

- Authorize executable plugin callbacks with account/session/project grants pinned to the selected source and capabilities. Verify real compiled WASM record writes and reads on server and paired hosts; share plugin planning before model tool selection. Browser HTTP lifecycle verification and first-party migrations remain in progress.

- Keep capability-expanding plugin updates pending for explicit review, including automatic compatible updates. Preserve the active version until the exact prepared update is approved.

- Add transactional plugin-owned record storage with account/project/plugin namespaces, revision checks, bounded pages and quotas, plus an isolated Rust source authoring checker. Shared collection adapters and first-party migrations remain in progress.

- Add Rust plugin SDK and isolated source-to-WASM preparation foundations, with a shared executable-tool workflow for server and paired hosts. First-party deployment and complete lifecycle verification remain unfinished.

- Retry transient Chrome/ChromeDriver setup failures up to three times in both frontend CI jobs, requiring executable browser and driver paths before testing.

- Retain partial wrapped-paragraph coverage through the shared editor facade with current source, ticket, syntax, file, project, account, font and layout ownership. Reuse immutable anchors and hide coverage during composition; browser publication remains unfinished.

- Add a shared Rust primitive for measured wrapped-paragraph coverage, rejecting uncovered rows and partial endpoint carets. Browser publication remains roadmap work.

- Record exact changed-phase wrapped-editor proof samples in both modes; archive the diagnostic patch separately from production code and retain the outstanding responsiveness work on the roadmap.

- Warm editor burst measurements with the published parser result or terminal fallback through the shared syntax transport, keeping strict incremental repaint and row-reuse assertions.

- Preserve retained editor row measurements across matching font notifications after an edit, while rejecting stale file, project, account, read and font scopes in both workspace modes.

- Enumerate cold editor geometry text nodes cooperatively, with lazy sibling traversal, cancellation and source revalidation before measurement. Preserve exact native node ordering and UTF-16 offsets in both workspace modes.

- Require every requested browser component-test filter to match a real WASM test, preventing partial contract selections from reporting success.

- Batch exact native paragraph range reads through the browser adapter, keeping geometry policy in Rust and independent full-renderer checks. Make the measured-prefix regression fixture portable across Linux font configurations.

- Reuse wrapped paragraph tails after a freshly measured incoming prefix reconnects at a complete word, preserving exact overlap and dimensions, source/style ownership, cancellation and full-layout fallback in both workspace modes.

- Yield while preparing uncapped styled run tables for unwrapped paragraph measurements, preserving the complete fallback after the retained cache limit is reached and rejecting canceled editor scopes.

- Prepare eligible styled paragraph run boundaries cooperatively before wrapped and unwrapped measurements, keeping the exact Unicode segmentation and run cap and rejecting canceled editor scopes before publication.

- Keep browser memory and skill persistence checks aligned with host-installed plugin tool availability, preserving tool-disable and context assertions. Give Output a distinct accessible label and keep syntax progress immediately before it, with Plugins before the reserved progress slot.

- Include plugin documentation in the project site navigation and repair roadmap links to the plugin section.

- Yield between bounded batches of retained paragraph-prefix probes in wrapped and unwrapped editor preparation, rechecking source ownership between batches and measuring terminal dimensions freshly.

- Make cold-editor queued-arrow and native-input browser checks deterministic by holding paint frames while testing pending layout, including composition and clipboard operations in local and remote projects.

- Bundle pinned official Web, Project Memory, Scheduling and Skill Authoring plugins on execution hosts and install them once per account, including offline setup. Preserve uninstalls, selected versions, update preferences and project opt-outs. PR Review remains optional.

- Show Update all with the available update count beside the Installed plugin heading, before the fixed count pill, including when the section is collapsed.

- Show active conversation monitors in the status line with a compact flyout for status, host authorization and cancellation; hide the indicator when no monitor is active.

- Enable installed plugins across existing and new projects by default, with persisted project opt-outs that survive updates. Store verified contributions atomically and preserve personal content on conflicts.
- Migrate optional web, memory, scheduling and skill-authoring tools to host-installed first-party plugins using one contribution policy in local and remote runs; retain core workspace tools.
- Notify about plugin updates by default with a status-bar count, per-plugin updates and Update All. Add Automatic/Off preferences, compatible update checks while the app is open, pinned active-run content and distinct menu action icons.

- Hide installed plugins from Available, restoring them after uninstall; choose installed plugin releases from their gear menu.

- Show plugins in a single Available list with individual marketplace source labels; use Open WebIDE branding and consistent plugin terminology.

- Install plugins on the server host without opening a project; project-specific opt-outs remain available. Use the standard overflow icon and download action, with plugin details on the title and release/management actions in the gear menu.
- Browse plugins in compact, searchable Installed and Available lists with counts, publisher/version metadata, inline actions and per-package gear menus. Keep release details and manual pinned installation behind secondary controls.
- Open plugin search and package lifecycle controls beside Output in the status bar, with a direct link to marketplace-only settings. Use the shared magnifier search row, hamburger action menu, Lucide add icons and modal spacing.
- Keep the official plugin marketplace permanently available, restore it in older account settings, and allow removal only for custom sources.
- Browse the official and custom public Git marketplaces from the status-bar Plugins interface,
  with searchable cached releases and repository inheritance. Install pinned
  skills packages on local or remote execution hosts, enable/disable them per
  project, apply version changes or rollbacks explicitly, and uninstall their
  managed skills across projects. Database revisions guard concurrent changes;
  failed refreshes retain cached catalogs, failed preparation preserves installed
  versions, and running tasks retain their original package instructions/resources.
  Package skills include provenance and use existing agent skill tools.
- Reuse completed wrapped paragraph measurements for unchanged prefixes and
  reconnecting source/style-owned suffixes in both workspace modes. Keep final
  dimensions freshly measured and preserve fresh layout when changed wrapping,
  missing anchors or overflow cannot be proved safe to reuse.

- Reveal omitted wrapped caret endpoints from exact retained glyph geometry in
  both workspace modes. Align source paint with the current scroll position
  before caret measurements, preserving consecutive Home/End jumps. Sparse gaps
  and ambiguous soft-wrap boundaries retain complete-layout fallback.

- Start wrapped and nonuniform editor files with bounded native input in both
  workspace modes. Paint exactly measured origin rows while complete layout
  prepares, keep partial geometry out of document extents, and preserve source,
  selection, composition and stale-result ownership. Touch input and unsupported
  layout still retain their complete-native or complete-layout fallback.

- Start uniform unwrapped files with bounded native input even when long rows
  contain tabs or bidirectional text. Preserve source selection and pending scroll
  ownership while exact layout prepares; restore complete native input on a current
  measurement failure. Both workspace modes use the shared editor input policy.

- Prepare eligible long wrapped paragraphs in bounded styled probes, clipping
  retained text runs and validating exact overlap glyphs before publishing complete
  geometry in both workspace modes. Cancel stale chunks without publishing partial
  tables; retain complete layout for unsupported seams and bidirectional text.
  Unsupported layout and full responsiveness remain in progress.

- Yield between bounded batches of exact cold-row glyph measurements, including
  wrapped and tabbed source. Discard incomplete geometry after source, account,
  project, font or layout changes in both workspace modes. Full-row shaping and
  unsupported-layout fallback remain unchanged.

- Give editor action-menu entries consistent icons and aligned labels using the
  shared menu and icon components in both workspace modes.

- Compare and copy syntax-worker reply sources across bounded UTF-8 batches,
  preserving exact full/delta publication and discarding cancelled partial replies
  before they become a reusable analysis base. Native and browser adapters use
  the same Rust full/delta publication policy.

- Collect retained lexical fallback regions across bounded batches before parsed
  context reconciliation, sharing metadata until individual records are copied
  and shifted into document coordinates in both workspace modes.

- Reconcile parsed editing regions and lexical fallbacks across bounded batches,
  preserving interpolation holes and stable region ordering while hiding
  incomplete contexts in both workspace modes.

- Avoid repeated global overflow layout during bounded paragraph preparation;
  preserve exact glyph geometry and freshly measure the final document extent
  in both workspace modes.

- Validate, stably order and deduplicate final syntax structure metadata across
  bounded worker batches, retaining exact source ownership and hiding incomplete
  contexts in both workspace modes.

- Settle Git sync and file-open requests safely after scope changes or closed dialogs,
  preserve editor reads across chat-session changes, and retain pending history
  selections while loading more commits. Keep timeline popovers inside their panel
  in both workspace modes.
- Isolate container inventory test runtimes from host-installed Docker and Podman
  so CI exercises both fixtures reliably.
- Give the HTTPS transport fixture room for registered tool schemas so it reaches
  its streaming checks without triggering unrelated context compaction.

- Prepare final bracket links across bounded worker batches, preserving embedded
  language boundaries and hiding partial editing contexts until completion in
  both workspace modes.
- Prevent nested syntax-worker yield checks from starving short preparation
  slices before parsing or embedded-body fallback can make progress.

- Keep combined skill, question and scheduling tool discovery within an 8K context budget in both execution modes, with full skill content read on demand.
- Avoid repeating tool definitions in startup context and keep workspace parameter
  descriptions compact, leaving room for replies and compaction at 8K ceilings.
- Reconstruct warm syntax-worker sources in bounded UTF-8 batches, retaining the
  validated base until completion and discarding cancelled partial sources in
  both workspace modes.

- Yield while publishing parser line indexes after large-file edits, preserving
  the previous coordinates until the complete replacement is ready. Cancelled
  or superseded work cannot publish partial indexes in either workspace mode.
- Keep browser discovery and compaction fixtures sized for expanded tool schemas,
  preserving over-capacity compaction coverage and bridge timeout checks.

- Carry cursor-provided parent context through syntax color classifiers and
  retained part checks, avoiding repeated tree searches on wide documents while
  preserving parent-dependent colors, folds and editing contexts.

- Resume syntax row-index replacement scanning across bounded byte/row batches,
  retain the validated source change until parsing starts, and discard canceled
  or superseded row work without exposing partial indexes or stale structure.

- Run session goals as durable host workers in remote, paired-local, and projectless chats, including after the browser closes. Evaluate turn evidence, continue unmet goals, stop on completion, and pause blockers, errors or stalled work. Preserve approvals, foreground priority and revision-safe recovery without replaying consumed prompts.
- Add the `monitor` agent tool for ephemeral host-owned follow-up checks, including bounded repeats, conversation status/cancellation and local-host authorization. Keep monitors out of saved tasks, survive browser closure, recover undelivered claims without replaying injected checks, and retain failure/expiry results in chat.

- Yield while checking syntax source limits before worker serialization and
  parsing, preserving exact byte/newline limits and rejecting canceled or
  superseded sources without publishing stale analysis.

- Add database-backed project skills to Sessions in both local and remote projects,
  with per-skill and project enable controls, revision-safe agent list/read/CRUD
  tools, on-demand instructions/resources and guided skill creation based on
  Anthropic's skill-creator workflow.
- Import skill Markdown files, folders, ZIP/.skill, tar and tar.gz/tgz archives for
  review before saving; export portable ZIPs preserving frontmatter and supporting
  text/binary resources. Decode in the app without requiring a bridge.
- Add `ask_user_question` for explicit choices and free-text replies in local,
  remote and project-less chat. Persist questions and answers across reconnects
  and devices, reject stale replies, and support cancellation, keyboard submission
  and phone layouts. Interrupted local runs resume from their saved answers.

- Yield during parser-free warm-source entry comparison and pass its validated
  replacement into plain-row reuse, preserving exact-base checks and avoiding
  a second comparison of the same source.
- Resume retained-source comparison before parser-backed syntax updates, using
  the same bounded UTF-8 comparison as plain fallback preparation. Cancel or
  replace unfinished comparisons without publishing stale contexts or folds.
- Partition optimized browser CI into editor and other UI contracts, keeping
  all tests and readiness deadlines while avoiding a single growing suite's
  browser deadline. Keep the keyboard-tooltip fixture in view and prevent its
  focus operation from triggering unrelated scroll dismissal.

- Compare retained plain sources across cooperative preparation batches in the
  syntax worker and browser fallback, preserving exact Unicode boundaries,
  complete publication and cancellation during warm edits.

- Configure SSH host administration in Settings and inspect a live homelab
  environment map from project-less chat. Approve immutable host plans, reply
  privately to interactive prompts, and retain operation results across browser
  disconnects, with reboot verification that never replays changes. Linux/macOS
  support per-command sudo; Windows uses the SSH account's privileges. Included
  by default in standard deployments; SSH credentials and a configured connection
  are required. Project sessions and paired companion bridges cannot use these tools.

- Yield within long plain rows while scanning, validating retained rows and
  copying fallback text, preserving complete publication and cancellation in
  the syntax worker and browser fallback.

- Resume grammar-free plain row preparation between worker tasks, preserving
  complete publication, raw-row reuse and cancellation for SQL documents.

- Remember unavailable styled run tables within their exact source/style scope,
  avoiding repeated capped scans while preserving complete paragraph preparation.

- Share Unicode traversal across ordered styled-paragraph anchor boundaries,
  preserving exact grapheme admission and sparse-checkpoint fallbacks.

- Use production tooltip styling in browser regressions, avoiding layout-induced
  dismissal and cascading menu/composer focus failures.

- Reuse immutable long-row tab metadata for native-input admission and paragraph
  measurement,
  avoiding repeated source scans while preserving complete-layout fallbacks.

- Name the editor input for its file and announce Ctrl+M Tab-navigation changes
  to assistive technology, with current keyboard guidance on the native input.

- Share exact Unicode coordinate traversal across ordered glyph queries when
  validating shifted paragraph replay, preserving every source-byte and browser
  overlap check while avoiding repeated grapheme-prefix scans.

- Organize Settings and keyboard shortcuts into accessible tabs with keyboard navigation, stable dialog sizes and automatic model-default saving with retry after errors. Restore header minimize controls for Files, Editor and Terminal, preserving open files, unsaved edits and running shells.
- Polish Changes and History without refresh layout shifts: retain matching results
  on failures, anchor feedback, group composer actions and reserve header progress.
  Draft staged commits with the exact primary model and visible context/output/
  transport-timeout limits; reject oversized input without truncating the diff.
  Add pull-then-push Sync, clear stage/unstage arrows and shared status-aware file
  and descendant folder icons. Retain history/diff tuples, hide patch metadata,
  align tree/filter rows and persist the internal tree width. Fill remaining width
  with rightmost History; show committer-time file timelines with local ticks and
  timezone. Opening the current file closes file history and focuses the editor;
  failed opens retain the modal. Share these behaviors across local/remote projects.

- Add one top-level resizable History panel with coherent branch/merge tracks,
  debounced search, branch/remotes/tag dropdowns, changed-file trees and read-only
  syntax diffs with original line numbers and per-parent comparisons. Add file
  history with rename tracking, a branch selector and dated timeline in a modal.
  Rename Explorer to File Tree. Add staged/unstaged sections, individual and bulk
  staging, staged-only commits, automatic editable commit summaries and stash
  save/apply/drop. Share local/remote actions, fetch/pull/push and tracking checkout,
  icon controls, action menus, bounded previews and stale-result guards.

- Reuse unchanged styled paragraph suffix measurements across insertions and
  deletions, with exact source/run/glyph and browser-overlap validation. Replay in
  bounded batches and freshly measure the complete extent; shifted tab grids and
  failed proofs retain fresh layout. Preserve styled replay candidates during
  plain preparation and matching font notifications after edits.

- Move each prompt’s saved run context into its action menu and a modal. Make generation speed open statistics with latest token counts and generation time, a session token breakdown and recent-call speed bars; retain estimate markers and close dialogs on chat, project or account changes.

- Present suggested follow-ups as compact outlined prompt buttons in chat history. Mark chat summaries with a compact ↪ indicator labeled Conversation recap. Make chat recaps and completion summaries conversational: lead with the topic or concrete result, omit third-person success reports and routine absence-of-blockers language, and refresh cached summaries when generation instructions change.

- Resolve carets directly from current source-owned painted coverage before
  preparing hidden movement layouts, including positions between retained sparse
  anchors. Reject stale scope/font data and unpainted source gaps.

- Reuse exact retained caret anchors before preparing a complete movement layout.
  Preserve source, account, project, font and layout ownership; sparse gaps and
  unsupported layouts retain the complete-renderer fallback. Verify benchmark
  beginning/end input against the full saved document.

- Skip known opaque text and languages without brackets during the final parsed
  bracket pass, while preserving each embedded body's separate bracket ancestry.
  Validate scope ranges before traversing their boundaries.


- Resume YAML scalar headers, literal bodies and dedentation checks in bounded
  syntax worker batches. Replay invalid headers and dedented rows through the
  shared scanner, preserving complete metadata and source-bound publication.


- Show suggested chat prompts as inline composer hints; Right Arrow accepts a matching hint without sending. Put clickable follow-up prompts and context suggestions in the history: follow-ups send immediately, while context suggestions attach to the draft. Keep attachments in a compact composer indicator with expandable previews, and restore composer focus after chat actions.

- Restore the projectless chat button’s active accent, background and underline in the app bar.

- Keep automatic task naming separate from scripted child edits in the browser
  approval regression fixture so CI checks the intended approval and history flow.

- Resume outer and embedded fallback scanning across syntax worker tasks, including
  long strings, raw strings, comments, regexes and template interpolation. Retain
  lexical state and publish only complete metadata; synchronous callers use the
  same scanner. YAML scalar lookahead and final assembly remain synchronous.


- Let scheduled tasks override their run model using a shared dropdown. Default to the current session model at execution time; preserve the session’s saved model in local, remote and projectless chats.

- Retain relative fallback contexts for unchanged embedded syntax bodies across
  source edits and coordinate shifts; rescan changed/new bodies without retaining
  extra source snapshots or bracket tables.

- Guide custom cron editing with five labeled fields, range hints, an expression preview, and full-expression paste.

- Use shared form controls and Repeating/One time/Cron tabs in scheduled tasks; keep Cancel available and support existing, new-per-run, or latest active session targets. Resolve automatic targets when due using user defaults, and retain generated sessions and run links.
- Rename Fast model to Assistance model. Use shared bounded generation with primary
  fallback and a user-owned database cache for automatic session, task and memory
  names, idle chat recaps, completion summaries, suggested prompts and context,
  editable Git drafts and related-term session search. Refresh automatic session
  names after six more turns and five minutes, preserving manual names. Discard
  stale context and Git results, preserve draft edits, and keep optional work behind
  active chat. Summarize actual scheduled-task outcomes and notification results;
  host-backed local runs use the same durable push delivery as remote runs.


- Match retained embedded syntax trees across worker batches, allowing yields and
  cancellation between bodies while preserving unchanged-tree reuse.

- Resume embedded-language selection across syntax worker batches, preserving
  traversal position, exact source ranges and cumulative node/code-body limits.

- Continue syntax parsing across worker tasks when its 100 ms batch expires,
  retaining outer/embedded parser progress instead of discarding cold Markdown
  analysis. Share request validation, queue limits, reply shaping and cancellation
  with the synchronous Rust service; publish only complete source-bound results.

- Schedule project or projectless prompts with weekday/time controls, custom cron or a one-time timestamp. Host-side dispatch uses durable UTC run claims, normal chat approvals, and verified paired-host execution for local folders. Tasks can be edited, paused, resumed or removed from Sessions, with run results and chat links.
- Match Memories to the Sessions controls with Enabled/Disabled tabs and a `+` button; refresh entries automatically.

- Reuse unchanged embedded syntax trees across paragraph insertion, removal and
  shifted Unicode/CRLF positions. Reparse only affected bodies; retain provider,
  range, cancellation and fresh-output validation.

- Prepare large Markdown prose with independent paragraph trees and shared
  per-language parsers. Keep inline prose outside the 64 embedded-code-body limit,
  retaining source/node/record limits, cancellation and fresh/incremental parity.

- Added project memories shared across sessions in both local and remote projects: editable entries and an enabled-by-default toggle in Sessions, bounded automatic run context, revision-safe agent memory tools, and database-backed opt-out.

- Show icons consistently for action rows in Files, Git, editor and tab menus,
  including their right-click menus. Keep project listings and pickers unchanged.

- Put Settings and System prompts in the user dropdown on desktop and phones;
  retain Servers, project actions, Help and About in the app menu. Keep server
  configuration and personal prompt management in separate screens.
- Scope system prompts to their owner, including chat references and defaults.
  Preserve existing prompt libraries and session selections during migration;
  allow different accounts to use the same prompt names.

- Show a compact syntax-preparation spinner in a reserved slot before Output,
  keeping the existing controls stable. Hide it when grammar colors or terminal
  plain fallback are ready in either workspace mode.
- Preserve TypeScript primitive-type colors without overlapping spans or treating
  type names as string literals. Reserve classic scrollbar space in file tabs.

- Keep source-wrapped spaces highlighted in added and removed Markdown preview
  sections, including list items, in both workspace modes.

- Move session goals into a compact, clickable chat statusline indicator with an on-demand context panel. Show active/paused/completed states and persist elapsed completion time across reloads in both workspace modes. Restore approval-mode colors in the statusline and menu.

- Add optional Web Push for finished remote and projectless runs and pending approvals, including child agents. Settings enables each browser subscription; notifications identify the project/session and open that chat. Persist server VAPID keys, account-owned subscriptions and a bounded retry queue, discard expired/resolved alerts, and suppress notifications for the attended chat. Local projects retain app-open notifications. Include the notification setup guide in the website navigation.

- Match Git changes row density to the file explorer automatically on desktop
  and phone layouts.

- Use grammar-backed syntax colors in the editor and diffs; leave pending and
  unsupported source plain, including extensionless LICENSE files. Add YAML/YML,
  Markdown with inline/fenced-code parsing, JSON/JSONC, TOML, INI/EditorConfig and
  XML ecosystem build configuration to the shared Rust/WASM language registry.

- Start eligible unwrapped files with bounded native input even when their first
  row and initial caret are short. Support files made entirely of short rows,
  retaining complete source extents, selection, scroll and measurement-failure
  fallback in local and remote projects. Preserve horizontal wheel requests made
  before complete widths are measured.

- Use compact file-tree rows automatically outside phone mode and larger touch rows in phone mode; remove the density menu option and ignore its legacy saved setting.
- Keep pointer focus from scrolling file tabs, retain keyboard focus navigation, and skip redundant project-tab list and selection updates.

- Keep temporary editor loading locks from flashing an encoding warning and shifting the file tabs; preserve the warning for files opened read-only.
- Contain hidden file/project tab menu buttons within each tab so offscreen tabs cannot create a large empty horizontal scroll area in the workspace.
- Keep modified file and folder icons yellow and reserve the full disclosure-button touch target in the phone file tree so it does not overlap the folder icon.
- Align file-tree Git added/removed line counts at the right edge, immediately before each row’s menu button.

- Reuse validated worker replacement spans during shared syntax parsing when the
  exact retained source base matches, preserving complete comparison fallback,
  cancellation, source limits and stale-base resynchronization.

- Look up sparse editor paragraph anchors within each measured glyph range,
  avoiding repeated whole-line anchor scans while preserving exact geometry and
  terminal-anchor behavior.

- Share retained editor paragraph rectangles directly during validated layout
  replay and check each prefix run-boundary interval once. Preserve exact geometry,
  retention limits and fallback when later paint boundaries change.

- Show the app logo and a clickable openwebide.com link in About, opening in a new browser tab. Use “project” in the app’s Open local/remote actions.

- Nest sibling filename variants (such as docker-compose.ssh.yml) beneath their base files with expandable groups, keyboard navigation and reveal support in local and remote projects. Show creation actions only for folders and keep Reveal in Files on editor tabs.
- Add Close to project and file tab menus and support middle-click closing with the existing unsaved-file prompts.

- Stop capped styled editor run-table construction as soon as the metadata budget
  is exceeded, avoiding unused long-token suffix scanning and allocation. Preserve
  complete run boundaries, Unicode/CRLF handling and complete-layout fallback.

- Use bounded startup input when restoring a caret in an eligible later long row,
  retaining its source selection, saved scroll and exact full-document geometry.
  Verify actual near-1-MiB Rust String highlighting through load, cached/uncached
  horizontal scrolling and a beginning edit with Monaspace Neon in both modes.

- Keep modal and file-tab browser regressions compatible with strict CI lint,
  preserving the exact tab-scroll assertion.

- Preserve open dropdown surfaces and cached branch choices during background
  option refreshes, while retaining the initial loading gate.
- Show the app logo and a website link in About; open openwebide.com in a new browser tab.

- Install source-owned native input windows before initial layout for eligible
  unwrapped files with large first rows. Keep complete source dimensions behind
  measured paint, preserve saved scroll during preparation, and restore complete
  native input on measurement failure after any active composition finishes.

- Make universal search a persistent header input with a viewport-bounded results panel, preserving keyboard navigation and scoped local/remote search. Move projectless chat beside the account menu.

- Keep tooltips dismissed after pointer activation and editor scroll restoration, preserving keyboard focus tooltips without reopening native title bubbles over clicked tabs.

- Keep editor view controls mounted and reserve consistent toolbar width across file switches, preserving file-tab positions and scroll offset when Preview availability changes. Limit segmented-control transitions to paint properties, and truncate long project names before they overlap neighboring tabs.

- Retain current in-flight editor measurements when a trusted font notification
  reports identical face availability and CSS metrics. Preserve refreshes for
  changed metrics, stale ownership and unknown measurement environments.

- Avoid reopening the selected file tab and retain cached editor text when its background disk check is unchanged, while applying external file changes in both workspace modes.

- Position dropdowns before showing them, retain option rows through metadata updates, and show branch menus only after discovery settles, with loading feedback in the trigger.

- Add shared Expand all/Collapse all tree actions, including cancellation when collapsing or changing workspaces and bounded discovery.

- Match tab context actions to shared menu rows; group mixed file, editor, session, Output and panel actions under headings matching the app menu.

- Apply shared responsive sizing and viewport margins to dialogs, keeping headers and actions visible as content scrolls. Keep About at the same height across both tabs, with independently scrolling software notices; restore focus to the app menu after a phone drawer opens another dialog.

- Show the active filename, project and conversation in the browser tab and app window title, updating when they change.

- Keep the app-menu logo at its compact size and align the brand label in desktop and phone navigation.

- Retain original styled editor run boundaries independently of completed geometry, sharing them with paragraph preparation and uncached viewport paint. Bound retained metadata, reject stale scopes and release old-source runs; repeated paints avoid token-prefix segmentation while preserving exact source and pointer geometry.

- Replay unchanged editor paragraph suffix measurements after exact source/run, origin, dimension and overlap reconnection, retaining fresh measurement when positions or shaping change. Verify plain/styled geometry and captured ownership in both workspace modes.

- Preserve fractional positioning when measuring long editor paragraphs by separating integer and fractional CSS offsets. Verify the styled overflow rounding regression against complete glyph geometry in both workspace modes.

- Record the rejected paragraph-suffix reconnection experiment and its styled overflow rounding counterexample. Run unchanged browser CI contracts against optimized WASM to reduce module-loading memory without relaxing geometry or readiness checks.

- Synchronize the startup theme contract with the actual settings request, keeping cold IndexedDB initialization outside the unchanged theme-application deadline and checking prepaint throughout the held response.

- Reuse exact sequential lexical row positions across unchanged source, keeping indexed recovery and multiline-state validation after changed rows. Record native/WASM before-after preparation measurements without changing cancellation or paint limits.

- Keep editor save acknowledgements scoped to the original account and workspace root, preserving dirty drafts after folder/bridge changes or project removal. Verify background tab saves, permission failures and newer-edit history with held writes in both adapters.

- Add a near-1-MiB styled-string release measurement that requires grammar color through cold paint, scrolling and input; record both-adapter Linux latency/PSS and reject ineffective DOM-call batching without changing production geometry.

- Retain grammar colors on long prepared code lines; keep source/work admission and lexical fallback limits. Resume eligible styled viewport slices at retained original paint-run boundaries and preserve token wrappers when cropping, with exact geometry and source checks in both workspace modes.

- Extend the isolated native/WASM editor layout probe with identical-glyph advance precision and missing-glyph diagnostics; record the rejected early native bootstrap and retain exact production geometry.

- Use the explicit boundary-size readiness budget for the debug-WASM paragraph font matrix, retaining exact geometry assertions, the whole-run deadline and ordinary short UI timeouts.

- Reuse validated unchanged probe prefixes within edited long paragraphs, preserving exact source/run/style ownership and fresh continuation checks. Bound retained measurements and reject stale account, project, font and layout scopes.

- Build grapheme/native coordinates once per bounded paragraph probe instead of repeatedly segmenting sparse checkpoint prefixes for every overlap glyph. Retain every exact DOM glyph measurement, validation and fallback.

- Make built-in language parser contracts independent of runner scheduling with a scoped test clock; verify deadline cancellation separately while retaining the production parser budget.

- Resume editor text segmentation at proven original paint-run boundaries for continuation probes and indexed plain viewport slices, preserving the original shaping spans without repeatedly scanning the unused prefix.
- Replace Commands with universal search across commands, filenames, projects and sessions, including keyboard/touch navigation, background discovery and stale account/project/session cancellation in both workspace modes.
- Combine branding and project tabs into one app row. Move folder opening, recent projects, settings, servers, system prompts, help and About into the logo menu and phone drawer; keep Sessions focused on conversations.
- Combine editor tabs/actions and the app/editor footer. Add Edit/Changes/Preview, Inline/Split changes against the last commit, a compact indentation popover, and confirmed Discard changes/Convert indentation in menus and search. Add account-synced hidden-file/compact-tree preferences, refresh/reveal actions, shared selected/focus styles and Git status icons with per-file line counts.

- Prepare eligible unwrapped editor paragraphs in at most 16 KiB styled probes, preserving original paint-run boundaries and validating every overlap glyph before publishing exact extents and anchors. Keep glyph measurements near the origin and fall back to complete layout when proof fails. Tabbed, wrapped and bidirectional paragraphs retain complete preparation.

- Preserve original grapheme-safe text-run boundaries when painting a cropped editor row, including short crops of long tokens. Stream run boundaries without allocating a complete run vector or segmenting the unused suffix; retain exact geometry validation and full-row fallback.

- Add a reproducible paragraph-mutation probe with fresh Chrome processes, production font/run markup, geometry checks and explicit timeout records. Record why retaining DOM nodes alone does not resolve admitted Unicode paragraph stalls.

- Size the editor gutter from the current document’s source-row index, retaining hidden fold rows and the final empty row. Restore native input directly from the projection’s normalized value without making temporary whole-file newline replacements.

- Flush pending source paint before an editor pointer gesture when background analysis has temporarily invalidated readiness. Keep source/account validation and exact glyph hit-testing for bounded native input.

- Schedule cooperative lexical rendering by elapsed preparation time, with a hard batch cap and conservative clock fallback. Memoize shared editor rules so missing/failed config reads do not re-detect settings on every ownership check. Keep task yields, source/default/file invalidation and stale-source cancellation between batches.

- Validate editor paint and cursor geometry against borrowed, newline-normalized source instead of creating temporary normalized copies for cold probes, cropped rows and visual motion.

- Compare complete native editor input in borrowed byte chunks, falling back to character normalization at CR/LF and Unicode edges. Keep minimal source edits, duplicate composition detection and LF/CRLF behavior shared across workspace adapters.

- Validate embedded-language range points through the parser’s incremental source-row index instead of rebuilding whole-file line starts. Share point mapping with parser edits and preserve invalid-range and cancellation fallbacks.

- Stream grammar paint segments from ordered protected regions, embedded scopes and semantic spans, removing the full boundary set/vector allocation while preserving disjoint paint and duplicate-edge handling.

- Compare shared editor source changes in byte chunks and adjust only the differing edges to UTF-8 boundaries. Preserve minimal edit spans across typing, folds, parser updates and worker deltas; record native and browser before/after measurements.

- Reuse indexed raw row boundaries outside a validated lexical source-change span. Preserve multiline state propagation, LF/CRLF normalization, terminal-row handling and cooperative batch budgets while scanning only intersecting rows for newline boundaries.

- Keep the clickable chat context meter at the shared compact button size on desktop, preserving larger phone touch targets.

- Reuse complete lexical row/context and token tables for unchanged source, preserving the current immutable source handle and rejecting language or newline-normalization mismatches.

- Keep Files tabs visible during on-demand search, focus search from its header control, and dismiss it with Escape. Make chat context usage clickable and compact on narrow screens. Use equal icon-and-label phone navigation and show Output as a sheet, with one header, optional command input and a copy-output action.

- Share immutable editor source with syntax request scopes, cooperative lexical jobs, parser preparation and validated worker results. Retain resolved worker request strings directly and preserve stale-source, cancellation and cache limits.

- Share immutable editor source between the Rust document, active view, retained file buffers, project snapshots and Find scopes. Publish edit/composition results as shared handles, preserve older views, and keep owned strings at write/recovery transfer boundaries. Validate projection provenance by both immutable source and document version, including fresh documents initialized from the same allocation.

- Add server-scoped All tools, Selected tools and Chat only controls with estimated per-tool schema cost and model-context share. Persist selections, filter shared local/remote/projectless requests and child agents, and reject unadvertised calls before execution. Count delegation during planning and shorten tool descriptions while preserving parameter schemas and approval rules.

- Reuse indexed line-ending summaries for native normalization and row-height eligibility, avoiding full projection-text scans in unfolded and folded editor views.

- Share lazily prepared visible-row tables with unfolded editor projections. Edit batches update changed rows and shifted suffixes, retain unchanged prefixes and preserve immutable older views; row growth uses bounded headroom and large deletions release excess capacity; folded and bounded projections keep their own visible rows.

- Share unfolded editor coordinate tables with the document index. Retained views detach on edits; folded and bounded views keep their own row coordinates, while unused projection caches allow in-place index updates.

- Share unfolded editor projections with the document's immutable source; LF native input shares that allocation too. Folded views reserve only visible bytes and keep assembled strings without copying them into another storage type. Release unused projection caches before edits, preserve retained snapshots, and normalize native text in one pass.

- Validate projection provenance by immutable text allocation, preserving empty-document replacement rejection for native replay and deferred selection paint.

- Expose source-pointer readiness separately from retained paint visibility, and await that capability in highlighted click/drag checks so a visible retained frame is not mistaken for an interactive replacement.

- Validate retained syntax scopes with the shared source revision and immutable allocation ownership, avoiding full-file comparisons for current worker/fallback snapshots. Content replacement invalidates old scopes even when bytes match; external snapshots retain complete-byte validation.

- Await current source hit-test geometry before highlighted click regressions, so CI does not dispatch clicks during a pending layout/font replacement; preserve exact column and line-end selection assertions.

- Share one guarded source snapshot across repeated syntax worker/fallback requests and tab-width changes. Build it directly from borrowed editor content, clear it on reset or requests without an open file, and reject changed source, project, account, reload or review ownership.

- Compare complete textarea input with borrowed, newline-normalized source characters instead of copying the full normalized file. Map native edit boundaries directly to source bytes, preserving Unicode and CRLF behavior; duplicate composition commits share the same comparison without changing selections or history.

- Update indentation guides from changed document rows and their neighboring blank runs. Ordinary text edits retain the existing immutable guide table when indentation is unchanged; edits that change guide values reuse retained columns without scanning unrelated source rows.

- Query indentation guides from shared indexed document rows and cache immutable results across repeated queries. Reuse disabled-guide tables for same-row-count edits above the structural limit, and resolve blank-row continuation in one pass while preserving tabs, Unicode whitespace and LF/CRLF behavior.

- Install shared editor document rows before syntax preparation and publish bounded neutral unwrapped frames before animation callbacks. Revalidate source-owned native input without reinstalling identical paint; preserve source/font/account guards and deferred styled, wrapped and oversized paint. Release old native window bindings when complete short text fits, restoring current source scroll extents.

- Make editor browser checks wait for styled token paint and verify repeated-row measurement bounds per probe, including font/layout retries on Linux.

- Paint bounded unwrapped editor viewports before terminal lexical fallback finishes, borrowing projection rows. Bind native windows from complete native dimensions; restore complete native input for unsupported cold viewports and retain active composition mappings.

- Keep rectangular cursor gestures on their source version without retaining file copies. Reuse indexed logical rows for column selection, preserve tab/Unicode/CRLF behavior, and reject stale project, account, reload, source, document and indentation state. Borrow source for fallback indentation rules and native selection/clipboard checks.

- Keep deferred active-line and bracket decorations source-free. Borrow current text, query indexed UTF-16 row coordinates and reject queued marks after source, selection, syntax, layout, project or account changes; skip bracket preparation away from brackets without copying the file.

- Reuse cold editor row dimensions for identical paint after two matching layout samples. Check every measured batch for conflicts before sharing dimensions; retain fresh layout for distinct rows and preserve source, font and account ownership checks.

- Bind bounded native input for ready unwrapped editor frames before full-file row measurement completes. Validate the current frame, complete native source and dimensions, retain its scroll extents before the handoff, and stamp restored windows before scroll reconciliation. Wrapped input and active composition retain their existing ownership boundary.
- Reuse incremental source-line coordinates for parser fold validation and shared fold assembly. Avoid splitting grammar-backed files or rebuilding their logical rows for each fold query; preserve LF/CRLF columns, closing-line siblings, directives and existing fallback limits.
- Separate lexical structural scanning from source ownership. Parser preparation and lexical folding consume borrowed metadata without copying complete files or embedded-language bodies into temporary structures; owned editing contexts use the same scanner and region queries.
- Maintain parser line coordinates incrementally and use them for edit positions and grammar paint boundaries. Share changed-row reconstruction and suffix rebasing with document UTF-16 indexes, preserve Unicode and LF/CRLF behavior, and bound direct parser updates to the existing row limit.
- Assemble grammar paint one row at a time and retain unchanged rows' piece-list allocations. Validate classification and piece identity while streaming; rebuild changed rows without allocating temporary lists for the complete file.

- Prepare the browser parser compiler by probing installed Clang versions before using package mirrors. Bound fallback package requests and setup duration so compiler installation cannot stall CI for hours.

- Share the parser’s exact source-change span with grammar paint when its retained source owns that base. Avoid repeated unchanged-byte comparisons; skipped paint versions fall back to comparing the retained source, and cancellation clears the span.

- Add slash-command suggestions, argument hints and searchable `/help` to chat. Group consecutive tool and finished reasoning events into activity summaries, opening approvals and failures automatically. Animate running tool and activity headings with the thinking spinner.
- Add explicit `/compact` with shared model fallback and budget checks, cancellation and stale-history rejection; retain original messages. Add session-scoped `/goal` objectives with persisted pause/continue/completion controls and shared local/remote run orchestration.

- Add About to the account menu and command palette, with separate About and Open-source software tabs for the frontend build version, commit, shared dependency inventory and bundled license notices for crates, Monaspace and Lucide. Load license notices only when opening their tab, with a bounded scroll area and a centered, compact About view. Cache the same tabs, build details and notices for the offline PWA screen.

- Keep the visible caret at the end of highlighted lines when clicking beyond the text. Measure the final token boundary instead of a standalone newline with an empty browser rectangle; verify painted caret position as well as insertion offsets.

- Avoid full-file copies during editor selection-overlay updates. Retain scoped projection ownership, borrow source for visual caret measurements, and reject queued measurements after source, projection, selection or account changes. Expand pointer probes across row padding, with selectable line endings and wrapping.

- Support Git over SSH in Docker with an OpenSSH client, opt-in host-agent forwarding and read-only public configuration import. Keep strict host-key checks, add pull/push credential guidance, and document native, Docker, Quadlet and HTTPS setup.

- Reuse color classifications from unchanged parser descendants while refreshing subtree roots that depend on external parent fields. Declare custom classifier dependencies and retain fresh extraction for document-dependent selectors, with shared limits and cancellation across worker and fallback preparation.

- Retain source-validated grammar paint pieces and immutable token rows across edits. Reuse unchanged token allocations when classification, embedded language and piece boundaries still match; repaint changed contexts and clear the cache on cancelled analysis.

- Reuse unchanged syntax subtrees inside classes, implementation blocks and other nested containers. Extract multiline containers and rebuilt wrappers separately, validate retained parent kinds, and keep external fold headers and interpolation owners fresh.

- Revalidate enclosing syntax classifications before reusing interpolation contexts, preventing edits elsewhere in a file from incorrectly protecting unchanged code while preserving reuse for stable ancestors.
- Extend highlighted editor pointer verification to ordinary clicks just past the last character and farther into blank space, checking insertion and undo in Rust, C# and JSON in both project modes.

- Reuse editing contexts from unchanged parser subtrees, rebasing protected regions, interpolation holes and selection ranges. Share retained-tree identity and work limits with folding; document-dependent custom classifiers and external interpolation owners use fresh extraction.

- Reuse parser fold descriptors from unchanged top-level subtrees across edits, rebasing shifted positions and checking closing-line text without another full tree walk. Keep analysis limits, embedded languages and parent-owned headers intact.
- Mark pending editor recovery checks as busy for assistive technology and wait for them before production pointer verification, avoiding gesture coordinates measured across the disappearing recovery banner.

- Reuse unchanged source structure and syntax token rows when tab width changes, while recomputing indentation-dependent folds. Source edits and cancellation still invalidate preparation.

- Publish command, clipboard, search replacement and composition results directly into shared editor state, returning caret selections without another full-file text copy.

- Keep highlighted editor clicks and drags at the intended column, including blank space after a line and padding above or below its glyphs. Validate browser hit results against measured text boundaries; cover trusted press/move/release plus insertion/undo in Rust, C# and JSON drafts in both project modes.

- Share source text, line indexes and prepared projections across document and composition snapshots. Detach edited versions with insertion headroom, retain unchanged snapshot coordinates/history, and restore original allocations and prepared projection when composition is cancelled.

- Borrow current editor source during pointer selection and rendering instead of copying or retaining the whole file. Reject drags from replaced documents even with identical text/revision, and add trusted Chromium highlighted-click checks for small, bounded and scrolled drafts in both project modes.

- Publish editor worker structural metadata as bounded list patches against the acknowledged analysis ticket. Retain standalone snapshots for small lists or language changes, reject stale/missing bases and invalid ranges, and check expanded record budgets before reconstructing and validating coordinates.

- Share immutable saved-text baselines across document and composition snapshots. Save acknowledgements replace only the written baseline, reuse identical versions and propagate one allocation through active composition while preserving dirty-state and recovery behavior.

- Share immutable undo transactions and retained history steps across document/composition snapshots. Appending a typing group detaches its metadata while reusing earlier edit payloads; undo, redo, divergent edits and per-document retention limits remain independent.

- Restore cancelled composition before publishing its borrowed preview and committed source. Avoid full-file cancellation copies for stale owners and allocate UI source only where the matching active view or project snapshot needs restoration, with shared local/remote guards.

- Borrow editor source when scheduling cursor/page movement and remove the motion queue's full-file copy. Guard queued requests with document identity as well as revision, selection and projection, rejecting replacements with identical text without affecting shared local/remote movement behavior.

- Add release-app Chromium composition and history checks to CI for local and remote drafts with LF and CRLF. Exercise pending syntax and bounded native windows, verify candidate updates/commit/cancel and one-step undo/redo against full recovered source, and retain physical PWA input/clipboard verification on the roadmap.

- Compress unchanged worker token rows into validated runs, retaining shared token allocations and the facade's immutable source snapshot in both workspace modes. Reject invalid run counts/ranges and reconstructed token-budget overflow. Warm syntax queries reuse that snapshot instead of cloning the whole buffer while preserving source-change cancellation guards.

- Send small editor worker requests as source replacement spans using shared Rust policy. Resync once with a full snapshot after cache eviction or a discarded base, reject invalid UTF-8 ranges and reconstructed limits, and prevent superseded retries from publishing across projects or accounts in either workspace mode.

- Publish small worker source updates as validated UTF-8 replacement spans against the acknowledged snapshot in both workspace modes. Preserve Unicode and CRLF, reject malformed ranges or mismatching sources, and retain standalone replies for stale or evicted bases.

- Publish unchanged worker token rows by validated base-ticket references instead of sending and reconstructing their token spans. Preserve shared row allocations in both workspace modes, reject stale or mismatching bases, and send complete results when a base is unavailable.

- Share compact immutable token rows during lexical updates and editor paint preparation. Reused rows retain their token strings instead of copying them across source revisions, while worker transfer validation and rendering keep the same content and scope guards. Defer full-row probes until cooperative fallback tokens resolve, avoiding discarded neutral cold-layout batches while preserving pending-worker source previews.

- Reuse unchanged lexical rows with matching incoming context in the shared Rust worker and cooperative editor fallback. Propagate changed comment state until convergence, preserve Unicode/CRLF across row shifts, and reset fallback reuse across file reads, documents and accounts in both workspace modes.

- Prepare terminal and unavailable-worker editor lexical paint cooperatively in Rust in both workspace modes. Preserve multiline state and Unicode/CRLF, share complete tokens and immutable worker source, coalesce edits, and discard cancelled or stale source/read/account jobs before publication.

- Reconcile late editor font notifications against current source-owned measurements using actual registered-face identity and load state in both workspace modes. Briefly await pending faces with a 250 ms budget, retain editing during slow or failed loads, and reject superseded jobs before measurement.

- Reuse exact row dimensions and glyph anchors when pending editor syntax resolves to an identical single plain-text run in both workspace modes. Preserve remeasurement for changed token spans, styles, fonts and source ownership.

- Separate opt-in editor cold-preparation traces into Rust rendering, DOM installation, row layout and source-geometry phases, retaining immutable ownership scopes and bounded diagnostic records in both workspace modes.

- Skip duplicate full-file lexical highlighting while editor worker results are pending. Borrow projected row bodies for cold paint, retain existing styled frames and defer their replacement measurements until syntax is ready in both workspace modes. Completed or failed analysis keeps the contextual lexical fallback.

- Avoid copying complete editor files for source-ownership checks, cursor counts, Select All and navigation queries. Map native window/fold/source selections through the shared facade without cloning source in selection, typing, paste or composition handlers. Ordinary typing keys skip the selection dispatcher’s file snapshot; edit and stale-context behavior stays shared across workspace modes.

- Reuse indexed admission counts for unchanged editor rows during typing, commands, multi-cursor edits and IME previews. Scan inserted text and joining rows while preserving byte/line limits, CRLF joins, failure ordering and history; non-admitted source retains full validation.

- Bind prepared desktop editors to scoped native surrounding text in both workspace modes, preserving full-source selections, clipboard actions, page navigation, chat capture and grouped history. Keep source scrolling independent of the input window, reject stale window/read/account events, and retain composition ownership across fold metadata updates. Reserve a consistent 2 MiB frontend WASM stack and reduce editor view return-value copying. Initial cold/touch input and physical PWA verification remain in progress.

- Consolidate repeated workspace, bridge fallback and reconnect guidance into linked references. Divide long documentation paragraphs and organize editor and design-system references into topic sections. Replace the roadmap's repeated editor implementation history with remaining tasks and links to detailed guides.

- Render documentation Mermaid fences in downloaded/file previews as well as hosted pages, with theme-aware ELK flowcharts and readable source fallback. Replace the architecture Components text tree with a Mermaid overview of the browser, Spin, bridge, files, database and model services; fix the agent sequence diagram's note syntax.

- Organize documentation into Getting started, Using Open WebIDE, Hosting and administration, Development, and Project, with section overviews, first-session and workspace guides, collapsible navigation and breadcrumbs. Validate navigation coverage during site builds while preserving existing guide URLs.

- Preserve complete source selections when replaying clipped native editor contexts, including Unicode/CRLF replacements, multiple cursors, grouped undo and composition previews in both workspace modes. Capture scoped surrounding text, reject stale document ownership, and rebase matching native values after commits. Existing input fallback borrows source; prepared desktop textarea binding is included above.

- Standardize app, container, service, deployment and tooling names on `openwebide`. Give Compose stable project/volume names with existing-volume overrides, document migration from older installs, and retain legacy generated-site marker recognition.

- Prepare SemVer release automation with full CI gates, native Linux amd64/arm64 GHCR image builds, macOS/Linux bridge archives, version/changelog validation, checksums and downloadable Compose/Quadlet install files. Support alpha, beta and RC channel tags and GitHub prereleases while reserving `latest` for stable releases; prerelease Quadlet downloads follow their own channel. Add release/configuration/backup guidance, support issue forms, architecture diagrams and Monaspace font credits; ignore generated Python/browser tooling output. Publication and release-host verification remain pending.

- Add shared project/file tab context actions for closing other tabs, closing tabs to either side and moving tabs left/right. File tabs reuse the tree’s file, Git and chat actions; bulk closes protect unsaved buffers with one guarded confirmation. Resolve editor gutter/container hits against measured text boundaries, including clicks past line ends, in both workspace modes.

- Retain rebased folding indicators and the last styled editor frame while syntax analysis and repainting are pending in both workspace modes. Replace indicators with authoritative ranges, disable obsolete fold controls, and release retained paint when the document, read, project or account changes.

- Scroll and extend prepared editor source selections when dragging near or beyond vertical and horizontal viewport edges, including stationary pointers, in both workspace modes. Share bounded speed rules in Rust, retry pending paint, and stop on release, blur or stale document/read/project/account/fold state. Cold and touch selection remain pending bounded input work.

- Keep the editor folding column reserved while syntax updates, preventing horizontal jumps when pressing Enter or editing files without folds.

- Invalidate editor dimensions and source geometry when switching already loaded font families, including Chrome with an empty font shorthand. Font properties and OpenType options share the same identity in measurement probes and viewport observers.

- Derive prepared editor scroll width and height from source row measurements in both workspace modes, reusing unchanged dimensions after edits and rejecting stale account/syntax/layout results. Preserve trailing padding without letting clamped native scroll echoes move the source viewport. Coalesce row planning and geometry probes until the next frame, measuring only the latest edit in an input burst. Cold and superseded layouts retain native fallback while bounded input remains in progress.

- Select and drag prepared editor source with shared Rust caret, word and logical-line rules, including Shift-click anchors and Unicode/CRLF direction, in both workspace modes. Discard drags after source, read, project or account changes. Route single-selection copy and cut through the shared source clipboard policy; cold/touch selection remains pending bounded input work.

- Paint the primary editor caret and selection from the same source geometry as secondary cursors in both workspace modes. Route single-cursor arrow, word and document navigation through the shared Rust engine, revealing offscreen carets through the common scroll viewport and rejecting stale paint/read/account results.

- Replay native editor edits and IME previews against borrowed source pieces in both workspace modes, preserving multiple cursors, repeated-text selections, Unicode/CRLF offsets and atomic rollback without building a complete replacement value. Reject IME previews whose eventual cursor replicas exceed editor limits. Composition snapshots and full textarea layout remain pending bounded-input work.

- Map native editor input directly to source replacements in both workspace modes, preserving CRLF, Unicode selections and hidden code without normalizing the complete source before replay. IME and multiple cursors retain shared replay validation; full source publication and textarea layout remain pending bounded input work.

- Bundle all five Monaspace editor font families with account-synced family, texture-healing and coding-ligature preferences. Enable both features by default and cache fonts for offline PWA use; share font metrics across input, syntax paint, gutters and diffs in both workspace modes.

- Separate document scrolling from the native editor input in both modes, preserving wheel, caret navigation and restored viewport positions while rejecting detached or previous-account scroll events. Native input still retains full source pending bounded layout work.

- Apply editor transactions and grouped undo/redo to the existing String buffer after validating proposed pieces, without building a full replacement candidate or doubling capacity for ordinary typing. Rebuild overlapping row contexts once, preserve distant interior indexes and unaffected collapsed folds, and keep failed edits atomic in both modes.

- Avoid reading the full textarea value after a validated trusted single-cursor insertion without folds in both modes. Preserve reconciliation for synthetic events, folds, multiple cursors and input fallbacks.

- Stabilize overflowing editor file tabs by removing percentage-height feedback, keeping tab nodes, dirty indicators and horizontal scroll position stable in both workspace modes.

- Commit browser-native ordinary typing through shared grouped editor transactions in both modes, preserving Unicode, CRLF, multiple cursors and rejected-edit state. Keep native IME/non-cancellable replay and share newline policy across both input paths; reject stale source/read/account commits without resetting the full textarea for single-cursor typing.

- Add reproducible Linux browser measurements for the built editor, with CJK/emoji fonts, required apportioned Chrome memory, recorded resource limits and fresh-run repetitions. Record wrapped and unwrapped byte, row-count and long-line boundary observations in both workspace modes; keep responsiveness and device validation open.

- Avoid discarding initial editor geometry when browser fonts are already settled in both workspace modes. Keep font completion/failure invalidation and source-bound measurement diagnostics.

- Preserve validated editor glyph anchors when equivalent syntax paint precedes row-height reconciliation in both workspace modes. Track actual font invalidation independently so layout bookkeeping retains proven geometry while font loading and stale source/read/account callbacks remain rejected. Add opt-in bounded production probe, worker and font timing traces.

- Slice long editor rows from validated styled anchors before generating and parsing HTML in both workspace modes. Preserve token styles, Unicode offsets, tab origins and complete logical extents; reject partial measurement failures and restore complete source before reprobe. Retain equivalent styled syntax geometry for unwrapped rows within the same font/layout epoch.

- Stabilize editor file tabs in both workspace modes: allow room for horizontal scrollbars without vertical overflow, retain tab nodes through unrelated updates, and reserve dirty-indicator space to prevent width jumps.

- Reuse wrapped glyph anchors from cold row-height measurement for first paint in both workspace modes. Keep geometry in the bounded shared cache, reject older source/read/account/syntax/font scopes, and remeasure when fonts load even if computed font text is unchanged. Preserve proven geometry across equivalent styled syntax results; cold measurement still shapes complete rows.

- Reuse exact styled anchors for newly visited wrapped editor intervals in both workspace modes. Slice bounded source before layout, preserve full row heights and native offsets, and restore full-paragraph measurement if shaping changes. Initial shaping and large-file input remain in progress.

- Correct editor performance measurements to scroll single unwrapped rows horizontally and verify single-row destination fragments before recording readiness. Record actual long-line input, scroll and process-memory observations in both modes; mark older unchecked single-row scroll values unverified.

- Retain exact styled horizontal glyph anchors for long editor rows in both workspace modes. Newly visited intervals isolate bounded source slices before layout, validate cached anchors and preserve full extents, Unicode offsets and tab origins; unsupported geometry or shaping restores full-row measurement. Initial shaping and bidirectional windows remain in progress.

- Reuse validated editor fragment paint when revisiting unchanged scroll intervals in both workspace modes. Bound retained paint to 16 entries and 2 MiB, invalidate source/account/read/fold/syntax/indentation/font/layout changes, and include shaping properties in row-measurement identity. New intervals still require styled probes.

- Share sparse long-row grapheme coordinates and horizontal-fragment eligibility across unchanged editor rows and folded views in both workspace modes. Paint and cursor probes reuse them; native/WASM measurements compare construction, distributed Unicode queries and metadata retention. Styled visual boundaries and row shaping remain in progress.

- Default chat and agent replies to the remaining model context capacity, recalculated after tool results and compaction in both workspace modes. Honor explicit output limits, retain bounded summary generation and use the provider default when context capacity is unknown.

- Window long unwrapped editor rows horizontally in both workspace modes, retaining full scroll extents, Unicode coordinates and original tab stops. Share fragment cloning and glyph validation with wrapped paint; use a single-caret probe to reveal omitted Find/navigation targets beyond movement limits, and retain complete bidirectional paragraph rendering. Repeated shaping and bidirectional windows remain in progress.

- Paint long wrapped editor lines as bounded visual fragments while retaining exact logical heights and complete input source in both workspace modes. Validate glyph positions after reshaping, preserve native pointer/selection mapping, and reveal omitted text through measured Find/navigation geometry. Full-row temporary shaping and horizontal long-line windows remain in progress.

- Map editor pointer hits through validated per-row paint coverage and projected native offsets in both workspace modes. Reject stale paint before pointer selection, preserve Unicode/CRLF and folded coordinates, and validate DOM text lengths without copying entire painted strings.

- Share gap-aware editor paint coordinates for carets, bracket marks and secondary selections, with Unicode, EOF and malformed-coverage browser contracts. Keep current complete-row rendering while preparing fine long-row paint in both workspace modes.

- Measure offscreen wrapped cursor neighborhoods on demand in temporary bounded batches instead of retaining hidden paint rows. Keep persistent editor paint in its viewport window and preserve exact movement for distant cursors in both workspace modes.

- Bound long-line native offset and cursor line/column queries with sparse Unicode coordinate checkpoints shared by documents and folded views in both workspace modes. Preserve CRLF and surrogate boundaries through edits, undo/redo and composition; add matching native/WASM construction and query measurements.

- Apply queued wrapped cursor movement before typing, composition, paste and cut while the full editor layout is still preparing in both workspace modes. Measure exact styled cursor neighborhoods through the shared motion engine, retaining Unicode, folds, sticky columns and stale ownership guards.

- Give the expanded frontend browser CI suite a five-minute runner budget, retaining individual UI readiness deadlines and all behavioral checks.

- Reuse exact unchanged wrapped-row heights after localized and disjoint edits, row insertions/deletions and undo in both workspace modes. Guard reuse by document ownership, layout, styled tokens and line endings; invalidate cached heights on font changes.

- Measure cold wrapped editor layouts in temporary row/byte batches, keeping native input visible and yielding to browser tasks and frames in both modes. Reject superseded jobs, preserve queued arrows while progress advances, and keep projection identity stable when cursor motion does not change folds. Incremental height reuse and fine long-row rendering remain in progress.

- Add Open Graph and Twitter large-image previews to every website page, plus an open-source credits page generated from Cargo and site-build dependency metadata.

- Add a WebFinger discovery alias for `@openwebide@openwebide.com` that resolves to the existing Mastodon account.

- Show a screenshot of the running PWA on the project landing page, with a full-resolution image for sharing.

- Update project links and Pages configuration guidance for the repository transfer to `openwebide/openwebide`.

- Add a disposable production-editor benchmark for native browser input, scrolling, frame stalls, WASM allocation and Chrome process-tree memory in both workspace modes. Record byte, row-count and long-line boundary baselines; bounded cold paint and incremental input remain in progress.

- Synchronize native editor viewport dimensions before paint and wrapped cursor measurements, keeping immediate and queued movement aligned when resize or scrollbar geometry settles between frames in both workspace modes.

- Window wrapped editor paint using exact browser row heights in both workspace modes. Preserve global Unicode/CRLF caret mapping, offscreen multi-cursor motion and resize remeasurement; reject stale layout tables. Initial full paint and bounded cold measurement remain in progress.

- Add a dedicated project landing page and GitHub Pages build workflow for openwebide.com. Generate documentation directly from repository Markdown with automatic navigation, validated links, shared app colors and buttons, GitHub Issues links for feedback and support, and Bluesky, Mastodon, and YouTube profile links with Mastodon website verification support.

- Open files beyond full-editor byte, line-count or long-line limits in bounded, read-only Unicode text pages in both workspace modes. Keep complete source separate from page text, preserve before/after review access, reject oversized interactive transactions before indexing, and recover clean oversized tabs by reopening their host file.

- Show the house logo in the README and a vertically centered chat welcome in both workspace modes, including empty saved sessions. Replace terminal execution labels with the app name and prompt guidance; share the brand mark with the wordmark and keep the welcome responsive when the pane resizes.

- Replace the code-bracket logo with a softly rounded house silhouette, thicker eaves, chimney and larger terminal-prompt cutout extending into the attic. Share the theme-aware SVG wordmark icon across both workspace modes and use matching square favicons, Apple touch and maskable PWA icons, cached with the app shell.

- Maintain incremental document line/UTF-16 coordinates through edits, grouped undo/redo and IME; reuse immutable folded/normalized view allocations across scrolling and caret consumers. Use indexed rows for commands and native selections, record native/WASM query and projection measurements, and keep composer growth compatible with the shared welcome container. Full wrapped viewport and end-to-end resource measurements remain in progress.

- Bound unwrapped editor paint, line-number gutters and fold controls to an overscanned row window. Cache syntax paint and indentation guides across scrolling, preserve global Unicode/CRLF caret offsets, and reveal offscreen Find/navigation targets in both workspace modes. Wrapped viewport rendering remains in progress.

- Prepare lexical JSON, TOML, YAML, SQL and Markdown paint through the same immutable syntax cache and Rust/WASM worker as grammar-backed languages. Preserve multiline comments, Unicode and CRLF; discard partial results on cancellation and reject stale source replies in both modes.

- Match wasm-bindgen's headless Linux Chrome flags in production-worker and recovery checks when running in CI, and include WebDriver failure details in test output.

- Prepare editor folds, structural contexts and syntax tokens in a dedicated Rust/WASM worker. Share the preparation engine and bounded LRU cache with the synchronous fallback, coalesce pending edits, reject stale or malformed replies, and wait for worker readiness before sending requests. Cache the matching hashed worker/WASM assets for PWA use; CI exercises the built worker in Chrome. Full viewport rendering and device verification remain in progress.

- Share one immutable syntax preparation per source and tab width across editor folds, structural commands and highlighting. Reuse source/context/token snapshots, preserve old snapshots across edits, and reject results after account, file-read, source or indentation changes in both modes.

- Split oversized Edit paint into escaped, Unicode-safe text runs so browser range measurement avoids scanning one enormous text node. Preserve grapheme clusters, syntax classes, source coordinates and wrap behavior in both modes.
- Add reproducible native/browser-WASM editor storage workloads with optional Crop/Ropey comparisons, CRLF-aware offset checks and recorded results. CI checks workload correctness without timing thresholds; production storage remains unchanged while viewport/worker work continues.

- Move cursors within long wrapped lines by indexing Unicode graphemes separately and measuring only each cursor’s current and neighboring visual rows. Locate rows with bounded DOM range searches, reuse measurements across cursors, and preserve pixel goals, folds and soft-wrap affinity in both modes.

- Protect PHP heredoc/nowdoc and shell heredoc text in shared editing and syntax paint. Expose PHP interpolated expressions and their braces, shell substitutions and arithmetic expansions as code; retain nested literal protection through enclosing string wrappers, Unicode/CRLF and incremental updates in both modes.

- Queue wrapped cursor movement while syntax paint is pending, preserving arrow order and Shift selections. Flush through the shared paint primitive before typing, editing, IME and clipboard actions; cancel stale file/account/projection requests and bound retries in both modes.

- Move multiple cursors by measured visual rows when word wrap is enabled, retaining horizontal position across short rows and soft-wrap affinity in the shared selection overlay. Skip hidden folds, preserve Unicode/CRLF and selection direction, and reject stale measurements atomically in both modes.
- Fix parser-disabled diff highlighting lint failures in backend and bridge builds by using the lexical fallback directly.
- Share full-document grammar and lexical paint across Inline/Split diffs, Git and recovery reviews, and chat diff previews. Combine syntax colors with word-change highlights, preserve Unicode and line-ending notes, and keep original line coordinates through alignment gaps in both modes.
- Add grammar-aware Edit highlighting through the cached shared editor facade and extensible provider selectors. Paint functions/types, HTML tags and CSS properties, preserve multiline literals/comments, and color interpolation and embedded script/style code independently. Preserve source text and CRLF overlay alignment in both modes.

- Use parser contexts for structural selection expansion and bracket navigation. Expand through named syntax nodes, expressions, blocks and functions while preserving selection direction and shrink history. Match interpolation code and embedded-language brackets, reject stale source data, and share the editor facade and bounded fallbacks in both modes.

- Use parser contexts for line comments, combining per-cursor line markers and CSS/HTML block-comment fallbacks in one undoable transaction. Clip inline script edits to their bodies, reject stale or escaping selections, deduplicate same-line targets, and share selection mapping with other editor commands in both modes.

- Use parser contexts for block comments, selecting the language independently for each cursor. Keep inline HTML script/style caret edits within their bodies, reject escaping selections and stale contexts, and retain selection direction and atomic undo in both workspace modes.

- Use validated parser contexts for selected-line reindent, preserving multiline literal content and separating HTML script/style bodies even after unclosed blocks. Reject stale contexts, retain undo and use the same editor facade in both workspace modes.

- Add a disposable live editor-recovery check against the built WASI API, SQLite and Chrome. Verify both-mode browser edits and autosave, selected-tab/draft restoration after server restart, reload and new windows, and durable close-all revisions that reject stale windows. Local browser recovery is checked without native folder access; native permission/device verification remains pending.

- Add recovered-file conflict reviews using the shared inline diff and modal components. Reload disk discards the draft; Save draft uses normal EditorConfig/save rules with one-use approval for the reviewed disk and draft versions. Preserve drafts on cancellation or stale editor/disk/root/account state, and explicitly recreate missing or empty files. Native folder-permission/device verification remains in progress.


- Wire automatic editor tab/draft restoration and debounced, serialized database recovery saves through one facade. Retain edits arriving during writes, show retry feedback, guard late loads and account/root changes, and require explicit choices for database revision conflicts. Check recovered baselines before enabling host Save and again before writes. Native folder-permission/device verification remains in progress.


- Add shared recovery client transport and coherent active/hidden buffer snapshots. Classify revision conflicts by HTTP status, validate recovery replies and save acknowledgements, keep transient IME text out of snapshots, and refuse dirty snapshots without a saved baseline. Guard shared REST session-expiry handling against old-session responses. Native folder-permission/device verification remains in progress.

- Add validated editor recovery snapshots and versioned user-scoped database/API storage. Preserve committed UTF-8/CRLF text, saved baselines, selections and collapsed folds; restore drafts as an undoable change. Guard project ownership/root identity and stale window revisions, retain close-all revisions, and keep draft bodies out of general settings reads. Native folder-permission/device verification remains in progress.

- Add editor file tabs with shared selected-tab styling, unsaved indicators, keyboard navigation and inline close icons. Keep independent buffers when selecting tabs; confirm unsaved closes, reject stale confirmations, and select an adjacent file after closing the active tab. Both workspace modes share the same facade. Native folder-permission/device verification remains in progress.

- Retain independent unsaved editor buffers when navigating between files, alongside each document's undo history, selections, folds and scroll position. Protect pending reads from newer input, project/account switches and repeated opens; guard hidden drafts against file mutations and clear affected buffers after confirmed delete/revert. Both filesystem modes use the same workspace facade. Native folder-permission/device verification remains in progress.

- Use shared parser contexts for paired typing/deletion and Enter, selecting the language per cursor in HTML script/style bodies and exposing template interpolation code while protecting literals. Retain bounded lexical fallback for incomplete input, including nested/escaped JavaScript templates. Reject stale contexts before changing text or history; richer highlighting and worker/performance work stay on the roadmap.

- Parse JavaScript and CSS bodies inside HTML independently, preserving global fold coordinates through Unicode/CRLF edits and declared-type changes. Share cancellation, recovery and bounded injection policy in both workspace modes; embedded-language highlighting remains in progress. Use stable Chrome in CI and optimize grammar dependencies in development builds.

- Add shared incremental grammar/folding providers for TypeScript/TSX, JavaScript/JSX, Python, Java, C#, C++, PHP, Shell, C, Go, HTML and CSS alongside Rust. Retain Python suite headers and distinguish JSX/TSX and Java/C#/PHP file types. Use extensible grammar descriptors and one browser C compatibility adapter; native and both-mode browser contracts exercise Unicode/CRLF updates, cancellation, size limits and recovery. Provider-backed editing, embedded-language contexts, richer highlighting and worker rendering remain in progress.

- Include fresh browser timezone, local date/time and UTC offset, locale, and 12/24-hour formatting defaults in run context for local, remote, and projectless chats. Share formatting across startup context and run transports, and omit unavailable values.

- Preserve multiline clipboard fragments across multiple editor selections using compact validated metadata. Restore each cursor’s fragment, including Unicode, CRLF and empty selections; match indentation per fragment when requested. Fall back to plain-text paste when metadata is missing, changed or invalid, with atomic limits and one undo step. Both workspace modes share the Rust engine and clipboard facade; native-device clipboard verification remains in progress.

- Add multi-cursor controls to Edit: occurrence, vertical cursor and expand/shrink commands in the shared editing menu and shortcuts; Alt-click toggles cursors and Alt+Shift click/drag selects columns. Paint secondary selections/carets with the syntax layer's text metrics and announce cursor counts. Move all cursors by grapheme, word, logical line or document boundary, retaining columns across short lines and skipping folded source. Both modes use the same Rust engine and facade; pending-paint motion queuing, large-file viewport preparation and real-device verification remain in progress.

- Integrate multi-selection clipboard operations with the shared editor engine. Copy/cut use source ranges across folds, cut waits for a successful clipboard write, and paste distributes matching lines or repeats the full text with one undo step. Failed clipboard access keeps the source unchanged in both modes. Real-device clipboard verification remains in progress.

- Replay native editor input across selections through the shared Rust document engine. IME previews retain the browser's primary input, then commit all selections in one undo step; cancellation, invalid frames and stale project/file/account events preserve the original document and history in both modes. Real-device input verification remains in progress.

### Changed

- Reuse cold measurement anchors for horizontal long-line paint as well as wrapped paint. Keep native input visible while the first geometry probe runs, and avoid a second complete-row shaping pass for the initial styled viewport.

- Bound retained editor typing contexts to 16 KiB through the shared Rust projection and typing facade. Preserve document/native offsets, partial rows, Unicode/CRLF, distant fold ownership and edits across larger forward/backward selections in both workspace modes. The textarea still lays out the full projected source; bounded native layout remains roadmap work.

- Preserve Edit viewport offsets while folding and unfolding. Refocus the native input without scrolling its distant caret into view, and keep syntax paint aligned in both modes.

- Add shared editor selection primitives: overlap normalization, next/all occurrences, grapheme/tab-aware columns, vertical cursors and expand/shrink selection. Preserve primary order/direction, canonicalize cursors after folds and edits, and replace selected ranges with bounded aggregate allocation and grouped undo. Route selection commands through the scope-checked editor facade and retain secondary selections during editing commands in both modes. Real-device verification remains in progress.

- Enable database-backed Word wrap and Show whitespace controls for Edit. Preserve native source offsets and text, measure wrapped fold rows after panel resizing, keep logical gutter numbers and navigate using rendered caret geometry. Normalize CRLF only in browser paint to match the textarea, retain horizontal scrolling by default and update decorations without regenerating syntax for a wrap-only change in both modes.

- Extend Find with case, Unicode whole-word and Rust regex options, source selection scope and match counts. Add next/all replacement with regex captures and atomic undo; retain Unicode/CRLF, reveal folded matches and reject invalid/stale queries or excessive match/output sizes. Keep pending review, read-only files and diff views protected, and share search/replacement policy in both modes.

- Add Edit navigation through the shared Rust editor facade: Ctrl/Cmd+G goes to a line/column and Ctrl/Cmd+Shift+\ jumps between matching code brackets. Reveal folded destinations, scroll long lines into view, show source cursor/selection status, and paint active lines, matching brackets and indentation guides. Preserve Unicode/CRLF source coordinates and reject delayed navigation after file/project changes in both modes.

- Retain unaffected collapsed blocks during native typing, editor commands, paste/cut, composition and undo/redo. Reveal only the source lines selected for a native edit; revalidate rebased fold anchors before restoring projected text and source selections. Keep unchanged native composition values and carets intact, including Unicode and CRLF, and share the behavior in both modes.

- Extend the shared folding provider with language-aware bracket/literal ranges, indentation fallback, consecutive comment groups and balanced explicit regions. Prefer Rust parser ranges; protect Python multiline strings, JavaScript regex/template literals and YAML block scalars from false folds. Use configured tab stops, preserve shared closing/header rows, bound synchronous work and clear cancelled/stale ranges in both modes. Embedded-language contexts and worker rendering remain in progress.

- Integrate Rust folding into Edit with gutter controls, cursor/recursive/all commands and keyboard shortcuts. Preserve source line numbers and syntax context, reveal Find matches, copy complete source selections and expand safely for editing/IME/clipboard operations; replay input-only browser events against full source without deleting hidden blocks. Use the shared editor facade in both modes. Provider-backed command contexts and advanced folding remain roadmap work.

- Build shared document fold state and source/visible-text projection for the upcoming folding controls: preserve logical lines, Unicode/CRLF offsets and directional selections; reveal hidden navigation targets, reject replacements across hidden gaps, and rebase unaffected headers through edits and grouped undo/redo. Route commands through the same editor facade in both modes; editor-view controls are described above.

- Add the shared incremental Rust syntax/folding provider, with Unicode/CRLF-safe tree updates, parser cancellation/size limits and per-file/project/account caches. Supply portable Clang/LLVM builds for browser WASM, CI and Docker; keep backend WASI builds independent of the optional parser. Provider-backed editing contexts and worker rendering remain in progress.

- The editor restores each file’s caret, selection direction and horizontal/vertical scroll position across file, project and edit/diff view switches; detached editor events cannot overwrite another view’s position.

- Add shared editor line movement, duplication/deletion, indented line insertion, snippet duplication and language-aware line/block comments. Expose commands through the existing action menu and familiar shortcuts. Preserve normal paste whitespace; Ctrl/Cmd+Shift+V explicitly matches snippet indentation. Add undoable selected-line reindent that ignores literal contents and preserves Python block depth, with unsupported actions visibly disabled. Share all commands across local and remote projects and retain Unicode, mixed line separators and directional selections.

- Add shared Rust block-aware Enter, closing-delimiter outdent and paired typing commands: auto-close language-supported brackets/quotes, wrap directional selections, skip existing closers and delete empty pairs. Keep strings, nested Rust comments/raw strings/lifetimes, Python triple strings and JavaScript regex literals opaque. Handle cancelable mobile input and desktop shortcuts through the same editor facade, retain Unicode/CRLF and atomic undo, and fall back to ordinary editing when synchronous structure limits are reached. Count indentation in visual columns when tab and indentation widths differ, and preserve selection columns inside rewritten indentation.

- Keep directory listings usable when an entry disappears during enumeration, including Chromium writable swap files and temporary bridge discovery probes. Skip missing metadata entries in browser, native bridge and WASI listings.

- Add shared Rust editor indentation settings: independent indentation and tab widths, per-file controls and explicit conversion, with user defaults saved in database settings. Both local and remote workspaces discover nested `.editorconfig` rules with root/section precedence and `unset`, then fall back to detected file style and defaults. Apply configured line endings, final-newline and trailing-whitespace policies as one undoable save command; retain caret positions and distinguish the saved snapshot from newer typing. Keep paint and diff tab widths aligned, and prevent unrelated typing groups from merging after editor remounts.

- Begin the Rust/WASM editor foundation: shared atomic text transactions, directional selections, bounded grouped undo/redo, per-file/project history, Tab/Shift+Tab indentation and indentation-preserving Enter. Route native typing and IME input through the shared editor facade, preserve CRLF and unrelated mixed line endings, reject stale file events, and provide Ctrl+M to let Tab move focus. Advanced editor roadmap work remains in progress.

- Limit rich Markdown gutter bars to changed list items and table rows, instead of marking their entire unchanged container. Keep unchanged changelog items unmarked in both Git and pending-edit previews.

- Diff Markdown prose within its existing lists, tables and inline formatting. Highlight changed words in place, so editing one word in a changelog item no longer duplicates the entire list in rich Preview; share the behavior across Git and pending-edit previews in both modes.

- Show green added, yellow modified and red removed gutter bars in rendered Markdown Preview for Git HEAD comparisons and pending agent edits, showing removed/replaced prose struck through in red alongside green additions in both modes.

- Refine file menus: enable New folder only on folders; distinguish Explain from Summarize; send chat shortcuts immediately while preserving unsent drafts/images; keep Review available for known changes. Share Git/chat menus with Changes rows and suppress the browser context menu throughout the app.
- Visibly dim disabled editor Preview options without hover highlighting. Support literal text previews for .txt/.text/.log and extensionless documentation (LICENSE/LICENCE, COPYING, NOTICE, AUTHORS, README and CHANGELOG) in both modes; keep Preview disabled for code and JSON.
- Enable editor Preview for Markdown, images, PDFs and plain-text documents, render PDFs through the browser viewer in both modes, and keep unsupported binaries in their placeholder view.

- Open existing session, server, prompt, chat-message and panel action menus with right-click, long press or Shift+F10. Preserve the inline menu buttons, shared styling, focus restoration and outside-click dismissal.
- Align backend history regression coverage with reload recovery: retain interrupted tool-call/result pairs with explicit unrecorded outcomes.

- Expand the editor roadmap with researched editing, indentation, folding, navigation and search/replace requirements, a Rust/Leptos WebAssembly implementation with thin browser glue, and explicit integration with the existing code-intelligence work.

- Offer Resume after reloading a local run with unfinished tool calls. Preserve call/result pairing in shared history, mark unrecorded outcomes explicitly, and continue with fresh turn IDs without replaying unfinished tools. Verify slow-model reloads, approvals and conflicting edit decisions across browser windows in both modes; user-confirmed OS-picked local folder access also survives reload without re-picking.

- Keep chat history message menus anchored to their prompt without obscuring history; outside clicks dismiss them normally.

- Keep Edit scrollbars above the syntax paint and outside the line-number gutter, with the gutter clipped to the visible text area when scrollbars or panel sizes change.

- Center chat prompt rows and their inline menus. Compact the status line and Send/Queue/Steer/Stop controls, add Ctrl/⌘+Enter to steer, and move Attach images into the Chat panel menu while preserving paste/drop.

- Show full-file Inline diffs, remove Content, and number logical lines in Edit, Inline and Split with compact, pinned gutters. Scroll long lines horizontally and keep Split panes synchronized. Add literal Find in file with next/previous navigation and Ctrl/⌘F.
- Show the existing PWA code mark beside the Open WebIDE wordmark.

- Explain each approval mode directly in its dropdown item instead of a shared footer hint.

- Keep Layout and Theme segmented controls compact in Settings, matching Sessions and Files.

- Keep the app within the viewport and give status-bar controls enough height to avoid page-level scrolling.

- Keep Minimize visible on every panel heading; menus contain only the remaining actions.

- Group Settings, Models and Log out under the username dropdown.
- Keep Terminal in the status bar as a full-width bottom dock with a resizable, remembered height; disable it without a project.

- Share one resize grip per vertical panel boundary, keeping drag direction correct after reordering and highlighting only the dragged grip.

- Standardize dock title-bar spacing and right-edge resize handles. Left-align labeled terminal actions and prioritize server names over provider types in narrow Sessions panels.

- Keep overflow menus inline with session and contextual rows. Place file creation actions beside Explorer/Changes and show them only in Explorer; use the shared Plus icon for all add/new buttons.

- Complete live Tailscale HTTPS verification: trusted certificates, PWA installability, REST/SSE/WSS in both workspace modes, and persistent node identity, Serve routes and app accounts after container recreation.

- Keep Git branch menus open during background status updates and avoid rediscovering branches when only working-tree status changes.

- Keep Run context and other transcript panels at their natural height in overflowing, narrow chat panes. History disclosure arrows point down when collapsed and up when expanded.

- Replace secondary action rows with shared inline overflow menus in chat (Edit/Fork/Rewind), sessions, servers/prompts, dock headings, files, Git diffs, and terminal controls.

- Standardize all dropdowns on the Recent menu’s shared surface, rows, keyboard navigation and dismissal. Use editor-style segmented switches and shared search rows throughout panels and settings; remove repeated reasoning/tool history rules.

- Display the top-bar wordmark as “Open WebIDE”.

- Add a minimize button beside each dock panel’s move controls; collapsing preserves mounted content and saved layout state.

- Switch Git branches from a shared selector in Changes and the footer, with New branch opening the existing name dialog. Load and switch repository branches through the same Git facade in local and remote projects.

- Keep one heading per tool panel, group session search and New chat together, and align Changes rows, selection, spacing and actions with Explorer.

- Share left-aligned disclosure headers across reasoning, run context and tool groups. Anchor resize handles to the full panel boundary so Terminal and other docked panels remain draggable regardless of their content wrappers.

- Build dialogs from shared sections, fields, notices and action bars with consistent spacing and visual hierarchy.
- Consolidate Explorer, Git Changes and persistent search into one resizable Files panel. Searching shows results; clearing restores the selected file view. Share resize handles, limits, keyboard resizing and user-scoped width saves across Sessions, Files, Chat and Terminal; move panel ordering controls into their headers.
- Use Lucide SVG icons through a shared component, larger action targets, fast accessible tooltips and labeled desktop top-bar actions. Give selected project tabs the same accent/background treatment as expanded panel tabs.
- Put prompt edit/fork/rewind actions inline, left-align thinking headings and group tool calls into initially collapsed disclosures with per-tool counts. Keep live output and pending approvals available, with inline Copy icons.
- Grow and shrink the chat composer with its content while reserving transcript space, align action buttons with the short input and allow image drops throughout the chat pane using the same attachment flow in every workspace.

- Keep an installed app's page and cached scripts on the same build during deployments; activate the new build after existing app windows close.

- Keep conversation forking busy until the new branch's draft and attachments are restored, preventing completion from racing the composer reset in every workspace mode.

- Add automatic phone layout with full-screen chat and mounted tool sheets, a saved Automatic/Desktop/Phone override, touch targets, a Terminal tool window, integrated Files/Changes/search views, and per-user panel ordering. Preserve drafts, terminal output and desktop panel preferences in local, remote and projectless chats.

- Make Open WebIDE installable with a manifest, icons, a versioned shell-only service worker and an offline server-unreachable screen. Add Settings installation guidance and browser prompting; exclude all API/bridge traffic from the cache and default HTTPS pages to same-origin `wss://<host>/bridge` while retaining explicit bridge settings.

- Make browser CI checks wait for session restoration, composer focus and persisted approval-mode changes instead of relying on microtask counts or fixed delays. Guard deferred layout saves after owner disposal or account changes. Wait for the HTTPS test's model fixture before streaming, and retain failure diagnostics.

- Clarify Auto approval policy so routine public-page research can be approved without the user naming an exact URL. Keep destructive actions, secret exposure, unrelated actions and uncertain decisions subject to manual approval; share the classifier prompt across browser, bridge and Spin runs.

- Add a `task` tool for parallel child agents with independent contexts in local, remote and projectless chats. Show nested collapsible runs with live status, elapsed time, tokens, tool counts, reasoning, output and inherited approvals; preserve child history through reload, Markdown export, fork and rewind. Use the fast model with primary fallback before any tool request, propagate cancellation, and bound recursion, model concurrency and tool/output budgets. Serialize file mutations through durable checkpoint/result recording so child edits participate in the parent’s review and rewind in execution order.

- Add session search across names and messages, durable pin/archive/restore controls, and full Markdown export in local, remote and projectless chats. Generate titles from the initial exchange using the fast model with primary fallback; serialize background attempts, preserve manual names and ignore stale account results. Keep pinned sessions first, archived history accessible, and metadata intact through rewind; forks start unpinned and unarchived with manual branch names.

- Add a searchable command palette (`Ctrl/⌘+Shift+P`) and keyboard-shortcut overlay (`Ctrl/⌘+/`), with a Commands button in the top bar. Support arrow keys, Enter, Escape and restored focus; reuse shared session, project, settings, model, panel and slash-command actions. Disable project-only commands in projectless chat, preserve drafts and close stale dialogs on account, project or session changes.

- Polish tool steps in local, remote and projectless chats with live elapsed time, persisted final durations, expandable ANSI-colored output and plain-text Copy (including an HTTP LAN fallback). Add per-prompt tool/file counts, including shell changes, and recorded model/tool time; mark incomplete timing as a lower bound. Exclude approval waits, freeze cancelled tools, preserve timing through reload, fork and rewind, and keep timing-save failures from stopping runs.

- Show live reasoning time to tenths of a second and estimated tokens for inline `<think>` blocks and provider reasoning in every chat mode. Freeze a compact “Thought for 3.2s · ~1.4k tokens” summary on completion, cancellation or provider failure, keep traces collapsed until expanded, and support keyboard toggling. Historical traces without observed timing show their token estimate without inventing a duration.

- Add private HTTPS deployment configuration for an external official Tailscale container, persistent same-origin `/bridge` Serve routes, loopback listeners, native and rootless Podman instructions, and a Caddy internal-CA alternative. Verify TLS REST, SSE and WSS in Docker and rootless Podman; keep the backend-only bridge secret bootstrap inaccessible through a proxy.

- Default new sessions to **Auto** approval mode and label the former Default choice **Manual**. Preserve saved modes and keep older sessions without a saved choice manual.

- Simplify model setup to one **Detect settings** action per chat model and a single modal **Cancel / Save**. Preview and testing leave configuration untouched until Save atomically commits server options, credentials, profiles and initial defaults. Keep only **Edit** on server rows and **New server** in the Servers heading.
- Add server presets and shared detection for Ollama, llama.cpp, LM Studio, vLLM, LiteLLM, OpenRouter, SGLang and KoboldCpp. Discover standard host endpoints on first setup, cache model facts by transport revision, preserve manual overrides, surface sampling, runtime context, size, quantization, loaded state, server version, CPU spill and tokenizer details when reported, and exclude embedding-only models from chat.
- Add optional model testing for structured and streamed tools, first-token latency and tokens/sec. Probe failures fall back to plain chat with a notice; tool streaming capability is scoped to each server and model. Shared project detection suggests test commands and linters in local and remote workspaces.
- Harden bridge lifecycle and HTTP handling: reap idle detached PTYs, send SIGHUP before forced termination, await delayed process cleanup at shutdown, time out stalled body reads and response writes, close HTTP/1.0 connections, support half-close responses, enforce WebSocket message limits, avoid response-body copies and bound displayed bridge errors. Compile Windows adapters in CI.
- Bound unterminated terminal lines, respect composer IME composition, restore background editor content after backup rejection while preserving new input, mark the active `/model` and support `/model default`. Normalize provider URLs with query strings or fragments without losing nested proxy prefixes.
- Extend auth, search, database rollback, model detection, transport, process lifecycle and browser parity regression coverage. Fake-backend deletions cascade, component click helpers accept combined classes, and frontend tests build natively without `--lib`. Revert the WASM size optimization that did not improve compressed size or startup.

- Establish shared feature facades for local and remote files, Git, execution hosts and session runs. UI send/stop/approval/resume actions use the run facade; planning, context, history recovery, streaming, persistence, permission policy and tool workflows live above thin browser, Spin and bridge adapters.
- Share provider model precedence, wire messages, plain and tool-stream lifecycles; split core domains and diff rendering, share line/word alignment and storage row mapping, and remove storage’s agent dependency. Use typed VFS creation/permission errors and Git responses while retaining browser thread checks and legacy wire formats.
- Verify common provider and filesystem contracts across adapters, including failure and fallback behavior. Browser chat-only runs share server reply persistence and telemetry; populated-directory deletion and binary Git previews behave consistently in both modes.

### Added

- Add a reproducible native/WASM text-layout comparison tool and recorded cold-layout candidate measurements. Keep experimental dependencies outside the production workspace and reuse its build directory.

- Add transactional editor recovery hydration and shared disk reconciliation primitives. Preserve saved baselines, ordered tabs and hidden drafts; reject stale editor activity and distinguish changed, missing and already-written disk text. Native folder-permission/device verification remains in progress.

- Add [file and folder context menus](docs/file-tree-menus.md) in both workspace modes: create, rename, move, copy path, confirmed delete/revert, status-aware Git tracking/staging/unstaging/ignore, and Explain/Summarize/Review chat shortcuts. Share filesystem policy and Git planning across adapters; guard unsaved buffers, pending reviews, concurrent operations and stale account/project/folder results. Preserve originals on failed moves and untracked files on revert.

- Add `todo_write` for an agent-maintained checklist pinned above the composer in local, remote and projectless chats. Show pending, in-progress and completed items with progress counts and collapsible themed scrolling. Persist prompt-anchored revisions in the database, restore the correct plan after reload, rewind and Fork, and include the latest checklist in subsequent model context. Reject stale prompt updates, preserve the plan when tool persistence or loading fails, and offer Retry for failed loads. Share tool policy and execution above thin browser, Spin and bridge persistence adapters.

- Add opt-in browser notifications for finished runs and approval requests in local, remote and projectless chats. Notify for chats that are out of focus, collapsed or inactive; clicking opens the owning project and session. Save the preference per user in the database, request browser permission only from Settings, explain unsupported/blocked browsers, and keep approvals available in the app. Suppress duplicate/replayed events and automatic approvals; guard pending permission responses and notification clicks across account changes, and close notifications on logout. Local projects require the app to stay open; remote and projectless chats also support Web Push.

- Queue prompts while a run is active, edit/remove pending prompts, pause or continue the queue, and steer by saving priority guidance before stopping the current run. Persist captured attachments and queue revisions in the database; consume each prompt atomically when its user message is saved, keep failed sends queued, and restore pending queues paused after reload or session changes. Add Edit and Fork on earlier prompts: copy the conversation prefix into a new branch while preserving the original session, images, editor context, model selection and approval mode. Share these workflows across local, remote and projectless chats; branching leaves project files unchanged.

- Attach immutable file contents, folder listings and Git diffs using `@file:path`, `@folder:path` and `@diff[:path]`, with keyboard autocomplete and quoted paths for spaces. Resolve mentions through shared workspace/Git facades in local and remote projects. Add image picker, paste and drag/drop with previews and removal, including projectless chat; persist images and reference snapshots with prompts for reload and rewind. Send real image inputs to Ollama and llama.cpp in plain and tool requests, normalize GIF/WebP and large rasters to PNG, expose detected/custom vision capability in model settings, and keep image pixels out of text compaction. Limit attachments to four images (2 MiB each, 4 MiB total) and reference context to 16 references/128 KiB. Guard preparation against cancellation and account/project/session changes.

- Add `/context` with a themed breakdown bar for the latest model request: system instructions, files, tool output, history and tool schemas, plus generated reply and free context. Share accounting with the compaction engine, scale estimated categories to the provider’s input count, and persist the breakdown with reply telemetry for reloads. Support local, remote and projectless runs through both streaming transports; retain `/tokens` for cumulative accounting.

- Add a changes panel beneath each new agent run’s prompt in local and remote projects. Review text changes per file or hunk, and created/deleted or binary files as a whole. Persist decisions in the database, mark pending lines in the editor, preserve accepted hunks during later edits, and keep rewind consistent after rejection. Capture shell changes even when a command fails; stale revisions and later manual edits cannot be overwritten.

- Add **Rewind to here** on chat prompts in local, remote and projectless sessions. Restore project file contents and conversation together, including shell/Git working-tree changes, created/deleted files and binary contents. Keep Git history, external effects and gitignored paths intact during shell rewind. Checkpoint snapshots and recovery history live in the database; conflict checks protect later edits, interrupted restores can resume, and unfinished or missing checkpoints refuse unsafe rewinds. Checkpoints capture up to 32 MiB and 10,000 project files, with a 10 MiB single-file limit; excluded paths remain unchanged.

- Keep a permanent speech-bubble chat tab at the right of the project tabs. Chat without a project using web search, page fetching and read-only `host_info`, with file, Git and shell access disabled. Save sessions and the selected chat in user-scoped database settings, restore them across devices, and preserve open project workspaces when switching back. Both bridge WebSocket and backend SSE runs use the same tool restrictions, permissions and compaction. Collapse and disable Files and Editor in projectless chat, restoring their saved visibility on return to a project. Remove the redundant open-project panel from chat.

- Add Rider-style vertical panel tabs: Sessions, Files, Editor and Chat together in one left tab bar. Collapse and expand panels without unmounting editors, drafts or running terminals; save visibility in user-scoped database settings and restore it across devices. Resize only visible panels, preserve collapsed widths, and reveal the relevant pane when opening a file, session, model setup or terminal in either workspace mode.
- Add `host_info` for local, remote and projectless chat. Read CPU, available/total RAM and disk capacity from the bridge host; report available temperatures, Linux fan RPM, NVIDIA GPU/VRAM/temperature/fan percentage, Linux AMD VRAM and macOS GPU inventory. Identify the bridge as the source and mark unavailable readings and container scope explicitly.
- Theme scrollbars throughout the app with the active dark/light palette, including panels, editor, terminal, dialogs and native controls.

- Set up model servers through a rerunnable wizard: choose Ollama or OpenAI-compatible, enter URL and optional write-only auth token, then discover models and review/customize settings. Retry discovery without duplicate servers, preserve saved tokens and manual model overrides, and choose an initial default model when applying reviewed settings. Launch setup from Servers, model configuration or the workspace toolbar in either mode.

- Automatically compact local and remote chat/agent context before model requests and tool continuations, using the configured threshold (85% by default, 0 disables). Reserve response and summary space, use provider tokenization with an estimate fallback, summarize through the fast model or primary fallback, and persist reusable summaries while retaining original history. Failed or cancelled summaries never replace history.

- Render Markdown tables with aligned columns, themed headers and borders, and horizontal scrolling for wide tables in agent replies and file previews. Support strikethrough while retaining sans-serif prose and monospace code in both modes.

- Distinguish user prompts with a compact, rounded, theme-aware background inset from the TUI panel edges and aligned on the right with tool and thought panels; remove repeated assistant headings while preserving a shared text alignment for prompts and replies.

- Keep the file tree ordered at every level: directories first, then files, using case-insensitive natural name sorting consistently across local and remote loads and refreshes.

- New chat immediately creates and selects a session, persisting its project startup context in both modes before the first prompt.


- Choose Default, Auto-accept edits, Auto, or YOLO from the TUI or with Shift+Tab. Persist session choices in user-scoped database settings and enforce them through one shared policy gate, with thin browser, bridge, and SSE adapters. Auto uses the configured fast model or the primary model and falls back to manual approval on uncertain, malformed, failed, or timed-out classification.

- Configure primary and optional fast models, per-model context/sampling/output/thinking/tool overrides, and an 85% auto-compaction threshold in database-backed settings shared by local and remote mode. Background work falls back to the primary model when no fast model is selected..
- Add model servers by URL, discover common local endpoints, detect model context/capabilities, and configure write-only API keys, proxy headers, timeouts, and Ollama keep-alive.

- Choose a session’s connection and model from a tiered TUI menu, with models discovered on expansion and new sessions starting from the configured default.

- Generate fresh startup context in local, backend, and bridge runs: environment and available tools, root and nested project instructions, and relative imports. Save the exact context as a collapsible session entry, with visible size and import limits.
- Automatically refresh the file tree in local and remote projects, preserving expanded folders and editor contents while pausing background tabs.
- Restore the last-used session per project when opening a project or a fresh browser window, using user-scoped database settings.

- Reload pending agent edits per project from the database and persist Accept/Reject decisions, retaining failed reviews and backups for retry.

- Store pending agent edits and review decisions per project in the database, with replay protection and revision checks; review UI integration follows separately.

- Keep terminal shells and output when hiding the dock, and start new shells in the active project folder with a visible workspace-root fallback notice, including when a remote folder no longer exists.

- Guard local browser file operations and agent completions with originating-thread checks, including cancellation when a completion stream is dropped.

- Reduce repeated recent-project filtering and statusline formatting work while preserving project order, telemetry text, and saved themes.

- Make dialogs keyboard accessible with focus containment, stacked Escape handling, focus restoration, and keyboard folder navigation.

- Apply the saved database theme before first paint without browser preference storage, and unify action buttons and theme-aware warning/diff colors.

- Reuse the browser database connection and remove saved local folder handles when projects are deleted, including stale handles owned by the signed-in account found at startup; preserve other accounts’ folders and handles saved during startup, including when the system clock changes.

- Handle CRLF and CR fallback chat streams and multi-line SSE data fields correctly.

- Clarify Open local / Open remote with device-based tooltips, a remote folder-picker subtitle, and matching documentation.

- Reduce the release WebAssembly download size with size-focused optimization.

- Debounce file-search typing by 250 ms while keeping clearing and ignored-folder toggles immediate.

- Load workspace settings and lists in parallel, and avoid redundant model requests when switching or renaming sessions on the same connection.

- Render terminal output incrementally with 10,000 lines of scrollback, split ANSI colour support, recovery from unfinished controls when processes exit or restart, progress-line updates, and scrolling that follows only near the bottom.

- Coalesce editor syntax highlighting to one animation frame while keeping typing and saving immediate.

- Keep conversation rows mounted while replies stream, reducing chat update work and preserving expanded reasoning and diff previews.

- Show streamed model reasoning and mark replies cut off by the output token limit; omit prior reasoning from model context.
- Preview file diffs before approving agent edits, expand long previews, and flag unreadable overwrites.
- Run `/test [filter]` in the terminal with shell-quoted filters and the project directory.
- Bundle the execution bridge in Docker and Podman deployments for terminals, Git operations, and streamed chat on port 3001.
- Resume interrupted local-mode runs from their saved conversation, preserving user messages and tool-step numbering.
- Add bridge-hosted chat/agent runs and streamed completions, with reconnect replay, cancellation, approvals, and TLS enabled by default.
- Add provider streaming for tool-call turns, with automatic non-streaming fallback for older llama.cpp servers.
- Add a frontend component test harness with a fake backend and headless Chrome tests in CI.
- **System theme option:** the default follows the OS light/dark preference and reacts live to changes; explicit theme choices are stored in per-user database settings.
- **Search "include ignored folders" toggle:** the file tree's search box now has a toggle button that also searches the ignored folders (`.git`, `target`, `node_modules`, `dist`, `.spin`); the hit and byte caps still apply, and toggling re-runs the current query. The flag is per query — not persisted, off on reload.
- **Bridge confinement:** canonicalize the workspace root and enforce lexical cwd bounds, while permitting directory symlinks.
- Agent edits back up original bytes before overwriting unreadable (binary or large) files; refusing an unreadable file edit now safely restores it from a backup rather than deleting it.
- **Scaffold:** Rust workspace (`core`, `llm`, `storage`), a Spin (`wasm32-wasip2`)
  backend, a Leptos WASM frontend shell, and CI running fmt, clippy, native
  tests, and both WASM builds.
- **Provider HTTP:** real Ollama and llama.cpp calls (`/api/models`,
  `/api/chat`) over Spin outbound HTTP, with a fake `HttpClient` for native
  tests and actionable errors for unreachable engines.
- **Chat sessions:** a sidebar of sessions (new/switch/rename/delete), SQLite
  message persistence, SSE token streaming, and a per-session system prompt
  and connection.
- **Container deployment:** a single-image multi-stage `Dockerfile`,
  `docker-compose.yml`, and Podman quadlet units, with SQLite on a named
  volume.
- **Workspace modes:** Remote mode (Spin-mounted host folder, `/api/files`)
  and Local mode (File System Access API, directory handle persisted in
  IndexedDB), plus Rider-style multi-project tabs.
- **Agentic coding loop:** tool-calling agent (`read_file`, `write_file`,
  `list_dir`, `search`) confined to the workspace, with turn/tool budgets, a
  permission handshake for gated tool calls, and server-side run
  cancellation.
- **IDE surface:** an in-browser syntax-highlighting code editor, a
  diff-first file viewer (inline diff, side-by-side diff, updated content,
  markdown/image preview), full-text file search, a per-session model
  picker, a Settings dialog (theme, default connection, default system
  prompt), and a system prompt manager.
- **Local user accounts:** registration and login, argon2id password
  hashing, HttpOnly cookie sessions, and user-scoped projects/sessions.
- **Custom dialogs & remote file browser:** themed confirmation and prompt
  dialogs, and a host file browser for picking a remote project folder,
  replacing browser-native `alert`/`confirm`/`prompt`.
- **Virtual File System (VFS):** a shared `Vfs` trait with `HostFsVfs`
  (remote) and `BrowserFsaVfs` (local) implementations, a universal
  `VfsToolExecutor` shared by both modes, a browser-driven local-mode agent
  loop against `/api/chat-tools`, workspace-wide `grep_search`, `search_web`,
  `fetch_web_page`, and zero-turn temporal context injection.
- **Process execution & WebSocket terminal bridge:** the native
  `openwebide-bridge` daemon (PTY sessions, `run_command` agent tool) and an
  integrated terminal pane, with Git operations forwarded to the bridge
  against the real host repository.
- **TUI-driven chat surface:** a terminal-native linear stream layout,
  collapsible `<think>` reasoning blocks, readline-style prompt history,
  an in-stream keyboard permission handshake, a slash-command engine
  (`/model`, `/tokens`, `/clear`, `/test`, `/diff`, `/help`, `/commit`,
  `/checkout`, `/branch`, `/sync`), and active-editor-context injection via
  a context pill and `Ctrl+L`/`Cmd+L`.
- **Git integration:** passive `.git/HEAD`/`.git/refs` status telemetry with
  a bridge-backed active engine for status, diff, branch, commit, checkout,
  and sync against the real host repository; a status bar branch/ahead-behind
  widget, file tree status badges, and `git_status`/`git_diff`/`git_commit`/
  `git_branch` agent tools.
- **Editor enhancements:** cursor/selection tracking (`EditorContext`) shared
  between the editor and chat prompt, diff viewing against Git HEAD, a
  markdown/image preview mode, and syntax highlighting expanded from 5 to 16
  languages.
- **TUI telemetry meters:** the statusline's `t/s` and `Ctx:` gauge are now
  backed by real provider usage. Ollama (`prompt_eval_count`, `eval_count`,
  `eval_duration`) and llama.cpp (`usage`, `timings`) report token counts and
  timing per call; missing fields fall back to an estimator and are marked
  `~` in the statusline and `/tokens`. Usage is forwarded as a `telemetry` SSE
  event, recorded into `SessionTelemetry`, persisted on the assistant
  message, and replayed on session reload.
- **Per-connection context limit:** an optional `context_limit` on each
  connection, resolved from the configured value, then provider discovery
  (`GET /api/models/context` — Ollama's `POST /api/show`, llama.cpp's
  `GET /props`), then a fallback. Ollama connections send it as
  `options.num_ctx` on every request so the gauge and the runtime context
  window agree; for llama.cpp the value drives the gauge display only,
  since its context size is fixed when `llama-server` starts.
- Bridge `hello` authentication with short-lived backend-minted tokens (`/api/bridge/token`), a `bridge_url` user setting, and optional `OPENWEBIDE_BRIDGE_TOKEN` pairing credentials.
- **Local mode integrity:** validate UTF-8 with a read-only fallback for unreadable files, surface missing directory permissions, and abort local runs without waiting for server round-trips.

### Changed

- Keep personal default/fast model choices in Settings; move shared model configuration to a separate dialog opened from Servers. Model profiles, context detection and compaction thresholds are shared per server/model, with existing preferences migrated.

- Share content/file search policy and budgets across browser, WASI and native filesystems, and share run planning and persisted agent events across browser, SSE and bridge runs.
- Route Git, terminal and agent startup through a common project execution-host facade. Model discovery uses shared probes on the backend or the local companion host.
- Normalize file paths and enforce a shared 10 MiB read limit across editor and agent adapters; browser filesystem failures retain typed error categories.


- Keep assistant thinking blocks collapsed while the model is thinking (click the header to expand the live trace), move the spinner after the "Thinking..." label with a live elapsed-time counter, and show the final elapsed time in the collapsed "Thought" summary; the spinner now cycles proper braille frames. Elapsed time scales with the run (e.g. `42s`, `2m05s`, `1h03m20s`), and aborting a turn clears the "Thinking..." state so the partial trace collapses into the summary.
- Raise the agent run budget from 12 turns / 24 tool calls to 256 turns / 512 tool calls; the budget is a runaway-loop guard (runs stay cancellable), so nontrivial tasks no longer exhaust it mid-task.
- Enforce selected pedantic Clippy lints across the workspace, remove unused code, and trim bridge Tokio features.
- Split bridge terminals, tool execution, and agent runs into modules; share an executor trait for host tools and add structured logs with `RUST_LOG` filtering.
- Refactor backend routing and typed errors; hide internal 500 details, surface project lookup failures, and accept raw binary file writes.
- Stop interrupts running commands, web requests, and searches while allowing file writes and Git mutations to finish.
- Sync prompt history and theme through user settings, import legacy history once, and fit panel widths to the viewport.
- Split frontend state into per-feature stores provided through Leptos context.
- Run local-mode command and git tools in the picked folder, discovered and verified through a temporary bridge probe.
- Persist interim tool calls for follow-up history and remember servers that reject streamed tool calls.
- Unify SSE and WebSocket chat events in one shared protocol; deploy frontend and backend together.
- Use the bridge for frontend chat and local completion streaming, with SSE fallback, run resume, and project availability notices.
- Share one authenticated bridge connection across terminal, run, and completion traffic; reconnect terminals automatically and open a fresh shell after a bridge restart.
- Stream server-side agent replies token by token, retain text before tool calls in conversation history, and add a run-plan API.
- Added session expiry handling (auto-logout) and statusline model switcher dropdown.
- **Bridge process lifecycle:** every command the bridge spawns now runs in its own process group, so timeout, request disconnect, Kill, and shutdown signal that group. Interactive shells may place background jobs in separate groups that survive shell cleanup. `Kill` sends the requested signal (`TERM`/`HUP`/`KILL`, default `KILL`) to the whole group; on a PTY, `INT` instead writes `^C` to the terminal like a real Ctrl+C. `/exec` output over ~1 MiB per stream now keeps the first 256 KiB and last 768 KiB with an omission marker instead of buffering unbounded output. Exited terminal/process sessions are now removed 30 minutes after they exit; running sessions are never reaped. The daemon now shuts down gracefully on Ctrl+C or `SIGTERM`, terminating every session's process group first.
- **Bridge HTTP resilience:** The execution bridge's HTTP and WebSocket server is now built on `hyper` instead of a hand-written parser, fixing header/body misreads under fragmented or chunked writes and case-sensitive `Upgrade` header matching. Requests are capped (16 KiB head, 1 MiB `/exec`/Git body, 16 MiB WebSocket message), idle sockets close after 10 s without a request head, WebSocket connections are pinged every 30 s and closed after 90 s without a reply, and the accept loop now backs off and keeps serving instead of exiting on the first accept error (e.g. EMFILE), bounded by 256 concurrent connections.
- Backend git operations now run within the project context, and bridge failures cleanly propagate rather than showing false success.
- **Backend resource bounds:** API request bodies are now capped per route (64 KiB auth, 1 MiB JSON, 4 MiB settings, 16 MiB chat, 10 MiB file write) and rejected with 413 Payload Too Large when exceeded. File search is bounded to 500 hits / 64 MiB / 20,000 entries, and the `include_ignored` flag is honored server-side.
- Database methods that update multiple interrelated tables (e.g. deleting a project, first-admin creation) now run within strict SQLite `BEGIN IMMEDIATE` ... `COMMIT` transactions on the backend. This guarantees complete rollback if an operation fails or if the WASM task cancels or panics midway, fixing race conditions and half-deleted states without relying on manual cascading deletion code or risking lock deadlocks.
- The user registration API endpoint (`POST /api/register`) now correctly isolates creation by atomically using the transaction, preventing race conditions where multiple parallel signups could occur on first setup.
- Constraint-violation errors are now reliably returned as HTTP 409 Conflict.
- **Direct Workspace Mounts:** Spin development servers and Docker deployments now require the `--direct-mounts --allow-transient-write` flags. This ensures file modifications write to the native host directory instead of a temporary Spin sandbox. Project creation validates paths, and the `.spin/` configuration directory is explicitly forbidden from file API and agent access.
- **Web fetch & redirect safety:** Hardened web fetching against SSRF by refusing requests to cloud metadata endpoints (`169.254.169.254`, `fd00:ec2::254`, their IPv4-mapped representations, and `metadata.google.internal`) on initial requests and redirect hops while keeping LAN and private access open. Hand-rolled RFC 3986 §5.2 redirect reference resolution to correctly handle absolute paths, network-path (`//`) references, relative paths, port preservation, and query-only redirects. Capped streaming HTTP response bodies directly at 512 KiB for web pages and 2 MiB for JSON to prevent memory exhaustion.
- **Bridge Git safety:** Protected native Git execution against command-line argument injection. Branch and remote names are validated (`git check-ref-format --branch`, remote membership) with option-like leading `-` and pathspecs like `.` rejected with 400 Bad Request. Branch switching now strictly uses `git switch` (requiring Git ≥ 2.23), commit operations with specified paths only stage and commit those paths, Git output is untrimmed, push/pull commit counts are calculated accurately via `rev-list`, and phantom `origin` branches from `origin/HEAD` symrefs are ignored.
- **Approval UX:** "Always approve" is now scoped per-session (cleared on logout) instead of globally, and explicitly never covers `run_command`. Keyboard approval shortcuts moved from bare `y`/`n`/`a` to `Alt+Y / Alt+N / Alt+A` (typed responses no longer trigger approvals). Stopping a run now cancels any pending tool prompt instead of leaving it clickable, fixing an issue where later runs' shortcuts acted on dead prompts.
- Enabled running unit tests for `openwebide-backend` natively via an `AppDb`
  abstraction backed by in-memory SQLite (`rusqlite`) on native targets and Spin
  SQLite (`SpinDb`) on WASM targets.
- Toolchain pinned to Rust 1.98.1; `argon2` 0.6, `rand` 0.10, `base64` 0.23,
  `hmac` 0.13, `sha2` 0.11, and `tokio-tungstenite` 0.30 (bridge only), with
  existing password hashes and bearer tokens verifying unchanged.
- `/tokens` now shows the context-window gauge as the *latest* call's token
  count against the resolved context limit, instead of a running total
  summed across the whole session; the Input/Output rows stay cumulative.
- The context-window fallback, used when a connection has no configured
  limit and provider discovery finds none, is `4,096` tokens (was a
  hard-coded `32,768`), and is marked estimated (`~`) in the UI.
- Deliver terminal output through a cursor-based SeqRing with ordered replay, retained exit events, truncation notices, and duplicate session-ID rejection.
- Apply numbered, idempotent database migrations transactionally via `PRAGMA user_version`; refuse databases newer than the build.
- Decode typed tool arguments and report malformed calls as tool errors; cap file, directory, and search tool results and support `include_ignored` in agent searches.
- Reconcile setup, architecture, bridge protocol, roadmap, changelog, and `/help` documentation with current behavior.

### Fixed

- Honor project and nested `.gitignore` rules in shell/Git checkpoints in local and remote projects, including negated patterns. Keep explicit file-edit snapshots for ignored files. Skip oversized or unreadable files without stopping the turn, persist coverage gaps, warn in tool results and rewind confirmation, and leave excluded paths untouched even when file sizes or ignore rules change.

- Delete nested remote files with writable directory descriptors and support recursive folder deletion without following symlinks, matching browser/native adapters.

- Recover recent local projects when browser folder access is missing: request permission or re-pick the original folder, starting at the saved handle when available and retaining the project and sessions.
- Defer oversized CLAUDE.md/AGENTS.md instructions with scoped read-file directions instead of injecting truncated fragments; retain imports beyond the inline limit.


- Route file-tree Git indicators, HEAD diffs, branch/sync controls, and Git slash commands through a shared project Git facade in both modes. Local projects use the verified local bridge instead of the remote-only project API; background refresh includes Git status and stale results are discarded.

- Keep editor syntax highlighting aligned during rapid scrolling and bottom-to-top scrolling by translating the highlight layer from the textarea’s offsets, matching scroll gutters, and showing only one text layer at a time.

- **Side-by-side diff alignment:** the detailed side-by-side diff is now built on the same line-level LCS as the inline diff, so an inserted or deleted line no longer shifts every line below it into a false pair (previously the whole changed middle rendered as insertions); unchanged lines stay aligned as context, and paired changed lines keep intra-line word highlighting and line-ending notes.

- Disable the agent chat composer until a project is open, with shortcuts to open a local or remote project.

- Accept llama.cpp base URLs with or without a trailing `/v1` for model discovery, chat completions, and context limits.

- Stop bridge agent runs when a tool result cannot be saved, delivering the completed result before the error and preventing overlapping runs in the same session.

- Keep split terminal controls intact when busy or recoverable-error notices appear during a running process.

- Preview SVG files with uppercase extensions.
- Preserve binary Git HEAD contents and hide Revert with a clear binary-file notice in text diffs.
- Isolate SSE run cancellation and permission cleanup so a quick resend preserves Stop and concurrent runs retain their decisions.
- Editor: Added confirmation dialogs before discarding unsaved edits and conditionally offered the Revert button only when HEAD is known.
- **Web fetch HTML→Markdown conversion:** the converter is now built on the `html5ever` tokenizer (same version `ammonia` already uses, so no second copy in the dependency tree), so fetched pages keep text the old hand-written scanner dropped — an unescaped `<` no longer swallows the rest of the line, bare `&` in text (`Q&A`, `AT&T`) survives, HTML5 entities are decoded fully, self-closing skip tags (`<svg/>`) no longer swallow everything after them, and `<head>` content (e.g. `<title>`) no longer leaks into the output. Links with unsafe `href` schemes (`javascript:`, `data:`, …) are now removed (link text is kept), relative/fragment links are kept, and `data-href` is no longer mistaken for `href`.
- **Line-ending and final-newline diffs:** the inline and side-by-side diffs now align lines with a line-level LCS that compares each line including its ending, so a CRLF→LF conversion or an added/removed trailing newline shows up as a change (with a small "⏎ CRLF → LF" / "no newline at end of file" note) instead of being invisible, and an inserted line no longer marks every line below it as changed. A whole-file line-ending change collapses to a single summary line.
- **Git status for paths with spaces, unicode, and unborn branches:** the bridge now reads `git status` with `--porcelain=v1 -b -z` (NUL-separated, unquoted paths) and the parser consumes rename/copy source records and recognizes the `No commits yet on` / `Initial commit on` / `HEAD (no branch)` headers, so files with spaces or non-ASCII names report the correct status and new repositories show their branch instead of failing to parse.
- **Stale search results no longer overwrite the view:** file-content searches are now guarded by a per-run generation, so a slow search for an older query can no longer land after a newer search for the same project and clobber its results, and clearing the search box can no longer be undone by a search that resolves after the clear.
- **Stale chat history no longer overwrites the view:** loading a session's message history is now guarded by a per-run generation, so a slow history request can no longer land after a newer request — including one for the same session — and clobber the conversation on screen. The first send in a brand-new chat also no longer duplicates the user message, since the redundant history fetch that fired the instant the session was created is now skipped.
- **Stale file-browser listings no longer overwrite the view:** the folder picker's directory listing is now guarded, so a slow browse response for an old directory can no longer land after a newer navigation and show the wrong listing.
- **\"New file\" no longer truncates an existing file:** creating a file at a path that already exists now returns an error (HTTP 409 in remote mode, an error banner in local mode) and leaves the file's content untouched. Agent `write_file` to a new local-mode path now works correctly.
- **Auth correctness:** the token-signing secret is now created with an atomic insert-if-absent, so concurrent first requests can no longer write different secrets and invalidate issued tokens, and token expiry checks fail closed with a 500 instead of accepting every token when the system clock is unavailable.
- **Docker project-path migration:** Remote project paths created before the `/workspace` mount (stored relative to the container root, e.g. `workspace/foo`) are rewritten automatically on first start, so projects from older Docker installs no longer appear missing after an upgrade.
- Plain-chat SSE streams always emit a terminal done/error event, including persistence failures.
- **Truncated replies are kept, not silently saved or dropped:** a reply cut short by a crashed or disconnected provider is now kept with a visible "[reply truncated]" marker instead of being silently saved as complete or dropped, and llama-server `error:` events now surface their message.
- Database foreign keys (`PRAGMA foreign_keys = ON`) are now correctly enabled when opening the Spin WASM SQLite database connection.
- Calling `delete_project` now relies natively on SQLite's cascading deletes, vastly simplifying the query footprint.
- **Frontend panics and terminal cleanup:** Fixed a panic in editor context capture when truncating selections lands inside a multi-byte character by mapping UTF-16 selection offsets to byte offsets and truncating at a UTF-8 char boundary. The terminal dock's WebSocket connection task is now cancelled and its spawned sessions killed when the dock closes, and its output auto-scroll no longer panics on an already-disposed DOM node. The status bar no longer panics on a stale Git status signal. Added a browser panic hook that logs to the console instead of showing an opaque `unreachable` error. Typed shell commands with arguments are now run via `sh -lc` instead of failing.
- **Provider stream robustness:** The Ollama/llama.cpp line-splitter now decodes UTF-8 across the whole buffered line instead of per network chunk, so a multi-byte character split across chunks no longer corrupts the reply. The buffer is capped at 16 MiB per line instead of growing without bound. A `tool_calls: []` response is now treated as a text reply instead of an empty tool-call turn that looped until the turn budget ran out.
- **Highlighter and diff robustness:** Fixed panics in the syntax highlighter on non-ASCII source lines and in HTML-to-Markdown truncation on multi-byte character boundaries. Lines over 10,000 bytes now render as a single unhighlighted token instead of freezing. Word-level diffs fall back to a whole-line delete/insert past a size budget instead of using unbounded `O(m·n)` memory.
- Approving one tool call could silently approve a later, different call
  (Ollama reuses `call_0` every turn), letting e.g. `run_command` run without a
  prompt. Tool calls now get session-unique ids and each approval is used once.
  This also stops later runs from overwriting earlier tool steps in history.
- Agent-directed file writes on the streaming write path no longer produce
  0-byte files.
- Remote file API path handling for nested and root-relative paths.
- Filesystem and outbound-denial error messages now point at the actionable
  fix (the host egress allowlist in `spin.toml`, or the remote mount
  configuration) instead of an opaque error.
- Duplicate projects sharing the same mode/path are deduplicated on
  startup, reassigning their sessions to the surviving project.
- Return CORS headers on API errors and reject extra path segments on session update/delete routes.
- Match file extensions without case sensitivity and use Markdown fences longer than any backtick run in injected editor context.

### Security

- **Bridge Security**: Added `Authorization: Bearer <SECRET>` for Origin-less `/exec` and `/git/*` routes. The bridge generates a 32-byte secret at startup and the backend auto-fetches it over loopback via `POST /secret`, with caching in SQLite.
- Hardened login system: transitioned to HttpOnly cookie sessions, added brute-force lockout, and implemented global logout (invalidates sessions on all devices). Added CSRF protection via custom header.
- Default-deny tool approval policy: agent tools now default to requiring user approval unless explicitly allow-listed as auto-approved (`read_file`, `list_dir`, `search`, `grep_search`, `git_status`, `git_diff`, `search_web`). `fetch_web_page` now requires user approval, and `write_file` rejects any path with a `.git` component (case-insensitive). Tool descriptions and summaries updated for `write_file` (showing line and byte counts) and `git_commit` (clarifying all tracked changes vs specific paths).
- Bridge exposure baseline: Enforced strict `Host` and `Origin` validation, rejecting foreign cross-origin browser requests and DNS rebinding. Removed wildcard `Access-Control-Allow-Origin: *` and restricted preflight responses. Enforced `application/json` Content-Type on browser POSTs (`POST /exec`, `/git/*`) to prevent CSRF, and added `--allowed-origin` and `--allowed-host` CLI/env configuration.
- `files/raw` non-image requests are now forced to download instead of rendering inline.
- Directly opened SVG/HTML files via `files/raw` are now sandboxed.
- Dropped support for `?token=` query parameter authentication. Media preview URLs now use short-lived blob URLs instead of exposing the bearer token.
- Markdown previews sanitize raw HTML with `ammonia` and no longer execute unsafe elements (e.g., `<script>`, `<style>`,
  `<iframe>`, `onload`/`onerror`, `javascript:` links). Safe elements like
  `<details>`, `<sub>`, and inline `<img>` remain. SVG previews display as
  images and no longer execute scripts. Added `openwebide-frontend` as a lib
  crate for a native test harness.
