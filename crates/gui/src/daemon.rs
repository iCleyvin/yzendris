//! Start / stop / status of the yzendris daemons from the GUI, by ROLE.
//!
//! The controlled binary depends on the role the user picked, not just the OS:
//!   - Server (host)  → yzendris-server.exe   (Windows only)
//!   - Client         → yzendris-client.exe   (Windows)  /  yzendris-client (Linux)
//!
//! Every call here forks a subprocess (tasklist/pgrep/systemctl/journalctl)
//! and would block whatever thread runs it. The egui render thread must never
//! call them directly — use `DaemonMonitor` (polls on a background thread) and
//! the `*_async` controls (act on a background thread).
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Which daemon a panel controls.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Server,
    Client,
}

#[cfg(windows)]
fn exe_name(t: Target) -> &'static str {
    match t {
        Target::Server => "yzendris-server.exe",
        Target::Client => "yzendris-client.exe",
    }
}
#[cfg(not(windows))]
fn exe_name(t: Target) -> &'static str {
    match t {
        Target::Server => "yzendris-server",
        Target::Client => "yzendris-client",
    }
}

fn config_path(t: Target) -> PathBuf {
    match t {
        Target::Server => crate::config_model::server_config_path(),
        Target::Client => crate::config_model::client_config_path(),
    }
}

fn log_path(t: Target) -> PathBuf {
    match t {
        Target::Server => crate::config_model::server_log_path(),
        Target::Client => crate::config_model::client_log_path(),
    }
}

// ─── Background monitor + async controls (UI-thread-safe) ────────────────────

/// Latest known daemon status, refreshed off the UI thread.
#[derive(Clone, Default)]
pub struct DaemonState {
    pub running: bool,
    pub log: String,
}

/// Polls daemon status + log on a background thread so the UI never blocks on
/// a subprocess. The UI reads `snapshot()` each frame (cheap mutex clone).
pub struct DaemonMonitor {
    state: Arc<Mutex<DaemonState>>,
}

impl DaemonMonitor {
    pub fn new(target: Target) -> Self {
        let state = Arc::new(Mutex::new(DaemonState {
            running: false,
            log: String::from("(consultando…)"),
        }));
        let shared = state.clone();
        std::thread::Builder::new()
            .name("yzendris-daemon-monitor".into())
            .spawn(move || loop {
                let running = daemon_running(target);
                let log = read_log_tail(target, 14);
                if let Ok(mut s) = shared.lock() {
                    *s = DaemonState { running, log };
                }
                std::thread::sleep(Duration::from_secs(2));
            })
            .ok();
        Self { state }
    }

    pub fn snapshot(&self) -> DaemonState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// Start the daemon on a background thread (returns immediately).
pub fn start_async(target: Target) {
    std::thread::spawn(move || {
        if let Err(e) = start_daemon(target) {
            tracing_warn(&format!("start_daemon: {e}"));
        }
    });
}

/// Stop the daemon on a background thread (returns immediately).
pub fn stop_async(target: Target) {
    std::thread::spawn(move || {
        let _ = stop_daemon(target);
    });
}

/// Stop, wait briefly, then start — all on a background thread so the 300 ms
/// settle never freezes the UI.
pub fn restart_async(target: Target) {
    std::thread::spawn(move || {
        let _ = stop_daemon(target);
        std::thread::sleep(Duration::from_millis(300));
        if let Err(e) = start_daemon(target) {
            tracing_warn(&format!("restart start_daemon: {e}"));
        }
    });
}

/// The GUI has no tracing subscriber; surface background errors on stderr.
fn tracing_warn(msg: &str) {
    eprintln!("[yzendris-gui] {msg}");
}

/// Build a Command that won't flash a console window on Windows.
fn quiet_command(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Locate the daemon binary: next to the GUI exe → install dir → dev build.
fn find_daemon_binary(name: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    candidates.push(crate::config_model::config_dir().join(name));
    #[cfg(not(windows))]
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin").join(name));
    }
    candidates.push(PathBuf::from("target/release").join(name));
    candidates.push(PathBuf::from("target/debug").join(name));

    candidates.into_iter().find(|p| p.exists())
}

// ─── Windows control ─────────────────────────────────────────────────────────

#[cfg(windows)]
pub fn daemon_running(target: Target) -> bool {
    let exe = exe_name(target);
    quiet_command("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {exe}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(exe))
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn start_daemon(target: Target) -> Result<(), String> {
    let exe = exe_name(target);
    let bin = find_daemon_binary(exe)
        .ok_or_else(|| format!("{exe} no encontrado (¿compilaste con cargo build --release?)"))?;
    let config = config_path(target);
    let mut cmd = quiet_command(bin.to_str().unwrap_or(exe));
    cmd.args(["--config", &config.to_string_lossy()]);

    // Both daemons log to their own file internally (server.log / client.log),
    // so no stdout/stderr capture is needed here.
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("no se pudo iniciar {exe}: {e}"))
}

#[cfg(windows)]
pub fn stop_daemon(target: Target) -> Result<(), String> {
    quiet_command("taskkill")
        .args(["/IM", exe_name(target), "/F"])
        .output()
        .map(|_| ())
        .map_err(|e| format!("taskkill falló: {e}"))
}

#[cfg(windows)]
pub fn read_log_tail(target: Target, max_lines: usize) -> String {
    let path = log_path(target);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            lines[start..].join("\n")
        }
        Err(_) => format!("(sin log todavía: {})", path.display()),
    }
}

// ─── Linux control ───────────────────────────────────────────────────────────

#[cfg(not(windows))]
fn systemd_unit_installed() -> bool {
    std::env::var_os("HOME")
        .map(|h| {
            PathBuf::from(h)
                .join(".config/systemd/user/yzendris-client.service")
                .exists()
        })
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn daemon_running(target: Target) -> bool {
    // The server is Windows-only; on Linux only the client is meaningful.
    if matches!(target, Target::Server) {
        return false;
    }
    quiet_command("pgrep")
        .args(["-x", exe_name(target)])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn start_daemon(target: Target) -> Result<(), String> {
    if matches!(target, Target::Server) {
        return Err("el servidor (host) solo corre en Windows".into());
    }
    if systemd_unit_installed() {
        let ok = quiet_command("systemctl")
            .args(["--user", "start", "yzendris-client.service"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Ok(());
        }
    }
    // Fallback: direct spawn (wrapper injects the Wayland env when present).
    let bin = find_daemon_binary("yzendris-client-wrapper.sh")
        .or_else(|| find_daemon_binary(exe_name(target)))
        .ok_or_else(|| format!("{} no encontrado", exe_name(target)))?;
    quiet_command(&bin.to_string_lossy())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("no se pudo iniciar el cliente: {e}"))
}

#[cfg(not(windows))]
pub fn stop_daemon(target: Target) -> Result<(), String> {
    if matches!(target, Target::Server) {
        return Ok(());
    }
    if systemd_unit_installed() {
        let _ = quiet_command("systemctl")
            .args(["--user", "stop", "yzendris-client.service"])
            .status();
    }
    let _ = quiet_command("pkill").args(["-x", exe_name(target)]).output();
    Ok(())
}

#[cfg(not(windows))]
pub fn read_log_tail(target: Target, max_lines: usize) -> String {
    if matches!(target, Target::Server) {
        return "(el servidor solo corre en Windows)".to_owned();
    }
    let out = quiet_command("journalctl")
        .args([
            "--user",
            "-u",
            "yzendris-client",
            "-n",
            &max_lines.to_string(),
            "--no-pager",
            "--output",
            "cat",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => "(sin acceso al log — ¿está instalada la unidad systemd?)".to_owned(),
    }
}
