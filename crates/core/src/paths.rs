//! Per-user file locations.
//!
//! Setting `YZENDRIS_HOME` relocates everything under one directory, which is
//! handy for portable setups and for running several instances side by side.

use std::path::PathBuf;

const APP: &str = "yzendris";

fn home_override() -> Option<PathBuf> {
    std::env::var_os("YZENDRIS_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Settings and identity: `~/.config/yzendris` or `%APPDATA%\yzendris`.
pub fn config_dir() -> PathBuf {
    if let Some(home) = home_override() {
        return home;
    }
    #[cfg(windows)]
    let base = env_dir("APPDATA");
    #[cfg(not(windows))]
    let base = env_dir("XDG_CONFIG_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".config")));
    base.unwrap_or_else(|| PathBuf::from(".")).join(APP)
}

/// Disposable data: `~/.cache/yzendris` or `%LOCALAPPDATA%\yzendris`.
pub fn cache_dir() -> PathBuf {
    if let Some(home) = home_override() {
        return home.join("cache");
    }
    #[cfg(windows)]
    let base = env_dir("LOCALAPPDATA");
    #[cfg(not(windows))]
    let base = env_dir("XDG_CACHE_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".cache")));
    base.unwrap_or_else(std::env::temp_dir).join(APP)
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn cert_file() -> PathBuf {
    config_dir().join("cert.pem")
}

pub fn key_file() -> PathBuf {
    config_dir().join("key.pem")
}

/// Files received through the shared clipboard land here until pasted.
pub fn incoming_dir() -> PathBuf {
    cache_dir().join("incoming")
}

pub fn log_file() -> PathBuf {
    cache_dir().join("yzendris.log")
}

/// Address of the daemon's local control endpoint: a Unix socket path on
/// Linux, a named pipe on Windows. Only the owning user can connect.
pub fn ipc_endpoint() -> PathBuf {
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_default();
        let mut tag: String = user
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(48)
            .collect();
        if let Some(home) = home_override() {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(home.to_string_lossy().as_bytes());
            tag.push_str(&format!(
                "-{:02x}{:02x}{:02x}{:02x}",
                digest[0], digest[1], digest[2], digest[3]
            ));
        }
        PathBuf::from(format!(r"\\.\pipe\{APP}-{tag}"))
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = home_override() {
            return home.join("control.sock");
        }
        env_dir("XDG_RUNTIME_DIR")
            .unwrap_or_else(config_dir)
            .join(format!("{APP}.sock"))
    }
}
