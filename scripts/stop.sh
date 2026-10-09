#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/config.sh"
test ! -f "$STATE_DIR/lsp.pid" || {
    lsp_pid="$(cat "$STATE_DIR/lsp.pid")"
    kill "$lsp_pid" 2>/dev/null || test ! -d "/proc/$lsp_pid"
}
rm -f "$STATE_DIR/lsp.pid"
offckb fiber stop
offckb node stop
