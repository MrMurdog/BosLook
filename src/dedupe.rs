//! Doppelfilter: Gleiche Telegramme auf demselben Kanal innerhalb des Zeitfensters nur einmal.
//!
//! Das erste wird sofort gespeichert und gemeldet. Eine spätere, bessere Fassung (mehr lesbare
//! Zeichen) ersetzt nur seinen Text; schlechtere fallen weg.

use crate::telegram::Telegram;
use std::time::{Duration, Instant};

/// Platzhalter des POCSAG-Dekoders für gestörte Zeichen
const BROKEN: char = '�';

pub struct Dedupe {
    window: Duration,
    /// Gespeicherte Telegramme im Zeitfenster und wann sie zuerst kamen
    recent: Vec<(Instant, Telegram)>,
}

#[derive(Debug)]
pub enum Verdict {
    New,
    /// Bessere Fassung: das gespeicherte Telegramm mit dem neuen Text
    Better(Telegram),
    Drop,
}

impl Dedupe {
    /// `window` 0 = aus
    pub fn new(window: Duration) -> Dedupe {
        Dedupe { window, recent: Vec::new() }
    }

    pub fn check(&mut self, t: &Telegram, now: Instant) -> Verdict {
        let window = self.window;
        self.recent.retain(|(at, _)| now.duration_since(*at) < window);
        let found = self.recent.iter_mut().map(|(_, seen)| seen).find(|seen| {
            (&seen.source, seen.kind, &seen.address, &seen.sub) == (&t.source, t.kind, &t.address, &t.sub) && same(&seen.message, &t.message)
        });
        match found {
            None => Verdict::New,
            Some(seen) if score(&t.message) > score(&seen.message) => {
                seen.message = t.message.clone();
                Verdict::Better(seen.clone())
            }
            Some(_) => Verdict::Drop,
        }
    }

    /// Gespeichertes Telegramm (mit ID) merken
    pub fn remember(&mut self, t: &Telegram, now: Instant) {
        if !self.window.is_zero() {
            self.recent.push((now, t.clone()));
        }
    }
}

/// Gleich bis auf gestörte Zeichen oder ein abgeschnittenes Ende
fn same(a: &str, b: &str) -> bool {
    a.trim_end().chars().zip(b.trim_end().chars()).all(|(x, y)| x == y || x == BROKEN || y == BROKEN)
}

/// Mehr lesbare Zeichen sind besser (weniger gestört bzw. nicht abgeschnitten), bei Gleichstand
/// weniger gestörte
fn score(message: &str) -> (usize, isize) {
    let message = message.trim_end();
    let broken = message.chars().filter(|c| *c == BROKEN).count();
    (message.chars().count() - broken, -(broken as isize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::{Decoded, Kind};

    fn pocsag(source: &str, address: &str, message: &str) -> Telegram {
        Telegram::new(Decoded { kind: Kind::Pocsag, address: address.into(), sub: "a".into(), message: message.into() }, source, chrono::Local::now())
    }

    #[test]
    fn better_versions_replace_worse_are_dropped() {
        let mut d = Dedupe::new(Duration::from_secs(20));
        let start = Instant::now();
        let mut first = pocsag("Funk", "1859991", "F2 Muster��r. 1");
        assert!(matches!(d.check(&first, start), Verdict::New));
        first.id = 7;
        d.remember(&first, start);
        // Gleich gut oder schlechter: weg
        assert!(matches!(d.check(&pocsag("Funk", "1859991", "F2 Muster��r. 1"), start), Verdict::Drop));
        assert!(matches!(d.check(&pocsag("Funk", "1859991", "F2 Mu��er��r. 1"), start), Verdict::Drop));
        assert!(matches!(d.check(&pocsag("Funk", "1859991", "F2 Muster"), start), Verdict::Drop));
        // Besser: ersetzt den Text des gespeicherten
        let later = start + Duration::from_secs(19);
        match d.check(&pocsag("Funk", "1859991", "F2 Musterstr. 1"), later) {
            Verdict::Better(t) => assert_eq!((t.id, t.message.as_str()), (7, "F2 Musterstr. 1")),
            v => panic!("{v:?}"),
        }
        // Danach zählt die bessere Fassung
        assert!(matches!(d.check(&pocsag("Funk", "1859991", "F2 Muster��r. 1"), later), Verdict::Drop));
        // Nach dem Zeitfenster (ab dem ersten) wieder neu
        assert!(matches!(d.check(&pocsag("Funk", "1859991", "F2 Musterstr. 1"), start + Duration::from_secs(20)), Verdict::New));
    }

    #[test]
    fn other_text_channel_or_ric_is_new() {
        let mut d = Dedupe::new(Duration::from_secs(20));
        let now = Instant::now();
        d.remember(&pocsag("Funk", "1859991", "F2 Musterstr. 1"), now);
        assert!(matches!(d.check(&pocsag("Funk", "1859991", "B3 Hauptstr. 4"), now), Verdict::New));
        assert!(matches!(d.check(&pocsag("SDR", "1859991", "F2 Musterstr. 1"), now), Verdict::New));
        assert!(matches!(d.check(&pocsag("Funk", "1859992", "F2 Musterstr. 1"), now), Verdict::New));
    }

    #[test]
    fn off() {
        let mut d = Dedupe::new(Duration::ZERO);
        let now = Instant::now();
        let t = pocsag("Funk", "1859991", "F2");
        d.remember(&t, now);
        assert!(matches!(d.check(&t, now), Verdict::New));
    }
}
