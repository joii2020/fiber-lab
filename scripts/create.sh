#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/config.sh"

# Only initialize a fresh environment; offckb would overwrite an existing Node3 config.
test ! -e "$LSP_DIR" || { echo "Node3 已存在；请用 scripts/start.sh 启动已有环境。" >&2; exit 1; }
test ! -e "$ROOT/configs/keys/operator-token" || { echo "管理 token 已存在；拒绝覆盖已有环境。" >&2; exit 1; }

offckb node --daemon
offckb fiber start --nodes 3 --binary-path "$FNN_BINARY" --daemon
offckb fiber stop

./lsp-cli admin init-lsp --config "$LSP_DIR/config.yml"
./lsp-cli admin create-token --config "$LSP_DIR/config.yml"

bash "$ROOT/scripts/start.sh"
