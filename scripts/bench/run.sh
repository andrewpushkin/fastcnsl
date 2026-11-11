#!/usr/bin/env bash
# Terminal benchmark, run inside the bench-ubuntu-26.04 image by
# `make bench`. Builds fastcnsl, writes the input files, starts a display
# and runs bench.py. Extra arguments go to bench.py (--reps, --terminals,
# --tests).
#
#   BENCH_DISPLAY=xvfb (default)  Xvfb, software rendering for everyone
#   BENCH_DISPLAY=host            the host's X server ($DISPLAY, e.g. WSLg),
#                                 GPU through /dev/dxg when it is passed in
set -euo pipefail

OUT=${BENCH_OUT:-dist/bench}
DATA=${BENCH_DATA:-target/bench-data}
MODE=${BENCH_DISPLAY:-xvfb}

cargo build --release
python3 scripts/bench/gen.py "$DATA" "${BENCH_MB:-32}"

mkdir -p "$OUT"
if [ "$MODE" = xvfb ]; then
    export DISPLAY=:99
    Xvfb :99 -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 &
    trap 'kill $! 2>/dev/null || true' EXIT
    for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done
fi

{
    echo "date:     $(date -u +%Y-%m-%dT%H:%MZ)"
    echo "mode:     $MODE (DISPLAY=$DISPLAY)"
    echo "os:       $(. /etc/os-release && echo "$PRETTY_NAME"), kernel $(uname -r)"
    echo "cpu:      $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | xargs), $(nproc) threads"
    echo "fastcnsl: $(target/bench-ubuntu-26.04/release/fastcnsl --version)"
    echo "konsole:  $(konsole --version 2>/dev/null | tail -1)"
    echo "xterm:    $(xterm -version)"
    echo "gnome-terminal: $(dpkg-query -W -f '${Version}' gnome-terminal)"
    echo "ptyxis:   $(dpkg-query -W -f '${Version}' ptyxis)"
    echo "mesa:     $(dpkg-query -W -f '${Version}' mesa-vulkan-drivers)"
} | tee "$OUT/env.txt"

python3 scripts/bench/bench.py --data "$DATA" --out "$OUT" "$@"
