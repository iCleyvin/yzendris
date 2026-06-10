//! Host (Windows server) configuration panel: connection settings, screen
//! arrangement with laptop placement, TLS pairing, daemon control and log.
use std::time::Duration;

use eframe::egui;

use crate::config_model::{
    self, LayoutConfig, ServerConfig,
};
use crate::daemon;
use crate::monitors::{self, MonitorInfo};

/// Where the laptop sits relative to this PC's monitors.
#[derive(Debug, Clone, PartialEq)]
pub enum LaptopPos {
    /// Outer edge of the whole desktop: "right" | "left" | "top" | "bottom".
    Edge(String),
    /// Between two adjacent monitors (indices into the sorted monitor list).
    Between(usize, usize),
}

pub struct HostPanel {
    cfg: ServerConfig,
    monitors: Vec<MonitorInfo>,
    laptop_pos: LaptopPos,
    trusted: Vec<String>,
    new_fingerprint: String,
    status_msg: String,
    monitor: daemon::DaemonMonitor,
}

impl HostPanel {
    pub fn new() -> Self {
        let cfg = config_model::load_server_config();
        let monitors = monitors::enumerate();
        let laptop_pos = laptop_pos_from_config(&cfg, &monitors);
        Self {
            cfg,
            monitors,
            laptop_pos,
            trusted: config_model::load_trusted_peers(),
            new_fingerprint: String::new(),
            status_msg: String::new(),
            monitor: daemon::DaemonMonitor::new(),
        }
    }

    /// Apply the chosen laptop position to the config struct.
    fn sync_layout_into_cfg(&mut self) {
        match &self.laptop_pos {
            LaptopPos::Edge(edge) => {
                self.cfg.edge = edge.clone();
                self.cfg.layout = None;
            }
            LaptopPos::Between(i, j) => {
                let (Some(a), Some(b)) = (self.monitors.get(*i), self.monitors.get(*j)) else {
                    return;
                };
                self.cfg.layout = Some(LayoutConfig {
                    mode: "between".into(),
                    monitor_left: a.device.clone(),
                    monitor_right: b.device.clone(),
                });
            }
        }
    }

    fn save(&mut self, running: bool) {
        self.sync_layout_into_cfg();
        match config_model::save_server_config(&self.cfg) {
            Ok(()) => {
                if running {
                    // Restart happens on a background thread — UI stays responsive.
                    daemon::restart_async();
                    self.status_msg = "✔ Guardado, reiniciando servidor…".into();
                } else {
                    self.status_msg = "✔ Configuración guardada".into();
                }
            }
            Err(e) => self.status_msg = format!("✘ Error al guardar: {e}"),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let status = self.monitor.snapshot();
        ui.ctx().request_repaint_after(Duration::from_secs(1));

        if !cfg!(windows) {
            ui.colored_label(
                egui::Color32::YELLOW,
                "⚠ El modo Host requiere Windows (la captura usa hooks de Win32).",
            );
            ui.separator();
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            // ── Conexión ────────────────────────────────────────────────────
            ui.heading("Conexión");
            egui::Grid::new("conn_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("IP del cliente (laptop):");
                ui.text_edit_singleline(&mut self.cfg.client_addr);
                ui.end_row();

                ui.label("Puerto:");
                ui.add(egui::DragValue::new(&mut self.cfg.port).range(1..=65535));
                ui.end_row();

                ui.label("Portapapeles compartido:");
                ui.checkbox(&mut self.cfg.clipboard, "");
                ui.end_row();

                ui.label("TLS (cifrado + pairing):");
                ui.checkbox(&mut self.cfg.tls, "");
                ui.end_row();
            });

            ui.add_space(10.0);

            // ── Disposición de pantallas ────────────────────────────────────
            ui.horizontal(|ui| {
                ui.heading("Posición del laptop");
                if ui.small_button("⟳ redetectar pantallas").clicked() {
                    self.monitors = monitors::enumerate();
                    self.laptop_pos = laptop_pos_from_config(&self.cfg, &self.monitors);
                }
            });
            ui.label("¿Dónde está físicamente el laptop respecto a las pantallas de esta PC?");
            ui.add_space(4.0);

            // Options: between each adjacent pair + the four outer edges.
            for i in 0..self.monitors.len().saturating_sub(1) {
                let label = format!(
                    "Entre la pantalla {} y la pantalla {}",
                    self.monitors[i].number(),
                    self.monitors[i + 1].number()
                );
                ui.radio_value(&mut self.laptop_pos, LaptopPos::Between(i, i + 1), label);
            }
            for (edge, label) in [
                ("right", "Al borde derecho de todo"),
                ("left", "Al borde izquierdo de todo"),
                ("top", "Arriba"),
                ("bottom", "Abajo"),
            ] {
                ui.radio_value(&mut self.laptop_pos, LaptopPos::Edge(edge.into()), label);
            }

            ui.add_space(6.0);
            draw_arrangement(ui, &self.monitors, &self.laptop_pos);
            ui.add_space(10.0);

            // ── TLS pairing ─────────────────────────────────────────────────
            if self.cfg.tls {
                ui.heading("Huellas TLS confiables");
                ui.label("Pega aquí la huella que imprime el cliente Linux al iniciar con TLS:");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_fingerprint)
                            .hint_text("sha256:aa:bb:cc:…")
                            .desired_width(420.0),
                    );
                    if ui.button("Agregar").clicked() {
                        let fp = self.new_fingerprint.trim().to_owned();
                        if !fp.is_empty() && !self.trusted.contains(&fp) {
                            self.trusted.push(fp);
                            let _ = config_model::save_trusted_peers(&self.trusted);
                            self.new_fingerprint.clear();
                        }
                    }
                });
                let mut remove: Option<usize> = None;
                for (i, fp) in self.trusted.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.monospace(shorten(fp, 56));
                        if ui.small_button("🗑").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.trusted.remove(i);
                    let _ = config_model::save_trusted_peers(&self.trusted);
                }
                ui.add_space(10.0);
            }

            // ── Control ─────────────────────────────────────────────────────
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("💾 Guardar configuración").clicked() {
                    self.save(status.running);
                }
                if status.running {
                    if ui.button("⏹ Detener servidor").clicked() {
                        daemon::stop_async();
                        self.status_msg = "Deteniendo servidor…".into();
                    }
                } else if ui.button("▶ Iniciar servidor").clicked() {
                    self.sync_layout_into_cfg();
                    let _ = config_model::save_server_config(&self.cfg);
                    daemon::start_async();
                    self.status_msg = "Iniciando servidor…".into();
                }

                let (dot, text) = if status.running {
                    (egui::Color32::GREEN, "en ejecución")
                } else {
                    (egui::Color32::GRAY, "detenido")
                };
                ui.colored_label(dot, "●");
                ui.label(text);
            });
            if !self.status_msg.is_empty() {
                ui.label(&self.status_msg);
            }

            // ── Log ─────────────────────────────────────────────────────────
            ui.add_space(8.0);
            ui.collapsing("Registro del servidor", |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("server_log")
                    .max_height(160.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.monospace(&status.log);
                    });
            });
        });
    }
}

/// Reverse-map the saved config to a UI position choice.
fn laptop_pos_from_config(cfg: &ServerConfig, monitors: &[MonitorInfo]) -> LaptopPos {
    if let Some(layout) = cfg.layout.as_ref().filter(|l| l.mode == "between") {
        let find = |name: &str| {
            let wanted = name.trim().trim_start_matches(r"\\.\").to_uppercase();
            monitors.iter().position(|m| {
                let dev = m.device.to_uppercase();
                dev == wanted || dev == format!("DISPLAY{wanted}")
            })
        };
        if let (Some(i), Some(j)) = (find(&layout.monitor_left), find(&layout.monitor_right)) {
            return LaptopPos::Between(i.min(j), i.max(j));
        }
        // Names not found (monitors changed) — default to first pair.
        if monitors.len() >= 2 {
            return LaptopPos::Between(0, 1);
        }
    }
    LaptopPos::Edge(cfg.edge.clone())
}

/// Paint a scaled diagram of the monitor arrangement with the laptop slot.
fn draw_arrangement(ui: &mut egui::Ui, monitors: &[MonitorInfo], pos: &LaptopPos) {
    if monitors.is_empty() {
        ui.label("(no se detectaron pantallas — disponible solo en Windows)");
        return;
    }

    // Virtual-space rects: monitors plus a representative laptop rect.
    let laptop_w = 1600.0_f32;
    let laptop_h = 1000.0_f32;
    let gap = 60.0_f32;

    // Base monitor rects in virtual coords.
    let mut rects: Vec<(egui::Rect, String, bool)> = monitors
        .iter()
        .map(|m| {
            (
                egui::Rect::from_min_size(
                    egui::pos2(m.left as f32, m.top as f32),
                    egui::vec2(m.width() as f32, m.height() as f32),
                ),
                format!("{}\n{}×{}", m.number(), m.width(), m.height()),
                false,
            )
        })
        .collect();

    // Insert the laptop and shift monitors for the "between" visualization.
    let bbox = rects
        .iter()
        .map(|(r, _, _)| *r)
        .reduce(|a, b| a.union(b))
        .unwrap();

    match pos {
        LaptopPos::Between(i, _) if *i < monitors.len() => {
            let boundary = monitors[*i].right as f32;
            for (r, _, _) in rects.iter_mut() {
                if r.min.x >= boundary - 1.0 {
                    *r = r.translate(egui::vec2(laptop_w + 2.0 * gap, 0.0));
                }
            }
            let cy = (monitors[*i].top + monitors[*i].bottom) as f32 / 2.0;
            rects.push((
                egui::Rect::from_center_size(
                    egui::pos2(boundary + gap + laptop_w / 2.0, cy),
                    egui::vec2(laptop_w, laptop_h),
                ),
                "Laptop".into(),
                true,
            ));
        }
        LaptopPos::Edge(edge) => {
            let center = bbox.center();
            let lap_rect = match edge.as_str() {
                "left" => egui::Rect::from_center_size(
                    egui::pos2(bbox.min.x - gap - laptop_w / 2.0, center.y),
                    egui::vec2(laptop_w, laptop_h),
                ),
                "top" => egui::Rect::from_center_size(
                    egui::pos2(center.x, bbox.min.y - gap - laptop_h / 2.0),
                    egui::vec2(laptop_w, laptop_h),
                ),
                "bottom" => egui::Rect::from_center_size(
                    egui::pos2(center.x, bbox.max.y + gap + laptop_h / 2.0),
                    egui::vec2(laptop_w, laptop_h),
                ),
                _ => egui::Rect::from_center_size(
                    egui::pos2(bbox.max.x + gap + laptop_w / 2.0, center.y),
                    egui::vec2(laptop_w, laptop_h),
                ),
            };
            rects.push((lap_rect, "Laptop".into(), true));
        }
        _ => {}
    }

    // Fit everything into the canvas.
    let total = rects
        .iter()
        .map(|(r, _, _)| *r)
        .reduce(|a, b| a.union(b))
        .unwrap();
    let canvas_size = egui::vec2(ui.available_width().min(560.0), 170.0);
    let (resp, painter) = ui.allocate_painter(canvas_size, egui::Sense::hover());
    let canvas = resp.rect.shrink(8.0);
    let scale = (canvas.width() / total.width())
        .min(canvas.height() / total.height())
        .min(1.0);
    let offset = canvas.center() - total.center().to_vec2() * scale;

    let to_screen = |r: &egui::Rect| {
        egui::Rect::from_min_max(
            offset + r.min.to_vec2() * scale,
            offset + r.max.to_vec2() * scale,
        )
    };

    for (r, label, is_laptop) in &rects {
        let sr = to_screen(r);
        let (fill, stroke_col) = if *is_laptop {
            (
                egui::Color32::from_rgb(35, 110, 65),
                egui::Color32::from_rgb(110, 230, 150),
            )
        } else {
            (
                egui::Color32::from_gray(45),
                egui::Color32::from_gray(140),
            )
        };
        painter.rect_filled(sr, 4.0, fill);
        painter.rect_stroke(
            sr,
            4.0,
            egui::Stroke::new(1.5, stroke_col),
            egui::StrokeKind::Inside,
        );
        painter.text(
            sr.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            egui::Color32::WHITE,
        );
    }
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        // Take by chars, not bytes, so we never slice through a UTF-8 boundary.
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}
