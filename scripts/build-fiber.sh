#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="${1:-$root/../fiber}"
cd "$source_dir"
cargo build --locked --release -p fiber-bin
