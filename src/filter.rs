//! Filter auf Basis regulärer Ausdrücke.
//!
//! Ein Telegramm wird verworfen, wenn ein Filter mit `exclude = true` passt.
//! Gibt es Filter ohne `exclude`, muss mindestens einer davon passen.

use crate::telegram::{Kind, Telegram};
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterConfig {
    pub name: String,
    pub kind: Option<Kind>,
    pub address: Option<String>,
    pub sub: Option<String>,
    pub message: Option<String>,
    pub ort: Option<String>,
    pub kurz: Option<String>,
    pub lang: Option<String>,
    #[serde(default)]
    pub exclude: bool,
}

pub struct Filter {
    kind: Option<Kind>,
    address: Option<Regex>,
    sub: Option<Regex>,
    message: Option<Regex>,
    ort: Option<Regex>,
    kurz: Option<Regex>,
    lang: Option<Regex>,
    exclude: bool,
}

impl Filter {
    pub fn new(c: &FilterConfig) -> Result<Filter> {
        let re = |pattern: &Option<String>| -> Result<Option<Regex>> {
            pattern
                .as_deref()
                .map(Regex::new)
                .transpose()
                .with_context(|| format!("Filter '{}'", c.name))
        };
        Ok(Filter {
            kind: c.kind,
            address: re(&c.address)?,
            sub: re(&c.sub)?,
            message: re(&c.message)?,
            ort: re(&c.ort)?,
            kurz: re(&c.kurz)?,
            lang: re(&c.lang)?,
            exclude: c.exclude,
        })
    }

    pub fn matches(&self, d: &Telegram) -> bool {
        let field = |re: &Option<Regex>, value: &str| re.as_ref().is_none_or(|re| re.is_match(value));
        self.kind.is_none_or(|k| k == d.kind)
            && field(&self.address, &d.address)
            && field(&self.sub, &d.sub)
            && field(&self.message, &d.message)
            && field(&self.ort, &d.ort)
            && field(&self.kurz, &d.kurz)
            && field(&self.lang, &d.lang)
    }
}

pub struct Filters(Vec<Filter>);

impl Filters {
    pub fn new(configs: &[FilterConfig]) -> Result<Filters> {
        Ok(Filters(configs.iter().map(Filter::new).collect::<Result<_>>()?))
    }

    pub fn allows(&self, d: &Telegram) -> bool {
        if self.0.iter().any(|f| f.exclude && f.matches(d)) {
            return false;
        }
        let mut includes = self.0.iter().filter(|f| !f.exclude).peekable();
        includes.peek().is_none() || includes.any(|f| f.matches(d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::Decoded;

    fn telegram(d: Decoded) -> Telegram {
        Telegram::new(d, "Test", chrono::Local::now())
    }

    fn pocsag(address: &str, message: &str) -> Telegram {
        telegram(Decoded { kind: Kind::Pocsag, address: address.into(), sub: "a".into(), message: message.into() })
    }

    fn filters(toml_text: &str) -> Filters {
        #[derive(Deserialize)]
        struct F {
            filter: Vec<FilterConfig>,
        }
        Filters::new(&toml::from_str::<F>(toml_text).unwrap().filter).unwrap()
    }

    #[test]
    fn no_filters_allow_everything() {
        assert!(Filters::new(&[]).unwrap().allows(&pocsag("1234567", "")));
    }

    #[test]
    fn include_and_exclude() {
        let f = filters(
            r#"
            [[filter]]
            name = "Eigene Schleifen"
            kind = "pocsag"
            address = "^18599"

            [[filter]]
            name = "Probealarm ausblenden"
            message = "(?i)probealarm"
            exclude = true
            "#,
        );
        assert!(f.allows(&pocsag("1859992", "Brand 3")));
        assert!(!f.allows(&pocsag("1859992", "PROBEALARM")));
        assert!(!f.allows(&pocsag("1234567", "Brand 3")));
        let fms = telegram(Decoded { kind: Kind::Fms, address: "18599000".into(), sub: "3".into(), message: String::new() });
        assert!(!f.allows(&fms));
    }

    #[test]
    fn description_fields() {
        let f = filters("[[filter]]\nname = 'Ort'\nort = '^Muster'");
        let mut t = pocsag("1234567", "");
        assert!(!f.allows(&t));
        t.ort = "Musterstadt".into();
        assert!(f.allows(&t));
    }

    #[test]
    fn invalid_regex_is_reported() {
        let c: FilterConfig = toml::from_str("name = 'kaputt'\naddress = '('").unwrap();
        let err = Filters::new(&[c]).err().unwrap();
        assert!(format!("{err:#}").contains("kaputt"));
    }
}
