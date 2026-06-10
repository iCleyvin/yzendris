//! yzendris-gui — graphical configurator for both sides of Yzendris KVM.
//!
//! On first run it asks whether this machine is the HOST (shares its keyboard
//! and mouse — the Windows PC) or a CLIENT (receives them — the Linux laptop),
//! then shows the matching configuration panel.

// No console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod client_panel;
mod config_model;
mod daemon;
mod host_panel;
mod monitors;

use eframe::egui;

use config_model::Role;

enum Screen {
    RoleSelect,
    Host(host_panel::HostPanel),
    Client(client_panel::ClientPanel),
}

struct App {
    screen: Screen,
}

impl App {
    fn new() -> Self {
        let screen = match config_model::load_role() {
            Some(Role::Host) => Screen::Host(host_panel::HostPanel::new()),
            Some(Role::Client) => Screen::Client(client_panel::ClientPanel::new()),
            None => Screen::RoleSelect,
        };
        Self { screen }
    }

    fn select_role(&mut self, role: Role) {
        let _ = config_model::save_role(role);
        self.screen = match role {
            Role::Host => Screen::Host(host_panel::HostPanel::new()),
            Role::Client => Screen::Client(client_panel::ClientPanel::new()),
        };
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Yzendris KVM");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match &self.screen {
                        Screen::Host(_) => {
                            ui.label("modo: Host");
                            if ui.small_button("cambiar rol").clicked() {
                                self.screen = Screen::RoleSelect;
                            }
                        }
                        Screen::Client(_) => {
                            ui.label("modo: Cliente");
                            if ui.small_button("cambiar rol").clicked() {
                                self.screen = Screen::RoleSelect;
                            }
                        }
                        Screen::RoleSelect => {}
                    }
                });
            });
        });

        egui::CentralPanel::default().show_inside(ui, |ui| match &mut self.screen {
            Screen::RoleSelect => {
                let mut selected: Option<Role> = None;
                role_select_ui(ui, &mut selected);
                if let Some(role) = selected {
                    self.select_role(role);
                }
            }
            Screen::Host(panel) => panel.ui(ui),
            Screen::Client(panel) => panel.ui(ui),
        });
    }
}

fn role_select_ui(ui: &mut egui::Ui, selected: &mut Option<Role>) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        ui.heading("¿Cómo se usará esta máquina?");
        ui.add_space(6.0);
        ui.label("Puedes cambiarlo después con el botón \"cambiar rol\".");
        ui.add_space(24.0);

        let host_available = cfg!(windows);
        let client_available = cfg!(target_os = "linux");
        let big = egui::vec2(360.0, 64.0);

        let host_btn = egui::Button::new(
            egui::RichText::new("🖥  Host — esta PC comparte su teclado y mouse").size(15.0),
        );
        if ui.add_sized(big, host_btn).clicked() {
            *selected = Some(Role::Host);
        }
        if !host_available {
            ui.small("(el host captura con hooks de Windows — esta máquina no es Windows)");
        }

        ui.add_space(14.0);

        let client_btn = egui::Button::new(
            egui::RichText::new("💻  Cliente — esta máquina recibe el teclado y mouse").size(15.0),
        );
        if ui.add_sized(big, client_btn).clicked() {
            *selected = Some(Role::Client);
        }
        if !client_available {
            ui.small("(el cliente inyecta vía /dev/uinput — esta máquina no es Linux)");
        }

        ui.add_space(30.0);
        ui.separator();
        ui.add_space(8.0);
        ui.small("Host = donde están el teclado y mouse físicos (PC Windows).");
        ui.small("Cliente = la máquina que los recibe por red (laptop Linux con Hyprland).");
    });
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 640.0])
            .with_min_inner_size([520.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Yzendris KVM",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}
