# Code intelligence

Language support is supplied by independently installable public plugins, grouped
by the language server they use. Bundled packages use the same SDK, artifact,
permissions and lifecycle as community packages. A missing or disabled provider
leaves ordinary editing available.

The bundled server integrations cover Rust, JavaScript/JSX/TypeScript/TSX, Python,
Go, C/C++, C#, Terraform/HCL, HTML, CSS/SCSS and JSON/JSONC. Related dialects share
one package and service. Terraform owns its HCL syntax through the public grammar
toolkit; its seven dialects are checked without initializing infrastructure.
See the [language contribution contract](plugins.md#language-support) for package
declarations, supported toolchains and extension points.

## Editing and analysis

Both Local and Remote editors share diagnostics, hover, completion, definitions,
type definitions, implementations, references, document outlines and workspace
symbol search. Completion supports snippet fields and additional import edits.
Formatting supports whole documents and selections; rename and code actions use
the normal review and undo workflow. Independently enabled analysis providers own
their diagnostic labels and fixes. Provider failure or disablement preserves drafts.
Each operation remains subject to its server's capabilities; Pyright, for example,
does not supply formatting.

Reviewed workspace edits include ordered file creation, movement and deletion.
The complete plan and affected drafts are validated against the original language
result before native execution. Commands require their own approval. Interrupted
operations retain durable ownership, original file identities and recovery state;
uncertain command effects require acknowledgement and are never blindly replayed.
Recovery preserves independent unsaved drafts rather than saving them to disk.
Browser-only Local saves remain available without the companion and are outside
its physical folder-locking guarantee.

Agent diagnostics, symbols, definitions and references use the same language
service facade with runtime account/project authority and installed providers.
Agent queries read saved workspace sources and keep their source inventory
separate from each editor's unsaved overlays. Mutations retain normal approval.
See [editor controls](editor.md#syntax-and-language-behavior) and
[native recovery](execution-bridge.md) for user-facing workflows.

## Reuse, freshness and resource limits

Suitable native services stay warm across reloads and client handoff. Separate
clients retain independent complete source inventories; active work and reviewed
commands pin their original owner. Native process-tree RSS sampling drives bounded
service eviction and backoff. Pressure recovery verifies that the original process
tree exits and that both clients can restore their different drafts.

Reusable state binds account, project, host, installed provider/server version,
effective configuration, source and declared dependency inputs. Project generations
reject stale results after source, Git, configuration, dependency, plugin or host
changes. Providers declare runtime inputs through public API 10; undeclared private
server reads are outside that declaration's visibility.

Supported Rust and Go derived caches live outside workspaces in owned namespaces.
Retention and sampled growth checks protect active owners and reclaim idle data.
The recorded C# relocation compatibility failure is preserved, and clangd indexing
remains enabled because its installed version has no supported arbitrary cache
relocation. No OpenWebIDE result cache was added.

Optional diagnostics/symbol prefetch is off by default. It uses the current
client's warm service, bounded fresh reads of open, recently closed and related
sources, one speculative slot, cancellation and interactive priority. Speculative
results never directly alter editor content or history.

These are sampled resource policies, not instantaneous OS memory or disk quotas.
The finite homelab measurements cover 128/512-module TypeScript workspaces,
64 reloads, 128 handoffs and four pressure recoveries across native macOS and a
constrained Linux application. Cache probes record mixed cold/restart latency;
Go retained-file reuse is not a general hit-rate counter. The constrained ABBA
prefetch measurements establish no general speedup, so prefetch remains optional.
See [service measurements](code-intelligence/service-measurements.md),
[process measurements](code-intelligence/owner-process-measurements.json) and
[prefetch measurements](code-intelligence/idle-prefetch-measurements.json).

## Verification

The shared `LanguageService` implements provider selection, source synchronization,
authority and result validation above browser, native and WASI transports.
Editor actions and read-only agent tools call that facade. The same behavioral
contracts cover both project adapters, including reconnect, concurrent overlays,
cancellation, invalidation, eviction and recovery.

The installed macOS Chrome PWA also exercises real native folders. After an actual
rename, the owned native process is stopped and killed, folder access is removed,
and a replacement host reopens the same durable recovery namespace. Local browser
File editing denial and Remote OS permission denial both block saves while
preserving drafts. Restoring original access and choosing recovery restores the
original inodes, external hard links, modes, timestamps, ACLs, extended attributes
and CRLF bytes, together with both unsaved Unicode drafts.
These checks establish the recorded Chrome cases; editor IME, assistive technology,
Edge and phone-device checks remain separate roadmap work.

[Physical records](code-intelligence/physical-folder-checks.json),
[hosted records](code-intelligence/hosted-checks.json) and the
[checkpoint history](code-intelligence/checkpoint-history.md) preserve original
failures, actual source identities and the limits of each observation.
