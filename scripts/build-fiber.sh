#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

source_dir="${1:-$root/../fiber}"
cd "$source_dir"
cargo build --release -p fiber-bin --features fnn/portable

cd "$root/fiber-lsp-client/"
cargo build --release

cd "$root"
if [[ ! -e lsp-cli && ! -L lsp-cli ]]; then
    ln -s fiber-lsp-client/target/release/fiber-lsp-client lsp-cli
fi
