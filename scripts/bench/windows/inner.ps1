# Runs inside the Windows terminal under test (see bench.ps1).
#   inner.ps1 RUNDIR type|ps FILE
# type: `cmd /c type FILE` (cmd's output through the console host)
# ps:   PowerShell's own `Get-Content FILE`
# Reports that it started and the window size, waits for the harness to
# sample the terminal's CPU time, times the command, then waits for `quit`
# and exits with 0 (Windows Terminal keeps a tab open after a failure).
# A failure is written to `done` too, so the harness never waits for it.
param([string]$RunDir, [string]$Mode, [string]$File)
$ErrorActionPreference = 'Stop'
function Write-Done([string]$text) {
    Set-Content -LiteralPath "$RunDir\done.tmp" -Value $text
    Move-Item -Force -LiteralPath "$RunDir\done.tmp" -Destination "$RunDir\done"
}
try {
    $size = $Host.UI.RawUI.WindowSize
    Set-Content -LiteralPath "$RunDir\begin.tmp" -Value "$($size.Width)x$($size.Height)"
    Move-Item -LiteralPath "$RunDir\begin.tmp" -Destination "$RunDir\begin"
    while (-not (Test-Path -LiteralPath "$RunDir\go")) { Start-Sleep -Milliseconds 10 }
    # Full path: a new Windows Terminal window may start with a PATH
    # that has no System32 in it.
    $cmd = Join-Path $env:SystemRoot 'System32\cmd.exe'
    $sw = [Diagnostics.Stopwatch]::StartNew()
    if ($Mode -eq 'type') {
        & $cmd /d /c type "$File"
        $rc = $LASTEXITCODE
    } else {
        Get-Content -LiteralPath $File
        $rc = 0
    }
    $sw.Stop()
    Write-Done "$($sw.Elapsed.TotalSeconds) $rc"
} catch {
    Write-Done "0 error:$($_.ToString() -replace '\s+', '_')"
}
while (-not (Test-Path -LiteralPath "$RunDir\quit")) { Start-Sleep -Milliseconds 100 }
exit 0
