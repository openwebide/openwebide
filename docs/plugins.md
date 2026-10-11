# Plugins and marketplaces

The **Plugins** button beside **Output** in the status bar opens discovery and
plugin management. Browse the official marketplace and any custom public Git
marketplaces configured for your account. Installed and available plugins appear
in compact rows with publisher/version details and inline actions. Search filters
both sections. Available excludes installed plugins; choose their releases from
the Installed gear menu. Uninstalling returns a plugin to Available. Listings show their marketplace as a Source label, with the official
source named Open WebIDE. Click a row’s name to inspect plugin details
and use the gear menu to choose a release. Install it on the open project's
execution host, or on the server host when no project is open. Plugins currently
contribute agent skills and host-executed Rust tools through the public SDK.
Older platform tool-group installations require an update to executable handlers.

Execution hosts bundle a commit-pinned subset of the official marketplace: Web,
Project Memory, Scheduling and Skill Authoring. Account initialization installs
missing defaults once through the normal validated host installation flow, with
Notify updates. Existing accounts receive this baseline too. PR Review stays optional.
The database remembers installation/removal by plugin identity; later sign-ins,
refreshes and app upgrades do not reinstall removed defaults, reset project
opt-outs or replace a selected version. Host unavailability defers initialization;
sign-in and existing installations remain available, and refresh retries it.

The selection is locked in `plugins/bundled.json`. `tools/bundle_plugins.py`
materializes only those package directories from the pinned public Git commit,
along with its MIT license. Docker builds regenerate the host snapshots; CI
checks the committed snapshots against upstream. Distribution builds compile each
selected Rust plugin in the installation compiler sandbox, check its exported tools
and events against its manifest, and embed the resulting WASM in the execution host.
First installation of these defaults needs no network or compiler. Development
builds without a compiled bundle embed source and compile it on installation. Browsers
receive manifests and managed skills through the existing APIs; plugin files are
prepared and published on execution hosts. Updates still use the marketplace.
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

Installation enables a plugin across your existing projects by default. New
projects inherit installed plugins too. **Disable for project** saves an opt-out;
updates preserve it. **Enable for project** removes that opt-out by loading the
installed version. Verified instructions/resources are stored as managed project
skills, with visible provenance. Personal skills remain independent; a contributed
skill name collision aborts the installation transaction without overwriting data.
The project's global Skills switch still applies.

Use **Manage marketplace sources** in the Plugins hamburger menu to open
Settings → Plugins. That settings tab
only configures marketplace sources; search, install, uninstall and project
enable/disable controls live in the Plugins interface.

The official source is `https://github.com/openwebide/plugins.git`, using its
default branch and root `marketplace.json`. It is built in and cannot be removed.
Additional sources accept a public Git repository URL, optional branch/tag/commit
reference, and catalog file path.
Releases declare only their immutable commit and package directory; every package
inherits its marketplace's repository. Refreshing catalogs updates discovery; Notify installations remain pinned. A failed refresh preserves cached
releases and reports the failed source. Removing a custom source stops discovery
without uninstalling its packages. Sources and caches are user-scoped database settings.

The Plugins status-bar button shows a count when updates are available. The
Installed heading shows **Update all (N)** when updates are available, even
when the section is collapsed; each installed plugin's gear menu offers an
individual update, release selection and update preferences. **Notify** is the
default: checking a catalog does not install its newer releases. **Automatic**
applies compatible updates; major version changes, and minor changes before 1.0,
still need a manual update. **Off** hides update notifications and prevents automatic
updates. Checks run at sign-in and every 15 minutes while the app is open. Failed
checks preserve cached releases; failed updates preserve the installed version.

Selecting another release updates or rolls back the installation and enabled
projects together. Project opt-outs remain disabled. **Uninstall** removes the
installation, inherited defaults and managed skills from all your projects,
without deleting personal memories, skills or schedules. Host snapshots and active
run instructions/resources remain pinned; changes apply to subsequent runs.

For packages outside a catalog, choose **Install a pinned plugin manually** from
the Plugins hamburger menu and
enter a public Git repository URL (HTTP, HTTPS, SSH or Git protocol), a full
lowercase commit ID, and the directory containing `plugin.json`; use `.` for the
repository root. The reference PR Review 0.1.0 package uses:

- Repository: `https://github.com/openwebide/plugins.git`
- Commit: `e79185c2b25f713503b70e23ee7e91e66c5af208`
- Package directory: `plugins/pr-review`

Local projects need their paired native bridge and folder access. Remote projects
use the server bridge, including when opened from a phone. The browser does not
clone packages or execute plugin code. Fetching uses the bridge host's existing Git
credentials; credentials cannot be embedded in repository URLs.

The shared plugin facade selects the project host. Both transports use the same
core validation and installation policy. The native bridge reads Git objects from
a bare cache, validates API 1 skills, API 3 executables and skill resources, rejects
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

Tool-group manifests are retired feature switches. The pinned default subset is Web, Project
Memory, Scheduling and Skill Authoring; PR Review remains optional.

## Executable plugin requirements

First-party plugins must own their behavior and use the same public Rust SDK,
versioned host interfaces and lifecycle as community plugins. They may use general
host capabilities such as workspace access, namespaced persistence, HTTP,
approved process execution and durable job dispatch. Feature-specific policy,
orchestration and result shaping must live in the plugin; a built-in feature
hidden behind a host API does not satisfy this requirement. There must be no
first-party identity dispatch, privileged feature endpoint or built-in fallback
when a plugin is missing, disabled or fails. Skill-only contributions may remain
instructions/resources without an executable.

The executable path ships Rust source and a committed Cargo.lock. API 3 declares
the library, SDK version, capabilities, exported tool schemas and event names. Installation
compiles source in isolation on the execution host, against its pinned compiler
and embedded public SDK, then validates the exported schemas before activation.
The artifact cache includes source, SDK and toolchain fingerprints. Compilation
failures preserve the installed version; clients never compile or execute plugins.
Publisher CI should validate source builds, but installing a plugin does not
require a publisher-hosted WASM artifact. Bundled plugins receive the same
interfaces and privileges.

The SDK, source preparation and shared execution workflow are foundations in
progress. HTTP and clock primitives run on the bridge. Plugin HTTP carries a host-added
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
HTTP bootstrap, with deployed browser verification still pending.
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
The Scheduling source owns recurrence and event policy; marketplace activation
and deployed browser verification remain pending.

The prototype `runs` capability exposes durable raw prompt submissions through
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

The unlisted Scheduling source now exports five SDK tools and two durable event
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

Web's source reference implementation now lives in the plugins repository and
uses only the public SDK HTTP primitive. Its API 3 release remains unlisted while
installation and migration verification continue. Memory's source implementation
now owns search, CRUD formatting and bounded context selection over the public
collection primitive; it remains unlisted pending planning and lifecycle
integration. Continue Scheduling and Skill Authoring. Verify the same behavior in
local and remote modes, including cancellation, crashes, disablement and updates,
before calling those migrations complete. Completion of executable plugins, MCP
servers, dependencies, language, UI and editor contributions remains on the roadmap.
