<img src="frontend/pwa/favicon.svg" width="80" height="80" alt="Open WebIDE house logo" />

# Open WebIDE

A self-hosted browser IDE for coding with local LLMs: edit files, run an agent,
review its changes, and use Git and a terminal in one workspace. Connect to
Ollama or OpenAI-compatible model servers such as llama.cpp and LM Studio.

The Rust frontend and backend run on WebAssembly; a native execution bridge
provides terminals, commands, Git operations, and persistent agent runs. Projects,
sessions, and preferences live in SQLite, so you can resume from another browser.

## What you can do

- Edit code with syntax highlighting, search, Git diffs, and Markdown/image previews.
- Work across project tabs, with an automatically refreshed file tree and the
  last-used session restored when opening a project.
- Chat with a coding agent that reads, searches, edits, runs commands, and uses Git;
  review pending edits with Accept/Reject controls.
- Choose Manual, Auto-accept edits, Auto, or YOLO approval modes with `Shift+Tab`.
  Auto uses an optional fast model, falling back to the primary model.
- View build details and bundled license notices through **About** in the app menu or command search, including from the offline PWA
  screen. See [About](docs/about.md).
- Choose personal primary/fast defaults in Settings. Configure shared servers,
  credentials and model settings through the Servers setup wizard. See
  [your first session](docs/first-session.md#connect-a-model) for setup and
  [architecture](docs/architecture.md#shared-feature-boundaries) for context compaction.
- Discover host-installed [plugins](docs/plugins.md), enable them by default across
  projects, and review available updates. First-party plugins provide web tools,
  project memory, scheduling, monitors and skill authoring through the public SDK.
- Extend agent tools with configured [MCP servers](docs/mcp.md) on server and
  paired local execution hosts.
- Run [goals](docs/chat-controls.md), [scheduled tasks](docs/scheduled-tasks.md)
  and [monitors](docs/monitors.md), and inspect your homelab through
  [host administration](docs/host-administration.md).
- Use slash commands such as `/model`, `/test`, `/diff`, `/commit`, and `/sync`,
  with live token/speed telemetry and a context-window gauge.

See the [roadmap](docs/roadmap.md) for remaining work and the
[changelog](CHANGELOG.md) for completed changes.

The [project site guide](docs/project-site.md) explains how the landing page and documentation
are built and published to GitHub Pages from this repository.

Documentation and project updates: [openwebide.com](https://openwebide.com/).
Browse the [documentation](docs/index.md) by Getting started, Using Open WebIDE,
Hosting and administration, Development, or Project.

## Run with Docker

Main builds automatically update `ghcr.io/openwebide/openwebide:nightly`. The pinned alpha
release is `ghcr.io/openwebide/openwebide:v1.0.0-alpha.6`
for Linux amd64 and arm64. Download the versioned Compose or Podman Quadlet files
from [the alpha release](https://github.com/openwebide/openwebide/releases/tag/v1.0.0-alpha.6)
and run without cloning or building. The GHCR package is public, so no registry
authentication is needed. See
[releases, configuration and upgrades](docs/releases.md) for installation and backup
instructions. The source-build path below remains available now.

Existing installs should follow the [deployment-name migration notes](docs/releases.md#existing-installations-with-the-old-project-name)
to retain their database and Tailscale identity when adopting the `openwebide` name.

From a checkout of this repository:

```sh
docker compose up --build
```

Open <http://localhost:8080/> and register the first account. Compose mounts
`~/source` as `/workspace`, persists the database in the `openwebide-data` volume,
and exposes the bundled bridge on port 3001. Choose a different workspace with:

```sh
OPENWEBIDE_WORKSPACE=/path/to/projects docker compose up --build
```

For a standalone container:

```sh
docker build -t openwebide .
docker run -d --name openwebide \
  -p 8080:3000 -p 3001:3001 \
  -v openwebide-data:/app/.spin -v ~/source:/workspace openwebide
```

The workspace mount is accessible to file tools and the agent; choose its scope
accordingly. `.spin/` is excluded from file access. The bundled bridge persists
its secret at `/app/.spin/bridge-secret`; set `OPENWEBIDE_BRIDGE=0` to disable it
and use SSE chat instead. If overriding the container command, retain
`spin up --direct-mounts --allow-transient-write` so edits reach the mounted files.
Legacy remote paths are migrated automatically on startup.

For Linux services, see the [Podman quadlet guide](docs/podman-quadlet.md).
For private HTTPS access from phones and other devices, the recommended approach
is [Tailscale Serve in a separate container](docs/tailscale.md).

## Host administration

Host administration is included by default. Configure **Settings → Host** with an
SSH username, operating system and verified public host keys; the address defaults
to the deployment host. Project-less chat
can inspect that machine and propose changes for approval, with private terminal
replies for interactive prompts. Container deployments use the same SSH agent
setup as Git below. See [host administration](docs/host-administration.md).
**Settings → Host → Updates** selects a release channel, checks published images
and installs a compatible release through a configured Compose or user Quadlet
updater. The update runs on the host and survives the app restarting.

## Git authentication

Native bridges use the host SSH agent and configuration. Docker can forward the
host agent with the optional `docker-compose.ssh.yml` overlay, keeping private keys
on the host. See [Git authentication](docs/git-ssh.md) for Docker Desktop, Linux,
Podman, host-key verification and HTTPS credential helpers.

## Run from source

Install [Rust](https://rustup.rs/), [Spin 4.x](https://spinframework.dev/docs/latest/installation/),
and [Trunk](https://trunkrs.dev/getting-started/install/). Rust's toolchain and WASM
targets are pinned in `rust-toolchain.toml` and installed by rustup on first build.
The browser syntax parser also needs Clang (Xcode Command Line Tools on macOS;
`sudo apt-get install clang` on Debian/Ubuntu). Rust's pinned `llvm-tools` component
supplies the WASM archiver; the build selects it automatically, including on macOS
where Apple's archiver does not retain WASM objects.
Host Git operations require Git 2.23 or newer. Python 3 generates the bundled
open-source notices during frontend builds.

Build from the repository root:

```sh
NO_COLOR=true spin build
cargo build -p openwebide-bridge
```

Start Spin and the bridge in separate terminals:

```sh
spin up --direct-mounts --allow-transient-write
```

```sh
./target/debug/openwebide-bridge --port 3001 --workspace ../.. --host 127.0.0.1 \
  --backend-url http://127.0.0.1:3000/api --secret-file /tmp/openwebide-bridge-secret
```

Open <http://localhost:3000/>. The bridge workspace must match the backend's
`files.source` in `spin.toml` (currently `../..`). Both services need normal host
filesystem access: launching them inside a coding agent's sandbox can prevent
writes to other projects. Keep the Spin flags above to avoid editing a temporary
copy of the workspace.

## Open a project

After signing in, [connect a model](docs/first-session.md#connect-a-model) and
choose **Open remote** for host folders or **Open local** for this device's folders.
See [local and remote projects](docs/workspaces.md) for browser requirements,
folder permissions and command/Git capabilities.

## Connect over the LAN

Use `http://<host-ip>:8080` for Docker or port 3000 for a source build. For a native
bridge, bind it to `0.0.0.0` instead of `127.0.0.1` so browsers can reach port 3001.

When using a custom hostname, set `OPENWEBIDE_BRIDGE_ALLOWED_HOSTS=<hostname>`.
If the page and bridge use different hostnames, also set
`OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS=http://<page-host>:<port>`. Both accept
comma-separated lists and are passed through by Compose. A backend connecting to
a bridge over a non-loopback address also needs an explicitly configured shared
secret; see the [bridge reference](docs/execution-bridge.md).

## Accounts and agent execution

Accounts use argon2id password hashing and HttpOnly session cookies. Registration
closes after the first account. Logging out invalidates that user's sessions across
devices. Preferences and session state are database-backed rather than localStorage.

File tools are confined to the workspace; commands and Git run as the bridge's
host user. Approval modes control when the agent asks permission. Before overwriting
binary or large files, the agent keeps originals in `.openwebide/backups/` so Reject
can restore them; retain those backups until the edits no longer need restoration.

The backend can reach LAN and public model servers, search, and documentation
pages. Web fetching blocks cloud metadata addresses. `fetch_web_page` goes through
the approval policy; `search_web` is auto-approved. The bridge checks request hosts,
origins, and authentication to protect against malicious browser tabs.

## Development

```sh
cargo test -p openwebide-storage -p openwebide-core -p openwebide-auth -p openwebide-agent -p openwebide-llm -p openwebide-bridge -p openwebide-backend
cargo test -p openwebide-frontend --lib
CHROMEDRIVER=<path> cargo test -p openwebide-frontend --target wasm32-unknown-unknown
```

Browser tests require Chrome, chromedriver, and `wasm-bindgen-cli` matching
`Cargo.lock`. For frontend-only development, run `cd frontend && trunk serve`
and open `http://localhost:8080/?api=http://localhost:3000/api` with Spin running.
Use the same hostname for both services so session cookies work; the API override
is accepted only from `localhost:8080` or `127.0.0.1:8080`.

| Directory | Purpose |
| --- | --- |
| `frontend/` | Leptos WASM app, built with Trunk |
| `backend/` | Spin REST/SSE component |
| `bridge/` | Native terminals, execution, and agent hosting |
| `crates/` | Shared domain, agent, provider, authentication, and storage code |

Further reference: [architecture](docs/architecture.md),
[execution bridge](docs/execution-bridge.md), [plugin host installation](docs/plugins.md), [plugin authoring](docs/plugin-authoring.md),
and [API examples](docs/api-examples.md).

## License

[MIT](LICENSE).

Browser notifications support [Web Push](docs/web-push.md) for remote and projectless runs.
