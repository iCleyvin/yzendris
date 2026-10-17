//! Small custom-painted controls shared by every screen.

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Frame, Margin, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};

use crate::theme::{self, Palette, RADIUS};

/// A bordered surface that groups related content.
pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(18))
        .show(ui, add)
        .inner
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Primary,
    Plain,
    Danger,
}

/// A button. Primary is the one action a screen wants you to take.
pub fn button(ui: &mut Ui, p: &Palette, kind: Kind, text: &str) -> Response {
    let font = theme::medium(14.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    let size = Vec2::new(galley.size().x + 32.0, 36.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    let hover = ui
        .ctx()
        .animate_bool_with_time(response.id, response.hovered() && enabled, 0.12);
    let (fill, text_color, stroke) = match kind {
        Kind::Primary => (
            mix(p.accent, p.text, 0.18 * hover),
            p.on_accent,
            Stroke::NONE,
        ),
        Kind::Plain => (
            mix(p.raised, p.border, hover),
            p.text,
            Stroke::new(1.0, p.border),
        ),
        Kind::Danger => (
            Palette::tint(p.danger, 0.10 + 0.10 * hover),
            p.danger,
            Stroke::new(1.0, Palette::tint(p.danger, 0.45)),
        ),
    };
    let (fill, text_color) = if enabled {
        (fill, text_color)
    } else {
        (p.raised, p.faint)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(RADIUS - 1),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    painter.galley_with_override_text_color(
        rect.center() - galley.size() / 2.0,
        galley,
        text_color,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(RADIUS + 1),
            Stroke::new(1.5, p.accent),
            StrokeKind::Outside,
        );
    }
    response.on_hover_cursor(if enabled {
        CursorIcon::PointingHand
    } else {
        CursorIcon::Default
    })
}

/// An on/off switch.
pub fn toggle(ui: &mut Ui, p: &Palette, on: &mut bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(40.0, 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });
    let t = ui.ctx().animate_bool_with_time(response.id, *on, 0.14);
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(11), mix(p.border, p.accent, t));
    let knob = Pos2::new(
        egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t),
        rect.center().y,
    );
    painter.circle_filled(knob, 8.0, if *on { p.on_accent } else { p.muted });
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(13),
            Stroke::new(1.5, p.accent),
            StrokeKind::Outside,
        );
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A small coloured status dot followed by text.
pub fn status(ui: &mut Ui, color: Color32, text: &str, text_color: Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 16.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(
            egui::RichText::new(text)
                .color(text_color)
                .font(theme::regular(13.0)),
        );
    });
}

/// A rounded label with a tinted background.
pub fn pill(ui: &mut Ui, color: Color32, text: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::medium(12.0), color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + Vec2::new(18.0, 8.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(255), Palette::tint(color, 0.14));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
}

pub fn muted(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.label(
        egui::RichText::new(text)
            .color(p.muted)
            .font(theme::regular(13.0)),
    )
}

pub fn title(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(theme::semibold(15.0)));
}

/// One row of a settings list: a label with a hint on the left, a control on
/// the right.
pub fn setting(ui: &mut Ui, p: &Palette, label: &str, hint: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.set_max_width((ui.available_width() - 190.0).max(160.0));
            ui.label(egui::RichText::new(label).font(theme::medium(14.0)));
            if !hint.is_empty() {
                ui.label(
                    egui::RichText::new(hint)
                        .color(p.muted)
                        .font(theme::regular(12.5)),
                );
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

pub fn divider(ui: &mut Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, p.border);
}

/// Segmented navigation, right-aligned in the remaining width. Returns the
/// newly chosen index, if any.
pub fn tabs(ui: &mut Ui, p: &Palette, current: usize, labels: &[&str]) -> Option<usize> {
    let font = theme::medium(13.5);
    let galleys: Vec<_> = labels
        .iter()
        .map(|l| {
            ui.painter()
                .layout_no_wrap((*l).to_owned(), font.clone(), Color32::PLACEHOLDER)
        })
        .collect();
    let gap = 2.0;
    let total: f32 =
        galleys.iter().map(|g| g.size().x + 26.0).sum::<f32>() + gap * (labels.len() as f32 - 1.0);
    let mut chosen = None;
    ui.add_space((ui.available_width() - total).max(0.0));
    ui.spacing_mut().item_spacing.x = gap;
    for (i, galley) in galleys.into_iter().enumerate() {
        let selected = i == current;
        let (rect, response) =
            ui.allocate_exact_size(galley.size() + Vec2::new(26.0, 14.0), Sense::click());
        let hover = ui
            .ctx()
            .animate_bool_with_time(response.id, response.hovered(), 0.12);
        let fill = if selected {
            p.raised
        } else {
            Palette::tint(p.raised, hover)
        };
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
        let color = if selected {
            p.text
        } else {
            mix(p.muted, p.text, hover)
        };
        ui.painter().galley_with_override_text_color(
            rect.center() - galley.size() / 2.0,
            galley,
            color,
        );
        if response.on_hover_cursor(CursorIcon::PointingHand).clicked() && !selected {
            chosen = Some(i);
        }
    }
    chosen
}

/// The cursor arrow, used as the "input is here" marker.
pub fn cursor_glyph(
    painter: &egui::Painter,
    tip: Pos2,
    size: f32,
    fill: Color32,
    outline: Color32,
) {
    let pts = [
        (0.0, 0.0),
        (0.0, 1.0),
        (0.27, 0.76),
        (0.45, 1.16),
        (0.60, 1.09),
        (0.42, 0.70),
        (0.74, 0.70),
    ]
    .map(|(x, y)| tip + Vec2::new(x, y) * size);
    // The arrow is concave: paint it as two convex pieces.
    painter.add(egui::Shape::convex_polygon(
        vec![pts[0], pts[1], pts[2], pts[5], pts[6]],
        fill,
        Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![pts[2], pts[3], pts[4], pts[5]],
        fill,
        Stroke::NONE,
    ));
    painter.add(egui::Shape::closed_line(
        pts.to_vec(),
        Stroke::new(1.2, outline),
    ));
}

/// Centered text inside `rect`, shrunk away if it does not fit.
pub fn fit_text(
    painter: &egui::Painter,
    rect: Rect,
    text: &str,
    font: egui::FontId,
    color: Color32,
    dy: f32,
) -> bool {
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    if galley.size().x > rect.width() - 8.0 || galley.size().y > rect.height() {
        return false;
    }
    painter.galley(
        rect.center() - galley.size() / 2.0 + Vec2::new(0.0, dy),
        galley,
        color,
    );
    true
}
