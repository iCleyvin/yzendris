//! Per-user installation: copy the programs, start at login, and — with one
//! administrator prompt — grant the device access the chosen role needs.
//!
//! Nothing here runs with elevated rights except one short, fixed script
//! (Linux) or one firewall command (Windows). No user-controlled text is ever
//! interpolated into what runs elevated.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use yzendris_core::config::{Config, Role};
use yzendris_core::ipc::{call, Request, Response};
use yzendris_core::paths;

#[cfg(target_os = "linux")]
const ICON: &[u8] = include_bytes!("../../../assets/icon.png");

fn current_config() -> Config {
    match call(&Request::GetConfig) {
        Ok(Response::Config { config }) => *config,
        _ => Config::load(&paths::config_file()).unwrap_or_default(),
    }
}

fn copy_program(name: &str, into: &Path) -> Result<Option<PathBuf>> {
    let exe = std::env::current_exe()?;
    let source = exe.with_file_name(name);
    if !source.exists() {
        return Ok(None);
    }
    std::fs::create_dir_all(into)?;
    let target = into.join(name);
    if source != target {
        // Replace atomically so a running copy is never half-overwritten.
        let staging = into.join(format!(".{name}.new"));
        std::fs::copy(&source, &staging).with_context(|| format!("copying {name}"))?;
        std::fs::rename(&staging, &target).with_context(|| format!("installing {name}"))?;
    }
    Ok(Some(target))
}

fn step(done: &str) {
    println!("  ✓ {done}");
}

/// When the service starts before the graphical session (it is tied to the
/// login, not to a particular desktop), wait for the session and start over
/// inside it so the clipboard and screens can be reached.
pub async fn follow_session() {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some() {
            return;
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if crate::platform::display_available() {
                tracing::info!("graphical session detected; restarting inside it");
                if let Ok(exe) = std::env::current_exe() {
                    let error = Command::new(exe).arg("run").exec();
                    tracing::error!("could not restart: {error}");
                }
                return;
            }
        }
    }
}

// ── Linux ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod os {
    use super::*;
    use anyhow::bail;
    use std::io::IsTerminal;

    const UNIT: &str = "\
[Unit]
Description=Yzendris — keyboard and mouse sharing
Documentation=https://github.com/iCleyvin/yzendris

[Service]
ExecStart=%h/.local/bin/yzendris run
Restart=on-failure
RestartSec=3
# The service needs the network, input devices and your clipboard — nothing else.
NoNewPrivileges=yes
LockPersonality=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
SystemCallArchitectures=native

[Install]
WantedBy=default.target
";

    /// Runs as root. `$1` is `host` or `client`, `$2` the TCP port.
    const ROOT_SCRIPT: &str = r#"set -eu
ROLE="$1"; PORT="$2"
RULES=/etc/udev/rules.d/70-yzendris.rules
{
  echo '# Installed by Yzendris. Remove with: yzendris uninstall'
  echo '# Let the person at this computer create virtual input devices.'
  echo 'KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"'
  if [ "$ROLE" = host ]; then
    echo '# Host role: let the person at this computer read its keyboards and mice.'
    echo 'SUBSYSTEM=="input", KERNEL=="event*", TAG+="uaccess"'
  fi
} > "$RULES"
echo uinput > /etc/modules-load.d/yzendris.conf
modprobe uinput 2>/dev/null || true
udevadm control --reload-rules
udevadm trigger --subsystem-match=misc --subsystem-match=input || true
if [ "$ROLE" = host ]; then
  if command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then
    firewall-cmd --quiet --permanent --add-port="$PORT/tcp" --add-port="7547/udp"
    firewall-cmd --quiet --reload
  elif command -v ufw >/dev/null 2>&1 && ufw status | grep -q 'Status: active'; then
    for NET in 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16 fe80::/10; do
      ufw allow from "$NET" to any port "$PORT" proto tcp comment yzendris >/dev/null
      ufw allow from "$NET" to any port 7547 proto udp comment yzendris-discovery >/dev/null
    done
  fi
fi
"#;

    const ROOT_UNDO: &str = "\
rm -f /etc/udev/rules.d/70-yzendris.rules /etc/modules-load.d/yzendris.conf
udevadm control --reload-rules || true
";

    fn home() -> Result<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")
    }

    fn data_home() -> Result<PathBuf> {
        Ok(std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".local/share")))
    }

    fn unit_path() -> Result<PathBuf> {
        Ok(paths::config_dir()
            .parent()
            .context("no config home")?
            .join("systemd/user/yzendris.service"))
    }

    fn systemctl(args: &[&str]) -> bool {
        Command::new("systemctl")
            .arg("--user")
            .args(args)
            .status()
            .is_ok_and(|s| s.success())
    }

    fn can_inject() -> bool {
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .is_ok()
    }

    fn can_capture() -> bool {
        std::fs::read_dir("/dev/input").is_ok_and(|dir| {
            let nodes: Vec<_> = dir
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
                .collect();
            !nodes.is_empty() && nodes.iter().all(|e| std::fs::File::open(e.path()).is_ok())
        })
    }

    /// Run a fixed script as root, asking through the terminal or a dialog.
    fn as_root(script: &str, args: &[&str]) -> Result<()> {
        let mut cmd = if std::io::stdin().is_terminal() {
            let mut c = Command::new("sudo");
            c.arg("--");
            c
        } else {
            Command::new("pkexec")
        };
        let status = cmd
            .args(["sh", "-c", script, "yzendris"])
            .args(args)
            .status()
            .context("could not ask for administrator rights (need sudo or pkexec)")?;
        if !status.success() {
            bail!("the administrator step was cancelled or failed");
        }
        Ok(())
    }

    pub fn install(system: bool) -> Result<()> {
        let cfg = current_config();
        let bin = home()?.join(".local/bin");
        println!("Installing Yzendris for {}", whoami());

        let _ = call(&Request::Quit); // release the binary if a copy is running
        copy_program("yzendris", &bin)?.context("cannot find the yzendris program")?;
        let gui = copy_program("yzendris-gui", &bin)?;
        step(&format!("programs in {}", bin.display()));

        if let Some(gui) = &gui {
            let icons = data_home()?.join("icons/hicolor/256x256/apps");
            std::fs::create_dir_all(&icons)?;
            std::fs::write(icons.join("yzendris.png"), ICON)?;
            let apps = data_home()?.join("applications");
            std::fs::create_dir_all(&apps)?;
            std::fs::write(
                apps.join("yzendris.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName=Yzendris\n\
                     Comment=Share one keyboard and mouse across your computers\n\
                     Comment[es]=Comparte un teclado y un ratón entre tus equipos\n\
                     Exec=\"{}\"\nIcon=yzendris\nTerminal=false\n\
                     Categories=Utility;\nKeywords=kvm;keyboard;mouse;share;\n\
                     StartupWMClass=yzendris\n",
                    gui.display()
                ),
            )?;
            step("application menu entry");
        }

        let role = if cfg.role == Some(Role::Host) {
            "host"
        } else {
            "client"
        };
        let needs_root = !can_inject() || (cfg.role == Some(Role::Host) && !can_capture());
        let has_firewall = cfg.role == Some(Role::Host);
        if system && (needs_root || has_firewall) {
            println!("  … device access needs administrator rights once");
            match as_root(ROOT_SCRIPT, &[role, &cfg.host.port.to_string()]) {
                Ok(()) => step("device access granted (udev rule), firewall opened if active"),
                Err(e) => println!(
                    "  ! {e:#}\n    Run `yzendris install` again later, or see docs/INSTALL.md"
                ),
            }
        }

        let unit = unit_path()?;
        std::fs::create_dir_all(unit.parent().context("unit dir")?)?;
        std::fs::write(&unit, UNIT)?;
        if systemctl(&["daemon-reload"]) && systemctl(&["enable", "yzendris.service"]) {
            systemctl(&["restart", "yzendris.service"]);
            step("starts with your session (systemd user service)");
        } else {
            println!(
                "  ! systemd user services are unavailable; start it yourself with `yzendris run`"
            );
        }

        if !bin.to_string_lossy().is_empty() && !path_contains(&bin) {
            println!("  note: {} is not on your PATH", bin.display());
        }
        println!("\nDone. Open “Yzendris” from your applications to finish setting up.");
        Ok(())
    }

    fn path_contains(dir: &Path) -> bool {
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == dir))
    }

    fn whoami() -> String {
        std::env::var("USER").unwrap_or_else(|_| "this user".into())
    }

    pub fn uninstall() -> Result<()> {
        systemctl(&["disable", "--now", "yzendris.service"]);
        let _ = call(&Request::Quit);
        let data = data_home()?;
        let bin = home()?.join(".local/bin");
        for file in [
            unit_path()?,
            data.join("applications/yzendris.desktop"),
            data.join("icons/hicolor/256x256/apps/yzendris.png"),
            bin.join("yzendris-gui"),
            bin.join("yzendris"),
        ] {
            let _ = std::fs::remove_file(file);
        }
        systemctl(&["daemon-reload"]);
        step("service, menu entry and programs removed");
        if Path::new("/etc/udev/rules.d/70-yzendris.rules").exists() {
            match as_root(ROOT_UNDO, &[]) {
                Ok(()) => step("device access rule removed"),
                Err(_) => println!("  ! remove it yourself: sudo rm /etc/udev/rules.d/70-yzendris.rules /etc/modules-load.d/yzendris.conf"),
            }
        }
        println!(
            "Your settings and pairing remain in {}",
            paths::config_dir().display()
        );
        Ok(())
    }

    pub fn doctor() -> Result<()> {
        let cfg = current_config();
        let check = |ok: bool, good: &str, bad: &str| {
            println!(
                "  {} {}",
                if ok { "✓" } else { "✗" },
                if ok { good } else { bad }
            )
        };
        println!("Yzendris {} on Linux", yzendris_core::VERSION);
        check(
            call(&Request::Status).is_ok(),
            "service is running",
            "service is not running (yzendris run / yzendris install)",
        );
        check(
            can_inject(),
            "can create virtual input devices",
            "no access to /dev/uinput — run `yzendris install`",
        );
        if cfg.role != Some(Role::Client) {
            check(
                can_capture(),
                "can read keyboards and mice (host)",
                "cannot read /dev/input — needed to be a host; run `yzendris install` as host",
            );
            check(
                crate::platform::cursor_visible_to_host(),
                "cursor position is readable: switch by screen edge",
                "this desktop hides the cursor position: as a host, switch with Ctrl+Shift+Alt+arrows",
            );
        }
        let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into());
        println!("  · session: {session}, desktop: {desktop}");
        let screens = crate::platform::screens();
        check(
            !screens.is_empty(),
            &format!("{} screen(s) detected", screens.len()),
            "no screen detected",
        );
        for s in screens {
            println!(
                "      {} {}×{} at {},{}",
                s.name, s.rect.w, s.rect.h, s.rect.x, s.rect.y
            );
        }
        Ok(())
    }
}

// ── Windows ─────────────────────────────────────────────────────────────────

#[cfg(windows)]
mod os {
    use super::*;
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    fn install_dir() -> Result<PathBuf> {
        let base = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
        Ok(PathBuf::from(base).join("Programs").join("Yzendris"))
    }

    fn quiet(program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    /// Run a fixed PowerShell snippet. Values reach it through environment
    /// variables, never by being pasted into the script text.
    fn powershell(script: &str, vars: &[(&str, &std::ffi::OsStr)]) -> bool {
        let mut cmd = quiet("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ]);
        for (key, value) in vars {
            cmd.env(key, value);
        }
        cmd.status().is_ok_and(|s| s.success())
    }

    pub fn install(system: bool) -> Result<()> {
        let cfg = current_config();
        let dir = install_dir()?;
        println!("Installing Yzendris into {}", dir.display());
        let _ = call(&Request::Quit);
        std::thread::sleep(std::time::Duration::from_millis(600));

        let daemon = copy_program("yzendris.exe", &dir)?.context("cannot find yzendris.exe")?;
        let gui = copy_program("yzendris-gui.exe", &dir)?;
        step("programs copied");

        let run = format!("\"{}\" run", daemon.display());
        let added = quiet("reg")
            .args([
                "add", RUN_KEY, "/v", "Yzendris", "/t", "REG_SZ", "/d", &run, "/f",
            ])
            .status()
            .is_ok_and(|s| s.success());
        if added {
            step("starts when you sign in");
        }

        if let Some(gui) = &gui {
            let made = powershell(
                "$s=(New-Object -ComObject WScript.Shell).CreateShortcut(\
                 (Join-Path ([Environment]::GetFolderPath('Programs')) 'Yzendris.lnk'));\
                 $s.TargetPath=$env:YZ_TARGET;$s.WorkingDirectory=(Split-Path $env:YZ_TARGET);$s.Save()",
                &[("YZ_TARGET", gui.as_os_str())],
            );
            if made {
                step("Start menu shortcut");
            }
        }

        if system && cfg.role == Some(Role::Host) {
            println!("  … opening the firewall needs administrator rights once");
            let opened = powershell(
                "$a='advfirewall firewall add rule name=Yzendris dir=in action=allow enable=yes \
                 profile=private,domain program=\"'+$env:YZ_TARGET+'\"';\
                 Start-Process netsh -Verb RunAs -WindowStyle Hidden -Wait -ArgumentList $a",
                &[("YZ_TARGET", daemon.as_os_str())],
            );
            if opened {
                step("firewall rule for private networks");
            } else {
                println!(
                    "  ! firewall not changed; Windows will ask the first time a device connects"
                );
            }
        }

        quiet(&daemon.to_string_lossy())
            .arg("run")
            .spawn()
            .context("starting the service")?;
        step("service started");
        println!("\nDone. Open “Yzendris” from the Start menu to finish setting up.");
        Ok(())
    }

    pub fn uninstall() -> Result<()> {
        let _ = call(&Request::Quit);
        std::thread::sleep(std::time::Duration::from_millis(600));
        let _ = quiet("reg")
            .args(["delete", RUN_KEY, "/v", "Yzendris", "/f"])
            .status();
        powershell(
            "Remove-Item (Join-Path ([Environment]::GetFolderPath('Programs')) 'Yzendris.lnk') -ErrorAction SilentlyContinue",
            &[],
        );
        if let Ok(dir) = install_dir() {
            let _ = std::fs::remove_file(dir.join("yzendris-gui.exe"));
            // The running uninstaller cannot delete itself; schedule nothing, just say so.
            if std::fs::remove_file(dir.join("yzendris.exe")).is_err() {
                println!("  note: delete {} once this window closes", dir.display());
            }
        }
        step("autostart and shortcuts removed");
        println!(
            "  The firewall rule “Yzendris”, if you added it, can be removed in Windows Security."
        );
        println!(
            "Your settings and pairing remain in {}",
            paths::config_dir().display()
        );
        Ok(())
    }

    pub fn doctor() -> Result<()> {
        println!("Yzendris {} on Windows", yzendris_core::VERSION);
        let running = call(&Request::Status).is_ok();
        println!(
            "  {} service {}",
            if running { "✓" } else { "✗" },
            if running {
                "is running"
            } else {
                "is not running"
            }
        );
        for s in crate::platform::screens() {
            println!(
                "  · screen {} {}×{} at {},{}",
                s.name, s.rect.w, s.rect.h, s.rect.x, s.rect.y
            );
        }
        println!("  · input reaches elevated (administrator) windows only if Yzendris itself runs elevated");
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod os {
    use super::*;
    use anyhow::bail;

    pub fn install(_system: bool) -> Result<()> {
        bail!("no installer for this operating system yet")
    }
    pub fn uninstall() -> Result<()> {
        bail!("no installer for this operating system yet")
    }
    pub fn doctor() -> Result<()> {
        println!(
            "Yzendris {} — this operating system is not supported yet",
            yzendris_core::VERSION
        );
        Ok(())
    }
}

pub use os::{doctor, install, uninstall};
