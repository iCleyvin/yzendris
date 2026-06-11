// yzendris-client — receives keyboard/mouse from the Windows server and injects
// them locally. Works on the Linux laptop (uinput + Hyprland) and, when the
// laptop is booted into Windows, on Windows (SendInput). The platform backend
// is selected at compile time in `platform`.

// No console window on Windows (startup task, double-click, etc). Logging goes
// to %APPDATA%\yzendris\client.log instead of a terminal.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod clipboard;
mod net;
mod platform;
mod tls;
mod tracker;

// Linux-only backends used by platform::linux.
#[cfg(target_os = "linux")]
mod hyprland;
#[cfg(target_os = "linux")]
mod uinput;

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use yzendris_protocol::Event;

// ─── Config ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct Config {
    /// Port to listen on (default 7547).
    #[serde(default = "default_port")]
    port: u16,

    /// Address to bind on (default "0.0.0.0").
    #[serde(default = "default_bind")]
    bind_addr: String,

    /// XKB layout for the virtual device (Linux only; ignored on Windows).
    /// Empty string = auto-detect from `hyprctl devices -j`.
    #[serde(default)]
    kb_layout: String,

    /// Milliseconds without a heartbeat before considering the server dead.
    #[serde(default = "default_heartbeat_timeout")]
    heartbeat_timeout_ms: u64,

    /// Sync clipboard on capture transitions.
    #[serde(default = "default_clipboard")]
    clipboard: bool,

    /// Enable TLS. When true, generates a self-signed cert on first run and
    /// requires the server to have the fingerprint in trusted_peers.txt.
    #[serde(default)]
    tls: bool,
}

fn default_port() -> u16 { 7547 }
fn default_bind() -> String { "0.0.0.0".to_owned() }
fn default_heartbeat_timeout() -> u64 { 5000 }
fn default_clipboard() -> bool { true }

impl Default for Config {
    fn default() -> Self {
        Self {
            port: default_port(),
            bind_addr: default_bind(),
            kb_layout: String::new(),
            heartbeat_timeout_ms: default_heartbeat_timeout(),
            clipboard: default_clipboard(),
            tls: false,
        }
    }
}

fn load_config(path: Option<PathBuf>) -> Result<Config> {
    let path = path.or_else(|| dirs_path().map(|d| d.join("client.toml")));

    if let Some(ref p) = path {
        if p.exists() {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("reading config {}", p.display()))?;
            let cfg: Config = toml::from_str(&text).context("parsing config TOML")?;
            return Ok(cfg);
        }
    }

    info!("no config file found — using defaults");
    Ok(Config::default())
}

/// Per-user config dir: %APPDATA%\yzendris on Windows, ~/.config/yzendris on Linux.
fn dirs_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("yzendris"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|base| base.join("yzendris"))
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() {
    // rustls 0.23+ needs a CryptoProvider registered before any TLS code runs.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async_main())
        .unwrap_or_else(|e| {
            eprintln!("fatal: {e:#}");
            std::process::exit(1);
        });
}

/// RUST_LOG wins when set; the info default applies when it's absent.
fn build_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("yzendris_client=info"))
}

/// Set up logging. On Windows there's no console (windows_subsystem="windows"),
/// so write to %APPDATA%\yzendris\client.log (no ANSI colour codes). On Linux
/// keep stdout, which systemd/journald captures.
fn init_logging() {
    #[cfg(windows)]
    {
        let log_path = dirs_path().map(|d| d.join("client.log"));
        if let Some(parent) = log_path.as_ref().and_then(|p| p.parent()) {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = log_path.as_ref().and_then(|p| {
            std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
        });
        match file {
            Some(f) => {
                tracing_subscriber::fmt()
                    .with_env_filter(build_filter())
                    .with_writer(std::sync::Mutex::new(f))
                    .with_ansi(false)
                    .init();
            }
            None => {
                tracing_subscriber::fmt().with_env_filter(build_filter()).init();
            }
        }
    }
    #[cfg(not(windows))]
    {
        tracing_subscriber::fmt().with_env_filter(build_filter()).init();
    }
}

async fn async_main() -> Result<()> {
    init_logging();

    let args: Vec<String> = std::env::args().collect();
    let stdin_mode = args.contains(&"--stdin".to_owned());
    let config_path = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| PathBuf::from(&w[1]));

    let config = load_config(config_path)?;
    info!("config: {config:?}");
    info!("injection backend: {}", platform::BACKEND);

    // Create the platform injector (Linux: uinput device; Windows: SendInput).
    let mut device = platform::Injector::new().context("creating injector")?;

    // Platform-specific post-create setup (Linux waits + applies kb_layout).
    platform::setup(&mut device, &config.kb_layout).await;

    // Build optional TLS acceptor.
    let tls_acceptor = if config.tls {
        let config_dir = dirs_path().unwrap_or_else(|| PathBuf::from("."));
        let (cert_chain, key) =
            tls::load_or_generate_cert(&config_dir).context("loading/generating TLS cert")?;

        let fp = tls::fingerprint(&cert_chain[0]);
        info!("TLS fingerprint: {fp}");
        eprintln!("=== TLS fingerprint (add to the server's trusted_peers.txt) ===");
        eprintln!("{fp}");
        eprintln!("===============================================================");

        Some(tls::make_acceptor(cert_chain, key).context("build TLS acceptor")?)
    } else {
        None
    };

    if stdin_mode {
        info!("STDIN mode — reading JSON events line by line");
        run_stdin_mode(&mut device).await?;
    } else {
        let addr = format!("{}:{}", config.bind_addr, config.port);
        run_tcp_mode(
            &mut device,
            &addr,
            config.heartbeat_timeout_ms,
            config.clipboard,
            tls_acceptor.as_ref(),
        )
        .await?;
    }

    Ok(())
}

/// Dev spike: read JSON-encoded Events from stdin, inject them.
async fn run_stdin_mode(device: &mut platform::Injector) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_owned();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match serde_json::from_str::<Event>(&line) {
            Ok(event) => {
                if let Err(e) = device.inject(&event) {
                    error!("inject error: {e}");
                }
            }
            Err(e) => warn!("JSON parse error: {e}  (line: {line})"),
        }
    }
    Ok(())
}

/// Normal mode: accept TCP connections, inject received Events.
async fn run_tcp_mode(
    device: &mut platform::Injector,
    addr: &str,
    heartbeat_timeout_ms: u64,
    clipboard_enabled: bool,
    tls_acceptor: Option<&tokio_rustls::TlsAcceptor>,
) -> Result<()> {
    let mut cursor_tracker = tracker::CursorTracker::new();

    loop {
        let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
        let (out_tx, out_rx) = mpsc::unbounded_channel::<Event>();

        let net_handle = {
            let addr = addr.to_owned();
            let acceptor = tls_acceptor.cloned();
            tokio::spawn(async move {
                if let Err(e) =
                    net::run_once(&addr, tx, out_rx, heartbeat_timeout_ms, clipboard_enabled, acceptor.as_ref())
                        .await
                {
                    error!("net error: {e}");
                }
            })
        };

        while let Some(event) = rx.recv().await {
            match &event {
                Event::CaptureEnd => {
                    info!("CaptureEnd received — releasing all keys");
                    cursor_tracker.on_capture_end();
                    if let Err(e) = device.release_all() {
                        error!("release_all: {e}");
                    }
                }
                Event::CaptureStart => {
                    info!("CaptureStart — ready to inject");
                    cursor_tracker.on_capture_start();
                }
                Event::EnterAt { edge, frac } => {
                    cursor_tracker.on_enter_at(*edge, *frac);
                }
                Event::MouseMove { dx, dy } => {
                    if let Err(e) = device.inject(&event) {
                        error!("inject error: {e}");
                    }
                    cursor_tracker.on_mouse_move(*dx, *dy, &out_tx);
                }
                other => {
                    if let Err(e) = device.inject(other) {
                        error!("inject error: {e}");
                    }
                }
            }
        }

        net_handle.await.ok();
        info!("connection closed — waiting for next connection");
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}
