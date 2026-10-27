//! Formulare für die Konfigurationsdateien.
//!
//! Ein Schema beschreibt Abschnitte und Felder. `to_json` liefert die Werte einer Datei für das
//! Formular, `apply` schreibt die Formularwerte zurück in den Text. Dabei bleiben Kommentare,
//! Reihenfolge und unbekannte Einträge erhalten, nur geänderte Werte werden ersetzt.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

#[derive(Debug, Serialize)]
pub struct Section {
    /// Name der Tabelle in der Datei, leer für Einträge auf oberster Ebene
    pub key: &'static str,
    pub title: &'static str,
    pub help: &'static str,
    pub kind: SectionKind,
    pub fields: Vec<Field>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SectionKind {
    /// Einträge auf oberster Ebene
    Root,
    /// `[tabelle]`, muss vorhanden sein
    Table,
    /// `[tabelle]`, darf fehlen
    Optional,
    /// `[[tabelle]]`, beliebig viele Einträge
    List,
}

#[derive(Debug, Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub input: Input,
    pub required: bool,
    /// Wert, wenn der Eintrag in der Datei fehlt
    pub default: Option<Value>,
    /// Nur vorhanden, wenn das Feld `.0` einen der Werte `.1` hat (z. B. Eingangstyp)
    pub when: Option<(&'static str, Vec<&'static str>)>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", content = "options", rename_all = "lowercase")]
pub enum Input {
    Text,
    /// Klartext, verdeckt angezeigt (Tokens, Passwörter für Dienste)
    Password,
    /// Gespeichert wird nur der Argon2-Hash. Das Formular schickt den alten Hash unverändert
    /// zurück oder `{"new": "Klartext"}`.
    Secret,
    Number,
    Bool,
    Select(Vec<&'static str>),
    /// Liste von Texten, im Formular eine Zeile je Eintrag
    Lines,
    /// Ein Text über mehrere Zeilen (Vorlagen)
    Multiline,
    /// Mehrfachauswahl aus festen Werten
    Choices(Vec<&'static str>),
    /// Mehrfachauswahl aus den Namen der Einträge eines anderen Abschnitts
    Names(&'static str),
    /// Soundkarte des Servers, leer = Standardgerät
    Device,
    /// Liste von Einträgen mit den Textfeldern (Schlüssel, Beschriftung), in der Datei als
    /// `[[abschnitt.feld]]`; Einträge mit leerem ersten Feld fallen weg
    Pairs(Vec<(&'static str, &'static str)>),
    /// Liste von Einträgen mit eigenen Feldern, in der Datei als `[[abschnitt.feld]]`
    /// (z. B. die Kanäle eines Plugins)
    Group(Vec<Field>),
}

pub fn field(key: &'static str, label: &'static str, input: Input) -> Field {
    Field { key, label, help: "", input, required: false, default: None, when: None }
}

impl Field {
    pub fn required(mut self) -> Field {
        self.required = true;
        self
    }

    pub fn help(mut self, help: &'static str) -> Field {
        self.help = help;
        self
    }

    pub fn default(mut self, value: impl Into<Value>) -> Field {
        self.default = Some(value.into());
        self
    }

    pub fn when(mut self, key: &'static str, values: &[&'static str]) -> Field {
        self.when = Some((key, values.to_vec()));
        self
    }

    fn active(&self, item: &Value) -> bool {
        self.when.as_ref().is_none_or(|(k, values)| item.get(*k).and_then(Value::as_str).is_some_and(|v| values.contains(&v)))
    }
}

const KINDS: [&str; 4] = ["pocsag", "fms", "zvei", "wetter"];
const PRIORITIES: [&str; 3] = ["normal", "high", "emergency"];

/// Felder, die Filter, Alarme und Desktop-Ereignisse gemeinsam haben
pub fn match_fields() -> Vec<Field> {
    vec![
        field("kind", "Art", Input::Select(KINDS.to_vec())),
        field("source", "Kanal", Input::Text).help("Regulärer Ausdruck auf den Namen des Eingangs (Reiter), z. B. ^SDR$"),
        field("address", "Adresse", Input::Text).help("Regulärer Ausdruck, z. B. ^18599"),
        field("sub", "Sub", Input::Text).help("Regulärer Ausdruck"),
        field("message", "Meldung", Input::Text).help("Regulärer Ausdruck"),
        field("ort", "Ort", Input::Text).help("Regulärer Ausdruck, Ort aus den Beschreibungen"),
        field("kurz", "Kurzbeschreibung", Input::Text).help("Regulärer Ausdruck"),
        field("lang", "Langbeschreibung", Input::Text).help("Regulärer Ausdruck"),
    ]
}

/// Felder eines Eingangs; `device` ist die Eingabe für die Soundkarte
fn input_fields(device: Input) -> Vec<Field> {
    vec![
        field("name", "Name", Input::Text).required(),
        field("type", "Typ", Input::Select(vec!["soundcard", "rtl_fm", "rtl_sdr", "stdin", "wav"]))
            .required()
            .help("rtl_fm: eine Frequenz; rtl_sdr: mehrere Frequenzen innerhalb von 1 MHz mit einem Stick"),
        field("device", "Soundkarte", device).when("type", &["soundcard"]).help("leer = Standardgerät"),
        field("frequency", "Frequenz", Input::Text).required().when("type", &["rtl_fm"]).help("z. B. 173.255M"),
        field("channel", "Kanäle", Input::Group(vec![
            field("name", "Name", Input::Text).required().help("erscheint als eigener Reiter"),
            field("frequency", "Frequenz", Input::Text).required().help("z. B. 173.255M"),
        ]))
        .when("type", &["rtl_sdr"])
        .help("Alle Frequenzen innerhalb von 1 MHz"),
        field("ppm", "Frequenzkorrektur (ppm)", Input::Number).default(0).when("type", &["rtl_fm", "rtl_sdr"]),
        field("gain", "Verstärkung", Input::Text).default("auto").when("type", &["rtl_fm", "rtl_sdr"]).help("in dB, z. B. 38.6, oder auto"),
        // Gleicher Schlüssel wie bei der Soundkarte, darum ohne Standardwert im Formular
        field("device", "Stick", Input::Text).when("type", &["rtl_fm", "rtl_sdr"]).help("Index oder Seriennummer, leer = 0"),
        field("squelch", "Rauschsperre", Input::Number).default(0).when("type", &["rtl_fm"]).help("0 = aus (für Dekoder empfohlen)"),
        field("program", "Programm", Input::Text)
            .when("type", &["rtl_fm", "rtl_sdr"])
            .help("Pfad zu rtl_fm bzw. rtl_sdr, leer = aus dem PATH bzw. neben boslook"),
        field("sample_rate", "Abtastrate (Hz)", Input::Number)
            .required()
            .when("type", &["stdin"])
            .help("Rohe Samples (s16le, mono), z. B. von rtl_fm"),
        field("path", "WAV-Datei", Input::Text).required().when("type", &["wav"]),
        field("decoders", "Dekoder", Input::Choices(crate::decoder::ALL.to_vec())).default(json!(crate::decoder::ALL)),
        field("pocsag", "POCSAG-Darstellung", Input::Select(vec!["alpha", "numeric", "auto"]))
            .default("alpha")
            .help("auto: Sub a als Ziffern, sonst als Text"),
    ]
}

/// Titel und Text einer Benachrichtigung (Alarme und Desktop-Ereignisse)
pub fn template_fields(title_help: &'static str, text_help: &'static str) -> Vec<Field> {
    vec![field("title", "Titel", Input::Text).help(title_help), field("text", "Text", Input::Multiline).help(text_help)]
}

/// Hilfetext mit den Platzhaltern aus [`crate::notify::PLACEHOLDERS`]
pub const TEMPLATE_HELP: &str =
    "Platzhalter: {kind} {address} {sub} {message} {ort} {kurz} {lang} {source} {time} {alarm}; leer = Standardtext";

pub fn priority_field(help: &'static str) -> Field {
    field("priority", "Priorität", Input::Select(PRIORITIES.to_vec())).default("normal").help(help)
}

/// Schema der `boslook.toml` mit den Abschnitten und Benachrichtigungsdiensten der
/// installierten Plugins
pub fn server_schema(plugins: &[crate::plugin::Manifest]) -> Vec<Section> {
    use crate::plugin::Capability;
    let name = || field("name", "Name", Input::Text).required();
    let notifying = || plugins.iter().filter(|m| m.has(Capability::Notify));
    // Fehlerhafte Felder sind schon beim Laden der Beschreibungen aufgefallen
    let plugin_notifier_fields = notifying().flat_map(|m| m.notifier_fields().unwrap_or_default());
    let plugin_sections = plugins.iter().map(|m| Section {
        key: crate::plugin::intern(&format!("plugin.{}", m.name)),
        title: crate::plugin::intern(&format!("Plugin: {}", m.name)),
        help: crate::plugin::intern(&m.description),
        kind: SectionKind::Optional,
        fields: m.section_fields().unwrap_or_default(),
    });
    let mut types = vec!["telegram", "pushover", "email"];
    types.extend(notifying().map(|m| crate::plugin::intern(&m.name)));
    let sections = vec![
        Section {
            key: "",
            title: "Allgemein",
            help: "",
            kind: SectionKind::Root,
            fields: vec![
                field("database", "Datenbank", Input::Text)
                    .default("boslook.db")
                    .help("Dateiname (SQLite) oder mysql://benutzer:passwort@server/datenbank bzw. postgres://…"),
                field("notify_repeat", "Wiederholungen unterdrücken (s)", Input::Number)
                    .default(60)
                    .help("Gleiche Telegramme innerhalb dieser Sekunden nur einmal benachrichtigen, 0 = aus"),
            ],
        },
        Section {
            key: "web",
            title: "Weboberfläche",
            help: "",
            kind: SectionKind::Table,
            fields: vec![
                field("listen", "Adresse", Input::Text).required().help("z. B. 0.0.0.0:8443"),
                field("tls", "TLS", Input::Select(vec!["off", "self-signed", "files"]))
                    .default("off")
                    .help("self-signed: Zertifikat wird beim ersten Start erstellt; files: vorhandenes Zertifikat"),
                field("cert", "Zertifikat", Input::Text).default("boslook-cert.pem").when("tls", &["self-signed", "files"]),
                field("key", "Schlüssel", Input::Text).default("boslook-key.pem").when("tls", &["self-signed", "files"]),
            ],
        },
        Section {
            key: "user",
            title: "Benutzer",
            help: "Anmeldung an Weboberfläche und Desktop-App",
            kind: SectionKind::List,
            fields: vec![name(), field("password_hash", "Passwort", Input::Secret).required()],
        },
        Section {
            key: "input",
            title: "Eingänge",
            help: "",
            kind: SectionKind::List,
            fields: input_fields(Input::Device),
        },
        Section {
            key: "rewrite",
            title: "Bearbeitung",
            help: "Passende Regeln ändern das Telegramm der Reihe nach, bevor gefiltert und gespeichert wird.",
            kind: SectionKind::List,
            fields: [
                vec![name()],
                match_fields(),
                vec![
                    field("replace", "Ersetzungen", Input::Pairs(vec![("pattern", "Suchen (regulärer Ausdruck)"), ("with", "Ersetzen durch")]))
                        .help("In der Meldung, nacheinander; $1, $2 … = Klammern aus dem Suchausdruck"),
                    field("address_add", "RIC addieren", Input::Number).help("Nur POCSAG, z. B. 1000 oder -1000"),
                    field("stop", "Danach keine weiteren Regeln", Input::Bool).default(false),
                ],
            ]
            .into_iter()
            .flatten()
            .collect(),
        },
        Section {
            key: "filter",
            title: "Filter",
            help: "Ein Filter mit Kanal gilt nur für diesen Kanal, einer ohne für alle. Passt ein Filter mit „Ausschließen“, wird das Telegramm verworfen. Gelten für den Kanal andere Filter, muss einer davon passen.",
            kind: SectionKind::List,
            fields: [vec![name()], match_fields(), vec![field("exclude", "Ausschließen", Input::Bool).default(false)]].into_iter().flatten().collect(),
        },
        Section {
            key: "notifier",
            title: "Benachrichtigungsziele",
            help: "",
            kind: SectionKind::List,
            fields: vec![
                name(),
                field("type", "Dienst", Input::Select(types)).required(),
                field("bot_token", "Bot-Token", Input::Password).required().when("type", &["telegram"]).help("Bot über @BotFather anlegen"),
                field("chat_id", "Chat-ID", Input::Text).required().when("type", &["telegram"]),
                field("token", "App-Token", Input::Password).required().when("type", &["pushover"]),
                field("user", "User-/Gruppen-Key", Input::Text).required().when("type", &["pushover"]),
                field("server", "SMTP-Server", Input::Text).required().when("type", &["email"]),
                field("security", "Verschlüsselung", Input::Select(vec!["starttls", "tls", "none"])).default("starttls").when("type", &["email"]),
                field("port", "Port", Input::Number).when("type", &["email"]).help("leer: 587 bei starttls, 465 bei tls, 25 bei none"),
                field("username", "Benutzer", Input::Text).when("type", &["email"]),
                field("password", "Passwort", Input::Password).when("type", &["email"]),
                field("from", "Absender", Input::Text).required().when("type", &["email"]),
                field("to", "Empfänger", Input::Lines).required().when("type", &["email"]).help("Eine Adresse je Zeile"),
            ]
            .into_iter()
            .chain(plugin_notifier_fields)
            .collect(),
        },
        Section {
            key: "alarm",
            title: "Alarme",
            help: "Benachrichtigt die gewählten Ziele bei passenden Telegrammen",
            kind: SectionKind::List,
            fields: [
                vec![name()],
                match_fields(),
                vec![
                    field("notify", "Ziele", Input::Names("notifier")).required(),
                    priority_field("Pushover: high = Ton auch bei „Nicht stören“, emergency = wiederholt, bis bestätigt"),
                ],
                template_fields("leer = Namen der Alarme ({alarm})", TEMPLATE_HELP),
            ]
            .into_iter()
            .flatten()
            .collect(),
        },
        Section {
            key: "weather",
            title: "Wetterwarnungen (DWD)",
            help: "",
            kind: SectionKind::Optional,
            fields: vec![
                field("warncells", "Warncell-IDs", Input::Lines).required().help("Eine ID je Zeile, suchen mit: boslook warncell <Ort>"),
                field("interval", "Abfrageintervall (s)", Input::Number).default(300),
            ],
        },
        Section {
            key: "client",
            title: "Clients",
            help: "BosLook-Clients, die Telegramme an diesen Server weiterleiten dürfen. Einrichten per Code auf der Seite „Clients“.",
            kind: SectionKind::List,
            fields: vec![
                name(),
                field("token_hash", "Token", Input::Secret).help("Leer lassen und auf der Seite „Clients“ einen Einrichtungscode erzeugen"),
                field("remote", "Fernkonfiguration", Input::Bool)
                    .default(false)
                    .help("Der Client nimmt die Eingänge von hier statt seiner eigenen; Änderungen gelten sofort nach dem Speichern"),
                // Soundkarten des Clients kennt der Server nicht, darum als Text
                field("input", "Eingänge", Input::Group(input_fields(Input::Text))).help("Nur mit Fernkonfiguration"),
            ],
        },
        Section {
            key: "forward",
            title: "Weiterleitung an einen Server",
            help: "Telegramme dieses BosLook an einen anderen weiterleiten",
            kind: SectionKind::Optional,
            fields: vec![
                field("server", "Server", Input::Text).required().help("z. B. https://mein-server.de:8443"),
                field("name", "Name", Input::Text).required().help("wie beim Server unter Clients eingetragen"),
                field("token", "Token", Input::Password).required(),
                field("cert", "Zertifikat des Servers", Input::Text).help("nur bei tls = self-signed auf dem Server"),
            ],
        },
    ];
    sections.into_iter().chain(plugin_sections).collect()
}

/// `plugin.name` → (`Some("plugin")`, `"name"`)
fn split_key(key: &str) -> (Option<&str>, &str) {
    match key.split_once('.') {
        Some((parent, key)) => (Some(parent), key),
        None => (None, key),
    }
}

/// Werte der Datei für das Formular. Fehlende Felder bekommen ihren Standardwert,
/// Listeneinträge ihre Position in der Datei (`_i`), damit `apply` sie wiederfindet.
pub fn to_json(text: &str, schema: &[Section]) -> Result<Value> {
    let mut values: Value = toml::from_str(text)?;
    for s in schema {
        let defaults = |item: &mut Value| defaults(&s.fields, item);
        match s.kind {
            SectionKind::Root => defaults(&mut values),
            SectionKind::Table | SectionKind::Optional => {
                // Unterabschnitte wie `[plugin.name]` stehen im Formular unter ihrem vollen Namen
                if let (Some(parent), key) = split_key(s.key)
                    && let Some(table) = values.get(parent).and_then(|p| p.get(key)).cloned()
                {
                    values[s.key] = table;
                }
                if let Some(table) = values.get_mut(s.key) {
                    defaults(table);
                }
            }
            SectionKind::List => {
                for (i, item) in values.get_mut(s.key).and_then(Value::as_array_mut).into_iter().flatten().enumerate() {
                    defaults(item);
                    item["_i"] = i.into();
                }
            }
        }
    }
    Ok(values)
}

fn defaults(fields: &[Field], item: &mut Value) {
    for f in fields {
        if let (Some(d), Some(obj)) = (&f.default, item.as_object_mut()) {
            obj.entry(f.key).or_insert_with(|| d.clone());
        }
        if let Input::Group(group) = &f.input {
            for entry in item.get_mut(f.key).and_then(Value::as_array_mut).into_iter().flatten() {
                defaults(group, entry);
            }
        }
    }
}

/// Schreibt die Formularwerte in den Text der Datei.
pub fn apply(text: &str, schema: &[Section], form: &Value) -> Result<String> {
    let mut doc: DocumentMut = text.parse()?;
    for s in schema {
        let value = form.get(s.key).filter(|v| !v.is_null());
        match s.kind {
            SectionKind::Root => fill(doc.as_table_mut(), &s.fields, form, s.title)?,
            SectionKind::Table | SectionKind::Optional => {
                let (parent, key) = split_key(s.key);
                match (value, parent) {
                    (None, None) if s.kind == SectionKind::Optional => {
                        doc.remove(key);
                    }
                    (None, Some(parent)) if s.kind == SectionKind::Optional => {
                        if let Some(p) = doc.get_mut(parent).and_then(Item::as_table_mut) {
                            p.remove(key);
                            if p.is_empty() {
                                doc.remove(parent);
                            }
                        }
                    }
                    (None, _) => bail!("{}: Abschnitt fehlt", s.title),
                    (Some(v), parent) => {
                        let mut container = doc.as_table_mut();
                        if let Some(parent) = parent {
                            // Nur `[plugin.name]` schreiben, kein leeres `[plugin]` davor
                            let mut implicit = Table::new();
                            implicit.set_implicit(true);
                            container = container.entry(parent).or_insert(Item::Table(implicit)).as_table_mut().with_context(|| format!("{parent} ist keine Tabelle"))?;
                        }
                        let table = container.entry(key).or_insert(toml_edit::table()).as_table_mut().with_context(|| format!("{} ist keine Tabelle", s.key))?;
                        fill(table, &s.fields, v, s.title)?;
                    }
                }
            }
            SectionKind::List => {
                let old: Vec<Table> = doc.get(s.key).and_then(Item::as_array_of_tables).map(|a| a.iter().cloned().collect()).unwrap_or_default();
                let mut list = ArrayOfTables::new();
                let mut position = old.first().and_then(Table::position);
                for (n, item) in value.and_then(Value::as_array).into_iter().flatten().enumerate() {
                    let mut table = item.get("_i").and_then(Value::as_u64).and_then(|i| old.get(i as usize)).cloned().unwrap_or_default();
                    // Neue Einträge hinter den vorherigen einsortieren
                    match table.position() {
                        Some(p) => position = Some(p),
                        None => table.set_position(position),
                    }
                    let name = item.get("name").and_then(Value::as_str).filter(|n| !n.is_empty());
                    let context = match name {
                        Some(name) => format!("{} '{name}'", s.title),
                        None => format!("{} Nr. {}", s.title, n + 1),
                    };
                    fill(&mut table, &s.fields, item, &context)?;
                    list.push(table);
                }
                if list.is_empty() {
                    doc.remove(s.key);
                } else {
                    doc.insert(s.key, Item::ArrayOfTables(list));
                }
            }
        }
    }
    Ok(doc.to_string())
}

fn fill(table: &mut Table, fields: &[Field], item: &Value, context: &str) -> Result<()> {
    for f in fields {
        let old = table.get(f.key).and_then(item_to_json);
        if !f.active(item) && fields.iter().any(|g| g.key == f.key && g.active(item)) {
            // Gleicher Schlüssel bei einem anderen Dienst (z. B. eines Plugins), der gerade gilt
            continue;
        }
        let new = if f.active(item) { convert(f, item.get(f.key), old.as_ref(), context)? } else { None };
        let Some(new) = new else {
            if f.required && f.active(item) {
                bail!("{context}: „{}“ ist leer", f.label);
            }
            table.remove(f.key);
            continue;
        };
        match &old {
            Some(old) if *old == new => continue,
            None if f.default.as_ref() == Some(&new) => continue,
            _ => {}
        }
        if let Input::Pairs(_) | Input::Group(_) = &f.input {
            let keys: Vec<&str> = match &f.input {
                Input::Pairs(pairs) => pairs.iter().map(|(k, _)| *k).collect(),
                Input::Group(group) => group.iter().map(|g| g.key).collect(),
                _ => unreachable!(),
            };
            let tables = new.as_array().into_iter().flatten().map(|item| json_to_table(item, &keys)).collect::<Result<_>>()?;
            table.insert(f.key, Item::ArrayOfTables(tables));
            continue;
        }
        let mut value = json_to_toml(&new).with_context(|| format!("{context}: {}", f.label))?;
        // Kommentar hinter dem alten Wert behalten
        if let Some(old) = table.get(f.key).and_then(Item::as_value) {
            *value.decor_mut() = old.decor().clone();
        }
        table.insert(f.key, Item::Value(value));
    }
    Ok(())
}

/// Formularwert in den Wert für die Datei umwandeln; `None` = Eintrag entfernen
fn convert(f: &Field, value: Option<&Value>, old: Option<&Value>, context: &str) -> Result<Option<Value>> {
    let value = value.unwrap_or(&Value::Null);
    let text = |s: &str| (!s.trim().is_empty()).then(|| Value::from(s.trim()));
    Ok(match (&f.input, value) {
        (_, Value::Null) => None,
        (Input::Secret, Value::Object(o)) => match o.get("new").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(secret) => Some(hash(secret)?.into()),
            None => old.cloned(),
        },
        (Input::Number, Value::String(s)) if s.trim().is_empty() => None,
        (Input::Number, Value::String(s)) => {
            Some(s.trim().parse::<i64>().map_err(|_| anyhow::anyhow!("{context}: {} ist keine Zahl", f.label))?.into())
        }
        (Input::Number, Value::Number(_)) | (Input::Bool, Value::Bool(_)) => Some(value.clone()),
        (Input::Lines, Value::String(s)) => {
            let lines: Vec<Value> = s.lines().filter_map(text).collect();
            (!lines.is_empty()).then(|| lines.into())
        }
        (Input::Lines | Input::Choices(_) | Input::Names(_), Value::Array(a)) => {
            let items: Vec<Value> = a.iter().filter_map(|v| v.as_str().and_then(text)).collect();
            (!items.is_empty()).then(|| items.into())
        }
        (Input::Pairs(keys), Value::Array(a)) => {
            // Ohne trim: Leerzeichen sind in Such- und Ersatztext Absicht
            let items: Vec<Value> = a
                .iter()
                .filter(|item| item.get(keys[0].0).and_then(Value::as_str).is_some_and(|s| !s.is_empty()))
                .map(|item| {
                    let fields = keys.iter().filter_map(|(k, _)| item.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| (k.to_string(), Value::from(s))));
                    Value::Object(fields.collect())
                })
                .collect();
            (!items.is_empty()).then(|| items.into())
        }
        (Input::Group(fields), Value::Array(a)) => {
            let mut items = Vec::new();
            for (n, item) in a.iter().enumerate() {
                let context = match item.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) {
                    Some(name) => format!("{context}, {} '{name}'", f.label),
                    None => format!("{context}, {} Nr. {}", f.label, n + 1),
                };
                let mut entry = serde_json::Map::new();
                for g in fields.iter().filter(|g| g.active(item)) {
                    match convert(g, item.get(g.key), None, &context)? {
                        // Standardwerte wie bei den übrigen Feldern nicht in die Datei schreiben
                        Some(v) if g.default.as_ref() == Some(&v) => {}
                        Some(v) => {
                            entry.insert(g.key.into(), v);
                        }
                        None if g.required => bail!("{context}: „{}“ ist leer", g.label),
                        None => {}
                    }
                }
                items.push(Value::Object(entry));
            }
            (!items.is_empty()).then(|| items.into())
        }
        (Input::Text | Input::Multiline | Input::Password | Input::Secret | Input::Select(_) | Input::Device, Value::String(s)) => text(s),
        _ => bail!("{context}: ungültiger Wert für {}", f.label),
    })
}

fn value_to_json(value: &toml_edit::Value) -> Value {
    let mut value = value.clone();
    value.decor_mut().clear();
    toml::from_str::<Value>(&format!("v = {value}")).ok().and_then(|mut v| v.get_mut("v").map(Value::take)).unwrap_or(Value::Null)
}

fn item_to_json(item: &Item) -> Option<Value> {
    match item {
        Item::Value(value) => Some(value_to_json(value)),
        Item::ArrayOfTables(tables) => tables.iter().map(|t| toml::from_str(&t.to_string()).ok()).collect::<Option<Vec<Value>>>().map(Value::from),
        _ => None,
    }
}

/// Eintrag für `[[abschnitt.feld]]` mit den Schlüsseln in der Reihenfolge des Schemas; ohne
/// eigene Position landet er direkt unter seinem Abschnitt
fn json_to_table(item: &Value, keys: &[&str]) -> Result<Table> {
    let mut table = Table::new();
    for k in keys {
        match item.get(*k) {
            // Gruppe in einer Gruppe (z. B. Kanäle eines Eingangs bei einem Client): [[a.b.c]]
            Some(Value::Array(a)) if !a.is_empty() && a.iter().all(Value::is_object) => {
                let mut list = ArrayOfTables::new();
                for entry in a {
                    let mut keys: Vec<&str> = entry.as_object().into_iter().flat_map(|o| o.keys()).map(String::as_str).collect();
                    // Name vorn, wie in den übrigen Abschnitten
                    keys.sort_by_key(|k| *k != "name");
                    list.push(json_to_table(entry, &keys)?);
                }
                table.insert(k, Item::ArrayOfTables(list));
            }
            Some(v) => {
                table.insert(k, Item::Value(json_to_toml(v)?));
            }
            None => {}
        }
    }
    Ok(table)
}

fn json_to_toml(value: &Value) -> Result<toml_edit::Value> {
    Ok(match value {
        Value::String(s) => s.as_str().into(),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => n.as_i64().context("Zahl zu groß")?.into(),
        Value::Array(a) => a.iter().map(json_to_toml).collect::<Result<toml_edit::Array>>()?.into(),
        _ => bail!("nicht unterstützter Wert"),
    })
}

/// Argon2-Hash für Passwörter und Tokens
pub fn hash(secret: &str) -> Result<String> {
    use argon2::PasswordHasher;
    Ok(argon2::Argon2::default().hash_password(secret.as_bytes()).map_err(|e| anyhow::anyhow!("{e}"))?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"# Kopfkommentar
database = "test.db"

[web]
listen = "0.0.0.0:8443" # öffentlich
tls = "self-signed"

# Erster Benutzer
[[user]]
name = "admin"
password_hash = "$argon2id$alt"

# Eingang A
[[input]]
name = "A"
type = "wav"
path = "a.wav"

# Eingang B
[[input]]
name = "B"
type = "stdin"
sample_rate = 22050
pocsag = "auto"
"#;

    fn roundtrip(change: impl FnOnce(&mut Value)) -> Result<String> {
        let schema = server_schema(&[]);
        let mut form = to_json(TEXT, &schema)?;
        change(&mut form);
        apply(TEXT, &schema, &form)
    }

    #[test]
    fn unchanged_form_keeps_text() {
        assert_eq!(roundtrip(|_| {}).unwrap(), TEXT);
    }

    #[test]
    fn defaults_are_filled_but_not_written() {
        let form = to_json(TEXT, &server_schema(&[])).unwrap();
        assert_eq!(form["notify_repeat"], 60);
        assert_eq!(form["input"][0]["pocsag"], "alpha");
        assert_eq!(form["input"][1]["_i"], 1);
    }

    #[test]
    fn change_keeps_comments() {
        let text = roundtrip(|f| f["web"]["listen"] = "127.0.0.1:9000".into()).unwrap();
        assert!(text.contains("listen = \"127.0.0.1:9000\" # öffentlich"), "{text}");
        assert!(text.starts_with("# Kopfkommentar"));
    }

    #[test]
    fn removing_list_entry_keeps_comments_of_others() {
        let text = roundtrip(|f| {
            f["input"].as_array_mut().unwrap().remove(0);
        })
        .unwrap();
        assert!(!text.contains("Eingang A") && !text.contains("a.wav"), "{text}");
        assert!(text.contains("# Eingang B\n[[input]]\nname = \"B\""), "{text}");
    }

    #[test]
    fn new_entry_lands_after_existing_ones() {
        let text = roundtrip(|f| {
            f["user"].as_array_mut().unwrap().push(json!({"name": "neu", "password_hash": {"new": "geheim"}}));
            f["filter"] = json!([{"name": "Lärm", "address": "^1", "exclude": true}]);
        })
        .unwrap();
        let config = crate::config::Config::parse(&text).unwrap();
        assert_eq!(config.users[1].name, "neu");
        assert!(config.users[1].password_hash.starts_with("$argon2"));
        assert!(text.find("name = \"neu\"").unwrap() < text.find("# Eingang A").unwrap(), "{text}");
        assert!(text.contains("[[filter]]\nname = \"Lärm\"\naddress = \"^1\"\nexclude = true\n"), "{text}");
    }

    #[test]
    fn empty_secret_keeps_hash() {
        let text = roundtrip(|f| f["user"][0]["password_hash"] = json!({"new": ""})).unwrap();
        assert!(text.contains("password_hash = \"$argon2id$alt\""));
    }

    #[test]
    fn switching_type_removes_old_fields() {
        let text = roundtrip(|f| {
            f["input"][0]["type"] = "soundcard".into();
            f["input"][0]["device"] = "".into();
            f["input"][1]["sample_rate"] = "48000".into();
        })
        .unwrap();
        assert!(!text.contains("a.wav") && !text.contains("device"), "{text}");
        assert!(text.contains("sample_rate = 48000"), "{text}");
    }

    #[test]
    fn client_with_remote_inputs() {
        let text = roundtrip(|f| {
            f["client"] = json!([{"name": "Pi", "token_hash": "", "remote": true, "input": [
                {"name": "Funk", "type": "rtl_fm", "frequency": "173.255M", "ppm": "42", "gain": "auto", "device": "", "squelch": 0, "decoders": crate::decoder::ALL},
                {"name": "Karte", "type": "soundcard", "device": "hw:1", "ppm": 0, "gain": "auto"},
            ]}]);
        })
        .unwrap();
        assert!(text.contains("[[client.input]]\nname = \"Funk\"\ntype = \"rtl_fm\"\nfrequency = \"173.255M\"\nppm = 42\n"), "{text}");
        assert!(text.contains("[[client.input]]\nname = \"Karte\"\ntype = \"soundcard\"\ndevice = \"hw:1\"\n"), "{text}");
        let config = crate::config::Config::parse(&text).unwrap();
        let client = &config.clients[0];
        assert!(client.remote && client.token_hash.is_empty());
        assert!(matches!(&client.inputs[0].source, crate::config::Source::RtlFm { ppm: 42, device, gain, .. } if device == "0" && gain == "auto"));
    }

    #[test]
    fn rtl_sdr_channels() {
        let channels = json!([{"name": "Kreis", "frequency": "173.255M"}, {"name": "FMS", "frequency": "172.89M"}]);
        let input = json!({"name": "Stick", "type": "rtl_sdr", "ppm": "3", "gain": "auto", "channel": channels});
        let text = roundtrip(|f| {
            f["input"] = json!([input.clone()]);
            f["client"] = json!([{"name": "Pi", "token_hash": "", "remote": true, "input": [input]}]);
        })
        .unwrap();
        assert!(text.contains("[[input.channel]]\nname = \"Kreis\"\nfrequency = \"173.255M\"\n"), "{text}");
        let config = crate::config::Config::parse(&text).unwrap();
        crate::serve::validate_inputs(&config.inputs).unwrap();
        crate::serve::validate_inputs(&config.clients[0].inputs).unwrap();
        assert_eq!(config.inputs[0].sources(), ["Kreis", "FMS"]);
        assert_eq!(config.clients[0].inputs[0].sources(), ["Kreis", "FMS"]);
        // Und wieder zurück ins Formular
        let form = to_json(&text, &server_schema(&[])).unwrap();
        assert_eq!(form["client"][0]["input"][0]["channel"][1]["name"], "FMS");
    }

    #[test]
    fn missing_required_field_is_reported() {
        let err = roundtrip(|f| f["input"][1]["sample_rate"] = "".into()).unwrap_err();
        assert_eq!(err.to_string(), "Eingänge 'B': „Abtastrate (Hz)“ ist leer");
        let err = roundtrip(|f| f["user"].as_array_mut().unwrap().push(json!({"name": "x"}))).unwrap_err();
        assert_eq!(err.to_string(), "Benutzer 'x': „Passwort“ ist leer");
    }

    #[test]
    fn optional_section_can_be_added_and_removed() {
        let text = roundtrip(|f| f["weather"] = json!({"warncells": "114713000\n\n", "interval": "300"})).unwrap();
        assert!(text.contains("[weather]\nwarncells = [\"114713000\"]\n"), "{text}");
        let schema = server_schema(&[]);
        let mut form = to_json(&text, &schema).unwrap();
        form["weather"] = Value::Null;
        assert!(!apply(&text, &schema, &form).unwrap().contains("weather"));
    }

    #[test]
    fn rewrite_replacements_as_nested_tables() {
        let text = roundtrip(|f| {
            f["rewrite"] = json!([{"name": "Stichwort", "replace": [{"pattern": "^F(\\d)", "with": "Feuer $1"}, {"pattern": " +", "with": " "}, {"pattern": "", "with": "weg"}], "address_add": "-1000"}]);
        })
        .unwrap();
        let config = crate::config::Config::parse(&text).unwrap();
        let r = &config.rewrites[0];
        assert_eq!((r.replace.len(), r.replace[1].with.as_str(), r.address_add), (2, " ", Some(-1000)), "{text}");
        assert!(text.contains("address_add = -1000\n\n[[rewrite.replace]]\npattern = '^F(\\d)'\nwith = \"Feuer $1\"\n"), "{text}");
        // Unverändert gespeichert bleibt der Text gleich
        let schema = server_schema(&[]);
        assert_eq!(apply(&text, &schema, &to_json(&text, &schema).unwrap()).unwrap(), text);
    }

    #[test]
    fn example_config_roundtrips() {
        let text = include_str!("../boslook.example.toml");
        let schema = server_schema(&[]);
        assert_eq!(apply(text, &schema, &to_json(text, &schema).unwrap()).unwrap(), text);
    }
}
