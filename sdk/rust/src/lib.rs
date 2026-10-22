//! Bibliothek für BosLook-Plugins in Rust (Schnittstelle 1). Doku: docs/PLUGINS.md
//!
//! ```no_run
//! use boslook_plugin::{Plugin, Process, Telegram};
//!
//! struct Gross;
//!
//! impl Plugin for Gross {
//!     fn process(&mut self, t: &Telegram) -> Process {
//!         Process::Changed(Telegram { message: t.message.to_uppercase(), ..t.clone() })
//!     }
//! }
//!
//! fn main() {
//!     boslook_plugin::run(Gross);
//! }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Version der Schnittstelle, die diese Bibliothek spricht
pub const API: u32 = 1;

/// Ein Telegramm, wie BosLook es schickt
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Telegram {
    /// Nummer in der Datenbank; 0, solange es nicht gespeichert ist
    #[serde(default)]
    pub id: i64,
    /// Empfangszeit (RFC 3339)
    #[serde(default)]
    pub time: String,
    /// Eingang bzw. Reiter
    #[serde(default)]
    pub source: String,
    /// pocsag, fms, zvei oder wetter
    pub kind: String,
    pub address: String,
    #[serde(default)]
    pub sub: String,
    #[serde(default)]
    pub message: String,
    /// Aus den Beschreibungen (beim Bearbeiten noch leer)
    #[serde(default)]
    pub ort: String,
    #[serde(default)]
    pub kurz: String,
    #[serde(default)]
    pub lang: String,
}

/// Start-Nachricht von BosLook
#[derive(Debug, Clone, Deserialize)]
pub struct Init {
    pub name: String,
    /// Version von BosLook
    pub boslook: String,
    pub api: u32,
    /// Einstellungen aus `[plugin.<name>]` (ohne `enabled`)
    pub config: Map<String, Value>,
    /// Ordner für eigene Daten
    pub data_dir: PathBuf,
}

/// Eine Benachrichtigung (Fähigkeit `notify`)
#[derive(Debug, Clone, Deserialize)]
pub struct Notification {
    /// Einstellungen des Benachrichtigungsziels, dazu `name`
    pub target: Map<String, Value>,
    pub title: String,
    pub body: String,
    /// normal, high oder emergency
    pub priority: String,
    /// Fehlt bei `boslook test-notify`
    pub telegram: Option<Telegram>,
}

/// Ergebnis von [`Plugin::process`]
#[allow(clippy::large_enum_variant)] // selten erzeugt, Box wäre für Plugin-Autoren nur umständlicher
pub enum Process {
    Unchanged,
    Changed(Telegram),
    /// Telegramm verwerfen
    Drop,
}

/// Die Fähigkeiten in `plugin.toml` legen fest, welche Methoden BosLook aufruft.
pub trait Plugin {
    /// Einmal nach dem Start
    fn init(&mut self, _init: &Init, _out: &Output) {}

    /// Fähigkeit `observe`: jedes gespeicherte Telegramm
    fn telegram(&mut self, _t: &Telegram) {}

    /// Fähigkeit `process`: vor der Verarbeitung, Antwort innerhalb von 2 Sekunden
    fn process(&mut self, _t: &Telegram) -> Process {
        Process::Unchanged
    }

    /// Fähigkeit `notify`: Fehler werden an BosLook gemeldet und dort geloggt
    fn notify(&mut self, _n: &Notification) -> Result<(), String> {
        Err("Plugin hat keine Benachrichtigungsart".into())
    }
}

/// Schreibt Nachrichten an BosLook; lässt sich klonen und in andere Threads geben.
#[derive(Clone)]
pub struct Output(Arc<Mutex<std::io::Stdout>>);

impl Output {
    fn send(&self, msg: &Value) {
        let mut out = self.0.lock().unwrap();
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
    }

    /// Fähigkeit `input`: Telegramm an BosLook liefern; `source` = Reiter, Standard = Name des Plugins
    pub fn send_telegram(&self, kind: &str, address: &str, sub: &str, message: &str, source: Option<&str>) {
        let mut t = json!({ "kind": kind, "address": address, "sub": sub, "message": message });
        if let Some(source) = source {
            t["source"] = source.into();
        }
        self.send(&json!({ "type": "telegram", "telegram": t }));
    }
}

/// Zeile ins Log von BosLook. `println!` nicht benutzen: stdout gehört dem Protokoll.
pub fn log(line: impl std::fmt::Display) {
    eprintln!("{line}");
}

/// Liest Nachrichten von BosLook, bis stdin endet.
pub fn run(mut plugin: impl Plugin) {
    let out = Output(Arc::new(Mutex::new(std::io::stdout())));
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Err(e) = handle(&mut plugin, &out, &line) {
            log(format!("Nachricht nicht verstanden: {e}"));
        }
    }
}

fn handle(plugin: &mut impl Plugin, out: &Output, line: &str) -> Result<(), serde_json::Error> {
    let msg: Value = serde_json::from_str(line)?;
    let id = msg["id"].clone();
    match msg["type"].as_str().unwrap_or_default() {
        "init" => {
            let init: Init = serde_json::from_value(msg)?;
            if init.api != API {
                log(format!("BosLook spricht Schnittstelle {}, dieses Plugin {API}", init.api));
            }
            plugin.init(&init, out);
        }
        "telegram" => plugin.telegram(&serde_json::from_value(msg["telegram"].clone())?),
        "process" => {
            let reply = match plugin.process(&serde_json::from_value(msg["telegram"].clone())?) {
                Process::Unchanged => json!({ "type": "processed", "id": id }),
                Process::Changed(t) => json!({ "type": "processed", "id": id, "telegram": t }),
                Process::Drop => json!({ "type": "processed", "id": id, "drop": true }),
            };
            out.send(&reply);
        }
        "notify" => {
            let reply = match plugin.notify(&serde_json::from_value(msg)?) {
                Ok(()) => json!({ "type": "notified", "id": id }),
                Err(e) => json!({ "type": "notified", "id": id, "error": e }),
            };
            out.send(&reply);
        }
        _ => {}
    }
    Ok(())
}
