# Installs paseo-led-bridge as an auto-starting background app.
# Run from PowerShell on Windows (from this folder, UNC path is fine):
#   powershell -ExecutionPolicy Bypass -File .\install-autostart.ps1
#
# What it does:
#   1. Stops the currently running bridge, if any.
#   2. Copies the exe to %LOCALAPPDATA%\PaseoLedBridge\ (stable local path,
#      independent of WSL being up).
#   3. Writes a .vbs launcher that runs it with a hidden console window
#      (a scheduled task in the user session would otherwise flash/keep a
#      console open; "run whether user is logged on or not" would hide it
#      but breaks foreground-window detection and BLE, which need the
#      interactive session).
#   4. Registers a Scheduled Task that starts it at logon and restarts it
#      up to 3 times, 1 minute apart, if it ever crashes. (The bridge
#      itself never exits on WS/BLE failures -- it reconnects forever --
#      so restarts only cover hard crashes.)

$ErrorActionPreference = "Stop"

$src = Join-Path $PSScriptRoot "target\x86_64-pc-windows-gnu\release\paseo-led-bridge.exe"
if (-not (Test-Path $src)) { $src = Join-Path $PSScriptRoot "paseo-led-bridge.exe" }
if (-not (Test-Path $src)) { throw "paseo-led-bridge.exe not found next to this script or in target\...\release" }

$task = Get-ScheduledTask -TaskName "PaseoLedBridge" -ErrorAction SilentlyContinue
if ($task) {
    Stop-ScheduledTask -InputObject $task -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 250
}

$dir = Join-Path $env:LOCALAPPDATA "PaseoLedBridge"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Copy-Item $src (Join-Path $dir "paseo-led-bridge.exe") -Force

$vbs = Join-Path $dir "paseo-led-bridge-hidden.vbs"
$exe = Join-Path $dir "paseo-led-bridge.exe"
$vbsContent = 'CreateObject("Wscript.Shell").Run """{0}"" run", 0' -f $exe
Set-Content -Path $vbs -Value $vbsContent

$action  = New-ScheduledTaskAction -Execute "wscript.exe" -Argument "`"$vbs`""
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$settings = New-ScheduledTaskSettingsSet `
    -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero)
Register-ScheduledTask -TaskName "PaseoLedBridge" -Action $action -Trigger $trigger `
    -Settings $settings -Force | Out-Null

Start-ScheduledTask -TaskName "PaseoLedBridge"
Write-Host "Updated and started. No Task Manager step is needed."
Write-Host "Logs: none (hidden window) -- run the exe manually in a terminal to debug."
