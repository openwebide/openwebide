# Plugin authoring

Current nightly hosts execute plugin-owned Rust compiled to WASM on the bridge.
API 1 supports skills; API 3 adds tools, read-only context hooks and durable events.
API 4 adds [declarative workspace panels](plugin-panels.md) and scoped view/action handlers.
The [panel playground](panel-playground.md) previews the production components
and exports validated documents and Rust SDK examples without running host code.
The SDK is version 0.1.0, independently of the app. API 2 tool groups are retired.
First-party Web, Project Memory, Scheduling, Skill Authoring and language families implement their
behavior through the same public SDK and grants as community plugins.

An agent in an open project can use `plugin_create` to create a package from
supplied files and `plugin_validate` to check its source structure and digest.
Creation requires approval and preserves existing files. Rust packages still
need a committed `Cargo.lock`; compilation and SDK export checks run on the
execution host during preparation. See [agent plugin management](plugins.md#agent-plugin-management).

Author-facing examples, schemas and publisher checks live in the
[plugins repository](https://github.com/openwebide/plugins). Start with its
[authoring guide](https://github.com/openwebide/plugins/blob/main/docs/authoring.md),
then use these references:

- [Rust example](https://github.com/openwebide/plugins/tree/main/examples/rust-plugin):
  complete manifest, source, Cargo.lock and pinned SDK checkout/build instructions.
- [Manifest and catalog schemas](https://github.com/openwebide/plugins/tree/main/schemas):
  fields, supported contributions, icons and validation limits.
- [Custom marketplace and capability reference](https://github.com/openwebide/plugins/blob/main/docs/custom-marketplaces.md):
  public Git hosting, immutable release pins, publisher CI and host API payloads.
- [SDK README](https://github.com/openwebide/openwebide/blob/main/crates/plugin-sdk/README.md), [Rust interfaces](https://github.com/openwebide/openwebide/blob/main/crates/plugin-sdk/src/lib.rs)
  and [component ABI](https://github.com/openwebide/openwebide/blob/main/crates/plugin-sdk/wit/plugin.wit): the public host contract.
- [Installation and lifecycle](plugins.md): default enablement, project opt-outs,
  capability review, Notify updates and host preparation.

The SDK remains in this app repository, with no crates.io release yet. Publisher
checks use a pinned public checkout and Cargo patch; the host supplies its SDK
when installing. Ship Rust source and a committed Cargo.lock, not an artifact URL
or dependencies on app implementation crates. Distribution builds precompile
selected official defaults for offline installation without changing privileges.

Host capabilities currently cover HTTP, clock, model completion, plugin-private
records, documented shared collections, durable jobs and raw agent runs. Grants
are scoped and capability checked. Components have bounded memory, fuel and host
calls, and no ambient filesystem, environment or network access. `workspace` is
reserved, with no adapter yet. Arbitrary shell execution, plugin MCP servers
and editor hooks remain future SDK APIs. API 5 pure language analysis and API 6
native language-server declarations are described below. Declarative workspace
panels use API 4 and the independent version-1 UI schema.
Core file editing, shell and Git tools remain built in.

Executable packages may set `contributions.context` to `false` when they have no
chat-context hook. Omission preserves the existing enabled behavior; `true`
explicitly enables it. This declaration is independent of tools, panels and
languages, so a language package can still contribute read-only chat context.
Language-only packages should opt out to avoid compiling unused hooks during
chat startup.

Validate manifest/export agreement and use fixture-based component checks before
publishing. Verify real behavior on both server and paired hosts: local projects
execute on the paired bridge, remote projects on the server bridge. Browser and
phone clients do not install or execute plugins. Follow the authoring guide's
failure, cancellation, update and duplicate-event checks. Remaining product
verification and future contracts live in [the roadmap](roadmap.md); documentation
of those plans does not make them supported contributions.

## Language plugin build toolchain

API 6 permits a language definition to declare `languageServer` with a native
`command`, `args`, `rootMarkers`, `initializationOptions` and `settings`. The host
runs the exact installed declaration, directly without a shell, in its detected
project workspace. Install the declared server and required project toolchain on
the execution host. Server-only packages need no executable component; syntax,
folding and structural handlers still use the component contract. A package can
combine both. Package languages by their server's coverage and keep unrelated
servers independently installable.

Native execution is a reviewed capability addition when upgrading older packages.
Declarations on API 5 remain inert. Requests use project-relative paths,
source digests, document/client identities, monotonic requests and revisions.
The native host negotiates UTF-16 positions and validates all edits before the
editor uses them. Navigation exposes project paths or opaque, source-bound
read-only external resources; it never gives a client arbitrary filesystem reads.
The shared language facade selects providers, prepares the exact package and
obtains fresh account/project authority above both host adapters. Providers cannot
substitute commands through query arguments or bypass installation review.

Clients may optionally send `{"operation":"prefetch","kind":"diagnostics"}`
or `{"operation":"prefetch","kind":"symbols"}` through that same protocol.
These requests have the same exact source identities and authority checks as
ordinary synchronization/document-symbol queries. The host can decline them,
limits their concurrency and duration, and cancels them for interactive work.
Diagnostics-only providers accept the diagnostics kind. No completion, edits or
commands are admitted as speculative operations. Treat rejection, cancellation
and unsupported older hosts as optional unavailability, preserving normal editing
and provider enablement. Speculation checks bounded source metadata inventories
before and after analysis, using the associations of every dialect sharing the
declared server and its project inputs. Content-based and association-free manual
languages conservatively include all files. Collection errors or limits decline
optional work; ordinary requests retain their existing behavior. A completed
request rejected for changed sources can retry, while interrupted speculation
stays suspended on its original process until restart. These observations do not
replace filesystem-event generations. This host contract does not enable an
editor queue or introduce a reusable result cache.

The macOS and Linux hosts additionally drain bounded native events for explicitly
collected files and directories. Watch installation is followed by another matching
inventory, and lost coverage invalidates the earlier generation. Descriptor caps
and collection errors decline optional work. The Windows implementation adds
explicit directory notifications with owned cancellation cleanup and handle/buffer
budgets. Its production and test code passes cross-compilation; native Windows
execution remains pending. Complete filesystem, Git, runtime/toolchain and
apply-time freshness remain roadmap work.
Required Git metadata observations use the same generation publisher, with a
reserved scan worker and notification capacity. A branch/reference round trip can
invalidate a native service even when its final branch and commit are unchanged;
retry with the current source through the ordinary facade. Index refreshes alone
do not invalidate the service. These checks do not enable reusable result caching.

API 7 adds optional `languageServer.languageId` for a server's document language
ID, preserving the package's published contribution ID. For example, a `jsx`
contribution can send `javascriptreact` to its server. Use API 7 when declaring
this field (or a newer API). Related dialects in one installed package share a service when their
command, arguments, root markers, initialization options, settings, project inputs
and virtual document declarations match;
the document language ID does not create another process. Open unsaved dialects
are synchronized with their own declarations and share request ordering. Closing
one dialect does not close the other documents or authorize another provider.

API 8 adds `languageServer.virtualDocuments`, an optional array of at most 16
read-only URI mappings. Each entry declares a unique lowercase URI `scheme`, a
custom namespaced request `method`, `parameters` (`text_document`, the default,
or `uri`) and a `textPointer` (RFC 6901, empty for a root string). For example:

```json
"virtualDocuments": [{
  "scheme": "csharp",
  "method": "csharp/metadata",
  "parameters": "text_document",
  "textPointer": "/source"
}]
```

The host sends either `{"textDocument":{"uri":"…"}}` or `{"uri":"…"}`
to the same installed server and extracts bounded source text from its response.
Only URIs returned in navigation results receive opaque resource IDs. IDs belong
to the originating source snapshot, document, editing client, project and installed
service; changing source, closing or revoking the service invalidates them. The
client cannot supply arbitrary URIs. Views remain read-only, and workspace edits
cannot target virtual documents. Standard workspace, document, window and client
methods (including command execution and applying edits) cannot be declared as
resource methods. No shell invocation, executable template or network fetch is
part of this mapping. Undeclared URI schemes fail explicitly.

API 9 adds `languageServer.role`. The default `"language"` participates in
primary-server and formatter selection. `"diagnostics"` contributes independent
lint/analysis results and fixes, with per-provider default/project enablement:

```json
"languageServer": {
  "command": "example-lint-server",
  "args": ["--stdio"],
  "role": "diagnostics"
}
```

Use `compatibility.pluginApi: 9` for a diagnostics role. These services may only
synchronize, receive save/close notifications, list code actions and resolve a
selected action. They cannot provide primary completion, formatting or navigation
through this role. Each request still prepares the installed package and acquires
ordinary project authority. Results retain the provider namespace; one service's
failure or later reply cannot replace another's diagnostics. The shared client
synchronizes related dialect buffers against that service's own declarations,
including when a different package owns primary language support.

API 9 also adds optional `languageServer.projectInputs` for configuration and
dependency files read from disk by the native service:

```json
"projectInputs": {
  "files": ["**/example.json", "dependencies.lock"],
  "exclude": ["generated/**", "vendor/**"]
}
```

Patterns are normalized project-relative paths. `*` matches within a path
segment; a whole `**` segment matches recursively. Braces, brackets, `?`,
backslashes and parent segments are unsupported. Declare 1–64 file patterns and
at most 64 exclusions, each at most 512 UTF-8 bytes. Exclusions prune directory
traversal; directory symlinks are not followed. Collection is bounded by time,
entries, file count, path bytes and content bytes and fails explicitly at a limit.
Matching disk content, creation, deletion or symlink-target changes reject an
old service's analysis; a later request starts fresh and resynchronizes buffers.
Complete observations also retain a monotonic revision for native modification
and identity metadata. A changed file can invalidate the service even when its
contents return to their previous value; unchanged metadata and excluded writes
preserve the warm service. Failed collections do not partially publish inventory
or consume revisions. Reviewed commands retain their post-edit observation and
check it again after folder waits and source verification. Omission leaves the
existing disk-input behavior unchanged. These sampled request-boundary checks do
not cover every unobserved change or replace filesystem-event generations.

API 9 can also declare persistent derived data through `languageServer.cache`:

```json
"cache": {
  "versionArgs": ["--version"],
  "environment": {"EXAMPLE_CACHE": "${cache}/index"}
}
```

The native host runs the declared server executable with `versionArgs` before
cache allocation. The probe has a five-second deadline, must exit successfully
and print 1–16,384 bytes to stdout. Declare 1–16 trimmed arguments of at most
4,096 UTF-8 bytes. The cache namespace includes this reported version, executable
identity, execution host/platform, account, project and physical folder,
provider/release, effective server declaration and declared disk inputs.
Source indexing and build freshness inside that namespace remain the server's
responsibility; this does not introduce an Open WebIDE result cache.

`${cache}` resolves in server arguments, initialization-option values, settings
values and the declared environment map. Environment names use ASCII identifier
syntax, are unique ignoring case and are limited to 32 entries of at most 128
bytes per name. Values start with `${cache}`; argument references may also use
`--flag=${cache}/index`. Templates contain one token followed by optional normal
ASCII path segments, at most 4,096 UTF-8 bytes in all. Parent segments, backslashes,
empty segments and arbitrary prefixes are rejected. Settings/options each permit
at most 4,096 nodes and 32 nested levels. A declaration must reference its cache.
Other inherited environment variables keep their normal behavior.

The execution host uses `OPENWEBIDE_BRIDGE_LANGUAGE_CACHE_DIR`, the OS cache
location or the host user's `.cache/openwebide/languages`, outside the opened
project. Installation choices, user preferences and recovery state remain in the
database. Native process leases protect these directories through confirmed
process-tree cleanup. Unconfirmed owners retain their directory; later launches
allocate a distinct physical namespace rather than taking it over after a timeout.
Allocation and confirmed retirement reclaim only idle owned namespaces, retaining
at most 32 recognized namespaces and a default four-GiB logical-byte budget.
Set `OPENWEBIDE_BRIDGE_LANGUAGE_CACHE_MIB` to a positive integer MiB host budget.
Inventories stop at 250,000 entries or four seconds; unknown directories and
symlink targets are excluded. Active or unconfirmed owners can prevent reclamation
and reject further allocations. The host also checks running caches periodically
(a ten-second sampling interval), with the same bounded inventory. It reclaims idle data first, then stops
owned producers until their sampled usage fits the budget. Shared namespaces are
counted once and all local producers of a selected namespace stop together.
An inventory failure stops local producers rather than leaving growth unchecked.
Retirement waits for confirmed process-tree cleanup before releasing storage,
even when a stopped session still retains unsaved documents for retry. Resource
readings include sampled/peak cache bytes, cache evictions and inventory failures.
Sampling permits overshoot between checks and does not impose an OS disk quota;
other bridge processes and unconfirmed owners retain their own leases.
Representative resource
measurements remain roadmap work. Only declare redirections supported by the
selected server and its toolchain; servers without an override keep their existing
behavior.

Native launches resolve the declared command against the execution host's `PATH`
and the selected workspace, preserving its launch alias. The host checks file
identity, canonical target and modification metadata for that executable and
native images observed in its process tree after initialization. Changes reject
the old service's requests and command dispatch; an ordinary retry resynchronizes
drafts into a new service. Startup inspection admits at most 256 owned processes,
64 executable paths and 128 KiB of image-path bytes. OS inspections run outside
the async worker, with a five-second deadline and at most four concurrent calls;
timed-out calls retain their capacity until the OS work actually finishes.
Transitive runtime/module dependencies and later toolchain selection still need
appropriate versioned configuration/runtime inputs and project generations.

For actions with attached commands, preparation verifies the original reviewed
context before applying files. Action synchronization then uses the exact retained
service and the reviewed post-operation disk inputs, even when a marker moved.
Ordinary analysis cannot use this transitional context. Durable discard releases
unclaimed native reviews without executing their commands. Claimed commands keep
their existing single-use dispatch and independent outcome recovery.

The host advertises LSP completion snippets, type definitions, implementations,
workspace symbol search/resolution and document/range formatting. Snippets use
bounded Rust parsing and expansion, linked placeholders, choices, variables and
regex transforms; unsupported regex syntax returns an explicit error. A completion
may request `editor.action.triggerSuggest` with no arguments as a client-only
follow-up. Other completion commands are rejected explicitly.
Code actions can return ordered text edits and file creation, movement or deletion.
An attached command must be advertised in `executeCommandProvider.commands`.
The editor reviews the whole action, including the command and arguments, then
applies its complete workspace transaction and synchronizes unsaved buffers before
claiming and dispatching the command once to its original service. Command-only
actions use the same approval journal. Unsupported combinations reject the whole
action before application. Lost replies and server errors retain an outcome for
explicit recovery; no automatic command replay or rollback of arbitrary effects
is provided. `workspace/applyEdit` remains unavailable.

Push diagnostics may omit their LSP version. Those notifications describe the
currently open document; explicit versions are accepted only when they match.
Source changes clear prior diagnostics, unknown/closed documents are ignored,
and client publication still checks source, revision, project and account.

API 5 introduces versioned language definitions and pure host syntax handlers.
Language packages use the [shared editor integration](plugins.md#language-support).
First-party packages are separated by intended language-server coverage;
related dialects share a package. Community packages use the same contract. Host analysis uses the same
component installation and export checks as tools and panels; it receives no
filesystem, process, HTTP or model grants.

Packages with C/C++ dependencies, including Tree-sitter grammars, use WASI SDK
34.0 in addition to the pinned Rust toolchain. The container distribution includes
it. For a native development or paired host, run from the app checkout:

```bash
export WASI_SDK_PATH="$(python3 tools/install_wasi_sdk.py ~/.cache/openwebide/wasi-sdk)"
```

Start the bridge with that environment. The installer chooses the supported
Linux/macOS architecture and verifies the release archive against
`tools/wasi-sdk.json`. The source compiler validates the configured SDK version,
exposes it read-only inside the compiler sandbox, and preserves offline,
read-only package compilation. Packages cannot choose another host directory or
grant themselves compiler access. Pure Rust packages can build without this
additional C/C++ toolchain.

Language source snapshots are limited to 2 MiB. Requests support full snapshots
and revision-checked UTF-8 replacements; a missing base requests a full snapshot.
Results carry the document, request, revision, language and SHA-256 source digest.
The shared service validates source rows, token coverage, folds and structural
coordinates before clients can use them. Analysis results are limited to 32 MiB
and 100,000 records. Components retain bounded, owner-isolated document state on
the host; eviction, restart and close discard that state. Cancellation, traps
and invalid results discard the affected component session. Clients retain plain
editing when analysis is unavailable.

`Structure.scopes` delegates embedded source bodies by language ID or alias.
The shared document service resolves each body using installed providers and the
same account/project preferences as standalone files. Disabled, missing,
ambiguous or failed providers leave that body plain; the outer language keeps
its own results. Source offsets, child tokens, folds, selections and bracket
partners are validated and translated before publication. Nested delegation is
bounded to eight levels and a shared source/work budget. Removed bodies and
closed documents release their component sessions.

Packages using the public syntax toolkit select their own family with
`default-features = false` and construct `LanguageSyntax::new_delegated`.
Custom grammars can supply their own `SyntaxProvider` with a neutral
`SyntaxLanguage::Plain` tag and their own public string language IDs. Parsed
contexts and declared bracket pairs supply structural editing metadata. The
optional `handle_with` callback refines ready analysis against its reconstructed
source for package-owned error recovery; shared bounds and validation still apply.
HTML and Markdown request installed providers instead of bundling other families'
parsers. Markdown's block and inline grammars run inside the same family component;
fence labels can name community languages. See the
[toolkit guide](https://github.com/openwebide/openwebide/blob/main/crates/language-syntax/README.md) for family feature names.
API 6 packages can declare `features.languageServer` to use the shared native
language-service facade in both project modes, with ordinary capability review.
The independent [Python package](https://github.com/openwebide/plugins/tree/main/plugins/python)
declares Pyright; its project configuration and virtual environments resolve on
the execution host. See [language support](plugins.md#language-support) for the
server contract and the Rust, JavaScript/TypeScript, C#, Go and C/C++ integrations.
The independent [Go package](https://github.com/openwebide/plugins/tree/main/plugins/go)
declares gopls through the same API 6 contract; the server discovers Go modules
and workspaces and shares unsaved buffers across their packages.
The independent [C/C++ package](https://github.com/openwebide/plugins/tree/main/plugins/c-cpp)
declares clangd through API 7, retaining `c` and `cpp` language IDs in one service.
Compilation flags stay in the project's compilation database or `.clangd` file.

To check a family's actual WASI component against its declared languages, run:

```bash
cargo run -p openwebide-plugin-runtime --example check_language --release -- \
  path/to/component.wasm path/to/plugin.json --family-matrix
```

The fixtures cover each declared ID, dialects, Unicode/CRLF source reconstruction,
folds and embedded delegation. CI also runs this matrix against every language
component in the compiled offline default bundle with
`tools/check-language-components.py`.
The family matrix contains fixtures for the first-party language IDs. Pass a
sample source file instead of `--family-matrix` to check other custom IDs.

A filename pattern made only of wildcards, such as `*`, acts as a last-resort
match. Exact filenames, specific patterns, extensions and first-line signatures
win over it. A missing explicitly preferred provider remains unavailable rather
than silently switching to this general contribution.

`configuration.uniformLineComments: true` declares one line-comment strategy
throughout a document, using its line marker or block-comment fallback. Such
commands stay immediate without host analysis. The default is false; structural
providers wait for current context. Results whose embedded scopes contradict a
uniform declaration are rejected. This is an explicit public contribution rule,
not a filename or first-party package exception.

Before a document's analysis arrives, the editor applies declared rules that do
not require syntax context and preserves literal input for context-dependent
operations. It does not infer embedded languages or protected regions from a
built-in browser parser. An automatic pair already inserted by the editor can
still be deleted or stepped over while the next host response is pending; that
is an editor-owned text transaction, not a new language analysis. Enter also
retains its validated closing indentation until another source edit or undo.
`surroundingPairs` apply to explicit selections without requiring analysis;
`autoClosingPairs.notIn` still guards automatic insertion at a caret. Reusing an
analysis after inverse edits requires identical source bytes and the same
document ownership, provider generation and configuration. Cancelling a consumer
does not invalidate an otherwise current immutable host result.
