//! The screens of the app.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Layout, RichText, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};
use yzendris_core::config::{Config, Role};
use yzendris_core::ipc::{Link, Notice, NoticeCode, Status};
use yzendris_core::proto::Os;

use crate::backend::Cmd;
use crate::canvas;
use crate::i18n::{self, tr, Lang};
use crate::theme::{self, Palette};
use crate::widgets::{self, Kind};
use crate::App;

fn os_name(os: Option<Os>) -> &'static str {
    match os {
        Some(Os::Linux) => "Linux",
        Some(Os::Windows) => "Windows",
        _ => "",
    }
}

/// Keep content readable on wide windows: a centred column.
fn column(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    let side = ((ui.available_width() - width) / 2.0).max(0.0);
    ui.horizontal_top(|ui| {
        ui.add_space(side);
        ui.vertical(|ui| {
            ui.set_width(width.min(ui.available_width()));
            add(ui);
        });
    });
}

// ── First run ───────────────────────────────────────────────────────────────

pub fn welcome(app: &mut App, ui: &mut Ui, p: &Palette) {
    ui.add_space((ui.available_height() * 0.10).max(12.0));
    column(ui, 640.0, |ui| {
        ui.label(
            RichText::new(tr(
                "Welcome to Yzendris",
                "Te damos la bienvenida a Yzendris",
            ))
            .font(theme::semibold(28.0)),
        );
        ui.add_space(-2.0);
        widgets::muted(
            ui,
            p,
            tr(
                "One keyboard and mouse for all your computers. What is this one?",
                "Un solo teclado y ratón para todos tus equipos. ¿Cuál es este?",
            ),
        );
        ui.add_space(18.0);
        let gap = 14.0;
        let width = (ui.available_width() - gap) / 2.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let host = role_card(
                ui,
                p,
                width,
                true,
                tr("This one has the keyboard and mouse", "Este tiene el teclado y el ratón"),
                tr(
                    "It shares them with your other computers. Choose this on your main desk machine.",
                    "Los comparte con tus otros equipos. Elígelo en tu equipo principal.",
                ),
            );
            if host.clicked() {
                app.backend.edit(|c| c.role = Some(Role::Host));
            }
            let client = role_card(
                ui,
                p,
                width,
                false,
                tr("This one is controlled from another", "Este se controla desde otro"),
                tr(
                    "It receives the keyboard and mouse of your main computer when the cursor comes over.",
                    "Recibe el teclado y el ratón de tu equipo principal cuando el cursor pasa a él.",
                ),
            );
            if client.clicked() {
                app.backend.edit(|c| c.role = Some(Role::Client));
            }
        });
        ui.add_space(14.0);
        widgets::muted(
            ui,
            p,
            tr(
                "You can change this later in Settings.",
                "Puedes cambiarlo después en Ajustes.",
            ),
        );
    });
}

fn role_card(
    ui: &mut Ui,
    p: &Palette,
    width: f32,
    host: bool,
    title: &str,
    body: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 216.0), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(
        response.id,
        response.hovered() || response.has_focus(),
        0.15,
    );
    let painter = ui.painter_at(rect);
    let stroke = Stroke::new(
        1.0 + hover * 0.6,
        if hover > 0.0 {
            p.accent.gamma_multiply(0.4 + 0.6 * hover)
        } else {
            p.border
        },
    );
    painter.rect(
        rect,
        CornerRadius::same(16),
        p.surface,
        stroke,
        StrokeKind::Inside,
    );

    // A tiny diagram: two screens, with the cursor on the one that owns it.
    let art = egui::Rect::from_min_size(
        rect.min + Vec2::new(22.0, 24.0),
        Vec2::new(width - 44.0, 84.0),
    );
    let big = egui::Rect::from_min_size(art.min, Vec2::new(104.0, 66.0));
    let small = egui::Rect::from_min_size(art.min + Vec2::new(118.0, 14.0), Vec2::new(78.0, 50.0));
    let (mine, other) = if host { (big, small) } else { (small, big) };
    painter.rect(
        other,
        CornerRadius::same(7),
        p.raised,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    painter.rect(
        mine,
        CornerRadius::same(7),
        Palette::tint(p.accent, 0.18),
        Stroke::new(1.4, p.accent),
        StrokeKind::Inside,
    );
    let cursor_on = if host { mine } else { other };
    widgets::cursor_glyph(
        &painter,
        cursor_on.center() - Vec2::new(4.0, 7.0),
        13.0,
        p.text,
        p.bg,
    );
    // A dashed path shows the cursor travelling to the other screen.
    let (from, to) = (big.right_center(), small.left_center());
    painter.extend(egui::Shape::dashed_line(
        &[from + Vec2::new(-14.0, 0.0), to + Vec2::new(14.0, 0.0)],
        Stroke::new(1.2, p.muted),
        4.0,
        3.0,
    ));

    let text_left = rect.left() + 22.0;
    let wrap = width - 44.0;
    let title = painter.layout(title.to_owned(), theme::semibold(16.0), p.text, wrap);
    let title_height = title.size().y;
    painter.galley(egui::pos2(text_left, art.bottom() + 12.0), title, p.text);
    let body = painter.layout(body.to_owned(), theme::regular(13.0), p.muted, wrap);
    painter.galley(
        egui::pos2(text_left, art.bottom() + 20.0 + title_height),
        body,
        p.muted,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Shown while the background service is not answering yet. If it stays
/// silent, say what to do instead of spinning forever.
pub fn starting(ui: &mut Ui, p: &Palette, waited: std::time::Duration) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.36);
        if waited < std::time::Duration::from_secs(6) {
            ui.add(egui::Spinner::new().size(22.0).color(p.accent));
            ui.add_space(6.0);
            widgets::muted(ui, p, tr("Starting Yzendris…", "Iniciando Yzendris…"));
        } else {
            ui.label(
                RichText::new(tr("The Yzendris service is not running", "El servicio de Yzendris no está en marcha"))
                    .font(theme::semibold(18.0)),
            );
            ui.add_space(2.0);
            widgets::muted(
                ui,
                p,
                tr(
                    "Install it once with the command below, or start it by hand with “yzendris run”.",
                    "Instálalo una vez con la orden de abajo, o arráncalo a mano con «yzendris run».",
                ),
            );
            ui.add_space(6.0);
            ui.label(RichText::new("yzendris install").font(theme::mono(14.0)).color(p.accent));
        }
    });
}

// ── Notices ─────────────────────────────────────────────────────────────────

fn notice_text(n: &Notice) -> (&'static str, bool) {
    match n.code {
        NoticeCode::InjectDenied => (
            tr(
                "Yzendris is not allowed to create its virtual keyboard and mouse yet.",
                "Yzendris aún no tiene permiso para crear su teclado y ratón virtuales.",
            ),
            true,
        ),
        NoticeCode::CaptureDenied => (
            tr(
                "Yzendris is not allowed to read this computer's keyboard and mouse yet.",
                "Yzendris aún no tiene permiso para leer el teclado y el ratón de este equipo.",
            ),
            true,
        ),
        NoticeCode::NoCursorProbe => (
            tr(
                "This desktop does not reveal where the cursor is, so screen edges cannot switch computers. Use Ctrl+Shift+Alt and the arrow keys instead.",
                "Este escritorio no revela dónde está el cursor, así que los bordes de pantalla no pueden cambiar de equipo. Usa Ctrl+Mayús+Alt y las flechas.",
            ),
            false,
        ),
        NoticeCode::NoScreens => (
            tr("No screen was detected on this computer.", "No se detectó ninguna pantalla en este equipo."),
            false,
        ),
        NoticeCode::ListenFailed => (
            tr(
                "Could not open the network port. Another program may be using it — change it in Settings.",
                "No se pudo abrir el puerto de red. Puede que otro programa lo use: cámbialo en Ajustes.",
            ),
            false,
        ),
        NoticeCode::ClipboardUnavailable => (
            tr("The clipboard is unreachable; copy and paste will not travel.", "No se puede acceder al portapapeles; copiar y pegar no viajará."),
            false,
        ),
    }
}

pub fn notices(app: &mut App, ui: &mut Ui, p: &Palette, status: &Status) {
    let installing = app.backend.lock().installing;
    for notice in &status.notices {
        let (text, fixable) = notice_text(notice);
        let info = notice.code == NoticeCode::NoCursorProbe;
        let color = if info { p.accent } else { p.warn };
        egui::Frame::NONE
            .fill(Palette::tint(color, 0.10))
            .stroke(Stroke::new(1.0, Palette::tint(color, 0.35)))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_width(ui.available_width());
                    if fixable {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_enabled_ui(!installing, |ui| {
                                let label = if installing {
                                    tr("Working…", "Trabajando…")
                                } else {
                                    tr("Grant access", "Dar permiso")
                                };
                                if widgets::button(ui, p, Kind::Plain, label).clicked() {
                                    app.backend.send(Cmd::Install);
                                }
                            });
                            ui.with_layout(
                                Layout::left_to_right(Align::Center).with_main_wrap(true),
                                |ui| {
                                    ui.label(
                                        RichText::new(text)
                                            .color(p.text)
                                            .font(theme::regular(13.0)),
                                    );
                                },
                            );
                        });
                    } else {
                        ui.add(
                            egui::Label::new(
                                RichText::new(text).color(p.text).font(theme::regular(13.0)),
                            )
                            .wrap(),
                        );
                    }
                });
            });
    }
}

// ── Host ────────────────────────────────────────────────────────────────────

pub fn host(app: &mut App, ui: &mut Ui, p: &Palette, status: &Status, config: &Config) {
    notices(app, ui, p, status);
    let sidebar = 300.0;
    let height = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        let canvas_width = (ui.available_width() - sidebar - 16.0).max(240.0);
        ui.allocate_ui_with_layout(
            Vec2::new(canvas_width, height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.horizontal(|ui| {
                    let here = match &status.active {
                        Some(name) => format!("{} {name}", tr("Typing on", "Escribiendo en")),
                        None => {
                            tr("Typing on this computer", "Escribiendo en este equipo").to_owned()
                        }
                    };
                    widgets::status(
                        ui,
                        if status.active.is_some() {
                            p.accent
                        } else {
                            p.ok
                        },
                        &here,
                        p.text,
                    );
                    if status.locked {
                        widgets::pill(ui, p.warn, tr("pointer locked", "puntero bloqueado"));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let (Some(addr), Some(port)) = (&status.address, status.port) {
                            widgets::muted(ui, p, &format!("{addr}:{port}"));
                        }
                    });
                });
                if let Some(canvas::Action::Move {
                    client,
                    monitor,
                    side,
                }) = canvas::show(ui, p, status, config, &mut app.drag)
                {
                    app.backend.edit(|c| {
                        if let Some(entry) =
                            c.host.clients.iter_mut().find(|e| e.fingerprint == client)
                        {
                            entry.monitor = monitor;
                            entry.side = side;
                        }
                    });
                }
            },
        );
        ui.allocate_ui_with_layout(
            Vec2::new(sidebar, height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_width(sidebar);
                match &status.pairing {
                    Some(pairing) => {
                        pairing_card(app, ui, p, status, &pairing.code, pairing.seconds_left)
                    }
                    None => devices(app, ui, p, status, config),
                }
            },
        );
    });
}

fn devices(app: &mut App, ui: &mut Ui, p: &Palette, status: &Status, config: &Config) {
    ui.horizontal(|ui| {
        widgets::title(ui, tr("Devices", "Dispositivos"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::button(ui, p, Kind::Primary, tr("Add device", "Añadir dispositivo"))
                .clicked()
            {
                app.backend.send(Cmd::PairOpen);
            }
        });
    });
    let footer = 96.0;
    egui::ScrollArea::vertical().max_height((ui.available_height() - footer).max(80.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if config.host.clients.is_empty() {
            widgets::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                widgets::muted(
                    ui,
                    p,
                    tr(
                        "No devices yet. Open Yzendris on your other computer, then press “Add device” here.",
                        "Aún no hay dispositivos. Abre Yzendris en tu otro equipo y pulsa «Añadir dispositivo» aquí.",
                    ),
                );
            });
        }
        for entry in &config.host.clients {
            let peer = status.peers.iter().find(|c| c.fingerprint == entry.fingerprint);
            let online = peer.is_some_and(|c| c.link == Link::Online);
            widgets::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.label(RichText::new(&entry.name).font(theme::semibold(14.5)));
                        let (color, text) = match (entry.enabled, online) {
                            (false, _) => (p.faint, tr("Paused", "En pausa").to_owned()),
                            (true, true) => {
                                let os = os_name(peer.and_then(|c| c.os));
                                let ms = peer.and_then(|c| c.latency_ms).map(|ms| format!(" · {ms} ms")).unwrap_or_default();
                                (p.ok, format!("{}{}{os}{ms}", tr("Connected", "Conectado"), if os.is_empty() { "" } else { " · " }))
                            }
                            (true, false) => (p.faint, tr("Offline", "Sin conexión").to_owned()),
                        };
                        widgets::status(ui, color, &text, p.muted);
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let mut enabled = entry.enabled;
                        if widgets::toggle(ui, p, &mut enabled).changed() {
                            let fp = entry.fingerprint;
                            app.backend.edit(|c| {
                                if let Some(e) = c.host.clients.iter_mut().find(|e| e.fingerprint == fp) {
                                    e.enabled = enabled;
                                }
                            });
                        }
                    });
                });
                if app.confirm_forget == Some(entry.fingerprint) {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if widgets::button(ui, p, Kind::Danger, tr("Remove", "Quitar")).clicked() {
                            app.backend.send(Cmd::Forget(entry.fingerprint));
                            app.confirm_forget = None;
                        }
                        if widgets::button(ui, p, Kind::Plain, tr("Keep", "Conservar")).clicked() {
                            app.confirm_forget = None;
                        }
                    });
                } else {
                    let link = ui.add(
                        egui::Label::new(RichText::new(tr("Remove this device", "Quitar este dispositivo")).color(p.faint).font(theme::regular(12.0)))
                            .sense(Sense::click()),
                    );
                    if link.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        app.confirm_forget = Some(entry.fingerprint);
                    }
                }
            });
        }
    });
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        shortcuts(ui, p, config.host.hotkeys);
    });
}

fn shortcuts(ui: &mut Ui, p: &Palette, enabled: bool) {
    if !enabled {
        return;
    }
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(2, 0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            ui.spacing_mut().interact_size.y = 14.0;
            let row = |ui: &mut Ui, keys: &str, what: &str| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(RichText::new(keys).font(theme::mono(11.5)).color(p.muted));
                    ui.label(
                        RichText::new(what)
                            .font(theme::regular(12.0))
                            .color(p.faint),
                    );
                });
            };
            // Bottom-up layout: last line first.
            row(
                ui,
                "Ctrl+Shift+Alt  L  ",
                tr("lock pointer here", "bloquear el puntero aquí"),
            );
            row(
                ui,
                "Ctrl+Shift+Alt  ← →",
                tr("switch computer", "cambiar de equipo"),
            );
            row(
                ui,
                "Ctrl+Shift+Alt     ",
                tr("bring input back", "traer el control de vuelta"),
            );
        });
}

fn pairing_card(
    app: &mut App,
    ui: &mut Ui,
    p: &Palette,
    status: &Status,
    code: &str,
    seconds_left: u32,
) {
    widgets::title(ui, tr("Add a device", "Añadir un dispositivo"));
    widgets::card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        widgets::muted(
            ui,
            p,
            tr(
                "Open Yzendris on the other computer, choose “controlled from another” and type this code:",
                "Abre Yzendris en el otro equipo, elige «se controla desde otro» y escribe este código:",
            ),
        );
        ui.add_space(6.0);
        code_display(ui, p, code);
        ui.add_space(6.0);
        // Time left, as a thin bar that drains.
        let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 3.0), Sense::hover());
        ui.painter()
            .rect_filled(bar, CornerRadius::same(2), p.border);
        let mut left = bar;
        left.set_width(bar.width() * (seconds_left as f32 / 120.0).clamp(0.0, 1.0));
        ui.painter()
            .rect_filled(left, CornerRadius::same(2), p.accent);
        widgets::muted(
            ui,
            p,
            &format!("{} {seconds_left} s", tr("Valid for", "Válido durante")),
        );
        if let (Some(addr), Some(port)) = (&status.address, status.port) {
            ui.add_space(4.0);
            widgets::muted(
                ui,
                p,
                tr(
                    "If it does not find this computer, its address is",
                    "Si no encuentra este equipo, su dirección es",
                ),
            );
            ui.label(RichText::new(format!("{addr}:{port}")).font(theme::mono(13.0)));
        }
        ui.add_space(8.0);
        if widgets::button(ui, p, Kind::Plain, tr("Cancel", "Cancelar")).clicked() {
            app.backend.send(Cmd::PairClose);
        }
    });
}

/// The pairing code as six large digit tiles.
fn code_display(ui: &mut Ui, p: &Palette, code: &str) {
    let digits: Vec<char> = code.chars().collect();
    let gap = 6.0;
    let group_gap = 10.0;
    let tile = ((ui.available_width() - gap * 4.0 - group_gap) / 6.0).min(40.0);
    let total = tile * 6.0 + gap * 4.0 + group_gap;
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), tile * 1.25), Sense::hover());
    let mut x = rect.center().x - total / 2.0;
    for (i, digit) in digits.iter().enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(tile, tile * 1.25));
        ui.painter().rect(
            r,
            CornerRadius::same(9),
            p.raised,
            Stroke::new(1.0, p.border),
            StrokeKind::Inside,
        );
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            digit,
            theme::mono(22.0),
            p.text,
        );
        x += tile + if i == 2 { group_gap } else { gap };
    }
}

// ── Client ──────────────────────────────────────────────────────────────────

pub fn client(app: &mut App, ui: &mut Ui, p: &Palette, status: &Status, config: &Config) {
    notices(app, ui, p, status);
    ui.add_space((ui.available_height() * 0.06).max(4.0));
    match status.peers.first() {
        Some(host) => column(ui, 520.0, |ui| {
            widgets::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                let controlled = status.active.is_some();
                let (color, headline, detail) = match host.link {
                    Link::Online if controlled => (
                        p.accent,
                        tr("Being controlled right now", "Te están controlando ahora"),
                        tr(
                            "The host's keyboard and mouse are driving this computer.",
                            "El teclado y el ratón del host manejan este equipo.",
                        ),
                    ),
                    Link::Online => (
                        p.ok,
                        tr("Connected", "Conectado"),
                        tr(
                            "Move the host's cursor over to this screen to use it.",
                            "Lleva el cursor del host hasta esta pantalla para usarla.",
                        ),
                    ),
                    Link::Connecting => (
                        p.warn,
                        tr("Connecting…", "Conectando…"),
                        tr(
                            "Looking for the host on the network.",
                            "Buscando el host en la red.",
                        ),
                    ),
                    Link::Refused => (
                        p.danger,
                        tr(
                            "The host no longer knows this computer",
                            "El host ya no reconoce este equipo",
                        ),
                        tr(
                            "It was removed there. Forget the host below and pair again.",
                            "Lo quitaron allí. Olvida el host aquí abajo y vuelve a emparejar.",
                        ),
                    ),
                    Link::Offline => (
                        p.faint,
                        tr("Host not reachable", "No se alcanza el host"),
                        tr(
                            "It may be off or on another network. Yzendris keeps trying.",
                            "Puede estar apagado o en otra red. Yzendris sigue intentándolo.",
                        ),
                    ),
                };
                widgets::status(ui, color, headline, p.text);
                ui.label(
                    RichText::new(if host.name.is_empty() {
                        tr("Your host", "Tu host")
                    } else {
                        &host.name
                    })
                    .font(theme::semibold(24.0)),
                );
                widgets::muted(ui, p, detail);
                ui.add_space(4.0);
                widgets::divider(ui, p);
                ui.add_space(2.0);
                let fact = |ui: &mut Ui, label: &str, value: String| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(label)
                                .color(p.muted)
                                .font(theme::regular(13.0)),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(value).font(theme::mono(12.5)));
                        });
                    });
                };
                if let Some(addr) = &host.address {
                    fact(ui, tr("Address", "Dirección"), addr.clone());
                }
                if let Some(ms) = host.latency_ms {
                    fact(
                        ui,
                        tr("Response time", "Tiempo de respuesta"),
                        format!("{ms} ms"),
                    );
                }
                fact(ui, tr("Identity", "Identidad"), host.fingerprint.short());
                for s in &status.screens {
                    fact(
                        ui,
                        tr("This screen", "Esta pantalla"),
                        format!("{} · {} × {}", s.name, s.rect.w, s.rect.h),
                    );
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if app.confirm_forget == Some(host.fingerprint) {
                    if widgets::button(ui, p, Kind::Danger, tr("Yes, forget it", "Sí, olvidarlo"))
                        .clicked()
                    {
                        app.backend.send(Cmd::Forget(host.fingerprint));
                        app.confirm_forget = None;
                    }
                    if widgets::button(ui, p, Kind::Plain, tr("Cancel", "Cancelar")).clicked() {
                        app.confirm_forget = None;
                    }
                } else if widgets::button(
                    ui,
                    p,
                    Kind::Plain,
                    tr("Forget this host", "Olvidar este host"),
                )
                .clicked()
                {
                    app.confirm_forget = Some(host.fingerprint);
                }
            });
        }),
        None => join(app, ui, p, config),
    }
}

fn join(app: &mut App, ui: &mut Ui, p: &Palette, _config: &Config) {
    let (hosts, joining) = {
        let mut shared = app.backend.lock();
        shared.want_hosts = true;
        (shared.hosts.clone(), shared.joining)
    };
    column(ui, 460.0, |ui| {
        ui.label(
            RichText::new(tr("Connect to your host", "Conecta con tu host"))
                .font(theme::semibold(24.0)),
        );
        widgets::muted(
            ui,
            p,
            tr(
                "On the computer with the keyboard and mouse, open Yzendris and press “Add device”. It will show a 6-digit code.",
                "En el equipo con el teclado y el ratón, abre Yzendris y pulsa «Añadir dispositivo». Mostrará un código de 6 dígitos.",
            ),
        );
        ui.add_space(8.0);
        widgets::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(tr("Host", "Host"))
                    .font(theme::medium(13.0))
                    .color(p.muted),
            );
            if hosts.is_empty() && !app.manual {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0).color(p.accent));
                    widgets::muted(
                        ui,
                        p,
                        tr(
                            "Looking for a host waiting to add a device…",
                            "Buscando un host que espere un dispositivo…",
                        ),
                    );
                });
            }
            for found in &hosts {
                let selected = app.join_addr == found.addr;
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
                let stroke = if selected {
                    Stroke::new(1.4, p.accent)
                } else {
                    Stroke::new(1.0, p.border)
                };
                let fill = if selected {
                    Palette::tint(p.accent, 0.12)
                } else {
                    p.raised
                };
                ui.painter().rect(
                    rect,
                    CornerRadius::same(10),
                    fill,
                    stroke,
                    StrokeKind::Inside,
                );
                ui.painter().text(
                    rect.left_center() + Vec2::new(14.0, 0.0),
                    Align2::LEFT_CENTER,
                    &found.name,
                    theme::medium(14.0),
                    p.text,
                );
                ui.painter().text(
                    rect.right_center() - Vec2::new(14.0, 0.0),
                    Align2::RIGHT_CENTER,
                    &found.addr,
                    theme::mono(12.0),
                    p.muted,
                );
                if resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    app.join_addr = found.addr.clone();
                    app.manual = false;
                }
            }
            // With a single host around there is nothing to choose.
            if hosts.len() == 1 && app.join_addr.is_empty() && !app.manual {
                app.join_addr = hosts[0].addr.clone();
            }
            if app.manual {
                ui.add(
                    egui::TextEdit::singleline(&mut app.join_addr)
                        .hint_text(tr(
                            "Address, e.g. 192.168.1.20",
                            "Dirección, p. ej. 192.168.1.20",
                        ))
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(12, 9)),
                );
            } else {
                let link = ui.add(
                    egui::Label::new(
                        RichText::new(tr("Type the address instead", "Escribir la dirección"))
                            .color(p.accent)
                            .font(theme::regular(12.5)),
                    )
                    .sense(Sense::click()),
                );
                if link
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    app.manual = true;
                }
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(tr("Code", "Código"))
                    .font(theme::medium(13.0))
                    .color(p.muted),
            );
            let field = ui.add(
                egui::TextEdit::singleline(&mut app.join_code)
                    .hint_text("000 000")
                    .font(theme::mono(22.0))
                    .char_limit(7)
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(12, 10)),
            );
            app.join_code.retain(|c| c.is_ascii_digit() || c == ' ');
            let digits = app.join_code.chars().filter(char::is_ascii_digit).count();
            let ready = digits == 6 && !app.join_addr.trim().is_empty() && !joining;
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let go = ui
                    .add_enabled_ui(ready, |ui| {
                        widgets::button(ui, p, Kind::Primary, tr("Connect", "Conectar"))
                    })
                    .inner;
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (go.clicked() || enter) && ready {
                    app.backend.send(Cmd::Join {
                        addr: app.join_addr.trim().to_owned(),
                        code: app.join_code.clone(),
                    });
                }
                if joining {
                    ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                }
            });
        });
    });
}

// ── Settings ────────────────────────────────────────────────────────────────

pub fn settings(app: &mut App, ui: &mut Ui, p: &Palette, status: &Status, config: &Config) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        column(ui, 620.0, |ui| {
            let section = |ui: &mut Ui, name: &str| {
                ui.add_space(6.0);
                ui.label(RichText::new(name).font(theme::medium(12.5)).color(p.muted));
            };

            section(ui, tr("THIS COMPUTER", "ESTE EQUIPO"));
            widgets::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                widgets::setting(
                    ui,
                    p,
                    tr("Name", "Nombre"),
                    tr(
                        "How your other computers see this one.",
                        "Cómo ven este equipo los demás.",
                    ),
                    |ui| {
                        let draft = app.name_draft.get_or_insert_with(|| config.name.clone());
                        let field = ui.add(
                            egui::TextEdit::singleline(draft)
                                .desired_width(170.0)
                                .char_limit(40)
                                .margin(egui::Margin::symmetric(10, 7)),
                        );
                        if field.lost_focus() {
                            let name = draft.trim().to_owned();
                            if !name.is_empty() && name != config.name {
                                app.backend.edit(|c| c.name = name);
                            }
                            app.name_draft = None;
                        }
                    },
                );
                widgets::divider(ui, p);
                widgets::setting(
                    ui,
                    p,
                    tr("Role", "Función"),
                    match config.role {
                        Some(Role::Host) => tr(
                            "Shares its keyboard and mouse.",
                            "Comparte su teclado y ratón.",
                        ),
                        _ => tr(
                            "Is controlled from another computer.",
                            "Se controla desde otro equipo.",
                        ),
                    },
                    |ui| {
                        let (label, next) = match config.role {
                            Some(Role::Host) => {
                                (tr("Make it a client", "Hacerlo cliente"), Role::Client)
                            }
                            _ => (tr("Make it the host", "Hacerlo host"), Role::Host),
                        };
                        if widgets::button(ui, p, Kind::Plain, label).clicked() {
                            app.backend.edit(|c| c.role = Some(next));
                            app.tab = 0;
                        }
                    },
                );
            });

            section(ui, tr("SHARING", "COMPARTIR"));
            widgets::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                let mut clipboard = config.clipboard;
                widgets::setting(
                    ui,
                    p,
                    tr("Clipboard", "Portapapeles"),
                    tr(
                        "Copy on one computer, paste on the other.",
                        "Copia en un equipo y pega en el otro.",
                    ),
                    |ui| {
                        if widgets::toggle(ui, p, &mut clipboard).changed() {
                            app.backend.edit(|c| c.clipboard = clipboard);
                        }
                    },
                );
                if config.clipboard {
                    widgets::divider(ui, p);
                    let mut files = config.files;
                    widgets::setting(
                        ui,
                        p,
                        tr("Copied files too", "También archivos copiados"),
                        tr(
                            "Files are carried across and never opened.",
                            "Los archivos viajan y nunca se abren.",
                        ),
                        |ui| {
                            if widgets::toggle(ui, p, &mut files).changed() {
                                app.backend.edit(|c| c.files = files);
                            }
                        },
                    );
                    if config.files {
                        widgets::divider(ui, p);
                        let mut max = config.max_file_mb;
                        widgets::setting(
                            ui,
                            p,
                            tr("Largest file", "Archivo más grande"),
                            "",
                            |ui| {
                                let drag = ui.add(
                                    egui::DragValue::new(&mut max)
                                        .range(1..=100_000)
                                        .suffix(" MB")
                                        .speed(5.0),
                                );
                                if drag.changed() && !drag.dragged() || drag.drag_stopped() {
                                    app.backend.edit(|c| c.max_file_mb = max);
                                }
                            },
                        );
                    }
                }
            });

            if config.role == Some(Role::Host) {
                section(ui, tr("SWITCHING COMPUTERS", "CAMBIAR DE EQUIPO"));
                widgets::card(ui, p, |ui| {
                    ui.set_width(ui.available_width());
                    let mut edges = config.host.edges;
                    widgets::setting(
                        ui,
                        p,
                        tr("Screen edges", "Bordes de pantalla"),
                        tr(
                            "Push the cursor past an edge to cross over.",
                            "Empuja el cursor más allá de un borde para cruzar.",
                        ),
                        |ui| {
                            if widgets::toggle(ui, p, &mut edges).changed() {
                                app.backend.edit(|c| c.host.edges = edges);
                            }
                        },
                    );
                    widgets::divider(ui, p);
                    let mut hotkeys = config.host.hotkeys;
                    widgets::setting(
                        ui,
                        p,
                        tr("Keyboard shortcuts", "Atajos de teclado"),
                        tr(
                            "Ctrl+Shift+Alt with arrows, digits or L.",
                            "Ctrl+Mayús+Alt con flechas, números o L.",
                        ),
                        |ui| {
                            if widgets::toggle(ui, p, &mut hotkeys).changed() {
                                app.backend.edit(|c| c.host.hotkeys = hotkeys);
                            }
                        },
                    );
                    widgets::divider(ui, p);
                    let mut speed = config.host.pointer_speed;
                    widgets::setting(
                        ui,
                        p,
                        tr(
                            "Pointer speed on other computers",
                            "Velocidad del puntero en otros equipos",
                        ),
                        "",
                        |ui| {
                            ui.spacing_mut().slider_width = 150.0;
                            let slider = ui.add(
                                egui::Slider::new(&mut speed, 0.4..=2.5)
                                    .step_by(0.05)
                                    .fixed_decimals(2)
                                    .suffix("×"),
                            );
                            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                                app.backend.edit(|c| c.host.pointer_speed = speed);
                            }
                        },
                    );
                    widgets::divider(ui, p);
                    let mut port = config.host.port;
                    widgets::setting(
                        ui,
                        p,
                        tr("Network port", "Puerto de red"),
                        tr(
                            "Clients connect here. Change only if it is taken.",
                            "Los clientes se conectan aquí. Cámbialo solo si está ocupado.",
                        ),
                        |ui| {
                            let drag = ui.add(
                                egui::DragValue::new(&mut port)
                                    .range(1024..=65535)
                                    .speed(1.0),
                            );
                            if (drag.lost_focus() || drag.drag_stopped())
                                && port != config.host.port
                            {
                                app.backend.edit(|c| c.host.port = port);
                            }
                        },
                    );
                });
            }

            section(ui, tr("APP", "APLICACIÓN"));
            widgets::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                widgets::setting(ui, p, tr("Language", "Idioma"), "", |ui| {
                    let current = i18n::current();
                    for (lang, label) in [(Lang::Es, "Español"), (Lang::En, "English")] {
                        let kind = if current == lang {
                            Kind::Primary
                        } else {
                            Kind::Plain
                        };
                        if widgets::button(ui, p, kind, label).clicked() {
                            app.prefs.language = if lang == Lang::Es { "es" } else { "en" }.into();
                            app.prefs_changed = true;
                        }
                    }
                });
                widgets::divider(ui, p);
                widgets::setting(ui, p, tr("Appearance", "Apariencia"), "", |ui| {
                    for (value, label) in [
                        ("light", tr("Light", "Claro")),
                        ("dark", tr("Dark", "Oscuro")),
                        ("system", tr("System", "Sistema")),
                    ] {
                        let kind = if app.prefs.theme == value {
                            Kind::Primary
                        } else {
                            Kind::Plain
                        };
                        if widgets::button(ui, p, kind, label).clicked() {
                            app.prefs.theme = value.into();
                            app.prefs_changed = true;
                        }
                    }
                });
                widgets::divider(ui, p);
                let (installing, result) = {
                    let shared = app.backend.lock();
                    (shared.installing, shared.install_result.clone())
                };
                let hint = match &result {
                    Some(Ok(())) => tr(
                        "Installed. Yzendris starts with your session.",
                        "Instalado. Yzendris arranca con tu sesión.",
                    )
                    .to_owned(),
                    Some(Err(e)) => e.clone(),
                    None => tr(
                        "Keep it running in the background and grant the device access it needs.",
                        "Mantenlo en segundo plano y dale el acceso a dispositivos que necesita.",
                    )
                    .to_owned(),
                };
                widgets::setting(
                    ui,
                    p,
                    tr("Start with the computer", "Arrancar con el equipo"),
                    &hint,
                    |ui| {
                        ui.add_enabled_ui(!installing, |ui| {
                            let label = if installing {
                                tr("Installing…", "Instalando…")
                            } else {
                                tr("Install", "Instalar")
                            };
                            if widgets::button(ui, p, Kind::Plain, label).clicked() {
                                app.backend.send(Cmd::Install);
                            }
                        });
                    },
                );
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                widgets::muted(
                    ui,
                    p,
                    &format!(
                        "Yzendris {} · {} {}",
                        status.version,
                        tr("identity", "identidad"),
                        status.fingerprint.short()
                    ),
                );
            });
            ui.add_space(16.0);
        });
    });
}

// ── Activity ────────────────────────────────────────────────────────────────

pub fn activity(app: &mut App, ui: &mut Ui, p: &Palette) {
    let lines = {
        let mut shared = app.backend.lock();
        shared.want_logs = true;
        shared.logs.clone()
    };
    widgets::muted(
        ui,
        p,
        tr(
            "What Yzendris has been doing. Keys, clipboard contents and file names are never recorded.",
            "Lo que ha hecho Yzendris. Nunca se registran teclas, contenido del portapapeles ni nombres de archivo.",
        ),
    );
    widgets::card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(ui.available_height());
        egui::ScrollArea::both()
            .stick_to_bottom(true)
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                if lines.is_empty() {
                    widgets::muted(ui, p, tr("Nothing yet.", "Nada todavía."));
                }
                for line in &lines {
                    let color = if line.contains(" ERROR ") {
                        p.danger
                    } else if line.contains(" WARN ") {
                        p.warn
                    } else {
                        p.muted
                    };
                    ui.label(RichText::new(line).font(theme::mono(12.0)).color(color));
                }
            });
    });
}

/// A transient message at the bottom of the window.
pub fn toast(ui: &mut Ui, p: &Palette, text: &str) {
    let screen = ui.ctx().content_rect();
    let galley = ui.painter().layout(
        text.to_owned(),
        theme::regular(13.5),
        p.text,
        (screen.width() - 120.0).min(520.0),
    );
    let size = galley.size() + Vec2::new(32.0, 22.0);
    let rect = egui::Rect::from_center_size(
        egui::pos2(screen.center().x, screen.bottom() - 34.0 - size.y / 2.0),
        size,
    );
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("toast"),
    ));
    painter.rect_filled(
        rect.translate(Vec2::new(0.0, 4.0)).expand(1.0),
        CornerRadius::same(13),
        Color32::from_black_alpha(50),
    );
    painter.rect(
        rect,
        CornerRadius::same(12),
        p.raised,
        Stroke::new(1.0, Palette::tint(p.danger, 0.55)),
        StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() / 2.0, galley, p.text);
}
