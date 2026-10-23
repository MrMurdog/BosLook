//! Formular für eine Konfiguration nach dem Schema aus `boslook::form` (als JSON, damit auch
//! das Schema des Servers genutzt werden kann). Die Werte werden direkt im JSON bearbeitet.

use eframe::egui::{self, Color32, RichText};
use serde_json::{Value, json};

pub struct Form {
    pub schema: Value,
    pub values: Value,
    /// Stand beim Laden, zum Erkennen ungespeicherter Änderungen
    original: Value,
    /// Soundkarten des Servers als (ID, Beschreibung)
    devices: Vec<(String, String)>,
    /// Testfeld unter den Bearbeitungsregeln (nur Serverkonfiguration)
    pub test: RewriteTest,
}

pub struct RewriteTest {
    kind: String,
    source: String,
    address: String,
    sub: String,
    message: String,
    /// Anfrage an `/api/rewrite/test`, die das Hauptfenster abholt und sendet
    pub request: Option<Value>,
    result: Option<Result<String, String>>,
}

impl Default for RewriteTest {
    fn default() -> RewriteTest {
        RewriteTest { kind: "pocsag".into(), source: String::new(), address: String::new(), sub: "a".into(), message: String::new(), request: None, result: None }
    }
}

impl RewriteTest {
    /// Antwort des Servers
    pub fn result(&mut self, result: Result<Value, String>) {
        self.result = Some(result.map(|r| {
            let t = &r["telegram"];
            let mut text = format!("→ {}{}: {}", str(&t["address"]), str(&t["sub"]), str(&t["message"]));
            let described: Vec<&str> = ["ort", "kurz", "lang"].iter().map(|k| str(&t[*k])).filter(|s| !s.is_empty()).collect();
            if !described.is_empty() {
                text += &format!("\n{}", described.join(" · "));
            }
            match str(&r["applied"]) {
                "" => text + "\nKeine Regel passt",
                applied => text + &format!("\nRegeln: {applied}"),
            }
        }));
    }

    fn show(&mut self, ui: &mut egui::Ui, kinds: &Value, rules: &Value) {
        let mut run = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Telegramm testen");
            run |= ui.add(egui::TextEdit::singleline(&mut self.source).hint_text("Kanal").desired_width(90.0)).changed();
            egui::ComboBox::from_id_salt("test-kind").selected_text(self.kind.to_uppercase()).show_ui(ui, |ui| {
                for k in kinds.as_array().into_iter().flatten().map(str) {
                    run |= ui.selectable_value(&mut self.kind, k.to_string(), k.to_uppercase()).clicked();
                }
            });
            run |= ui.add(egui::TextEdit::singleline(&mut self.address).hint_text("Adresse").desired_width(90.0)).changed();
            egui::ComboBox::from_id_salt("test-sub").selected_text(&self.sub).width(40.0).show_ui(ui, |ui| {
                for x in ["a", "b", "c", "d"] {
                    run |= ui.selectable_value(&mut self.sub, x.to_string(), x).clicked();
                }
            });
            run |= ui.add(egui::TextEdit::singleline(&mut self.message).hint_text("Meldung").desired_width(320.0)).changed();
            run |= ui.button("Testen").clicked();
        });
        match &self.result {
            Some(Ok(text)) => {
                ui.label(RichText::new(text).monospace());
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(crate::RED));
            }
            None => {}
        }
        if run && !self.address.trim().is_empty() {
            self.request = Some(json!({ "rules": rules, "kind": self.kind, "source": self.source, "address": self.address, "sub": self.sub, "message": self.message }));
        }
    }
}

impl Form {
    pub fn new(schema: Value, mut values: Value, devices: Vec<(String, String)>) -> Form {
        // Fehlende Felder und Listen gleich anlegen; das Formular würde sie beim Zeichnen
        // ergänzen und sonst als Änderung werten
        for section in schema.as_array().into_iter().flatten() {
            let key = str(&section["key"]);
            let fields = &section["fields"];
            match str(&section["kind"]) {
                "root" => complete(fields, &mut values),
                "table" if values[key].is_null() => values[key] = new_item(fields),
                "table" | "optional" if values[key].is_object() => complete(fields, &mut values[key]),
                "list" if !values[key].is_array() => values[key] = json!([]),
                "list" => values[key].as_array_mut().into_iter().flatten().for_each(|item| complete(fields, item)),
                _ => {}
            }
        }
        Form { schema, original: values.clone(), values, devices, test: RewriteTest::default() }
    }

    /// Antwort von `/api/config/form`
    pub fn from_server(mut form: Value) -> Form {
        let devices = form["devices"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| (str(&d["id"]).to_string(), str(&d["label"]).replace('\t', ", ")))
            .collect();
        Form::new(form["schema"].take(), form["values"].take(), devices)
    }

    pub fn changed(&self) -> bool {
        self.values != self.original
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let Form { schema, values, devices, test, .. } = self;
        let all = values.clone();
        for section in schema.as_array().into_iter().flatten() {
            let key = str(&section["key"]);
            egui::CollapsingHeader::new(RichText::new(str(&section["title"])).strong()).id_salt(key).default_open(true).show(ui, |ui| {
                help(ui, &section["help"]);
                let fields = &section["fields"];
                let ctx = Ctx { all: &all, devices };
                match str(&section["kind"]) {
                    "root" => ctx.fields(ui, key, fields, values),
                    "table" => {
                        if values[key].is_null() {
                            values[key] = new_item(fields);
                        }
                        ctx.fields(ui, key, fields, &mut values[key]);
                    }
                    "optional" => {
                        let mut on = !values[key].is_null();
                        if ui.checkbox(&mut on, "aktiv").changed() {
                            values[key] = if on { new_item(fields) } else { Value::Null };
                        }
                        if on {
                            ctx.fields(ui, key, fields, &mut values[key]);
                        }
                    }
                    _ => {
                        if !values[key].is_array() {
                            values[key] = json!([]);
                        }
                        let list = values[key].as_array_mut().expect("Liste");
                        let mut remove = None;
                        for (i, item) in list.iter_mut().enumerate() {
                            ui.separator();
                            ui.push_id(i, |ui| ctx.fields(ui, key, fields, item));
                            if ui.button("Entfernen").clicked() {
                                remove = Some(i);
                            }
                        }
                        if let Some(i) = remove {
                            list.remove(i);
                        }
                        ui.separator();
                        if ui.button("+ Hinzufügen").clicked() {
                            list.push(new_item(fields));
                        }
                        if key == "rewrite" {
                            ui.separator();
                            let kinds = fields.as_array().into_iter().flatten().find(|f| f["key"] == "kind").map_or(&Value::Null, |f| &f["input"]["options"]);
                            test.show(ui, kinds, &values[key]);
                        }
                    }
                }
            });
        }
    }
}

struct Ctx<'a> {
    /// Alle Werte vor diesem Bild, für die Namen anderer Abschnitte
    all: &'a Value,
    devices: &'a [(String, String)],
}

impl Ctx<'_> {
    fn fields(&self, ui: &mut egui::Ui, section: &str, fields: &Value, item: &mut Value) {
        egui::Grid::new(section).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            for f in fields.as_array().into_iter().flatten() {
                let when = &f["when"];
                if when.is_array() && !when[1].as_array().is_some_and(|v| v.contains(&item[str(&when[0])])) {
                    continue;
                }
                let mut label = RichText::new(str(&f["label"]));
                if f["required"] == true {
                    label = RichText::new(format!("{} *", str(&f["label"])));
                }
                ui.label(label);
                ui.vertical(|ui| {
                    ui.set_max_width(420.0);
                    self.input(ui, f, &mut item[str(&f["key"])]);
                    help(ui, &f["help"]);
                });
                ui.end_row();
            }
        });
    }

    fn input(&self, ui: &mut egui::Ui, f: &Value, value: &mut Value) {
        let key = str(&f["key"]);
        let options: Vec<String> = f["input"]["options"].as_array().into_iter().flatten().map(|o| str(o).to_string()).collect();
        match str(&f["input"]["type"]) {
            "bool" => {
                let mut on = value.as_bool().unwrap_or(false);
                if ui.checkbox(&mut on, "").changed() {
                    *value = on.into();
                }
            }
            kind @ ("select" | "device") => {
                let mut choices: Vec<(String, String)> = if kind == "device" {
                    let mut c = vec![(String::new(), "Standardgerät".to_string())];
                    c.extend(self.devices.iter().map(|(id, label)| (id.clone(), format!("{id} – {label}"))));
                    c
                } else {
                    options.iter().map(|o| (o.clone(), o.clone())).collect()
                };
                if kind == "select" && f["required"] != true && f["default"].is_null() {
                    choices.insert(0, (String::new(), "–".into()));
                }
                let current = str(value).to_string();
                if !current.is_empty() && !choices.iter().any(|(o, _)| *o == current) {
                    choices.push((current.clone(), current.clone()));
                }
                let text = choices.iter().find(|(o, _)| *o == current).map(|(_, t)| t.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt(key).selected_text(text).width(ui.available_width()).show_ui(ui, |ui| {
                    for (o, t) in choices {
                        if ui.selectable_label(o == current, t).clicked() {
                            *value = o.into();
                        }
                    }
                });
            }
            kind @ ("choices" | "names") => {
                let mut options = options;
                if kind == "names" {
                    let names = self.all[options.first().map(String::as_str).unwrap_or_default()].as_array();
                    options = names.into_iter().flatten().map(|n| str(&n["name"]).to_string()).filter(|n| !n.is_empty()).collect();
                }
                let mut selected: Vec<String> = value.as_array().into_iter().flatten().map(|v| str(v).to_string()).collect();
                for v in &selected {
                    if !options.contains(v) {
                        options.push(v.clone());
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    for o in &options {
                        let mut on = selected.contains(o);
                        if ui.checkbox(&mut on, o).changed() {
                            selected = options.iter().filter(|x| if *x == o { on } else { selected.contains(x) }).cloned().collect();
                            *value = selected.clone().into();
                        }
                    }
                });
            }
            "pairs" => {
                // Erst beim Hinzufügen eine Liste anlegen, sonst gälte schon das Zeichnen als Änderung
                let mut remove = None;
                for (i, item) in value.as_array_mut().into_iter().flatten().enumerate() {
                    ui.push_id(i, |ui| {
                        ui.horizontal(|ui| {
                            let width = (ui.available_width() - 40.0) / options.len() as f32;
                            for (n, label) in f["input"]["options"].as_array().into_iter().flatten().enumerate() {
                                let k = str(&label[0]);
                                let mut text = str(&item[k]).to_string();
                                let edit = egui::TextEdit::singleline(&mut text).hint_text(str(&label[1])).font(egui::TextStyle::Monospace).desired_width(width);
                                if ui.push_id(n, |ui| ui.add(edit)).inner.changed() {
                                    item[k] = text.into();
                                }
                            }
                            if ui.button("×").on_hover_text("Entfernen").clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                }
                if let (Some(i), Some(list)) = (remove, value.as_array_mut()) {
                    list.remove(i);
                }
                if ui.button("+").on_hover_text("Hinzufügen").clicked() {
                    if !value.is_array() {
                        *value = json!([]);
                    }
                    value.as_array_mut().expect("Liste").push(json!({}));
                }
            }
            "group" => {
                // Wie bei „pairs“ erst beim Hinzufügen eine Liste anlegen
                let fields = &f["input"]["options"];
                let mut remove = None;
                for (i, item) in value.as_array_mut().into_iter().flatten().enumerate() {
                    ui.push_id(i, |ui| {
                        ui.group(|ui| {
                            self.fields(ui, key, fields, item);
                            if ui.button("Entfernen").clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                }
                if let (Some(i), Some(list)) = (remove, value.as_array_mut()) {
                    list.remove(i);
                }
                if ui.button("+ Hinzufügen").clicked() {
                    if !value.is_array() {
                        *value = json!([]);
                    }
                    value.as_array_mut().expect("Liste").push(new_item(fields));
                }
            }
            "lines" | "multiline" => {
                let mut text = match &*value {
                    Value::Array(a) => a.iter().map(str).collect::<Vec<_>>().join("\n"),
                    v => str(v).to_string(),
                };
                if ui.add(egui::TextEdit::multiline(&mut text).desired_rows(3).desired_width(f32::INFINITY)).changed() {
                    *value = text.into();
                }
            }
            "secret" => {
                // Gespeichert ist nur der Hash: leer lassen = unverändert
                let hint = if value.is_string() { "unverändert" } else { "neu festlegen" };
                let mut text = str(&value["new"]).to_string();
                let edit = egui::TextEdit::singleline(&mut text).password(true).hint_text(hint).desired_width(f32::INFINITY);
                if ui.add(edit).changed() {
                    *value = json!({ "new": text });
                }
            }
            kind => {
                let mut text = match &*value {
                    Value::Number(n) => n.to_string(),
                    v => str(v).to_string(),
                };
                let edit = egui::TextEdit::singleline(&mut text).password(kind == "password").desired_width(f32::INFINITY);
                if ui.add(edit).changed() {
                    *value = text.into();
                }
            }
        }
    }
}

fn complete(fields: &Value, item: &mut Value) {
    for f in fields.as_array().into_iter().flatten() {
        if let Some(obj) = item.as_object_mut() {
            obj.entry(str(&f["key"])).or_insert(Value::Null);
        }
        if f["input"]["type"] == "group" {
            for entry in item.get_mut(str(&f["key"])).and_then(Value::as_array_mut).into_iter().flatten() {
                complete(&f["input"]["options"], entry);
            }
        }
    }
}

/// Neuer Listeneintrag mit Standardwerten; Auswahlfelder ohne Standard nehmen den ersten Wert
fn new_item(fields: &Value) -> Value {
    let mut item = json!({});
    complete(fields, &mut item);
    for f in fields.as_array().into_iter().flatten() {
        let key = str(&f["key"]);
        if !f["default"].is_null() {
            item[key] = f["default"].clone();
        } else if f["required"] == true && f["input"]["type"] == "select" {
            item[key] = f["input"]["options"][0].clone();
        }
    }
    item
}

fn help(ui: &mut egui::Ui, help: &Value) {
    if !str(help).is_empty() {
        ui.label(RichText::new(str(help)).small().color(Color32::GRAY));
    }
}

fn str(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}
