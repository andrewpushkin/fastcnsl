#!/usr/bin/env bash
# Runs the release build on a virtual X display inside the dev container,
# drives the UI with xdotool and saves screenshots. Used by `make screenshot`.
#   screenshot.sh [out-dir]   (default dist/screenshots)
set -euo pipefail

OUT="${1:-dist/screenshots}"
BIN=./target/release/fastcnsl
SAMPLES="$(pwd)/scripts/samples"
mkdir -p "$OUT"

export DISPLAY=:99
unset WAYLAND_DISPLAY
export SHELL=/bin/bash USER=dev XDG_RUNTIME_DIR=/tmp
export PS1='dev@fastcnsl:\w\$ '

# Shell without rc files so the prompt above is used as-is; dark theme so
# the run doesn't depend on the container's (absent) desktop setting.
mkdir -p "$HOME/.config/fastcnsl"
cat > "$HOME/.config/fastcnsl/config.toml" <<'TOML'
shell = "/bin/bash"
shell_args = ["--norc", "--noprofile"]
theme = "dark"
TOML

Xvfb :99 -screen 0 1100x700x24 >/dev/null 2>&1 &
XVFB=$!
trap 'kill $XVFB 2>/dev/null || true' EXIT
sleep 1

RUST_LOG=fastcnsl=info "$BIN" &
APP=$!
trap 'kill $APP $XVFB 2>/dev/null || true' EXIT

WIN=$(timeout 20 xdotool search --sync --classname fastcnsl | head -1) || true
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

# A second search box: + is the third of the four buttons (↑ ↓ + ×, 24 px
# each) at the right end of the bar, which ends 18 px from the window edge.
BAR_R=$((WIDTH - 18))
click $((BAR_R - 3 - 24 * 2 + 12)) 55
xdotool type --delay 40 "int"
shot multisearch
# Its colour button at the left end of the 58-cell bar; then the fifth colour.
BAR_L=$((BAR_R - 58 * 9 - 12))
click $((BAR_L + 15)) 89
shot search_colors
click $((BAR_L + 30 + 12 + 4 * 27)) 89
shot search_recolored

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
# The "Font" tab, right of "General", then "Keyboard shortcuts" after it.
click 223 81
shot font
# The next font family: the panel keeps its size and place.
xdotool key Down
shot font_next
xdotool key Up
click 348 81
shot shortcuts
# Drag the panel by its title strip.
xdotool mousemove --window "$WIN" 400 40 mousedown 1
xdotool mousemove --window "$WIN" 300 140
xdotool mousemove --window "$WIN" 250 200 mouseup 1
shot options_moved
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
xdotool key Escape

# Rebind "New tab" to Alt+N in Options, then use it.
xdotool key ctrl+shift+comma
click 420 81
click 430 121
xdotool key alt+n
shot shortcut_changed
xdotool key Escape
xdotool key alt+n
shot shortcut_used

# Tab 1's right-click menu: copy, save, log, open folder, close.
click 100 15
click 100 15 3
shot tab_menu
# "Log to file" (fifth item, after previous/next tab, copy and save),
# then run something: a dot on the tab and a notice at the bottom.
xdotool key Down Down Down Down Down Return
type_line "seq 3"
shot tab_logging
# "Save text to file" (fourth item).
click 100 15 3
xdotool key Down Down Down Down Return
shot tab_saved
# Options → Logging, the fourth page.
xdotool key ctrl+shift+comma
click 488 81
shot options_logging
xdotool key Escape

# Output while tab 1 is in the background: a bell on it, gone when selected.
click 100 15
type_line "sleep 1.5; echo done"
click 320 15
sleep 2.5
shot tab_activity
click 100 15
shot tab_activity_cleared

LOGS="$HOME/fastcnsl-logs"
[ -d "$HOME/Documents" ] && LOGS="$HOME/Documents/fastcnsl-logs"
for f in "$LOGS"/*; do echo "== $f"; cat "$f"; done
