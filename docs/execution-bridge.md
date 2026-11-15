# Execution bridge

The native bridge daemon (`openwebide-bridge`) runs on the host to provide interactive PTY terminals, process execution (`POST /exec`), and host Git operations for the web frontend and coding agents. The backend reaches the bridge at `127.0.0.1:3001` and sends project-relative `cwd`.

> [!NOTE]
> Git operations performed by the bridge require Git ≥ 2.23 on the host machine for branch switching via `git switch`.

Enabled language plugins run their native servers on this host. For C/C++, install
clangd and the project's compiler/SDK, and make `clangd` available on the daemon's
PATH. Native archive users install language servers and toolchains separately;
published container images include clangd and C/C++ build tools. See the
[language support guide](https://github.com/openwebide/openwebide/blob/main/docs/plugins.md#language-support)
for project configuration and other language requirements.

The bridge's HTTP/1 and WebSocket server (built on `hyper`) enforces request limits: a 16 KiB
HTTP request head, a 1 MiB `/exec`/Git request body, a 16 MiB WebSocket message/frame, and 256
concurrent connections. Idle sockets are closed after 10 s without a request head; WebSocket
connections are pinged every 30 s and closed after 90 s without an inbound frame.

## Running the bridge

```sh
cargo build -p openwebide-bridge
./target/debug/openwebide-bridge --port 3001 --workspace ../.. --host 127.0.0.1 \
  --backend-url http://127.0.0.1:3000/api --secret-file /tmp/openwebide-bridge-secret
```

Signed-in connections can run chat and agents over the bridge when it shares the
backend's secret. The frontend selects WebSocket runs when `hello_ok` advertises
run support. In browser DevTools, prompts send `run_start` and replies arrive as
`run_event` frames. Local agents remain in the browser and request model
completions through `completion_start`.

HTTP-only builds (`cargo build -p openwebide-bridge --no-default-features`)
advertise no run support; terminal and Git operations remain available.
See [chat execution and fallback](architecture.md#chat-execution-and-streaming)
for transport selection, and [reload recovery](reload-recovery.md) for reconnect
and interrupted-run behavior.

- `--backend-url <URL>` (or env `OPENWEBIDE_BRIDGE_BACKEND_URL`): backend API URL for run plans and persistence (default: `http://127.0.0.1:3000/api`); set it on the daemon, never in a client request. A frontend `?api=` development override needs a matching `--backend-url`.
- `--workspace <DIR>` (or env `OPENWEBIDE_BRIDGE_WORKSPACE`): sets the workspace root directory for command execution and repository operations (defaults to the current working directory). This directory must match the `files` source mount used by the Spin backend (e.g. `~/source` locally or `/workspace` in Docker). All `cwd` arguments passed to the bridge are evaluated relative to this root, and requests escaping the root are rejected (lexical confinement).
- `-p, --port <PORT>` (or env `OPENWEBIDE_BRIDGE_PORT`): port to bind on (default: `3001`).
- `--host <HOST>` (or env `OPENWEBIDE_BRIDGE_HOST`): host address to bind on (default: `127.0.0.1`). To expose the bridge to your local network (e.g. phone/tablet use over LAN or Tailscale), bind to `0.0.0.0` or a specific LAN IP:
  ```sh
  ./target/debug/openwebide-bridge --host 0.0.0.0 --port 3001
  ```
- `--secret-file <FILE>`: persistent shared-secret file for non-browser API commands. `OPENWEBIDE_BRIDGE_SECRET` overrides the file and must contain at least 32 characters. Otherwise a persistent random 32-byte secret is stored at `--secret-file`, `$XDG_CONFIG_HOME/openwebide/bridge-secret`, or `~/.config/openwebide/bridge-secret`. The backend lazily fetches it over loopback via `POST /secret` and caches it in SQLite.
- `--token <TOKEN>` (or env `OPENWEBIDE_BRIDGE_TOKEN`): Optional pairing token (≥ 16 characters) to authenticate local companion apps without needing the backend's minted session tokens. Allows local-mode execution: `openwebide-bridge --token <16+ chars> --workspace <path>`.

> [!WARNING]
> When the backend reaches a remote bridge through a non-loopback URL, set both `SPIN_VARIABLE_BRIDGE_URL=http://<bridge-host>:3001` and `SPIN_VARIABLE_BRIDGE_SECRET` to the bridge secret from its secret file or `OPENWEBIDE_BRIDGE_SECRET`. The daemon logs the secret source, never the secret itself. Binding to `0.0.0.0` still permits automatic bootstrap when the backend connects over loopback.

## File writes

Backend file API and SSE-agent writes use `HostFsVfs` and the shared core
`FileMutation` dispatch. With the default `workspace_write_policy=coordinated`,
`POST /workspace/write` requires backend service authentication and delegates to
the same directory-bound `WorkspaceFiles` and folder/recovery coordinator as
native tools. User, paired-browser and plugin credentials cannot call this route.
Its bounded JSON envelope carries at most one `MAX_READ_BYTES` file as base64;
malformed or oversized contents are rejected before mutation. File operations
retain native exclusion through accepted blocking I/O, including observer loss.
Busy recovery and uncertain replies return API conflicts without a direct retry.

`SPIN_VARIABLE_WORKSPACE_WRITE_POLICY=direct` explicitly selects WASI mutations
outside native folder locking. Container startup selects it when
`OPENWEBIDE_BRIDGE=0`, unless the operator sets a policy explicitly. This keeps
companion-disabled file saves and SSE tools available; it is never an automatic
fallback after a native failure. Direct writes hold an opened project directory
for each operation and reject links escaping it. Optional atomic replacement,
nonrecursive removal and atomic movement remain unsupported in this mode.
Browser-only Local saves also remain outside native locking.

The [backend writer checks](code-intelligence/backend-writer-checks.json) cover
real browser/WASI workflows in both modes, a distinct project alias to the same
physical folder, and native/direct/disabled-container policies. They do not
establish complete durable writer coverage, native Windows behavior or optimized
full-distribution verification. A focused Linux follow-up passes 48 filesystem and
two authenticated HTTP cases, including directory replacement, runtime shutdown,
ACLs and extended attributes on a persistent Linux volume.

Ordinary file writes, commands, Git mutations and folder proofs can coexist.
Reviewed resource and server-command operations exclude overlapping activity,
including nested paths and physical aliases. Within each coordinator, waiting
reviews stop later ordinary work from bypassing them. Accepted file I/O retains its claim until completion.
Healthy acknowledged background jobs retain an ordinary claim, so they no longer
hold up saves. Uncertain cleanup quarantines the original directory across
coordinators and processes while leaving unrelated siblings available.
The [ordinary admission checks](code-intelligence/ordinary-admission-checks.json)
cover macOS/Linux contracts, a normal Linux background job, older-companion
rejection and both built-app transports. Windows is cross-compiled only.

## Native recovery after an issuer is decommissioned

On Linux and macOS, run recovery administration as the same OS account as the
bridge. For Remote projects, run it on the execution host; for Local projects,
run it on the companion host. Both use the same native records and policy.
Prefer normal app recovery when its original project and issuer are available.

```sh
openwebide-bridge recovery inspect
openwebide-bridge recovery retire <ID> --expected <DIGEST> --note "Reviewed the original effects; issuer decommissioned"
```

Inspect lists original owners, pending settlement metadata and prior retirement
receipts without printing their private credentials. It never initializes an
absent namespace. Review the original file/command effects and copy that record's
`id` and `expected` values into the retirement command. A changed record requires
another inspection. Retirement acknowledges those effects; it does not undo files
or mark original backend records settled. Start a new reviewed operation when you
use the folder again; its retired operation cannot resume.

A fencing owner requires its exact original physical folder and the same exclusive
folder gate used by native writers. A worker or watcher that retains that claim prevents retirement.
Dispatched commands need their original positive cleanup receipt; missing public
rows, failed callbacks and inaccessible folders cannot supply one. Legacy commands
without a cleanup record remain blocked. A non-fencing settlement outbox can be
retired without a folder or backend connection, because command cleanup and native
retirement already finished before it was queued.

Retirement atomically replaces the original native record with an audit receipt
and a permanent transaction fence. The receipt retains the inspected context and
original result or final result digest, alongside the review note. Repeating the same reviewed request is
idempotent; stale or changed requests fail. The audit is bounded to 1024 records
and the existing private database quota, and never evicts a fence automatically.
Ordinary admission upgrades the native namespace to schema 11 without replacing
the original database or losing recovery records. Retirement preserves that
version. Upgrade older companions before using the same namespace; older
binaries reject its newer format.
No HTTP endpoint or profile/namespace override grants this OS-owner operation.
The [Linux CLI contract](code-intelligence/linux-recovery-administration-checks.json)
passes across fresh containers with the installed OS policy, including a late
original actor and ordinary HTTP writing after retirement. It uses current dev
binaries mounted into the runtime-state image. Optimized full-app distributions,
deleted-issuer application workflows and remaining platform checks are tracked in
the [roadmap](roadmap.md). The
[native deleted-project contract](code-intelligence/deleted-project-command-checks.json)
uses a real dispatched command and SQLite project deletion in both modes. A lost
settlement acknowledgement preserves the native fence after deletion; explicit
review retains its original result and restores ordinary writing. That contract
uses the shared CLI executor with a test-selected home. Its
[built-app follow-up](code-intelligence/built-deleted-project-checks.json) now verifies
browser command approval and authenticated project deletion through the real WASI
API in both modes. A fresh native host receives 404 for the original callback and
retains the fence until explicit retirement. The shared executor then preserves
the original result and restores actual file writing. This still uses an isolated
test home and Local OPFS correspondence; the OS-default CLI in that sequence,
optimized native/WASI distributions and physical device checks remain open.
The initial workflow closes the page before deletion. A
[follow-up](code-intelligence/browser-reload-deletion-checks.json) now reloads the
page and leaves it active through deletion and native restarts in both modes,
with a stable companion address and exact file snapshots at every stage. The
[original-page case](code-intelligence/original-page-deletion-checks.json) also
keeps the approval document running without reload. Both modes preserve every
original file observation and the exact audit while ordinary writing resumes
only after explicit retirement. The
[account decommission check](code-intelligence/account-decommission-checks.json)
then removes the sole isolated fixture account administratively, with a database
backup and foreign-key enforcement. After backend restart the old cookie receives
401, while the original native context/result and fence remain until explicit
recovery. This verifies recovery without an app account; it adds no account-delete
API. A [normal Linux CLI follow-up](code-intelligence/normal-cli-application-checks.json)
now runs that same two-mode application sequence with the normal companion and
`recovery` entry point, without test-support features or a helper-selected home.
Fresh containers use the shipped root-owned OS policy and the same private
storage. The browser and real WASI backend run on the Mac; the normal Linux
companion uses its development profile. Optimized full distributions and the
remaining platform/physical checks remain unfinished.

## Host and Origin security baseline

To protect against DNS rebinding and malicious websites opened in the user's browser, the bridge validates incoming HTTP and WebSocket requests:

1. **Host Header Rules:**
   - Allowed if the `Host` is an **IP literal** (IPv4 or IPv6, e.g. `127.0.0.1`, `[::1]`, `192.168.1.50`).
   - Allowed if `localhost`, this machine's hostname (e.g. `mymachine`), or `<hostname>.local`.
   - Allowed if explicitly added via `--allowed-host <HOSTNAME>` (or `OPENWEBIDE_BRIDGE_ALLOWED_HOSTS` comma-separated list).
   - Any unrecognized or rebinding domain name is rejected with `403 Forbidden`.

2. **Origin & CORS Rules:**
   - Requests without an `Origin` header (such as Spin backend calls, `curl`, and local daemon tools) are permitted, but API routes require authorization via `Authorization: Bearer <SECRET>`. For example:
     ```sh
     curl -X POST http://127.0.0.1:3001/exec \
       -H "Authorization: Bearer <your-secret>" \
       -H "Content-Type: application/json" \
       -d '{"command": "echo test"}'
     ```
   - Browser requests with an `Origin` header are permitted only if:
     - The origin is in the allowed origins list (default: `http://localhost:3000`, `http://127.0.0.1:3000`, `http://localhost:8080`, `http://127.0.0.1:8080`, plus any `--allowed-origin` entries); **or**
     - The origin's hostname matches the request's `Host` hostname (allowing phone/LAN access when the frontend and bridge are accessed on the same host machine).
   - `Origin: null` and untrusted cross-origin requests are rejected with `403 Forbidden`.
   - Wildcard `Access-Control-Allow-Origin: *` is disabled; allowed origins receive their exact origin echoed with `Vary: Origin`.

3. **JSON-Only Browser POSTs:**
   - Browser POST requests carrying an `Origin` header require `Content-Type: application/json`; simple browser requests (e.g. `text/plain`, form-urlencoded) are rejected with `415 Unsupported Media Type` to prevent browser CSRF.

## Process lifecycle

Every command the bridge spawns (`/exec`, `run_command`, PTY shells, Git subprocesses) starts in its own process group. Signals target that group; interactive shells can put background jobs into separate groups, which may survive shell cleanup:

- **Kill semantics:** sending `Kill` with no signal (or `KILL`/`SIGKILL`) terminates the whole process group immediately. `TERM`/`SIGTERM` and `HUP`/`SIGHUP` signal the group directly. `INT`/`SIGINT` on a PTY session instead writes `^C` to the terminal, matching a real Ctrl+C — it interrupts whatever's in the foreground rather than killing the shell itself.
- **Timeouts and cancellation:** a `run_command`/`/exec` timeout or cancellation terminates the command's process group (SIGTERM, then SIGKILL after a 2 s grace period). App HTTP command requests set `cancel_on_disconnect: true`, so a read EOF also cancels their work. The flag defaults to false for clients that intentionally half-close their write side and still expect a response; EOF alone cannot distinguish them from a fully closed client. Connection resets drop the observer in either case.
- **Output capping:** `/exec` keeps the first 256 KiB and last 768 KiB of each of stdout/stderr per stream, with an omission marker in between, so a runaway command can't exhaust memory.
- **Session reaping:** exited terminal/process sessions are removed 30 minutes after they exit, freeing their output buffers; running sessions are never reaped, however long they've been open.
- **Graceful shutdown:** on Ctrl+C or `SIGTERM`, the bridge stops accepting new connections and terminates every session's process group (with the same grace period) before exiting.

On Linux and macOS, configured `/exec` and agent commands also acquire the shared
physical folder gate. An independent watcher receives the original directory locks
before starting the command, so bridge loss cannot release them while the managed
group still runs. Cleanup retains the original unreaped child identity until the
group is quiet, then waits for the group to disappear before releasing exclusion.
Unconfirmed cleanup blocks overlapping native work. Successful acknowledged
commands preserve background work; their watcher retains an ordinary folder claim
until it ends. Ordinary saves can proceed, while reviewed work waits for that
claim. Unconfirmed cleanup blocks both classes of overlapping activity.
This covers the original managed group. Escaped groups, PTY/Git process ownership,
Windows ownership and complete durable filesystem authority remain unfinished.


[Back to the README](../README.md).

## MCP processes

The bridge also owns configured [MCP servers](mcp.md). Shared agent code handles
configuration validation, discovery, schema translation, approval previews and
call results. The native bridge provides filesystem, process, stdio and timeout
primitives. Both server runs and browser runs through paired hosts use this same
policy and immutable, authenticated catalog contract.
