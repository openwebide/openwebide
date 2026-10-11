# Execution bridge

The native bridge daemon (`openwebide-bridge`) runs on the host to provide interactive PTY terminals, process execution (`POST /exec`), and host Git operations for the web frontend and coding agents. The backend reaches the bridge at `127.0.0.1:3001` and sends project-relative `cwd`.

> [!NOTE]
> Git operations performed by the bridge require Git ≥ 2.23 on the host machine for branch switching via `git switch`.

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
- **Timeouts:** a `run_command`/`/exec` timeout, or the client disconnecting mid-request, terminates the command's process group (SIGTERM, then SIGKILL after a 2 s grace period) instead of leaving it (and any children) running.
- **Output capping:** `/exec` keeps the first 256 KiB and last 768 KiB of each of stdout/stderr per stream, with an omission marker in between, so a runaway command can't exhaust memory.
- **Session reaping:** exited terminal/process sessions are removed 30 minutes after they exit, freeing their output buffers; running sessions are never reaped, however long they've been open.
- **Graceful shutdown:** on Ctrl+C or `SIGTERM`, the bridge stops accepting new connections and terminates every session's process group (with the same grace period) before exiting.


[Back to the README](../README.md).

## MCP processes

The bridge also owns configured [MCP servers](mcp.md). Shared agent code handles
configuration validation, discovery, schema translation, approval previews and
call results. The native bridge provides filesystem, process, stdio and timeout
primitives. Both server runs and browser runs through paired hosts use this same
policy and immutable, authenticated catalog contract.
