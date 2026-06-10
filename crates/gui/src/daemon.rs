//! Start / stop / status of the yzendris daemons from the GUI.
//!
//! Windows: controls yzendris-server.exe (tasklist/taskkill, no console flash).
//! Linux:   controls yzendris-client via systemd user unit when installed,
//!          falling back to direct spawn / pkill.
//!
//! Every call here forks a subprocess (tasklist/pgrep/systemctl/journalctl)
//! and would block whatever thread runs it. The egui render thread must never
//! call them directly — use `DaemonMonitor` (polls on a background thread) and
//! the `*_async` controls (act on a background thread).
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(windows)]
const SERVER_EXE: &str = "yzendris-server.exe";
#[cfg(not(windows))]
const CLIENT_BIN: &str = "yzendris-client";

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
    pub fn new() -> Self {
        let state = Arc::new(Mutex::new(DaemonState {
            running: false,
            log: String::from("(consultando…)"),
        }));
        let shared = state.clone();
        std::thread::Builder::new()
            .name("yzendris-daemon-monitor".into())
            .spawn(move || loop {
                let running = daemon_running();
                let log = read_log_tail(14);
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
pub fn start_async() {
    std::thread::spawn(|| {
        if let Err(e) = start_daemon() {
            tracing_warn(&format!("start_daemon: {e}"));
        }
    });
}

/// Stop the daemon on a background thread (returns immediately).
pub fn stop_async() {
    std::thread::spawn(|| {
        let _ = stop_daemon();
    });
}

/// Stop, wait briefly, then start — all on a background thread so the 300 ms
/// settle never freezes the UI.
pub fn restart_async() {
    std::thread::spawn(|| {
        let _ = stop_daemon();
        std::thread::sleep(Duration::from_millis(300));
        if let Err(e) = start_daemon() {
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

// ─── Windows: server control ─────────────────────────────────────────────────

#[cfg(windows)]
pub fn daemon_running() -> bool {
    quiet_command("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {SERVER_EXE}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(SERVER_EXE))
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn start_daemon() -> Result<(), String> {
    let bin = find_daemon_binary(SERVER_EXE)
        .ok_or_else(|| format!("{SERVER_EXE} no encontrado (¿compilaste con cargo build --release?)"))?;
    let config = crate::config_model::server_config_path();
    quiet_command(bin.to_str().unwrap_or(SERVER_EXE))
        .args(["--config", &config.to_string_lossy()])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("no se pudo iniciar el servidor: {e}"))
}

#[cfg(windows)]
pub fn stop_daemon() -> Result<(), String> {
    quiet_command("taskkill")
        .args(["/IM", SERVER_EXE, "/F"])
        .output()
        .map(|_| ())
        .map_err(|e| format!("taskkill falló: {e}"))
}

/// Last lines of the daemon log.
#[cfg(windows)]
pub fn read_log_tail(max_lines: usize) -> String {
    let path = crate::config_model::server_log_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            lines[start..].join("\n")
        }
        Err(_) => format!("(sin log todavía: {})", path.display()),
    }
}

// ─── Linux: client control ───────────────────────────────────────────────────

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
pub fn daemon_running() -> bool {
    quiet_command("pgrep")
        .args(["-x", CLIENT_BIN])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn start_daemon() -> Result<(), String> {
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
        .or_else(|| find_daemon_binary(CLIENT_BIN))
        .ok_or_else(|| format!("{CLIENT_BIN} no encontrado"))?;
    quiet_command(&bin.to_string_lossy())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("no se pudo iniciar el cliente: {e}"))
}

#[cfg(not(windows))]
pub fn stop_daemon() -> Result<(), String> {
    if systemd_unit_installed() {
        let _ = quiet_command("systemctl")
            .args(["--user", "stop", "yzendris-client.service"])
            .status();
    }
    let _ = quiet_command("pkill").args(["-x", CLIENT_BIN]).output();
    Ok(())
}

#[cfg(not(windows))]
pub fn read_log_tail(max_lines: usize) -> String {
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
