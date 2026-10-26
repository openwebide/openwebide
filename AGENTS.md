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
- The current platform tool-group manifests are transitional feature switches, not completed behavior migrations. Do not claim migration complete until the plugin artifact owns the implementation and the same contract passes in local and remote modes, including disablement, failure and updates.

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
   - `wasm-bindgen` and `wasm-bindgen-test-runner` (`wasm-bindgen-cli` matching the exact
     `wasm-bindgen` version in `Cargo.lock`)
   - `google-chrome` and `chromedriver` (matching versions, e.g. Chrome for Testing in
     `~/.local/bin`)
   - `trunk` and `spin`

   Quick check:
   ```bash
   command -v cargo rustfmt cargo-clippy clang trunk spin wasm-bindgen-test-runner google-chrome chromedriver
   ```
   If any tool or target is missing or out of date, install or update it in the local environment
   before proceeding. Do not treat missing local tooling as an excuse to defer verification to CI.

2. **Mandatory Pre-Push Checks (Must Pass With Zero Warnings/Failures)**:
   Run these commands before every commit/push:
   ```bash
   # 1. Formatting
   cargo fmt --all --check

   # 2. Clippy across all CI targets (native, WASI backend, WASM frontend, bridge)
   cargo clippy -p openwebide-storage -p openwebide-core -p openwebide-auth -p openwebide-agent -p openwebide-llm --all-targets -- -D warnings
   cargo clippy -p openwebide-backend --target wasm32-wasip2 -- -D warnings
   cargo clippy -p openwebide-frontend --target wasm32-unknown-unknown -- -D warnings
   cargo clippy -p openwebide-bridge --all-targets -- -D warnings

   # 3. Native unit & integration tests
   cargo test -p openwebide-storage -p openwebide-core -p openwebide-auth -p openwebide-agent -p openwebide-llm -p openwebide-bridge -p openwebide-backend
   cargo test -p openwebide-frontend --lib

   # 4. Headless Chrome WASM UI tests (required whenever frontend, core, agent, or shared types change)
   CHROMEDRIVER=$(command -v chromedriver) cargo test -p openwebide-frontend --release --target wasm32-unknown-unknown --lib
   CHROMEDRIVER=$(command -v chromedriver) cargo test -p openwebide-frontend --release --target wasm32-unknown-unknown --test components
   ```
   When iterating on a specific frontend area, run the targeted browser test module first (for
   example `--test components -- streaming::`) and the full affected suite before pushing.

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
  In Codex, use `exec_command` with `sandbox_permissions: "require_escalated"` and
  `tty: true` for both server commands. Builds can stay sandboxed. A sandboxed server
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
