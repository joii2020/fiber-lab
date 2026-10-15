#!/usr/bin/env bash
set -euxo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

./scripts/clean.sh
./scripts/build-fiber.sh
./scripts/create.sh

echo "等待 Fiber LSP 就绪（最多 120 秒）..."
token="$(cat configs/keys/operator-token)"
deadline=$((SECONDS + 120))
until curl --noproxy '*' -fs --max-time 2 http://127.0.0.1:21716 \
    -H 'Content-Type: application/json' \
    -H "Authorization: Bearer $token" \
    --data '{"jsonrpc":"2.0","id":1,"method":"lsp_get_status","params":[]}' \
    | jq -e '.error == null and .result.public_node_id != null' >/dev/null; do
    (( SECONDS < deadline )) || { echo "等待 Fiber LSP 启动超时，请查看 .state/devnet/lsp.log。" >&2; exit 1; }
    sleep 1
done

./lsp-cli create-channel -u 1
./lsp-cli create-channel -u 2
