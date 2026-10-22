//! Beschreibungen (Adressbuch) für POCSAG-Adressen: Ort, Kurz- und Langbeschreibung.
//!
//! Adressen sind RIC (7-stellig) und Sub `a`–`d`, oder `*` für alle Subs einer RIC.
//! Import als CSV im eigenen Format (mit Kopfzeile) oder im BosMon-Format (ohne).

use anyhow::{Result, bail};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Description {
    pub address: String,
    #[serde(default)]
    pub ort: String,
    #[serde(default)]
    pub kurz: String,
    #[serde(default)]
    pub lang: String,
}

const HEADER: [&str; 4] = ["adresse", "ort", "kurzbeschreibung", "langbeschreibung"];

/// `1234567a`, `123 b`, `1234567*` → einheitliche Schreibweise (`1234567a`, `0000123b`, …).
pub fn normalize_address(address: &str) -> Result<String> {
    let compact: String = address.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    let split = compact.len().saturating_sub(1);
    let (ric, sub) = compact.split_at(split);
    if ric.is_empty() || ric.len() > 7 || !ric.bytes().all(|b| b.is_ascii_digit()) || !matches!(sub, "a" | "b" | "c" | "d" | "*") {
        bail!("ungültige Adresse „{}“ (erwartet RIC und Sub a–d oder *, z. B. 1234567a)", address.trim());
    }
    Ok(format!("{ric:0>7}{sub}"))
}

impl Description {
    fn normalized(mut self) -> Result<Description> {
        self.address = normalize_address(&self.address)?;
        for field in [&mut self.ort, &mut self.kurz, &mut self.lang] {
            *field = field.trim().to_string();
        }
        Ok(self)
    }
}

/// Ergebnis eines Imports
#[derive(Debug, Default, Serialize)]
pub struct Import {
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    /// „Zeile 5: …“
    pub errors: Vec<String>,
    #[serde(skip)]
    pub delete: Vec<String>,
    #[serde(skip)]
    pub insert: Vec<Description>,
}

/// CSV-Datei lesen und mit den vorhandenen Beschreibungen abgleichen. Mit `clear` werden alle
/// vorhandenen gelöscht, sonst nur die mit geänderten Einträgen ersetzt.
pub fn import(data: &[u8], existing: &[Description], clear: bool) -> Import {
    let text = decode(data);
    let first_line = text.lines().next().unwrap_or_default();
    let delimiter = b";,\t".iter().copied().max_by_key(|d| fields(first_line, *d)).unwrap_or(b',');
    let reader = csv::ReaderBuilder::new().has_headers(false).flexible(true).delimiter(delimiter).from_reader(text.as_bytes());

    let mut out = Import::default();
    let mut incoming: HashMap<String, Description> = HashMap::new();
    let mut order = Vec::new();
    for (n, record) in reader.into_records().enumerate() {
        let line = n + 1;
        let record = match record {
            Ok(r) => r,
            Err(e) => {
                out.errors.push(format!("Zeile {line}: {e}"));
                continue;
            }
        };
        let field = |i: usize| record.get(i).map(repair).unwrap_or_default();
        if n == 0 && field(0).trim().eq_ignore_ascii_case(HEADER[0]) {
            continue;
        }
        if record.iter().all(|f| f.trim().is_empty()) {
            continue;
        }
        let d = Description { address: field(0), ort: field(1), kurz: field(2), lang: field(3) };
        match d.normalized() {
            Ok(d) => {
                if !incoming.contains_key(&d.address) {
                    order.push(d.address.clone());
                }
                incoming.insert(d.address.clone(), d);
            }
            Err(e) => out.errors.push(format!("Zeile {line}: {e}")),
        }
    }

    let existing: HashMap<&str, &Description> = existing.iter().map(|d| (d.address.as_str(), d)).collect();
    for address in order {
        let d = incoming.remove(&address).expect("eingelesen");
        match existing.get(address.as_str()) {
            None => out.new += 1,
            Some(old) if **old == d => {
                out.unchanged += 1;
                if !clear {
                    continue;
                }
            }
            Some(_) => {
                out.changed += 1;
                if !clear {
                    out.delete.push(address);
                }
            }
        }
        out.insert.push(d);
    }
    out
}

/// Anzahl der Felder einer Zeile mit diesem Trennzeichen (zum Erkennen des Trennzeichens)
fn fields(line: &str, delimiter: u8) -> usize {
    let mut reader = csv::ReaderBuilder::new().has_headers(false).delimiter(delimiter).from_reader(line.as_bytes());
    reader.records().next().and_then(|r| r.ok()).map_or(0, |r| r.len())
}

/// UTF-8 (mit oder ohne BOM), sonst Windows-1252 (ältere Windows-Programme)
fn decode(data: &[u8]) -> String {
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    match std::str::from_utf8(data) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode_without_bom_handling(data).0.into_owned(),
    }
}

/// Repariert doppelt kodierte Umlaute („FÃ¼hrung“ → „Führung“), wie sie entstehen, wenn
/// UTF-8 als Windows-1252 gelesen und wieder gespeichert wurde. Auch mehrfach und gemischt.
pub fn repair(text: &str) -> String {
    let byte = |c: char| -> Option<u8> {
        if (c as u32) < 0x100 {
            return Some(c as u8);
        }
        let mut buf = [0u8; 4];
        let (bytes, _, errors) = encoding_rs::WINDOWS_1252.encode(c.encode_utf8(&mut buf));
        (!errors && bytes.len() == 1).then(|| bytes[0])
    };
    let mut text = text.to_string();
    for _ in 0..3 {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < chars.len() {
            let len = match byte(chars[i]) {
                Some(0xC2..=0xDF) => 2,
                Some(0xE0..=0xEF) => 3,
                Some(0xF0..=0xF4) => 4,
                _ => 0,
            };
            let bytes: Option<Vec<u8>> = chars.get(i..i + len).filter(|_| len > 0).and_then(|cs| cs.iter().map(|c| byte(*c)).collect());
            if let Some(decoded) = bytes.as_deref().and_then(|b| std::str::from_utf8(b).ok()) {
                out.push_str(decoded);
                i += len;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        if out == text {
            break;
        }
        text = out;
    }
    // „ß“ ist manchmal als „ÃY“ übrig (das Zeichen Ÿ wurde dabei zu Y)
    text.replace("ÃY", "ß")
}

/// CSV im eigenen Format (mit Kopfzeile, Semikolon), nach Adresse sortiert.
pub fn export(all: &[Description]) -> Result<Vec<u8>> {
    let mut sorted: Vec<&Description> = all.iter().collect();
    sorted.sort_by(|a, b| a.address.cmp(&b.address));
    let mut writer = csv::WriterBuilder::new().delimiter(b';').from_writer(Vec::new());
    writer.write_record(HEADER)?;
    for d in sorted {
        writer.write_record([&d.address, &d.ort, &d.kurz, &d.lang])?;
    }
    Ok(writer.into_inner()?)
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Search {
    /// Regulärer Ausdruck (ohne Beachtung der Groß-/Kleinschreibung) über alle Spalten
    #[serde(default)]
    pub q: String,
    /// address, ort, kurz oder lang
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub desc: bool,
    #[serde(default)]
    pub offset: usize,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    /// Anzahl aller Treffer
    pub total: usize,
    pub rows: Vec<Description>,
    /// Hinweis, z. B. wenn die Suche kein gültiger regulärer Ausdruck ist
    pub notice: Option<String>,
}

pub fn search(all: &[Description], s: &Search) -> Page {
    let build = |pattern: &str| RegexBuilder::new(pattern).case_insensitive(true).build();
    let (re, notice): (Regex, _) = match build(&s.q) {
        Ok(re) => (re, None),
        Err(_) => (build(&regex::escape(&s.q)).expect("escaped"), Some("Kein gültiger regulärer Ausdruck, gesucht wird nach dem Text".to_string())),
    };
    let mut hits: Vec<&Description> =
        all.iter().filter(|d| s.q.is_empty() || [&d.address, &d.ort, &d.kurz, &d.lang].iter().any(|f| re.is_match(f))).collect();
    let column = |d: &Description| -> String {
        match s.sort.as_str() {
            "ort" => d.ort.to_lowercase(),
            "kurz" => d.kurz.to_lowercase(),
            "lang" => d.lang.to_lowercase(),
            _ => d.address.clone(),
        }
    };
    hits.sort_by_cached_key(|d| (column(d), d.address.clone()));
    if s.desc {
        hits.reverse();
    }
    let limit = s.limit.unwrap_or(100).min(1000);
    let rows = hits.iter().skip(s.offset).take(limit).map(|d| (*d).clone()).collect();
    Page { total: hits.len(), rows, notice }
}

/// Eine Beschreibung im Editor anlegen oder ändern (`old`: bisherige Adresse).
/// Gibt die zu löschenden Adressen und den neuen Eintrag zurück.
pub fn edit(all: &[Description], old: Option<&str>, d: Description) -> Result<(Vec<String>, Description)> {
    let d = d.normalized()?;
    if old != Some(d.address.as_str()) && all.iter().any(|x| x.address == d.address) {
        bail!("Die Adresse {} gibt es schon", d.address);
    }
    let delete = old.into_iter().map(str::to_string).chain([d.address.clone()]).collect();
    Ok((delete, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(address: &str, ort: &str) -> Description {
        Description { address: address.into(), ort: ort.into(), kurz: String::new(), lang: String::new() }
    }

    #[test]
    fn addresses() {
        assert_eq!(normalize_address("1234567a").unwrap(), "1234567a");
        assert_eq!(normalize_address(" 123 B ").unwrap(), "0000123b");
        assert_eq!(normalize_address("0082643*").unwrap(), "0082643*");
        for bad in ["1234567", "1234567e", "12345678a", "1234567 1", "abc*", "*", ""] {
            assert!(normalize_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn repairs_double_encoding() {
        assert_eq!(repair("FF SÃ¼lzegrung"), "FF Sülzegrung");
        assert_eq!(repair("Planung Ã¶ffentl. RD, GroÃŸveranst."), "Planung öffentl. RD, Großveranst.");
        assert_eq!(repair("FF HohenmÃƒÂ¶sen"), "FF Hohenmösen");
        assert_eq!(repair("FF Bad Kösen | FÃ¼hrungskrÃ¤fte"), "FF Bad Kösen | Führungskräfte");
        assert_eq!(repair("Dessau-RoÃYlau"), "Dessau-Roßlau");
        assert_eq!(repair("Größe 5 € – ok"), "Größe 5 € – ok");
    }

    #[test]
    fn import_bosmon() {
        let data = "\u{feff}\"0082643*\",\"RD\",\"\",\"RD BLK -\"\r\n\"1938857a\",\"Burgenlandkreis (SA)\",\"BLK FW\",\"BLK FW Trebnitz\"\r\n\"kaputt\",\"\",\"\",\"\"\r\n\"1921560*\",\"FW BLK\",\"\",\"FF SÃ¼lzegrung\"\r\n";
        let existing = [d("0082643*", "RD"), d("1938857a", "alt"), d("7777777*", "bleibt")];
        let mut existing_ok = existing[0].clone();
        existing_ok.lang = "RD BLK -".into();
        let existing = [existing_ok, existing[1].clone(), existing[2].clone()];
        let r = import(data.as_bytes(), &existing, false);
        assert_eq!((r.new, r.changed, r.unchanged), (1, 1, 1));
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].starts_with("Zeile 3: "), "{:?}", r.errors);
        assert_eq!(r.delete, ["1938857a"]);
        assert_eq!(r.insert.iter().map(|d| d.address.as_str()).collect::<Vec<_>>(), ["1938857a", "1921560*"]);
        assert_eq!(r.insert[1].lang, "FF Sülzegrung");
        assert_eq!(r.insert[0].kurz, "BLK FW");

        let r = import(data.as_bytes(), &existing, true);
        assert_eq!(r.insert.len(), 3);
        assert!(r.delete.is_empty());
    }

    #[test]
    fn import_own_format_and_windows_1252() {
        let export = export(&[d("1234567b", "Süd; Mitte"), d("0000001*", "Nord")]).unwrap();
        let text = String::from_utf8(export.clone()).unwrap();
        assert!(text.starts_with("adresse;ort;kurzbeschreibung;langbeschreibung\n0000001*;Nord;;\n"), "{text}");
        let r = import(&export, &[], false);
        assert_eq!((r.new, r.errors.len()), (2, 0));
        assert_eq!(r.insert[1].ort, "Süd; Mitte");

        let cp1252 = b"1234567a;M\xfcnchen;FW;Gro\xdfalarm\n";
        let r = import(cp1252, &[], false);
        assert_eq!((r.insert[0].ort.as_str(), r.insert[0].lang.as_str()), ("München", "Großalarm"));
    }

    #[test]
    fn search_sort_page() {
        let all: Vec<_> = (0..250).map(|i| d(&format!("{i:07}a"), if i % 2 == 0 { "Gerade" } else { "Ungerade" })).collect();
        let page = search(&all, &Search { q: "^ger".into(), ..Default::default() });
        assert_eq!((page.total, page.rows.len()), (125, 100));
        let page = search(&all, &Search { q: "ungerade".into(), desc: true, offset: 120, ..Default::default() });
        assert_eq!(page.rows.iter().map(|d| d.address.as_str()).collect::<Vec<_>>(), ["0000009a", "0000007a", "0000005a", "0000003a", "0000001a"]);
        let page = search(&all, &Search { q: "(".into(), ..Default::default() });
        assert!(page.notice.is_some());
        assert_eq!(page.total, 0);
    }

    #[test]
    fn edit_checks_duplicates() {
        let all = [d("1234567a", "x"), d("1234567b", "y")];
        assert!(edit(&all, None, d("1234567 A", "neu")).is_err());
        let (delete, new) = edit(&all, Some("1234567a"), d("1234567c", "z")).unwrap();
        assert_eq!((delete, new.address.as_str()), (vec!["1234567a".to_string(), "1234567c".to_string()], "1234567c"));
        assert!(edit(&all, Some("1234567a"), d("1234567a", "geändert")).is_ok());
    }
}
