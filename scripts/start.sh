#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/config.sh"

# CKB devnet and ordinary Fiber nodes are managed by offckb.
offckb node --daemon
offckb fiber start --nodes 2 --binary-path "$FNN_BINARY" --daemon

# Start the LSP separately with its own config and data.
mkdir -p "$STATE_DIR"
FIBER_SECRET_KEY_PASSWORD="$(cat "$LSP_DIR/password")" \
nohup "$FNN_BINARY" -d "$LSP_DIR" -c "$LSP_DIR/config.yml" \
    </dev/null >>"$STATE_DIR/lsp.log" 2>&1 &
echo "$!" > "$STATE_DIR/lsp.pid"
