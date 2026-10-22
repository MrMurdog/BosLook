//! BosLook Desktop: zeigt die Telegramme eines BosLook-Servers und führt lokale Ereignisse aus
//! (Ton, Benachrichtigung/Popup, Programm). Läuft beim Schließen im Infobereich weiter.

// Unter Windows kein zusätzliches Konsolenfenster öffnen
#![cfg_attr(windows, windows_subsystem = "windows")]

mod client;
mod config;
mod descriptions;
mod events;
mod form;

use boslook::telegram::{Kind, Telegram};
use clap::{Parser, Subcommand};
use client::{FromClient, ToClient};
use config::DesktopConfig;
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;

/// Rot des Logos
const RED: Color32 = Color32::from_rgb(0xdd, 0x31, 0x27);
/// So viele Telegramme je Quelle bleiben in der Anzeige
const MAX_ROWS: usize = 1000;
/// Spalten der Tabelle: Schlüssel, Überschrift, Anfangsbreite (`None`: Meldung, bekommt den Rest)
const COLUMNS: [(&str, &str, Option<f32>); 8] = [
    ("time", "Zeit", Some(150.0)),
    ("kind", "Art", Some(70.0)),
    ("address", "Adresse", Some(90.0)),
    ("sub", "Sub", Some(60.0)),
    ("message", "Meldung", None),
    ("ort", "Ort", Some(140.0)),
    ("kurz", "Kurzbeschreibung", Some(140.0)),
    ("lang", "Langbeschreibung", Some(200.0)),
];

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
    let options = options(egui::ViewportBuilder::default().with_title("BosLook").with_inner_size([1000.0, 650.0]));
    eframe::run_native(
        "BosLook",
        options,
        Box::new(|cc| {
            let hidden = cc.storage.and_then(|s| eframe::get_value(s, HIDDEN)).unwrap_or_default();
            Ok(Box::new(App::new(cli.config, hidden, &cc.egui_ctx)))
        }),
    )
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
    desktop: Editor,
    server: Editor,
    /// Anfrage an den Server läuft, nicht nochmal laden
    server_loading: bool,
    descriptions: descriptions::Window,
    /// Adresse der Weboberfläche des Servers, für Seiten, die im Browser aufgehen
    server_url: Option<String>,
    /// Plugins mit eigener Seite
    plugin_pages: Vec<String>,
    /// Ausgeblendete Spalten (Schlüssel aus `COLUMNS`)
    hidden: Vec<String>,
    tray: Option<tray_icon::TrayIcon>,
    tray_rx: mpsc::Receiver<TrayAction>,
    quit: bool,
}

/// Schlüssel der ausgeblendeten Spalten im gespeicherten Zustand
const HIDDEN: &str = "hidden_columns";

enum TrayAction {
    Show,
    Quit,
}

impl App {
    fn new(config_path: String, hidden: Vec<String>, ctx: &egui::Context) -> App {
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
            desktop: Editor::default(),
            server: Editor::default(),
            server_loading: false,
            descriptions: descriptions::Window::default(),
            server_url: None,
            plugin_pages: Vec::new(),
            hidden,
            tray,
            tray_rx,
            quit: false,
        };
        if !std::path::Path::new(&app.config_path).exists() {
            let _ = std::fs::write(&app.config_path, config::TEMPLATE);
        }
        app.load_desktop();
        match DesktopConfig::parse(&std::fs::read_to_string(&app.config_path).unwrap_or_default()) {
            Ok(c) if c.server.password != "ERSETZEN" => match events::Events::new(&c.events, c.repeat) {
                Ok(events) => {
                    let ctx = ctx.clone();
                    app.server_url = Some(c.server.url.trim_end_matches('/').to_string());
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
        self.desktop.msg = msg;
    }

    fn load_desktop(&mut self) {
        let text = std::fs::read_to_string(&self.config_path).unwrap_or_default();
        let schema = serde_json::to_value(config::schema()).expect("Schema");
        match boslook::form::to_json(&text, &config::schema()) {
            Ok(values) if !self.desktop.text_mode => self.desktop.form = Some(form::Form::new(schema, values, Vec::new())),
            Ok(_) => {}
            Err(e) => {
                self.desktop.text_mode = true;
                self.desktop.msg = format!("Als Formular nicht lesbar, bitte als Text korrigieren: {e:#}");
            }
        }
        self.desktop.set_text(text);
    }

    /// Formular bzw. Text in die Datei übernehmen
    fn save_desktop(&mut self) -> anyhow::Result<()> {
        let text = match &self.desktop.form {
            Some(form) if !self.desktop.text_mode => {
                boslook::form::apply(&std::fs::read_to_string(&self.config_path)?, &config::schema(), &form.values)?
            }
            _ => self.desktop.text.clone(),
        };
        DesktopConfig::parse(&text)?;
        std::fs::write(&self.config_path, text)?;
        Ok(())
    }

    fn receive(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                FromClient::Status { text, connected } => (self.status, self.connected) = (text, connected),
                FromClient::Reset { sources, history, pages } => {
                    self.plugin_pages = pages;
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
                FromClient::ServerConfig(Ok(text)) => {
                    self.server_loading = false;
                    self.server.set_text(text);
                }
                FromClient::ServerForm(Ok(values)) => {
                    self.server_loading = false;
                    self.server.form = Some(form::Form::from_server(values));
                }
                FromClient::ServerConfig(Err(e)) | FromClient::ServerForm(Err(e)) => {
                    self.server_loading = false;
                    self.server.msg = e;
                }
                FromClient::Saved(Err(e)) => self.server.msg = e,
                FromClient::Saved(Ok(msg)) => {
                    // Neu laden, damit Listeneinträge und Passwort-Hashes zum gespeicherten Stand passen
                    self.server.msg = msg;
                    self.server.clear();
                }
                FromClient::Descriptions(result) => self.descriptions.page(result),
                FromClient::DescriptionSaved { row, result } => self.descriptions.saved(row, result),
                FromClient::DescriptionsChanged(result) => self.descriptions.changed(result),
                FromClient::RewriteTested(result) => {
                    if let Some(form) = &mut self.server.form {
                        form.test.result(result);
                    }
                }
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
        // Spaltenbreiten lassen sich mit der Maus ziehen und bleiben über Neustarts erhalten
        // (je Auswahl an Spalten)
        let columns: Vec<_> = COLUMNS.iter().filter(|c| !self.hidden.iter().any(|h| h == c.0)).collect();
        let line = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        // Werden die Spalten breiter als das Fenster, lässt sich waagerecht scrollen
        egui::ScrollArea::horizontal().show(ui, |ui| {
            let mut table = TableBuilder::new(ui)
                .id_salt(("rows", columns.iter().map(|c| c.0).collect::<Vec<_>>()))
                .striped(true)
                .resizable(true)
                .auto_shrink(false);
            for (_, _, width) in &columns {
                table = table.column(match width {
                    Some(w) => Column::initial(*w).at_least(30.0).clip(true),
                    None => Column::remainder().at_least(100.0),
                });
            }
            table
                .header(line, |mut header| {
                    for (_, label, _) in &columns {
                        header.col(|ui| {
                            ui.strong(*label);
                        });
                    }
                })
                .body(|mut body| {
                    let rows: Vec<&Telegram> = self.rows.get(&active).into_iter().flatten().collect();
                    // Lange Texte werden umgebrochen, die Zeile wächst mit
                    let widths = body.widths().to_vec();
                    let ui = body.ui_mut();
                    let heights: Vec<f32> = rows
                        .iter()
                        .map(|t| {
                            columns.iter().zip(&widths).fold(line, |height, ((key, _, _), width)| {
                                let text = wrapped(t, key).unwrap_or_default();
                                let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Wrap), *width, egui::TextStyle::Body);
                                height.max(galley.size().y + 6.0)
                            })
                        })
                        .collect();
                    body.heterogeneous_rows(heights.into_iter(), |mut row| {
                        let t = rows[row.index()];
                        for (key, _, _) in &columns {
                            row.col(|ui| match *key {
                                "time" => {
                                    ui.label(t.time.format("%d.%m.%Y %H:%M:%S").to_string());
                                }
                                "kind" => {
                                    ui.label(RichText::new(t.kind.as_str().to_uppercase()).strong().color(Color32::WHITE).background_color(kind_color(t.kind)));
                                }
                                "address" => {
                                    ui.monospace(&t.address);
                                }
                                "sub" => {
                                    ui.monospace(&t.sub);
                                }
                                key => {
                                    ui.add(egui::Label::new(wrapped(t, key).unwrap_or_default()).wrap());
                                }
                            });
                        }
                    });
                });
        });
    }

    fn desktop_settings(&mut self, ui: &mut egui::Ui) {
        match self.desktop.toolbar(ui, true) {
            Some(Action::Reload) => {
                self.desktop.clear();
                self.load_desktop();
            }
            Some(Action::Save { restart }) => match self.save_desktop() {
                Ok(()) if restart => boslook::serve::restart(),
                Ok(()) => {
                    self.load_desktop();
                    self.desktop.msg = "Gespeichert. Wirksam nach einem Neustart.".into();
                }
                Err(e) => self.desktop.msg = format!("Nicht gespeichert: {e:#}"),
            },
            None => {}
        }
        if !self.desktop.loaded() {
            self.load_desktop();
        }
        self.desktop.show(ui);
    }

    fn server_settings(&mut self, ui: &mut egui::Ui) {
        match self.server.toolbar(ui, self.connected) {
            Some(Action::Reload) => self.server.clear(),
            Some(Action::Save { restart }) => {
                let cmd = match &self.server.form {
                    Some(form) if !self.server.text_mode => ToClient::SaveServerForm { values: form.values.clone(), restart },
                    _ => ToClient::SaveServerConfig { text: self.server.text.clone(), restart },
                };
                self.send(cmd);
            }
            None => {}
        }
        if self.connected && !self.server.loaded() && !self.server_loading {
            self.server_loading = true;
            self.send(if self.server.text_mode { ToClient::LoadServerConfig } else { ToClient::LoadServerForm });
        }
        self.server.show(ui);
        if let Some(body) = self.server.form.as_mut().and_then(|f| f.test.request.take()) {
            self.send(ToClient::TestRewrite(body));
        }
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
        let cmd = &self.cmd;
        self.descriptions.ui(ctx, &mut |c| {
            if let Some(tx) = cmd {
                let _ = tx.send(c);
            }
        });
        // Mit Infobereich nur verstecken, damit Ereignisse weiter ausgeführt werden
        if ctx.input(|i| i.viewport().close_requested()) && self.tray.is_some() && !self.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, HIDDEN, &self.hidden);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Bos").strong());
                // Blau des Logos, im dunklen Design heller für besseren Kontrast
                let blue = if ui.visuals().dark_mode { Color32::from_rgb(0x4a, 0x9b, 0xe0) } else { Color32::from_rgb(0x00, 0x57, 0x95) };
                ui.heading(RichText::new("Look").strong().color(blue));
                ui.separator();
                ui.selectable_value(&mut self.view, View::Telegrams, "Telegramme");
                ui.selectable_value(&mut self.view, View::Desktop, "Einstellungen");
                ui.selectable_value(&mut self.view, View::Server, "Server-Konfiguration");
                ui.separator();
                if ui.button("Beschreibungen").clicked() {
                    self.descriptions.show(ui.ctx());
                }
                if let Some(url) = &self.server_url
                    && ui.button("Plugins").on_hover_text("Öffnet die Seite im Browser").clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(format!("{url}/plugins")));
                }
                if let Some(url) = &self.server_url {
                    for name in &self.plugin_pages {
                        if ui.button(name).on_hover_text("Seite des Plugins im Browser").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(format!("{url}/plugins/{name}/")));
                        }
                    }
                }
                if self.view == View::Telegrams {
                    // Offen lassen, damit mehrere Spalten auf einmal umgeschaltet werden können
                    let config = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                    egui::containers::menu::MenuButton::new("Spalten").config(config).ui(ui, |ui| {
                        for (key, label, _) in COLUMNS {
                            let mut on = !self.hidden.iter().any(|h| h == key);
                            if ui.checkbox(&mut on, label).changed() {
                                self.hidden.retain(|h| h != key);
                                if !on {
                                    self.hidden.push(key.to_string());
                                }
                            }
                        }
                    });
                }
            });
        });
        // Statuszeile unten, damit lange Fehlermeldungen umbrechen statt die Reiter zu überdecken
        egui::Panel::bottom("status").show(ui, |ui| {
            let color = if self.connected { Color32::from_rgb(0x43, 0xa0, 0x47) } else { RED };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("●").color(color));
                ui.label(&self.status);
            });
        });
        egui::CentralPanel::default_margins().show(ui, |ui| match self.view {
            View::Telegrams => self.telegrams(ui),
            View::Desktop => self.desktop_settings(ui),
            View::Server => self.server_settings(ui),
        });
    }
}

enum Action {
    Reload,
    Save { restart: bool },
}

/// Konfiguration als Formular, umschaltbar auf Text (als Rückfallebene)
#[derive(Default)]
struct Editor {
    text_mode: bool,
    text: String,
    /// Geladener Text, `None` = noch nicht geladen
    original: Option<String>,
    form: Option<form::Form>,
    msg: String,
}

impl Editor {
    fn set_text(&mut self, text: String) {
        self.original = Some(text.clone());
        self.text = text;
    }

    fn loaded(&self) -> bool {
        if self.text_mode { self.original.is_some() } else { self.form.is_some() }
    }

    fn changed(&self) -> bool {
        if self.text_mode { self.original.as_ref().is_some_and(|o| *o != self.text) } else { self.form.as_ref().is_some_and(form::Form::changed) }
    }

    fn clear(&mut self) {
        self.original = None;
        self.form = None;
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, enabled: bool) -> Option<Action> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(enabled, |ui| {
                if ui.button("Neu laden").clicked() {
                    self.msg.clear();
                    action = Some(Action::Reload);
                }
                for (label, restart) in [("Speichern", false), ("Speichern und neu starten", true)] {
                    if ui.add_enabled(self.loaded(), egui::Button::new(label)).clicked() {
                        action = Some(Action::Save { restart });
                    }
                }
                let label = if self.text_mode { "Als Formular bearbeiten" } else { "Als Text bearbeiten" };
                let toggle = ui.add_enabled(!self.changed(), egui::Button::new(label)).on_disabled_hover_text("Erst speichern oder neu laden");
                if toggle.clicked() {
                    self.text_mode = !self.text_mode;
                    self.msg.clear();
                    self.clear();
                }
            });
        });
        action
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        if !self.msg.is_empty() {
            ui.label(RichText::new(&self.msg).monospace());
        }
        if !self.loaded() {
            ui.label("lädt …");
            return;
        }
        match &mut self.form {
            Some(form) if !self.text_mode => {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| form.show(ui));
            }
            _ => editor(ui, &mut self.text),
        }
    }
}

fn editor(ui: &mut egui::Ui, text: &mut String) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add(egui::TextEdit::multiline(text).code_editor().desired_width(f32::INFINITY).desired_rows(30));
    });
}

/// Text der umbrechenden Spalten
fn wrapped<'a>(t: &'a Telegram, key: &str) -> Option<&'a str> {
    match key {
        "message" => Some(&t.message),
        "ort" => Some(&t.ort),
        "kurz" => Some(&t.kurz),
        "lang" => Some(&t.lang),
        _ => None,
    }
}

fn kind_color(kind: Kind) -> Color32 {
    match kind {
        Kind::Pocsag => RED,
        Kind::Fms => Color32::from_rgb(0x15, 0x65, 0xc0),
        Kind::Zvei => Color32::from_rgb(0x2e, 0x7d, 0x32),
        Kind::Wetter => Color32::from_rgb(0xef, 0x6c, 0x00),
    }
}

/// Symbol für Fenster und Infobereich (aus images/icon.svg erzeugt)
fn icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("icon.png")).expect("icon.png")
}

/// Unter Wayland kann winit Fenster weder verstecken noch im Vordergrund halten.
/// Deshalb unter Linux X11 nutzen (unter Wayland über XWayland), sofern vorhanden.
fn options(viewport: egui::ViewportBuilder) -> eframe::NativeOptions {
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions { viewport: viewport.with_icon(icon()), ..Default::default() };
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_some() {
        use winit::platform::x11::EventLoopBuilderExtX11;
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_x11();
        }));
    }
    options
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
    let icon = icon();
    let tray = tray_icon::Icon::from_rgba(icon.rgba, icon.width, icon.height).ok().and_then(|icon| {
        tray_icon::TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("BosLook").with_icon(icon).build().ok()
    });
    (tray, rx)
}

/// Alarmfenster im Vordergrund, bleibt bis zum Quittieren.
fn popup(title: String, body: String, emergency: bool) -> eframe::Result {
    let options = options(egui::ViewportBuilder::default().with_title(format!("BosLook – {title}")).with_inner_size([520.0, 300.0]).with_always_on_top());
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
                ui.label(RichText::new("NOTFALL").size(16.0).strong().color(RED));
            }
            ui.label(RichText::new(&self.title).size(26.0).strong().color(RED));
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
