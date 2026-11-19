# Plugin runtime

See [Plugins and marketplaces](plugins.md) for installation and project controls and [Plugin authoring](plugin-authoring.md) for the SDK.

## Bundled defaults

The selection is locked in `plugins/bundled.json`. `tools/bundle_plugins.py`
materializes only those package directories from the pinned public Git commit,
along with its MIT license. Docker builds regenerate the host snapshots; CI
checks the committed snapshots against upstream. Distribution builds compile each
selected Rust plugin in the installation compiler sandbox, check its exported tools, events, panels and language definitions against its manifest, and embed the resulting WASM in the execution host.
First installation of these defaults needs no network or compiler. Development
builds without a compiled bundle embed source and compile it on installation. Browsers
receive manifests and managed skills through the existing APIs; plugin files are
prepared and published on execution hosts. Updates still use the marketplace.
The lock's optional `commits` map can pin individual package paths independently
of its default commit, preserving the exact releases of unchanged packages.
To change the baseline, edit the lock, run `python3 tools/bundle_plugins.py`, and
commit the regenerated host snapshots. Use `--repository <upstream-clone>` to
regenerate or check without network access.

To produce a native host with offline executable defaults:

```sh
cargo run -p openwebide-plugin-runtime --bin openwebide-bundle-plugins --locked -- \
  bridge/bundled/plugins.json /tmp/openwebide-bundled-plugins.json
OPENWEBIDE_BUNDLED_ARTIFACTS=/tmp/openwebide-bundled-plugins.json \
  cargo build -p openwebide-bridge --features bundled-defaults --release --locked
```

The build tool uses the pinned Rust toolchain and public SDK. Linux root/container
builds require the `openwebide-plugin-build` helper alongside it; non-root Linux
hosts require bubblewrap, and macOS uses the native compiler sandbox. Only matching
source commits, package digests, SDK digests and toolchains reuse embedded artifacts.
The installer still validates exports and publishes the same per-owner cache and
receipt; execution uses normal capability grants. Custom plugins follow the same
source compilation path rather than borrowing built-in behavior.

`bundled-defaults` makes a distribution build fail if the compiled bundle is absent.
Run `cargo test -p openwebide-bridge --features bundled-defaults bundled_tests` with
the same environment variable to verify fresh server and paired-host caches. The
compiled bundle is a generated build artifact; it is not committed to this repo.

## Preparation and installation

The shared plugin facade selects the project host. Both transports use the same
core validation and installation policy. The native bridge reads Git objects from
a bare cache, validates API 1 skills, API 3/4 executables and skill resources, rejects
retired API 2 tool-group activation, and
publishes a commit/content-addressed snapshot by atomic rename. Hooks, checkout
filters, package scripts and runtime installers are not run. Symlinks, submodules,
path collisions and unsupported contributions are rejected. Packages are limited
to 2,048 files, 128 KiB per file and 16 MiB in total; contributed skills must also
meet the existing project skill import limits.

Caches live outside project folders. Native bridges use
`$XDG_DATA_HOME/openwebide/plugins` or `~/.local/share/openwebide/plugins`;
`OPENWEBIDE_PLUGIN_DIR` overrides that path with an absolute host cache path. The container uses
`/app/.spin/plugins` on the existing persistent volume. Backend users have separate
cache namespaces; locally paired clients use the paired host namespace.

Logical installation records are user-scoped database settings, shared across
clients. They include the source commit, validated manifest, content digest and
historical preparation receipts for each host. Receipts describe a completed
preparation, not ongoing host availability. Reinstalling on another host prepares
that same version there. Preparing the same cached version verifies it before
returning success and works offline. A modified snapshot is rejected rather than
silently repaired in place.

To change the selected commit, refresh the installation list and install the new
commit. Database updates check the installation revision, and preparation failures
leave the previous record and snapshot intact. Account, project, session or host
changes prevent stale browser results from recording an installation. An already
submitted server transaction may finish for its authenticated account.

Historical Plugin API 2 receipts may contain `contributions.toolGroups`: `web`,
`memory`, `scheduling` and `skill-authoring`. They remain readable for existing
installations, but no longer expose built-in agent handlers or feature context.
Plugins shows Update required, and shared preparation, installation and activation
policy rejects these releases. Existing receipts can still change their update
preferences; project opt-outs, pins and stored data remain intact. Updates to
executable releases use the normal capability review. File editing, shell
execution, git, questions and task coordination remain core tools.

Skill-only contributions do not require Skill Authoring. When no enabled
executable plugin supplies both skill discovery and reading, shared
run preparation advertises `plugin_skill_list` and `plugin_skill_read` and includes
a bounded catalog of enabled plugin skills. Server, bridge and browser executors
read the snapshot captured when the run started, including named resources; later
updates, disablement or removal cannot replace those instructions mid-run. This
generic loader excludes personal skills and provides no authoring operations.
Executable skill handlers continue to use their own SDK implementation.

Tool-group manifests are retired feature switches. PR Review remains optional.

## Language service lifecycle

Both execution hosts use the same authenticated service and result contract.

### Startup and document close

Servers start on demand and remain warm across tab changes and brief disconnects.
Closing a document never starts a missing server or retries failed startup. It
cancels an older pending request and closes retained overlays even after a file,
parent folder or workspace marker disappears. A close with no running service
returns an inert `closed` acknowledgement, which cannot identify an executable
server command.

Service acquisition registers requests before capacity eviction
can select them; explicit revocation and memory limits can still stop a service.

### Workspace identity and declared inputs

Queries check physical project and workspace identities before processing and
before returning a result. Replacing either folder stops its old service; a later
request can start a fresh one and synchronize the current buffers.

Declared input collections retain a content snapshot and a monotonic observation
revision for native identity/modification changes. Restoring previous contents
does not make an earlier observation current again. Reviewed commands admit the
expected post-edit configuration, then retain that observation through subsequent
synchronization, queued waits and final source verification. Failed collections
publish no partial inventory. These sampled checks do not replace complete
filesystem-event and transitive-input generations.

### Git revision and metadata

Services also bind their analysis to the Git commit and symbolic branch captured
at startup, including unborn branches, detached HEAD and linked worktrees. A
different current revision rejects an in-flight reply or a reviewed command
before dispatch. The execution host's Git adapter reads revision metadata without
scanning source files or refreshing the index. The next query
can establish a fresh service with the current unsaved buffers. Observed branch
and commit changes clear editor intelligence results without discarding drafts;
ordinary status-counter changes do not reset language feedback.

Git metadata also uses the shared bounded generation tracker: HEAD, symbolic-ref
chains, selected reference namespaces, packed refs, worktree pointers/configuration
and reftable table lists/namespaces are observed around native analysis and command dispatch.
A branch or reference round trip invalidates the original service even when its
final commit, branch and HEAD bytes match. The observation follows the original
process through client handoff. Ordinary index refreshes do not invalidate it.
Git resolves metadata paths, including linked worktrees and external reference
directory aliases; the host does not scan the source tree for these checks.

One of the four scan workers and one quarter of the notification-handle budget
are reserved for required metadata, so optional source speculation cannot exhaust
them. Failed observations do not publish a partial generation. Complete unobserved
Git/filesystem changes and editor apply-time freshness remain roadmap work.
The [Git metadata checkpoint](code-intelligence/git-metadata-checks.json) records
the native before/after regression, both authenticated transports and platform limits.

### Executables and runtime inputs

The host also retains the declared launch executable's file identity, target and
modification metadata, plus native executable paths observed in its process tree
after initialization. It also retains modification and identity metadata for
symbolic links along their launch paths, including parent directories and
intermediate targets. Replacing a link rejects stale services even when it still
selects the same executable. Replacing or editing one of these files rejects stale
requests and commands and stops the old service. Commands check native context
again after verifying the affected files, immediately before dispatch. An ordinary
retry can start a fresh service with the current drafts. Launch aliases remain
intact for toolchain shims. These boundary checks do not track every transitive toolchain dependency
or replace the planned filesystem-event generation contract.

API 10 packages can additionally declare `runtimeInputs`: selected file-argument
positions and exact sidecar implementation paths. Those files share executable
identity/modification checks and contribute to derived-cache identity. Explicit
roles preserve mutable log/output arguments; see the
[public declaration](plugin-authoring.md#language-plugin-build-toolchain).

### Client handoff

Different editing clients keep separate unsaved source inventories. A matching
idle process can transfer between clients: the host closes its previous live
documents, restores the incoming client's complete inventory and issues a fresh
service identity. LSP document versions increase across transfers, and old
action, completion, symbol and resource handles are discarded. Active requests
and unexpired command reviews prevent transfer or capacity eviction. Retained
sources survive cancelled restoration and invalid paths; closing a parked file
does not touch the current client's overlays or start another process.

Overlapping requests can require independent processes. Once compatible services
are both idle and have no command pins, an ordinary acquisition or idle maintenance
retains one process and releases the redundant process. Each client's complete
source inventory and request watermarks remain separate; a later handoff restores
that inventory with a fresh service identity. Active requests and outstanding
command reviews retain their original process. Derived-cache leases remain
protected until the retired process tree confirms cleanup.

### Validate retained analysis

Intelligence protocol 2 adds an optional `context` to native analysis replies.
It binds the normalized result digest to the original live service, editing client,
document source/revision, observed project-source generation and unsaved-overlay
epoch. Each service issues bounded, authenticated proofs with a five-minute
lifetime; replacement and client handoff invalidate them. The shared
`LanguageService::retain_analysis` captures the normalized reply, document and
selected service in an immutable `LanguageAnalysis`. Its validation uses the shared
`validate_analysis` facade, which checks the retained result bytes before preparing
authority, then validates against that original service without an LSP request,
new overlays or a replacement process. The ordinary scoped transports also
recheck declared inputs, Git, executable and folder identity. Contexts provide
freshness evidence and grant no filesystem permissions.

Analysis queries now observe associated sources around their native requests.
Known changes reject the reply and retire mutable completion/action handles.
Incomplete coverage preserves ordinary read-only results without a context, while
prefetch, validation and edit-producing resolution, rename and formatting require
complete observation. Formatting and resolved completion now retain their exact
normalized result and selected service, then validate on that original service
before publishing an editor history entry. Delayed validation cannot overwrite
changed drafts, selections, accounts or providers. Completion keeps its popup
during validation and on failure, with retry, snippets and undo preserved.

### Review and apply workspace edits

Workspace reviews using the normalized `files` list now retain the same analysis,
reject changed edit bodies and capture the exact displayed review for approval.
Original-service validation precedes atomic publication of all affected editor
histories; rejection preserves the review for retry. Stale approvals and changed
drafts cannot publish a replacement review. The
[workspace text checkpoint](code-intelligence/workspace-text-application-checks.json)
records both-mode contracts and compatibility checks.

Ordered `changes` and
command-bearing reviews now retain their original analysis too. Changed command
arguments and attempts to apply only a command's edits reject before resource
inspection. Command approval uses the retained request/provider; native validation
precedes transaction creation and rechecks the editor recovery guard afterwards,
including intrinsic read-only changes
([resource checkpoint](code-intelligence/resource-review-analysis-checks.json)).

Local action discovery and rename now establish the physical folder capability
before requesting analysis, then retain that same session through review and
execution. Creating a new proof after analysis changes directory observation and
correctly invalidates the old result; Apply therefore reuses the original
capability. Only resource previews retain recovery handles. Unavailable resource
access still permits validated text-only edits. Acknowledged session close
suppresses redundant cleanup; ambiguous acknowledgements retain cleanup retry.
Installed resource, command, interruption/recovery and declared-input workflows
pass in both modes, including original-service and folder-session trace checks
([capability checkpoint](code-intelligence/review-capability-checks.json)). The
Local browser fixture uses OPFS/native correspondence; physical permission checks
remain separate evidence.

Schema 66 adds optional durable `transaction.language_review` data using the public
`workspace_transaction::LanguageReview` type. It retains the original provider,
binding revision, request, context and complete normalized result, with a digest
of the exact resource plan and editor patch. Its serialized bytes count toward
the existing journal body and account quotas; recovery scopes stay small.
Retries cannot replace or remove the review, and old records without one remain
recoverable. Migration replay rebuilds the complete charge from stored data.
The [storage checkpoint](code-intelligence/language-review-storage-checks.json)
records this format and persistence boundary.

### Native worker admission and recovery

The native verifier now authenticates the exact original service, request and
normalized result under its physical folder lease, without a new LSP request or
replacement service. Only the matching prepared command receives an exception
to the pending-command fence; ordinary analysis queries retain their rejection.
A guard pins the original overlays and service while it remains held. The
frontend and verifier use the same planner to derive recovery and editor patches,
including unchanged affected input records. Forged text, saved baselines,
selections, scroll and read-only state reject even when a client recomputes its
unsigned transaction digest. Native contracts exercise both authority profiles;
shared replay contracts cover both project modes
([verification checkpoint](code-intelligence/native-review-checks.json)).

Initial worker admission now checks a fresh manual UI grant, exact installed
provider, current selection/configuration, binding revision and project root in
the same database transaction as worker claim. Command-bearing reviews also
require approval of the complete original action. Schema 67 records the accepted
full-review digest atomically with the worker credential and reserves its 64 bytes
in recovery accounting. Grants remain outside durable evidence and recovery scopes.
Old schema-66 evidence remains unaccepted on upgrade. Accepted identity survives
migration replay and database reopen; changed evidence and stale workers reject.

The Remote runner verifies the original native proof under its lease before this
claim. Accepted recovery uses the immutable digest instead of requiring the old
service or expired grant, including after its own file changes. Native tests cover
provider revocation at claim, missing command approval, retry and an ambiguous
physical move restored after service shutdown, without command replay. Both
modes use the same database acceptance policy, including paired delegation
([admission checkpoint](code-intelligence/review-admission-checks.json)).

Local and Remote now use the same complete native transaction runner above
thin folder and journal adapters. Local submits one Execute on the original
proven session. The worker retains its physical gate through the shared
interpreter and durable release, including after observer loss. It acquires the
gate before loading a full snapshot and rejects stale queued recovery before
mutation. Private paired preflight reads require the original bootstrap and
native worker credential; they do not admit a worker. Legacy browser recovery
remains available for older records.

A real Local LSP/HMAC contract verifies original evidence before admission,
rejects an invalid manual grant, permits the unchanged review to retry and
acknowledges accepted committed work on a successor session after the old service
and grant expire. Fresh installed resource/command and declared-input workflows
pass in both modes. The Local trace requires one Execute on the original session
and no browser mutation/cursor loop. Interrupted checks compare the actual native
folder with the explicitly refreshed OPFS test counterpart
([complete worker checkpoint](code-intelligence/paired-worker-checks.json)).
These fixtures do not establish physical permission prompts or unconditional
SIGKILL restoration during a review-bearing Local transaction.

### Affected file identity and metadata

Workspace-edit normalization now records affected native files independently
of language associations. Ordinary original-service validation and initial native
admission share that evidence. On Linux/macOS, reads use a captured directory
descriptor, retain parent/file/link identity and change times, and reject changed
or inconsistent repeated reads. Missing targets retain their original namespace.
Text aliases retain both link and canonical target observations. The complete
set is rechecked after asynchronous context checks and before publication or
initial file execution. Restoring original bytes and modification time cannot
restore the original proof.

Intelligence protocol 3 carries a sorted path set alongside the opaque proof;
the service signature binds its native metadata aggregate and exact result.
There is no retained native result cache. Paths have count/byte limits within
the existing complete response limit. In-flight observations reserve a shared
64 MiB charged pool, capped at 16 MiB per operation; this is separate from full
process/browser memory accounting. The existing 10,000-entry resource inventory
fits the metadata charge. Schema 68 fences older readers while preserving
archived protocol-2 language evidence and protocol-1/2 command-review identity.
Native and browser checks plus fresh installed workflows pass
([affected-file checkpoint](code-intelligence/affected-files-checks.json)).
Windows cross-compilation verifies the adapter builds; native Windows behavior
and descriptor anchoring still need verification.

Protocol-3 analysis also carries bounded native object identities aligned with
its paths, bound into the same original-service aggregate. The shared resource
interpreter tracks each original path and creation ordinal through complete trees,
forward moves, rollback and private cleanup. Both native workers compare those
identities at the current script locations, recheck after durable intent and
preserve same-content replacements. Creation receipts and file anchors follow
created objects after moves. Accepted-recovery contracts cover unstarted work,
unacknowledged directory moves, intent-time replacements and irreversible cleanup
after the original service and grant expire. Physical folder checks also reject
reparented roots and replaced ancestors, even when the leaf still matches
([stable object identity checkpoint](code-intelligence/accepted-object-checks.json)).

Protocol-3 contexts also carry bounded opaque original metadata aligned with
those paths and identities. Native Linux/macOS readers observe descriptor metadata
before and after identity acquisition. Shared provenance requires exact metadata
for untouched originals; a moved root permits its own change time, and a changed
directory permits namespace-induced size, link and time changes. Permissions and
ownership remain checked, and descendants do not inherit a moved root exception.
Initial checks reject changed originals before the first durable intent and cancel
untouched Prepared work. Accepted-recovery contracts cover permissions, modification
times, restored bytes/mtime, added hard links and partially completed cleanup.
New resource reviews require persistent birth identity; ordinary analysis remains
available on filesystems without it. Empty command-only observations remain strict
and authorize no original-object access. These rules do not establish universal
change-time ABA detection after the application's own moves or namespace changes.

Original revisions also bind native extended attributes, ACLs and semantic flags
to an opaque digest. Reads use the same owned descriptor with before/after checks;
macOS ACLs use the descriptor API, while Linux ACLs use their native xattrs.
The digest remains checked after the script's own moves and namespace changes.
Linux retains unknown flag bits while excluding known internal storage-layout
flags; macOS flags are captured in full. Access times are excluded because normal
application reads can update them.

Attribute collection reserves working capacity first: a shared 16 MiB pool,
64 KiB of names, 256 names, 1 MiB of names/values and 64 KiB of ACL copy data.
The existing metadata/response limits remain unchanged. Incomplete access or
capacity leaves a partial observation, not an assertion that attributes are empty.
Plans declaring file operations require complete original observations before
admission and at each original-resource check. Text and command reviews without
file operations preserve ordinary reads and original-service validation without
acquiring a native resource capability. Real native contracts cover changed xattrs,
ACLs and flags on already-moved roots in both modes, plus the bounded text fallback
([original attribute checkpoint](code-intelligence/accepted-attribute-checks.json)).

New native creation receipts retain an opaque metadata revision before creating
a file anchor. Publication, rollback, unpublished staging and final anchor cleanup
compare that witness, accounting for the application's own link and namespace
changes. Shared provenance follows created objects through moves and child changes.
An immutable, identity-bound native intent records parent namespace changes before
staging a child, including a child whose publication is interrupted. Attribute,
permission and ownership checks remain in force. Complete and partial intent
records have bounded validation and cleanup; unknown metadata is preserved.

Local and Remote contracts cover later file/directory permissions, modification
times, extended attributes, ACLs and flags, plus additional file hard links, after
the original service and grant expire. Controls restore unchanged objects without
query, command or editor replay. Native fault cases also cover changed unpublished
objects and the final file anchor
([creation receipt checkpoint](code-intelligence/accepted-creation-checks.json)).

New private creation claims also protect the interruption before that metadata
receipt becomes durable. Recovery preserves the unwitnessed object for inspection,
even if it is still empty; a fresh read never becomes the lost historical baseline.
After explicitly inspecting and resolving that artifact outside automatic recovery,
retrying the accepted transaction restores its original state without a new language
query, grant, command or editor publication. Unknown claim versions and lost receipts
preserve their objects. Archived claims and receipts retain their earlier contract
([unwitnessed creation checkpoint](code-intelligence/unwitnessed-creation-checks.json)).

The editor now builds `LanguageReview` from its retained original result and
selected package revision, without a new query, preparation or grant. It persists
that immutable evidence for native transactions, then obtains fresh sessionless
manual authority under the project lock before initial execution. Local and Remote
share this orchestration and the complete native worker. Accepted recovery and
committed retries request no new grant or analysis. Installed checks compare the
stored evidence with the exact native reply and cover host interruption, access
loss/regrant, Prepared cancellation and command non-replay in both modes
([retained review and metadata checkpoint](code-intelligence/retained-review-execution-checks.json)).
Local installed fixtures use OPFS correspondence; review-bearing physical-folder
permission prompts and native Windows execution still require separate verification.
The [editor checkpoint](code-intelligence/editor-application-checks.json)
records the earlier formatting/completion contracts and remaining coverage. Live intelligence requests use
version 3, separately from plugin API 9 and syntax protocol 1. Schema 68 prevents
older builds from reopening newer state; archived version-1/2 action reviews and
version-2 language evidence remain recoverable without rewriting their input. The
[analysis-context checkpoint](code-intelligence/analysis-context-checks.json)
records both transport contracts, platform checks and these limits.

### Idle language preparation

The public intelligence protocol also accepts optional `prefetch` queries for
`diagnostics` and `symbols`. Both use the ordinary authenticated, source-bound
query path. The native host permits one speculative query at a time, including
filesystem discovery, with a two-second deadline. It requires the editing
client's own warm service, declines work while interactive requests or
command reviews are active, and never starts a cold service or evicts one merely
to prefetch. A parked client retains its independent sources but cannot reclaim
another client's current process for optional work. Valid interactive requests
cancel speculation across documents and projects; the speculative owner retains
its slot through source retirement. Background request accounting leaves capacity
for newly opened foreground documents without dropping recent stale-request fences.

The optional `close: true` field admits a temporary source only when its document
ID and physical path are not already open. Owned cleanup closes that overlay or
stops the original service before releasing its source bank, preserving other
unsaved documents through cancellation and recovery. These replies contain no
reusable analysis context. The default `close: false` is omitted from the wire;
older strict hosts reject the new field rather than retain a temporary overlay.

An interrupted prefetch suspends further speculation on that original
process, because a server may continue computing after cancellation. This state
follows process handoff and resets on actual process restart. Interactive queries
remain available. A completed request rejected for changed project files may retry
on the same service.

**Settings → Editor → Language support** includes **Warm language services while idle**, disabled by
default and saved in the account's database settings. The shared editor scheduler
waits 750 ms after ordinary work settles, then considers at most four files.
Current open buffers take priority, followed by known related unopened files and
recently closed files within the remaining budget. Retained unsaved text wins;
clean files receive fresh bounded reads through `Workspace`, using cancellable
Local streams or the Remote bounded-file endpoint. Failed reads also consume the
file and reserved byte budget. There is no recursive dependency discovery.

Each file is limited to 256 KiB and a batch to 512 KiB,
with at most eight diagnostics/symbol attempts per source revision and service.
A three-second client deadline also bounds authorization and transport waits.
Foreground work, edits, composition, recovery, scope changes, unmounting or turning
the option off cancel pending work. Speculative replies never enter editor results,
errors or undo history; this prepares the server's own state without adding a
result cache. Installed public-plugin checks cover fresh external edits, unopened
related files, recently closed files, temporary-source retirement and preservation
of the original service and other drafts in both production editors. The Local
fixture uses OPFS correspondence, not a physical-folder permission prompt.

### Source observations and file watches

Before and after analysis, both native routes collect the same bounded
metadata inventory of associated source files, declared project inputs, directories
and symbolic links. Related dialects sharing a server contribute their associations
to one inventory; content-based or manually selected languages conservatively
include all files. Directory aliases are followed with cycle detection, while Git
metadata and private runtime payloads are excluded. Native identity and change-time
metadata detect restored contents and modification times in the tested Unix cases.
Only complete collections publish a generation. Errors, cancellation or inventory
limits reject optional work and edit-producing queries while retaining ordinary
read-only replies without a proof; read workers retain their concurrency slots
until they actually exit. These sampled checks do not establish complete
filesystem-event coverage, transitive toolchain invalidation or editor apply-time
freshness. Complete production resource/benefit measurements remain roadmap work.

A same-build comparison of the earlier open-buffer scheduler in 128 modules found nearly
unchanged median foreground-symbol latency and a higher Remote p95 with warming
enabled. The combined 512-module workflow exhausted optional object-watch capacity
in its Local phase while foreground work continued. Idle preparation remains
opt-in; the [measurements](code-intelligence/idle-prefetch-measurements.json)
record the failed comparison criterion and remaining limits.
The [native checkpoint](code-intelligence/project-generation-checks.json) records
the tested cases, source hashes, bounds and remaining limits.

On macOS and Linux, these observations also retain explicit object watches for
collected regular files and directories. The shared manager owns reconciliation,
limits and cancellation above thin kqueue and inotify adapters. The kernel never
discovers or recursively adds watches. Registration changes require another matching inventory; errors,
revocation or events arriving during collection reject that optional observation.
Losing and rebuilding coverage advances the generation even if disk metadata is
unchanged. Notification descriptors, including queues, share a process-wide cap
of 1,024 and at most one quarter of the inherited descriptor limit. Linux binds
inotify watches to owned file descriptors, retains shared hard-link registrations
until their last path is removed, and rejects lost watches or queue overflow.
The total handle budget is partitioned between source speculation and required
metadata; source speculation can use at most three quarters of it.

Windows additionally implements nonrecursive directory notifications with a unique
event per pending read. Owned handles, buffers and permits remain retained through
confirmed completion, including cancellation. A single cleanup worker retires
pending reads; per-process handle and 16 MiB notification-buffer budgets bound
admission. Parent-only watches filter unrelated names and verify object identity
without requiring stable directory timestamps. Windows production and test code
passes cross-compilation, while native Windows execution remains pending.

Complete filesystem-event coverage, Git metadata and transitive runtime
invalidation remain unfinished.
The [source-event checkpoint](code-intelligence/source-event-checks.json) records
both platforms' native contracts, descriptor bounds and source hashes. The earlier
[macOS checkpoint](code-intelligence/vnode-checks.json) remains a historical record.
The [Windows implementation checkpoint](code-intelligence/windows-source-event-checks.json)
separates cross-compilation and macOS regressions from outstanding native Windows checks.

### Service limits and memory pressure

Service counts, retained source and response sizes, request time and idle lifetime are bounded. The native
host samples process-tree resident memory every two seconds, counting child
processes once. Sampling retains only original-process identity, allowing idle
handoff during OS observation. Before applying a reading, the host resolves that
process's current owner under the acquisition lock; retired or replaced process
samples cannot evict a new service that reuses its key or numeric PID.

Default limits are 1024 MiB per service and 1536 MiB across services,
with a 128 MiB host-memory reserve. Operators can override these MiB budgets with
`OPENWEBIDE_BRIDGE_LANGUAGE_SERVICE_MEMORY_MIB`,
`OPENWEBIDE_BRIDGE_LANGUAGE_TOTAL_MEMORY_MIB` and
`OPENWEBIDE_BRIDGE_LANGUAGE_MEMORY_RESERVE_MIB` on the execution host.

Over-budget services are stopped; other memory
pressure reclaims idle services first. Requests report the eviction and wait five
seconds before retrying. Recreated services restore their retained source inventory;
the shared synchronization facade also supplies current unsaved buffers. These are sampled limits; transient allocation
between samples can exceed them. Missing
toolchains and unavailable hosts leave ordinary editing available.

## Executable plugin requirements

First-party plugins must own their behavior and use the same public Rust SDK,
versioned host interfaces and lifecycle as community plugins. They may use general
host capabilities such as namespaced persistence, HTTP, model completion
and durable job dispatch. Workspace and process interfaces remain future work. Feature-specific policy,
orchestration and result shaping must live in the plugin; a built-in feature
hidden behind a host API does not satisfy this requirement. There must be no
first-party identity dispatch, privileged feature endpoint or built-in fallback
when a plugin is missing, disabled or fails. Skill-only contributions may remain
instructions/resources without an executable.

The executable path ships Rust source and a committed Cargo.lock. API 3 declares
the library, SDK version, capabilities, exported tool schemas and event names.
API 4 adds typed panel definitions and their exported view/action handlers. Installation
compiles source in isolation on the execution host, against its pinned compiler
and embedded public SDK, then validates the exported schemas before activation.
The artifact cache includes source, SDK and toolchain fingerprints. Compilation
failures preserve the installed version; clients never compile or execute plugins.
Publisher CI should validate source builds, but installing a plugin does not
require a publisher-hosted WASM artifact. Bundled plugins receive the same
interfaces and privileges.

The SDK, source preparation and shared execution workflow ship in nightly builds. HTTP and clock primitives run on the bridge. Plugin HTTP carries a host-added
transport marker that plugin headers cannot remove. Open WebIDE rejects marked
requests to its authenticated backend and bridge control endpoints, including
credential bootstrap and WebSocket upgrades. Ordinary HTTP access to public and
LAN services remains available; the HTTP capability does not supply TCP tunnels. Private record callbacks
use opaque run grants checked against the authenticated account, session, original
project and pinned plugin source/capabilities. Record collections persist across
plugin updates and reinstalls, with revision checks, bounded pages and quotas.
Grant tokens stay in host orchestration and are never passed to plugin code.
Execution contexts can scope a grant to an owned project without a chat session.
They use the same storage callbacks and retain the selected primary model where
configured; chat planners pin their selected model, including run overrides.
Context grants and chat grants cannot be used through each other's callback
endpoint. Existing active grants retain their pinned plugin version across
updates or removal; new grants require the currently enabled installed version.
Memory UI mutations use the same declared tool invocation policy as agent runs,
with sessionless grants and no built-in behavior fallback. The explicit UI action
grant permits manual editing while automatic Memory context is switched off;
plugin code cannot opt itself into this authority. Plugins on a paired host do
not require that host to see a local project's browser folder. Workspace commands
and Git retain their separate folder mapping requirement. Background runs use
shared leased delivery; compiled defaults have verified server and paired-host
HTTP bootstrap, with remaining lifecycle checks tracked in the roadmap.
The separate `completion` grant exposes bounded text generation through the
session's configured primary or fast model. Plugins supply prompts and interpret
the results; the host supplies model selection and credentials. Inputs are limited
to 32 KiB, outputs to 1–1024 tokens and 16 KiB of text, with a 30-second completion
deadline and no tools. Context hooks cannot request completions. The source
Memory plugin owns its naming prompt, profile fallback and content-derived title.
Workspace callbacks remain unfinished. Installation progress, cancellation and
compiled offline defaults are implemented; deployed browser lifecycle verification
remains open. API 2 tool-group releases require an update to executable handlers.

The separate `collections` capability exposes schema-validated CRUD for the
app-visible `memories` and `skills` collections. It preserves existing UI records and enforces
the user's Memory switch, ownership, quotas and revision checks; it does not
provide search, naming or context formatting. Private `records` grants do not
authorize these shared collections. The SDK also exports a read-only context
hook with an 8 KiB maximum contribution and the ability to disable only its own
declared tools. Shared run preparation executes that hook on both execution hosts
before model tool selection, including when model tools are disabled.

Plugins can also declare `contributions.events` and export matching names through
`Plugin::events()`. The host invokes `Plugin::event(EventInput)` through the same
capability-gated workflow as tools, using a fresh WASM instance. Event payloads
are bounded to 256 KiB; undeclared names are rejected before execution. An
event-only plugin may export no model-facing tools. These callbacks provide the
execution contract. The `jobs` capability supplies a durable one-shot queue with
idempotency keys, version snapshots, bounded pages and renewable delivery leases.
Jobs retain their creating plugin version across updates; disabled or removed
plugins receive no new claims. `jobs.schedule` defaults to `scope: "origin"`,
retaining its creating conversation. `scope: "project"` retains the project and
model context while dropping the conversation anchor, so saved work survives
origin deletion. In projectless contexts this scope is the account's global
conversation realm. The scope is immutable under an idempotency key; neither
choice authorizes a different account, project or execution host. Lease-bound callbacks stop after cancellation,
completion or lease replacement. Terminal jobs can be deleted to release quota
and their idempotency keys. The bridge polls for due events and runs them through the shared execution
workflow, renewing leases during compilation and execution. Delivery is at least
once; plugin code must use stable keys and revision checks for repeatable effects.
The published Scheduling source owns recurrence and event policy; remaining
browser lifecycle checks are tracked in the roadmap.

The `runs` capability exposes durable raw prompt submissions through
`list`, `read`, `submit`, `cancel` and terminal `delete`. Submissions use stable
keys and owned conversations in the grant's project, including a job's retained
origin conversation. A source may request a new conversation with its own title
or select an existing owned conversation. Prompts are bounded to 32 KiB; history
holds at most 1,000 runs and lists return up to 16 entries. Explicit or inherited
models stay on the queued run rather than changing conversation settings.
Plugin prompts are marked for host delivery and cannot be edited or drained by
the browser's foreground queue. Removing one cancels its pending submission.
Queue consumption and the user message commit together; consumed prompts cannot
be replayed. Internal daemon claims, lease renewal, busy-claim release and status
reports share the same durable store. Each claim receives a fresh queue revision
and runner identity, so stale preparations cannot consume replacement claims.
Recovery requeues only unconsumed prompts; a lost host after consumption marks the
run interrupted. Local execution requires a user-authorized host folder binding.
The bridge now dispatches these prompts through the shared agent runner, using
nonce-bound runner identities and the same lease, cancellation, approval and
status policy on server and paired hosts. Busy conversations release unconsumed
claims. Transient status-report failures retry while the lease heartbeat remains
active; lost leases cancel preparation or execution. Daemon shutdown drops active
deliveries. Completed reports retain bounded raw assistant output, without
interpreting a task or monitor outcome. Submissions may attach a `completion`
object containing a declared `event` and at most 64 KiB of `payload`. The host
reserves an event queue slot as part of submission; a full queue rolls back the
prompt and conversation creation. That event waits until the run reaches a
terminal state, then receives `{run: PluginRun, data: payload}` atomically with
the state update. Queue cancellation, preflight failure and interrupted-host
recovery also release it. Repeated status acknowledgements do not enqueue another
event. Released events survive run-history deletion and retain the submitting
source version, model and context across updates. Existing event lease/disablement
rules and at-least-once delivery apply. Callback cancellation suppresses delivery;
plugins own idempotency and outcome interpretation. Scheduling's executable source
implements its completion policy through this callback contract.

The `tasks` collection exposes the existing saved-task and monitor records through
scoped CRUD. Values contain `draft`, `next_run`, `state` (a plugin-owned JSON object)
and optional `monitor` metadata (`session_id`, `interval_seconds`, `remaining`,
`expires_at`). Reads also include the owning namespace and an `editable` flag;
these fields are read-only. The host validates data integrity and conversation/model
ownership, not cron, recurrence, future-time or monitor-outcome policy. Updates and
deletes require the current record revision. An unowned legacy task can be adopted
when no prior delivery is active; another plugin's task cannot be mutated. Adoption
preserves its ID/history and stops legacy dispatch. Goal-worker records are excluded.
Lists use ascending ID cursors, at most 32 records and a 1 MiB serialized envelope.
Scheduling's executable source manages these records and task history through
general collection callbacks.

Raw run `submit` accepts optional `prerequisites`, with at most eight objects of
`{capability, collection, id, revision}`. Capability is `records` or `collections`
and requires that separate grant. The host reads these records within the same
transaction that creates the prompt, rejecting missing, changed or disabled
records without creating a run, conversation or completion event. No caller-supplied
account, project or namespace is accepted. Identical-key retries return prior work
even if its prerequisite has since changed; a prerequisite protects new submissions,
not cancellation of already queued or running work. Plugins must explicitly cancel
that work when changing their policy.

The read-only `conversations` collection supports `list` (ascending ID cursor,
32 records and at most 1 MiB) and scoped `read`. Values expose names, creation and
last-message activity timestamps, archive/pin/automatic-title flags, raw session
model overrides and connection model metadata. `origin` identifies the grant's
retained originating conversation; project-scoped events have no origin. Only
owned conversations in the grant's project (or its global realm) appear, and
message contents and connection credentials are omitted. Revisions are opaque
positive metadata-content tokens; `updated_at` denotes conversation activity.
All mutations are rejected. Plugin code decides how to rank/filter these records
or select a target; the host does not provide a "latest conversation" policy.

The read-only `configuration` collection has one record, ID 1. Its value exposes
raw account preferences `primary`, `fast` and `default_connection`, plus shared
server metadata `servers: [{id, model, enabled}]`. It omits server names, endpoints,
credentials and unrelated settings. Read or list from cursor 0 returns the current
snapshot; later cursors are empty. Revisions are opaque positive content tokens,
and `updated_at` is 0. Values are limited to 64 KiB and 256 servers. The host does
not choose a default model. Source can use this metadata at delivery time rather
than silently retaining the creating chat's primary model.

The published Scheduling source exports five SDK tools and two durable event
handlers. Native workflow tests cover its CRUD, recurrence, model/target selection,
generation invalidation, bounded monitors and callback interpretation. An explicit
cross-repository component contract uses `OPENWEBIDE_SCHEDULING_COMPONENT` and
`OPENWEBIDE_SCHEDULING_MANIFEST` to run the built WASM against shared SQLite grants
and collections in both project modes (`cargo test -p openwebide-bridge --test
scheduling_component -- --ignored`). It covers task creation, due submission,
cancellation completion and deletion; it does not prove Git installation, actual
model execution, production compiler isolation or the deployed browser lifecycle.
Separate deployed server and paired-host HTTP checks verify plugin-owned naming,
agent execution, bounded monitors, restart and interrupted-run recovery, source
version handoff and pinning, streamed updates, and full-quota history journaling
and retention recovery. Deployed browser lifecycle verification remains open.

Skills collection writes accept a `draft` object using the existing skill schema.
Reads include that draft and read-only `origin` metadata for managed plugin skills.
The host protects managed or disabled skills from mutation and omits disabled
skills from reads. Pages hold at most eight skills and approximately 1 MiB of
serialized records to bound resource transfers. Collection mutations support the
existing 128 KiB skill data schema even when JSON escaping expands its payload;
private records retain their 64 KiB value limit.
Authoring, search and prompt policy stay in plugin source.

Every shared tool, context and event invocation prepares the selected source on
its execution host before starting WASM. The host receipt must match the pinned
source, manifest and digest; a different host ID is allowed. Preparation failures
stop execution. Valid source and artifact caches support offline reuse.

Both host adapters prepare plugin tools through the shared run planner before
applying connection tool selection and model tool settings. A plugin without an
advertised tool retains no tool execution grant after context planning. Context
hooks receive temporary pinned authority independently of tool selection. The installed version remains active
when a prepared update requests additional capabilities, including compatible
automatic updates. The Plugins interface shows the additions for explicit review;
approval applies only to that prepared version and installation revision.

Source builds use a read-only source/SDK/toolchain and a separate writable work
directory. Dependency retrieval is isolated too, with network access enabled only
for retrieval. macOS uses its native sandbox; native Linux users use bubblewrap.
Root Linux containers use the packaged `openwebide-plugin-build` launcher with a
fresh unprivileged identity, fully enforced Landlock ABI 3, and a syscall filter.
The ordinary Docker profile stays in place. Container kernels must support that
Landlock ABI; compilation fails if the protections cannot be applied. The Docker
image supplies the pinned toolchain in a location the isolated identity can read.
The source-to-WASM/runtime contract runs in CI on macOS and in a default Linux
container, including attempts to read host files, alter source metadata and use
the network during compilation.

Web, Project Memory, Scheduling and Skill Authoring ship as API 3 source
plugins in the official marketplace and compiled default bundle. They own their
behavior through the same public SDK as community plugins. Remaining lifecycle
verification and registry SDK publication are tracked in [the roadmap](roadmap.md).
API 4 supports declarative panels; API 5 language contributions and editor
integration are described in [plugin authoring](plugin-authoring.md#language-plugin-build-toolchain).
API 6 adds native LSP services; API 7 adds per-document server language IDs and
shared dialect synchronization through those same declarations. API 8 adds
declared read-only virtual documents for external definitions. API 9 adds
independently enabled diagnostics/fix services, bounded project-input declarations
and optional native derived-cache paths outside projects. API 10 adds declared
native runtime implementation files. Cache redirections use
the public server declaration and ordinary process review; recognized namespaces
have count/byte retention budgets and stay protected until native cleanup is
confirmed. Bounded periodic inventories stop owned cache producers after sampled
growth exceeds the budget; they reclaim idle data first and preserve unsaved
documents through process retirement. Sampling permits overshoot and does not
provide a hard disk quota. Plugin MCP
contributions, editor annotations and hooks remain future contribution work.

Configured host [MCP servers](mcp.md) are supported independently of plugins.
Declaring an MCP server in a plugin manifest remains future contribution work.
