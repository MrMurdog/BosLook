//! Wetterwarnungen des Deutschen Wetterdienstes (Open Data, `warnings.json`).
//!
//! Jede Warnung wird zu einem Telegramm mit `kind = "wetter"`: Adresse = Gebiet,
//! Sub = Warnstufe (1 Vorabinformation … 5 extremes Unwetter), Meldung = Überschrift,
//! Zeitraum und Beschreibung. Bereits gespeicherte Warnungen werden nicht erneut gemeldet.

use crate::db::Db;
use crate::filter::Filters;
use crate::telegram::{Decoded, Kind, Telegram};
use anyhow::{Context, Result};
use chrono::{Local, TimeZone};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;

const WARNINGS_URL: &str = "https://www.dwd.de/DWD/warnungen/warnapp/json/warnings.json";
const WARNCELLS_URL: &str = "https://www.dwd.de/DE/leistungen/opendata/help/warnungen/cap_warncellids_csv.csv?__blob=publicationFile";
pub const SOURCE: &str = "Wetter";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeatherConfig {
    /// Warncell-IDs der Gebiete, siehe `boslook warncell <Name>`
    pub warncells: Vec<String>,
    /// Abfrageintervall in Sekunden
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    300
}

#[derive(Deserialize)]
struct Response {
    warnings: HashMap<String, Vec<Warning>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Warning {
    level: u8,
    start: i64,
    end: Option<i64>,
    region_name: String,
    headline: String,
    description: String,
}

/// Die aktuellen Warnungen für die gewünschten Gebiete aus der DWD-Antwort (JSONP).
fn parse(jsonp: &str, warncells: &[String]) -> Result<Vec<Decoded>> {
    let json = jsonp
        .find('(')
        .zip(jsonp.rfind(')'))
        .map(|(a, b)| &jsonp[a + 1..b])
        .context("Unerwartetes Format der DWD-Warnungen")?;
    let mut response: Response = serde_json::from_str(json).context("DWD-Warnungen")?;
    let time = |ms: i64| Local.timestamp_millis_opt(ms).single().map(|t| t.format("%d.%m. %H:%M").to_string()).unwrap_or_default();
    let mut out = Vec::new();
    for cell in warncells {
        for w in response.warnings.remove(cell).unwrap_or_default() {
            let until = w.end.map(|end| format!(" bis {}", time(end))).unwrap_or_default();
            out.push(Decoded {
                kind: Kind::Wetter,
                address: w.region_name,
                sub: w.level.to_string(),
                message: format!("{} (ab {}{until}): {}", w.headline, time(w.start), w.description),
            });
        }
    }
    Ok(out)
}

pub fn run(config: &WeatherConfig, filters: &Filters, db: &Mutex<Db>, tx: &mpsc::Sender<Telegram>) {
    loop {
        let result = fetch(WARNINGS_URL).and_then(|text| parse(&text, &config.warncells));
        match result {
            Ok(warnings) => {
                for d in warnings.into_iter().filter(|d| filters.allows(d)) {
                    let known = db.lock().unwrap().contains(d.kind, &d.address, &d.message).unwrap_or(false);
                    if !known {
                        let _ = tx.blocking_send(Telegram::new(d, SOURCE, Local::now()));
                    }
                }
            }
            Err(e) => eprintln!("Wetterwarnungen: {e:#}"),
        }
        std::thread::sleep(Duration::from_secs(config.interval));
    }
}

fn fetch(url: &str) -> Result<String> {
    Ok(ureq::get(url).call()?.body_mut().read_to_string()?)
}

/// `boslook warncell <Name>`: passende Warncell-IDs ausgeben.
pub fn search(name: &str) -> Result<()> {
    let csv = fetch(WARNCELLS_URL).context("Liste der Warncell-IDs laden")?;
    let name = name.to_lowercase();
    for line in csv.lines().skip(1) {
        let fields: Vec<&str> = line.split(';').collect();
        let [id, full, short, ..] = fields[..] else { continue };
        // Nur Kreise, Seen, Küsten und Regionen kommen in den Warnungen vor, keine Gemeinden
        if id.starts_with(['1', '2', '5', '9']) && (full.to_lowercase().contains(&name) || short.to_lowercase().contains(&name)) {
            println!("{id}\t{full}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dwd_warnings_for_configured_cells() {
        let jsonp = include_str!("testdata/dwd-warnings.json");
        let got = parse(jsonp, &["109189000".into(), "999999999".into()]).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].kind, Kind::Wetter);
        assert_eq!(got[0].address, "Kreis Traunstein");
        assert_eq!(got[0].sub, "3");
        assert!(got[0].message.starts_with("Amtliche WARNUNG vor STURMBÖEN (ab "), "{}", got[0].message);
        assert!(got[0].message.contains(" bis "));
        assert!(parse(jsonp, &["105970000".into()]).unwrap()[0].message.contains("60 km/h"));
        assert!(parse("Fehler", &[]).is_err());
    }
}
