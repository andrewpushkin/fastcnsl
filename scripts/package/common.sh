# Shared helpers for the packaging scripts. Sourced, not executed.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
ROOT="$(pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
DIST="$ROOT/dist"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
mkdir -p "$DIST"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

# Fails with a hint when a packaging step runs before the compile step.
need_binary() {
    if [[ ! -f "$1" ]]; then
        echo "error: $1 not found; compile first (make $2)" >&2
        exit 1
    fi
}
