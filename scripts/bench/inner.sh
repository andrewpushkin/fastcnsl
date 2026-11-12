#!/bin/sh
# Runs inside the terminal under test (see bench.py).
#   inner.sh RUNDIR FILE
# Reports when the command started and the grid size, waits for the
# harness to sample the terminal's CPU time, then times `cat FILE`.
# Also used for WSL tabs by the Windows benchmark (windows/bench.ps1).
# The time is taken on the monotonic clock: the wall clock (date) can
# jump under WSL2. `done` holds: duration rc start end (ns), so that runs
# in several tabs at once can be lined up (bench.py tabs16).
d=$1
echo "$(date +%s%N) $(stty size)" > "$d/begin.tmp" && mv "$d/begin.tmp" "$d/begin"
while [ ! -e "$d/go" ]; do sleep 0.01; done
python3 -I -c '
import subprocess, sys, time
t = time.monotonic_ns()
rc = subprocess.call(["cat", sys.argv[2]])
e = time.monotonic_ns()
with open(sys.argv[1] + "/done.tmp", "w") as f:
    f.write(f"{e - t} {rc} {t} {e}\n")
' "$d" "$2"
mv "$d/done.tmp" "$d/done"
# Exit cleanly once told to (Windows Terminal keeps a tab open after a
# non-zero exit); on Linux the harness kills it anyway.
while [ ! -e "$d/quit" ]; do sleep 0.1; done
