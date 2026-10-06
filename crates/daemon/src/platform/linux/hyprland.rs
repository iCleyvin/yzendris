//! Hyprland-specific integration.
//!
//! Hyprland matches key bindings against a keymap it compiles per keyboard.
//! A virtual keyboard that appears at runtime can end up without a layout, in
//! which case modifiers (Super, Ctrl, Alt) reach applications but never fire
//! Hyprland's own binds. We give the device the same layout as the physical
//! keyboards. Other compositors apply their global layout to new keyboards
//! and need none of this.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use yzendris_core::Point;

/// Name Hyprland derives from "Yzendris Keyboard".
const DEVICE: &str = "yzendris-keyboard";

/// Directory holding the live instance's IPC sockets, with its signature.
fn instance() -> Option<(PathBuf, String)> {
    let hypr = super::runtime_dir()?.join("hypr");
    let alive = |sig: &str| hypr.join(sig).join(".socket.sock").exists();
    // The inherited variable can name an instance from a previous session.
    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        if !sig.is_empty() && alive(&sig) {
            return Some((hypr.join(&sig), sig));
        }
    }
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(&hypr).ok()?.flatten() {
        let Ok(sig) = entry.file_name().into_string() else {
            continue;
        };
        if !alive(&sig) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
            newest = Some((modified, sig));
        }
    }
    newest.map(|(_, sig)| (hypr.join(&sig), sig))
}

pub fn running() -> bool {
    instance().is_some()
}

/// Run `hyprctl` against the live instance and return its output.
fn hyprctl(args: &[&str]) -> Option<String> {
    let (_, sig) = instance()?;
    let out = Command::new("hyprctl")
        .args(args)
        .env("HYPRLAND_INSTANCE_SIGNATURE", sig)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `hyprctl` exits 0 even when a command is rejected; only "ok" means success.
fn hyprctl_ok(args: &[&str]) -> bool {
    hyprctl(args).is_some_and(|out| out.trim() == "ok")
}

fn keyboards() -> Vec<serde_json::Value> {
    hyprctl(&["devices", "-j"])
        .and_then(|out| serde_json::from_str::<serde_json::Value>(&out).ok())
        .and_then(|v| v["keyboards"].as_array().cloned())
        .unwrap_or_default()
}

fn layout_of(keyboard: &serde_json::Value) -> Option<String> {
    let layout = keyboard["layout"].as_str()?;
    let layout = layout.split('(').next().unwrap_or(layout).trim();
    (!layout.is_empty()).then(|| layout.to_owned())
}

/// Make sure Hyprland has a layout for our virtual keyboard. Runs in the
/// background because the device needs a moment to be registered.
pub fn fix_keyboard_layout(configured: String) {
    if !running() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("hyprland-layout".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            let all = keyboards();
            let ours = all.iter().find(|k| k["name"].as_str() == Some(DEVICE));
            let wanted = if configured.is_empty() {
                all.iter()
                    .filter(|k| !k["name"].as_str().unwrap_or("").contains("yzendris"))
                    .find_map(layout_of)
            } else {
                Some(configured)
            };
            let Some(wanted) = wanted else { return };
            // The value is interpolated into a command below; allow only what an
            // XKB layout list can contain.
            if !wanted
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b',' || b == b'_')
            {
                return;
            }
            // Already right (inherited from the global config): leave it alone —
            // a per-device rule is only worth creating when something is wrong.
            if ours.and_then(layout_of).as_deref() == Some(wanted.as_str()) {
                tracing::debug!("Hyprland already uses layout '{wanted}' for the virtual keyboard");
                return;
            }
            let classic = hyprctl_ok(&[
                "-r",
                "keyword",
                &format!("device:{DEVICE}:kb_layout"),
                &wanted,
            ]);
            // Builds configured through Lua reject `keyword` and want `eval`.
            let applied = classic
                || hyprctl_ok(&[
                    "eval",
                    &format!("hl.device({{ name = '{DEVICE}', kb_layout = '{wanted}' }})"),
                ]);
            if applied {
                tracing::info!("set keyboard layout '{wanted}' on the virtual keyboard (Hyprland)");
            } else {
                tracing::warn!(
                    "could not set the virtual keyboard's layout in Hyprland; \
                 if Super/Ctrl shortcuts do not fire, set `keyboard_layout` in the config"
                );
            }
        });
    if spawned.is_err() {
        tracing::debug!("could not start the Hyprland layout helper");
    }
}

/// Ask the compositor directly (no process spawn) where the cursor is.
pub fn cursor() -> Option<Point> {
    use std::io::{Read, Write};
    let (dir, _) = instance()?;
    let mut socket = std::os::unix::net::UnixStream::connect(dir.join(".socket.sock")).ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok()?;
    socket.write_all(b"j/cursorpos").ok()?;
    let mut reply = String::new();
    socket.take(256).read_to_string(&mut reply).ok()?;
    let v: serde_json::Value = serde_json::from_str(&reply).ok()?;
    Some(Point::new(v["x"].as_i64()? as i32, v["y"].as_i64()? as i32))
}

pub fn warp(to: Point) {
    let (x, y) = (to.x.to_string(), to.y.to_string());
    let moved = hyprctl_ok(&["dispatch", "movecursor", &x, &y])
        || hyprctl_ok(&[
            "eval",
            &format!("hl.dispatch(hl.dsp.cursor.move({{ x = {x}, y = {y} }}))"),
        ]);
    if !moved {
        tracing::debug!("Hyprland refused to move the cursor");
    }
}
