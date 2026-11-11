#!/usr/bin/env bash
# Windows benchmark, started from WSL by `make bench-windows`: copies the
# Windows build, the scripts and the input files to %TEMP%\fastcnsl-bench
# and runs windows/bench.ps1 there, natively (no Docker). Results go to
# dist/bench-windows/. Extra arguments go to bench.ps1, e.g.
#   scripts/bench/windows.sh -Reps 5 -Terminals fastcnsl,wt -Tests startup,type-ascii
set -euo pipefail
cd "$(dirname "$0")/../.."

BUILD=target/windows-x86_64/x86_64-pc-windows-gnu/release
OUT=dist/bench-windows
DATA=target/bench-data
[ -f "$BUILD/fastcnsl.exe" ] || { echo "no $BUILD/fastcnsl.exe; run make windows-x86_64" >&2; exit 1; }
if tasklist.exe /FI "IMAGENAME eq fastcnsl.exe" 2>/dev/null | grep -qi fastcnsl.exe; then
    echo "close the running fastcnsl windows first: new-tab would open the tests in them" >&2
    exit 1
fi

WIN_TEMP=$(cmd.exe /d /c 'echo %TEMP%' 2>/dev/null | tr -d '\r')
B=$(wslpath -u "$WIN_TEMP")/fastcnsl-bench
mkdir -p "$B/fastcnsl" "$B/data" "$B/data-lf"
rm -rf "$B/runs" "$B/out"
for f in fastcnsl.exe fastcnsl-wsl conpty.dll OpenConsole.exe; do
    [ -f "$BUILD/$f" ] && cp "$BUILD/$f" "$B/fastcnsl/"
done
cp scripts/bench/windows/inner.ps1 scripts/bench/windows/startup.cmd scripts/bench/windows/bench.ps1 "$B/"
# Windows input files with CRLF line ends; sizes are taken from them.
python3 scripts/bench/gen.py "$DATA" 32
# WSL tests read the 32 MiB files from Linux. Windows' own console output
# (cmd's type, PowerShell) runs at a few MB/s whatever the terminal, so
# those tests use the first 4 MiB, with CRLF line ends.
for f in "$DATA"/*.txt; do
    n=$(basename "$f" .txt)
    [ -f "$B/data-lf/$n.txt" ] || cp "$f" "$B/data-lf/$n.txt"
    [ -f "$B/data/${n}4m.txt" ] || awk '{ n += length($0) + 2; if (n > 4194304) exit; printf "%s\r\n", $0 }' "$f" > "$B/data/${n}4m.txt"
done
rm -f "$B"/data/{ascii,color,unicode,lines,small}.txt

mkdir -p "$OUT"
powershell.exe -NoProfile -Command '
    $os = Get-CimInstance Win32_OperatingSystem
    "date:     " + (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mmZ")
    "windows:  " + $os.Caption + " " + $os.Version
    "cpu:      " + (Get-CimInstance Win32_Processor).Name.Trim() + ", " + [Environment]::ProcessorCount + " threads"
    "gpu:      " + ((Get-CimInstance Win32_VideoController).Name -join ", ")
    "terminal: Windows Terminal " + (Get-AppxPackage Microsoft.WindowsTerminal).Version
    "powershell: " + $PSVersionTable.PSVersion
' 2>/dev/null | tr -d '\r' > "$OUT/env.txt"
echo "fastcnsl: $(scripts/get_version.sh)" >> "$OUT/env.txt"
cat "$OUT/env.txt"

powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(wslpath -w "$B/bench.ps1")" \
    -Bench "$(wslpath -w "$B")" -Out "$(wslpath -w "$B/out")" \
    -WslDistro "${WSL_DISTRO_NAME:-Ubuntu}" -WslBench "$B" \
    -WslData "$(pwd)/$DATA" -WslInner "$(pwd)/scripts/bench/inner.sh" "$@" | tr -d '\r'

cp "$B/out/results.jsonl" "$OUT/"
python3 scripts/bench/windows/summary.py "$OUT/results.jsonl" | tee "$OUT/summary.md"
