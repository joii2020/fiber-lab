# Sourced by scripts or by a developer shell: source scripts/config.sh
cd "$(dirname -- "${BASH_SOURCE[0]}")/.."
ROOT="$PWD"
FNN_BINARY="$ROOT/../fiber/target/release/fnn"
STATE_DIR="$ROOT/.state/devnet"
export RUST_LOG='info,fnn=debug' NO_COLOR=1
FIBER_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/fiber"
LSP_DIR="$FIBER_DIR/nodes/3"

RPC_URL="http://127.0.0.1:21716"
