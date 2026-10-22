//! Eigenes Fenster zum Bearbeiten der Beschreibungen (Ort, Kurz- und Langbeschreibung je Adresse).

use crate::client::ToClient;
use boslook::descriptions::{Description, Page, Search};
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use std::time::{Duration, Instant};

const PAGE: usize = 100;
/// So lange nach der letzten Eingabe in der Suche warten
const DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Default)]
pub struct Window {
    pub open: bool,
    search: Search,
    /// Suche geändert, wird nach `DEBOUNCE` gesendet
    typed: Option<Instant>,
    /// Seite muss (neu) geladen werden
    reload: bool,
    rows: Vec<Row>,
    total: usize,
    msg: String,
    error: bool,
    clear: bool,
    next_id: u64,
}

struct Row {
    /// Zum Zuordnen der Antwort beim Speichern
    id: u64,
    /// Gespeicherte Adresse, `None` bei neuen Zeilen
    old: Option<String>,
    d: Description,
    edited: bool,
    bad: bool,
    confirm_delete: bool,
}

impl Window {
    pub fn show(&mut self, ctx: &egui::Context) {
        self.open = true;
        self.reload = true;
        ctx.send_viewport_cmd_to(viewport(), egui::ViewportCommand::Focus);
    }

    fn row(&mut self, d: Description, old: Option<String>) -> Row {
        self.next_id += 1;
        Row { id: self.next_id, old, d, edited: false, bad: false, confirm_delete: false }
    }

    fn message(&mut self, text: impl Into<String>, error: bool) {
        (self.msg, self.error) = (text.into(), error);
    }

    pub fn page(&mut self, result: Result<Page, String>) {
        match result {
            Ok(page) => {
                self.total = page.total;
                self.rows = page.rows.into_iter().map(|d| self.row(d.clone(), Some(d.address))).collect();
                let notice = page.notice.unwrap_or_default();
                let error = !notice.is_empty();
                if error || self.error {
                    self.message(notice, error);
                }
            }
            Err(e) => self.message(e, true),
        }
    }

    pub fn saved(&mut self, id: u64, result: Result<Description, String>) {
        let Some(row) = self.rows.iter_mut().find(|r| r.id == id) else { return };
        match result {
            Ok(d) => {
                if row.old.is_none() {
                    self.total += 1;
                }
                (row.old, row.d, row.bad) = (Some(d.address.clone()), d, false);
                let text = format!("{} gespeichert", row.d.address);
                self.message(text, false);
            }
            Err(e) => {
                row.bad = true;
                self.message(e, true);
            }
        }
    }

    pub fn changed(&mut self, result: Result<String, String>) {
        match result {
            Ok(text) => {
                let error = text.contains("fehlerhaft");
                self.message(text, error);
                self.reload = true;
            }
            Err(e) => self.message(e, true),
        }
    }

    /// Zeichnet das Fenster, solange es offen ist; Anfragen an den Server gehen über `send`.
    pub fn ui(&mut self, ctx: &egui::Context, send: &mut impl FnMut(ToClient)) {
        if !self.open {
            return;
        }
        if self.typed.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
            self.typed = None;
            self.search.offset = 0;
            self.reload = true;
        }
        if let Some(t) = self.typed {
            ctx.request_repaint_after(DEBOUNCE.saturating_sub(t.elapsed()));
        }
        if std::mem::take(&mut self.reload) {
            send(ToClient::SearchDescriptions(Search { limit: Some(PAGE), ..self.search.clone() }));
        }
        let builder = egui::ViewportBuilder::default().with_title("BosLook – Beschreibungen").with_inner_size([1000.0, 650.0]);
        ctx.show_viewport_immediate(viewport(), builder, |ui, _| {
            if ui.input(|i| i.viewport().close_requested()) {
                self.open = false;
            }
            egui::Panel::top("descriptions-top").show(ui, |ui| self.toolbar(ui, send));
            egui::CentralPanel::default_margins().show(ui, |ui| self.table(ui, send));
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, send: &mut impl FnMut(ToClient)) {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let search = egui::TextEdit::singleline(&mut self.search.q)
                .hint_text("Suchen (regulärer Ausdruck, z. B. ^19388 oder FW|RD)")
                .desired_width(320.0);
            if ui.add(search).changed() {
                self.typed = Some(Instant::now());
            }
            if ui.button("+ Neu").clicked() {
                let row = self.row(Description { address: String::new(), ort: String::new(), kurz: String::new(), lang: String::new() }, None);
                self.rows.insert(0, row);
            }
            ui.separator();
            ui.checkbox(&mut self.clear, "Vorher alles löschen");
            if ui.button("Importieren …").on_hover_text("CSV im eigenen Format (mit Kopfzeile) oder aus BosMon (ohne Kopfzeile)").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv", "txt"]).pick_file()
            {
                self.message("Importiere …", false);
                send(ToClient::ImportDescriptions { path, clear: self.clear });
            }
            if ui.button("Exportieren …").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("beschreibungen.csv").save_file()
            {
                send(ToClient::ExportDescriptions(path));
            }
        });
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(self.search.offset > 0, egui::Button::new("‹")).clicked() {
                self.search.offset = self.search.offset.saturating_sub(PAGE);
                self.reload = true;
            }
            let from = if self.total > 0 { self.search.offset + 1 } else { 0 };
            let to = (self.search.offset + PAGE).min(self.total);
            ui.label(format!("{}–{} von {}", number(from), number(to), number(self.total)));
            if ui.add_enabled(to < self.total, egui::Button::new("›")).clicked() {
                self.search.offset += PAGE;
                self.reload = true;
            }
            if !self.msg.is_empty() {
                let color = if self.error { crate::RED } else { ui.visuals().weak_text_color() };
                ui.label(RichText::new(&self.msg).color(color));
            }
        });
        ui.add_space(4.0);
    }

    fn table(&mut self, ui: &mut egui::Ui, send: &mut impl FnMut(ToClient)) {
        let line = ui.text_style_height(&egui::TextStyle::Body) + 10.0;
        let mut sort = None;
        let mut remove = None;
        TableBuilder::new(ui)
            .id_salt("descriptions")
            .striped(true)
            .resizable(true)
            .auto_shrink(false)
            .column(Column::initial(100.0).at_least(80.0))
            .column(Column::initial(200.0).at_least(60.0).clip(true))
            .column(Column::initial(200.0).at_least(60.0).clip(true))
            .column(Column::remainder().at_least(100.0).clip(true))
            .column(Column::exact(110.0))
            .header(line, |mut header| {
                for (key, label) in [("address", "Adresse"), ("ort", "Ort"), ("kurz", "Kurzbeschreibung"), ("lang", "Langbeschreibung")] {
                    header.col(|ui| {
                        let current = self.search.sort == key || (key == "address" && self.search.sort.is_empty());
                        let arrow = match (current, self.search.desc) {
                            (false, _) => "",
                            (true, false) => " ▲",
                            (true, true) => " ▼",
                        };
                        if ui.add(egui::Button::new(RichText::new(format!("{label}{arrow}")).strong()).frame(false)).clicked() {
                            sort = Some((key, current));
                        }
                    });
                }
                header.col(|_| {});
            })
            .body(|body| {
                body.rows(line, self.rows.len(), |mut table_row| {
                    let row = &mut self.rows[table_row.index()];
                    let mut save = false;
                    for (i, value) in [&mut row.d.address, &mut row.d.ort, &mut row.d.kurz, &mut row.d.lang].into_iter().enumerate() {
                        table_row.col(|ui| {
                            let mut edit = egui::TextEdit::singleline(value).desired_width(f32::INFINITY);
                            if i == 0 {
                                edit = edit.font(egui::TextStyle::Monospace).hint_text("1234567a");
                                if row.bad {
                                    edit = edit.text_color(crate::RED);
                                }
                            }
                            let response = ui.add(edit);
                            row.edited |= response.changed();
                            save |= response.lost_focus();
                        });
                    }
                    table_row.col(|ui| {
                        if row.confirm_delete {
                            if ui.button(RichText::new("Wirklich?").color(crate::RED)).clicked() {
                                remove = Some(row.id);
                            }
                        } else if ui.button("Löschen").clicked() {
                            row.confirm_delete = true;
                        }
                    });
                    if save && row.edited && !row.d.address.trim().is_empty() {
                        row.edited = false;
                        send(ToClient::SaveDescription { row: row.id, old: row.old.clone(), description: row.d.clone() });
                    }
                });
            });
        if let Some((key, current)) = sort {
            self.search.desc = current && !self.search.desc;
            self.search.sort = key.to_string();
            self.search.offset = 0;
            self.reload = true;
        }
        if let Some(id) = remove {
            let index = self.rows.iter().position(|r| r.id == id).expect("Zeile");
            if let Some(address) = self.rows.remove(index).old {
                send(ToClient::DeleteDescription(address));
            }
        }
    }
}

fn viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("descriptions")
}

/// 85397 → „85.397“
fn number(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    out
}
