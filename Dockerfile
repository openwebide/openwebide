# syntax=docker/dockerfile:1

FROM node:24-bookworm-slim@sha256:c86ecead9e55a5210ac1727de38a4ca840bf03dcaf34dc9749addbf383b1920c AS language-server-tools
WORKDIR /opt/openwebide-language-servers
COPY tools/typescript-language-server/package.json tools/typescript-language-server/package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts --registry=https://registry.npmjs.org --no-audit --no-fund
WORKDIR /opt/openwebide-python-language-server
COPY tools/python-language-server/package.json tools/python-language-server/package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts --registry=https://registry.npmjs.org --no-audit --no-fund
WORKDIR /opt/openwebide-web-language-servers
COPY tools/web-language-servers/package.json tools/web-language-servers/package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts --registry=https://registry.npmjs.org --no-audit --no-fund

FROM mcr.microsoft.com/dotnet/sdk:10.0.403-noble@sha256:de4fe90a98845d75910b9769662465c84de1a934665c431279d1382b2bddc02b AS csharp-language-server-tools
RUN apt-get update && \
    apt-get install -y --no-install-recommends python3 && \
    rm -rf /var/lib/apt/lists/*
COPY tools/setup-csharp-language-server.py /opt/openwebide-setup/
COPY tools/csharp-language-server/versions.json tools/csharp-language-server/LICENSE /opt/openwebide-setup/csharp-language-server/
RUN python3 /opt/openwebide-setup/setup-csharp-language-server.py --tool-path /opt/openwebide-csharp-language-server

FROM ghcr.io/spinframework/spin:v4.1.0 AS go-language-server-tools
RUN apt-get update && \
    apt-get install -y --no-install-recommends python3 ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY tools/setup-go-language-server.py tools/install_wasi_sdk.py /opt/openwebide-setup/
COPY tools/go-language-server/versions.json /opt/openwebide-setup/go-language-server/
COPY tools/go-language-server/dependencies /opt/openwebide-setup/go-language-server/dependencies
RUN python3 /opt/openwebide-setup/setup-go-language-server.py --tool-path /opt/openwebide-go-language-server

FROM ghcr.io/spinframework/spin:v4.1.0 AS clangd-language-server-tools
RUN apt-get update && \
    apt-get install -y --no-install-recommends python3 ca-certificates libstdc++6 zlib1g && \
    rm -rf /var/lib/apt/lists/*
COPY tools/setup-clangd-language-server.py /opt/openwebide-setup/
COPY tools/clangd-language-server/versions.json tools/clangd-language-server/LICENSE.TXT /opt/openwebide-setup/clangd-language-server/
RUN python3 /opt/openwebide-setup/setup-clangd-language-server.py --tool-path /opt/openwebide-clangd-language-server

FROM ghcr.io/spinframework/spin:v4.1.0 AS terraform-language-server-tools
RUN apt-get update && \
    apt-get install -y --no-install-recommends python3 ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY tools/setup-terraform-language-server.py /opt/openwebide-setup/
COPY tools/terraform-language-server/versions.json /opt/openwebide-setup/terraform-language-server/
RUN python3 /opt/openwebide-setup/setup-terraform-language-server.py --tool-path /opt/openwebide-terraform-language-server

# --- builder: Spin runtime + Rust + Trunk; builds both components via `spin build` ---
FROM ghcr.io/spinframework/spin:v4.1.0 AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends curl ca-certificates build-essential clang python3 git && \
    rm -rf /var/lib/apt/lists/*

COPY tools/install_wasi_sdk.py tools/wasi-sdk.json /tmp/openwebide-wasi-tools/
RUN python3 /tmp/openwebide-wasi-tools/install_wasi_sdk.py /opt/openwebide-wasi-sdk \
    > /tmp/openwebide-wasi-sdk-path && \
    ln -s "$(cat /tmp/openwebide-wasi-sdk-path)" /opt/openwebide-wasi-sdk/current
ENV WASI_SDK_PATH=/opt/openwebide-wasi-sdk/current

ENV PATH="/root/.cargo/bin:${PATH}"
# Both build-time and runtime plugin compilers drop privileges. The public
# toolchain must be traversable without exposing the builder's private /root.
ENV RUSTUP_HOME=/opt/openwebide-toolchain/rustup
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain 1.98.1
RUN rustup component add llvm-tools rust-analyzer rust-src rustfmt

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

ARG OPENWEBIDE_NIGHTLY_BUILD=false
ARG OPENWEBIDE_BUILD_COMMIT=unknown
ENV OPENWEBIDE_BUILD_COMMIT=${OPENWEBIDE_BUILD_COMMIT}

WORKDIR /src
COPY . .
RUN if [ "$OPENWEBIDE_NIGHTLY_BUILD" = true ]; then \
        python3 tools/release.py stamp-nightly --commit "$OPENWEBIDE_BUILD_COMMIT"; \
    fi
# Regenerate only the locked subset from its immutable upstream Git commit.
RUN python3 tools/bundle_plugins.py
# Runs the [component.x.build] commands from spin.toml:
#   backend  -> cargo build -p openwebide-backend --target wasm32-wasip2 --release
#   frontend -> cd frontend && trunk build --release
# BuildKit can inject an OTLP Unix socket that Spin does not support.
# Disable only build-command telemetry; runtime configuration remains available.
RUN OTEL_SDK_DISABLED=1 spin build
# Compile the selected defaults once in the same isolation used for user installs.
# The final host embeds validated components; first launch requires no Git/network/compiler.
RUN cargo build -p openwebide-plugin-build -p openwebide-plugin-runtime --bin openwebide-plugin-build --bin openwebide-bundle-plugins --release --locked && \
    /src/target/release/openwebide-bundle-plugins bridge/bundled/plugins.json /src/target/bundled-plugins.json && \
    OPENWEBIDE_BUNDLED_ARTIFACTS=/src/target/bundled-plugins.json \
    cargo build -p openwebide-bridge --features bundled-defaults --release --locked

# --- public execution tools, independently smoke-testable ---
FROM ghcr.io/spinframework/spin:v4.1.0 AS runtime-tools

# SSH client is included for Git and app-configured host administration.
RUN apt-get update && \
    apt-get install -y --no-install-recommends openssh-client build-essential ca-certificates libicu72 libssl3 zlib1g python3 python3-venv libedit2 libffi8 libz3-4 libzstd1 liblzma5 && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=language-server-tools /usr/local/bin/node /usr/local/bin/node
COPY --from=language-server-tools /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/npm
COPY --from=language-server-tools /usr/local/LICENSE ./licenses/node-LICENSE
COPY --from=language-server-tools /opt/openwebide-language-servers/node_modules /opt/openwebide-language-servers/node_modules
ENV PATH="/opt/openwebide-language-servers/node_modules/.bin:${PATH}"
COPY --from=language-server-tools /opt/openwebide-python-language-server/node_modules /opt/openwebide-python-language-server/node_modules
ENV PATH="/opt/openwebide-python-language-server/node_modules/.bin:${PATH}"
COPY --from=language-server-tools /opt/openwebide-web-language-servers/node_modules /opt/openwebide-web-language-servers/node_modules
ENV PATH="/opt/openwebide-web-language-servers/node_modules/.bin:${PATH}"
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm && \
    ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx && \
    node --version && npm --version && typescript-language-server --version && tsc --version && \
    pyright --version && python3 --version
COPY --from=csharp-language-server-tools /usr/share/dotnet /usr/share/dotnet
COPY --from=csharp-language-server-tools /opt/openwebide-csharp-language-server /opt/openwebide-csharp-language-server
COPY tools/csharp-language-server/LICENSE ./licenses/csharp-ls-MIT.txt
ENV DOTNET_ROOT=/usr/share/dotnet
ENV DOTNET_CLI_TELEMETRY_OPTOUT=1
ENV DOTNET_GENERATE_ASPNET_CERTIFICATE=false
ENV DOTNET_NOLOGO=1
ENV PATH="/opt/openwebide-csharp-language-server:/usr/share/dotnet:${PATH}"
RUN dotnet --list-sdks && csharp-ls --version
COPY --from=go-language-server-tools /opt/openwebide-go-language-server /opt/openwebide-go-language-server
COPY --from=go-language-server-tools /opt/openwebide-go-language-server/go/LICENSE ./licenses/go-BSD.txt
COPY tools/go-language-server/LICENSE ./licenses/gopls-BSD.txt
ENV PATH="/opt/openwebide-go-language-server/bin:/opt/openwebide-go-language-server/go/bin:${PATH}"
RUN go version && gopls version
COPY --from=clangd-language-server-tools /opt/openwebide-clangd-language-server /opt/openwebide-clangd-language-server
COPY tools/clangd-language-server/LICENSE ./licenses/clangd-Apache-LLVM.txt
ENV PATH="/opt/openwebide-clangd-language-server/bin:${PATH}"
RUN clangd --version
COPY --from=terraform-language-server-tools /opt/openwebide-terraform-language-server /opt/openwebide-terraform-language-server
COPY --from=terraform-language-server-tools /opt/openwebide-terraform-language-server/licenses ./licenses/terraform
ENV PATH="/opt/openwebide-terraform-language-server/bin:${PATH}"
ENV CHECKPOINT_DISABLE=1
ENV TF_IN_AUTOMATION=1
RUN terraform version -json && terraform-ls --version

# --- runtime: Spin + prebuilt components ---
FROM runtime-tools
COPY --from=builder /src/target/release/openwebide-bridge /usr/local/bin/
COPY --from=builder /src/target/release/openwebide-plugin-build /usr/local/bin/
# The source compiler uses the pinned toolchain without relying on /root traversal
# after dropping privileges. Package code cannot write this toolchain or the SDK.
COPY --from=builder /opt/openwebide-toolchain/rustup /opt/openwebide-toolchain/rustup
COPY --from=builder /opt/openwebide-wasi-sdk /opt/openwebide-wasi-sdk
COPY --from=builder /root/.cargo/bin/rustup /opt/openwebide-toolchain/bin/rustup
ENV RUSTUP_HOME=/opt/openwebide-toolchain/rustup
ENV WASI_SDK_PATH=/opt/openwebide-wasi-sdk/current
ENV PATH="/opt/openwebide-toolchain/bin:${PATH}"
RUN ln -s rustup /opt/openwebide-toolchain/bin/cargo && \
    ln -s rustup /opt/openwebide-toolchain/bin/rustc && \
    ln -s rustup /opt/openwebide-toolchain/bin/rust-analyzer && \
    ln -s rustup /opt/openwebide-toolchain/bin/rustfmt
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
ENV OPENWEBIDE_MCP_CONFIG=/app/.spin/mcp.json
# Compose maps this name to Docker's host gateway; Podman provides it automatically.
ENV OPENWEBIDE_HOST_SSH_HOST=host.docker.internal
VOLUME /workspace

RUN sed -i 's#source = "../.."#source = "/workspace"#' spin.toml && grep -q 'source = "/workspace"' spin.toml && mkdir -p /workspace

ENTRYPOINT ["/app/entrypoint.sh"]
CMD []
