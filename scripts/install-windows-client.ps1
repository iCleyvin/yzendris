# Install yzendris-client on Windows (the laptop, when booted into Windows).
# Run from the repo root: .\scripts\install-windows-client.ps1
# Requires PowerShell 7+ and a compiled yzendris-client.exe.
#
# This is the CLIENT side: it listens for the host PC and injects keyboard/mouse
# via SendInput. (The server/host installer is install-windows.ps1.)
#
# Usage:
#   .\scripts\install-windows-client.ps1 [-BinaryPath .\target\release\yzendris-client.exe] [-Port 7547] [-Startup]

[CmdletBinding()]
param(
    [string]$BinaryPath = ".\target\release\yzendris-client.exe",
    [string]$GuiPath    = ".\target\release\yzendris-gui.exe",
    [int]   $Port       = 7547,
    [switch]$Startup
)

$ErrorActionPreference = "Stop"

$InstallDir = "$env:APPDATA\yzendris"
$Dest       = "$InstallDir\yzendris-client.exe"
$GuiDest    = "$InstallDir\yzendris-gui.exe"

New-Item -ItemType Directory -Force $InstallDir | Out-Null
Copy-Item -Force $BinaryPath $Dest
Write-Host "✓ installed $Dest"

if (Test-Path $GuiPath) {
    Copy-Item -Force $GuiPath $GuiDest
    $StartMenu = [Environment]::GetFolderPath("Programs")
    $GuiLnk    = "$StartMenu\Yzendris KVM.lnk"
    $Wsh = New-Object -ComObject WScript.Shell
    $Lnk = $Wsh.CreateShortcut($GuiLnk)
    $Lnk.TargetPath       = $GuiDest
    $Lnk.WorkingDirectory = $InstallDir
    $Lnk.IconLocation     = "$GuiDest, 0"
    $Lnk.Description      = "Yzendris KVM configurator"
    $Lnk.Save()
    Write-Host "✓ installed $GuiDest + start menu shortcut"
} else {
    Write-Host "  (GUI not found at $GuiPath — build with: cargo build --release -p yzendris-gui)"
}

# ── Default client config ─────────────────────────────────────────────────────
$CfgFile = "$InstallDir\client.toml"
if (-not (Test-Path $CfgFile)) {
    @"
# yzendris client (Windows) — listens for the host PC and injects input.
port = $Port
bind_addr = "0.0.0.0"
heartbeat_timeout_ms = 5000
clipboard = true
# TLS: on first run with tls=true a cert is generated and its fingerprint is
# shown in the GUI (Cliente panel) — add it to the host's trusted_peers.txt.
tls = true
"@ | Set-Content $CfgFile -Encoding UTF8
    Write-Host "✓ wrote default config to $CfgFile"
}

# ── Inbound firewall rule (the host connects IN to this client) ───────────────
$RuleName = "Yzendris KVM inbound"
if (-not (Get-NetFirewallRule -DisplayName $RuleName -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule `
        -DisplayName $RuleName `
        -Direction   Inbound `
        -Protocol    TCP `
        -LocalPort   $Port `
        -Action      Allow `
        -Profile     Private,Domain | Out-Null
    Write-Host "✓ firewall: inbound TCP $Port allowed"
} else {
    Write-Host "  (firewall rule already exists)"
}

# ── Optional startup entry ────────────────────────────────────────────────────
if ($Startup) {
    $StartupDir = [Environment]::GetFolderPath("Startup")
    $LnkPath    = "$StartupDir\Yzendris KVM Client.lnk"
    $Wsh2 = New-Object -ComObject WScript.Shell
    $Lnk2 = $Wsh2.CreateShortcut($LnkPath)
    $Lnk2.TargetPath       = $Dest
    $Lnk2.Arguments        = "--config `"$CfgFile`""
    $Lnk2.WorkingDirectory = $InstallDir
    $Lnk2.IconLocation     = "$Dest, 0"
    $Lnk2.Description      = "Yzendris KVM Client"
    $Lnk2.Save()
    Write-Host "✓ startup shortcut: $LnkPath"
}

Write-Host ""
Write-Host "Done. Launch the GUI (Start Menu → Yzendris KVM), pick 'Cliente', then Start."
Write-Host "Or run directly:  & '$Dest' --config '$CfgFile'"
