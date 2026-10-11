#!/usr/bin/env bash
# Run in a normal root Rust container. Reuse the caller's shared build directory.
set -euo pipefail
: "${CARGO_TARGET_DIR:?Set CARGO_TARGET_DIR to the shared build directory}"
target_args=()
if [[ -n "${OPENWEBIDE_PLUGIN_TEST_TARGET:-}" ]]; then
  target_args=(--target "$OPENWEBIDE_PLUGIN_TEST_TARGET")
fi
cargo build -p openwebide-plugin-build --locked "${target_args[@]}"
cargo clippy -p openwebide-plugin-build -p openwebide-plugin-runtime --locked "${target_args[@]}" --all-targets -- -D warnings
cargo test -p openwebide-plugin-build --locked "${target_args[@]}" --test container
cargo test -p openwebide-plugin-runtime --locked "${target_args[@]}" --lib
