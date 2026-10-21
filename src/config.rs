use crate::filter::FilterConfig;
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
    #[serde(default, rename = "filter")]
    pub filters: Vec<FilterConfig>,
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

fn default_database() -> String {
    "boslook.db".into()
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
        toml::from_str(&text).with_context(|| format!("Konfiguration {path}"))
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
    }

    #[test]
    fn parses_example_client_config() {
        let c: Config = toml::from_str(include_str!("../boslook-client.example.toml")).unwrap();
        assert!(c.web.is_none());
        assert_eq!(c.forward.unwrap().name, "Zuhause");
        assert!(matches!(c.inputs[0].source, Source::Soundcard { device: None }));
    }
}
