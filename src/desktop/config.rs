use anyhow::Result;
use boslook::form::{Input, Section, SectionKind, TEMPLATE_HELP, field, match_fields, priority_field, template_fields};
use boslook::notify::Priority;
use boslook::telegram::Kind;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopConfig {
    pub server: ServerConfig,
    #[serde(default, rename = "event")]
    pub events: Vec<EventConfig>,
    /// Gleiche Telegramme innerhalb dieser Sekunden nur einmal auslösen (0 = jedes Mal)
    #[serde(default = "default_repeat")]
    pub repeat: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Adresse der Weboberfläche, z. B. `https://mein-server.de:8443`
    pub url: String,
    pub user: String,
    pub password: String,
    /// Zertifikat des Servers bei `tls = "self-signed"`
    pub cert: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventConfig {
    pub name: String,
    pub kind: Option<Kind>,
    pub address: Option<String>,
    pub sub: Option<String>,
    pub message: Option<String>,
    pub ort: Option<String>,
    pub kurz: Option<String>,
    pub lang: Option<String>,
    /// normal = Systembenachrichtigung, high/emergency = Popup-Fenster im Vordergrund
    #[serde(default)]
    pub priority: Priority,
    /// Tondatei (WAV, MP3, …)
    pub sound: Option<String>,
    /// Programm, das gestartet wird; in `args` werden die Platzhalter ersetzt (siehe `notify::fill`)
    pub program: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Vorlagen für Benachrichtigung bzw. Fenster
    pub title: Option<String>,
    pub text: Option<String>,
}

fn default_repeat() -> u64 {
    60
}

impl DesktopConfig {
    pub fn parse(text: &str) -> Result<DesktopConfig> {
        let config: DesktopConfig = toml::from_str(text)?;
        crate::events::Events::new(&config.events, 0)?;
        Ok(config)
    }
}

/// Schema für das Formular der Einstellungen
pub fn schema() -> Vec<Section> {
    vec![
        Section {
            key: "",
            title: "Allgemein",
            help: "",
            kind: SectionKind::Root,
            fields: vec![
                field("repeat", "Wiederholungen unterdrücken (s)", Input::Number)
                    .default(60)
                    .help("Gleiche Telegramme innerhalb dieser Sekunden nur einmal auslösen, 0 = jedes Mal"),
            ],
        },
        Section {
            key: "server",
            title: "Server",
            help: "",
            kind: SectionKind::Table,
            fields: vec![
                field("url", "Adresse", Input::Text).required().help("Adresse der Weboberfläche, z. B. https://mein-server.de:8443"),
                field("user", "Benutzer", Input::Text).required(),
                field("password", "Passwort", Input::Password).required(),
                field("cert", "Zertifikat des Servers", Input::Text).help("Nur bei tls = self-signed: Pfad zur kopierten boslook-cert.pem"),
            ],
        },
        Section {
            key: "event",
            title: "Ereignisse",
            help: "Lokale Aktionen bei passenden Telegrammen",
            kind: SectionKind::List,
            fields: [
                vec![field("name", "Name", Input::Text).required()],
                match_fields(),
                vec![
                    priority_field("normal = Systembenachrichtigung, high/emergency = Fenster im Vordergrund bis zum Quittieren"),
                    field("sound", "Tondatei", Input::Text).help("WAV, MP3 …"),
                    field("program", "Programm", Input::Text),
                    field("args", "Argumente", Input::Lines).help(
                        "Eins je Zeile; Platzhalter: {kind} {address} {sub} {message} {ort} {kurz} {lang} {source} {time} {alarm}",
                    ),
                ],
                template_fields("Benachrichtigung bzw. Fenster; leer = Namen der Ereignisse ({alarm})", TEMPLATE_HELP),
            ]
            .into_iter()
            .flatten()
            .collect(),
        },
    ]
}

/// Wird beim ersten Start angelegt.
pub const TEMPLATE: &str = r#"# BosLook Desktop – Einstellungen

# Gleiche Telegramme innerhalb dieser Sekunden nur einmal auslösen (0 = jedes Mal)
repeat = 60

[server]
url = "https://mein-server.de:8443"
user = "admin"
password = "ERSETZEN"
# Nur bei tls = "self-signed" auf dem Server: dessen Zertifikatsdatei (boslook-cert.pem)
# hierher kopieren. Dann wird genau dieses Zertifikat akzeptiert.
# cert = "server-cert.pem"

# Ereignisse: Felder wie bei den Filtern (kind, address, sub, message, ort, kurz, lang als
# reguläre Ausdrücke). title und text sind Vorlagen mit Platzhaltern wie {ort} oder {message}.
# priority = "normal" zeigt eine Systembenachrichtigung, "high" und "emergency" ein Fenster
# im Vordergrund, das bleibt, bis man es quittiert.
[[event]]
name = "Eigene Schleife"
kind = "pocsag"
address = "^18599"
priority = "high"
# sound = "alarm.wav"
# program = "C:/Programme/Alarm/alarm.exe"
# args = ["{address}", "{message}"]
"#;
