#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="$root/.tools/offckb"
revision="${OFFCKB_REVISION:-cd16d015e5339a791656f1acfb471adf473f7fa7}"
if [[ ! -d "$source_dir/.git" ]]; then
    git clone https://github.com/ckb-devrel/offckb.git "$source_dir"
fi
if [[ -n "$(git -C "$source_dir" status --porcelain)" ]]; then
    echo 'offckb checkout has local changes; preserve them before setup' >&2
    exit 1
fi
git -C "$source_dir" fetch origin "$revision"
git -C "$source_dir" checkout --detach FETCH_HEAD
cd "$source_dir"
# Native cpu-features is optional; avoid dependency lifecycle scripts.
pnpm install --frozen-lockfile --ignore-scripts
pnpm run build
node build/index.js fiber start --help
