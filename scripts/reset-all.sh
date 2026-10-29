#!/usr/bin/env bash
set -euo pipefail

source "$(dirname -- "${BASH_SOURCE[0]}")/config.sh"

./scripts/clean.sh
./scripts/build-fiber.sh
./scripts/create.sh

./lsp-cli create-channel -u 1 --rpc-url "$RPC_URL"
./lsp-cli create-channel -u 2 --rpc-url "$RPC_URL"
