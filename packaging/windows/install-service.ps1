# Run Lan Mouse as a Windows service (run as administrator):
#
#   powershell -ExecutionPolicy Bypass -File install-service.ps1 [-Exe <path to lan-mouse.exe>]
#   powershell -ExecutionPolicy Bypass -File install-service.ps1 -Uninstall
#
# Copies lan-mouse.exe to Program Files and installs the "LanMouse" service.
# The service starts Lan Mouse in the signed-in user's session with the
# SYSTEM token of that session's winlogon.exe, so it can control elevated
# windows, UAC prompts and the sign-in / lock screen. Lan Mouse keeps using
# the user's configuration (%LOCALAPPDATA%\lan-mouse).
#Requires -RunAsAdministrator
param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\lan-mouse.exe'),
    [string]$UserProfile = $env:USERPROFILE,
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$Dir = Join-Path $env:ProgramFiles 'Lan Mouse'
$Service = 'LanMouse'

function Stop-LanMouse {
    if (Get-Service $Service -ErrorAction SilentlyContinue) { Stop-Service $Service -Force -ErrorAction SilentlyContinue }
    Get-Process lan-mouse -ErrorAction SilentlyContinue | Stop-Process -Force
}

if ($Uninstall) {
    Stop-LanMouse
    if (Get-Service $Service -ErrorAction SilentlyContinue) { cmd /c "sc.exe delete $Service >nul 2>&1" }
    Get-NetFirewallRule -DisplayName 'Lan Mouse*' -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    Remove-Item -Recurse -Force $Dir -ErrorAction SilentlyContinue
    Write-Host 'Lan Mouse service removed (settings in %LOCALAPPDATA%\lan-mouse kept).'
    return
}

if (-not (Test-Path $Exe)) { throw "lan-mouse.exe not found at $Exe" }
Stop-LanMouse
New-Item -ItemType Directory -Force $Dir | Out-Null
Copy-Item -Force $Exe (Join-Path $Dir 'lan-mouse.exe')
$Installed = Join-Path $Dir 'lan-mouse.exe'

# input (UDP) and clipboard / files (TCP) on the default port
Get-NetFirewallRule -DisplayName 'Lan Mouse*' -ErrorAction SilentlyContinue | Remove-NetFirewallRule
New-NetFirewallRule -DisplayName 'Lan Mouse (keyboard and mouse)' -Direction Inbound -Program $Installed -Protocol UDP -LocalPort 4242 -Action Allow -Profile Any | Out-Null
New-NetFirewallRule -DisplayName 'Lan Mouse (clipboard and files)' -Direction Inbound -Program $Installed -Protocol TCP -LocalPort 4242 -Action Allow -Profile Any | Out-Null

$BinPath = "`"$Installed`" service --user-profile `"$UserProfile`""
if (Get-Service $Service -ErrorAction SilentlyContinue) {
    Set-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Services\$Service" -Name ImagePath -Value $BinPath
    Set-Service $Service -StartupType Automatic
} else {
    New-Service -Name $Service -BinaryPathName $BinPath -DisplayName 'Lan Mouse' -StartupType Automatic `
        -Description 'Shares keyboard and mouse with other computers running Lan Mouse.' | Out-Null
}
cmd /c "sc.exe failure $Service reset= 86400 actions= restart/2000/restart/5000/restart/10000 >nul 2>&1"
Start-Service $Service
Write-Host "Lan Mouse runs as the '$Service' service. Logs: $UserProfile\AppData\Local\lan-mouse\daemon.log and service.log"
