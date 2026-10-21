use crate::filter::FilterConfig;
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_database")]
    pub database: String,
    pub web: WebConfig,
    #[serde(default, rename = "user")]
    pub users: Vec<UserConfig>,
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
pub struct InputConfig {
    pub name: String,
    #[serde(flatten)]
    pub source: Source,
    #[serde(default = "default_decoders")]
    pub decoders: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    Wav { path: String },
    /// Rohe Samples (s16le, mono) von stdin, z. B. `rtl_fm ... | boslook serve`
    Stdin { sample_rate: u32 },
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
        assert_eq!(c.web.tls, Tls::SelfSigned);
        assert_eq!(c.inputs.len(), 2);
        assert!(matches!(c.inputs[1].source, Source::Stdin { sample_rate: 22050 }));
        assert_eq!(c.filters.len(), 2);
    }
}
