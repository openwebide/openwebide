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
