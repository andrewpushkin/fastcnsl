#!/usr/bin/env bash
# Runs the release build on a virtual X display inside the dev container,
# types a few commands and saves a screenshot. Used by `make screenshot`.
set -euo pipefail

OUT="${1:-dist/screenshot.png}"
BIN=./target/release/ragamuffin
SAMPLE="$(pwd)/scripts/samples/hello.c"

export DISPLAY=:99
unset WAYLAND_DISPLAY
export SHELL=/bin/bash USER=dev XDG_RUNTIME_DIR=/tmp
export PS1='dev@ragamuffin:\w\$ '

# Shell without rc files so the prompt below is used as-is.
mkdir -p "$HOME/.config/ragamuffin"
cat > "$HOME/.config/ragamuffin/config.toml" <<'TOML'
shell = "/bin/bash"
shell_args = ["--norc", "--noprofile"]
TOML

Xvfb :99 -screen 0 1100x700x24 >/dev/null 2>&1 &
XVFB=$!
trap 'kill $XVFB 2>/dev/null || true' EXIT
sleep 1

RUST_LOG=ragamuffin=info "$BIN" &
APP=$!
trap 'kill $APP $XVFB 2>/dev/null || true' EXIT

WIN=$(timeout 20 xdotool search --sync --name ragamuffin | head -1) || true
if [ -z "$WIN" ]; then
    echo "terminal window did not appear" >&2
    exit 1
fi
xdotool windowfocus --sync "$WIN"
sleep 2
xdotool type --delay 20 "cd $(dirname "$SAMPLE")"
xdotool key Return
sleep 0.5
xdotool type --delay 20 "cat hello.c"
xdotool key Return
sleep 1
xdotool key ctrl+shift+f
xdotool type --delay 40 "greet"
sleep 1
import -window "$WIN" "$OUT"
echo "-> $OUT"
