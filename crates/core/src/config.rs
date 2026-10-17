//! The single configuration file, `config.toml`.
//!
//! The daemon owns this file: the GUI reads and changes it through the
//! control endpoint so there is exactly one writer.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::fingerprint::Fingerprint;
use crate::geom::Side;
use crate::DEFAULT_PORT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// This machine owns the keyboard and mouse and shares them.
    Host,
    /// This machine is controlled by a host.
    Client,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Unset until the user picks one; the daemon idles meanwhile.
    pub role: Option<Role>,
    /// Name other machines see for this one.
    pub name: String,
    /// Share clipboard text when the cursor changes machine.
    pub clipboard: bool,
    /// Also carry copied files across (requires `clipboard`).
    pub files: bool,
    /// Largest single file, in MiB, accepted or offered through the clipboard.
    pub max_file_mb: u32,
    pub host: HostConfig,
    pub client: ClientConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    pub port: u16,
    /// Address to listen on. `0.0.0.0` accepts paired clients on any network
    /// interface; set a specific LAN address to narrow it.
    pub bind: String,
    /// Switch machines by pushing the cursor through a screen edge.
    pub edges: bool,
    /// Switch machines with Ctrl+Shift+Alt shortcuts.
    pub hotkeys: bool,
    /// Multiplier applied to pointer motion sent to clients.
    pub pointer_speed: f32,
    pub clients: Vec<ClientEntry>,
}

/// A paired client and where its screen sits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientEntry {
    pub name: String,
    pub fingerprint: Fingerprint,
    /// Host monitor the client is attached to; empty means the primary one.
    #[serde(default)]
    pub monitor: String,
    /// Side of that monitor the client sits on.
    #[serde(default)]
    pub side: Side,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    /// `address:port` of the paired host (last known).
    pub host_addr: String,
    pub host_name: String,
    /// Identity of the paired host. Nothing is accepted from anyone else.
    pub host_fingerprint: Option<Fingerprint>,
    /// XKB layout forced on the virtual keyboard under Hyprland. Empty keeps
    /// the compositor's own setting, which is right almost everywhere.
    pub keyboard_layout: String,
}

fn yes() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            role: None,
            name: default_name(),
            clipboard: true,
            files: true,
            max_file_mb: 500,
            host: HostConfig::default(),
            client: ClientConfig::default(),
        }
    }
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            bind: "0.0.0.0".into(),
            edges: true,
            hotkeys: true,
            pointer_speed: 1.0,
            clients: Vec::new(),
        }
    }
}

/// This machine's hostname, used as the default display name.
pub fn default_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_default();
    let name = clean_name(&raw);
    if name.is_empty() {
        "yzendris".into()
    } else {
        name
    }
}

/// Make a peer-supplied or user-typed name safe to display and store.
pub fn clean_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl Config {
    /// Clamp every field into its valid range. Called on load and before save
    /// so neither a hand-edited file nor a GUI bug can produce bad state.
    pub fn sanitize(&mut self) {
        self.name = clean_name(&self.name);
        if self.name.is_empty() {
            self.name = default_name();
        }
        self.max_file_mb = self.max_file_mb.clamp(1, 100_000);
        if self.host.port == 0 {
            self.host.port = DEFAULT_PORT;
        }
        if self.host.bind.trim().is_empty() {
            self.host.bind = "0.0.0.0".into();
        }
        if !self.host.pointer_speed.is_finite() {
            self.host.pointer_speed = 1.0;
        }
        self.host.pointer_speed = self.host.pointer_speed.clamp(0.2, 4.0);

        let mut seen = std::collections::HashSet::new();
        self.host.clients.retain(|c| seen.insert(c.fingerprint));
        for (i, c) in self.host.clients.iter_mut().enumerate() {
            c.name = clean_name(&c.name);
            if c.name.is_empty() {
                c.name = format!("client-{}", i + 1);
            }
            c.monitor = clean_name(&c.monitor);
        }
        self.client.host_addr = self.client.host_addr.trim().to_owned();
        self.client.host_name = clean_name(&self.client.host_name);
        let layout = &self.client.keyboard_layout;
        if !layout
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b',' || b == b'_')
        {
            self.client.keyboard_layout.clear();
        }
    }

    pub fn max_file_bytes(&self) -> u64 {
        u64::from(self.max_file_mb) * 1024 * 1024
    }

    /// Load `path`; a missing file yields defaults.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.sanitize();
        Ok(cfg)
    }

    /// Write atomically: a crash mid-save never leaves a truncated config.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut cfg = self.clone();
        cfg.sanitize();
        let text = toml::to_string_pretty(&cfg).context("serializing config")?;
        write_private(path, text.as_bytes())
    }
}

/// Atomically replace `path` with `data`, readable by the owner only.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    create_private_dir(dir)?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Create `dir` (and parents) so only the owner can enter it.
pub fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(cfg.role, None);
        assert!(cfg.clipboard);
        assert_eq!(cfg.host.port, DEFAULT_PORT);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let mut cfg = Config {
            role: Some(Role::Host),
            ..Config::default()
        };
        cfg.host.clients.push(ClientEntry {
            name: "portátil".into(),
            fingerprint: Fingerprint([7; 32]),
            monitor: "DP-1".into(),
            side: Side::Left,
            enabled: true,
        });
        cfg.client.host_fingerprint = Some(Fingerprint([9; 32]));
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
    }

    #[cfg(unix)]
    #[test]
    fn saved_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("config.toml");
        Config::default().save(&path).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    #[test]
    fn sanitize_repairs_hostile_values() {
        let fp = Fingerprint([1; 32]);
        let entry = |name: &str| ClientEntry {
            name: name.into(),
            fingerprint: fp,
            monitor: String::new(),
            side: Side::Right,
            enabled: true,
        };
        let mut cfg = Config {
            name: "  evil\u{7}\nname  ".into(),
            max_file_mb: 0,
            ..Config::default()
        };
        cfg.host.port = 0;
        cfg.host.pointer_speed = f32::NAN;
        cfg.host.clients = vec![entry("a"), entry("duplicate")];
        cfg.client.keyboard_layout = "es'}) os.execute('x') --".into();
        cfg.sanitize();
        assert_eq!(cfg.name, "evilname");
        assert_eq!(cfg.max_file_mb, 1);
        assert_eq!(cfg.host.port, DEFAULT_PORT);
        assert_eq!(cfg.host.pointer_speed, 1.0);
        assert_eq!(cfg.host.clients.len(), 1);
        assert!(cfg.client.keyboard_layout.is_empty());
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        let cfg: Config = toml::from_str("role = \"client\"\nfuture_option = 3\n").unwrap();
        assert_eq!(cfg.role, Some(Role::Client));
    }
}
