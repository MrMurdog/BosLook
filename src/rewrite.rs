//! Bearbeitungsregeln: Nachrichten per regulärem Ausdruck ändern und RICs verschieben.
//!
//! Alle passenden Regeln werden der Reihe nach angewendet, jede sieht das Ergebnis der
//! vorherigen. Nach einer Regel mit `stop = true` werden keine weiteren geprüft.

use crate::db::Db;
use crate::filter::{Filter, FilterConfig};
use crate::telegram::{Kind, Telegram};
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

/// Größte RIC, die POCSAG übertragen kann (21 Bit)
const MAX_RIC: i64 = 9_999_999;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewriteConfig {
    pub name: String,
    pub kind: Option<Kind>,
    /// Name des Eingangs (Reiter)
    pub source: Option<String>,
    pub address: Option<String>,
    pub sub: Option<String>,
    pub message: Option<String>,
    pub ort: Option<String>,
    pub kurz: Option<String>,
    pub lang: Option<String>,
    /// Ersetzungen in der Nachricht, nacheinander
    #[serde(default)]
    pub replace: Vec<ReplaceConfig>,
    /// Zur RIC addieren (nur POCSAG)
    pub address_add: Option<i64>,
    #[serde(default)]
    pub stop: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceConfig {
    pub pattern: String,
    /// `$1`, `$2` … = Klammern aus `pattern`
    #[serde(default)]
    pub with: String,
}

struct Rewrite {
    name: String,
    filter: Filter,
    replace: Vec<(Regex, String)>,
    address_add: Option<i64>,
    stop: bool,
}

pub struct Rewrites(Vec<Rewrite>);

impl Rewrites {
    pub fn new(configs: &[RewriteConfig]) -> Result<Rewrites> {
        let rules = configs.iter().map(|c| {
            let context = || format!("Bearbeitung '{}'", c.name);
            let filter = Filter::new(&FilterConfig {
                name: c.name.clone(),
                kind: c.kind,
                source: c.source.clone(),
                address: c.address.clone(),
                sub: c.sub.clone(),
                message: c.message.clone(),
                ort: c.ort.clone(),
                kurz: c.kurz.clone(),
                lang: c.lang.clone(),
                exclude: false,
            })
            .with_context(context)?;
            let replace = c.replace.iter().map(|r| Ok((Regex::new(&r.pattern).with_context(context)?, r.with.clone()))).collect::<Result<_>>()?;
            Ok(Rewrite { name: c.name.clone(), filter, replace, address_add: c.address_add, stop: c.stop })
        });
        Ok(Rewrites(rules.collect::<Result<_>>()?))
    }

    /// Regeln aus den Werten des Formulars (Abschnitt „Bearbeitung“), z. B. für das Testfeld
    pub fn from_form(rules: &serde_json::Value) -> Result<Rewrites> {
        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            rewrite: Vec<RewriteConfig>,
        }
        let schema: Vec<_> = crate::form::server_schema(&[]).into_iter().filter(|s| s.key == "rewrite").collect();
        let text = crate::form::apply("", &schema, &serde_json::json!({ "rewrite": rules }))?;
        Rewrites::new(&toml::from_str::<File>(&text)?.rewrite)
    }

    /// Wendet die passenden Regeln an und liefert ihre Namen. Ändert eine Regel die RIC, holt
    /// `describe` die Beschreibung der neuen, damit folgende Regeln schon deren Ort sehen.
    pub fn apply(&self, t: &mut Telegram, mut describe: impl FnMut(&mut Telegram) -> Result<()>) -> Result<Vec<&str>> {
        let mut applied = Vec::new();
        for rule in &self.0 {
            if !rule.filter.matches(t) {
                continue;
            }
            applied.push(rule.name.as_str());
            for (re, with) in &rule.replace {
                if let std::borrow::Cow::Owned(message) = re.replace_all(&t.message, with.as_str()) {
                    t.message = message;
                }
            }
            if let Some(add) = rule.address_add.filter(|_| t.kind == Kind::Pocsag) {
                match t.address.parse::<i64>().map(|ric| ric + add) {
                    Ok(ric @ 0..=MAX_RIC) => {
                        t.address = format!("{ric:07}");
                        (t.ort, t.kurz, t.lang) = Default::default();
                        describe(t)?;
                    }
                    _ => eprintln!("Bearbeitung '{}': RIC {} {add:+} ergibt keine gültige RIC", rule.name, t.address),
                }
            }
            if rule.stop {
                break;
            }
        }
        Ok(applied)
    }

    /// Beschreibung nachschlagen und Regeln anwenden; die Regeln laufen auch, wenn das
    /// Nachschlagen fehlschlägt.
    pub fn describe_and_apply(&self, db: &Db, t: &mut Telegram) -> Result<Vec<&str>> {
        let described = db.describe(t);
        let applied = self.apply(t, |t| db.describe(t))?;
        described?;
        Ok(applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::Decoded;

    fn rewrites(toml_text: &str) -> Rewrites {
        #[derive(Deserialize)]
        struct R {
            rewrite: Vec<RewriteConfig>,
        }
        Rewrites::new(&toml::from_str::<R>(toml_text).unwrap().rewrite).unwrap()
    }

    fn pocsag(address: &str, message: &str) -> Telegram {
        Telegram::new(Decoded { kind: Kind::Pocsag, address: address.into(), sub: "a".into(), message: message.into() }, "Test", chrono::Local::now())
    }

    #[test]
    fn replace_in_order_with_groups() {
        let r = rewrites(
            r#"
            [[rewrite]]
            name = "Stichwort"
            address = "^18599"
            [[rewrite.replace]]
            pattern = "^F(\\d)\\b"
            with = "Feuer $1"
            [[rewrite.replace]]
            pattern = "\\s+"
            with = " "
            "#,
        );
        let mut t = pocsag("1859991", "F2   Musterstr.  1");
        assert_eq!(r.apply(&mut t, |_| Ok(())).unwrap(), ["Stichwort"]);
        assert_eq!(t.message, "Feuer 2 Musterstr. 1");
        let mut other = pocsag("1234567", "F2");
        assert!(r.apply(&mut other, |_| Ok(())).unwrap().is_empty());
        assert_eq!(other.message, "F2");
    }

    #[test]
    fn later_rules_see_result_and_stop() {
        let r = rewrites(
            r#"
            [[rewrite]]
            name = "A"
            [[rewrite.replace]]
            pattern = "x"
            with = "y"
            [[rewrite]]
            name = "B"
            message = "^y$"
            stop = true
            [[rewrite.replace]]
            pattern = "y"
            with = "z"
            [[rewrite]]
            name = "C"
            [[rewrite.replace]]
            pattern = "z"
            with = "!"
            "#,
        );
        let mut t = pocsag("1", "x");
        assert_eq!(r.apply(&mut t, |_| Ok(())).unwrap(), ["A", "B"]);
        assert_eq!(t.message, "z");
    }

    #[test]
    fn address_add() {
        let r = rewrites("[[rewrite]]\nname = \"Plus\"\naddress_add = 1000\n");
        let mut t = pocsag("0001234", "");
        r.apply(&mut t, |_| Ok(())).unwrap();
        assert_eq!(t.address, "0002234");
        let mut top = pocsag("2097000", "");
        r.apply(&mut top, |_| Ok(())).unwrap();
        assert_eq!(top.address, "2097000", "über der größten RIC bleibt sie unverändert");
        let mut zvei = Telegram::new(Decoded { kind: Kind::Zvei, address: "12345".into(), sub: String::new(), message: String::new() }, "Test", chrono::Local::now());
        r.apply(&mut zvei, |_| Ok(())).unwrap();
        assert_eq!(zvei.address, "12345");
        let minus = rewrites("[[rewrite]]\nname = \"Minus\"\naddress_add = -2000\n");
        let mut t = pocsag("0001234", "");
        minus.apply(&mut t, |_| Ok(())).unwrap();
        assert_eq!(t.address, "0001234");
    }

    #[test]
    fn changed_address_gets_new_description() {
        let dir = std::env::temp_dir().join(format!("boslook-rewrite-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let db = Db::open(dir.to_str().unwrap()).unwrap();
        let d = |address: &str, ort: &str| crate::descriptions::Description { address: address.into(), ort: ort.into(), kurz: String::new(), lang: String::new() };
        db.write_descriptions(false, &[], &[d("0001234*", "Alt"), d("0002234a", "Neu")]).unwrap();
        let r = rewrites("[[rewrite]]\nname = \"Plus\"\nort = \"^Alt$\"\naddress_add = 1000\n[[rewrite]]\nname = \"Danach\"\nort = \"^Neu$\"\n");
        let mut t = pocsag("0001234", "");
        assert_eq!(r.describe_and_apply(&db, &mut t).unwrap(), ["Plus", "Danach"]);
        assert_eq!((t.address.as_str(), t.ort.as_str()), ("0002234", "Neu"));
        drop(db);
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn invalid_pattern_names_rule() {
        #[derive(Deserialize)]
        struct R {
            rewrite: Vec<RewriteConfig>,
        }
        let c = toml::from_str::<R>("[[rewrite]]\nname = \"Kaputt\"\n[[rewrite.replace]]\npattern = \"(\"\n").unwrap();
        let err = Rewrites::new(&c.rewrite).err().unwrap();
        assert!(format!("{err:#}").starts_with("Bearbeitung 'Kaputt'"), "{err:#}");
    }
}
