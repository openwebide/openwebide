# Code intelligence checkpoint history

This is the chronological working record through the implementation checkpoint
`867fb766`. Statements about remaining work describe their recorded checkpoints.
The current behavior and limits are documented in [Code intelligence](../code-intelligence.md).
Original failures and measurement limits remain part of the record.

### Code intelligence: host language services, LSP and caching

Rust 0.3.0 declares `rust-analyzer` through API 9, with declared Cargo/toolchain
inputs and Cargo build output in the execution host's cache. The API 7
JavaScript/TypeScript plugin declares `typescript-language-server` for JS, JSX,
TS and TSX; its dialects share one service and synchronize unsaved open buffers.
C# 0.2.0 declares `csharp-ls` through API 8, with the .NET 10 SDK and
multi-project `.slnx` or standalone `.csproj` workspaces. Its public virtual-document
declaration supplies read-only framework definitions.
Python 0.2.0 declares Pyright through API 6, with project configuration and
virtual-environment import resolution on the execution host. Unsaved cross-file
types, diagnostics, completion, navigation and reviewed rename use the shared
service; Pyright does not provide formatting.
Go 0.3.0 declares gopls through API 9, with declared project inputs, external
build/module caches, single-module `go.mod` and
multi-module `go.work` projects, unsaved cross-module analysis, completion imports,
read-only standard-library definitions and reviewed rename/formatting in both modes.
C/C++ 0.2.0 declares clangd through API 7. C and C++ retain their language IDs
while sharing a service, compilation-database or `.clangd` configuration and
unsaved header overlays. Diagnostics, completion, navigation and reviewed
header/source rename and formatting work in both modes. The shared host refreshes
dependent documents after overlay changes without saving files or restarting
the service.
HTML, CSS/SCSS and JSON/JSONC 0.2.0 packages declare the corresponding VS Code
language servers through the public API 7 contract. Related dialects share their
server while each package remains independently installable. Local JSON schemas,
unsaved buffers, snippets, diagnostics and provider lifecycle use the same service
and editor paths in both modes.
Terraform 0.1.0 owns HCL parsing and editing rules in an independently installable
API 9 package built on the public toolkit. Seven Terraform/HCL dialects share
`terraform-ls`. Installed editor checks cover component-owned syntax, unsaved
cross-file types, navigation, completion, reviewed formatting and undo, diagnostics,
disable/re-enable and host restart in both modes without initializing infrastructure
([editor contract](https://github.com/openwebide/openwebide/blob/main/tools/check-terraform-server-client.py)).
The shared host facade supports diagnostics, hover panels and pointer information,
manual and while-typing completion, definitions, references, document symbols,
rename, document formatting and text-edit code actions in both project modes. Clients validate source-bound
results, review edits and stage changes through existing editor history and
recovery. Native services retain warm state with bounded service counts,
source buffers, requests and idle lifetime; unsaved clients stay isolated.
Completion popups support keyboard and pointer selection, single-transaction
insertion with imports, and normal Tab focus navigation. Automatic requests are
debounced and coalesced; source, selection and provider changes reject stale
results. Pointer information preserves the caret and works in read-only files.
Dismissing completion preserves new-source refreshes and periodic diagnostics.
Completion snippets, selection formatting, type definitions, implementations,
filtered outlines, workspace-symbol search and diagnostic traversal use the
shared editor paths. Independently enabled analysis providers label their own
problems and fixes; failures preserve ordinary editing. Production browser checks
cover these workflows and provider lifecycle in both modes.

Remaining work:

- Finish durable phase authority for managed native operations, participating
  native/backend/agent filesystem paths, multi-process/profile ownership,
  durable outcomes and frozen-owner recovery. Keep ordinary browser-only Local
  saves available without the companion; these saves are outside the physical
  folder locking guarantee. The installed macOS Chrome PWA checks now cover native
  moves and reviewed commands, permission revocation/regrant, automatic remote
  rollback and manual Local rollback after an unacknowledged move before editor
  commit. Original inode/hard link/mode/ACL/extended attributes/CRLF bytes and
  unsaved Unicode drafts survive recovery. Pending commands retain their recovery
  through permission loss and require manual acknowledgement for uncertain effects
  ([recorded cases and checkpoint limits](physical-folder-checks.json)).
  These device cases do not establish the remaining mutation authority guarantees.
  Native admission now checks captured root and ancestor creation-time observations
  using the shared resource identity
  ([focused native checks](native-folder-identity-checks.json)).
  Both native journal transports now share private recovery credential issuance
  bound to the original journal and a separate persistent backend namespace.
  Worker release, transport-secret rotation and a closed/reopened backend database
  preserve that credential
  ([focused issuer checks](private-recovery-authority-checks.json)).
  Their private scope now observes original file and command recovery together
  without loading snapshots or provider payloads; stale authorization and missing
  activity are rejected in both transports. This observation does not grant a lease.
  Compact private command recovery now retains the original invocation, revision,
  phase and outcome fingerprint without reading provider/source payloads. Both
  transports use the original issuer and reject stale or incomplete replies;
  readable archived records gain their binding once, while unreadable legacy
  records remain pending
  ([command metadata checks](command-recovery-metadata-checks.json)).
  The backend now keeps command-worker claims and settlement independent of the
  file worker. Both native transports share original identity and outcome
  validation; completed commands retain writer/deletion protection until cleanup
  and settlement are acknowledged. Upgrade/replay and callback contracts pass in
  both modes ([command worker checks](command-worker-checks.json)).
  The shared native producer now resumes the original permit, reserves result
  storage before command admission, records one dispatch boundary and publishes
  outcomes/settlement under the complete folder lease after browser loss. Both
  modes execute once, preserve warm services and keep no-dispatch rejections
  fenced through manual acknowledgement
  ([live producer checks](native-command-producer-checks.json)).
  Status returns stored outcomes without the original host, including uncertain
  results. Stop can return the stored result after positive native cleanup;
  otherwise it still requires the original host. Both adapters use the same
  backend authorization decision and retain account/context checks.
  Bounded recovery snapshots and outcome acknowledgements now use preserved
  command metadata without loading the archived provider or source payload.
  Native controls resolve the original retained session; recovery does not load
  installed providers. Reload keeps completed/acknowledged commands visible and
  saves paused until native settlement clears.
  Complete bridge HTTP command cases now use real storage approvals, native file
  admission and paired folder/command delegation in an isolated test namespace.
  Join, stop-before-start, Git branch/round-trip and post-cache outcomes retain
  their original assertions
  ([command HTTP checks](command-http-checks.json)).
  Attached-command result space and the original private worker are now reserved
  atomically with folder ownership before first file admission. Capacity failure
  publishes neither a new folder owner nor a command intent. Only that worker can
  promote a reservation after file recovery is idle; existing execution intents
  remain single-use
  ([reservation checks](command-reservation-checks.json)).
  Backend file admission also retains the original command worker atomically,
  without permission to dispatch. Start still checks fresh manual authority;
  paired callbacks for the reserved worker can publish cleanup after Start denial.
  Legacy file admissions and intents can still recover after provider revocation;
  the new reservation requirement applies to first admission
  ([backend reservation checks](backend-command-reservation-checks.json)).
  Native owners now retain their original private callback credential and origin,
  plus a positive file-completion receipt before file credentials are released.
  A recovery-only backend channel checks that issuer and the original command
  worker; it cannot start a command. Early Stop can publish a no-dispatch rejection
  and settle after acknowledgement without the original LSP session. All ten
  HTTP lifecycle cases now verify backend settlement and a subsequent ordinary
  native writer, including a fresh native host using the same private namespace.
  Legacy owners retain unknown file effects. Local namespace lock attempts now
  queue behind waiting publishers within the existing bounded wait
  ([original cleanup checks](original-command-cleanup-checks.json)).
  Recorded native outcomes now resume through the original callback at startup
  and on controls, with bounded, deduplicated recovery ownership. Known results
  remain readable during callback outages or folder replacement; cleanup waits
  for the original folder and issuer. Eight real process cases kill the recorded
  result's owner with SIGKILL, then verify successor recovery in both modes,
  including a settlement committed before its HTTP reply is lost
  ([recorded restart checks](recorded-command-restart-checks.json)).
  Settlement retries now check the original native record independently of callback
  context, retry later folder loss and stop after another process retires that record.
  Both-mode contracts cover no-origin owners, a newer owner retaining exclusion,
  later folder substitution and two real publishers joined across a stale reply
  ([peer settlement checks](settlement-peer-recovery-checks.json)).
  Ten deterministic both-mode cases now verify actual registered jobs fall from
  one to zero after peer retirement with the original folder absent: a retained
  no-dispatch result, admitted/unadmitted cancellation and real received completed
  and uncertain results. Dispatched commands execute exactly once. Uncertain
  results retain manual review; completed commands preserve their warm service
  until host shutdown. Removing the early original-presence check reproduces the
  live-job leak; the full affected command HTTP suite passes with it restored
  ([retired worker checks](retired-command-worker-checks.json)).
  Cancelled native reservations now persist an outcome-free receipt and retire only
  after exact original cleanup and settlement. The shared facade sends recovery
  controls without fresh provider authority; bounded native observations and owned
  retries continue independently of browser lifetime. Reservations whose first
  backend admission never committed require positive original no-worker/file proof;
  unknown or unfinished effects remain protected. Both modes pass twenty new HTTP
  cases, including eight process kills, revoked providers, folder substitution and
  lost cleanup/final replies. No command is replayed
  ([native cancellation checks](native-cancellation-checks.json)).
  Committed-edit command discard and outcome acknowledgement now share native
  Stop/cleanup policy above both adapters, without acquiring fresh provider grants.
  Already-recorded backend rejections retain their exact original result through
  bounded authenticated reads; adoption requires positive no-dispatch and completed
  file proof under the original folder lease. Warm and retired controls keep the
  original Rejected or Cancelled decision. Twenty discard/control cases and two
  terminal-cache cases pass in both modes, including six more process kills
  ([committed discard checks](committed-discard-checks.json)).
  Original process watchers now persist managed-group cleanup receipts before
  releasing their complete folder descriptors. Command results precede watcher
  release, preserving exact outcomes through an unacknowledged handoff. A successor
  with the original issuer and folder can recover a dispatched command as uncertain
  after its owner is killed, without replay, and retains fencing through manual
  acknowledgement and settlement. Both modes pass the real dispatch/process-loss
  contract plus all prior command HTTP cases
  ([watcher receipt checks](watcher-receipt-checks.json)).
  Prepared native intents now recover through original Stop after owner loss
  before backend Claim or after its commit but before ACK. Both modes pass all four
  real process-loss cases with no RPC, unchanged source/editor recovery, late-worker
  fencing, manual acknowledgement and successful ordinary writing afterwards
  ([prepared command checks](prepared-command-checks.json)).
  Received completed and uncertain results now pass sixteen real owner-loss cases
  across both adapters, including watcher handoff, original namespace/folder loss,
  lost final settlement replies and concurrent recovery hosts. Exact results,
  physical fencing, manual review and ordinary writer recovery are retained without
  replay ([received result checks](received-command-result-checks.json)).
  Finish remaining admission/watcher enrollment and outcome/context interruption windows.
  Original final settlements now survive public history pruning in bounded private
  receipts. A durable native metadata acknowledgement queue retires independently
  of folder access and ordinary writer admission, including a second process loss
  after the backend disposes its receipt. Upgrade/replay, capacity and authenticated
  callback checks pass in both modes
  ([pruned history checks](pruned-command-history-checks.json)).
  Configured backend address changes now reconnect original command callbacks
  only after an authenticated private scope reply. Both adapters retain the
  verified move through process loss and missing folder paths, without exposing
  the original recovery proofs or changing command authority
  ([configured callback checks](configured-command-callback-checks.json)).
  The current browser connection now supplies a scoped candidate through the
  shared project host without requiring folder access. Running native publishers
  follow a verified move; both transports reject replacement issuers and replayed
  replies, and metadata-only queues reconnect after pruning and a lost final ACK
  ([browser callback checks](browser-command-callback-checks.json)).
  Built-browser checks now pass the real WASI backend/paired transports and
  recovery UI after a backend address move with the native process unchanged.
  Original Stop/manual acknowledgement and ordinary writing recover in both
  modes; the new Local origin has no folder handle and no command is replayed
  ([built app checks](built-command-callback-checks.json)).
  The complete built resource/command workflow now passes in both modes with
  persistent volume identities, asynchronous settlement refresh, independent
  command/file callback persistence
  and Local folder regrant through the existing physical proof while ordinary
  commands remain fenced. It verifies native restart, rollback, lost dispatch,
  same-window Stop, draft persistence and command non-replay
  ([current checks](command-settlement-checks.json)).
  Persistent macOS volume identities now cover native ownership, resource areas,
  creation receipts and workspace roots. Native library tests and process-separated
  APFS remount recovery pass for both mode scopes
  ([native volume checks](native-volume-checks.json)).
  Read-only `recovery upgrade inspect` now validates original objects, committed
  bytes, known schema and empty record/history tables through the OS-selected
  CLI, including the real account. Reviewed publication, resumption and
  cancellation now preserve original objects across process loss and actual APFS
  remounts. Backup-first activation now fences legacy writers before the visible
  upgrade intent; pre-marker resume and cancellation pass changed-device remount
  checks. Cancellation also preserves an old prepared journal written after its
  fence is removed. A reviewed upgrade of the actual empty OS-account registry
  now restores ordinary admission, retains the exact original objects and bytes,
  and accepts an idempotent reviewed retry
  ([upgrade inspection checks](native-upgrade-checks.json)).
  Qualify optimized native/WASI distributions, installed apps and physical
  permissions; frozen-owner administration and escaped groups remain open.
  The native OS-owner recovery CLI now inspects and explicitly retires reviewed
  original records without the old issuer. It requires the original folder gate
  and positive command cleanup, retains an audit and permanently fences old
  transaction credentials. A later normal command can settle while preserving the
  audit and new acknowledgement metadata
  ([native CLI checks](native-recovery-administration-checks.json)).
  Fresh Linux containers now pass the actual OS-default CLI, late-original
  fencing and ordinary HTTP writing, plus both-mode native metadata contracts
  ([Linux CLI checks](linux-recovery-administration-checks.json)).
  Native HTTP/SQLite checks now cover a genuinely dispatched command whose
  project is deleted after settlement but before native acknowledgement. Both
  adapters retain the original result and require explicit retirement before
  ordinary writing resumes
  ([deleted-project checks](deleted-project-command-checks.json)).
  Built-browser/WASI checks now cover real command approval, a lost committed
  settlement reply and authenticated project deletion. A new native host observes
  the original callback's 404 and keeps the fence; explicit retirement preserves
  the result and permits an actual file write after another restart in both modes
  ([built-app deletion checks](built-deleted-project-checks.json)).
  Reloaded pages now remain active through deletion and native restarts in both
  modes. The companion keeps its address; complete file snapshots stay exact
  through recovery, followed by the requested ordinary write
  ([reload checks](browser-reload-deletion-checks.json)).
  The original approval page now also stays active without reload through the
  same deletion/recovery sequence in both modes, with all seven file observations
  exact ([original-page checks](original-page-deletion-checks.json)).
  Account decommissioning now has the same two-mode recovery evidence: after
  actual API project deletion, a guarded administrative SQLite removal enforces
  foreign keys and rejects the old login. Native ownership retains the original
  context/result until explicit recovery; all eight file observations stay exact
  ([account checks](account-decommission-checks.json)).
  The normal Linux companion and OS-default CLI now pass the same application/
  account-removal sequence in both modes, using the shipped root-owned policy
  and retaining exact files, original results and the audit through fresh
  containers ([normal CLI app checks](normal-cli-application-checks.json)).
  Finish remaining command/process fault checks, optimized distributions,
  native Windows and complete CI
  ([operator workflow](../execution-bridge.md#native-recovery-after-an-issuer-is-decommissioned)).
  Native reviewed workers now check private recovery-state continuity in the
  OS-selected private directory under their complete folder gate. Original database,
  directory and external identity-file observations reject missing or identical
  replacement copies before admission and during execution. Shared filesystem/Git
  filters protect their names and ordinary native VFS/execution roots reject aliases
  into private runtime folders
  ([namespace checkpoint](native-recovery-namespace-checks.json)).
  The shared native gate also persists original transaction owners before the
  complete worker's backend claim. Overlapping ordinary native writers remain
  fenced across process/profile changes; only the original private issuer and
  physical folder can resume recovery. File completion clears a file-only owner
  only after an idle compound observation and acknowledged worker release.
  Attached-command owners remain pending until original command cleanup and settlement
  ([owner checks](native-recovery-owner-checks.json)).
  Private state reads also reject FIFOs without waiting for a peer. Container
  images select the existing data volume through root-owned per-UID installation
  policy, keeping SSH/tool home paths and browser-only Local saves available.
  Fresh Linux runtime-state containers retain the same original namespace objects,
  reject missing/copied databases and accept explicit restoration of the same
  original database. Namespace and both-adapter admission contracts also pass on
  Linux ([installation checkpoint](native-recovery-installation-checks.json)).
  Revisioned SQLite publication and recovery now preserve the original database
  inode across process loss, with an external seal preventing a missing update
  record from becoming fresh state
  ([persistence checkpoint](native-recovery-persistence-checks.json)).
  Per-file companion calls now share durable admission and original private
  scope validation. Closing revoked access retains protection until positive
  original recovery. Complete compound command phase authority, a Windows
  namespace adapter and full packaged-app verification.
  Native agent/child-task and plugin-management file writes now share the live
  folder gate with reviewed operations and commands. Accepted I/O survives
  observer loss; queued cancellation and changed-root rejection pass the native
  contracts ([writer checks](native-file-writer-checks.json)).
  Native file mutations now retain their lease inside the actual blocking
  operation through runtime shutdown. Version-1 transaction files reuse the
  original outer lease; raw atomic move/replacement calls remain non-yielding.
  A real blocked-write regression verifies retention until I/O finishes
  ([runtime ownership checks](native-file-runtime-checks.json)).
  Ordinary native mutations now use the retained original directory capability
  through one shared mutation policy, including atomic replacement and existing
  descriptor-relative moves. A real root-replacement regression verifies all
  eight mutation cases against the original object while leaving the lookalike
  untouched; alias and entry-kind contracts remain available
  ([directory binding checks](native-file-root-checks.json)).
  Backend API and SSE-agent file mutations now use that same native policy by
  default, including a distinct project alias after backend settlement while
  original native recovery remains pending. Both built-app modes preserve the
  fence through project/account deletion and restore actual writing after explicit
  recovery. Explicit companion-disabled deployments retain directory-bound WASI
  saves outside the physical-lock guarantee; native failures never fall back
  ([backend writer checks](backend-writer-checks.json)).
  The same directory-binding, admitted-I/O lifetime, ACL/xattr and authenticated
  writer contracts now also pass on Linux (48 filesystem and two HTTP cases on
  a persistent Linux volume). These are focused development-profile contracts,
  not a complete rebuilt distribution. Finish native Windows runtime verification
  and full packaged-app checks. Ordinary saves, commands, Git operations and
  proofs now share admission while reviewed resource/command work remains
  exclusive. Waiting reviews retain priority, accepted I/O owns its lifetime and
  uncertain cleanup quarantines overlapping work across original directory
  descriptors, independent coordinators and processes. macOS/Linux contracts,
  actual background-save compatibility, namespace upgrade/retirement and both
  built-app transports pass ([admission checks](ordinary-admission-checks.json)).
  Linux process watchers now adopt and reap their original managed descendants,
  releasing reviewed exclusion after acknowledged background work even when the
  daemon is PID 1 without a container init process. The normal daemon passes with
  and without init; all 59 focused native contracts pass. Complete combined-source
  verification remains open.
  Durable authority across lost owners and participating mutation paths remains
  outstanding.
  Companion observers now have deadlines without cancelling accepted native work.
  Expired and abandoned folder-proof cleanup now uses bounded registered workers
  and the original physical folder gate, preserving changed proofs/folders and
  safely declining cleanup during shutdown, runtime loss or capacity pressure
  ([proof cleanup checkpoint](folder-proof-cleanup-checks.json)).
  Shared runtime-name filters now protect Git views, staging, commits and new stashes;
  restore, branch and pull checks reject directory collisions and legacy runtime
  entries. Browser saves, file-tree operations, pending-edit resolution, rewinds
  and Git writes now check fresh file/command journals under the same project
  browser lock as resource execution. Queued Git callbacks retain their original
  request scope; draft guards run again after waits. Browser dispatch checks now
  use one compact, owned database observation of file and command recovery;
  direct file API mutations reject pending recovery before dispatch. Native Git
  and foreground command workers now share physical folder leases within each
  bridge process with resource execution and paired calls, including nested roots
  and worktree scope.
  On macOS/Linux, the shared gate now also acquires bounded OS directory locks
  across live bridge processes, with shared ancestor locks, physical alias checks
  and cancellation that retains no partial lock set while waiting. Real process
  tests include suspended owners, quarantine, replacement and shutdown
  ([coordination checkpoint](folder-coordination-checks.json)).
  On macOS/Linux, configured foreground and agent commands now dispatch under
  independent process watchers that receive the original directory locks before
  spawn. The child enters the pinned directory; cancellation and bridge loss retain
  exclusion through original-group cleanup. Acknowledged background work retains
  its watcher and folder claim until that group ends. Unconfirmed cleanup blocks
  overlapping work. This [managed-group checkpoint](command-process-group-checks.json) does not cover escaped groups,
  PTY/Git lifetimes or native Windows ownership; durable actor ownership and complete
  writer coverage remain unfinished.
  Native resource workers now obtain bounded owned recovery scope metadata and
  queue physical folders before taking the large-snapshot slot; full journals
  and folder identity are checked again after waiting. Database-generated record
  identities distinguish a replacement even when its public ID and revision repeat.
  Preparation, phase updates, execution acknowledgements and lost-reply recovery
  now retain that identity in both modes; queued browser/native work rejects a
  replacement before mutation or editor publication
  ([identity checkpoint](journal-identity-checks.json)). This
  identity fence does not establish exclusive worker authority.
  Remote resource workers now obtain durable private worker credentials after
  acquiring the complete native folder gate. Phase updates and release bind the
  journal generation and current worker; admission compares the full compact
  scope, including the previous owner. Released worker IDs remain as tombstones,
  native callbacks without admission are rejected, and database reopen does not
  expire ownership ([worker checkpoint](worker-ownership-checks.json)).
  Scoped Local journal delegation and its bounded native HTTP adapter now reuse
  the same worker admission and cursor policy without deployment credentials. A
  real SQLite/native-folder contract applies the shared resource interpreter on
  macOS and Linux ([transport checkpoint](local-delegation-checks.json)).
  Proven Local folder sessions now mint private workers, admit account-scoped
  grants and proxy cursor writes while authenticated browser recovery reads stay
  at the backend. Every mutable native call and cached acknowledgement checks
  current ownership under its physical folder gate. The shared interpreter
  recovers lost file/cursor replies through a successor bridge; production
  resource workflows pass in both modes
  ([session checkpoint](local-session-checks.json)).
  Untouched Prepared journals now cancel after admission through an account-owned
  metadata CAS without folder access. Revoked credentials retain worker identity
  fences and leave accepted native gates intact. Native mutation calls also
  require a durable execution phase; normal rollback stays on its existing
  authorization path ([cancellation checkpoint](prepared-cancellation-checks.json)).
  Compound phase authority and writer coverage across physical aliases, accounts
  and profiles remain unfinished.
  Accepted Git work and command cancellation cleanup retain their lease after
  disconnect; queued cancelled commands stop before dispatch. Approved LSP commands
  now share these leases, recheck their context after queued waits and wait for
  original-service cleanup before releasing folder access. Linux/macOS services now
  run under independent process watchers, using the same bridge executable and
  unchanged server stdio. Approved commands loan their physical directory locks
  to the watcher, which retains the original managed group identity and locks
  through lost observers and confirmed group cleanup. Warm services survive
  acknowledged command completion; native memory sampling includes watchers
  ([process-group checkpoint](lsp-process-group-checks.json)).
  Escaped groups and remaining command/filesystem writers still need the complete
  authority protocol. Unconfirmed cleanup rejects overlapping native work;
  physical directory locks also exclude other bridge processes while their
  owning host or watcher retains them. Scheduled, plugin
  and interactive bridge runs share configured services. Project deletion retains
  pending command recovery after the linked file journal commits. Final combined-source
  verification remains open.
- Complete production reload/device-handoff verification and resource measurements.
  Matching idle processes now transfer under the service-acquisition lock, closing
  the previous client's live documents before restoring the incoming client's
  separate, bounded source inventory. Active requests and unexpired command reviews
  keep their original service. Fresh service identities and increasing LSP versions
  invalidate earlier result handles; cancellation and changed paths preserve dirty
  sources for retry or explicit close. Native contracts cover both authenticated
  transports. Closing a retained file starts no process, and claimed receipts
  survive service removal without retaining its process or documents. Measure
  complete retained host/native/WASM memory and broader pressure behavior before
  declaring this complete. Production TypeScript
  measurements now cover 128/512-module workspaces, eight reloads and sixteen
  alternating-client queries per mode, with separate unsaved overlays and sampled
  native RSS. A larger Local baseline retained two services after overlap. Idle
  consolidation now releases redundant compatible processes while retaining both
  source inventories and command pins. Both production adapters now pass the same
  workloads again with an explicit single-idle-server convergence check; pressure
  behavior, broader resource measurements and prefetch remain open. The same
  production workflow also passes in the complete Linux arm64 app under verified
  2 CPU / 2 GiB / no-swap limits, with 1.07/1.19 GiB cgroup peaks for 128/512 modules
  and no OOM events. Additional real cgroup reserve-pressure checks observed the
  original idle server trees leaving the bridge's descendant inventory and both
  clients' distinct drafts returning on replacement processes in both modes.
  In-flight analysis/command pressure behavior and complete
  retained-memory stability remain open
  ([measurements and limits](service-measurements.md)).
  The current-runtime rerun includes process owners and checks original PIDs
  against the whole container at eviction and after both clients recover. All
  64 reloads and 128 alternating queries passed across native macOS and the
  constrained Linux app. Historical descendant-only observations remain separate.
  Calibrated browser probes now separately record retained V8 heap, live unshared
  WASM linear capacities and natural process RSS. Repeated retained menus build
  their owned controls once, dispose subscriptions on unmount and reject stale
  detached clicks. Full finite query series distinguish bounded DOM warmup from
  remaining small JavaScript heap trends; complete native/WASM allocator stability
  and active-work pressure remain open
  ([retained measurements](retained-memory-measurements.json)).
- Finish persistent derived-cache adoption, active disk-growth bounds and resource measurements.
  Public API 9 cache declarations now bind native environment, argument and settings
  paths outside the opened project through both authenticated adapters. Namespaces
  include account/project/physical folder, provider/configuration, executable and
  reported server version plus declared disk inputs. Confirmed process cleanup
  releases native leases; unconfirmed owners retain their physical directory.
  Allocation and confirmed retirement enforce a count/byte retention budget,
  reclaiming idle owned data only. Periodic bounded inventories now stop owned
  producers after sampled growth exceeds that budget, reclaiming idle data first
  and releasing leases after confirmed cleanup while preserving unsaved sources.
  Shared namespaces count once; failed inventories stop local producers. Measure
  sampling overhead and overshoot, and finish cross-process enforcement and hard
  active-growth bounds; sampled stopping is not an OS disk quota.
  Real Rust and Go probes retain derived files across process restart outside the
  project, and Cargo reuses unchanged build-script output. Tiny cold/restart samples
  show mixed latency rather than an established speed improvement. Complete
  representative hit/resource measurements, C# build-output compatibility and supported
  overrides for other servers; do not remove clangd background indexing to force
  a relocation its installed version does not support. Measure cold/warm latency,
  cache hits and resource cost before adding OpenWebIDE result caches. Installation
  settings and user preferences remain in the database.
- Complete reusable-result freshness and production
  resource/benefit measurements for optional language prefetch.
  The shared facade and native host now accept optional diagnostics/symbol queries
  through ordinary source and authority checks. One speculative request per host
  includes discovery, requires the client's own warm service and yields to interactive
  work within a two-second budget. Interrupted speculation stays suspended on its
  original process through handoff until restart, without disabling interactive
  queries. Bounded source metadata inventories now reject speculation spanning
  associated-source, directory or alias changes; failed collections publish no
  partial generation. Completed analysis rejected for changed sources can retry
  on the same service. A default-off account preference now enables shared editor
  scheduling in both modes: 750 ms idle delay, at most four files,
  256 KiB per file, 512 KiB per batch and eight diagnostics/symbol attempts per
  revision/service. Current open buffers take priority; known related unopened
  files and recently closed files use remaining capacity. Retained unsaved text
  overrides fresh bounded disk reads; failed reads consume the reserved budget.
  Owned cleanup retires temporary overlays before releasing their source bank or
  stops the original service, preserving other drafts. Shared request accounting
  reserves room for new foreground documents while retaining stale-request fences.
  Installed public-plugin checks cover both production editors and the actual
  authenticated bounded-file endpoint; Local uses OPFS correspondence.
  Source/scope changes, foreground work, composition,
  recovery, unmount and preference changes cancel pending work. Replies are
  discarded rather than cached or published. Candidate hints do not recursively
  discover a dependency graph. A same-build native 128-module preference comparison
  of the earlier open-buffer scheduler found nearly unchanged median symbol latency
  and higher Remote p95 with warming on. In the combined 512-module workflow,
  Local optional watches reached the shared descriptor budget while foreground
  work continued; that historical combined comparison remains incomplete.
  Separate isolated 512-module Remote and Local ABBA runs now pass in the complete
  Linux arm64 app under verified 2 CPU / 2 GiB / no-swap limits. Each enabled arm
  records 24 successful matching warming replies. Observed mean whole-cgroup CPU
  cost increased about 4% in each mode; latency was mixed, including slower Local
  symbol observations. Two fresh runs per arm/mode do not establish a general
  benefit, and retained-memory/allocator stability remains open. Keep prefetch
  default-off while completing broader resource and benefit measurements
  ([finite measurements and limits](idle-prefetch-measurements.json)).
  Context-sensitive completion is excluded from speculation.
- Key reusable results by account/project/host, source, effective configuration,
  dependencies and plugin/server version. Verify external edits, branch changes,
  dependency/configuration changes, updates and host restarts invalidate affected
  results. Cross-file analysis needs a project-generation contract as well as
  document revisions. Git revision checks now reject analysis and command dispatch
  spanning branch/commit changes, and observed revisions clear stale editor results.
  Native services now also retain bounded Git metadata generations through the
  same atomic publisher as source observations. HEAD/symbolic-ref chains, selected
  reference namespaces, packed refs, linked-worktree pointers/configuration and
  reftable table lists/namespaces reject branch/reference round trips around startup, analysis
  and command dispatch. Index refreshes preserve the service. Required metadata
  has reserved worker/notification capacity so optional source scans cannot
  exhaust it. The [checkpoint](git-metadata-checks.json) records
  macOS/Linux contracts and selected native Windows Git metadata and
  source-generation checks.
  Complete unobserved Git changes and apply-time freshness remain open.
  API 9 declared configuration/dependency inputs provide bounded disk snapshots
  checked around native requests, rejecting analysis spanning matching changes in
  both transports. Native input observations also retain a monotonic revision for
  metadata and identity changes, rejecting restored contents across startup,
  analysis, handoff and queued command dispatch. Complete inventories publish
  atomically, leaving failed observations uncommitted. These sampled observations
  do not replace filesystem-event generations or detect every unobserved change.
  Reviewed commands prepare before file application and retain
  their exact service through action-specific synchronization; durable discard
  releases unclaimed reviews. Declared executables and observed startup images now
  have identity/modification checks around native requests and command dispatch;
  replaced direct, parent-directory and intermediate launch links also reject
  old services even if their final target is unchanged. Command dispatch rechecks
  native context after affected-file verification. Optional prefetch now collects
  bounded metadata inventories of associated sources, declared inputs, directories
  and links through both native routes, including aliases and restored-content
  Unix cases. macOS and Linux additionally retain bounded, explicit object watches and
  recheck inventories after registration changes. Lost coverage advances the
  generation; failed setup publishes no partial inventory. Linux preserves shared
  hard-link registrations and rejects watch loss or queue overflow. Windows now
  implements nonrecursive directory notifications with owned pending I/O, unique
  completion events, one retirement worker and shared handle/buffer budgets.
  Native Windows CI now verifies parent notification filtering, directory metadata
  changes and source-generation/retirement contracts
  ([checkpoint](windows-source-event-checks.json)). Native intelligence
  protocol 2 now returns bounded, result-bound analysis contexts and validates them
  against the original live service through both authenticated adapters, without
  an LSP request or replacement. Proofs bind source generations and overlay epochs;
  changed results, expired proofs and changed native context reject. Incomplete
  observations preserve read-only replies without a proof and reject edit-producing
  queries. Schema 65 preserves archived version-1 command-review recovery while
  fencing older builds ([checkpoint](analysis-context-checks.json)).
  Formatting and resolved completion now retain immutable results and their selected
  service, validate before editor publication and reject late scope/source changes
  in both modes. Popup retry, snippets and unsaved undo pass the browser contracts
  ([checkpoint](editor-application-checks.json)). Workspace
  reviews using normalized `files` now retain the original analysis, bind approval
  to the displayed review and validate before atomic draft/history publication.
  Both-mode contracts cover tampering, rejection/retry, undo, stale approvals and
  late draft changes ([checkpoint](workspace-text-application-checks.json)).
  Ordered `changes` and command-bearing reviews now retain original analysis,
  reject changed commands/edit-only downgrades and validate before transaction
  creation. Shared-policy fixtures cover displayed-review identity, rejection/retry,
  no intent while validation is pending and intrinsic read-only changes in both
  filesystem modes ([checkpoint](resource-review-analysis-checks.json)).
  Local folder proof now precedes original action discovery/rename; the same
  capability survives review and execution, with recovery handles retained only
  for actual resource previews. Installed resource/command workflows, interrupted
  recovery, command non-replay and declared-input rejection/acceptance pass in
  both modes. Traces verify original-service validation and Local session reuse
  ([checkpoint](review-capability-checks.json)). Finish
  remaining editor interleaving coverage.
  Schema 66 now stores optional `LanguageReview` evidence bound to the exact plan
  and editor patch, with immutable retry identity, quota accounting, old-record
  recovery and migration replay checks in both modes
  ([checkpoint](language-review-storage-checks.json)). The native
  verifier now authenticates the exact original service/request/result under its
  folder lease without replaying the LSP query, preserves the ordinary pending-command
  fence and pins overlays. It reproduces the plan and complete editor patch through
  the same planner used by frontend review. Both authority profiles and both-mode
  shared replay contracts pass
  ([checkpoint](native-review-checks.json)). Initial worker CAS
  now checks fresh manual authority, current provider/configuration and complete
  command approval, then atomically stores the accepted full-review digest.
  Schema 67 reserves receipt bytes and preserves acceptance across migration
  replay/reopen. Remote recovery restores an ambiguous native move without the
  original service or expired grant and never replays its command. Both modes
  share the database acceptance policy, including paired delegation
  ([checkpoint](review-admission-checks.json)). Local and Remote
  now share complete native execution above thin journal/root adapters. Local
  retains the original proven session and whole-operation physical gate through
  durable release. Both-mode contracts cover observer loss, competing access and
  stale queued recovery before full snapshot load; a real Local LSP/HMAC fixture
  verifies original admission and accepted committed retry on a successor session.
  Fresh installed resource/command and declared-input workflows pass in both
  modes, with one Local Execute and no browser cursor/mutation loop
  ([checkpoint](paired-worker-checks.json)). The editor now
  persists `LanguageReview` from the retained original result and package revision,
  then obtains fresh sessionless manual authority before initial execution. Both
  installed modes verify exact native evidence, one Execute, interruption/restart
  recovery, access loss/regrant, Prepared cancellation and command non-replay
  ([retained review checkpoint](retained-review-execution-checks.json)).
  Installed macOS Chrome PWA checks now also cover retained file and
  command-bearing reviews during physical folder permission loss/regrant after
  accepted native execution. Replacement hosts restore original file metadata
  and both unsaved Unicode drafts without the original service; an unstarted
  approved command remains pending until explicit discard and is never replayed
  ([physical followup](physical-folder-checks.json)). An additional
  native PWA case removes folder access while the command is confirmed running
  and its physical lock excludes competing work. Reload and regrant preserve both
  exact CRLF/Unicode drafts and original disk metadata; saves remain paused until
  manual acknowledgement, with no command replay
  ([running-command recovery](running-command-permission-checks.json)).
  Independent-provider production contracts now cover source-neutral diagnostic
  refresh, disablement and installed-version replacement before authorization and
  at native admission, identical-looking results and late replies. All seven
  families pass for resource-only and command-bearing reviews in both modes,
  retaining original review/session, cancellation, disk/draft and undo assertions
  ([provider review checkpoint](provider-review-checks.json)).
  Finish remaining editor interleaving coverage.
  Result normalization and initial native admission now retain affected-file
  identity/change-time evidence outside language associations. Ordinary validation
  shares the stateless signed path set and rechecks it after asynchronous context
  checks. Native contracts cover restored contents/mtime, aliases, missing paths,
  repeated reads, bounded metadata capacity and fresh retry; protocol 3/schema 68
  preserve archived review identity
  ([checkpoint](affected-files-checks.json)). Shared original and
  created-object provenance now rejects same-content replacements after accepted
  Prepared state, ambiguous directory moves, durable intent and private cleanup in
  both native modes. Folder fences reject reparented roots and replaced ancestors
  ([identity checkpoint](accepted-object-checks.json)). Shared
  accepted-recovery metadata checks now reject changed permissions, mtime, restored
  bytes/mtime and added hard links on untouched originals, including partial
  cleanup. Script-owned moves and directory changes permit only their corresponding
  metadata transitions. Persistent original birth identity is required for new
  resource reviews; ordinary analysis remains available without it. The recorded
  native contracts pass macOS and Linux. Original ACLs, extended attributes and
  semantic flags now remain checked after script-owned moves and namespace changes.
  Bounded attribute collection preserves text and command reviews without file
  operations; both modes preserve changed private originals before rollback or
  cleanup ([attribute checkpoint](accepted-attribute-checks.json)).
  New native creation receipts now preserve changed metadata through publication,
  rollback, interrupted staging and final anchor cleanup. Both modes cover later
  permissions, timestamps, attributes, ACLs, flags and file hard links after the
  original service/grant expires; archived recovery remains compatible
  ([creation receipt checkpoint](accepted-creation-checks.json)).
  New claims preserve unwitnessed creations before receipt publication, requiring
  explicit inspection/resolution instead of guessing their original metadata.
  Both native modes resume after that resolution without replay; archived claims
  and damaged/future records have separate recovery checks
  ([unwitnessed creation checkpoint](unwitnessed-creation-checks.json)).
  Finish reusable-result keys, filesystem-event coverage and transitive runtime/toolchain
  invalidation. Complete native Windows file identity/change-time and accepted-resource
  recovery verification beyond the selected notification and Git metadata contracts.
  API 10 now declares interpreter argument files and exact sidecars separately
  from mutable outputs. Public schema, lifecycle/cache and installed Local/Remote
  checks pass, with rebuilt SDK-matching defaults, native macOS/Linux contracts
  and Windows cross lint ([runtime input checks](runtime-input-checks.json)).
  This does not discover undeclared transitive dependencies or replace filesystem events.
- Run the same service/cache contracts against both adapters, including reconnect,
  concurrent overlays, cancellation, eviction, folder-access recovery and updates.
  The `146c26b3` Linux memory-recovery fixture passed within its 64 MiB
  per-service / 64 MiB total test envelope, preserving the 96 MiB pressure
  allocation, backoff, source and process-continuity assertions. Production
  limits are unchanged. That runtime job failed a separate process-group shutdown
  assertion; twenty Linux arm64 repeats pass without establishing its cause or
  a fix for the hosted x86-64 failure. Failure-only process-state diagnostics
  preserve the original assertion and deadline.
  Earlier manual runs pass both complete editor partitions. On `146c26b3`, both
  automatic-release wrapped replay jobs failed after all twenty geometry-oracle
  calls returned and before final owner restoration. Both automatic editor
  partitions pass on the later published `80d57c39`; the earlier failure's cause
  remains unproven. The `80d57c39` runtime job still fails the separate process-group
  shutdown assertion; failure-state diagnostics and release packaging are tracked separately.
  On the later `f4a10bc7`, the backend pin-fetch, native shutdown and Local
  wrapped-replay jobs failed separately. Published `1e02b234` passes all 34 local
  CI cases and the actual push gate of 765 browser tests. Its hosted run finished
  with a Local wrapped-replay timeout; the other fourteen executed jobs passed.
  The later published `762a24e6` also passes the complete local gate, and both
  hosted editor shards pass with their original workloads and deadlines. Its
  Intel macOS bridge job failed dependency DNS resolution; ARM macOS hit the
  existing plugin-build time limit. Complete reliable hosted packaging and
  investigate the failures without inferring their cause from later passes.
  The native builder now grants the trusted resolved system DNS file only during
  dependency fetch, identifies immutable packages before compilation and retains
  available bounded stderr on timeout or cancellation without requiring pipe EOF. Native macOS release jobs
  run the sandbox/process regressions before building bundled packages. The
  original fetch, compilation and log-drain budgets remain; these changes do not
  establish a fix for either hosted failure.
  The shared wrapped replay fixture now advances account generations monotonically,
  matching production sign-in/logout. Both-mode original/candidate checks retain
  all five scenarios, four edits each, complete geometry oracles and deadlines;
  dedicated counter-restoration fixtures remain. Four exact local runs pass, with
  one large runner wall-time outlier recorded separately from its browser duration.
  Keep complete fixtures, assertions and existing deadlines while investigating
  hosted completion. These finite local checks establish no performance benefit
  or hosted failure cause/fix ([hosted checkpoint](hosted-checks.json)).
  Benchmark resource use on a small homelab before claiming prefetch improvements.

Extend the public [language contribution contract](../plugins.md#language-support),
with the same permissions and lifecycle for bundled and community packages.

