//! Visual language: a deep-navy and steel-blue palette taken from the app
//! icon, the Geist typeface, generous spacing and soft corners.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    TextStyle, Theme,
};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub danger: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const DARK: Palette = Palette {
    bg: rgb(0x0E1220),
    surface: rgb(0x151A2C),
    raised: rgb(0x1C2238),
    border: rgb(0x29314D),
    text: rgb(0xE9EDF6),
    muted: rgb(0x8E98B6),
    faint: rgb(0x5B6585),
    accent: rgb(0x86AEE0),
    on_accent: rgb(0x0E1220),
    ok: rgb(0x6DD3A4),
    warn: rgb(0xF0B765),
    danger: rgb(0xF07F7F),
};

pub const LIGHT: Palette = Palette {
    bg: rgb(0xF3F5FA),
    surface: rgb(0xFFFFFF),
    raised: rgb(0xEAEEF6),
    border: rgb(0xD3DAE9),
    text: rgb(0x161B2E),
    muted: rgb(0x5C6787),
    faint: rgb(0x97A0B8),
    accent: rgb(0x3C6AA3),
    on_accent: rgb(0xFFFFFF),
    ok: rgb(0x1B8F60),
    warn: rgb(0xA86B12),
    danger: rgb(0xC24545),
};

impl Palette {
    pub fn of(theme: Theme) -> Self {
        match theme {
            Theme::Dark => DARK,
            Theme::Light => LIGHT,
        }
    }

    /// `color` at `alpha` (0–1) over transparent.
    pub fn tint(color: Color32, alpha: f32) -> Color32 {
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), (alpha * 255.0) as u8)
    }
}

pub const RADIUS: u8 = 10;

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8], family: FontFamily| {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
        // Ours first; egui's bundled fonts stay behind as glyph fallback.
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.to_owned());
    };
    add(
        "geist",
        include_bytes!("../assets/Geist-Regular.ttf"),
        FontFamily::Proportional,
    );
    add(
        "geist-mono",
        include_bytes!("../assets/GeistMono-Regular.ttf"),
        FontFamily::Monospace,
    );
    add(
        "geist-medium",
        include_bytes!("../assets/Geist-Medium.ttf"),
        FontFamily::Name("medium".into()),
    );
    add(
        "geist-semibold",
        include_bytes!("../assets/Geist-SemiBold.ttf"),
        FontFamily::Name("semibold".into()),
    );
    for family in ["medium", "semibold"] {
        let fallback = fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        fonts
            .families
            .entry(FontFamily::Name(family.into()))
            .or_default()
            .extend(fallback);
    }
    ctx.set_fonts(fonts);

    for theme in [Theme::Dark, Theme::Light] {
        let p = Palette::of(theme);
        ctx.style_mut_of(theme, |style| {
            style.text_styles = [
                (TextStyle::Heading, semibold(22.0)),
                (TextStyle::Body, regular(14.0)),
                (TextStyle::Button, medium(14.0)),
                (TextStyle::Small, regular(12.0)),
                (TextStyle::Monospace, mono(12.5)),
            ]
            .into();
            let s = &mut style.spacing;
            s.item_spacing = egui::vec2(10.0, 10.0);
            s.button_padding = egui::vec2(14.0, 8.0);
            s.interact_size.y = 34.0;
            s.window_margin = Margin::same(0);
            s.scroll.bar_width = 6.0;
            s.scroll.floating = true;

            let v = &mut style.visuals;
            v.dark_mode = theme == Theme::Dark;
            v.panel_fill = p.bg;
            v.window_fill = p.surface;
            v.extreme_bg_color = p.bg;
            v.faint_bg_color = p.raised;
            v.override_text_color = Some(p.text);
            v.hyperlink_color = p.accent;
            v.selection.bg_fill = Palette::tint(p.accent, 0.35);
            v.selection.stroke = Stroke::new(1.0, p.accent);
            v.window_stroke = Stroke::new(1.0, p.border);
            v.window_corner_radius = CornerRadius::same(14);
            v.menu_corner_radius = CornerRadius::same(RADIUS);
            let radius = CornerRadius::same(RADIUS - 2);
            let w = &mut v.widgets;
            for state in [
                &mut w.noninteractive,
                &mut w.inactive,
                &mut w.hovered,
                &mut w.active,
                &mut w.open,
            ] {
                state.corner_radius = radius;
                state.fg_stroke = Stroke::new(1.0, p.text);
                state.bg_stroke = Stroke::new(1.0, p.border);
                state.bg_fill = p.raised;
                state.weak_bg_fill = p.raised;
                state.expansion = 0.0;
            }
            w.noninteractive.bg_fill = p.surface;
            w.noninteractive.fg_stroke = Stroke::new(1.0, p.muted);
            w.hovered.bg_stroke = Stroke::new(1.0, p.faint);
            w.hovered.weak_bg_fill = p.border;
            w.hovered.bg_fill = p.border;
            w.active.bg_stroke = Stroke::new(1.0, p.accent);
            w.active.weak_bg_fill = p.border;
        });
    }
}
