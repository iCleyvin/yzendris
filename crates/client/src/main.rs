// yzendris-client — Linux side
// Injects keyboard/mouse events into Hyprland via uinput.
//
// On non-Linux: stub that exits with an error (workspace builds everywhere).

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("yzendris-client is Linux-only.");
    std::process::exit(1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Linux implementation
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod clipboard;
#[cfg(target_os = "linux")]
mod hyprland;
#[cfg(target_os = "linux")]
mod net;
#[cfg(target_os = "linux")]
mod tls;
#[cfg(target_os = "linux")]
mod uinput;

#[cfg(target_os = "linux")]
use anyhow::{Context, Result};
#[cfg(target_os = "linux")]
use serde::Deserialize;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use tokio::sync::mpsc;
#[cfg(target_os = "linux")]
use tracing::{error, info, warn};
#[cfg(target_os = "linux")]
use yzendris_protocol::Event;

// ─── Config ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[derive(Deserialize, Debug)]
struct Config {
    /// Port to listen on (default 7547).
    #[serde(default = "default_port")]
    port: u16,

    /// Address to bind on (default "0.0.0.0").
    #[serde(default = "default_bind")]
    bind_addr: String,

    /// XKB layout to assign to the virtual device.
    /// Empty string = auto-detect from `hyprctl devices -j`.
    #[serde(default)]
    kb_layout: String,

    /// Milliseconds without a heartbeat before considering the server dead.
    #[serde(default = "default_heartbeat_timeout")]
    heartbeat_timeout_ms: u64,

    /// Sync clipboard on capture transitions (requires wl-clipboard on Linux).
    #[serde(default = "default_clipboard")]
    clipboard: bool,

    /// Enable TLS.  When true, generates a self-signed cert on first run and
    /// requires the Windows server to have the fingerprint in trusted_peers.txt.
    #[serde(default)]
    tls: bool,
}

#[cfg(target_os = "linux")]
fn default_port() -> u16 { 7547 }
#[cfg(target_os = "linux")]
fn default_bind() -> String { "0.0.0.0".to_owned() }
#[cfg(target_os = "linux")]
fn default_heartbeat_timeout() -> u64 { 5000 }
#[cfg(target_os = "linux")]
fn default_clipboard() -> bool { true }

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn load_config(path: Option<PathBuf>) -> Result<Config> {
    let path = path
        .or_else(|| {
            // XDG_CONFIG_HOME/yzendris/client.toml
            dirs_path().map(|d| d.join("client.toml"))
        });

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

#[cfg(target_os = "linux")]
fn dirs_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
        })
        .map(|base| base.join("yzendris"))
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn main() {
    // rustls 0.23+ needs a CryptoProvider registered before any TLS code runs.
    // The `ring` feature ships the implementation; this call wires it in as the
    // process default.  Ignore the result: failure means it was already installed.
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

#[cfg(target_os = "linux")]
async fn async_main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("yzendris_client=info".parse().unwrap()),
        )
        .init();

    // CLI: optional config path, --stdin flag for Fase-0 spike
    let args: Vec<String> = std::env::args().collect();
    let stdin_mode = args.contains(&"--stdin".to_owned());
    let config_path = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| PathBuf::from(&w[1]));

    let config = load_config(config_path)?;
    info!("config: {config:?}");

    // Create the uinput virtual device.
    let mut device = uinput::VirtualKbdMouse::create()
        .context("creating uinput device")?;
    info!("uinput device '{}' created", uinput::DEVICE_NAME);

    // Wait for libinput / Hyprland to register the device.
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    // Detect keyboard layout and apply it to the virtual device.
    let layout = if config.kb_layout.is_empty() {
        hyprland::detect_layout()
    } else {
        config.kb_layout.clone()
    };
    hyprland::apply_layout(uinput::DEVICE_NAME, &layout)
        .context("applying kb_layout")?;

    // Build optional TLS acceptor.
    let tls_acceptor = if config.tls {
        let config_dir = dirs_path().unwrap_or_else(|| std::path::PathBuf::from("."));
        let (cert_chain, key) = tls::load_or_generate_cert(&config_dir)
            .context("loading/generating TLS cert")?;

        // Print fingerprint so the user can add it to the Windows server's
        // trusted_peers.txt.
        let fp = tls::fingerprint(&cert_chain[0]);
        info!("TLS fingerprint: {fp}");
        eprintln!("=== TLS fingerprint (add to Windows trusted_peers.txt) ===");
        eprintln!("{fp}");
        eprintln!("=========================================================");

        Some(tls::make_acceptor(cert_chain, key).context("build TLS acceptor")?)
    } else {
        None
    };

    if stdin_mode {
        // ── Fase-0 spike: read JSON events from stdin ──────────────────────
        info!("STDIN mode — reading JSON events line by line");
        info!("Format: {{\"KeyPress\":{{\"keycode\":125}}}}  or  {{\"MouseMove\":{{\"dx\":10,\"dy\":0}}}}");
        run_stdin_mode(&mut device).await?;
    } else {
        // ── Normal mode: TCP listener ──────────────────────────────────────
        let addr = format!("{}:{}", config.bind_addr, config.port);
        run_tcp_mode(&mut device, &addr, config.heartbeat_timeout_ms, config.clipboard, tls_acceptor.as_ref()).await?;
    }

    Ok(())
}

/// Fase-0 spike: read JSON-encoded Events from stdin, inject them.
#[cfg(target_os = "linux")]
async fn run_stdin_mode(device: &mut uinput::VirtualKbdMouse) -> Result<()> {
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
            Err(e) => {
                warn!("JSON parse error: {e}  (line: {line})");
            }
        }
    }
    Ok(())
}

/// Normal mode: accept TCP connections, inject received Events.
#[cfg(target_os = "linux")]
async fn run_tcp_mode(
    device: &mut uinput::VirtualKbdMouse,
    addr: &str,
    heartbeat_timeout_ms: u64,
    clipboard_enabled: bool,
    tls_acceptor: Option<&tokio_rustls::TlsAcceptor>,
) -> Result<()> {
    loop {
        let (tx, mut rx) = mpsc::unbounded_channel::<Event>();

        // Accept one connection; this returns when it disconnects.
        let net_handle = {
            let addr = addr.to_owned();
            // Clone the acceptor (Arc inside, so cheap).
            let acceptor = tls_acceptor.cloned();
            tokio::spawn(async move {
                if let Err(e) = net::run_once(&addr, tx, heartbeat_timeout_ms, clipboard_enabled, acceptor.as_ref()).await {
                    error!("net error: {e}");
                }
            })
        };

        // Drain events as they arrive.
        while let Some(event) = rx.recv().await {
            match &event {
                Event::CaptureEnd => {
                    info!("CaptureEnd received — releasing all keys");
                    if let Err(e) = device.release_all() {
                        error!("release_all: {e}");
                    }
                }
                Event::CaptureStart => {
                    info!("CaptureStart — ready to inject");
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

        // Small back-off before accepting again.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}
