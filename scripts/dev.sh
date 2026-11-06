#!/usr/bin/env bash
# Run cargo commands inside the development container.
set -euo pipefail

IMAGE=ragamuffin-dev
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

build_image() {
    docker build -f "$ROOT/Dockerfile.dev" -t "$IMAGE" "$ROOT"
}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    build_image
fi

TTY_FLAGS=()
if [ -t 0 ] && [ -t 1 ]; then TTY_FLAGS=(-it); fi

run() {
    docker run --rm "${TTY_FLAGS[@]}" \
        -u "$(id -u):$(id -g)" \
        -e HOME=/tmp \
        -v "$ROOT:/work" \
        -v ragamuffin-cargo:/cargo \
        -w /work \
        "$IMAGE" "$@"
}

cmd="${1:-build}"
shift || true

case "$cmd" in
    image)     build_image ;;
    build)     run cargo build --release "$@" ;;
    debug)     run cargo build "$@" ;;
    test)      run cargo test "$@" ;;
    clippy)    run cargo clippy --all-targets "$@" -- -D warnings ;;
    fmt)       run cargo fmt "$@" ;;
    windows)   run cargo build --release --target x86_64-pc-windows-gnu "$@" ;;
    check-all)
        run cargo check
        run cargo check --target x86_64-pc-windows-gnu
        run cargo check --target x86_64-apple-darwin
        ;;
    shell)     run bash ;;
    *)         run "$cmd" "$@" ;;
esac
