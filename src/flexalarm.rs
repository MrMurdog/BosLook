//! Flexalarm: Ton-RICs (ohne Text) bekommen den Text der Text-RIC, die kurz danach kommt.
//!
//! Ton-RICs warten dafür höchstens `window` Sekunden. Kommt die Text-RIC, laufen alle wartenden
//! des Kanals mit ihrem Text weiter, sonst nach Ablauf des Fensters ohne Text.

use crate::telegram::{Kind, Telegram};
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlexalarmConfig {
    pub name: String,
    /// Name des Eingangs (Reiter), regulärer Ausdruck; leer = alle
    pub source: Option<String>,
    pub text_ric: String,
    /// Unteradresse der Text-RIC (a–d), leer = jede
    pub text_sub: Option<String>,
    pub start_ric: Option<String>,
    /// Regulärer Ausdruck auf die Ton-RICs; leer = jede POCSAG-Meldung ohne Text
    pub tone: Option<String>,
    /// Sekunden, die Ton-RICs auf die Text-RIC warten
    #[serde(default = "default_window")]
    pub window: u64,
    #[serde(default = "default_true")]
    pub show_text: bool,
    #[serde(default = "default_true")]
    pub show_start: bool,
}

fn default_window() -> u64 {
    3
}

fn default_true() -> bool {
    true
}

struct Rule {
    source: Option<Regex>,
    text_ric: u32,
    text_sub: Option<String>,
    start_ric: Option<u32>,
    tone: Option<Regex>,
    window: Duration,
    show_text: bool,
    show_start: bool,
}

struct Waiting {
    rule: usize,
    deadline: Instant,
    t: Telegram,
}

pub struct Flexalarm {
    rules: Vec<Rule>,
    waiting: Vec<Waiting>,
}

/// Größte RIC, die POCSAG übertragen kann (21 Bit)
const MAX_RIC: u32 = 2_097_151;

fn ric(s: &str) -> Option<u32> {
    s.trim().parse().ok().filter(|r| *r <= MAX_RIC)
}

impl Flexalarm {
    pub fn new(configs: &[FlexalarmConfig]) -> Result<Flexalarm> {
        let rules = configs.iter().map(|c| {
            let context = || format!("Flexalarm '{}'", c.name);
            let re = |s: &Option<String>| s.as_deref().filter(|s| !s.is_empty()).map(Regex::new).transpose().with_context(context);
            let parse = |s: &str| ric(s).with_context(|| format!("Flexalarm '{}': '{s}' ist keine RIC (0 bis {MAX_RIC})", c.name));
            Ok(Rule {
                source: re(&c.source)?,
                text_ric: parse(&c.text_ric)?,
                text_sub: c.text_sub.clone().filter(|s| !s.is_empty()),
                start_ric: c.start_ric.as_deref().filter(|s| !s.is_empty()).map(parse).transpose()?,
                tone: re(&c.tone)?,
                window: Duration::from_secs(c.window),
                show_text: c.show_text,
                show_start: c.show_start,
            })
        });
        Ok(Flexalarm { rules: rules.collect::<Result<_>>()?, waiting: Vec::new() })
    }

    /// Nimmt ein Telegramm an und liefert, was jetzt weiterläuft (in Empfangsreihenfolge)
    pub fn push(&mut self, t: Telegram, now: Instant) -> Vec<Telegram> {
        let mut out = self.expire(now);
        let ric = ric(&t.address).filter(|_| t.kind == Kind::Pocsag);
        for (i, rule) in self.rules.iter().enumerate() {
            let Some(ric) = ric else { break };
            if !rule.source.as_ref().is_none_or(|re| re.is_match(&t.source)) {
                continue;
            }
            if ric == rule.text_ric && rule.text_sub.as_ref().is_none_or(|s| *s == t.sub) {
                let (mine, rest) = std::mem::take(&mut self.waiting).into_iter().partition(|w| w.rule == i && w.t.source == t.source);
                self.waiting = rest;
                out.extend(mine.into_iter().map(|w: Waiting| Telegram { message: t.message.clone(), ..w.t }));
                if rule.show_text {
                    out.push(t);
                }
                return out;
            }
            if Some(ric) == rule.start_ric {
                if rule.show_start {
                    out.push(t);
                }
                return out;
            }
            let tone = match &rule.tone {
                Some(re) => re.is_match(&t.address),
                None => t.message.trim().is_empty(),
            };
            if tone {
                self.waiting.push(Waiting { rule: i, deadline: now + rule.window, t });
                return out;
            }
        }
        out.push(t);
        out
    }

    /// Ton-RICs, deren Zeitfenster abgelaufen ist, laufen ohne Text weiter
    pub fn expire(&mut self, now: Instant) -> Vec<Telegram> {
        let (done, rest) = std::mem::take(&mut self.waiting).into_iter().partition(|w| w.deadline <= now);
        self.waiting = rest;
        done.into_iter().map(|w: Waiting| w.t).collect()
    }

    /// Wann die nächste wartende Ton-RIC abläuft
    pub fn next_deadline(&self) -> Option<Instant> {
        self.waiting.iter().map(|w| w.deadline).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::Decoded;

    fn flex(toml_text: &str) -> Flexalarm {
        #[derive(Deserialize)]
        struct F {
            flexalarm: Vec<FlexalarmConfig>,
        }
        Flexalarm::new(&toml::from_str::<F>(toml_text).unwrap().flexalarm).unwrap()
    }

    fn pocsag(source: &str, address: &str, message: &str) -> Telegram {
        Telegram::new(Decoded { kind: Kind::Pocsag, address: address.into(), sub: "a".into(), message: message.into() }, source, chrono::Local::now())
    }

    fn texts(list: &[Telegram]) -> Vec<(&str, &str)> {
        list.iter().map(|t| (t.address.as_str(), t.message.as_str())).collect()
    }

    /// Ablauf wie beim Tester: drei Ton-RICs, zwei Sekunden später die Text-RIC
    #[test]
    fn tones_get_text_of_text_ric() {
        let mut f = flex("[[flexalarm]]\nname = 'LS'\ntext_ric = '1859969'");
        let start = Instant::now();
        for ric in ["1856098", "1856107", "1856131"] {
            assert!(f.push(pocsag("Funk", ric, ""), start).is_empty());
        }
        assert_eq!(f.next_deadline(), Some(start + Duration::from_secs(3)));
        let out = f.push(pocsag("Funk", "1859969", "BMA. Einsatz"), start + Duration::from_secs(2));
        assert_eq!(
            texts(&out),
            [("1856098", "BMA. Einsatz"), ("1856107", "BMA. Einsatz"), ("1856131", "BMA. Einsatz"), ("1859969", "BMA. Einsatz")]
        );
        assert_eq!(f.next_deadline(), None);
    }

    #[test]
    fn without_text_ric_tones_continue_after_window() {
        let mut f = flex("[[flexalarm]]\nname = 'LS'\ntext_ric = '1859969'\nwindow = 2");
        let start = Instant::now();
        assert!(f.push(pocsag("Funk", "1856098", ""), start).is_empty());
        assert!(f.expire(start + Duration::from_secs(1)).is_empty());
        assert_eq!(texts(&f.expire(start + Duration::from_secs(2))), [("1856098", "")]);
        // Telegramme mit Text warten nicht
        assert_eq!(texts(&f.push(pocsag("Funk", "1234567", "Probe"), start)), [("1234567", "Probe")]);
    }

    #[test]
    fn channel_sub_start_and_hidden_text() {
        let mut f = flex(
            "[[flexalarm]]\nname = 'LS'\nsource = '^Funk$'\ntext_ric = '1859969'\ntext_sub = 'd'\nstart_ric = '1859960'\nshow_text = false\nshow_start = false",
        );
        let now = Instant::now();
        // Start-RIC wird ausgeblendet, wartet aber nicht selbst auf den Text
        assert!(f.push(pocsag("Funk", "1859960", ""), now).is_empty());
        assert!(f.push(pocsag("Funk", "1856098", ""), now).is_empty());
        // Anderer Kanal: gilt nicht
        assert_eq!(texts(&f.push(pocsag("SDR", "1856107", ""), now)), [("1856107", "")]);
        // Text-RIC mit anderer Sub ist keine Text-RIC, hat aber Text und läuft gleich weiter
        assert_eq!(texts(&f.push(pocsag("Funk", "1859969", "x"), now)), [("1859969", "x")]);
        let mut text = pocsag("Funk", "1859969", "BMA");
        text.sub = "d".into();
        assert_eq!(texts(&f.push(text, now)), [("1856098", "BMA")]);
    }

    #[test]
    fn tone_pattern() {
        let mut f = flex("[[flexalarm]]\nname = 'LS'\ntext_ric = '1859969'\ntone = '^18561'");
        let now = Instant::now();
        assert_eq!(texts(&f.push(pocsag("Funk", "1856098", ""), now)), [("1856098", "")]);
        assert!(f.push(pocsag("Funk", "1856107", ""), now).is_empty());
        assert_eq!(texts(&f.push(pocsag("Funk", "1859969", "BMA"), now)), [("1856107", "BMA"), ("1859969", "BMA")]);
    }

    #[test]
    fn invalid_ric() {
        #[derive(Deserialize)]
        struct F {
            flexalarm: Vec<FlexalarmConfig>,
        }
        let c = toml::from_str::<F>("[[flexalarm]]\nname = 'LS'\ntext_ric = 'abc'").unwrap();
        assert!(Flexalarm::new(&c.flexalarm).is_err());
        let c = toml::from_str::<F>("[[flexalarm]]\nname = 'LS'\ntext_ric = '2859969'").unwrap();
        assert!(Flexalarm::new(&c.flexalarm).is_err());
    }
}
