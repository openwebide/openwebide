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
HTML and Markdown request installed providers instead of bundling other families'
parsers. Markdown's block and inline grammars run inside the same family component;
fence labels can name community languages. See the
[toolkit guide](https://github.com/openwebide/openwebide/blob/main/crates/language-syntax/README.md) for family feature names.
LSP execution remains the following roadmap item.

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
