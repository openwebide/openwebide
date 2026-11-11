# Open WebIDE plugin SDK

Public Rust interfaces for plugin-owned behavior executed as WASM components on
an Open WebIDE bridge host. First-party and community plugins use the same
interfaces and capability grants. API 3 covers tools/context/events; API 4 adds
declarative panels in nightly hosts. SDK 0.1.0 is
independently versioned from the app and registry publication is pending.

Implement `Plugin`, then use `openwebide_plugin_sdk::export!(YourPlugin)` to export the component
contract. Tools and event names must match the plugin manifest. The host checks
those exports before accepting an installation.

## Contributions

- `tools` and `execute`: publish JSON-schema agent tools and implement handlers.
- `context`: contribute bounded prompt text and disable the plugin's own tools
  during run planning. The default contributes nothing.
- `events` and `event`: declare and handle host callbacks, including scheduled
  jobs, periodic reconciliation and agent-run completion.
- Skills and their resources are declared in the manifest alongside executable
  contributions; they do not require a Rust handler.

Set `contributions.context: false` in the executable package manifest when no
context hook is needed. The host then skips that hook without affecting other
contributions. Omission preserves legacy invocation; `true` explicitly enables
it. Language and panel packages can contribute context through the same hook.

## Declarative panels (API 4)

Implement `panels()` to return the exact manifest panel definitions, and `panel()`
to handle their views/actions. The `panels` module provides typed version-1 UI
blocks, forms, buttons, documents and inputs. Return the next revision for each
request. Host views are read-only; actions use normal capability grants and their
declared approval policy. UI documents contain data, never browser code or markup.
See the app's [panel contract](../../docs/plugin-panels.md).

## Language contributions (API 5)

Language components serve editors and previews through the app's shared host
services; see [plugin authoring](../../docs/plugin-authoring.md#language-plugin-build-toolchain). Implement
`languages()` with the manifest's exact `LanguageDefinition` values and
`language(LanguageInput)` with version-1 `LanguageOutput` data. `export!` supplies
the reserved metadata/service dispatchers; do not declare them as model tools.
Contribution language IDs are strings. `SyntaxLanguage` tags belong to the
optional parser toolkit and do not restrict community language IDs or engines.

Definitions contain recognition, comments, brackets, conditional automatic pairs,
surrounding pairs and indentation rules. Block comments explicitly declare
whether they can nest. Data-only packages can contribute a subset without an
executable. API 5 formatting, snippet, diagnostics and language-server metadata
alone does not implement those services. Native server execution requires the
newer contract below; it does not grant the syntax component host capabilities.

Syntax handlers run only on the execution host, with no host capability grants.
Requests identify an opaque client document, request, revision, language, path,
tab width and SHA-256 source digest. Sources use full snapshots or UTF-8 byte
replacements with an exact base revision. Return `NeedsSource` after cache loss;
the shared facade retries once with a full snapshot. Never claim incremental
parsing when performing a full lexical pass.

Results contain per-row token end offsets, folds and optional source-coordinate
structure, never replacement source text. Keep token rows lossless and ordered,
scope/protected regions ordered and nonoverlapping, and bracket partners
reciprocal. Empty embedded scopes retain insertion rules. Limits are 2 MiB of
source, 32 MiB of result JSON and 100,000 analysis records. Hosts may destroy a
component on close, cancellation, failure, update or bounded idle eviction;
component memory is disposable, not plugin configuration or persistent storage.

The [syntax toolkit](../language-syntax/README.md) is optional. A custom language
can implement this same protocol directly, including a different grammar or a
pure Rust lexer. Browser clients only render validated results and apply
declarative rules.

## Native language services (APIs 6–9)

API 6 opts a language into its declared native server command and ordinary
process-access installation review. API 7 adds per-document `languageId` values
for related dialects sharing a service. API 8 adds declared, read-only virtual
source requests. API 9 adds optional `LanguageServer.project_inputs` (JSON
`projectInputs`) for bounded configuration/dependency file patterns checked at
native request boundaries. See the authoring guide for pattern syntax, limits
and remaining project-generation work. It also adds `LanguageServerRole::Diagnostics` (`"diagnostics"` in
JSON), independent of the default primary `Language` role. Diagnostics providers
have separate user/project enablement, preserve result provenance, and may supply
fixes through code-action review. They do not replace primary completion,
formatting or navigation providers.

API 9 also adds optional `LanguageServer.cache`: a bounded `versionArgs` probe
and `environment` map of native cache paths. `${cache}` references in arguments,
initialization options and settings resolve to the same derived-data directory
outside the project. This is ordinary public metadata, with the same process
approval and lifecycle for bundled and community packages. See the authoring
guide for namespace ownership, limits and supported path templates.

Servers execute only on project execution hosts. The shared facade owns provider
selection, preparation, fresh project authority and unsaved-buffer synchronization
above both transports. Host clients validate source-bound results and render
completion snippets, document/selection formatting, symbols and diagnostics;
packages do not inject editor scripts. See the full
[native service contract](../../docs/plugin-authoring.md#language-plugin-build-toolchain)
for bounds, supported methods and explicit unsupported operations.

## Host capabilities

Declare required capabilities in `executable.capabilities`. Every host call is
checked against the invocation's grants; the generic `request` function does not
provide additional authority.

| Capability | SDK entry point | Operations |
| --- | --- | --- |
| `http` | `http` | Bounded HTTP requests to public and LAN services. |
| `clock` | `request("clock", ...)` | Read host time. |
| `completion` | `complete` | Bounded text generation using the configured primary or fast model, with tools disabled. |
| `records` | `records` | List, read, create, update and delete plugin-private records. |
| `collections` | `collections` | Supported app-visible data collections with schema and revision validation. |
| `jobs` | `jobs` | List, read, schedule, cancel and delete durable event jobs. |
| `runs` | `runs` | List, read, submit, cancel and delete durable agent-run records. |

App collections currently include memories, skills, owned tasks and task-run
history, plus read-only conversation and public configuration metadata. Access
is limited to the authenticated account and applicable project/conversation
scope. Private records also belong to the originating plugin source; plugins
cannot choose another account or another plugin's namespace.

Context hooks may read the clock and list/read records, collections, jobs and
runs. They cannot mutate data, make HTTP requests or request completions. Prompt
contributions are bounded by the host's context budget.

Agent submissions use the normal agent execution and permission system. HTTP
does not provide direct access to authenticated Open WebIDE control APIs.
Capability additions in updates require review, including automatic updates.

`workspace` is reserved in the manifest format and has no implemented host
adapter yet. Language servers and editor hooks remain future contracts; language
analysis and declarative panels use the contracts above.

## Execution and authoring

Components have bounded memory, fuel, messages, host calls and invocation time.
They receive no filesystem preopens, environment variables or direct network
access. Feature policy, prompts, parsing and result handling belong in plugin
source; the host provides general primitives and enforces scope and limits.

Marketplaces publish Rust source and Cargo.lock at an immutable Git commit.
The host supplies the SDK, compiles in isolation and caches validated WASM by
source, SDK and toolchain. Authors can test against a public app checkout through
the marketplace's `scripts/validate_rust.py` checker. See the
[Rust authoring example](https://github.com/openwebide/plugins/tree/main/examples/rust-plugin)
and [custom marketplace guide](https://github.com/openwebide/plugins/blob/main/docs/custom-marketplaces.md).


Start with the [plugin authoring guide](https://github.com/openwebide/plugins/blob/main/docs/authoring.md)
for creating a package, locked builds, export validation, fixture execution,
installation testing and publishing. The
[capability reference](https://github.com/openwebide/plugins/blob/main/docs/custom-marketplaces.md#rust-source-plugins)
documents request/response shapes and lifecycle semantics. Rust API types are in
[src/lib.rs](src/lib.rs); the component ABI is in [wit/plugin.wit](wit/plugin.wit).
SDK source lives here; marketplace sources, schemas, examples and publisher CI
live in `openwebide/plugins`. No separate SDK repository is needed.

The current workflow patches the exact `openwebide-plugin-sdk = "=0.1.0"`
dependency to a pinned public app checkout during author checks. Installation
supplies the host's embedded SDK. Do not replace it with a published dependency
on app internals or assume a crates.io release exists.
