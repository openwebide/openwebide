# syntax=docker/dockerfile:1

# --- builder: Spin runtime + Rust + Trunk; builds both components via `spin build` ---
FROM ghcr.io/spinframework/spin:v4.1.0 AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends curl ca-certificates build-essential clang python3 git && \
    rm -rf /var/lib/apt/lists/*

ENV PATH="/root/.cargo/bin:${PATH}"
# Both build-time and runtime plugin compilers drop privileges. The public
# toolchain must be traversable without exposing the builder's private /root.
ENV RUSTUP_HOME=/opt/openwebide-toolchain/rustup
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain 1.98.1
RUN rustup component add llvm-tools

# Trunk (version must match .github/workflows/ci.yml)
ARG TARGETARCH
RUN set -eux; \
    case "${TARGETARCH}" in \
        amd64) trunk_arch=x86_64-unknown-linux-gnu ;; \
        arm64) trunk_arch=aarch64-unknown-linux-gnu ;; \
        *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    curl -sSL "https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-${trunk_arch}.tar.gz" \
        | tar -xz -C /usr/local/bin trunk

ARG OPENWEBIDE_BUILD_COMMIT=unknown
ENV OPENWEBIDE_BUILD_COMMIT=${OPENWEBIDE_BUILD_COMMIT}

WORKDIR /src
COPY . .
# Regenerate only the locked subset from its immutable upstream Git commit.
RUN python3 tools/bundle_plugins.py
# Runs the [component.x.build] commands from spin.toml:
#   backend  -> cargo build -p openwebide-backend --target wasm32-wasip2 --release
#   frontend -> cd frontend && trunk build --release
RUN spin build
RUN cargo build -p openwebide-plugin-build --release --locked
# Compile the selected defaults once in the same isolation used for user installs.
# The final host embeds validated components; first launch requires no Git/network/compiler.
RUN cargo run -p openwebide-plugin-runtime --bin openwebide-bundle-plugins --release --locked -- \
    bridge/bundled/plugins.json /src/target/bundled-plugins.json
RUN OPENWEBIDE_BUNDLED_ARTIFACTS=/src/target/bundled-plugins.json \
    cargo build -p openwebide-bridge --features bundled-defaults --release --locked

# --- runtime: Spin + prebuilt components ---
FROM ghcr.io/spinframework/spin:v4.1.0

# SSH client is included for Git and app-configured host administration.
RUN apt-get update && \
    apt-get install -y --no-install-recommends openssh-client build-essential ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /src/target/release/openwebide-bridge /usr/local/bin/
COPY --from=builder /src/target/release/openwebide-plugin-build /usr/local/bin/
# The source compiler uses the pinned toolchain without relying on /root traversal
# after dropping privileges. Package code cannot write this toolchain or the SDK.
COPY --from=builder /opt/openwebide-toolchain/rustup /opt/openwebide-toolchain/rustup
COPY --from=builder /root/.cargo/bin/rustup /opt/openwebide-toolchain/bin/rustup
ENV RUSTUP_HOME=/opt/openwebide-toolchain/rustup
ENV PATH="/opt/openwebide-toolchain/bin:${PATH}"
RUN ln -s rustup /opt/openwebide-toolchain/bin/cargo && \
    ln -s rustup /opt/openwebide-toolchain/bin/rustc
COPY --chmod=755 docker/entrypoint.sh ./entrypoint.sh
COPY --chmod=755 docker/ssh-init.sh /usr/local/bin/openwebide-ssh-init
COPY --chmod=755 docker/git.sh /usr/local/bin/git
COPY --from=builder /src/spin.toml ./
COPY --from=builder /src/target/wasm32-wasip2/release/openwebide_backend.wasm \
     ./target/wasm32-wasip2/release/
COPY --from=builder /src/frontend/dist ./frontend/dist
COPY --from=builder /src/bridge/bundled/LICENSE ./licenses/core-plugins-MIT.txt

# Frontend and API share one port; the bridge uses its own.
EXPOSE 3000 3001
# SQLite data lives in .spin/ (mount a volume here to persist it).
VOLUME /app/.spin
ENV OPENWEBIDE_PLUGIN_DIR=/app/.spin/plugins
VOLUME /workspace

RUN sed -i 's#source = "../.."#source = "/workspace"#' spin.toml && grep -q 'source = "/workspace"' spin.toml && mkdir -p /workspace

ENTRYPOINT ["/app/entrypoint.sh"]
CMD []
