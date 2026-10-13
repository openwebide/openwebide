#!/bin/bash
set -u

openwebide-ssh-init || exit $?

if [[ $# -gt 0 && $1 != -* ]]; then
    exec "$@"
fi

children=()

shutdown() {
    local signal=$1 status=$2
    trap '' TERM INT
    kill -s "$signal" "${children[@]}" 2>/dev/null || true
    wait "${children[@]}" 2>/dev/null || true
    exit "$status"
}

trap 'shutdown TERM 143' TERM
trap 'shutdown INT 130' INT

if [[ ${OPENWEBIDE_BRIDGE:-1} != 0 ]]; then
    OPENWEBIDE_APP_CONTAINER=1 openwebide-bridge --host "${OPENWEBIDE_BRIDGE_HOST:-0.0.0.0}" --port 3001 --workspace /workspace \
        --backend-url http://127.0.0.1:3000/api --secret-file /app/.spin/bridge-secret &
    children+=("$!")
fi

spin up --listen "${OPENWEBIDE_APP_HOST:-0.0.0.0}:3000" --direct-mounts --allow-transient-write "$@" &
children+=("$!")

wait -n "${children[@]}"
status=$?
shutdown TERM "$status"
