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

# ── 4. Startup launcher ───────────────────────────────────────────────────────
# Strategy: keep the .vbs for the actual silent launch (WindowStyle=0, no flash)
# but wrap it in a .lnk shortcut whose icon points to the .exe.
# Result: Task Manager / Settings > Startup shows our app icon, not the VBS icon.
$StartupDir = [Environment]::GetFolderPath("Startup")
$LnkPath    = "$StartupDir\Yzendris KVM Server.lnk"

# Remove any stale .vbs launchers left in Startup from older installs.
Remove-Item "$StartupDir\yzendris-server.vbs" -Force -ErrorAction SilentlyContinue

# Direct .lnk → .exe (no console window because the exe uses windows_subsystem="windows").
# Windows shows the app name and embedded icon correctly in Task Manager / Startup settings.
$Wsh = New-Object -ComObject WScript.Shell
$Lnk = $Wsh.CreateShortcut($LnkPath)
$Lnk.TargetPath       = $Dest
$Lnk.Arguments        = "--config `"$InstallDir\server.toml`""
$Lnk.WorkingDirectory = $InstallDir
$Lnk.IconLocation     = "$Dest, 0"
$Lnk.Description      = "Yzendris KVM Server"
$Lnk.Save()

Write-Host "✓ startup shortcut: $LnkPath"

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
