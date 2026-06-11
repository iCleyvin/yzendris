//! One-click setup so a fresh user gets a working, persistent install entirely
//! from the GUI — no terminal needed:
//!   - copy the daemon next to the config (%APPDATA%\yzendris)
//!   - open the firewall (outbound for the host, inbound for the client)
//!   - enable autostart (Startup shortcut for the host, scheduled task for the
//!     client so it runs hidden and elevated)
//!
//! On Windows these need administrator rights, so we run them through a single
//! elevated PowerShell (one UAC prompt). On Linux the client uses a systemd
//! user unit (no root) and tries `ufw` for the firewall.
use crate::config_model::config_dir;
use crate::daemon::Target;

/// Human-readable result of a setup attempt.
pub type SetupResult = Result<String, String>;

// ─── Windows ─────────────────────────────────────────────────────────────────

#[cfg(windows)]
pub fn enable(target: Target, port: u16) -> SetupResult {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("crear carpeta: {e}"))?;

    let exe_name = match target {
        Target::Server => "yzendris-server.exe",
        Target::Client => "yzendris-client.exe",
    };
    let src = crate::daemon::locate_binary(exe_name)
        .ok_or_else(|| format!("{exe_name} no encontrado (ponlo junto a la GUI)"))?;
    let dst = dir.join(exe_name);
    let cfg = match target {
        Target::Server => dir.join("server.toml"),
        Target::Client => dir.join("client.toml"),
    };
    let dir_s = dir.display().to_string();
    let src_s = src.display().to_string();
    let dst_s = dst.display().to_string();
    let cfg_s = cfg.display().to_string();

    let script = match target {
        Target::Server => format!(
            "$ErrorActionPreference='SilentlyContinue'\n\
             Copy-Item -Force '{src_s}' '{dst_s}'\n\
             if(-not(Get-NetFirewallRule -DisplayName 'Yzendris KVM outbound')){{\
               New-NetFirewallRule -DisplayName 'Yzendris KVM outbound' -Direction Outbound \
               -Protocol TCP -RemotePort {port} -Action Allow -Profile Any}}\n\
             $startup=[Environment]::GetFolderPath('Startup')\n\
             $w=New-Object -ComObject WScript.Shell\n\
             $s=$w.CreateShortcut(\"$startup\\Yzendris KVM Server.lnk\")\n\
             $s.TargetPath='{dst_s}'\n\
             $s.Arguments='--config \"{cfg_s}\"'\n\
             $s.WorkingDirectory='{dir_s}'\n\
             $s.IconLocation='{dst_s}, 0'\n\
             $s.Save()\n"
        ),
        Target::Client => format!(
            "$ErrorActionPreference='SilentlyContinue'\n\
             Copy-Item -Force '{src_s}' '{dst_s}'\n\
             if(-not(Get-NetFirewallRule -DisplayName 'Yzendris KVM inbound')){{\
               New-NetFirewallRule -DisplayName 'Yzendris KVM inbound' -Direction Inbound \
               -Protocol TCP -LocalPort {port} -Action Allow -Profile Any}}\n\
             $a=New-ScheduledTaskAction -Execute '{dst_s}' -Argument '--config \"{cfg_s}\"'\n\
             $t=New-ScheduledTaskTrigger -AtLogOn\n\
             $p=New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Highest\n\
             $set=New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
             -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew\n\
             Register-ScheduledTask -TaskName 'Yzendris Client' -Action $a -Trigger $t \
             -Principal $p -Settings $set -Force\n"
        ),
    };

    run_elevated(&script)?;
    Ok(format!(
        "✔ Instalado en {dir_s}: firewall + inicio automático listos."
    ))
}

/// Write the script to a temp file and run it via an elevated PowerShell
/// (single UAC prompt). Blocks until it finishes — call from a worker thread.
#[cfg(windows)]
fn run_elevated(script: &str) -> Result<(), String> {
    let dir = config_dir();
    let path = dir.join("yz-setup.ps1");
    std::fs::write(&path, script).map_err(|e| format!("escribir script: {e}"))?;

    // -Verb RunAs triggers UAC; -Wait so we know when it's done.
    let inner = format!(
        "Start-Process powershell -Verb RunAs -Wait -ArgumentList \
         '-NoProfile','-ExecutionPolicy','Bypass','-File','\"{}\"'",
        path.display()
    );

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &inner])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("lanzar PowerShell elevado: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("se canceló el permiso de administrador (UAC) o falló la instalación".into())
    }
}

// ─── Linux (client only; the host is Windows-only) ───────────────────────────

#[cfg(not(windows))]
pub fn enable(target: Target, port: u16) -> SetupResult {
    use std::process::Command;

    if matches!(target, Target::Server) {
        return Err("el host (servidor) solo corre en Windows".into());
    }

    let mut notes = Vec::new();

    // systemd user unit (no root). Requires the unit installed by
    // install-linux.sh; if absent, tell the user to run it once.
    let unit = std::env::var_os("HOME").map(|h| {
        std::path::PathBuf::from(h).join(".config/systemd/user/yzendris-client.service")
    });
    if unit.as_ref().map(|u| u.exists()).unwrap_or(false) {
        let _ = Command::new("systemctl")
            .args(["--user", "enable", "--now", "yzendris-client.service"])
            .status();
        notes.push("systemd: unit habilitada".to_string());
    } else {
        notes.push("(corre scripts/install-linux.sh una vez para el autoarranque systemd)".to_string());
    }

    // Firewall via ufw, asking for privileges through pkexec if available.
    if Command::new("sh")
        .arg("-c")
        .arg("command -v ufw >/dev/null")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        let cmd = format!("ufw allow {port}/tcp comment yzendris");
        let ok = Command::new("pkexec")
            .args(["sh", "-c", &cmd])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        notes.push(if ok {
            format!("ufw: puerto {port}/tcp abierto")
        } else {
            format!("ufw: abre el puerto manualmente: sudo ufw allow {port}/tcp")
        });
    }

    let _ = config_dir();
    Ok(notes.join(" · "))
}
