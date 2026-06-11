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

    /// Multiple clients, each at its own boundary/edge. When non-empty this
    /// takes over from the single-client fields above (which stay for backward
    /// compatibility with existing configs).
    #[serde(default)]
    clients: Vec<ClientCfg>,
}

#[cfg(windows)]
#[derive(Deserialize, Debug, Clone)]
struct ClientCfg {
    /// Friendly name for logs.
    #[serde(default)]
    name: String,
    /// IP or hostname of the client.
    addr: String,
    /// Port the client listens on.
    #[serde(default = "default_port")]
    port: u16,
    /// Use TLS for this client.
    #[serde(default)]
    tls: bool,
    /// Placement: the two monitor names the client sits between. Orientation
    /// (side by side / stacked) is inferred from their geometry.
    #[serde(default)]
    between: Option<Vec<String>>,
    /// Placement: an edge ("right"/"left"/"top"/"bottom"). With `monitor` it's
    /// that monitor's free edge; without, the whole desktop's edge.
    #[serde(default)]
    edge: Option<String>,
    /// The specific monitor whose edge the client sits at (with `edge`).
    #[serde(default)]
    monitor: Option<String>,
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
            clients:      Vec::new(),
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

    // Resolve the client list (multi-client config, or the legacy single client).
    let clients = resolve_clients(&config);
    if clients.is_empty() {
        anyhow::bail!("no clients configured");
    }
    for c in &clients {
        info!("client {} '{}' @ {} — portal {:?}", c.id, c.name, c.addr, c.layout);
    }

    // Park the captured cursor at the centre of the virtual screen.
    let vs = edge::virtual_screen();
    hook::set_capture_center((vs.left + vs.right) / 2, (vs.top + vs.bottom) / 2);

    // One portal per client, one channel per client.
    let portals = clients
        .iter()
        .map(|c| hook::Portal { client: c.id, layout: c.layout })
        .collect();
    hook::configure_portals(portals);

    let mut senders = Vec::with_capacity(clients.len());
    let mut receivers = Vec::with_capacity(clients.len());
    for _ in &clients {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<yzendris_protocol::Event>();
        senders.push(tx);
        receivers.push(rx);
    }
    hook::init(senders);

    // Start the Win32 hook thread.
    hook::start();
    info!("hooks installed");

    // Build one TLS connector (shared) if any client uses TLS.
    let tls_connector = if clients.iter().any(|c| c.tls) {
        let trusted_path = std::env::var_os("APPDATA")
            .map(|d| std::path::PathBuf::from(d).join("yzendris").join("trusted_peers.txt"))
            .unwrap_or_else(|| std::path::PathBuf::from("trusted_peers.txt"));
        let trusted = tls::load_trusted(&trusted_path);
        info!("TLS enabled — {} trusted fingerprint(s)", trusted.len());
        if trusted.is_empty() {
            tracing::warn!("No trusted fingerprints in {}", trusted_path.display());
        }
        Some(tls::make_connector(trusted).context("build TLS connector")?)
    } else {
        None
    };

    // One network task per client (each reconnects forever on its own).
    let mut handles = Vec::with_capacity(clients.len());
    for (c, rx) in clients.into_iter().zip(receivers) {
        let connector = if c.tls { tls_connector.clone() } else { None };
        let heartbeat = config.heartbeat_ms;
        let clipboard = config.clipboard;
        handles.push(tokio::spawn(async move {
            if let Err(e) = net::run(c.id, &c.addr, rx, heartbeat, clipboard, connector).await {
                tracing::error!("client {} net task ended: {e}", c.id);
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    hook::stop();
    Ok(())
}

/// A client resolved to its address + portal layout.
#[cfg(windows)]
struct ResolvedClient {
    id: usize,
    name: String,
    addr: String,
    tls: bool,
    layout: hook::Layout,
}

/// Build the client list: the new `[[clients]]` array if present, otherwise a
/// single client from the legacy top-level fields (backward compatible).
#[cfg(windows)]
fn resolve_clients(config: &Config) -> Vec<ResolvedClient> {
    let mons = monitors::enumerate();

    if !config.clients.is_empty() {
        return config
            .clients
            .iter()
            .enumerate()
            .map(|(id, c)| {
                let layout = if let Some(pair) = c.between.as_ref().filter(|p| p.len() == 2) {
                    layout_between(&pair[0], &pair[1], &mons)
                        .unwrap_or_else(|| layout_edge(c.edge.as_deref().unwrap_or("right")))
                } else if let Some(mon) = c.monitor.as_deref() {
                    layout_monitor_side(mon, c.edge.as_deref().unwrap_or("right"), &mons)
                        .unwrap_or_else(|| layout_edge(c.edge.as_deref().unwrap_or("right")))
                } else {
                    layout_edge(c.edge.as_deref().unwrap_or("right"))
                };
                let name = if c.name.is_empty() { format!("client{id}") } else { c.name.clone() };
                ResolvedClient { id, name, addr: format!("{}:{}", c.addr, c.port), tls: c.tls, layout }
            })
            .collect();
    }

    // Legacy single client.
    vec![ResolvedClient {
        id: 0,
        name: "client".into(),
        addr: format!("{}:{}", config.client_addr, config.port),
        tls: config.tls,
        layout: build_layout(config),
    }]
}

/// Build the runtime layout from config + actual monitor geometry.
/// Falls back to classic edge mode when "between" can't be satisfied
/// (missing monitors, only one display, unknown names…).
#[cfg(windows)]
fn build_layout(config: &Config) -> hook::Layout {
    let Some(layout_cfg) = config.layout.as_ref().filter(|l| l.mode == "between") else {
        return layout_edge(&config.edge);
    };

    let mons = monitors::enumerate();
    // Resolve the two monitor names (explicit, or auto = two leftmost).
    let (a_name, b_name) = if !layout_cfg.monitor_left.is_empty()
        && !layout_cfg.monitor_right.is_empty()
    {
        (layout_cfg.monitor_left.clone(), layout_cfg.monitor_right.clone())
    } else {
        let mut sorted = mons.clone();
        sorted.sort_by_key(|m| m.left);
        if sorted.len() >= 2 {
            (sorted[0].device.clone(), sorted[1].device.clone())
        } else {
            tracing::warn!("'between' needs 2+ monitors — falling back to edge mode");
            return layout_edge(&config.edge);
        }
    };

    layout_between(&a_name, &b_name, &mons).unwrap_or_else(|| layout_edge(&config.edge))
}

/// Build an outer-edge layout from an edge string.
#[cfg(windows)]
fn layout_edge(edge: &str) -> hook::Layout {
    let edge_kind = edge::Edge::from_str(edge).unwrap_or_else(|| {
        tracing::warn!("unknown edge '{edge}' — defaulting to 'right'");
        edge::Edge::Right
    });
    hook::Layout::Edge { side: edge_kind.side(), screen: edge::virtual_screen() }
}

/// Build a "client at a specific monitor's free edge" layout.
#[cfg(windows)]
fn layout_monitor_side(mon_name: &str, edge: &str, mons: &[monitors::Monitor]) -> Option<hook::Layout> {
    let m = monitors::find(mons, mon_name)?;
    let side = edge::Edge::from_str(edge)?.side();
    Some(hook::Layout::MonitorSide {
        rect: hook::Rect { left: m.left, top: m.top, right: m.right, bottom: m.bottom },
        side,
    })
}

/// Build a between-monitors layout from two monitor names. Orientation
/// (side by side vs stacked) is inferred from their real geometry. Returns
/// None if a monitor isn't found or the two don't share an edge.
#[cfg(windows)]
fn layout_between(a_name: &str, b_name: &str, mons: &[monitors::Monitor]) -> Option<hook::Layout> {
    let a = monitors::find(mons, a_name)?.clone();
    let b = monitors::find(mons, b_name)?.clone();
    let rect = |m: &monitors::Monitor| hook::Rect {
        left: m.left, top: m.top, right: m.right, bottom: m.bottom,
    };

    let y_overlap = (a.top.max(b.top) < a.bottom.min(b.bottom)) as i32
        * (a.bottom.min(b.bottom) - a.top.max(b.top));
    let x_overlap = (a.left.max(b.left) < a.right.min(b.right)) as i32
        * (a.right.min(b.right) - a.left.max(b.left));

    if y_overlap >= x_overlap && y_overlap > 0 {
        let (l, r) = if a.left <= b.left { (&a, &b) } else { (&b, &a) };
        Some(hook::Layout::SideBySide { left: rect(l), right: rect(r), boundary_x: l.right })
    } else if x_overlap > 0 {
        let (t, bo) = if a.top <= b.top { (&a, &b) } else { (&b, &a) };
        Some(hook::Layout::Stacked { top: rect(t), bottom: rect(bo), boundary_y: t.bottom })
    } else {
        tracing::warn!("monitors '{}' and '{}' don't share an edge", a.device, b.device);
        None
    }
}
