// yzendris-server — Windows side
// Captures keyboard + mouse via low-level hooks and streams events to the
// Linux client over TCP.
//
// On non-Windows: stub that exits with an error.

// No console window when launched on Windows (startup, double-click, etc).
// Logging goes to %APPDATA%\yzendris\server.log instead of stdout.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("yzendris-server is Windows-only.");
    std::process::exit(1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Windows implementation
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod edge;
#[cfg(windows)]
mod hook;
#[cfg(windows)]
mod monitors;
#[cfg(windows)]
mod net;
#[cfg(windows)]
mod tls;

#[cfg(windows)]
use anyhow::{Context, Result};
#[cfg(windows)]
use serde::Deserialize;
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use tracing::info;

// ─── Config ──────────────────────────────────────────────────────────────────

#[cfg(windows)]
#[derive(Deserialize, Debug)]
struct Config {
    /// IP or hostname of the Linux client.
    #[serde(default = "default_client_addr")]
    client_addr: String,

    /// Port to connect to.
    #[serde(default = "default_port")]
    port: u16,

    /// Screen edge that triggers capture.
    #[serde(default = "default_edge")]
    edge: String,

    /// Heartbeat interval in milliseconds.
    #[serde(default = "default_heartbeat_ms")]
    heartbeat_ms: u64,

    /// Sync clipboard on capture transitions (requires wl-clipboard on Linux).
    #[serde(default = "default_clipboard")]
    clipboard: bool,

    /// Enable TLS.  When true, the server verifies the Linux client's
    /// certificate fingerprint against `trusted_peers.txt`.
    #[serde(default)]
    tls: bool,

    /// Optional multi-monitor layout (laptop *between* two monitors).
    /// When absent or mode="edge", the classic `edge` setting applies.
    #[serde(default)]
    layout: Option<LayoutConfig>,
}

#[cfg(windows)]
#[derive(Deserialize, Debug, Default, Clone)]
struct LayoutConfig {
    /// "edge" (classic) or "between" (laptop between two monitors).
    #[serde(default)]
    mode: String,

    /// Device name of the monitor at the LEFT of the laptop ("DISPLAY1", "1").
    /// Empty = auto-pick the leftmost of the first two monitors.
    #[serde(default)]
    monitor_left: String,

    /// Device name of the monitor at the RIGHT of the laptop.
    #[serde(default)]
    monitor_right: String,
}

#[cfg(windows)]
fn default_client_addr() -> String { "192.168.1.42".to_owned() }
#[cfg(windows)]
fn default_port()        -> u16    { 7547 }
#[cfg(windows)]
fn default_edge()        -> String { "right".to_owned() }
#[cfg(windows)]
fn default_heartbeat_ms()-> u64    { 1000 }
#[cfg(windows)]
fn default_clipboard()   -> bool   { true }

#[cfg(windows)]
impl Default for Config {
    fn default() -> Self {
        Self {
            client_addr:  default_client_addr(),
            port:         default_port(),
            edge:         default_edge(),
            heartbeat_ms: default_heartbeat_ms(),
            clipboard:    default_clipboard(),
            tls:          false,
            layout:       None,
        }
    }
}

#[cfg(windows)]
fn load_config(path: Option<PathBuf>) -> Result<Config> {
    let path = path.or_else(|| {
        // %APPDATA%\yzendris\server.toml
        std::env::var_os("APPDATA")
            .map(|d| PathBuf::from(d).join("yzendris").join("server.toml"))
    });

    if let Some(ref p) = path {
        if p.exists() {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("reading config {}", p.display()))?;
            let cfg: Config = toml::from_str(&text).context("parse TOML")?;
            return Ok(cfg);
        }
    }

    info!("no config file found — using defaults");
    Ok(Config::default())
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[cfg(windows)]
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

#[cfg(windows)]
async fn async_main() -> Result<()> {
    // With windows_subsystem="windows" stdout is detached — log to file instead.
    let log_path = std::env::var_os("APPDATA")
        .map(|d| std::path::PathBuf::from(d).join("yzendris").join("server.log"));

    let filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("yzendris_server=debug".parse().unwrap());

    if let Some(ref path) = log_path {
        if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => {
                tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_writer(std::sync::Mutex::new(file))
                    .with_ansi(false)
                    .init();
            }
            Err(_) => {
                tracing_subscriber::fmt().with_env_filter(filter).init();
            }
        }
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    let args: Vec<String> = std::env::args().collect();
    let config_path = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| PathBuf::from(&w[1]));

    let config = load_config(config_path)?;
    info!("config: {config:?}");

    // Resolve the screen layout (classic edge or laptop-between-monitors).
    let layout = build_layout(&config);
    info!("layout: {layout:?}");

    // Park the captured cursor at the centre of the virtual screen.
    let vs = edge::virtual_screen();
    hook::set_capture_center((vs.left + vs.right) / 2, (vs.top + vs.bottom) / 2);
    hook::configure_layout(layout);

    // Create channel: hook → net.
    let (event_tx, event_rx) =
        tokio::sync::mpsc::unbounded_channel::<yzendris_protocol::Event>();
    hook::init(event_tx);

    // Start the Win32 hook thread.
    hook::start();
    info!("hooks installed");

    // Build optional TLS connector.
    let tls_connector = if config.tls {
        // Path to trusted fingerprints file: same dir as the config or %APPDATA%\yzendris\
        let trusted_path = std::env::var_os("APPDATA")
            .map(|d| std::path::PathBuf::from(d).join("yzendris").join("trusted_peers.txt"))
            .unwrap_or_else(|| std::path::PathBuf::from("trusted_peers.txt"));

        let trusted = tls::load_trusted(&trusted_path);
        info!("TLS enabled — {} trusted fingerprint(s)", trusted.len());
        if trusted.is_empty() {
            tracing::warn!(
                "No trusted fingerprints found in {}",
                trusted_path.display()
            );
            tracing::warn!(
                "Start the Linux client once (TLS mode) to get its fingerprint,\
                 then add it to trusted_peers.txt"
            );
        }
        Some(tls::make_connector(trusted).context("build TLS connector")?)
    } else {
        None
    };

    // Connect to Linux client and stream events (blocks forever, reconnects).
    let addr = format!("{}:{}", config.client_addr, config.port);
    net::run(&addr, event_rx, config.heartbeat_ms, config.clipboard, tls_connector).await?;

    hook::stop();
    Ok(())
}

/// Build the runtime layout from config + actual monitor geometry.
/// Falls back to classic edge mode when "between" can't be satisfied
/// (missing monitors, only one display, unknown names…).
#[cfg(windows)]
fn build_layout(config: &Config) -> hook::Layout {
    let edge_layout = || {
        let edge_kind = edge::Edge::from_str(&config.edge).unwrap_or_else(|| {
            tracing::warn!("unknown edge '{}' — defaulting to 'right'", config.edge);
            edge::Edge::Right
        });
        hook::Layout::Edge { side: edge_kind.side(), screen: edge::virtual_screen() }
    };

    let Some(layout_cfg) = config.layout.as_ref().filter(|l| l.mode == "between") else {
        return edge_layout();
    };

    let mons = monitors::enumerate();
    if mons.len() < 2 {
        tracing::warn!(
            "layout mode 'between' needs 2+ monitors but {} detected — falling back to edge mode",
            mons.len()
        );
        return edge_layout();
    }

    // Resolve the two monitors flanking the laptop.
    let (left_m, right_m) = if !layout_cfg.monitor_left.is_empty()
        && !layout_cfg.monitor_right.is_empty()
    {
        match (
            monitors::find(&mons, &layout_cfg.monitor_left),
            monitors::find(&mons, &layout_cfg.monitor_right),
        ) {
            (Some(a), Some(b)) => (a.clone(), b.clone()),
            _ => {
                tracing::warn!(
                    "layout monitors '{}'/'{}' not found (have: {:?}) — falling back to edge mode",
                    layout_cfg.monitor_left,
                    layout_cfg.monitor_right,
                    mons.iter().map(|m| m.device.clone()).collect::<Vec<_>>()
                );
                return edge_layout();
            }
        }
    } else {
        // Auto: the two monitors sorted left-to-right.
        let mut sorted = mons.clone();
        sorted.sort_by_key(|m| m.left);
        (sorted[0].clone(), sorted[1].clone())
    };

    // Ensure left really is to the left of right (swap if user mixed them up).
    let (left_m, right_m) = if left_m.left <= right_m.left {
        (left_m, right_m)
    } else {
        tracing::warn!("monitor_left/right appear swapped — correcting");
        (right_m, left_m)
    };

    let boundary_x = left_m.right;
    if (right_m.left - boundary_x).abs() > 1 {
        tracing::warn!(
            "monitors '{}' and '{}' are not adjacent (gap of {}px) — boundary at x={}",
            left_m.device, right_m.device, right_m.left - boundary_x, boundary_x
        );
    }

    hook::Layout::Between {
        left_mon: hook::Rect {
            left: left_m.left, top: left_m.top, right: left_m.right, bottom: left_m.bottom,
        },
        right_mon: hook::Rect {
            left: right_m.left, top: right_m.top, right: right_m.right, bottom: right_m.bottom,
        },
        boundary_x,
    }
}
