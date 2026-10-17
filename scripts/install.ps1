# Yzendris installer for Windows 10/11.
#
#   irm https://raw.githubusercontent.com/iCleyvin/yzendris/main/scripts/install.ps1 | iex
#
# Downloads the latest release, checks it against the published SHA-256, and
# runs `yzendris install`, which copies the programs to
# %LOCALAPPDATA%\Programs\Yzendris and starts them when you sign in. No
# administrator rights are needed, except one optional prompt to add a
# firewall rule when this PC is the host.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'iCleyvin/yzendris'
$name = 'yzendris-windows-x86_64'
$base = "https://github.com/$repo/releases/latest/download"
$tmp  = Join-Path ([IO.Path]::GetTempPath()) ("yzendris-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    Write-Host "Downloading $name…"
    Invoke-WebRequest "$base/$name.zip" -OutFile "$tmp\$name.zip" -UseBasicParsing
    Invoke-WebRequest "$base/$name.zip.sha256" -OutFile "$tmp\$name.zip.sha256" -UseBasicParsing
    $expected = ((Get-Content "$tmp\$name.zip.sha256" -Raw) -split '\s+')[0].ToLower()
    $actual   = (Get-FileHash "$tmp\$name.zip" -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw 'Checksum mismatch - download corrupted. Aborting.' }
    Expand-Archive "$tmp\$name.zip" -DestinationPath $tmp -Force
    $exe = Join-Path $tmp "$name\yzendris.exe"
    $run = Start-Process -FilePath $exe -ArgumentList 'install' -Wait -PassThru -NoNewWindow
    if ($run.ExitCode -ne 0) { throw "Installation failed (exit code $($run.ExitCode))." }
    Write-Host 'Installed. Open "Yzendris" from the Start menu.'
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
