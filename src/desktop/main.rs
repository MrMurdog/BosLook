//! BosLook Desktop: zeigt die Telegramme eines BosLook-Servers und führt lokale Ereignisse aus
//! (Ton, Benachrichtigung/Popup, Programm). Läuft beim Schließen im Infobereich weiter.

// Unter Windows kein zusätzliches Konsolenfenster öffnen
#![cfg_attr(windows, windows_subsystem = "windows")]

mod client;
mod config;
mod events;

use boslook::telegram::{Kind, Telegram};
use clap::{Parser, Subcommand};
use client::{FromClient, ToClient};
use config::DesktopConfig;
use eframe::egui::{self, Color32, RichText};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;

/// So viele Telegramme je Quelle bleiben in der Anzeige
const MAX_ROWS: usize = 1000;

#[derive(Parser)]
#[command(name = "boslook-desktop", version, about = "BosLook Desktop")]
struct Cli {
    #[arg(short, long, default_value = "boslook-desktop.toml")]
    config: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Alarmfenster anzeigen (wird von der App selbst gestartet)
    #[command(hide = true)]
    Popup {
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
        #[arg(long)]
        emergency: bool,
    },
}

fn main() -> eframe::Result {
    let cli = Cli::parse();
    if let Some(Command::Popup { title, body, emergency }) = cli.command {
        return popup(title, body, emergency);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("BosLook").with_inner_size([1000.0, 650.0]).with_icon(icon()),
        ..Default::default()
    };
    eframe::run_native("BosLook", options, Box::new(|cc| Ok(Box::new(App::new(cli.config, &cc.egui_ctx)))))
}

#[derive(PartialEq)]
enum View {
    Telegrams,
    Desktop,
    Server,
}

struct App {
    config_path: String,
    rx: mpsc::Receiver<FromClient>,
    cmd: Option<tokio::sync::mpsc::UnboundedSender<ToClient>>,
    status: String,
    connected: bool,
    sources: Vec<String>,
    rows: HashMap<String, VecDeque<Telegram>>,
    unread: HashMap<String, usize>,
    active: Option<String>,
    view: View,
    desktop_text: String,
    desktop_msg: String,
    server_text: String,
    server_msg: String,
    tray: Option<tray_icon::TrayIcon>,
    tray_rx: mpsc::Receiver<TrayAction>,
    quit: bool,
}

enum TrayAction {
    Show,
    Quit,
}

impl App {
    fn new(config_path: String, ctx: &egui::Context) -> App {
        let (tx, rx) = mpsc::channel();
        let (tray, tray_rx) = tray(ctx);
        let mut app = App {
            config_path,
            rx,
            cmd: None,
            status: String::new(),
            connected: false,
            sources: Vec::new(),
            rows: HashMap::new(),
            unread: HashMap::new(),
            active: None,
            view: View::Telegrams,
            desktop_text: String::new(),
            desktop_msg: String::new(),
            server_text: String::new(),
            server_msg: String::new(),
            tray,
            tray_rx,
            quit: false,
        };
        if !std::path::Path::new(&app.config_path).exists() {
            let _ = std::fs::write(&app.config_path, config::TEMPLATE);
        }
        app.desktop_text = std::fs::read_to_string(&app.config_path).unwrap_or_default();
        match DesktopConfig::parse(&app.desktop_text) {
            Ok(c) if c.server.password != "ERSETZEN" => match events::Events::new(&c.events, c.repeat) {
                Ok(events) => {
                    let ctx = ctx.clone();
                    app.cmd = Some(client::spawn(c.server, events, tx, move || ctx.request_repaint()));
                }
                Err(e) => app.open_settings(format!("Fehler: {e:#}")),
            },
            Ok(_) => app.open_settings(format!("Bitte Server und Zugangsdaten in {} eintragen.", app.config_path)),
            Err(e) => app.open_settings(format!("Fehler in {}: {e:#}", app.config_path)),
        }
        app
    }

    fn open_settings(&mut self, msg: String) {
        self.status = "nicht verbunden".into();
        self.view = View::Desktop;
        self.desktop_msg = msg;
    }

    fn receive(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                FromClient::Status { text, connected } => (self.status, self.connected) = (text, connected),
                FromClient::Reset { sources, history } => {
                    self.rows.clear();
                    for t in history {
                        self.rows.entry(t.source.clone()).or_default().push_back(t);
                    }
                    for s in &sources {
                        self.rows.entry(s.clone()).or_default();
                    }
                    self.sources = sources;
                    if self.active.as_ref().is_none_or(|a| !self.sources.contains(a)) {
                        self.active = self.sources.first().cloned();
                    }
                }
                FromClient::Telegram(t) => {
                    if !self.sources.contains(&t.source) {
                        self.sources.push(t.source.clone());
                    }
                    self.active.get_or_insert_with(|| t.source.clone());
                    if self.active.as_ref() != Some(&t.source) {
                        *self.unread.entry(t.source.clone()).or_default() += 1;
                    }
                    let rows = self.rows.entry(t.source.clone()).or_default();
                    rows.push_front(t);
                    rows.truncate(MAX_ROWS);
                }
                FromClient::ServerConfig(Ok(text)) => (self.server_text, self.server_msg) = (text, String::new()),
                FromClient::ServerConfig(Err(e)) | FromClient::Saved(Err(e)) => self.server_msg = e,
                FromClient::Saved(Ok(msg)) => self.server_msg = msg,
            }
        }
    }

    fn telegrams(&mut self, ui: &mut egui::Ui) {
        if self.sources.is_empty() {
            ui.label("Noch keine Quellen vorhanden.");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            for s in &self.sources {
                let mut label = s.clone();
                if let Some(n) = self.unread.get(s).filter(|n| **n > 0) {
                    label = format!("{s} ({n})");
                }
                if ui.selectable_label(self.active.as_ref() == Some(s), label).clicked() {
                    self.active = Some(s.clone());
                }
            }
        });
        let Some(active) = self.active.clone() else { return };
        self.unread.remove(&active);
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("rows").striped(true).num_columns(5).spacing([12.0, 6.0]).show(ui, |ui| {
                for h in ["Zeit", "Art", "Adresse", "Sub", "Meldung"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for t in self.rows.get(&active).into_iter().flatten() {
                    ui.label(t.time.format("%d.%m.%Y %H:%M:%S").to_string());
                    ui.label(RichText::new(t.kind.as_str().to_uppercase()).strong().color(Color32::WHITE).background_color(kind_color(t.kind)));
                    ui.monospace(&t.address);
                    ui.monospace(&t.sub);
                    ui.add(egui::Label::new(&t.message).wrap());
                    ui.end_row();
                }
            });
        });
    }

    fn desktop_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let save = ui.button("Speichern").clicked();
            let restart = ui.button("Speichern und neu starten").clicked();
            if save || restart {
                match DesktopConfig::parse(&self.desktop_text).and_then(|_| Ok(std::fs::write(&self.config_path, &self.desktop_text)?)) {
                    Ok(()) if restart => boslook::serve::restart(),
                    Ok(()) => self.desktop_msg = "Gespeichert. Wirksam nach einem Neustart.".into(),
                    Err(e) => self.desktop_msg = format!("Nicht gespeichert: {e:#}"),
                }
            }
        });
        ui.label(RichText::new(&self.desktop_msg).monospace());
        editor(ui, &mut self.desktop_text);
    }

    fn server_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_enabled_ui(self.connected, |ui| {
                if ui.button("Neu laden").clicked() {
                    self.send(ToClient::LoadServerConfig);
                }
                for (label, restart) in [("Speichern", false), ("Speichern und neu starten", true)] {
                    if ui.button(label).clicked() {
                        self.send(ToClient::SaveServerConfig { text: self.server_text.clone(), restart });
                    }
                }
            });
        });
        ui.label(RichText::new(&self.server_msg).monospace());
        editor(ui, &mut self.server_text);
    }

    fn send(&mut self, cmd: ToClient) {
        if let Some(tx) = &self.cmd {
            let _ = tx.send(cmd);
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive();
        while let Ok(action) = self.tray_rx.try_recv() {
            match action {
                TrayAction::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayAction::Quit => {
                    self.quit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        // Mit Infobereich nur verstecken, damit Ereignisse weiter ausgeführt werden
        if ctx.input(|i| i.viewport().close_requested()) && self.tray.is_some() && !self.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Bos").strong());
                ui.heading(RichText::new("Look").strong().color(Color32::from_rgb(0xc6, 0x28, 0x28)));
                ui.separator();
                ui.selectable_value(&mut self.view, View::Telegrams, "Telegramme");
                ui.selectable_value(&mut self.view, View::Desktop, "Einstellungen");
                if ui.selectable_value(&mut self.view, View::Server, "Server-Konfiguration").clicked() && self.server_text.is_empty() {
                    self.send(ToClient::LoadServerConfig);
                }
                let color = if self.connected { Color32::from_rgb(0x43, 0xa0, 0x47) } else { Color32::from_rgb(0xc6, 0x28, 0x28) };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.status);
                    ui.label(RichText::new("●").color(color));
                });
            });
        });
        egui::CentralPanel::default_margins().show(ui, |ui| match self.view {
            View::Telegrams => self.telegrams(ui),
            View::Desktop => self.desktop_settings(ui),
            View::Server => self.server_settings(ui),
        });
    }
}

fn editor(ui: &mut egui::Ui, text: &mut String) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add(egui::TextEdit::multiline(text).code_editor().desired_width(f32::INFINITY).desired_rows(30));
    });
}

fn kind_color(kind: Kind) -> Color32 {
    match kind {
        Kind::Pocsag => Color32::from_rgb(0xc6, 0x28, 0x28),
        Kind::Fms => Color32::from_rgb(0x15, 0x65, 0xc0),
        Kind::Zvei => Color32::from_rgb(0x2e, 0x7d, 0x32),
        Kind::Wetter => Color32::from_rgb(0xef, 0x6c, 0x00),
    }
}

/// Rotes Rundsymbol für Fenster und Infobereich.
fn icon_rgba() -> (Vec<u8>, u32) {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
            let r = (dx * dx + dy * dy).sqrt();
            let pixel = if r < 9.0 {
                [0xff, 0xff, 0xff, 0xff]
            } else if r < 15.5 {
                [0xc6, 0x28, 0x28, 0xff]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend(pixel);
        }
    }
    (rgba, SIZE)
}

fn icon() -> egui::IconData {
    let (rgba, size) = icon_rgba();
    egui::IconData { rgba, width: size, height: size }
}

/// Symbol im Infobereich mit Menü „Anzeigen" / „Beenden". Ohne Infobereich beendet das
/// Schließen des Fensters die App.
fn tray(ctx: &egui::Context) -> (Option<tray_icon::TrayIcon>, mpsc::Receiver<TrayAction>) {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{MouseButton, TrayIconEvent};
    let (tx, rx) = mpsc::channel();
    let menu = Menu::new();
    let show = MenuItem::new("Anzeigen", true, None);
    let quit = MenuItem::new("Beenden", true, None);
    if menu.append_items(&[&show, &quit]).is_err() {
        return (None, rx);
    }
    let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
    let (menu_tx, menu_ctx) = (tx.clone(), ctx.clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let action = if e.id == quit_id {
            TrayAction::Quit
        } else if e.id == show_id {
            TrayAction::Show
        } else {
            return;
        };
        let _ = menu_tx.send(action);
        menu_ctx.request_repaint();
    }));
    let (click_tx, click_ctx) = (tx, ctx.clone());
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, .. } = e {
            let _ = click_tx.send(TrayAction::Show);
            click_ctx.request_repaint();
        }
    }));
    let (rgba, size) = icon_rgba();
    let tray = tray_icon::Icon::from_rgba(rgba, size, size).ok().and_then(|icon| {
        tray_icon::TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("BosLook").with_icon(icon).build().ok()
    });
    (tray, rx)
}

/// Alarmfenster im Vordergrund, bleibt bis zum Quittieren.
fn popup(title: String, body: String, emergency: bool) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("BosLook – {title}"))
            .with_inner_size([520.0, 300.0])
            .with_always_on_top()
            .with_icon(icon()),
        ..Default::default()
    };
    eframe::run_native(
        "BosLook Alarm",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            if emergency {
                cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
            }
            Ok(Box::new(Popup { title, body, emergency }))
        }),
    )
}

struct Popup {
    title: String,
    body: String,
    emergency: bool,
}

impl eframe::App for Popup {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            if self.emergency {
                ui.label(RichText::new("NOTFALL").size(16.0).strong().color(Color32::from_rgb(0xc6, 0x28, 0x28)));
            }
            ui.label(RichText::new(&self.title).size(26.0).strong().color(Color32::from_rgb(0xc6, 0x28, 0x28)));
            ui.add_space(8.0);
            egui::ScrollArea::vertical().max_height(170.0).show(ui, |ui| {
                ui.label(RichText::new(&self.body).size(18.0));
            });
            ui.add_space(8.0);
            if ui.add(egui::Button::new(RichText::new("Quittieren").size(18.0))).clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}
