use crate::filter::FilterConfig;
use crate::notify::{AlarmConfig, NotifierConfig};
use crate::rewrite::RewriteConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    /// Einmal am Tag nach einer neuen Version suchen
    #[serde(default = "default_true")]
    pub update_check: bool,
    #[serde(default)]
    pub update_channel: UpdateChannel,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// Nur fertige Versionen
    #[default]
    Stable,
    /// Auch Vorabversionen
    Beta,
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
    /// Argon2-Hash des Tokens, erzeugt mit `boslook hash-password` oder beim Einrichtungscode;
    /// leer = noch nicht eingerichtet
    #[serde(default)]
    pub token_hash: String,
    /// Fernkonfiguration: Der Client nimmt statt seiner eigenen Eingänge die unter `inputs`
    #[serde(default)]
    pub remote: bool,
    #[serde(default, rename = "input")]
    pub inputs: Vec<InputConfig>,
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
    /// SHA-256-Fingerabdruck des Server-Zertifikats (aus dem Einrichtungscode), wirkt wie `cert`
    pub fingerprint: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    Wav { path: String },
    /// Rohe Samples (s16le, mono) von stdin, z. B. `rtl_fm ... | boslook serve`
    Stdin { sample_rate: u32 },
    /// Soundkarte; `device` wie von `boslook devices` angezeigt, sonst das Standardgerät
    Soundcard { device: Option<String> },
    /// Ein RTL-SDR-Stick über `rtl_fm`, das BosLook selbst startet
    RtlFm {
        /// z. B. `173.255M`
        frequency: String,
        /// Frequenzkorrektur des Sticks
        #[serde(default)]
        ppm: i32,
        /// Verstärkung in dB oder `auto`
        #[serde(default = "default_gain")]
        gain: String,
        /// Index oder Seriennummer des Sticks
        #[serde(default = "default_rtl_device")]
        device: String,
        #[serde(default)]
        squelch: u32,
        /// Programm, falls `rtl_fm` nicht im PATH liegt
        program: Option<String>,
    },
    /// Mehrere Kanäle aus einem RTL-SDR-Stick über `rtl_sdr` und eigenen Demodulator (siehe `sdr.rs`)
    RtlSdr {
        #[serde(default)]
        ppm: i32,
        #[serde(default = "default_gain")]
        gain: String,
        #[serde(default = "default_rtl_device")]
        device: String,
        /// Programm, falls `rtl_sdr` nicht im PATH liegt
        program: Option<String>,
        /// Jeder Kanal erscheint als eigener Reiter
        #[serde(default, rename = "channel")]
        channels: Vec<ChannelConfig>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    pub name: String,
    /// z. B. `173.255M`
    pub frequency: String,
}

impl InputConfig {
    /// Namen, unter denen die Telegramme dieses Eingangs erscheinen (Reiter): die Kanäle eines
    /// Sticks mit `rtl_sdr`, sonst der Name des Eingangs
    pub fn sources(&self) -> Vec<String> {
        match &self.source {
            Source::RtlSdr { channels, .. } => channels.iter().map(|c| c.name.clone()).collect(),
            _ => vec![self.name.clone()],
        }
    }
}

/// Eingänge, die ein Client vom Server bekommt (Fernkonfiguration, siehe `forward.rs`)
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    #[serde(default, rename = "input")]
    pub inputs: Vec<InputConfig>,
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

fn default_true() -> bool {
    true
}

fn default_gain() -> String {
    "auto".into()
}

fn default_rtl_device() -> String {
    "0".into()
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
