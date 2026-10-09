#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/config.sh"

bash "$ROOT/scripts/stop.sh"

offckb fiber clean --yes
offckb clean

rm -rf -- "${FIBER_DIR%/fiber}" "$ROOT/.state"
rm -f -- "$ROOT/configs/keys/operator-token"
