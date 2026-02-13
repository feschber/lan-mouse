#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Build and run lan-mouse (as service or directly)
.DESCRIPTION
    Optionally builds lan-mouse in debug mode, stops any running service, copies binaries to target/svc/,
    then either starts the Windows service or runs the executable directly.
.PARAMETER Build
    Build lan-mouse before deployment
.PARAMETER Service
    Start the lan-mouse Windows service after deployment
.PARAMETER Direct
    Run ./target/svc/lan-mouse.exe directly (not as a service)
.PARAMETER Install
    If specified with -Service, registers the service via 'lan-mouse install' before starting
.PARAMETER Clean
    Truncate all log files in C:\ProgramData\lan-mouse\ before starting
.EXAMPLE
    .\run.ps1 -Build -Service
    .\run.ps1 -Build -Service -Install
    .\run.ps1 -Build -Direct
    .\run.ps1 -Direct
    .\run.ps1 -Clean -Service
#>

param(
    [switch]$Build,
    [switch]$Service,
    [switch]$Direct,
    [switch]$Install,
    [switch]$Clean
)

if (-not $Service -and -not $Direct) {
    Write-Host "Error: You must specify either -Service or -Direct" -ForegroundColor Red
    Write-Host "  -Service  Start the lan-mouse Windows service"
    Write-Host "  -Direct   Run the executable directly"
    exit 1
}

if ($Service -and $Direct) {
    Write-Host "Error: Cannot specify both -Service and -Direct" -ForegroundColor Red
    exit 1
}

$ErrorActionPreference = "Stop"

# Change to repository root
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $ScriptDir

try {
    if ($Build) {
        Write-Host "Building lan-mouse (debug, no default features)..." -ForegroundColor Cyan
        cargo build --no-default-features
        if ($LASTEXITCODE -ne 0) {
            throw "Build failed with exit code $LASTEXITCODE"
        }
    }

    Write-Host "`nStopping lan-mouse service..." -ForegroundColor Cyan
    $svcInfo = Get-Service -Name "lan-mouse" -ErrorAction SilentlyContinue
    if ($svcInfo -and $svcInfo.Status -eq "Running") {
        try {
            Stop-Service -Name "lan-mouse" -Force -ErrorAction Stop
            Write-Host "Service stopped" -ForegroundColor Green
        } catch {
            Write-Host "Stop-Service failed: $($_.Exception.Message)" -ForegroundColor Yellow
            Write-Host "Attempting to kill lan-mouse process..." -ForegroundColor Yellow
            $proc = Get-Process -Name "lan-mouse" -ErrorAction SilentlyContinue
            if ($proc) {
                $proc | Stop-Process -Force
                Write-Host "Process killed" -ForegroundColor Green
            } else {
                Write-Host "No lan-mouse process found" -ForegroundColor Yellow
            }
        }
    } elseif ($svcInfo) {
        Write-Host "Service exists but not running (status: $($svcInfo.Status))" -ForegroundColor Yellow
        # Still check for orphan process
        $proc = Get-Process -Name "lan-mouse" -ErrorAction SilentlyContinue
        if ($proc) {
            Write-Host "Found orphan lan-mouse process, killing..." -ForegroundColor Yellow
            $proc | Stop-Process -Force
            Write-Host "Process killed" -ForegroundColor Green
        }
    } else {
        Write-Host "Service not registered (will install if -Install is used)" -ForegroundColor Yellow
    }

    Write-Host "`nCopying binaries to target/svc/..." -ForegroundColor Cyan
    $SvcDir = Join-Path $ScriptDir "target\svc"
    if (-not (Test-Path $SvcDir)) {
        New-Item -ItemType Directory -Path $SvcDir | Out-Null
    }

    Copy-Item -Path "target\debug\*" -Destination $SvcDir -Recurse -Force
    Write-Host "Binaries copied" -ForegroundColor Green

    if ($Clean) {
        Write-Host "`nTruncating log files in C:\ProgramData\lan-mouse\..." -ForegroundColor Cyan
        $LogDir = "C:\ProgramData\lan-mouse"
        Get-ChildItem -Path $LogDir -Filter "*.log" -ErrorAction SilentlyContinue | ForEach-Object {
            Clear-Content -Path $_.FullName -ErrorAction SilentlyContinue
            Write-Host "  Truncated: $($_.Name)" -ForegroundColor Gray
        }
    }

    if ($Service) {
        if ($Install) {
            Write-Host "`nInstalling service..." -ForegroundColor Cyan
            $ServiceExe = Join-Path $SvcDir "lan-mouse.exe"
            & $ServiceExe install
            if ($LASTEXITCODE -ne 0) {
                throw "Service installation failed with exit code $LASTEXITCODE"
            }
            Write-Host "Service installed" -ForegroundColor Green
        }

        Write-Host "`nStarting lan-mouse service..." -ForegroundColor Cyan
        Start-Service -Name "lan-mouse"
        Write-Host "Service started" -ForegroundColor Green

        Write-Host "`nDeployment complete!" -ForegroundColor Green
        Write-Host "`nTailing service log (Ctrl+C to stop)..." -ForegroundColor Cyan
        Get-Content -Wait -Tail 20 "C:\ProgramData\lan-mouse\winsvc.log"
    } elseif ($Direct) {
        Write-Host "`nRunning lan-mouse directly..." -ForegroundColor Cyan
        $ServiceExe = Join-Path $SvcDir "lan-mouse.exe"
        & $ServiceExe
    }
} catch {
    Write-Host "`nError: $_" -ForegroundColor Red
    exit 1
} finally {
    Pop-Location
}
