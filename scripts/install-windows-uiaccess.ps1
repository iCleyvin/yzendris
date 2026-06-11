<#
.SYNOPSIS
  Instala yzendris-client.exe como aplicación UIAccess para que el mouse/teclado
  inyectado pueda tocar prompts de UAC y el Secure Desktop.

.DESCRIPTION
  UAC (consent.exe) corre a integridad System; UIPI bloquea el input inyectado de
  un proceso inferior, incluso uno elevado. La única forma de cruzar esa frontera
  (sin desactivar UAC) es UIAccess, que requiere las 4 condiciones de abajo. Este
  script las cumple todas y deja el cliente corriendo como tarea programada.

  Requisitos para que Windows CONCEDA uiAccess:
    1. Manifest del exe con uiAccess="true"  (ya embebido por crates/client/build.rs)
    2. Exe Authenticode-firmado por un cert en LocalMachine\Root
    3. Exe en ubicación segura (%ProgramFiles%)
    4. UAC encendido (EnableLUA=1)

  Gotcha: Task Scheduler NO puede lanzar un exe uiAccess directo (devuelve
  0x800702E4 ERROR_ELEVATION_REQUIRED). Hay que lanzarlo por el broker ShellExecute;
  por eso la tarea corre wscript sobre un .vbs y va con RunLevel=Limited.

.NOTES
  Ejecutar ELEVADO (admin) en la máquina cliente Windows.
#>
[CmdletBinding()]
param(
    # Ruta al yzendris-client.exe recién compilado (target\release\…).
    [Parameter(Mandatory)] [string] $ExePath,
    # Config TOML que el cliente debe usar (queda en %APPDATA%, no se mueve).
    [string] $ConfigPath = "$env:APPDATA\yzendris\client.toml",
    [string] $TaskName   = "Yzendris Client",
    [string] $CertSubject = "CN=Yzendris KVM Code Signing"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $ExePath)) { throw "no existe: $ExePath" }

# ── 1. Cert de code-signing (reutiliza si ya existe) ─────────────────────────
$cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object Subject -eq $CertSubject | Select-Object -First 1
if (-not $cert) {
    Write-Host "creando cert self-signed $CertSubject…"
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $CertSubject `
        -CertStoreLocation Cert:\CurrentUser\My -KeyUsage DigitalSignature `
        -KeyExportPolicy Exportable -NotAfter (Get-Date).AddYears(10)
}

# ── 2. Firmar el exe ─────────────────────────────────────────────────────────
$sig = Set-AuthenticodeSignature -FilePath $ExePath -Certificate $cert -HashAlgorithm SHA256
Write-Host "firma aplicada (status host sin trust: $($sig.Status))"

# ── 3. Confiar el cert en la máquina (Root + TrustedPublisher) ───────────────
$cerTmp = Join-Path $env:TEMP "yzendris-codesign.cer"
Export-Certificate -Cert $cert -FilePath $cerTmp -Force | Out-Null
Import-Certificate -FilePath $cerTmp -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
Import-Certificate -FilePath $cerTmp -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
[System.IO.File]::Delete($cerTmp)
Write-Host "cert confiado en LocalMachine\Root + TrustedPublisher"

# ── 4. Instalar en ubicación segura (%ProgramFiles%) ─────────────────────────
$prog = Join-Path $env:ProgramFiles "Yzendris"
$dst  = Join-Path $prog "yzendris-client.exe"
New-Item -ItemType Directory -Force -Path $prog | Out-Null
Copy-Item $ExePath $dst -Force
$v = Get-AuthenticodeSignature $dst
if ($v.Status -ne "Valid") { throw "la firma no validó tras instalar: $($v.Status)" }
Write-Host "exe instalado y firma Valid: $dst"

# ── 5. Launcher VBS (ShellExecute = broker que concede uiAccess) ─────────────
$vbs = Join-Path $env:APPDATA "yzendris\launch-uiaccess.vbs"
New-Item -ItemType Directory -Force -Path (Split-Path $vbs) | Out-Null
$vbsLines = @(
    'Set sh = CreateObject("Shell.Application")',
    ('sh.ShellExecute "' + $dst + '", "--config ""' + $ConfigPath + '""", "", "open", 0')
)
Set-Content -Path $vbs -Value $vbsLines -Encoding ASCII

# ── 6. Tarea programada: wscript launcher, RunLevel Limited, al iniciar sesión ─
$action    = New-ScheduledTaskAction -Execute "wscript.exe" -Argument ('"' + $vbs + '"')
$trigger   = New-ScheduledTaskTrigger -AtLogOn
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
$settings  = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -Hidden
Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings -Force | Out-Null
Write-Host "tarea '$TaskName' registrada (wscript + RunLevel Limited)"

# ── 7. Arrancar y verificar uiAccess concedido ───────────────────────────────
Stop-Process -Name yzendris-client -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400
schtasks /Run /TN $TaskName | Out-Null
Start-Sleep -Seconds 4
$p = Get-Process yzendris-client -ErrorAction SilentlyContinue
if (-not $p) { throw "el cliente no arrancó (revisar LastTaskResult)" }

Add-Type @"
using System;using System.Runtime.InteropServices;
public class YzTok{
 [DllImport("advapi32.dll",SetLastError=true)] public static extern bool OpenProcessToken(IntPtr h,uint a,out IntPtr t);
 [DllImport("advapi32.dll",SetLastError=true)] public static extern bool GetTokenInformation(IntPtr t,int c,out uint info,uint len,out uint ret);
 [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
}
"@
[IntPtr]$tok = 0
[void][YzTok]::OpenProcessToken($p.Handle, 0x0008, [ref]$tok)   # TOKEN_QUERY
$info = 0; $ret = 0
[void][YzTok]::GetTokenInformation($tok, 26, [ref]$info, 4, [ref]$ret)  # TokenUIAccess=26
[void][YzTok]::CloseHandle($tok)

if ($info -ne 0) {
    Write-Host "OK: UIAccess CONCEDIDO (pid $($p.Id)) — el cliente puede tocar prompts de UAC" -ForegroundColor Green
} else {
    Write-Warning "UIAccess NO concedido — revisar firma/ubicación/cert (los 4 requisitos)"
}
