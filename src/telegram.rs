use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Pocsag,
    Fms,
    Zvei,
    /// Wetterwarnung des DWD
    Wetter,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Pocsag => "pocsag",
            Kind::Fms => "fms",
            Kind::Zvei => "zvei",
            Kind::Wetter => "wetter",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "pocsag" => Some(Kind::Pocsag),
            "fms" => Some(Kind::Fms),
            "zvei" => Some(Kind::Zvei),
            "wetter" => Some(Kind::Wetter),
            _ => None,
        }
    }
}

/// Ein dekodiertes Telegramm, wie es ein Dekoder liefert (ohne Zeit/Quelle).
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub kind: Kind,
    /// POCSAG: RIC, FMS: 8-stellige Kennung, ZVEI: 5-Ton-Folge, Wetter: Gebiet
    pub address: String,
    /// POCSAG: Unteradresse a-d, FMS: Status (0-F), ZVEI: leer, Wetter: Warnstufe 1-5
    pub sub: String,
    /// POCSAG: Meldungstext, FMS: Status-/Richtungstext, ZVEI: leer, Wetter: Warnung
    pub message: String,
}

/// Ein Telegramm mit Empfangszeit und Quelle, wie es gespeichert und angezeigt wird.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Telegram {
    #[serde(default)]
    pub id: i64,
    pub time: DateTime<Local>,
    pub source: String,
    pub kind: Kind,
    pub address: String,
    pub sub: String,
    pub message: String,
    /// Aus den Beschreibungen (nur POCSAG): Ort, Kurz- und Langbeschreibung
    #[serde(default)]
    pub ort: String,
    #[serde(default)]
    pub kurz: String,
    #[serde(default)]
    pub lang: String,
}

impl Telegram {
    pub fn new(d: Decoded, source: &str, time: DateTime<Local>) -> Telegram {
        Telegram {
            id: 0,
            time,
            source: source.to_string(),
            kind: d.kind,
            address: d.address,
            sub: d.sub,
            message: d.message,
            ort: String::new(),
            kurz: String::new(),
            lang: String::new(),
        }
    }
}
