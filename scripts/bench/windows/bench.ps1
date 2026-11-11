# Windows terminal benchmark: fastcnsl against Windows Terminal and the
# classic console (conhost). Runs natively on Windows (Windows PowerShell
# 5.1); started by scripts/bench/windows.sh from WSL, which copies the
# build, these scripts and the input files to %TEMP%\fastcnsl-bench.
#
# Tests (run inside each terminal):
#   startup        launch -> a cmd.exe inside the window is running
#   type-<file>    cmd /c type FILE          (console host -> terminal)
#                  <file>4m: the first 4 MiB, CRLF (see windows.sh)
#   ps-<file>      PowerShell Get-Content FILE
#   wsl-<file>     wsl.exe -e cat FILE       (fastcnsl: its pipe relay)
# For every run: CPU time of the terminal's processes (incl. its console
# host, OpenConsole.exe / conhost.exe) from the start of the command until
# it settles, and its memory (working set).
#
# Windows Terminal runs every window in one WindowsTerminal.exe; when one
# is already open, its CPU time and memory are counted as the increase
# over the value just before the run.
param(
    [Parameter(Mandatory)] [string]$Bench,
    [Parameter(Mandatory)] [string]$Out,
    [int]$Reps = 3,
    [string]$Terminals = 'fastcnsl,wt,conhost',
    [string]$Tests = 'startup,type-ascii4m,type-lines4m,ps-ascii4m,wsl-ascii,wsl-color,wsl-unicode,wsl-lines',
    [string]$WslDistro = 'Ubuntu',
    [string]$WslBench,
    [string]$WslData,
    [string]$WslInner
)
$ErrorActionPreference = 'Stop'
$Fastcnsl = Join-Path $Bench 'fastcnsl\fastcnsl.exe'
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$jsonl = Join-Path $Out 'results.jsonl'

function Get-Procs { Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name }

function Get-Cpu([int[]]$ids) {
    $t = 0.0
    foreach ($id in $ids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p) { $t += $p.TotalProcessorTime.TotalSeconds }
    }
    $t
}

function Get-Mem([int[]]$ids) {
    $ws = 0; $peak = 0
    foreach ($id in $ids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p) { $ws += $p.WorkingSet64; $peak += $p.PeakWorkingSet64 }
    }
    @{ ws = $ws / 1MB; peak = $peak / 1MB }
}

function Wait-File([string]$path, [double]$timeout) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.Elapsed.TotalSeconds -lt $timeout) {
        if (Test-Path -LiteralPath $path) {
            for ($i = 0; $i -lt 50; $i++) {
                try { return (Get-Content -LiteralPath $path -Raw).Trim() } catch { Start-Sleep -Milliseconds 10 }
            }
        }
        Start-Sleep -Milliseconds 5
    }
    throw "timed out waiting for $path"
}

# The command the terminal runs, as an argument list.
# Full paths: a Windows Terminal started from here may get a PATH without
# Windows PowerShell in it.
$Sys32 = Join-Path $env:SystemRoot 'System32'
$PowerShell = Join-Path $Sys32 'WindowsPowerShell\v1.0\powershell.exe'
function Get-Command-For([string]$test, [string]$runDir, [string]$wslRunDir) {
    if ($test -eq 'startup') {
        return @((Join-Path $Sys32 'cmd.exe'), '/d', '/c', (Join-Path $Bench 'startup.cmd'), $runDir)
    }
    $kind, $file = $test -split '-', 2
    switch ($kind) {
        'type' { return @($PowerShell, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Bench 'inner.ps1'), $runDir, 'type', (Join-Path $Bench "data\$file.txt")) }
        'ps'   { return @($PowerShell, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Bench 'inner.ps1'), $runDir, 'ps', (Join-Path $Bench "data\$file.txt")) }
        'wsl'  { return @((Join-Path $Sys32 'wsl.exe'), '-d', $WslDistro, '-e', 'sh', $WslInner, $wslRunDir, "$WslData/$file.txt") }
    }
    throw "unknown test $test"
}

function Get-InputSize([string]$test) {
    if ($test -eq 'startup') { return 0 }
    $kind, $file = $test -split '-', 2
    if ($kind -eq 'wsl') { $p = Join-Path $Bench "data-lf\$file.txt" } else { $p = Join-Path $Bench "data\$file.txt" }
    (Get-Item -LiteralPath $p).Length
}

$runNo = 0
function Invoke-Run([string]$term, [string]$test) {
    $script:runNo++
    $runDir = Join-Path $Bench "runs\$($script:runNo)"
    $wslRunDir = "$WslBench/runs/$($script:runNo)"
    Remove-Item -Recurse -Force -LiteralPath $runDir -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $runDir | Out-Null
    $res = [ordered]@{ terminal = $term; test = $test }
    $cmd = Get-Command-For $test $runDir $wslRunDir

    $before = Get-Procs
    $beforeIds = @{}; foreach ($p in $before) { $beforeIds[[int]$p.ProcessId] = $true }
    $wtMain = @($before | Where-Object Name -eq 'WindowsTerminal.exe' | ForEach-Object { [int]$_.ProcessId })
    $wtCpu0 = Get-Cpu $wtMain
    $wtWs0 = (Get-Mem $wtMain).ws
    # warm: the window opens in a Windows Terminal that is already running.
    if ($term -eq 'wt') { if ($wtMain.Count -gt 0) { $res.wt = 'warm' } else { $res.wt = 'cold' } }

    $sw = [Diagnostics.Stopwatch]::StartNew()
    switch ($term) {
        'fastcnsl' { $launched = Start-Process -FilePath $Fastcnsl -ArgumentList (@('new-tab', 'bench', '--') + $cmd) -PassThru }
        'wt'       { $launched = Start-Process -FilePath 'wt.exe' -ArgumentList (@('-w', 'new', '--size', '100,30') + $cmd) -PassThru }
        'conhost'  { $launched = Start-Process -FilePath 'conhost.exe' -ArgumentList $cmd -PassThru }
    }
    try {
        $begin = Wait-File (Join-Path $runDir 'begin') 60
        $res.startup_ms = $sw.Elapsed.TotalMilliseconds
        $res.grid = ($begin -split '\s+' | Select-Object -Last 2) -join ' '
        if ($test.StartsWith('wsl')) { $g = $begin -split '\s+'; $res.grid = "$($g[2])x$($g[1])" }
        Start-Sleep -Milliseconds 1000

        $now = Get-Procs
        $new = @($now | Where-Object { -not $beforeIds.ContainsKey([int]$_.ProcessId) })
        switch ($term) {
            'fastcnsl' { $main = @($new | Where-Object Name -eq 'fastcnsl.exe' | ForEach-Object { [int]$_.ProcessId }) }
            'wt'       { $main = @($now | Where-Object Name -eq 'WindowsTerminal.exe' | ForEach-Object { [int]$_.ProcessId }) }
            'conhost'  { $main = @([int]$launched.Id) }
        }
        $hosts = @($new | Where-Object { ($_.Name -eq 'OpenConsole.exe' -or $_.Name -eq 'conhost.exe') -and ($main -contains [int]$_.ParentProcessId) } | ForEach-Object { [int]$_.ProcessId })
        $ids = @($main + $hosts | Select-Object -Unique)
        $res.procs = ($new | Where-Object { $ids -contains [int]$_.ProcessId } | ForEach-Object Name) -join ','
        # Windows Terminal's shared process: only what this window added.
        $wsBase = 0; if ($term -eq 'wt') { $wsBase = $wtWs0 }
        $res.idle_ws_mb = (Get-Mem $ids).ws - $wsBase

        if ($test -ne 'startup') {
            $cpu0 = Get-Cpu $ids
            New-Item -ItemType File -Path (Join-Path $runDir 'go') | Out-Null
            $done = (Wait-File (Join-Path $runDir 'done') 900) -split '\s+'
            $tDone = $sw.Elapsed.TotalSeconds
            if ($test.StartsWith('wsl')) { $res.cat_s = [double]$done[0] / 1e9 } else { $res.cat_s = [double]$done[0] }
            if ($done.Count -gt 1 -and $done[1] -ne '0') { throw "command exited with $($done[1])" }
            $res.mb_s = (Get-InputSize $test) / 1MB / $res.cat_s
            # Settled: under 10 ms of CPU over the last 300 ms.
            $samples = New-Object System.Collections.ArrayList
            [void]$samples.Add(@($tDone, (Get-Cpu $ids)))
            while ($sw.Elapsed.TotalSeconds - $tDone -lt 120) {
                Start-Sleep -Milliseconds 100
                [void]$samples.Add(@($sw.Elapsed.TotalSeconds, (Get-Cpu $ids)))
                $n = $samples.Count
                if ($n -gt 3 -and ($samples[$n - 1][1] - $samples[$n - 4][1]) -lt 0.010) { break }
            }
            $n = $samples.Count
            $res.settle_s = 0.0
            if ($n -gt 3) { $res.settle_s = [Math]::Max(0.0, $samples[$n - 4][0] - $tDone) }
            $res.cpu_s = $samples[$n - 1][1] - $cpu0
            $m = Get-Mem $ids
            $res.ws_mb = $m.ws - $wsBase
            if ($term -ne 'wt') { $res.peak_ws_mb = $m.peak }
        }
    } catch {
        $res.error = "$_"
    } finally {
        New-Item -ItemType File -Force -Path (Join-Path $runDir 'quit') | Out-Null
        # Wait for the window to close on its own, then clean up leftovers
        # (never the Windows Terminal process that was already running).
        $deadline = [Diagnostics.Stopwatch]::StartNew()
        while ($deadline.Elapsed.TotalSeconds -lt 10) {
            $left = @(Get-Procs | Where-Object { -not $beforeIds.ContainsKey([int]$_.ProcessId) -and
                    @('fastcnsl.exe', 'OpenConsole.exe', 'conhost.exe', 'powershell.exe', 'cmd.exe', 'wsl.exe') -contains $_.Name -and
                    ($_.ProcessId -ne $PID) })
            if ($left.Count -eq 0) { break }
            Start-Sleep -Milliseconds 100
        }
        foreach ($p in $left) { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue }
        if ($left.Count -gt 0) { $res.leftover = ($left | ForEach-Object Name) -join ',' }
        # A Windows Terminal this script started stays running after its
        # last window closes; stop it so every run starts the same way.
        Get-Process WindowsTerminal -ErrorAction SilentlyContinue |
            Where-Object { -not $UserWt.ContainsKey($_.Id) } |
            Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
    }
    $res
}

# Windows Terminal processes that were running before: never stopped.
$UserWt = @{}
Get-Process WindowsTerminal -ErrorAction SilentlyContinue | ForEach-Object { $UserWt[$_.Id] = $true }
$termList = $Terminals -split '[,\s]+'
$testList = $Tests -split '[,\s]+'
for ($rep = 0; $rep -lt $Reps; $rep++) {
    foreach ($test in $testList) {
        foreach ($term in $termList) {
            $r = Invoke-Run $term $test
            $r.rep = $rep
            ($r | ConvertTo-Json -Compress) | Add-Content -LiteralPath $jsonl -Encoding UTF8
            if ($r.error) { $status = $r.error }
            elseif ($test -eq 'startup') { $status = "startup {0:N0}ms ws {1:N0}MB" -f $r.startup_ms, $r.idle_ws_mb }
            else { $status = "cat {0:N2}s settle {1:N2}s cpu {2:N2}s startup {3:N0}ms ws {4:N0}MB grid {5}" -f $r.cat_s, $r.settle_s, $r.cpu_s, $r.startup_ms, $r.ws_mb, $r.grid }
            Write-Host ("[{0}/{1}] {2,-12} {3,-9} {4}" -f ($rep + 1), $Reps, $test, $term, $status)
        }
    }
}
