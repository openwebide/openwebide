# Open WebIDE plugin SDK

Public Rust interfaces for plugin-owned behavior executed as WASM components on
an Open WebIDE bridge host. First-party and community plugins use the same
interfaces and capability grants. Plugin API 3 ships in current alpha hosts;
remaining lifecycle checks are tracked in the app roadmap. SDK 0.1.0 is
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
adapter yet. Language services, editor hooks and declarative UI panels remain
future contribution contracts.

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
