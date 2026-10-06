//! Talks to the daemon on a background thread so the interface never waits
//! on I/O. The UI reads a snapshot each frame and queues commands.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui;
use yzendris_core::config::Config;
use yzendris_core::ipc::{Client, FoundHost, Request, Response, Status};
use yzendris_core::Fingerprint;

pub enum Cmd {
    SetConfig(Box<Config>),
    PairOpen,
    PairClose,
    Join {
        addr: String,
        code: String,
    },
    Forget(Fingerprint),
    /// Run `yzendris install` (autostart + device permissions).
    Install,
}

#[derive(Default)]
pub struct Shared {
    /// The daemon answered the last poll.
    pub online: bool,
    pub status: Option<Status>,
    pub config: Option<Config>,
    pub logs: Vec<String>,
    pub hosts: Vec<FoundHost>,
    /// Last failed action, shown once.
    pub error: Option<String>,
    /// A pairing attempt is in flight.
    pub joining: bool,
    pub installing: bool,
    pub install_result: Option<Result<(), String>>,
    /// What the visible screen needs refreshed.
    pub want_logs: bool,
    pub want_hosts: bool,
    /// When the UI last painted; polling pauses while the window is hidden.
    pub last_frame: Option<Instant>,
}

#[derive(Clone)]
pub struct Backend {
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::Sender<Cmd>,
}

fn sibling(name: &str) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let path = std::env::current_exe().ok()?.with_file_name(name);
    path.exists().then_some(path)
}

/// Start the daemon if it is not running: through systemd when installed that
/// way, otherwise directly.
fn start_daemon() {
    #[cfg(target_os = "linux")]
    {
        let unit = yzendris_core::paths::config_dir()
            .parent()
            .map(|p| p.join("systemd/user/yzendris.service"));
        if unit.is_some_and(|u| u.exists()) && std::env::var_os("YZENDRIS_HOME").is_none() {
            let started = Command::new("systemctl")
                .args(["--user", "start", "yzendris.service"])
                .status()
                .is_ok_and(|s| s.success());
            if started {
                return;
            }
        }
    }
    if let Some(daemon) = sibling("yzendris") {
        let mut cmd = Command::new(daemon);
        cmd.arg("run")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let _ = cmd.spawn();
    }
}

fn run_installer() -> Result<(), String> {
    let daemon = sibling("yzendris").ok_or("yzendris program not found next to the app")?;
    let mut cmd = Command::new(daemon);
    cmd.arg("install").stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    match text.lines().find(|l| l.trim_start().starts_with('!')) {
        Some(problem) => Err(problem.trim_start_matches([' ', '!']).to_owned()),
        None if out.status.success() => Ok(()),
        None => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
    }
}

impl Backend {
    pub fn start(ctx: egui::Context) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = mpsc::channel::<Cmd>();
        let state = shared.clone();
        let _ = std::thread::Builder::new()
            .name("daemon-link".into())
            .spawn(move || {
                let mut client: Option<Client> = None;
                let mut last_start = Instant::now() - Duration::from_secs(60);
                let lock = || state.lock().unwrap_or_else(|p| p.into_inner());
                loop {
                    let cmd = match rx.recv_timeout(Duration::from_millis(1000)) {
                        Ok(cmd) => Some(cmd),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    };
                    let visible = lock()
                        .last_frame
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(3));
                    if cmd.is_none() && !visible {
                        continue; // nothing to show anyone
                    }
                    if client.is_none() {
                        client = Client::connect().ok();
                        if client.is_none() && last_start.elapsed() > Duration::from_secs(5) {
                            last_start = Instant::now();
                            start_daemon();
                        }
                    }
                    let mut call = |request: Request| -> Option<Response> {
                        let reply = client.as_mut()?.call(&request);
                        if reply.is_err() {
                            client = None;
                        }
                        reply.ok()
                    };

                    match cmd {
                        Some(Cmd::Install) => {
                            lock().installing = true;
                            ctx.request_repaint();
                            let result = run_installer();
                            let mut s = lock();
                            s.installing = false;
                            s.install_result = Some(result);
                            continue;
                        }
                        Some(cmd) => {
                            let joining = matches!(cmd, Cmd::Join { .. });
                            if joining {
                                lock().joining = true;
                                ctx.request_repaint();
                            }
                            let request = match cmd {
                                Cmd::SetConfig(config) => Request::SetConfig { config },
                                Cmd::PairOpen => Request::PairOpen,
                                Cmd::PairClose => Request::PairClose,
                                Cmd::Join { addr, code } => Request::PairJoin { addr, code },
                                Cmd::Forget(fingerprint) => Request::Forget { fingerprint },
                                Cmd::Install => unreachable!("handled above"),
                            };
                            let reply = call(request);
                            let mut s = lock();
                            s.joining = false;
                            if let Some(Response::Error { message }) = reply {
                                s.error = Some(message);
                            }
                        }
                        None => {}
                    }

                    let (want_logs, want_hosts) = {
                        let s = lock();
                        (s.want_logs, s.want_hosts)
                    };
                    let status = call(Request::Status);
                    let config = call(Request::GetConfig);
                    let logs = if want_logs { call(Request::Logs) } else { None };
                    let hosts = if want_hosts {
                        call(Request::Discover)
                    } else {
                        None
                    };
                    let mut s = lock();
                    s.online = status.is_some();
                    if let Some(Response::Status(st)) = status {
                        s.status = Some(*st);
                    }
                    if let Some(Response::Config { config }) = config {
                        s.config = Some(*config);
                    }
                    if let Some(Response::Logs { lines }) = logs {
                        s.logs = lines;
                    }
                    if let Some(Response::Hosts { hosts }) = hosts {
                        s.hosts = hosts;
                    }
                    drop(s);
                    ctx.request_repaint();
                }
            });
        Self { shared, tx }
    }

    /// A backend that never talks to a daemon, for demo scenes.
    pub fn offline(shared: Shared) -> Self {
        let (tx, _rx) = mpsc::channel();
        Self {
            shared: Arc::new(Mutex::new(shared)),
            tx,
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// Apply a change to the configuration: shown immediately, then sent to
    /// the daemon, whose answer is authoritative.
    pub fn edit(&self, change: impl FnOnce(&mut Config)) {
        let mut s = self.lock();
        let Some(config) = s.config.as_mut() else {
            return;
        };
        change(config);
        let config = Box::new(config.clone());
        drop(s);
        self.send(Cmd::SetConfig(config));
    }
}
