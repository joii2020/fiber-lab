#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/config.sh"

# Configure a fresh LSP; reuse existing configuration and credentials with start.sh.
test ! -e "$LSP_DIR" || { echo "Node3 已存在；请用 scripts/start.sh 启动已有环境。" >&2; exit 1; }
test ! -e "$ROOT/configs/keys/operator-token" || { echo "管理 token 已存在；拒绝覆盖已有环境。" >&2; exit 1; }

offckb node --daemon
offckb fiber init --nodes 3 --binary-path "$FNN_BINARY"

./lsp-cli admin init-lsp --config "$LSP_DIR/config.yml"
./lsp-cli admin init-accounts --rpc-url "$RPC_URL"
./lsp-cli admin create-token --config "$LSP_DIR/config.yml"

offckb fiber start --daemon
