//! `yzendris-gui` — the control panel.
//!
//! A thin window onto the background service: it shows what the service
//! reports and sends it the changes you make. Closing the window never stops
//! keyboard and mouse sharing.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod backend;
mod canvas;
mod demo;
mod i18n;
mod theme;
mod views;
mod widgets;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, CornerRadius, RichText, Vec2};
use serde::{Deserialize, Serialize};
use yzendris_core::config::Role;
use yzendris_core::{paths, Fingerprint};

use backend::Backend;
use i18n::{tr, Lang};
use theme::Palette;

const ICON: &[u8] = include_bytes!("../../../assets/icon.png");

/// Preferences of the window itself (not of the service).
#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Prefs {
    /// `""` follows the system; otherwise `en` or `es`.
    pub language: String,
    /// `system`, `dark` or `light`.
    pub theme: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            language: String::new(),
            theme: "system".into(),
        }
    }
}

impl Prefs {
    fn path() -> PathBuf {
        paths::config_dir().join("gui.toml")
    }

    fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let Ok(text) = toml::to_string_pretty(self) {
            let _ = yzendris_core::config::write_private(&Self::path(), text.as_bytes());
        }
    }

    fn apply(&self, ctx: &egui::Context) {
        i18n::set(match self.language.as_str() {
            "es" => Lang::Es,
            "en" => Lang::En,
            _ => i18n::system(),
        });
        ctx.set_theme(match self.theme.as_str() {
            "dark" => egui::ThemePreference::Dark,
            "light" => egui::ThemePreference::Light,
            _ => egui::ThemePreference::System,
        });
    }
}

pub struct App {
    pub backend: Backend,
    pub prefs: Prefs,
    pub prefs_changed: bool,
    pub tab: usize,
    pub drag: Option<canvas::Drag>,
    pub join_addr: String,
    pub join_code: String,
    pub manual: bool,
    pub name_draft: Option<String>,
    pub confirm_forget: Option<Fingerprint>,
    toast: Option<(String, Instant)>,
    logo: egui::TextureHandle,
    persist: bool,
    opened: Instant,
    screenshot: Option<PathBuf>,
    frames: u32,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, args: &Args) -> Self {
        theme::install(&cc.egui_ctx);
        let mut prefs = if args.demo.is_some() {
            Prefs::default()
        } else {
            Prefs::load()
        };
        if let Some(lang) = &args.lang {
            prefs.language = lang.clone();
        }
        if let Some(theme) = &args.theme {
            prefs.theme = theme.clone();
        }
        prefs.apply(&cc.egui_ctx);
        let backend = match &args.demo {
            Some(scene) => Backend::offline(demo::scene(scene)),
            None => Backend::start(cc.egui_ctx.clone()),
        };
        let logo = {
            let image = image::load_from_memory(ICON)
                .expect("bundled icon is valid")
                .to_rgba8();
            let size = [image.width() as usize, image.height() as usize];
            let pixels = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
            cc.egui_ctx
                .load_texture("logo", pixels, egui::TextureOptions::LINEAR)
        };
        Self {
            backend,
            prefs,
            prefs_changed: false,
            tab: args.tab,
            drag: None,
            join_addr: String::new(),
            join_code: String::new(),
            manual: false,
            name_draft: None,
            confirm_forget: None,
            toast: None,
            logo,
            persist: args.demo.is_none(),
            opened: Instant::now(),
            screenshot: args.screenshot.clone(),
            frames: 0,
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, p: &Palette, role: Option<Role>, name: &str) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.add(
                egui::Image::new(&self.logo)
                    .fit_to_exact_size(Vec2::splat(28.0))
                    .corner_radius(CornerRadius::same(7)),
            );
            ui.label(RichText::new("Yzendris").font(theme::semibold(17.0)));
            if let Some(role) = role {
                let label = match role {
                    Role::Host => tr("host", "host"),
                    Role::Client => tr("client", "cliente"),
                };
                widgets::pill(ui, p.accent, label);
                ui.label(
                    RichText::new(name)
                        .color(p.muted)
                        .font(theme::regular(13.5)),
                );
                let first = match role {
                    Role::Host => tr("Screens", "Pantallas"),
                    Role::Client => tr("Connection", "Conexión"),
                };
                let labels = [
                    first,
                    tr("Settings", "Ajustes"),
                    tr("Activity", "Actividad"),
                ];
                if let Some(tab) = widgets::tabs(ui, p, self.tab, &labels) {
                    self.tab = tab;
                    self.confirm_forget = None;
                }
            }
        });
    }

    fn capture(&mut self, ui: &egui::Ui) {
        let Some(path) = self.screenshot.clone() else {
            return;
        };
        self.frames += 1;
        ui.ctx().request_repaint();
        if self.frames == 12 {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let shot = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            let _ = image::save_buffer(
                &path,
                image.as_raw(),
                image.width() as u32,
                image.height() as u32,
                image::ColorType::Rgba8,
            );
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let p = Palette::of(ui.ctx().theme());
        let (online, status, config) = {
            let mut shared = self.backend.lock();
            shared.last_frame = Some(Instant::now());
            // Screens that need these set them again while they are visible.
            shared.want_logs = false;
            shared.want_hosts = false;
            if let Some(error) = shared.error.take() {
                self.toast = Some((error, Instant::now()));
            }
            (shared.online, shared.status.clone(), shared.config.clone())
        };
        // Keep the picture fresh without spinning: one repaint a second.
        ui.ctx().request_repaint_after(Duration::from_secs(1));

        let role = config.as_ref().and_then(|c| c.role);
        let name = config.as_ref().map(|c| c.name.clone()).unwrap_or_default();
        egui::Panel::top("header")
            .frame(egui::Frame::NONE.fill(p.bg).inner_margin(egui::Margin {
                left: 22,
                right: 22,
                top: 14,
                bottom: 10,
            }))
            .show(ui, |ui| self.header(ui, &p, role, &name));

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(p.bg).inner_margin(egui::Margin {
                left: 22,
                right: 22,
                top: 6,
                bottom: 20,
            }))
            .show(ui, |ui| match (&status, &config) {
                (Some(status), Some(config)) if online => match (role, self.tab) {
                    (None, _) => views::welcome(self, ui, &p),
                    (_, 1) => views::settings(self, ui, &p, status, config),
                    (_, 2) => views::activity(self, ui, &p),
                    (Some(Role::Host), _) => views::host(self, ui, &p, status, config),
                    (Some(Role::Client), _) => views::client(self, ui, &p, status, config),
                },
                _ => views::starting(ui, &p, self.opened.elapsed()),
            });

        if let Some((text, since)) = &self.toast {
            if since.elapsed() < Duration::from_secs(6) {
                views::toast(ui, &p, text);
            } else {
                self.toast = None;
            }
        }
        if std::mem::take(&mut self.prefs_changed) {
            self.prefs.apply(ui.ctx());
            if self.persist {
                self.prefs.save();
            }
        }
        self.capture(ui);
    }
}

#[derive(Default)]
struct Args {
    demo: Option<String>,
    lang: Option<String>,
    theme: Option<String>,
    screenshot: Option<PathBuf>,
    tab: usize,
    size: Option<(f32, f32)>,
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--demo" => args.demo = it.next(),
            "--lang" => args.lang = it.next(),
            "--theme" => args.theme = it.next(),
            "--screenshot" => args.screenshot = it.next().map(PathBuf::from),
            "--tab" => args.tab = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--size" => {
                args.size = it.next().and_then(|v| {
                    let (w, h) = v.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                })
            }
            _ => {}
        }
    }
    args
}

fn main() -> eframe::Result {
    let args = parse_args();
    let size = args.size.unwrap_or((980.0, 640.0));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Yzendris")
        .with_app_id("yzendris")
        .with_inner_size([size.0, size.1])
        .with_min_inner_size([760.0, 520.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "yzendris",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, &args)))),
    )
}
