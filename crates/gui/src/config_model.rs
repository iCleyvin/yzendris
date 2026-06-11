//! Config structs mirroring the daemons' TOML formats, plus load/save helpers.
//! Field names and defaults MUST stay in sync with crates/server and
//! crates/client.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ─── Paths ───────────────────────────────────────────────────────────────────

/// Directory holding all yzendris files for this user.
/// Windows: %APPDATA%\yzendris   Linux: ~/.config/yzendris
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(|d| PathBuf::from(d).join("yzendris"))
            .unwrap_or_else(|| PathBuf::from("yzendris"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|base| base.join("yzendris"))
            .unwrap_or_else(|| PathBuf::from("yzendris"))
    }
}

pub fn server_config_path() -> PathBuf { config_dir().join("server.toml") }
pub fn client_config_path() -> PathBuf { config_dir().join("client.toml") }
pub fn gui_config_path()    -> PathBuf { config_dir().join("gui.toml") }
pub fn trusted_peers_path() -> PathBuf { config_dir().join("trusted_peers.txt") }
pub fn server_log_path()    -> PathBuf { config_dir().join("server.log") }
pub fn client_log_path()    -> PathBuf { config_dir().join("client.log") }
pub fn server_status_path() -> PathBuf { config_dir().join("status") }

// ─── GUI state (role) ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Client,
}

#[derive(Serialize, Deserialize, Default)]
struct GuiConfig {
    #[serde(default)]
    role: String,
}

pub fn load_role() -> Option<Role> {
    let text = std::fs::read_to_string(gui_config_path()).ok()?;
    let cfg: GuiConfig = toml::from_str(&text).ok()?;
    match cfg.role.as_str() {
        "host" => Some(Role::Host),
        "client" => Some(Role::Client),
        _ => None,
    }
}

pub fn save_role(role: Role) -> anyhow::Result<()> {
    let cfg = GuiConfig {
        role: match role {
            Role::Host => "host".into(),
            Role::Client => "client".into(),
        },
    };
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(gui_config_path(), toml::to_string(&cfg)?)?;
    Ok(())
}

// ─── Server (host) config ────────────────────────────────────────────────────

/// Host config: global settings + one or more clients.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub heartbeat_ms: u64,
    pub clipboard: bool,
    pub clients: Vec<ClientEntry>,
}

/// One client and where it sits in the host's screen arrangement.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ClientEntry {
    #[serde(default)]
    pub name: String,
    pub addr: String,
    #[serde(default = "default_client_port")]
    pub port: u16,
    #[serde(default)]
    pub tls: bool,
    /// Placement: the two monitor names the client sits between.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub between: Option<Vec<String>>,
    /// Placement: an outer edge ("right"/"left"/"top"/"bottom").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
}

fn default_client_port() -> u16 { 7547 }

impl Default for ClientEntry {
    fn default() -> Self {
        Self {
            name: "cliente".into(),
            addr: "192.168.1.42".into(),
            port: 7547,
            tls: false,
            between: None,
            edge: Some("right".into()),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct LayoutConfig {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub monitor_left: String,
    #[serde(default)]
    pub monitor_right: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            heartbeat_ms: 1000,
            clipboard: true,
            clients: vec![ClientEntry::default()],
        }
    }
}

// Captures both the new `[[clients]]` form and the legacy single-client fields.
#[derive(Deserialize, Default)]
struct ServerConfigPartial {
    heartbeat_ms: Option<u64>,
    clipboard: Option<bool>,
    clients: Option<Vec<ClientEntry>>,
    // legacy single-client
    client_addr: Option<String>,
    port: Option<u16>,
    edge: Option<String>,
    tls: Option<bool>,
    layout: Option<LayoutConfig>,
}

pub fn load_server_config() -> ServerConfig {
    let default = ServerConfig::default();
    let Ok(text) = std::fs::read_to_string(server_config_path()) else {
        return default;
    };
    let Ok(p) = toml::from_str::<ServerConfigPartial>(&text) else {
        return default;
    };

    let clients = match p.clients {
        Some(cs) if !cs.is_empty() => cs,
        _ => {
            // Synthesize one client from the legacy fields.
            let between = p
                .layout
                .as_ref()
                .filter(|l| l.mode == "between")
                .map(|l| vec![l.monitor_left.clone(), l.monitor_right.clone()]);
            let edge = if between.is_none() {
                Some(p.edge.clone().unwrap_or_else(|| "right".into()))
            } else {
                None
            };
            vec![ClientEntry {
                name: "laptop".into(),
                addr: p.client_addr.unwrap_or_else(|| "192.168.1.42".into()),
                port: p.port.unwrap_or(7547),
                tls: p.tls.unwrap_or(false),
                between,
                edge,
            }]
        }
    };

    ServerConfig {
        heartbeat_ms: p.heartbeat_ms.unwrap_or(default.heartbeat_ms),
        clipboard: p.clipboard.unwrap_or(default.clipboard),
        clients,
    }
}

pub fn save_server_config(cfg: &ServerConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(server_config_path(), toml::to_string_pretty(cfg)?)?;
    Ok(())
}

// ─── Client config ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ClientConfig {
    pub port: u16,
    pub bind_addr: String,
    pub kb_layout: String,
    pub heartbeat_timeout_ms: u64,
    pub clipboard: bool,
    pub tls: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            port: 7547,
            bind_addr: "0.0.0.0".into(),
            kb_layout: String::new(),
            heartbeat_timeout_ms: 5000,
            clipboard: true,
            tls: false,
        }
    }
}

#[derive(Deserialize, Default)]
struct ClientConfigPartial {
    port: Option<u16>,
    bind_addr: Option<String>,
    kb_layout: Option<String>,
    heartbeat_timeout_ms: Option<u64>,
    clipboard: Option<bool>,
    tls: Option<bool>,
}

pub fn load_client_config() -> ClientConfig {
    let default = ClientConfig::default();
    let Ok(text) = std::fs::read_to_string(client_config_path()) else {
        return default;
    };
    let Ok(p) = toml::from_str::<ClientConfigPartial>(&text) else {
        return default;
    };
    ClientConfig {
        port: p.port.unwrap_or(default.port),
        bind_addr: p.bind_addr.unwrap_or(default.bind_addr),
        kb_layout: p.kb_layout.unwrap_or(default.kb_layout),
        heartbeat_timeout_ms: p.heartbeat_timeout_ms.unwrap_or(default.heartbeat_timeout_ms),
        clipboard: p.clipboard.unwrap_or(default.clipboard),
        tls: p.tls.unwrap_or(default.tls),
    }
}

pub fn save_client_config(cfg: &ClientConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(client_config_path(), toml::to_string_pretty(cfg)?)?;
    Ok(())
}

// ─── trusted_peers.txt ───────────────────────────────────────────────────────

pub fn load_trusted_peers() -> Vec<String> {
    std::fs::read_to_string(trusted_peers_path())
        .map(|t| {
            t.lines()
                .map(|l| l.trim().to_owned())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect()
        })
        .unwrap_or_default()
}

pub fn save_trusted_peers(peers: &[String]) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    let mut text = peers.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    std::fs::write(trusted_peers_path(), text)?;
    Ok(())
}
