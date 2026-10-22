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
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

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

/// Anfrage der eigenen Seite an `api/…` (Fähigkeit `web`)
#[derive(Debug, Clone, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    /// Pfad ohne `/api`, z. B. `/stats`
    pub path: String,
    /// Query-String ohne `?`
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub body: String,
    /// Angemeldeter Benutzer
    pub user: Option<String>,
}

/// Antwort an die eigene Seite
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

impl HttpResponse {
    pub fn json(value: &Value) -> HttpResponse {
        HttpResponse { status: 200, content_type: "application/json".into(), body: value.to_string() }
    }

    pub fn text(body: impl Into<String>) -> HttpResponse {
        HttpResponse { status: 200, content_type: "text/plain; charset=utf-8".into(), body: body.into() }
    }

    pub fn not_found() -> HttpResponse {
        HttpResponse { status: 404, ..HttpResponse::text("nicht gefunden") }
    }
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

    /// Fähigkeit `web`: Anfragen der eigenen Seite, Antwort innerhalb von 10 Sekunden
    fn http(&mut self, _r: &HttpRequest) -> HttpResponse {
        HttpResponse::not_found()
    }
}

/// Schreibt Nachrichten an BosLook und fragt Daten ab; lässt sich klonen und in andere
/// Threads geben (z. B. in `init` klonen und im Plugin aufheben).
#[derive(Clone)]
pub struct Output {
    out: Arc<Mutex<std::io::Stdout>>,
    /// Offene Abfragen nach ID
    queries: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>,
    next_id: Arc<AtomicU64>,
}

impl Output {
    fn send(&self, msg: &Value) {
        let mut out = self.out.lock().unwrap();
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
    }

    /// Daten von BosLook lesen, z. B. `query("telegrams", json!({"source": "SDR", "limit": 100}))`,
    /// `query("sources", json!({}))` oder `query("descriptions", json!({"q": "^19388"}))`.
    /// Funktioniert auch innerhalb der Methoden von [`Plugin`].
    pub fn query(&self, query: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.queries.lock().unwrap().insert(id, tx);
        let mut msg = json!({ "type": "query", "id": id, "query": query });
        if let (Some(msg), Value::Object(params)) = (msg.as_object_mut(), params) {
            msg.extend(params);
        }
        self.send(&msg);
        let reply = rx.recv_timeout(Duration::from_secs(10));
        self.queries.lock().unwrap().remove(&id);
        let reply = reply.map_err(|_| format!("keine Antwort auf Abfrage {query}"))?;
        match reply["error"].as_str() {
            Some(e) => Err(e.to_string()),
            None => Ok(reply["result"].clone()),
        }
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

/// Verarbeitet Nachrichten von BosLook, bis stdin endet.
pub fn run(mut plugin: impl Plugin) {
    let out = Output { out: Arc::new(Mutex::new(std::io::stdout())), queries: Arc::default(), next_id: Arc::new(AtomicU64::new(1)) };
    // Eigener Thread zum Lesen, damit Antworten auf query() ankommen, während eine Methode wartet
    let (tx, rx) = mpsc::channel::<String>();
    let queries = out.queries.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Ok(msg) = serde_json::from_str::<Value>(&line)
                && msg["type"] == "result"
            {
                let waiting = msg["id"].as_u64().and_then(|id| queries.lock().unwrap().remove(&id));
                if let Some(waiting) = waiting {
                    let _ = waiting.send(msg);
                }
                continue;
            }
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    for line in rx {
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
        "http" => {
            let r = plugin.http(&serde_json::from_value(msg)?);
            out.send(&json!({ "type": "response", "id": id, "status": r.status, "content_type": r.content_type, "body": r.body }));
        }
        _ => {}
    }
    Ok(())
}
