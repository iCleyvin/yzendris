//! Host (Windows server) panel: the screen arrangement, the list of clients
//! and where each one sits, TLS pairing, one-click install, and daemon control.
use std::time::Duration;

use eframe::egui;

use crate::config_model::{self, ClientEntry, ServerConfig};
use crate::daemon;
use crate::monitors::{self, MonitorInfo};
use crate::setup;

/// Where a client sits relative to the host monitors.
#[derive(Clone, PartialEq)]
enum Placement {
    /// Between two monitors (device names); orientation inferred from geometry.
    Between(String, String),
    /// Outer edge of the whole desktop.
    Edge(String),
}

pub struct HostPanel {
    cfg: ServerConfig,
    monitors: Vec<MonitorInfo>,
    trusted: Vec<String>,
    new_fingerprint: String,
    status_msg: String,
    monitor: daemon::DaemonMonitor,
    setup_status: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl HostPanel {
    pub fn new() -> Self {
        Self {
            cfg: config_model::load_server_config(),
            monitors: monitors::enumerate(),
            trusted: config_model::load_trusted_peers(),
            new_fingerprint: String::new(),
            status_msg: String::new(),
            monitor: daemon::DaemonMonitor::new(daemon::Target::Server),
            setup_status: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn save(&mut self, running: bool) {
        match config_model::save_server_config(&self.cfg) {
            Ok(()) => {
                if running {
                    daemon::restart_async(daemon::Target::Server);
                    self.status_msg = "✔ Guardado, reiniciando servidor…".into();
                } else {
                    self.status_msg = "✔ Configuración guardada".into();
                }
            }
            Err(e) => self.status_msg = format!("✘ Error al guardar: {e}"),
        }
    }

    fn any_tls(&self) -> bool {
        self.cfg.clients.iter().any(|c| c.tls)
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
            // ── Estado en vivo (arriba, para que se vea de un vistazo) ───────
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if status.running {
                        let total = self.cfg.clients.len();
                        let connected = (0..total)
                            .filter(|&i| status.connected_mask & (1u64 << i) != 0)
                            .count();
                        ui.colored_label(egui::Color32::GREEN, "●");
                        ui.strong("Servidor en ejecución");
                        ui.label(format!("· {connected}/{total} cliente(s) conectado(s)"));
                    } else {
                        ui.colored_label(egui::Color32::GRAY, "○");
                        ui.strong("Servidor detenido");
                        ui.label("· usa «Iniciar servidor» abajo");
                    }
                });
            });
            ui.add_space(6.0);

            // ── Disposición (pantallas + clientes) ──────────────────────────
            ui.horizontal(|ui| {
                ui.heading("Disposición");
                if ui.small_button("⟳ redetectar pantallas").clicked() {
                    self.monitors = monitors::enumerate();
                }
            });
            ui.label("Gris = pantallas · verde = clientes. Arrastra un cliente a un borde o al hueco entre dos pantallas para mover por dónde se entra a él.");
            arrangement_editor(ui, &self.monitors, &mut self.cfg.clients);
            ui.add_space(8.0);

            // ── Clientes ────────────────────────────────────────────────────
            ui.heading("Clientes");
            ui.label(
                "Cada equipo que recibe el teclado y mouse, y por dónde se entra a él \
                 (entre dos pantallas, o por un borde).",
            );
            ui.add_space(4.0);

            let options = placement_options(&self.monitors);
            let mut remove: Option<usize> = None;
            let n = self.cfg.clients.len();
            for i in 0..n {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    let c = &mut self.cfg.clients[i];
                    ui.horizontal(|ui| {
                        // Per-client connection indicator (from the status file).
                        let connected = status.running && status.connected_mask & (1u64 << i) != 0;
                        let (dot, col, tip) = if connected {
                            ("●", egui::Color32::GREEN, "conectado")
                        } else {
                            ("○", egui::Color32::GRAY, "sin conexión")
                        };
                        ui.colored_label(col, dot).on_hover_text(tip);
                        ui.label("Nombre:");
                        ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(140.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if n > 1 && ui.button("🗑 Quitar").clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                    egui::Grid::new(("client_grid", i)).num_columns(2).spacing([10.0, 5.0]).show(ui, |ui| {
                        ui.label("IP del cliente:");
                        ui.text_edit_singleline(&mut c.addr);
                        ui.end_row();

                        ui.label("Puerto:");
                        ui.add(egui::DragValue::new(&mut c.port).range(1..=65535));
                        ui.end_row();

                        ui.label("TLS:");
                        ui.checkbox(&mut c.tls, "");
                        ui.end_row();

                        ui.label("Posición:");
                        let current = client_placement(c);
                        let current_label = placement_label(&self.monitors, &current);
                        egui::ComboBox::from_id_salt(("placement", i))
                            .selected_text(current_label)
                            .width(300.0)
                            .show_ui(ui, |ui| {
                                for opt in &options {
                                    let label = placement_label(&self.monitors, opt);
                                    let mut sel = current.clone();
                                    if ui.selectable_value(&mut sel, opt.clone(), label).clicked() {
                                        set_placement(c, opt);
                                    }
                                }
                            });
                        ui.end_row();
                    });
                });
                ui.add_space(4.0);
            }
            if let Some(i) = remove {
                self.cfg.clients.remove(i);
            }
            if ui.button("➕ Agregar cliente").clicked() {
                self.cfg.clients.push(ClientEntry::default());
            }

            ui.add_space(8.0);
            ui.checkbox(&mut self.cfg.clipboard, "Compartir portapapeles");

            // ── TLS pairing ─────────────────────────────────────────────────
            if self.any_tls() {
                ui.add_space(10.0);
                ui.heading("Huellas TLS confiables");
                ui.label("Pega la huella que muestra cada cliente (panel Cliente) al activar TLS:");
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
                let mut remove_fp: Option<usize> = None;
                for (i, fp) in self.trusted.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.monospace(shorten(fp, 56));
                        if ui.small_button("🗑").clicked() {
                            remove_fp = Some(i);
                        }
                    });
                }
                if let Some(i) = remove_fp {
                    self.trusted.remove(i);
                    let _ = config_model::save_trusted_peers(&self.trusted);
                }
            }

            // ── Instalación autosuficiente ──────────────────────────────────
            ui.add_space(10.0);
            ui.separator();
            ui.heading("Instalación");
            ui.label(
                "Deja esta PC lista sola: copia el programa, abre el firewall y lo arranca al \
                 iniciar Windows. Pide permiso de administrador una vez.",
            );
            if ui.button("⚙ Instalar y habilitar inicio automático").clicked() {
                let _ = config_model::save_server_config(&self.cfg);
                let status_arc = self.setup_status.clone();
                let port = self.cfg.clients.first().map(|c| c.port).unwrap_or(7547);
                *status_arc.lock().unwrap() = Some("Instalando… (aprueba el aviso de administrador)".into());
                std::thread::spawn(move || {
                    let r = setup::enable(daemon::Target::Server, port);
                    *status_arc.lock().unwrap() =
                        Some(match r { Ok(m) => m, Err(e) => format!("✘ {e}") });
                });
            }
            if let Some(msg) = self.setup_status.lock().unwrap().clone() {
                ui.label(msg);
            }
            ui.add_space(8.0);

            // ── Control ─────────────────────────────────────────────────────
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("💾 Guardar configuración").clicked() {
                    self.save(status.running);
                }
                if status.running {
                    if ui.button("⏹ Detener servidor").clicked() {
                        daemon::stop_async(daemon::Target::Server);
                        self.status_msg = "Deteniendo servidor…".into();
                    }
                } else if ui.button("▶ Iniciar servidor").clicked() {
                    let _ = config_model::save_server_config(&self.cfg);
                    daemon::start_async(daemon::Target::Server);
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

// ─── Placement helpers ───────────────────────────────────────────────────────

fn client_placement(c: &ClientEntry) -> Placement {
    if let Some(b) = c.between.as_ref().filter(|b| b.len() == 2) {
        Placement::Between(b[0].clone(), b[1].clone())
    } else {
        Placement::Edge(c.edge.clone().unwrap_or_else(|| "right".into()))
    }
}

fn set_placement(c: &mut ClientEntry, p: &Placement) {
    match p {
        Placement::Between(a, b) => {
            c.between = Some(vec![a.clone(), b.clone()]);
            c.edge = None;
        }
        Placement::Edge(e) => {
            c.edge = Some(e.clone());
            c.between = None;
        }
    }
}

fn monitor_number(monitors: &[MonitorInfo], device: &str) -> String {
    let want = device.trim().trim_start_matches(r"\\.\").to_uppercase();
    monitors
        .iter()
        .find(|m| {
            let d = m.device.to_uppercase();
            d == want || d == format!("DISPLAY{want}")
        })
        .map(|m| m.number())
        .unwrap_or_else(|| device.to_owned())
}

fn placement_label(monitors: &[MonitorInfo], p: &Placement) -> String {
    match p {
        Placement::Between(a, b) => {
            let na = monitor_number(monitors, a);
            let nb = monitor_number(monitors, b);
            let kind = pair_kind(monitors, a, b);
            format!("Entre pantalla {na} y {nb}{kind}")
        }
        Placement::Edge(e) => match e.as_str() {
            "left" => "Borde izquierdo de todo".into(),
            "top" => "Arriba de todo".into(),
            "bottom" => "Abajo de todo".into(),
            _ => "Borde derecho de todo".into(),
        },
    }
}

/// " (lado a lado)" / " (apiladas)" suffix for a between-pair, or "" if unknown.
fn pair_kind(monitors: &[MonitorInfo], a: &str, b: &str) -> String {
    let find = |name: &str| {
        let want = name.trim().trim_start_matches(r"\\.\").to_uppercase();
        monitors.iter().find(|m| {
            let d = m.device.to_uppercase();
            d == want || d == format!("DISPLAY{want}")
        })
    };
    match (find(a), find(b)) {
        (Some(x), Some(y)) if pair_is_stacked(x, y) => " (apiladas)".into(),
        (Some(_), Some(_)) => " (lado a lado)".into(),
        _ => String::new(),
    }
}

/// All placement options: every adjacent monitor pair + the four outer edges.
fn placement_options(monitors: &[MonitorInfo]) -> Vec<Placement> {
    let mut out = Vec::new();
    for (i, j, _) in adjacent_pairs(monitors) {
        out.push(Placement::Between(
            monitors[i].device.clone(),
            monitors[j].device.clone(),
        ));
    }
    for e in ["right", "left", "top", "bottom"] {
        out.push(Placement::Edge(e.into()));
    }
    out
}

fn pair_is_stacked(a: &MonitorInfo, b: &MonitorInfo) -> bool {
    let y_ov = (a.top.max(b.top) < a.bottom.min(b.bottom)) as i32
        * (a.bottom.min(b.bottom) - a.top.max(b.top));
    let x_ov = (a.left.max(b.left) < a.right.min(b.right)) as i32
        * (a.right.min(b.right) - a.left.max(b.left));
    x_ov > y_ov
}

fn adjacent_pairs(monitors: &[MonitorInfo]) -> Vec<(usize, usize, &'static str)> {
    let mut out = Vec::new();
    for i in 0..monitors.len() {
        for j in (i + 1)..monitors.len() {
            let a = &monitors[i];
            let b = &monitors[j];
            let y_ov = a.top.max(b.top) < a.bottom.min(b.bottom);
            let x_ov = a.left.max(b.left) < a.right.min(b.right);
            let h_adj = y_ov && ((a.right - b.left).abs() <= 2 || (b.right - a.left).abs() <= 2);
            let v_adj = x_ov && ((a.bottom - b.top).abs() <= 2 || (b.bottom - a.top).abs() <= 2);
            if v_adj && pair_is_stacked(a, b) {
                out.push((i, j, "apiladas"));
            } else if h_adj {
                out.push((i, j, "lado a lado"));
            }
        }
    }
    out
}

// ─── Arrangement diagram ─────────────────────────────────────────────────────

/// Are two placements the same spot? (Between is order-insensitive.)
fn placement_eq(a: &Placement, b: &Placement) -> bool {
    let norm = |s: &str| s.trim().trim_start_matches(r"\\.\").to_uppercase();
    match (a, b) {
        (Placement::Edge(x), Placement::Edge(y)) => x == y,
        (Placement::Between(a1, a2), Placement::Between(b1, b2)) => {
            let (na1, na2, nb1, nb2) = (norm(a1), norm(a2), norm(b1), norm(b2));
            (na1 == nb1 && na2 == nb2) || (na1 == nb2 && na2 == nb1)
        }
        _ => false,
    }
}

/// Interactive arrangement: drag each client (green) to a drop slot — an outer
/// edge or the gap between two adjacent monitors — to set how the cursor
/// crosses into it. Monitors stay where Windows reports them.
fn arrangement_editor(ui: &mut egui::Ui, monitors: &[MonitorInfo], clients: &mut [ClientEntry]) {
    if monitors.is_empty() {
        ui.label("(no se detectaron pantallas — disponible solo en Windows)");
        return;
    }

    let avg_w = monitors.iter().map(|m| m.width()).sum::<i32>() as f32 / monitors.len() as f32;
    let avg_h = monitors.iter().map(|m| m.height()).sum::<i32>() as f32 / monitors.len() as f32;
    let slot_w = (avg_w * 0.7).max(350.0);
    let slot_h = (avg_h * 0.7).max(260.0);
    let pad = (avg_w * 0.06).max(40.0);
    let box_w = slot_w;
    let box_h = slot_h;

    // Monitor rects (mutable). We "explode" the arrangement: insert a real gap
    // at every adjacent boundary so each client sits cleanly in a gap instead
    // of overlapping the monitors.
    let mut mon: Vec<egui::Rect> = monitors
        .iter()
        .map(|m| {
            egui::Rect::from_min_size(
                egui::pos2(m.left as f32, m.top as f32),
                egui::vec2(m.width() as f32, m.height() as f32),
            )
        })
        .collect();

    // Drop slots: (placement, virtual center).
    let mut cands: Vec<(Placement, egui::Pos2)> = Vec::new();
    let pairs = adjacent_pairs(monitors);

    // Side-by-side gaps (vertical boundaries), left to right.
    let mut sbs: Vec<(usize, usize)> = pairs
        .iter()
        .filter(|(i, j, _)| !pair_is_stacked(&monitors[*i], &monitors[*j]))
        .map(|(i, j, _)| (*i, *j))
        .collect();
    sbs.sort_by_key(|(i, j)| monitors[*i].left.min(monitors[*j].left));
    let gap_w = slot_w + 2.0 * pad;
    for (i, j) in sbs {
        let (l, r) = if monitors[i].left <= monitors[j].left { (i, j) } else { (j, i) };
        let boundary_x = mon[l].max.x;
        let band_cy = (mon[l].min.y.max(mon[r].min.y) + mon[l].max.y.min(mon[r].max.y)) / 2.0;
        for m in mon.iter_mut() {
            if m.min.x >= boundary_x - 1.0 {
                *m = m.translate(egui::vec2(gap_w, 0.0));
            }
        }
        for (_, p) in cands.iter_mut() {
            if p.x >= boundary_x - 1.0 {
                p.x += gap_w;
            }
        }
        cands.push((
            Placement::Between(monitors[i].device.clone(), monitors[j].device.clone()),
            egui::pos2(boundary_x + gap_w / 2.0, band_cy),
        ));
    }

    // Stacked gaps (horizontal boundaries), top to bottom.
    let mut stk: Vec<(usize, usize)> = pairs
        .iter()
        .filter(|(i, j, _)| pair_is_stacked(&monitors[*i], &monitors[*j]))
        .map(|(i, j, _)| (*i, *j))
        .collect();
    stk.sort_by_key(|(i, j)| monitors[*i].top.min(monitors[*j].top));
    let gap_h = slot_h + 2.0 * pad;
    for (i, j) in stk {
        let (t, bo) = if monitors[i].top <= monitors[j].top { (i, j) } else { (j, i) };
        let boundary_y = mon[t].max.y;
        let band_cx = (mon[t].min.x.max(mon[bo].min.x) + mon[t].max.x.min(mon[bo].max.x)) / 2.0;
        for m in mon.iter_mut() {
            if m.min.y >= boundary_y - 1.0 {
                *m = m.translate(egui::vec2(0.0, gap_h));
            }
        }
        for (_, p) in cands.iter_mut() {
            if p.y >= boundary_y - 1.0 {
                p.y += gap_h;
            }
        }
        cands.push((
            Placement::Between(monitors[i].device.clone(), monitors[j].device.clone()),
            egui::pos2(band_cx, boundary_y + gap_h / 2.0),
        ));
    }

    // Edge slots, outside the exploded bounding box.
    let bbox = mon.iter().copied().reduce(|a, b| a.union(b)).unwrap();
    let c = bbox.center();
    let em = slot_w.max(slot_h) * 0.6 + pad;
    cands.push((Placement::Edge("left".into()), egui::pos2(bbox.min.x - em, c.y)));
    cands.push((Placement::Edge("right".into()), egui::pos2(bbox.max.x + em, c.y)));
    cands.push((Placement::Edge("top".into()), egui::pos2(c.x, bbox.min.y - em)));
    cands.push((Placement::Edge("bottom".into()), egui::pos2(c.x, bbox.max.y + em)));

    // Virtual area to fit, with room for the edge slot boxes.
    let total = bbox.expand2(egui::vec2(em + slot_w * 0.6, em + slot_h * 0.6));
    let canvas_size = egui::vec2(ui.available_width().min(580.0), 240.0);
    let (resp, painter) = ui.allocate_painter(canvas_size, egui::Sense::hover());
    let canvas = resp.rect.shrink(8.0);
    let scale = (canvas.width() / total.width())
        .min(canvas.height() / total.height())
        .min(1.0);
    let offset = canvas.center() - total.center().to_vec2() * scale;
    let to_screen = |p: egui::Pos2| offset + p.to_vec2() * scale;
    let to_screen_rect = |r: egui::Rect| {
        egui::Rect::from_min_max(offset + r.min.to_vec2() * scale, offset + r.max.to_vec2() * scale)
    };
    let to_virtual = |sp: egui::Pos2| ((sp - offset) / scale).to_pos2();

    let nearest = |pv: egui::Pos2| -> usize {
        let mut best = 0;
        let mut bd = f32::MAX;
        for (k, (_, pos)) in cands.iter().enumerate() {
            let d = pos.distance_sq(pv);
            if d < bd {
                bd = d;
                best = k;
            }
        }
        best
    };

    // Monitors (gray).
    for (i, r) in mon.iter().enumerate() {
        let sr = to_screen_rect(*r);
        painter.rect_filled(sr, 4.0, egui::Color32::from_gray(45));
        painter.rect_stroke(sr, 4.0, egui::Stroke::new(1.5, egui::Color32::from_gray(140)), egui::StrokeKind::Inside);
        painter.text(
            sr.center(),
            egui::Align2::CENTER_CENTER,
            format!("{}\n{}×{}", monitors[i].number(), monitors[i].width(), monitors[i].height()),
            egui::FontId::proportional(12.0),
            egui::Color32::WHITE,
        );
    }
    // Candidate slot markers (faint).
    for (_, pos) in &cands {
        painter.circle_filled(to_screen(*pos), 3.0, egui::Color32::from_gray(90));
    }

    let box_screen = egui::vec2(box_w * scale, box_h * scale);

    // Each client: a draggable box snapping to the nearest slot on release.
    for (ci, client) in clients.iter_mut().enumerate() {
        let cur = client_placement(client);
        let slot_pos = cands
            .iter()
            .find(|(p, _)| placement_eq(p, &cur))
            .map(|(_, p)| *p)
            .unwrap_or(cands[0].1);
        let center = to_screen(slot_pos);
        let rect = egui::Rect::from_center_size(center, box_screen);
        let id = ui.id().with(("yz_client_box", ci));
        let r = ui.interact(rect, id, egui::Sense::drag());

        let mut draw_center = center;
        if r.dragged() {
            if let Some(pp) = r.interact_pointer_pos() {
                draw_center = pp;
                let n = nearest(to_virtual(pp));
                painter.circle_stroke(to_screen(cands[n].1), 10.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(110, 230, 150)));
            }
        }
        if r.drag_stopped() {
            if let Some(pp) = r.interact_pointer_pos() {
                let n = nearest(to_virtual(pp));
                set_placement(client, &cands[n].0);
            }
        }

        let br = egui::Rect::from_center_size(draw_center, box_screen);
        let (fill, stroke) = if r.dragged() {
            (egui::Color32::from_rgb(45, 140, 85), egui::Color32::from_rgb(150, 250, 190))
        } else {
            (egui::Color32::from_rgb(35, 110, 65), egui::Color32::from_rgb(110, 230, 150))
        };
        painter.rect_filled(br, 4.0, fill);
        painter.rect_stroke(br, 4.0, egui::Stroke::new(1.5, stroke), egui::StrokeKind::Inside);
        painter.text(
            br.center(),
            egui::Align2::CENTER_CENTER,
            &client.name,
            egui::FontId::proportional(12.0),
            egui::Color32::WHITE,
        );
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
}


fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}
