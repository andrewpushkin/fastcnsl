#!/usr/bin/env bash
# Runs the release build on a virtual X display inside the dev container,
# drives the UI with xdotool and saves screenshots. Used by `make screenshot`.
#   screenshot.sh [out-dir]   (default dist/screenshots)
set -euo pipefail

OUT="${1:-dist/screenshots}"
BIN=./target/release/ragamuffin
SAMPLES="$(pwd)/scripts/samples"
mkdir -p "$OUT"

export DISPLAY=:99
unset WAYLAND_DISPLAY
export SHELL=/bin/bash USER=dev XDG_RUNTIME_DIR=/tmp
export PS1='dev@ragamuffin:\w\$ '

# Shell without rc files so the prompt above is used as-is; dark theme so
# the run doesn't depend on the container's (absent) desktop setting.
mkdir -p "$HOME/.config/ragamuffin"
cat > "$HOME/.config/ragamuffin/config.toml" <<'TOML'
shell = "/bin/bash"
shell_args = ["--norc", "--noprofile"]
theme = "dark"
TOML

Xvfb :99 -screen 0 1100x700x24 >/dev/null 2>&1 &
XVFB=$!
trap 'kill $XVFB 2>/dev/null || true' EXIT
sleep 1

RUST_LOG=ragamuffin=info "$BIN" &
APP=$!
trap 'kill $APP $XVFB 2>/dev/null || true' EXIT

WIN=$(timeout 20 xdotool search --sync --classname ragamuffin | head -1) || true
if [ -z "$WIN" ]; then
    echo "terminal window did not appear" >&2
    exit 1
fi
xdotool windowfocus --sync "$WIN"
sleep 2

shot() { sleep 0.6; import -window "$WIN" "$OUT/$1.png"; echo "-> $OUT/$1.png"; }
type_line() { xdotool type --delay 15 "$1"; xdotool key Return; }
click() { xdotool mousemove --window "$WIN" "$1" "$2" click "${3:-1}"; }

# Window size, for clicking the ⋮ button: it sits left of the three
# 46 px window buttons (minimize, maximize, close) in the top-right corner.
eval "$(xdotool getwindowgeometry --shell "$WIN")"
MENU_X=$((WIDTH - 3 * 46 - 15))
CLOSE_X=$((WIDTH - 23))

type_line "cd $SAMPLES"
type_line "cat hello.c"
xdotool key ctrl+shift+t
sleep 0.5
type_line "echo 'second tab'"
xdotool key ctrl+Tab
xdotool key ctrl+shift+f
xdotool type --delay 40 "greet"
shot main

# Pointer over the close button: red hover.
xdotool mousemove --window "$WIN" "$CLOSE_X" 15
shot titlebar
xdotool mousemove --window "$WIN" 300 200

xdotool key Escape
click $MENU_X 15
shot menu
xdotool key Escape

click 300 200 3
shot context
xdotool key Escape

xdotool key ctrl+shift+comma
shot options
# The "Keyboard shortcuts" page button, right of "General".
click 455 40
shot shortcuts
xdotool key Escape

# About through the menu, with the keyboard: Up from the bottom lands on
# "About", the last item.
click $MENU_X 15
xdotool key Up Return
shot about
xdotool key Escape

# Theme → Light via Ctrl+Shift+L (dark → light).
xdotool key ctrl+shift+l
xdotool key ctrl+shift+f
shot light
