#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/config.sh"

bash "$ROOT/scripts/stop.sh"

offckb clean

rm -rf -- "$ROOT/.state"
rm -f -- "$ROOT/configs/keys/operator-token"
