#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/config.sh"

test -f "$ROOT/configs/keys/operator-token" || { echo "请先用 scripts/create.sh 创建 LSP 环境。" >&2; exit 1; }

offckb node --daemon
offckb fiber start --daemon
