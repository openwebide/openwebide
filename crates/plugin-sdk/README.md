# Open WebIDE plugin SDK

Public Rust interfaces for plugin-owned behavior executed as WASM components on
an Open WebIDE bridge host. First-party and community plugins use the same
interfaces and capability grants. Plugin API 3 is still undergoing deployment
verification; SDK registry publication is pending.

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
[Rust authoring example](https://github.com/openwebide/plugins/tree/plugin-rust-runtime/examples/rust-plugin)
and [custom marketplace guide](https://github.com/openwebide/plugins/blob/plugin-rust-runtime/docs/custom-marketplaces.md).
