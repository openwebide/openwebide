# Development

[Build and run from source](../README.md#run-from-source), then use the
[development commands](../README.md#development) to run native and browser tests.
Repository working conventions live in `AGENTS.md` at the repository root.

| Reference | Purpose |
| --- | --- |
| [Architecture](architecture.md) | Components, transports, shared behavior and data flow. |
| [API examples](api-examples.md) | HTTP requests and response examples. |
| [Panel playground](panel-playground.md) | Live component examples, JSON validation and Rust exports. |
| [Design system](design-system.md) | Shared components, visual tokens and interaction patterns. |
| [Editor performance](editor-performance.md) | Performance baselines and measurement procedures. |
| [Project website](project-site.md) | Build, organize and publish these docs. |
| [Execution bridge](execution-bridge.md) | Native runtime and browser/host integration. |

When adding behavior, keep the shared feature implementation above local and
remote adapters. Update its guide and [changelog](../CHANGELOG.md), and leave any
unfinished mode support explicit in the [roadmap](roadmap.md).

## Replaying a chat turn

The browser component tests can replay a `RunEvent` sequence through the same
composer, stream handler and transcript as a normal remote send. Queue the
events in `FakeBackend::scripted_events`. For intermediate assertions, supply a
receiver in `scripted_event_permits` and release one event at a time. This makes
split deltas, approvals and tool transitions reproducible without timing sleeps
or a running inference server. The local and remote indicator tests exercise the
shared transcript state.

```sh
cargo test -p openwebide-frontend --release --target wasm32-unknown-unknown --test components streaming::
```

Use the pinned Chrome and ChromeDriver for these tests. For transport and
approval replay, the `runs::lost_queued` cases drive the real Chat pane through
the bridge messages for a disconnect, retry and completed run. The bridge test
`reconnect_during_approval_executes_the_command_once` sends duplicate decisions
over a real socket and checks the command's file output.

For wider transport and reconnection coverage, use production browser checks with a scripted model on
the actual execution host. The
[agent language check](https://github.com/openwebide/openwebide/blob/main/tools/check-agent-language-client.py) is an example of
driving both project modes this way.
