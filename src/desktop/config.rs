use anyhow::Result;
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
    /// normal = Systembenachrichtigung, high/emergency = Popup-Fenster im Vordergrund
    #[serde(default)]
    pub priority: Priority,
    /// Tondatei (WAV, MP3, …)
    pub sound: Option<String>,
    /// Programm, das gestartet wird; in `args` werden {kind} {address} {sub} {message} {source} ersetzt
    pub program: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
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

# Ereignisse: Felder wie bei den Filtern (kind, address, sub, message als reguläre Ausdrücke).
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
