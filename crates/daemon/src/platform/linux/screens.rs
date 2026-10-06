//! Finding this machine's monitors.
//!
//! Tried in order: the Wayland compositor (standard `wl_output` +
//! `xdg-output`, implemented by every compositor), the X server (RandR), and
//! finally the kernel's display list, which needs no session at all.

use std::os::unix::net::UnixStream;
use std::path::Path;

use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use yzendris_core::geom::Rect;
use yzendris_core::layout::Screen;

pub fn screens() -> Vec<Screen> {
    let mut found = wayland().unwrap_or_default();
    if found.is_empty() {
        found = x11().unwrap_or_default();
    }
    if found.is_empty() {
        found = drm();
    }
    found.retain(|s| s.rect.is_valid());
    if !found.iter().any(|s| s.primary) {
        // Wayland has no notion of a primary output: prefer the one at the
        // origin, else the first.
        let at_origin = found.iter().position(|s| s.rect.x == 0 && s.rect.y == 0);
        if let Some(s) = found.get_mut(at_origin.unwrap_or(0)) {
            s.primary = true;
        }
    }
    found
}

/// Name of a live Wayland socket in `runtime` (`wayland-0`, `wayland-1`, …).
pub fn wayland_socket_name(runtime: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(runtime)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
        .collect();
    names.sort();
    names
        .into_iter()
        .find(|n| UnixStream::connect(runtime.join(n)).is_ok())
}

// ── Wayland ─────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Output {
    proxy: Option<wl_output::WlOutput>,
    name: String,
    /// Position and size in compositor (logical) coordinates, from xdg-output.
    logical: Option<(i32, i32, i32, i32)>,
    /// Fallbacks from core `wl_output`.
    origin: (i32, i32),
    mode: (i32, i32),
    scale: i32,
    rotated: bool,
}

#[derive(Default)]
struct Probe {
    outputs: Vec<Output>,
    manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_output" => {
                let index = state.outputs.len();
                let proxy =
                    registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, index);
                state.outputs.push(Output {
                    proxy: Some(proxy),
                    scale: 1,
                    ..Output::default()
                });
            }
            "zxdg_output_manager_v1" => {
                state.manager = Some(registry.bind(name, version.min(3), qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(out) = state.outputs.get_mut(*index) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                out.origin = (x, y);
                out.rotated = matches!(
                    transform,
                    WEnum::Value(
                        wl_output::Transform::_90
                            | wl_output::Transform::_270
                            | wl_output::Transform::Flipped90
                            | wl_output::Transform::Flipped270
                    )
                );
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) {
                    out.mode = (width, height);
                }
            }
            wl_output::Event::Scale { factor } => out.scale = factor.max(1),
            wl_output::Event::Name { name } => out.name = name,
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(out) = state.outputs.get_mut(*index) else {
            return;
        };
        let logical = out.logical.get_or_insert((0, 0, 0, 0));
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => (logical.0, logical.1) = (x, y),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                (logical.2, logical.3) = (width, height)
            }
            zxdg_output_v1::Event::Name { name } if out.name.is_empty() => out.name = name,
            _ => {}
        }
    }
}

fn wayland() -> Option<Vec<Screen>> {
    let conn = Connection::connect_to_env().ok()?;
    let mut queue = conn.new_event_queue::<Probe>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut probe = Probe::default();
    queue.roundtrip(&mut probe).ok()?;
    if let Some(manager) = probe.manager.clone() {
        for (index, out) in probe.outputs.iter().enumerate() {
            if let Some(proxy) = &out.proxy {
                manager.get_xdg_output(proxy, &qh, index);
            }
        }
    }
    // Second trip delivers the output details and the xdg-output replies.
    queue.roundtrip(&mut probe).ok()?;

    let screens = probe
        .outputs
        .into_iter()
        .enumerate()
        .map(|(i, out)| {
            let rect = match out.logical {
                Some((x, y, w, h)) if w > 0 && h > 0 => Rect::new(x, y, w, h),
                _ => {
                    let (mut w, mut h) = (out.mode.0 / out.scale, out.mode.1 / out.scale);
                    if out.rotated {
                        std::mem::swap(&mut w, &mut h);
                    }
                    Rect::new(out.origin.0, out.origin.1, w, h)
                }
            };
            let name = if out.name.is_empty() {
                format!("output-{}", i + 1)
            } else {
                out.name
            };
            Screen {
                name,
                rect,
                primary: false,
            }
        })
        .collect();
    Some(screens)
}

// ── X11 ─────────────────────────────────────────────────────────────────────

fn x11() -> Option<Vec<Screen>> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::ConnectionExt as _;

    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let screen = conn.setup().roots.get(screen_num)?;
    let whole = Rect::new(
        0,
        0,
        i32::from(screen.width_in_pixels),
        i32::from(screen.height_in_pixels),
    );
    let monitors = conn
        .randr_get_monitors(screen.root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.monitors)
        .unwrap_or_default();
    let mut screens: Vec<Screen> = monitors
        .into_iter()
        .map(|m| {
            let name = conn
                .get_atom_name(m.name)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                .unwrap_or_default();
            Screen {
                name,
                rect: Rect::new(
                    i32::from(m.x),
                    i32::from(m.y),
                    i32::from(m.width),
                    i32::from(m.height),
                ),
                primary: m.primary,
            }
        })
        .collect();
    if screens.is_empty() {
        screens.push(Screen {
            name: "screen".into(),
            rect: whole,
            primary: true,
        });
    }
    Some(screens)
}

// ── Kernel (DRM) ────────────────────────────────────────────────────────────

/// Connected displays as the kernel lists them, laid out left to right at
/// their preferred resolution. Scaling and arrangement are unknown here, so
/// this is only a last resort.
fn drm() -> Vec<Screen> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut connectors: Vec<(String, i32, i32)> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let status = std::fs::read_to_string(dir.join("status")).ok()?;
            if status.trim() != "connected" {
                return None;
            }
            let modes = std::fs::read_to_string(dir.join("modes")).ok()?;
            let (w, h) = modes
                .lines()
                .next()?
                .trim_end_matches('i')
                .split_once('x')?;
            let full = entry.file_name().into_string().ok()?;
            // "card1-HDMI-A-1" → "HDMI-A-1"
            let name = full
                .split_once('-')
                .map_or(full.as_str(), |(_, n)| n)
                .to_owned();
            Some((name, w.parse().ok()?, h.parse().ok()?))
        })
        .collect();
    connectors.sort();
    let mut x = 0;
    connectors
        .into_iter()
        .map(|(name, w, h)| {
            let rect = Rect::new(x, 0, w, h);
            x += w;
            Screen {
                name,
                rect,
                primary: false,
            }
        })
        .collect()
}
