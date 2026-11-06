#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
node -e '
const [major, minor, patch] = process.versions.node.split(".").map(Number);
if (major < 22 || (major === 22 && (minor < 22 || (minor === 22 && patch < 2)))) {
    throw new Error("Language-server verification requires Node.js 22.22.2 or newer");
}
'
# Use only the integrity-locked public tools, independently of personal npm
# registries and credentials. Never execute dependency installation scripts.
npm_config_directory="$(mktemp -d)"
trap 'rm -rf "$npm_config_directory"' EXIT
touch "$npm_config_directory/user" "$npm_config_directory/global"
NPM_CONFIG_USERCONFIG="$npm_config_directory/user" \
NPM_CONFIG_GLOBALCONFIG="$npm_config_directory/global" \
    npm ci --prefix "$repo_root/tools/python-language-server" \
        --registry=https://registry.npmjs.org --ignore-scripts --no-audit --no-fund
"$repo_root/tools/python-language-server/node_modules/.bin/pyright" --version
python3 --version
