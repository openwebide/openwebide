# AGENTS.md

Instructions, conventions, and architectural principles for AI agents working on the Open WebIDE codebase.

---

## 1. Core Principles

### Support Every Feature in Both Local and Remote Modes
- Features must work in both local and remote projects, with the same user-facing behavior.
- Users should only need to consider the mode when opening a project: it determines which filesystem the folder picker shows. Once a project is open, workflows and controls should behave consistently without requiring users to manage the mode.
- Keep filesystem access behind the shared `Workspace` abstraction and mode-specific transport details behind the appropriate adapters.
- **Share feature implementations across modes.** Put policy, orchestration, validation, and domain behavior in shared code. Route host-specific operations through thin adapters, following the VFS pattern; adapters should handle filesystem, transport, and runtime integration rather than implement a second version of the feature. Avoid parallel local/remote business logic that can drift, and test the shared behavior plus each adapter’s contract.
- **Required structure:** UI components, state actions, and agent workflows call a shared feature facade. That facade selects an adapter; adapters expose filesystem, HTTP/WebSocket, process, clock, and persistence primitives. Feature policy, validation, orchestration, result shaping, limits, and fallback decisions live once above those adapters. Adding a trait around two complete feature implementations does not satisfy this rule.
- **Before implementing or fixing a feature:** locate its existing shared entry point and both mode adapters. Extend that entry point. If it is missing, extract the common behavior first rather than add a second implementation. Check UI controls, slash commands, background refresh, and agent tools for callers that bypass the facade.
- **Mode checks belong at adapter selection and platform capability boundaries**, such as folder picking and browser permissions. Do not branch on local/remote mode inside feature workflows or UI actions to select separate business logic. Differences in runtime libraries (WASI, Tokio, browser APIs) justify different primitives, not different feature contracts.
- **Required verification:** run the same behavioral contract against both adapters, including failures and fallbacks. Add regression coverage when fixing a parity bug; verify stale results cannot update another project/session or account. A feature is incomplete until both modes support it. Report any remaining gap explicitly.
- **Review gate:** flag duplicated orchestration, mode-specific feature paths, direct calls to a remote-only API from shared UI, and inconsistent limits or error semantics. Before reporting completion or preparing a commit/PR, identify the shared implementation and thin adapters and check the affected callers and parity tests. Do not mark roadmap work complete while a mode is missing support.

### First-Party Plugins Must Use the Public Extension Contract
- First-party plugins must own their contributed behavior and use the same versioned public SDK, host APIs, artifact format and lifecycle as community plugins.
- Do not implement a plugin by selecting a built-in feature through its identity, a tool-group flag, a privileged endpoint or a plugin-specific host handler. Do not provide built-in fallbacks for missing, disabled or failed plugin implementations.
- Host APIs may supply general capabilities such as workspace access, namespaced persistence, HTTP, approved process execution and durable job dispatch. Feature-specific policy, orchestration and result shaping belong in the plugin. A host API that simply performs a particular plugin's feature violates this rule.
- Bundling and default installation may differ; execution privileges and available interfaces may not. Skill-only plugins may contribute instructions/resources without executable code.
- Legacy platform tool-group declarations are retired feature switches. Do not claim migration complete until the plugin artifact owns the implementation and the same contract passes in local and remote modes, including disablement, failure and updates.

### Always Use the Component System for UI Elements
- **Consistency First**: Always use unified UI component patterns and shared styling classes across the frontend. Never introduce ad-hoc, isolated button/input styles that clash with the rest of the application.
- **Design Tokens & Theme Variables**: Rely strictly on the established CSS variables defined in `frontend/styles.css`:
  - Backgrounds: `var(--bg)`, `var(--bg-panel)`, `var(--bg-hover)`
  - Borders: `var(--border)`
  - Accents & Actions: `var(--accent)`, `var(--online)`, `var(--offline)`, `var(--warn)`, `var(--git-modified)`, `var(--on-accent)`
  - Typography & Code: `var(--text)`, `var(--text-muted)`, `var(--mono)`; body text uses the system font stack
- **Shared Components & Buttons**: Use standard button classes (`.btn`, `.btn.send`, `.btn.stop`, `.btn.approve`, `.btn.deny`, etc.) or reusable Leptos component abstractions. When new interactive controls are added, integrate them into the shared styling system so themes (dark/light) and visual hierarchy remain coherent.

### Always Use the Database for Persistence (No LocalStorage)
- **Seamless Multi-Device Continuity**: The core product philosophy is that a user must be able to switch machines, devices, or browsers and immediately pick up right where they left off without losing state.
- **No user state is persisted in localStorage**: theme and prompt history use user-scoped database settings; legacy keys are removed after import.
- **Never rely on `localStorage`** for user preferences, workspaces, layout configurations, or session state. The session is an HttpOnly cookie.
- **User-Scoped Database Settings**:
  - Persist all user layout preferences, panel dimensions (`panel_sidebar_width`, `panel_tree_width`, `panel_chat_width`), active tabs (`open_tabs`), active project (`active_project`), last-used session per project (`last_session_<project_id>`), theme, `bridge_url`, and default configurations in the SQLite database via the `/api/settings` endpoints (`crates/storage/src/store.rs` -> `user_settings` table).
  - User preferences and model defaults are scoped to the authenticated `user_id`. Server transport, model profiles (context, sampling, capabilities and compaction thresholds), and detection results describe shared server/model configuration; store them once per server/model, outside user settings. Never make runtime model facts or configuration vary by user.
  - Tools that depend on the `bridge_url` user setting must authenticate natively via `BridgeCredentials`.
  - The only browser-specific storage permitted is IndexedDB for local file system directory handles (`FileSystemDirectoryHandle`) and the bridge pairing token when running in browser local-mode, where native browser permissions or security requirements require origin-bound data. (There are no exceptions to this rule.)

---

## 2. Architecture Overview

- **Frontend (`frontend/`)**:
  - Built with **Leptos 0.8** compiling to WebAssembly (`wasm32-unknown-unknown`) via **Trunk**.
  - Feature stores in `frontend/src/state/*` are created by `App` and provided through Leptos context.
  - Single-page application providing code editing, terminal multiplexing, git status/diff visualization, collapsible file tree, and chat/agent interactions.
- **Backend (`backend/`)**:
  - WebAssembly microservices running on the **Fermyon Spin** runtime (`wasm32-wasip2`).
  - Implements REST and SSE endpoints for auth, workspace management, file I/O, LLM streaming, sessions, and settings.
- **Storage (`crates/storage/`)**:
  - SQLite storage layer. Spin has no migration runner, so `crates/storage/src/migrations.rs` applies version-gated migrations: `PRAGMA user_version` tracks the schema version against `SCHEMA_VERSION`, and each numbered step in `apply_step` runs once (every step stays idempotent, since pre-versioning databases start at 0 and replay). A database whose version is newer than the build refuses to start.
  - Supports user isolation, session histories, project metadata, and key-value user settings.
- **Execution Bridge (`bridge/`)**:
  - Native daemon (`openwebide-bridge`) with separate `terminals` (PTY/headless sessions), `exec` (host command/Git execution), `runs` (agent/chat runs), and `server` (HTTP/WebSocket routing) modules.
  - Uses a hyper HTTP server, a backend shared secret, and a WebSocket `hello` gate with short-lived bridge tokens (or a local pairing token). Chat/agent runs and completions stream over the shared WebSocket; the daemon is bundled alongside Spin in the Docker image.
  - HTTP tools and agent runs share `Arc<dyn ToolExecution>` on `ServerConfig`; terminals retain full host shell access outside that trait.
- **Shared Crates (`crates/`)**:
  - `openwebide-core`: Shared domain types, diff algorithms, syntax highlighting, TUI telemetry, and VFS abstractions.
  - `openwebide-auth`: argon2id hashing and HMAC session/bridge tokens (not JWT).
  - `openwebide-agent`: Tool-calling agent loop, tool execution, and permission gating.
  - `openwebide-llm`: Provider integrations (Ollama, and llama.cpp via its OpenAI-compatible API). There are no OpenAI or Anthropic providers.

---

## 3. Build & Test Commands

- **Toolchain**: pinned in `rust-toolchain.toml` (currently `1.98.1`). A toolchain bump moves
  `rust-toolchain.toml`, `.github/workflows/ci.yml`, and the `Dockerfile` builder stage together.
- **Backend WASM (Spin)**:
  ```bash
  cargo build -p openwebide-backend --target wasm32-wasip2 --release
  ```
- **Frontend Bundle (Trunk)**:
  ```bash
  cd frontend && trunk build --release
  ```
- **Unit & Integration Tests**:
  ```bash
  # Run tests across native crates
  cargo test -p openwebide-storage -p openwebide-core -p openwebide-auth -p openwebide-agent -p openwebide-llm -p openwebide-bridge -p openwebide-backend
  cargo test -p openwebide-frontend --lib
  ```
- **Frontend UI Tests** (needs Chrome + chromedriver and `wasm-bindgen-cli` matching
  `Cargo.lock`'s `wasm-bindgen`; always pass `--release` on `--test components` so Chrome loads an
  optimized WASM binary instead of a ~250 MiB debug module):
  ```bash
  CHROMEDRIVER=$(command -v chromedriver) cargo test -p openwebide-frontend --release --target wasm32-unknown-unknown --test components
  ```
- **Execution Bridge**:
  ```bash
  cargo build -p openwebide-bridge
  ```
  Language-server process watchers run the same bridge executable in a private
  control mode. Build it before filtered native `--lib` tests or standalone
  language-host examples; an embedded native host must have the matching bridge
  executable beside it. The ordinary integration-test gate builds it automatically.

### Required Pre-Commit / Pre-Push Verification & Tooling Gate

**Never commit or push without running the full local CI verification suite.** Many frontend UI
components (`frontend/src/components/*`) are gated behind `#[cfg(target_arch = "wasm32")]` and are
**not** compiled by native `cargo test -p openwebide-frontend --lib`. Skipping WASM compilation or
browser tests leaves compile errors, Clippy warnings, and DOM regressions undetected until GitHub
Actions runs.

1. **Verify Local Tooling First (Install If Missing — Never Skip Checks)**:
   Before running verification, confirm all required binaries and targets are installed and on `PATH`
   (e.g. `~/.local/bin` and `~/.cargo/bin`):
   - `rustup` toolchain matching `rust-toolchain.toml` with components `rustfmt`, `clippy` and
     targets `wasm32-unknown-unknown`, `wasm32-wasip2`
   - `clang` supporting `--target=wasm32-unknown-unknown` (required by `tree-sitter` C sources when
     compiling for WASM; e.g. Homebrew `llvm` symlinked into `~/.local/bin/clang`)
   - Native `cc` for the disposable interpreter used by executable-lifecycle
     tests. It must compile for the execution host, independently of WASI targets.
   - `wasm-bindgen` and `wasm-bindgen-test-runner` (`wasm-bindgen-cli` matching the exact
     `wasm-bindgen` version in `Cargo.lock`)
   - `google-chrome` and `chromedriver` (matching versions, e.g. Chrome for Testing in
     `~/.local/bin`)
   - `trunk` and `spin`
   - Bash 4.3 or newer for local container-supervisor checks (`wait -n`); macOS
     ships Bash 3.2, so install a current Bash and put it first on PATH.
   - `rust-analyzer`, `rust-src` and `rustfmt` from the pinned Rust toolchain for
     the required real language-server service check:
     `rustup component add rust-analyzer rust-src rustfmt`
   - Node.js 22.22.2 or newer and npm. Install the locked TypeScript language-server
     verification tools with `bash tools/setup-typescript-language-server.sh`;
     prepend `tools/typescript-language-server/node_modules/.bin` to `PATH`.
     Both real-server examples are required: `check_language_server` and
     `check_typescript_server`. The latter verifies all four related dialects,
     unsaved cross-file edits, diagnostics, completion imports and isolation.
   - Python 3 with `venv` support and the locked Pyright tools installed with
     `bash tools/setup-python-language-server.sh`; prepend
     `tools/python-language-server/node_modules/.bin` to `PATH`. The required
     `check_python_server` example verifies project configuration, virtual-environment
     imports, unsaved buffers, diagnostics, completion and isolation. The production
     browser check is `python3 tools/check-python-server-client.py` in both modes.
   - Reviewed resource operations and interrupted recovery have a production
     browser contract: `python3 tools/check-workspace-transactions-client.py`.
     Build the current release backend/frontend and release bridge with compiled
     bundled defaults first. It uses a deterministic LSP peer through the public
     HTML plugin; browser OPFS coverage does not prove a physical permission prompt.
     It also verifies command approval/outcomes, no replay after lost dispatch,
     reload and same-window stop, draft persistence during execution and rollback
     before command claim. Command cases use fresh corresponding native/OPFS folders.
   - Declared project inputs have a production contract:
     `python3 tools/check-project-inputs-client.py`. Build current release
     backend/frontend and bridge with compiled bundled defaults first. It installs
     a disposable server-only API 9 package through the public plugin API, rejects
     changed dependencies before any file primitive, and checks configuration moves
     plus unsaved text and a command on the original service in both modes.
     The local fixture mirrors selected configuration files between OPFS and its
     native discovery folder; it does not prove physical-folder permissions.
   - Agent language tools have a production contract:
     `python3 tools/check-agent-language-client.py`. Build the current release
     backend/frontend and bridge with bundled defaults first. It uses the real
     installed TypeScript server and a scripted local model to check browser,
     native bridge and backend executors, separate editor drafts and provider
     disablement. The pinned TypeScript tools must be installed, even if another
     language-server executable already exists on `PATH`.
   - Navigation and independent diagnostics have a production contract:
     `python3 tools/check-language-navigation-client.py`. Build current release
     backend/frontend artifacts and the release bridge with bundled defaults
     first. It installs two server-only packages through the public plugin API
     and uses deterministic LSP peers to verify navigation after result dialogs
     close, UTF-16 locations, diagnostic traversal, owning-provider fixes and
     disablement/failure/recovery in both modes. Its local folder uses OPFS.
   - Go 1.27.3 and gopls v0.24.0, installed with
     `python3 tools/setup-go-language-server.py`. Prepend
     `tools/go-language-server/installed/bin` and
     `tools/go-language-server/installed/go/bin` to `PATH`. The required
     `check_go_server` example verifies modules, multi-module workspaces,
     unsaved overlays, diagnostics, completion and isolation. The production
     browser check is `python3 tools/check-go-server-client.py` in both modes.
   - The pinned clangd distributions, installed with
     `python3 tools/setup-clangd-language-server.py`. Prepend
     `tools/clangd-language-server/installed/bin` to `PATH`. The required
     `check_clangd_server` example verifies C/C++ compilation flags, unsaved
     headers, diagnostics, reviewed edits and isolation. The production browser
     check is `python3 tools/check-clangd-server-client.py` in both modes.
   - The .NET 10 SDK. CI pins its version in `tools/csharp-language-server/versions.json`.
     Run `python3 tools/setup-csharp-language-server.py` and prepend
     `tools/csharp-language-server/installed` to `PATH`. The required real C#
     example is `check_csharp_server`: it verifies multi-project `.slnx` and
     standalone `.csproj` discovery, unsaved references, diagnostics, code actions
     and isolation. New solution fixtures must use `.slnx`.
   - Terraform and terraform-ls, installed from checksum-pinned distributions with
     `python3 tools/setup-terraform-language-server.py`. Prepend
     `tools/terraform-language-server/installed/bin` to `PATH`; set
     `CHECKPOINT_DISABLE=1` and `TF_IN_AUTOMATION=1`. The required
     `check_terraform_server` example verifies all seven supported dialects,
     unsaved cross-file analysis, completion, diagnostics, reviewable formatting,
     client/account isolation and revocation without initializing infrastructure.
     After rebuilding the app and compiled defaults, run
     `python3 tools/check-terraform-server-client.py` for installed syntax, real
     server queries, formatting/undo and lifecycle in both production editors.
     Its Local OPFS correspondence does not prove physical folder permissions.

   Quick check:
   ```bash
   command -v cargo rustfmt cargo-clippy clang trunk spin wasm-bindgen-test-runner google-chrome chromedriver
   ```
   If any tool or target is missing or out of date, install or update it in the local environment
   before proceeding. Do not treat missing local tooling as an excuse to defer verification to CI.
   Use CI's Chrome capabilities in ignored `webdriver.json` files at the root,
   `frontend/` and `tools/editor-bench/`: set `goog:chromeOptions.binary` to the
   installed Chrome executable and `args` to `["--window-size=1280,900"]`.
   Run suites with wall-clock deadlines without concurrent compilation or other heavy checks.

2. **Mandatory Pre-Push Checks (Must Pass With Zero Warnings/Failures)**:
   Run these commands before every commit/push:
   ```bash
   # 1. Formatting
   cargo fmt --all --check

   # 2. Clippy across all CI targets (native, WASI backend, WASM frontend, bridge)
   cargo clippy -p openwebide-storage -p openwebide-core -p openwebide-auth -p openwebide-agent -p openwebide-llm --all-targets -- -D warnings
   cargo clippy -p openwebide-backend --target wasm32-wasip2 -- -D warnings
   cargo clippy -p openwebide-frontend --target wasm32-unknown-unknown --all-targets -- -D warnings
   cargo clippy -p openwebide-bridge --all-targets -- -D warnings

   # 3. Native unit & integration tests
   cargo test -p openwebide-storage -p openwebide-core -p openwebide-auth -p openwebide-agent -p openwebide-llm -p openwebide-bridge -p openwebide-backend
   cargo test -p openwebide-frontend --lib

   # 4. Headless Chrome WASM UI tests (required whenever frontend, core, agent, or shared types change)
   CHROMEDRIVER=$(command -v chromedriver) WASM_BINDGEN_TEST_TIMEOUT=300 cargo test -p openwebide-frontend --release --target wasm32-unknown-unknown -- --skip 'editor::'
   CHROMEDRIVER=$(command -v chromedriver) WASM_BINDGEN_TEST_TIMEOUT=300 python3 tools/run-editor-browser-tests.py
   ```
   When iterating on a specific frontend area, run the targeted browser test module first (for
   example `--test components -- streaming::`) and the full affected suite before pushing.
   The complete browser gate uses the same partitions as CI: every non-editor test
   artifact, then independently checked editor groups and separate font matrices.
   Keep the existing per-run and per-test deadlines and complete compiled inventory.

3. **Tracked Pre-Push Git Hook (`.githooks/pre-push`)**:
   Ensure the repository's tracked Git hooks directory is active (never bypass with `--no-verify`):
   ```bash
   git config core.hooksPath .githooks
   ```

### Launching local development

Run from the repository root. Build the current changes before starting either service:

```bash
NO_COLOR=true spin build
cargo build -p openwebide-bridge
```

`NO_COLOR=true` avoids Trunk rejecting an inherited `NO_COLOR=1` value.

Start these in separate long-running terminal sessions:

```bash
spin up --direct-mounts --allow-transient-write
```

```bash
./target/debug/openwebide-bridge --port 3001 --workspace ../.. --host 127.0.0.1 \
  --backend-url http://127.0.0.1:3000/api --secret-file /tmp/openwebide-bridge-secret
```

- **Launch both Spin and the bridge outside the coding agent's filesystem sandbox.**
  Use normal host access and a persistent terminal for both server commands.
  Builds can stay sandboxed. A sandboxed server
  can start successfully and serve the UI while agent writes and shell commands fail
  with permission denials in other remote projects. Restart sandboxed processes with
  normal host access; do not change project permissions to compensate.
- The bridge's `--workspace` must match `component.backend.files.source` in
  `spin.toml` (currently `../..`, resolving to `~/source`). Keep the Spin flags above:
  they make remote edits apply to the host files rather than a temporary mount copy.
- Check existing listeners on ports 3000 and 3001 before launching. When restarting,
  stop the old processes and keep the new terminal sessions running.
- Verify the UI at `http://localhost:3000/` and bridge health at
  `http://localhost:3001/health`. `/api/health` requires authentication; an unsigned
  request returning 401 is expected. Verify a temporary file can be written, read,
  and deleted in a remote project outside this repo before claiming remote writes work.

Spin keeps the existing database in `.spin/sqlite_db.db` and logs in `.spin/logs/`.

### Build disk usage

`target/` grows fast here: native, `wasm32-unknown-unknown` and `wasm32-wasip2` builds, each
with its own test and clippy artifacts. A full repo copy that builds its own `target/` adds tens
of GB, and agents running several copies at once have filled the disk before.

- Experiments, review copies and worktrees reuse the repo's build directory: set
  `CARGO_TARGET_DIR=<repo>/target` (cargo locks it, so concurrent builds are safe). Never let a
  scratch copy create its own `target/`.
- Prefer `git worktree add` over copying the repo, and remove copies and worktrees when done.
- Dev builds keep only line tables (`[profile.dev] debug = "line-tables-only"`, and no debug info
  for dependencies) — don't turn full debug info back on in the workspace profile.
- Run `cargo clean` after finishing a feature, once it's pushed, and also whenever free space
  drops under ~50 GB between batches. Never run it while a build is running.

---

## 4. Code Conventions for Agents

1. **Reactive State**: In Leptos components, prefer `RwSignal`, `Signal::derive`, and `Callback` with clear ownership. Avoid cloning heavy state needlessly inside reactive closures. Shared frontend state lives in `frontend/src/state/*`, is provided from `App` with context, and belongs in the matching feature store.
2. **Error Handling**: Backend handlers live in `backend/src/api/*` and return `Result<T, ApiError>` through typed `FsError`, `BridgeError`, and `StorageError` conversions; never classify errors by substring. Internal errors log their detail with the route and return a generic 500 body. Use user-friendly error banners or notifications in the frontend.
3. **Database Migrations**: Append a numbered step in `apply_step` (`crates/storage/src/migrations.rs`), bump `SCHEMA_VERSION`, keep every step idempotent, and never edit a shipped step; update `Store` methods with corresponding unit tests in `crates/storage/src/store.rs`.
4. **Resilience & Safe Layouts**: When implementing layout resizing, enforce sane minimum and maximum bounds to ensure critical panels (like the code editor or diff viewer) are never crushed.

5. **Lint Policy**: Clippy pedantic picks are enforced via `[workspace.lints]` — every crate inherits it with `[lints] workspace = true`, and CI enforces it with `-D warnings` on native crates, the WASI backend, the WASM frontend, and the bridge. Add a lint there, not per crate; `#[allow]` needs a reason.

6. **Roadmap & Changelog**: Keep `docs/roadmap.md` and `CHANGELOG.md` current in the same change as the implementation. When work is completed and verified in both local and remote modes, remove it from the roadmap and record the shipped behavior under `[Unreleased]` in the changelog. For partial completion, rewrite the roadmap item to describe only the remaining work; configuration alone does not complete the feature it configures. Before reporting a feature complete or preparing a commit/PR, check the affected roadmap entries, dependencies, and cross-references for stale claims. Reviewers should flag missing updates.

## 5. Threat model

Single-user home lab on a trusted LAN. Keep what bites at home (a malicious website in a
browser tab, including another port on the same host; prompt-injected model output; data loss;
crashes; broken contracts), but security fixes must not remove features. Deliberately not done
for that reason: bridge commands inherit the user's environment (no env allow-list), and the
backend may reach any LAN host (no private-address egress block; only cloud-metadata addresses
are blocked and `fetch_web_page` needs approval).
