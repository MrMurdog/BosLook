//! Einrichtungscode für Clients: In der Weboberfläche des Servers erzeugt, auf dem Client mit
//! `boslook setup <code>` eingelöst. Der Code ist `boslook1:` + Base64 (URL-sicher) von JSON mit
//! Server-Adresse, Name, Token und Fingerabdruck des Server-Zertifikats.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

const PREFIX: &str = "boslook1:";

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SetupCode {
    pub server: String,
    pub name: String,
    pub token: String,
    /// Nur bei selbst signiertem Zertifikat, sonst wird über die Zertifizierungsstellen geprüft
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

impl SetupCode {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("JSON");
        format!("{PREFIX}{}", data_encoding::BASE64URL_NOPAD.encode(&json))
    }

    pub fn decode(code: &str) -> Result<SetupCode> {
        let data = code.trim().strip_prefix(PREFIX).context("Das ist kein Einrichtungscode von BosLook (beginnt mit boslook1:)")?;
        let json = data_encoding::BASE64URL_NOPAD.decode(data.as_bytes()).context("Einrichtungscode unvollständig oder beschädigt")?;
        serde_json::from_slice(&json).context("Einrichtungscode unvollständig oder beschädigt")
    }
}

/// `boslook setup`: Konfiguration des Clients schreiben und auf Wunsch als Dienst einrichten
pub fn run(code: &str, config: &str, service: bool, force: bool) -> Result<()> {
    let code = SetupCode::decode(code)?;
    let path = std::path::absolute(config)?;
    if path.exists() && !force {
        bail!("{} gibt es schon. Mit --force wird sie überschrieben.", path.display());
    }
    let dir = path.parent().context("Ordner der Konfiguration")?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, client_config(&code, &dir.join("boslook.db")))?;
    #[cfg(unix)]
    {
        // Das Token steht im Klartext darin
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    println!("Konfiguration geschrieben: {}", path.display());
    if service {
        install_service(&path)?;
    } else {
        println!("Starten mit: boslook serve -c \"{}\"", path.display());
    }
    println!("Die Eingänge werden auf dem Server unter „Einstellungen“ → „Clients“ → „{}“ eingestellt (Fernkonfiguration).", code.name);
    Ok(())
}

fn client_config(code: &SetupCode, database: &Path) -> String {
    let quote = |s: &str| toml_edit::Value::from(s).to_string();
    let mut text = format!(
        "# BosLook-Client, eingerichtet mit `boslook setup`.\n\
         # Die Eingänge kommen vom Server (Fernkonfiguration); eigene [[input]]-Abschnitte gelten\n\
         # nur, solange der Server keine schickt.\n\n\
         database = {}\n\n\
         [forward]\n\
         server = {}\n\
         name = {}\n\
         token = {}\n",
        quote(&database.to_string_lossy()),
        quote(&code.server),
        quote(&code.name),
        quote(&code.token),
    );
    if let Some(f) = &code.fingerprint {
        text += &format!("# Fingerabdruck des selbst signierten Server-Zertifikats\nfingerprint = {}\n", quote(f));
    }
    text
}

#[cfg(target_os = "linux")]
fn install_service(config: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let dir = config.parent().unwrap_or(Path::new("/"));
    let unit = format!(
        "[Unit]\nDescription=BosLook\nWants=network-online.target\nAfter=network-online.target\n\n\
         [Service]\nExecStart=\"{}\" serve -c \"{}\"\nWorkingDirectory={}\nRestart=always\nRestartSec=5\n\n\
         [Install]\nWantedBy=multi-user.target\n",
        exe.display(),
        config.display(),
        dir.display()
    );
    let path = "/etc/systemd/system/boslook.service";
    std::fs::write(path, unit).with_context(|| format!("{path} schreiben (als root ausführen, z. B. mit sudo)"))?;
    command("systemctl", &["daemon-reload"])?;
    command("systemctl", &["enable", "--now", "boslook.service"])?;
    println!("Dienst eingerichtet und gestartet: {path}\nLog ansehen mit: journalctl -u boslook -f");
    Ok(())
}

/// Unter Windows als Aufgabe, die beim Systemstart läuft (ein echter Dienst bräuchte eine eigene
/// Anbindung an die Dienststeuerung)
#[cfg(windows)]
fn install_service(config: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let run = format!("\"{}\" serve -c \"{}\"", exe.display(), config.display());
    command("schtasks", &["/Create", "/TN", "BosLook", "/SC", "ONSTART", "/RU", "SYSTEM", "/RL", "HIGHEST", "/F", "/TR", &run])
        .context("Als Administrator ausführen")?;
    command("schtasks", &["/Run", "/TN", "BosLook"])?;
    println!("Aufgabe „BosLook“ eingerichtet (startet mit Windows) und gestartet.");
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn install_service(_config: &Path) -> Result<()> {
    bail!("--service gibt es nur unter Linux (systemd) und Windows")
}

#[cfg(any(target_os = "linux", windows))]
fn command(program: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(program).args(args).status().with_context(|| format!("{program} starten"))?;
    if !status.success() {
        bail!("{program} {} fehlgeschlagen ({status})", args.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trip() {
        let code = SetupCode { server: "https://a.de:8443".into(), name: "Pi".into(), token: "abc".into(), fingerprint: Some("AB:CD".into()) };
        let text = code.encode();
        assert!(text.starts_with("boslook1:"));
        assert_eq!(SetupCode::decode(&format!(" {text}\n")).unwrap(), code);
        assert!(SetupCode::decode("boslook1:!!").is_err());
        assert!(SetupCode::decode("xyz").is_err());
    }

    #[test]
    fn written_config_is_valid() {
        let code = SetupCode { server: "https://a.de".into(), name: "Pi \"Keller\"".into(), token: "t".into(), fingerprint: Some("AB".into()) };
        let config = crate::config::Config::parse(&client_config(&code, Path::new("/opt/boslook/boslook.db"))).unwrap();
        let forward = config.forward.unwrap();
        assert_eq!(forward.name, "Pi \"Keller\"");
        assert_eq!(forward.fingerprint.as_deref(), Some("AB"));
        assert!(config.inputs.is_empty());
    }
}
