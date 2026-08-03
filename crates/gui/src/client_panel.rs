//! Client (Linux laptop) configuration panel: listening settings, TLS
//! fingerprint display, daemon control and log.
use std::time::Duration;

use eframe::egui;

use crate::config_model::{self, ClientConfig};
use crate::daemon;
use crate::setup;

pub struct ClientPanel {
    cfg: ClientConfig,
    fingerprint: Option<String>,
    /// Host fingerprints we accept — pinning runs both ways.
    trusted: Vec<String>,
    new_fingerprint: String,
    status_msg: String,
    monitor: daemon::DaemonMonitor,
    setup_status: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl ClientPanel {
    pub fn new() -> Self {
        Self {
            cfg: config_model::load_client_config(),
            fingerprint: config_model::own_fingerprint(),
            trusted: config_model::load_trusted_peers(),
            new_fingerprint: String::new(),
            status_msg: String::new(),
            monitor: daemon::DaemonMonitor::new(daemon::Target::Client),
            setup_status: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn save(&mut self, running: bool) {
        match config_model::save_client_config(&self.cfg) {
            Ok(()) => {
                if running {
                    daemon::restart_async(daemon::Target::Client);
                    self.status_msg = "✔ Guardado, reiniciando cliente…".into();
                } else {
                    self.status_msg = "✔ Configuración guardada".into();
                }
            }
            Err(e) => self.status_msg = format!("✘ Error al guardar: {e}"),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let status = self.monitor.snapshot();
        if self.fingerprint.is_none() {
            self.fingerprint = config_model::own_fingerprint();
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Recepción");
            egui::Grid::new("client_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Puerto de escucha:");
                ui.add(egui::DragValue::new(&mut self.cfg.port).range(1..=65535));
                ui.end_row();

                ui.label("Dirección de enlace:");
                ui.text_edit_singleline(&mut self.cfg.bind_addr);
                ui.end_row();

                ui.label("Layout de teclado:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.kb_layout)
                        .hint_text("vacío = autodetectar (es, us, …)"),
                );
                ui.end_row();

                ui.label("Portapapeles compartido:");
                ui.checkbox(&mut self.cfg.clipboard, "");
                ui.end_row();

                if self.cfg.clipboard {
                    ui.label("Tamaño máx. de archivo:");
                    ui.add(
                        egui::DragValue::new(&mut self.cfg.max_file_mb)
                            .range(1..=100_000)
                            .suffix(" MB"),
                    );
                    ui.end_row();
                }

                ui.label("TLS (cifrado + pairing):");
                ui.checkbox(&mut self.cfg.tls, "");
                ui.end_row();
            });

            if self.cfg.tls {
                ui.add_space(10.0);
                ui.heading("Huella TLS de esta máquina");
                match &self.fingerprint {
                    Some(fp) => {
                        ui.label("Agrega esta huella en el Host (panel \"Huellas TLS confiables\"):");
                        ui.horizontal(|ui| {
                            ui.monospace(fp);
                            if ui.small_button("📋 Copiar").clicked() {
                                ui.ctx().copy_text(fp.clone());
                            }
                        });
                    }
                    None => {
                        ui.label(
                            "El certificado aún no existe — inicia el cliente una vez con TLS \
                             activado y volverá a aparecer aquí.",
                        );
                    }
                }

                ui.add_space(10.0);
                ui.heading("Huellas TLS confiables");
                ui.label(
                    "Pega aquí la huella del Host (panel Host → \"Huella TLS de este host\"). \
                     Sin ella se rechaza cualquier conexión: es lo que impide que otra máquina \
                     de la red se conecte y escriba en tu equipo.",
                );
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
                            daemon::restart_async(daemon::Target::Client);
                        }
                    }
                });
                if self.trusted.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(240, 180, 80),
                        "⚠ Ninguna huella confiable — el cliente rechazará toda conexión.",
                    );
                }
                let mut remove_fp: Option<usize> = None;
                for (i, fp) in self.trusted.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.monospace(fp);
                        if ui.small_button("🗑").clicked() {
                            remove_fp = Some(i);
                        }
                    });
                }
                if let Some(i) = remove_fp {
                    self.trusted.remove(i);
                    let _ = config_model::save_trusted_peers(&self.trusted);
                    daemon::restart_async(daemon::Target::Client);
                }
            }

            // ── Instalación autosuficiente ──────────────────────────────────
            ui.add_space(10.0);
            ui.separator();
            ui.heading("Instalación");
            ui.label(
                "Deja esta máquina lista para recibir: copia el programa, abre el firewall \
                 y lo arranca al iniciar sesión. En Windows pide permiso de administrador una vez.",
            );
            if ui.button("⚙ Instalar y habilitar inicio automático").clicked() {
                let _ = config_model::save_client_config(&self.cfg);
                let status_arc = self.setup_status.clone();
                let port = self.cfg.port;
                *status_arc.lock().unwrap() = Some("Instalando…".into());
                std::thread::spawn(move || {
                    let r = setup::enable(daemon::Target::Client, port);
                    *status_arc.lock().unwrap() =
                        Some(match r { Ok(m) => m, Err(e) => format!("✘ {e}") });
                });
            }
            if let Some(msg) = self.setup_status.lock().unwrap().clone() {
                ui.label(msg);
            }

            // ── Control ─────────────────────────────────────────────────────
            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("💾 Guardar configuración").clicked() {
                    self.save(status.running);
                }
                if status.running {
                    if ui.button("⏹ Detener cliente").clicked() {
                        daemon::stop_async(daemon::Target::Client);
                        self.status_msg = "Deteniendo cliente…".into();
                    }
                } else if ui.button("▶ Iniciar cliente").clicked() {
                    let _ = config_model::save_client_config(&self.cfg);
                    daemon::start_async(daemon::Target::Client);
                    self.status_msg = "Iniciando cliente…".into();
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
            ui.collapsing("Registro del cliente", |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("client_log")
                    .max_height(160.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.monospace(&status.log);
                    });
            });
        });
    }
}

