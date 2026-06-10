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
    let uid = libc_getuid();
    let hypr_dir = format!("/run/user/{uid}/hypr");

    // Trust the env var only if its instance is actually alive — after a
    // compositor restart the inherited signature points at a dead socket.
    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        if !sig.is_empty()
            && std::path::Path::new(&format!("{hypr_dir}/{sig}/.socket.sock")).exists()
        {
            return Some(sig);
        }
    }

    // Scan /run/user/<uid>/hypr/: stale dirs from previous sessions can
    // coexist with the live one, so pick the most recently modified dir
    // that still has its IPC socket.
    let entries = std::fs::read_dir(&hypr_dir).ok()?;
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join(".socket.sock").exists() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
            best = Some((mtime, name.to_owned()));
        }
    }
    best.map(|(_, name)| name)
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

/// `hyprctl` exits 0 even when the command fails (e.g. on Lua-config builds
/// the classic syntax prints an error but still returns success), so the only
/// reliable success signal is the literal "ok" reply.
fn hyprctl_ok(args: &[&str]) -> bool {
    match hyprctl(args) {
        Some(out) => {
            let ok = out.trim() == "ok";
            if !ok {
                tracing::debug!("hyprctl {:?} replied: {}", args, out.trim());
            }
            ok
        }
        None => false,
    }
}

/// Whether this Hyprland build uses the Lua (v2) config API, where the
/// classic `hyprctl dispatch movecursor X Y` / `hyprctl keyword …` syntax is
/// rejected ("keyword can't work with non-legacy parsers. Use eval.") and
/// commands must go through `hyprctl eval "hl.…"`. Detected on first use.
static LUA_MODE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

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
    let classic = |x: i32, y: i32| {
        hyprctl_ok(&["dispatch", "movecursor", &x.to_string(), &y.to_string()])
    };
    let lua = |x: i32, y: i32| {
        let expr = format!("hl.dispatch(hl.dsp.cursor.move({{ x = {x}, y = {y} }}))");
        hyprctl_ok(&["eval", &expr])
    };

    let ok = match LUA_MODE.get() {
        Some(false) => classic(x, y),
        Some(true) => lua(x, y),
        None => {
            if classic(x, y) {
                LUA_MODE.set(false).ok();
                true
            } else if lua(x, y) {
                tracing::info!("Hyprland Lua config API detected — using hl.* commands");
                LUA_MODE.set(true).ok();
                true
            } else {
                false
            }
        }
    };
    if !ok {
        tracing::warn!("movecursor({x},{y}) failed in both classic and Lua forms");
    }
}

/// Tell Hyprland to assign `layout` to the virtual uinput device `device_name`
/// at runtime (no config file is touched).
///
/// This is the critical step documented in CLAUDE.md — without it modifiers
/// don't register in Hyprland binds.
pub fn apply_layout(device_name: &str, layout: &str) -> Result<()> {
    instance_signature().context("could not determine HYPRLAND_INSTANCE_SIGNATURE")?;

    // `layout` may come from a user-edited config and is interpolated into a
    // Lua expression for `hyprctl eval`, so reject anything that isn't a plain
    // XKB layout token to avoid Lua injection (e.g. "es'}}) os.execute(...) --").
    // Valid XKB layouts look like "es", "us", "es,us", "latam".
    if !layout.bytes().all(|b| b.is_ascii_lowercase() || b == b',') || layout.is_empty() {
        tracing::warn!("rejecting suspicious kb_layout '{layout}' — falling back to 'us'");
        return apply_layout(device_name, "us");
    }

    // If the compositor's global config already gives the device the right
    // layout, do NOT touch it: creating a per-device rule (Lua hl.device)
    // has been observed to degrade event processing for the device.
    if device_layout(device_name).as_deref() == Some(layout) {
        tracing::info!(
            "kb_layout '{layout}' already active on '{device_name}' (inherited) — nothing to do"
        );
        return Ok(());
    }

    // Classic (pre-Lua) runtime keyword.
    let keyword = format!("device:{device_name}:kb_layout");
    let classic_ok = hyprctl_ok(&["-r", "keyword", &keyword, layout]);

    // Lua (v2) config API fallback.
    let lua_ok = if classic_ok {
        LUA_MODE.set(false).ok();
        false
    } else {
        let expr = format!("hl.device({{ name = '{device_name}', kb_layout = '{layout}' }})");
        let ok = hyprctl_ok(&["eval", &expr]);
        if ok {
            tracing::info!("Hyprland Lua config API detected — using hl.* commands");
            LUA_MODE.set(true).ok();
        }
        ok
    };

    // Verify against what Hyprland actually reports for the device.
    let actual = device_layout(device_name);
    match (&actual, classic_ok || lua_ok) {
        (Some(actual), _) if actual == layout => {
            tracing::info!("kb_layout '{layout}' active on device '{device_name}'");
        }
        (Some(actual), applied) => {
            tracing::warn!(
                "kb_layout mismatch on '{device_name}': wanted '{layout}', device reports \
                 '{actual}' (apply command success: {applied})"
            );
        }
        (None, applied) => {
            tracing::warn!(
                "could not verify kb_layout on '{device_name}' (apply command success: {applied})"
            );
        }
    }
    Ok(())
}

/// Layout that Hyprland currently reports for `device_name`, if visible.
fn device_layout(device_name: &str) -> Option<String> {
    let out = hyprctl(&["devices", "-j"])?;
    let json: serde_json::Value = serde_json::from_str(&out).ok()?;
    json["keyboards"]
        .as_array()?
        .iter()
        .find(|kb| kb["name"].as_str() == Some(device_name))
        .and_then(|kb| kb["layout"].as_str())
        .map(|s| s.split('(').next().unwrap_or(s).trim().to_owned())
}
