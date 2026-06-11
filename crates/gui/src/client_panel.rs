//! Client (Linux laptop) configuration panel: listening settings, TLS
//! fingerprint display, daemon control and log.
use std::time::Duration;

use eframe::egui;

use crate::config_model::{self, ClientConfig};
use crate::daemon;

pub struct ClientPanel {
    cfg: ClientConfig,
    fingerprint: Option<String>,
    status_msg: String,
    monitor: daemon::DaemonMonitor,
}

impl ClientPanel {
    pub fn new() -> Self {
        Self {
            cfg: config_model::load_client_config(),
            fingerprint: read_fingerprint(),
            status_msg: String::new(),
            monitor: daemon::DaemonMonitor::new(daemon::Target::Client),
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
            self.fingerprint = read_fingerprint();
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

/// SHA-256 fingerprint of the client certificate, matching the format the
/// client daemon prints (`sha256:aa:bb:…`). Works on both Linux and Windows —
/// the client generates `cert.pem` in the config dir on either OS.
fn read_fingerprint() -> Option<String> {
    use sha2::{Digest, Sha256};
    let pem = std::fs::read(config_model::config_dir().join("cert.pem")).ok()?;
    let cert = rustls_pemfile::certs(&mut pem.as_slice()).next()?.ok()?;
    let hash = Sha256::digest(cert.as_ref());
    let hex: Vec<String> = hash.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("sha256:{}", hex.join(":")))
}
