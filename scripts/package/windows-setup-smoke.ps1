# Installer smoke test, run on an x64 Windows PC (e.g. from WSL):
#   copy the two setups into one folder as x64.exe and arm64.exe, then
#   powershell -ExecutionPolicy Bypass -File windows-setup-smoke.ps1 -Dir <folder>
# Installs per user into <folder>\fc, upgrades over a running fastcnsl with
# the Start menu box unchecked, uninstalls, and checks that the ARM64 setup
# refuses this PC. The user's PATH is checked (and restored if needed) at
# the end. Stops if fastcnsl is already installed.
param([string]$Dir)
$ErrorActionPreference = 'Stop'
$id   = '{4FFEA6F9-A30F-4EF2-9FF2-6CF7D8C247F9}_is1'
$unk  = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$id"
$app  = Join-Path $Dir 'fc'
$lnkS = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\fastcnsl.lnk'
$lnkD = Join-Path ([Environment]::GetFolderPath('Desktop')) 'fastcnsl.lnk'
$cfg  = Join-Path $env:APPDATA 'fastcnsl\config.toml'
function UserPath { (Get-Item HKCU:\Environment).GetValue('Path', '', 'DoNotExpandEnvironmentNames') }
function Count($p) { @($p -split ';' | Where-Object { $_.TrimEnd('\') -ieq $app }).Count }
function Check($name, $ok) { if ($ok) { "PASS $name" } else { "FAIL $name"; $script:fail = $true } }
$fail = $false

if (Test-Path $unk) { 'ABORT: fastcnsl is already installed on this PC'; exit 2 }
if ((Get-Item HKCU:\Environment).GetValueKind('Path') -ne 'ExpandString') { 'note: user Path is not REG_EXPAND_SZ' }
$pathBefore = UserPath
$cfgBefore  = Test-Path $cfg
if ($cfgBefore) { $cfgTime = (Get-Item $cfg).LastWriteTimeUtc }

# 1. Fresh install: Start menu + PATH, no desktop icon.
$p = Start-Process (Join-Path $Dir 'x64.exe') -Wait -PassThru -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',"/LOG=$Dir\i1.log","/DIR=$app",'/TASKS=startmenu,addtopath'
Check 'install exit 0' ($p.ExitCode -eq 0)
foreach ($f in 'fastcnsl.exe','fastcnsl-wsl','conpty.dll','OpenConsole.exe','LICENSE','unins000.exe') { Check "file $f" (Test-Path (Join-Path $app $f)) }
Check 'no portable marker' (-not (Test-Path (Join-Path $app 'portable')))
Check 'start menu shortcut' (Test-Path $lnkS)
Check 'no desktop shortcut' (-not (Test-Path $lnkD))
Check 'PATH has dir once' ((Count (UserPath)) -eq 1)
$u = Get-ItemProperty $unk
Check "Apps entry: $($u.DisplayName) $($u.DisplayVersion) icon=$($u.DisplayIcon)" ($u.DisplayName -eq 'fastcnsl' -and $u.DisplayIcon -like '*fastcnsl.exe')

# 2. Upgrade over a running fastcnsl, Start menu now unchecked.
$fc = Start-Process (Join-Path $app 'fastcnsl.exe') -PassThru
Start-Sleep 4
Check 'fastcnsl running before upgrade' (-not $fc.HasExited)
$p = Start-Process (Join-Path $Dir 'x64.exe') -Wait -PassThru -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',"/LOG=$Dir\i2.log",'/TASKS=addtopath'
Check 'upgrade exit 0' ($p.ExitCode -eq 0)
Start-Sleep 1
Check 'running fastcnsl was closed' ($fc.HasExited)
Check 'start menu shortcut removed' (-not (Test-Path $lnkS))
Check 'PATH still has dir once' ((Count (UserPath)) -eq 1)
Select-String -Path "$Dir\i2.log" -Pattern 'Restart Manager|close|Closing' | Select-Object -First 4 | ForEach-Object { '  log: ' + $_.Line.Trim() }

# 3. Uninstall.
Start-Process (Join-Path $app 'unins000.exe') -Wait -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART'
for ($i = 0; $i -lt 30 -and (Test-Path (Join-Path $app 'fastcnsl.exe')); $i++) { Start-Sleep 1 }
Start-Sleep 2
Check 'files removed' (-not (Test-Path (Join-Path $app 'fastcnsl.exe')))
Check 'Apps entry removed' (-not (Test-Path $unk))
Check 'PATH entry removed' ((Count (UserPath)) -eq 0)
Check 'PATH same as before' ((UserPath) -eq $pathBefore)
if ($cfgBefore) { Check 'settings untouched' ((Get-Item $cfg).LastWriteTimeUtc -eq $cfgTime) }
elseif (Test-Path $cfg) { 'note: the test run created %APPDATA%\fastcnsl\config.toml (fastcnsl first start); removing it'; Remove-Item $cfg }

# 4. The ARM64 setup on this x64 PC must refuse.
$p = Start-Process (Join-Path $Dir 'arm64.exe') -Wait -PassThru -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',"/LOG=$Dir\a.log"
Check "arm64 setup refused (exit $($p.ExitCode))" ($p.ExitCode -ne 0)
Check 'refusal names the x64 download' ([bool](Select-String -Path "$Dir\a.log" -Pattern 'fastcnsl-windows-x86_64-setup.exe' -Quiet))
Check 'arm64 installed nothing' (-not (Test-Path $unk))

if ((UserPath) -ne $pathBefore) {
  'RESTORING user Path from backup'
  Set-ItemProperty HKCU:\Environment -Name Path -Value $pathBefore -Type ExpandString
}
if ($fail) { 'RESULT: FAIL'; exit 1 } else { 'RESULT: PASS' }
