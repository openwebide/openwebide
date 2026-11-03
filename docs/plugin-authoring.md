# Plugin authoring

Current nightly hosts execute plugin-owned Rust compiled to WASM on the bridge.
API 1 supports skills; API 3 adds tools, read-only context hooks and durable events.
API 4 adds [declarative workspace panels](plugin-panels.md) and scoped view/action handlers.
The [panel playground](panel-playground.md) previews the production components
and exports validated documents and Rust SDK examples without running host code.
The SDK is version 0.1.0, independently of the app. API 2 tool groups are retired.
First-party Web, Project Memory, Scheduling and Skill Authoring implement their
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
reserved, with no adapter yet. Arbitrary shell execution, MCP servers, language
services and editor contributions are not current SDK APIs. Declarative workspace
panels use API 4 and the independent version-1 UI schema.
Core file editing, shell and Git tools remain built in.

Validate manifest/export agreement and use fixture-based component checks before
publishing. Verify real behavior on both server and paired hosts: local projects
execute on the paired bridge, remote projects on the server bridge. Browser and
phone clients do not install or execute plugins. Follow the authoring guide's
failure, cancellation, update and duplicate-event checks. Remaining product
verification and future contracts live in [the roadmap](roadmap.md); documentation
of those plans does not make them supported contributions.

## Language plugin build toolchain

API 5 introduces versioned language definitions and pure host syntax handlers.
The editor migration and bundled language packages remain tracked in the
[roadmap](roadmap.md#language-support-as-plugins). Host analysis uses the same
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
