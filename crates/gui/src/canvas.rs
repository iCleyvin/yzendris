//! The arrangement view: the host's monitors drawn to scale with every
//! client docked on the side it is attached to. Dragging a client onto
//! another side re-attaches it.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use yzendris_core::config::Config;
use yzendris_core::geom::{self, Side};
use yzendris_core::ipc::{Link, Status};
use yzendris_core::layout::{find_monitor, neighbor_of, Screen};
use yzendris_core::Fingerprint;

use crate::i18n::tr;
use crate::theme::{self, Palette};
use crate::widgets;

#[derive(Clone, Copy, PartialEq)]
struct Slot {
    monitor: usize,
    side: Side,
}

/// A client being dragged.
#[derive(Clone, Copy)]
pub struct Drag {
    client: Fingerprint,
    /// Pointer offset from the dragged box's centre.
    grab: Vec2,
}

pub enum Action {
    Move {
        client: Fingerprint,
        monitor: String,
        side: Side,
    },
}

struct View {
    /// Top-left of the world in screen points, and points per world pixel.
    origin: Pos2,
    scale: f32,
    /// World coordinates of the top-left corner.
    world: (f32, f32),
}

impl View {
    fn rect(&self, r: geom::Rect) -> Rect {
        let min = self.origin
            + Vec2::new(r.x as f32 - self.world.0, r.y as f32 - self.world.1) * self.scale;
        Rect::from_min_size(min, Vec2::new(r.w as f32, r.h as f32) * self.scale)
    }
}

/// World-space box where a client attached to `slot` is drawn.
fn slot_rect(monitors: &[Screen], slot: Slot, size: (i32, i32)) -> geom::Rect {
    let m = monitors[slot.monitor].rect;
    let (w, h) = size;
    let (cx, cy) = (m.x + m.w / 2, m.y + m.h / 2);
    let Some(n) = neighbor_of(monitors, m, slot.side) else {
        return match slot.side {
            Side::Right => geom::Rect::new(m.right(), cy - h / 2, w, h),
            Side::Left => geom::Rect::new(m.x - w, cy - h / 2, w, h),
            Side::Bottom => geom::Rect::new(cx - w / 2, m.bottom(), w, h),
            Side::Top => geom::Rect::new(cx - w / 2, m.y - h, w, h),
        };
    };
    // Between two monitors the client straddles their shared edge, drawn
    // smaller and off-centre so it does not cover the monitors' labels.
    let (w, h) = (w * 2 / 3, h * 2 / 3);
    let (lo, hi) = m.span(slot.side);
    let (nlo, nhi) = n.span(slot.side);
    let (lo, hi) = (lo.max(nlo), hi.min(nhi));
    let along = lo + (hi - lo) * 3 / 4;
    match slot.side {
        Side::Right => geom::Rect::new(
            m.right() - w / 2,
            (along - h / 2).clamp(lo, (hi - h).max(lo)),
            w,
            h,
        ),
        Side::Left => geom::Rect::new(
            m.x - w / 2,
            (along - h / 2).clamp(lo, (hi - h).max(lo)),
            w,
            h,
        ),
        Side::Bottom => geom::Rect::new(
            (along - w / 2).clamp(lo, (hi - w).max(lo)),
            m.bottom() - h / 2,
            w,
            h,
        ),
        Side::Top => geom::Rect::new(
            (along - w / 2).clamp(lo, (hi - w).max(lo)),
            m.y - h / 2,
            w,
            h,
        ),
    }
}

/// Draw the arrangement. Returns a change the user made by dragging.
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    status: &Status,
    config: &Config,
    drag: &mut Option<Drag>,
) -> Option<Action> {
    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::hover());
    let area = response.rect;
    painter.rect(
        area,
        CornerRadius::same(14),
        p.surface,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );

    // Subtle dot grid: this is a map, not a form.
    let step = 22.0;
    let mut y = area.top() + step;
    while y < area.bottom() {
        let mut x = area.left() + step;
        while x < area.right() {
            painter.circle_filled(Pos2::new(x, y), 0.8, Palette::tint(p.faint, 0.35));
            x += step;
        }
        y += step;
    }

    let monitors = &status.screens;
    let Some(bounds) = geom::Rect::bounding(&monitors.iter().map(|m| m.rect).collect::<Vec<_>>())
    else {
        painter.text(
            area.center(),
            Align2::CENTER_CENTER,
            tr("No screen detected", "No se detectó ninguna pantalla"),
            theme::regular(14.0),
            p.muted,
        );
        return None;
    };

    // Every client is drawn at one common size so the layout stays calm;
    // the room for it is reserved on all four sides.
    let largest = monitors
        .iter()
        .map(|m| m.rect.w.max(m.rect.h))
        .max()
        .unwrap_or(1920);
    let unit = (largest as f32 * 0.42) as i32;
    let client_size = |fp: &Fingerprint| {
        let screens = status
            .peers
            .iter()
            .find(|c| c.fingerprint == *fp)
            .map(|c| c.screens.as_slice())
            .unwrap_or(&[]);
        let (w, h) = geom::Rect::bounding(screens).map_or((16, 10), |b| (b.w, b.h));
        let k = unit as f32 / w.max(h) as f32;
        ((w as f32 * k) as i32, (h as f32 * k) as i32)
    };
    // ── Where can a client go? ──────────────────────────────────────────────
    let all_slots: Vec<Slot> = (0..monitors.len())
        .flat_map(|monitor| Side::ALL.map(|side| Slot { monitor, side }))
        // A shared edge is one place, owned by the monitor on its left/top.
        .filter(|s| {
            !(neighbor_of(monitors, monitors[s.monitor].rect, s.side).is_some()
                && matches!(s.side, Side::Left | Side::Top))
        })
        .collect();
    let slot_of = |monitor: &str, side: Side| -> Option<Slot> {
        let m = find_monitor(monitors, monitor)?;
        let index = monitors.iter().position(|x| x.name == m.name)?;
        // Normalise a shared edge to the slot that owns it.
        match neighbor_of(monitors, m.rect, side) {
            Some(n) if matches!(side, Side::Left | Side::Top) => {
                let owner = monitors.iter().position(|x| x.rect == n)?;
                Some(Slot {
                    monitor: owner,
                    side: side.opposite(),
                })
            }
            _ => Some(Slot {
                monitor: index,
                side,
            }),
        }
    };

    // Frame what is there; while dragging, zoom out to reveal every place a
    // client could go. The change is animated so the map never jumps.
    let mut shown: Vec<geom::Rect> = monitors.iter().map(|m| m.rect).collect();
    if drag.is_some() || config.host.clients.is_empty() {
        shown.extend(
            all_slots
                .iter()
                .map(|s| slot_rect(monitors, *s, (unit, unit * 5 / 8))),
        );
    } else {
        shown.extend(config.host.clients.iter().filter_map(|e| {
            Some(slot_rect(
                monitors,
                slot_of(&e.monitor, e.side)?,
                client_size(&e.fingerprint),
            ))
        }));
    }
    let target = geom::Rect::bounding(&shown).unwrap_or(bounds);
    let glide = |tag: &str, v: i32| {
        ui.ctx()
            .animate_value_with_time(ui.id().with(tag), v as f32, 0.22)
    };
    let (wx, wy, ww, wh) = (
        glide("wx", target.x),
        glide("wy", target.y),
        glide("ww", target.w),
        glide("wh", target.h),
    );
    let pad = 34.0;
    let scale = ((area.width() - pad * 2.0) / ww)
        .min((area.height() - pad * 2.0 - 16.0) / wh)
        .min(0.22);
    let origin = area.center() - Vec2::new(0.0, 6.0) - Vec2::new(ww, wh) * scale / 2.0;
    let view = View {
        origin,
        scale,
        world: (wx, wy),
    };

    // ── Host monitors ───────────────────────────────────────────────────────
    for m in monitors {
        let r = view.rect(m.rect).shrink(2.0);
        painter.rect(
            r,
            CornerRadius::same(9),
            p.raised,
            Stroke::new(1.0, p.border),
            StrokeKind::Inside,
        );
        // A stand hint makes it read as "screen" at a glance.
        let named = widgets::fit_text(&painter, r, &m.name, theme::semibold(14.0), p.text, -9.0);
        if named {
            widgets::fit_text(
                &painter,
                r,
                &format!("{} × {}", m.rect.w, m.rect.h),
                theme::regular(12.0),
                p.muted,
                10.0,
            );
        }
        if m.primary && monitors.len() > 1 && r.width() > 90.0 {
            painter.text(
                r.left_top() + Vec2::new(10.0, 9.0),
                Align2::LEFT_TOP,
                tr("main", "principal"),
                theme::regular(11.0),
                p.faint,
            );
        }
    }
    if status.active.is_none() {
        if let Some(m) = monitors.iter().find(|m| m.primary).or(monitors.first()) {
            let r = view.rect(m.rect);
            widgets::cursor_glyph(
                &painter,
                r.left_bottom() + Vec2::new(16.0, -30.0),
                13.0,
                p.text,
                p.bg,
            );
        }
    }

    let pointer = ui.input(|i| i.pointer.interact_pos());
    let mut action = None;
    let mut hover_slot = None;

    if let (Some(d), Some(pos)) = (drag.as_ref(), pointer) {
        let size = client_size(&d.client);
        hover_slot = all_slots.iter().copied().min_by(|a, b| {
            let da = view
                .rect(slot_rect(monitors, *a, size))
                .center()
                .distance(pos - d.grab);
            let db = view
                .rect(slot_rect(monitors, *b, size))
                .center()
                .distance(pos - d.grab);
            da.total_cmp(&db)
        });
        for slot in &all_slots {
            let r = view.rect(slot_rect(monitors, *slot, size)).shrink(3.0);
            let target = hover_slot == Some(*slot);
            let color = if target {
                p.accent
            } else {
                Palette::tint(p.faint, 0.6)
            };
            if target {
                painter.rect_filled(r, CornerRadius::same(9), Palette::tint(p.accent, 0.12));
            }
            dashed_rect(
                &painter,
                r,
                Stroke::new(if target { 1.6 } else { 1.0 }, color),
            );
        }
    }

    // ── Clients ─────────────────────────────────────────────────────────────
    let mut stacked: Vec<Slot> = Vec::new();
    for entry in &config.host.clients {
        let peer = status
            .peers
            .iter()
            .find(|c| c.fingerprint == entry.fingerprint);
        let online = peer.is_some_and(|c| c.link == Link::Online) && entry.enabled;
        let active = status.active.as_deref() == Some(entry.name.as_str());
        let Some(slot) = slot_of(&entry.monitor, entry.side) else {
            continue;
        };
        let size = client_size(&entry.fingerprint);
        let dragging = drag.as_ref().is_some_and(|d| d.client == entry.fingerprint);

        let mut r = view.rect(slot_rect(monitors, slot, size)).shrink(3.0);
        // Two clients on the same side: fan them out instead of hiding one.
        let already = stacked.iter().filter(|s| **s == slot).count() as f32;
        stacked.push(slot);
        r = r.translate(Vec2::splat(14.0 * already));
        if let (true, Some(d), Some(pos)) = (dragging, drag.as_ref(), pointer) {
            r = Rect::from_center_size(pos - d.grab, r.size());
        }

        let id = ui.id().with(("client", entry.fingerprint));
        let resp = ui.interact(r, id, Sense::click_and_drag());
        if resp.drag_started() {
            if let Some(pos) = pointer {
                *drag = Some(Drag {
                    client: entry.fingerprint,
                    grab: pos - r.center(),
                });
            }
        }
        if resp.drag_stopped() && dragging {
            if let Some(target) = hover_slot.filter(|t| *t != slot) {
                action = Some(Action::Move {
                    client: entry.fingerprint,
                    monitor: monitors[target.monitor].name.clone(),
                    side: target.side,
                });
            }
            *drag = None;
        }
        let hover = ui
            .ctx()
            .animate_bool_with_time(id, resp.hovered() || dragging, 0.12);

        let line = if online { p.accent } else { p.faint };
        let fill = if online {
            Palette::tint(p.accent, 0.16 + 0.06 * hover)
        } else {
            Palette::tint(p.faint, 0.10 + 0.05 * hover)
        };
        if dragging {
            painter.rect_filled(
                r.translate(Vec2::new(0.0, 6.0)).expand(2.0),
                CornerRadius::same(12),
                Color32::from_black_alpha(60),
            );
        }
        painter.rect_filled(r, CornerRadius::same(9), p.surface);
        painter.rect(
            r,
            CornerRadius::same(9),
            fill,
            Stroke::new(if active { 2.0 } else { 1.2 }, line),
            StrokeKind::Inside,
        );
        if !entry.enabled {
            widgets::fit_text(
                &painter,
                r,
                tr("paused", "en pausa"),
                theme::regular(11.5),
                p.faint,
                10.0,
            );
        } else if !online {
            widgets::fit_text(
                &painter,
                r,
                tr("offline", "sin conexión"),
                theme::regular(11.5),
                p.faint,
                10.0,
            );
        } else if let Some(b) = peer.and_then(|c| geom::Rect::bounding(&c.screens)) {
            widgets::fit_text(
                &painter,
                r,
                &format!("{} × {}", b.w, b.h),
                theme::regular(11.5),
                p.muted,
                10.0,
            );
        }
        let name_color = if online { p.text } else { p.muted };
        if !widgets::fit_text(
            &painter,
            r,
            &entry.name,
            theme::semibold(13.5),
            name_color,
            -8.0,
        ) {
            let short: String = entry
                .name
                .chars()
                .take(1)
                .collect::<String>()
                .to_uppercase();
            widgets::fit_text(&painter, r, &short, theme::semibold(13.5), name_color, 0.0);
        }
        if active {
            widgets::cursor_glyph(
                &painter,
                r.right_bottom() - Vec2::new(22.0, 24.0),
                12.0,
                p.accent,
                p.bg,
            );
        }
        if resp.hovered() && drag.is_none() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    if drag.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if !ui.input(|i| i.pointer.any_down()) {
            *drag = None; // released outside any client
        }
    }

    if config.host.clients.is_empty() {
        painter.text(
            area.center_bottom() - Vec2::new(0.0, 22.0),
            Align2::CENTER_BOTTOM,
            tr(
                "Add a device and it will appear beside your screen",
                "Añade un dispositivo y aparecerá junto a tu pantalla",
            ),
            theme::regular(13.0),
            p.muted,
        );
    } else if config.host.clients.len() <= 3 {
        painter.text(
            area.center_bottom() - Vec2::new(0.0, 14.0),
            Align2::CENTER_BOTTOM,
            tr(
                "Drag a device to the side where it physically sits",
                "Arrastra cada dispositivo al lado donde está físicamente",
            ),
            theme::regular(12.0),
            p.faint,
        );
    }
    action
}

fn dashed_rect(painter: &egui::Painter, r: Rect, stroke: Stroke) {
    let c = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    painter.extend(egui::Shape::dashed_line(&c, stroke, 5.0, 4.0));
}
