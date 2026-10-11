# MCP tools

Open WebIDE can start Model Context Protocol (MCP) servers on an execution host
and expose their tools to the agent. Servers run as child processes over standard
input/output. Install the server and its runtime on the host first; Open WebIDE
starts the configured executable directly, without a shell.

The same client serves remote projects and paired hosts for local projects.
The browser never starts an MCP process. For a local project, connect its execution
host and configure the project’s host folder mapping before using project-specific
MCP configuration. Host-global configuration is available without a project folder.

## Configuration

Put host-wide configuration in `~/.openwebide/mcp.json`. Set
`OPENWEBIDE_MCP_CONFIG` to choose another file. The container image uses
`/app/.spin/mcp.json`, inside the persistent application-data volume.

A host project can also contain `.openwebide/mcp.json`. Project entries override
host-wide entries with the same server name. A disabled project entry can turn off
a host-wide server. Server names use ASCII letters, digits and hyphens, up to
32 characters.

```json
{
  "mcpServers": {
    "docs": {
      "command": "/opt/mcp/docs-server",
      "args": ["--stdio"],
      "env": {
        "DOCS_DIRECTORY": "/srv/documentation"
      },
      "disabled": false
    }
  }
}
```

Commands inherit the execution host’s environment; `env` overlays specified
variables. They start in the project’s mapped host directory, or the execution
host’s workspace root for project-less runs. Keep credentials out of committed
project configuration. Remote HTTP MCP servers are not supported by this client.

## Discovery, approvals and lifecycle

At run preparation, the host starts enabled servers, negotiates the protocol and
reads their tool lists, including pagination. Tool names appear as
`mcp__docs__tool_name`. The run pins that catalog; a server cannot change a tool’s
identity or schema midway through execution. Model connections configured for
chat only or with tools disabled do not use MCP tools.

MCP calls follow the agent’s normal approval policy. Server annotations do not
automatically grant permission. Approval previews identify the server and arguments.
Calls return MCP content and error status to the model. File editing, shell and
other core workspace tools retain their existing behavior.

Servers that fail discovery are reported as unavailable in the run’s diagnostics;
other successfully discovered servers remain usable. Configuration errors do not
silently become an empty tool list. Discovery, message sizes, tool counts and calls
have bounded limits and deadlines. A broken connection is closed. A subsequent
call may start a new process only after its discovered tools match the pinned
catalog; a failed call is never automatically replayed.

Processes belong to the run’s host catalog. Run teardown closes the catalog and
terminates its processes, including descendants. Abandoned catalogs expire after
an hour without use, and bridge shutdown closes all catalogs. Authenticated bridge
requests bind catalogs to their owning account or local pairing.

## Troubleshooting

- Check that `command` exists inside the execution host or container, and that its
  runtime and dependencies are installed there.
- Ensure standard output contains newline-delimited JSON-RPC only. Send logs to
  standard error; this client does not display server stderr.
- Check host/project configuration for unsupported fields or malformed schemas.
- Connect a local project’s paired host and verify its host folder mapping.
- If tool definitions changed after a server restart, start a new agent run.

Plugin-declared MCP contributions and deferred tool loading remain separate roadmap
work. This feature uses explicitly configured host servers; it does not add MCP
fields to the plugin manifest.

Protocol reference: [modern versioning and compatibility](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning)
and [legacy initialization](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle).

## Deferred tool loading

Large tool sets use the shared `tool_search` mechanism in both remote and paired-host
runs. When optional schemas exceed 10% of a known model context limit, the agent
initially sees a bounded list of names and a search tool instead of every full schema.
Keyword search covers the whole authorized catalog, including names omitted from
the initial list. File
reading/writing, directory listing, workspace search and shell execution stay loaded.
Small sets and models with unknown limits keep the original behavior.

The agent can search names/descriptions with keywords or use `select:` followed by
up to three exact names. Matching schemas become available on the next model turn;
a call bundled into the same response as discovery cannot bypass that boundary.
Older optional schemas rotate out when the active tool schemas exceed a quarter
of the model context limit. A single selected schema remains subject to the normal
context-budget checks. Search again for a tool that is no longer listed.

Discovery only indexes the run's existing, granted tools. It does not start more
servers, install plugins or grant permissions. MCP and plugin calls still pass
through their ordinary approval checks. Each run, including delegated child runs,
owns its loaded schemas; new or resumed runs rediscover tools from their current
authorized catalog.
