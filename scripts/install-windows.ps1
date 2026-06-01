# Install yzendris-server on Windows 11.
# Run from the repo root: .\scripts\install-windows.ps1
# Requires PowerShell 7+ and a compiled yzendris-server.exe.
#
# Usage:
#   .\scripts\install-windows.ps1 [-BinaryPath .\target\release\yzendris-server.exe] [-Port 7547]

[CmdletBinding()]
param(
    [string]$BinaryPath = ".\target\release\yzendris-server.exe",
    [int]   $Port       = 7547
)

$ErrorActionPreference = "Stop"

# ── 1. Destination ────────────────────────────────────────────────────────────
# Use %APPDATA%\yzendris so config and binary live together.
$InstallDir = "$env:APPDATA\yzendris"
$Dest       = "$InstallDir\yzendris-server.exe"

New-Item -ItemType Directory -Force $InstallDir | Out-Null
Copy-Item -Force $BinaryPath $Dest
Write-Host "✓ installed $Dest"

# ── 2. Default config ─────────────────────────────────────────────────────────
$CfgFile = "$InstallDir\server.toml"
if (-not (Test-Path $CfgFile)) {
    $ExampleCfg = Join-Path (Split-Path $PSCommandPath) "..\config\server.example.toml"
    Copy-Item $ExampleCfg $CfgFile
    Write-Host "✓ wrote default config to $CfgFile"
    Write-Host "  → Edit $CfgFile to set client_addr to your laptop's IP."
} else {
    Write-Host "  (config already exists at $CfgFile — not overwritten)"
}

# ── 3. Firewall rule (outbound TCP to laptop) ─────────────────────────────────
$RuleName = "Yzendris KVM outbound"
if (-not (Get-NetFirewallRule -DisplayName $RuleName -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule `
        -DisplayName $RuleName `
        -Direction   Outbound `
        -Protocol    TCP `
        -RemotePort  $Port `
        -Action      Allow `
        -Profile     Private,Domain | Out-Null
    Write-Host "✓ firewall: outbound TCP $Port allowed"
} else {
    Write-Host "  (firewall rule already exists)"
}

# ── 4. Startup launcher (VBS, fully hidden — no console flash) ───────────────
$StartupDir = [Environment]::GetFolderPath("Startup")
$VbsPath    = "$StartupDir\yzendris-server.vbs"
$LnkPath    = "$StartupDir\Yzendris KVM Server.lnk"

# Remove stale .lnk if it exists alongside our .vbs (avoids duplicate launches).
if (Test-Path $LnkPath) { Remove-Item $LnkPath -Force }

if (-not (Test-Path $VbsPath)) {
    $vbs = @"
' yzendris-server launcher — runs hidden, no console window on login.
Set WshShell = CreateObject("WScript.Shell")
exePath = WshShell.ExpandEnvironmentStrings("%APPDATA%\yzendris\yzendris-server.exe")
cfgPath = WshShell.ExpandEnvironmentStrings("%APPDATA%\yzendris\server.toml")
WshShell.Run """" & exePath & """ --config """ & cfgPath & """", 0, False
"@
    Set-Content -Path $VbsPath -Value $vbs -Encoding UTF8
    Write-Host "✓ startup launcher created: $VbsPath"
} else {
    Write-Host "  (startup launcher already exists — not overwritten)"
}

Write-Host ""
Write-Host "Installation complete."
Write-Host "Launch now:  & '$Dest' --config '$InstallDir\server.toml'"
Write-Host "Or restart Windows to auto-start."
Write-Host ""
Write-Host "TLS setup (optional):"
Write-Host "  1. Set tls=true in $CfgFile"
Write-Host "  2. Start the Linux client with tls=true — it prints a fingerprint."
Write-Host "  3. Add that fingerprint to $InstallDir\trusted_peers.txt"
Write-Host "  4. Restart yzendris-server."
