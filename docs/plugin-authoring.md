# Plugin authoring

Current alpha hosts execute plugin-owned Rust compiled to WASM on the bridge.
API 1 supports skills; API 3 adds tools, read-only context hooks and durable events.
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
services, editor contributions and declarative UI panels are not current SDK APIs.
Core file editing, shell and Git tools remain built in.

Validate manifest/export agreement and use fixture-based component checks before
publishing. Verify real behavior on both server and paired hosts: local projects
execute on the paired bridge, remote projects on the server bridge. Browser and
phone clients do not install or execute plugins. Follow the authoring guide's
failure, cancellation, update and duplicate-event checks. Remaining product
verification and future contracts live in [the roadmap](roadmap.md); documentation
of those plans does not make them supported contributions.
