#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/config.sh"
kill "$(cat "$STATE_DIR/lsp.pid")"
rm "$STATE_DIR/lsp.pid"
offckb fiber stop
offckb node stop
