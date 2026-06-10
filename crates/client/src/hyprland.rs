/// Hyprland helpers: layout detection and runtime device assignment.
///
/// CRITICAL: without calling `apply_layout` after uinput device creation,
/// modifier keys (Super/Ctrl/Alt) are NOT recognised by Hyprland bind rules.
use anyhow::{Context, Result};

/// Return the running Hyprland instance signature.
///
/// Tries `HYPRLAND_INSTANCE_SIGNATURE` first; if unset, scans
/// `/run/user/<uid>/hypr/` for the first directory entry.
fn instance_signature() -> Option<String> {
    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        if !sig.is_empty() {
            return Some(sig);
        }
    }

    // Derive from filesystem: /run/user/<uid>/hypr/<sig>/.socket.sock
    let uid = libc_getuid();
    let hypr_dir = format!("/run/user/{uid}/hypr");
    let entries = std::fs::read_dir(&hypr_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                return Some(name.to_owned());
            }
        }
    }
    None
}

/// Thin wrapper around `libc::getuid` — we only need this to find the runtime dir.
fn libc_getuid() -> u32 {
    // SAFETY: getuid is always safe.
    unsafe { libc::getuid() }
}

/// Detect the XKB layout of the first physical keyboard registered in Hyprland.
/// Falls back to "us" if detection fails.
pub fn detect_layout() -> String {
    let sig = match instance_signature() {
        Some(s) => s,
        None => {
            tracing::warn!("could not detect Hyprland instance — falling back to 'us'");
            return "us".to_owned();
        }
    };

    let output = match std::process::Command::new("hyprctl")
        .args(["devices", "-j"])
        .env("HYPRLAND_INSTANCE_SIGNATURE", &sig)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("hyprctl devices failed: {e}");
            return "us".to_owned();
        }
    };

    let json: serde_json::Value =
        match serde_json::from_slice(&output.stdout) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("hyprctl devices JSON parse failed: {e}");
                return "us".to_owned();
            }
        };

    // Try keyboards[0].layout  ("es" / "us" / "es(nodeadkeys)" / …)
    // The field is "layout" in Hyprland's JSON (not "keymap").
    if let Some(km) = json["keyboards"]
        .as_array()
        .and_then(|arr| arr.iter().find(|kb| {
            // Skip virtual devices (our own yzendris kb shows up here too).
            kb["name"].as_str()
                .map(|n| !n.contains("yzendris") && !n.contains("virtual"))
                .unwrap_or(true)
        }))
        .and_then(|kb| kb["layout"].as_str())
    {
        let layout = km.split('(').next().unwrap_or(km).trim().to_owned();
        if !layout.is_empty() {
            tracing::info!("detected kb_layout: {layout}");
            return layout;
        }
    }

    tracing::warn!("could not detect kb_layout, falling back to 'us'");
    "us".to_owned()
}

/// Run `hyprctl` with the instance signature injected; return stdout on success.
fn hyprctl(args: &[&str]) -> Option<String> {
    let sig = instance_signature()?;
    let output = std::process::Command::new("hyprctl")
        .args(args)
        .env("HYPRLAND_INSTANCE_SIGNATURE", &sig)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Current cursor position in global (layout) coordinates.
/// `hyprctl cursorpos` prints "x, y".
pub fn cursor_pos() -> Option<(i32, i32)> {
    let out = hyprctl(&["cursorpos"])?;
    let mut parts = out.trim().split(',');
    let x = parts.next()?.trim().parse().ok()?;
    let y = parts.next()?.trim().parse().ok()?;
    Some((x, y))
}

/// Geometry of the focused monitor in global LOGICAL coordinates:
/// (x, y, width, height). Accounts for scale and 90°/270° transforms.
pub fn focused_monitor_rect() -> Option<(i32, i32, i32, i32)> {
    let out = hyprctl(&["monitors", "-j"])?;
    let json: serde_json::Value = serde_json::from_str(&out).ok()?;
    let mons = json.as_array()?;
    let mon = mons
        .iter()
        .find(|m| m["focused"].as_bool() == Some(true))
        .or_else(|| mons.first())?;

    let x = mon["x"].as_i64()? as i32;
    let y = mon["y"].as_i64()? as i32;
    let mut w = mon["width"].as_i64()? as f64;
    let mut h = mon["height"].as_i64()? as f64;
    let scale = mon["scale"].as_f64().unwrap_or(1.0).max(0.1);
    // Odd transforms (1, 3, 5, 7) are rotated 90°/270° — swap dimensions.
    if mon["transform"].as_i64().unwrap_or(0) % 2 == 1 {
        std::mem::swap(&mut w, &mut h);
    }
    Some((x, y, (w / scale).round() as i32, (h / scale).round() as i32))
}

/// Warp the cursor to global coordinates (used when the mouse enters from
/// the Windows side so it appears at the matching edge).
pub fn move_cursor(x: i32, y: i32) {
    let xs = x.to_string();
    let ys = y.to_string();
    if hyprctl(&["dispatch", "movecursor", &xs, &ys]).is_none() {
        tracing::warn!("hyprctl dispatch movecursor failed");
    }
}

/// Tell Hyprland to assign `layout` to the virtual uinput device `device_name`
/// at runtime (no config file is touched).
///
/// This is the critical step documented in CLAUDE.md — without it modifiers
/// don't register in Hyprland binds.
pub fn apply_layout(device_name: &str, layout: &str) -> Result<()> {
    let sig = instance_signature()
        .context("could not determine HYPRLAND_INSTANCE_SIGNATURE")?;

    let keyword = format!("device:{device_name}");
    let status = std::process::Command::new("hyprctl")
        .args(["-r", "keyword", &keyword, layout])
        .env("HYPRLAND_INSTANCE_SIGNATURE", &sig)
        .status()
        .context("hyprctl keyword")?;

    if status.success() {
        tracing::info!("applied kb_layout={layout} to device '{device_name}'");
    } else {
        tracing::warn!(
            "hyprctl keyword returned non-zero ({status}) for device '{device_name}'"
        );
    }
    Ok(())
}
