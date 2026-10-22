use crate::filter::FilterConfig;
use crate::notify::{AlarmConfig, NotifierConfig};
use crate::rewrite::RewriteConfig;
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_database")]
    pub database: String,
    /// Ohne `[web]` läuft BosLook ohne Weboberfläche (z. B. als reiner Client mit `[forward]`)
    pub web: Option<WebConfig>,
    #[serde(default, rename = "user")]
    pub users: Vec<UserConfig>,
    /// Clients, die Telegramme an diesen Server weiterleiten dürfen
    #[serde(default, rename = "client")]
    pub clients: Vec<ClientConfig>,
    /// Telegramme an einen anderen BosLook-Server weiterleiten
    pub forward: Option<ForwardConfig>,
    #[serde(default, rename = "input")]
    pub inputs: Vec<InputConfig>,
    #[serde(default, rename = "rewrite")]
    pub rewrites: Vec<RewriteConfig>,
    #[serde(default, rename = "filter")]
    pub filters: Vec<FilterConfig>,
    #[serde(default, rename = "notifier")]
    pub notifiers: Vec<NotifierConfig>,
    #[serde(default, rename = "alarm")]
    pub alarms: Vec<AlarmConfig>,
    /// Wetterwarnungen des DWD
    pub weather: Option<crate::weather::WeatherConfig>,
    /// Einstellungen der Plugins: `[plugin.<name>]`, in der Reihenfolge der Datei
    #[serde(default, rename = "plugin", deserialize_with = "plugin_tables")]
    pub plugins: Vec<(String, toml::Table)>,
    /// Gleiche Telegramme innerhalb dieser Sekunden nur einmal benachrichtigen (0 = aus)
    #[serde(default = "default_notify_repeat")]
    pub notify_repeat: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    pub listen: String,
    #[serde(default)]
    pub tls: Tls,
    #[serde(default = "default_cert")]
    pub cert: String,
    #[serde(default = "default_key")]
    pub key: String,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Tls {
    /// Unverschlüsseltes HTTP
    #[default]
    Off,
    /// Zertifikat wird beim ersten Start selbst erstellt (unter `cert`/`key` gespeichert)
    SelfSigned,
    /// Vorhandenes Zertifikat aus `cert`/`key` laden (z. B. Let's Encrypt)
    Files,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    pub name: String,
    /// Argon2-Hash, erzeugt mit `boslook hash-password`
    pub password_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub name: String,
    /// Argon2-Hash des Tokens, erzeugt mit `boslook hash-password`
    pub token_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForwardConfig {
    /// Adresse der Weboberfläche des Servers, z. B. `https://mein-server.de:8443`
    pub server: String,
    /// Name und Token wie beim Server unter `[[client]]` eingetragen
    pub name: String,
    pub token: String,
    /// Zertifikat des Servers (bei `tls = "self-signed"` dessen `cert`-Datei), sonst
    /// wird das Zertifikat über die üblichen Zertifizierungsstellen geprüft
    pub cert: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InputConfig {
    pub name: String,
    #[serde(flatten)]
    pub source: Source,
    #[serde(default = "default_decoders")]
    pub decoders: Vec<String>,
    /// Darstellung von POCSAG-Meldungen: alpha (Standard), numeric oder auto
    #[serde(default)]
    pub pocsag: crate::decoder::pocsag::Format,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    Wav { path: String },
    /// Rohe Samples (s16le, mono) von stdin, z. B. `rtl_fm ... | boslook serve`
    Stdin { sample_rate: u32 },
    /// Soundkarte; `device` wie von `boslook devices` angezeigt, sonst das Standardgerät
    Soundcard { device: Option<String> },
}

fn plugin_tables<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<(String, toml::Table)>, D::Error> {
    use serde::de::Error;
    let table = toml::Table::deserialize(d)?;
    table
        .into_iter()
        .map(|(name, value)| match value {
            toml::Value::Table(t) => Ok((name, t)),
            _ => Err(D::Error::custom(format!("plugin.{name} muss ein Abschnitt [plugin.{name}] sein"))),
        })
        .collect()
}

fn default_database() -> String {
    "boslook.db".into()
}

fn default_notify_repeat() -> u64 {
    60
}

fn default_cert() -> String {
    "boslook-cert.pem".into()
}

fn default_key() -> String {
    "boslook-key.pem".into()
}

fn default_decoders() -> Vec<String> {
    crate::decoder::ALL.iter().map(|s| s.to_string()).collect()
}

impl Config {
    pub fn load(path: &str) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("Konfiguration {path}"))?;
        Config::parse(&text).with_context(|| format!("Konfiguration {path}"))
    }

    pub fn parse(text: &str) -> Result<Config> {
        Ok(toml::from_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let c: Config = toml::from_str(include_str!("../boslook.example.toml")).unwrap();
        assert_eq!(c.web.unwrap().tls, Tls::SelfSigned);
        assert_eq!(c.inputs.len(), 2);
        assert!(matches!(c.inputs[1].source, Source::Stdin { sample_rate: 22050 }));
        assert_eq!(c.inputs[0].pocsag, crate::decoder::pocsag::Format::Alpha);
        assert_eq!(c.inputs[1].pocsag, crate::decoder::pocsag::Format::Auto);
        assert_eq!(c.filters.len(), 2);
        assert_eq!(c.clients[0].name, "Zuhause");
        assert_eq!(c.notifiers.len(), 3);
        assert_eq!(c.alarms[0].notify, ["Handy", "Leitstelle"]);
        assert_eq!(c.weather.unwrap().warncells, ["114713000"]);
    }

    #[test]
    fn parses_example_client_config() {
        let c: Config = toml::from_str(include_str!("../boslook-client.example.toml")).unwrap();
        assert!(c.web.is_none());
        assert_eq!(c.forward.unwrap().name, "Zuhause");
        assert!(matches!(c.inputs[0].source, Source::Soundcard { device: None }));
    }
}
