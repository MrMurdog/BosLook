//! Benachrichtigungen per Telegram, E-Mail, Pushover und Plugins.
//!
//! `[[notifier]]` beschreibt ein Ziel, `[[alarm]]` eine Regel (Felder wie bei den Filtern),
//! die bei passenden Telegrammen die genannten Ziele benachrichtigt.

use crate::filter::{Filter, FilterConfig};
use crate::plugin::{Plugin, Plugins};
use crate::telegram::{Kind, Telegram};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
#[serde(try_from = "toml::Table")]
pub struct NotifierConfig {
    pub name: String,
    pub target: Target,
}

/// `type` ist ein eingebauter Dienst oder der Name eines Plugins mit der Fähigkeit `notify`.
impl TryFrom<toml::Table> for NotifierConfig {
    type Error = String;

    fn try_from(mut table: toml::Table) -> Result<NotifierConfig, String> {
        let name = match table.remove("name") {
            Some(toml::Value::String(name)) => name,
            _ => return Err("notifier: name fehlt".into()),
        };
        let kind = table.get("type").and_then(toml::Value::as_str).map(str::to_string);
        let target = match kind.as_deref() {
            Some("telegram" | "pushover" | "email") => table.try_into().map_err(|e: toml::de::Error| format!("notifier '{name}': {}", e.message()))?,
            Some(_) => {
                let plugin = kind.expect("type");
                table.remove("type");
                Target::Plugin { plugin, settings: table }
            }
            None => return Err(format!("notifier '{name}': type fehlt")),
        };
        Ok(NotifierConfig { name, target })
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Target {
    /// Bot über @BotFather anlegen; `chat_id` des Empfängers oder der Gruppe
    Telegram { bot_token: String, chat_id: String },
    /// `token` der Pushover-Anwendung, `user` = User- oder Gruppen-Key
    Pushover { token: String, user: String },
    Email {
        server: String,
        /// Standard: 587 bei starttls, 465 bei tls, 25 bei none
        port: Option<u16>,
        #[serde(default)]
        security: Security,
        username: Option<String>,
        password: Option<String>,
        from: String,
        to: Vec<String>,
    },
    /// Dienst eines Plugins; `settings` sind die übrigen Einträge des Ziels
    #[serde(skip)]
    Plugin { plugin: String, settings: toml::Table },
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    #[default]
    Starttls,
    Tls,
    /// Unverschlüsselt, nur für einen Mailserver im eigenen Netz
    None,
}

/// Bei Pushover: high = Ton auch bei „Nicht stören", emergency = wiederholt, bis bestätigt.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    #[default]
    Normal,
    High,
    Emergency,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlarmConfig {
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
    /// Namen der `[[notifier]]`, die benachrichtigt werden
    pub notify: Vec<String>,
    #[serde(default)]
    pub priority: Priority,
    /// Vorlagen mit Platzhaltern (siehe [`fill`]); ohne: Namen der Regeln bzw. [`text`]
    pub title: Option<String>,
    pub text: Option<String>,
}

struct Alarm {
    name: String,
    filter: Filter,
    targets: Vec<usize>,
    priority: Priority,
    title: Option<String>,
    text: Option<String>,
}

/// Prüft Telegramme gegen die Alarmregeln und gibt Nachrichten an die Ziele weiter.
/// Jedes Ziel sendet in einem eigenen Thread, damit ein langsamer Dienst die anderen nicht aufhält.
pub struct Notify {
    alarms: Vec<Alarm>,
    senders: Vec<mpsc::Sender<(String, String, Priority, Telegram)>>,
    repeats: Repeats,
}

/// Erkennt Wiederholungen: gleiche Telegramme innerhalb von `window` nur einmal melden
/// (Alarme werden oft mehrfach gesendet). Das Fenster zählt ab der letzten Wiederholung.
pub struct Repeats {
    window: Duration,
    seen: HashMap<String, Instant>,
}

impl Repeats {
    pub fn new(window: Duration) -> Repeats {
        Repeats { window, seen: HashMap::new() }
    }

    pub fn is_repeat(&mut self, t: &Telegram, now: Instant) -> bool {
        let window = self.window;
        self.seen.retain(|_, last| now.duration_since(*last) < window);
        let key = format!("{}|{}|{}|{}", t.kind.as_str(), t.address, t.sub, t.message);
        self.seen.insert(key, now).is_some()
    }
}

impl Notify {
    /// Muss innerhalb einer Tokio-Laufzeit aufgerufen werden (für Plugins).
    pub fn new(alarms: &[AlarmConfig], notifiers: Vec<NotifierConfig>, repeat: Duration, plugins: &Plugins) -> Result<Notify> {
        let alarms = alarms.iter().map(|a| compile(a, &notifiers)).collect::<Result<_>>()?;
        let senders = notifiers
            .into_iter()
            .map(|n| {
                let (tx, rx) = mpsc::channel::<(String, String, Priority, Telegram)>();
                let sender = Sender::new(&n.target, plugins);
                std::thread::spawn(move || {
                    for (title, body, priority, t) in rx {
                        if let Err(e) = sender.send(&n, &title, &body, priority, Some(&t)) {
                            eprintln!("Benachrichtigung '{}': {e:#}", n.name);
                        }
                    }
                });
                tx
            })
            .collect();
        Ok(Notify { alarms, senders, repeats: Repeats::new(repeat) })
    }

    pub fn handle(&mut self, t: &Telegram) {
        if self.repeats.is_repeat(t, Instant::now()) {
            return;
        }
        for (target, title, body, priority) in targets(&self.alarms, t) {
            let _ = self.senders[target].send((title, body, priority, t.clone()));
        }
    }
}

/// Versand an ein Ziel: eingebaute Dienste direkt, Plugin-Dienste über ihr Plugin
pub struct Sender {
    plugin: Option<(Arc<Plugin>, tokio::runtime::Handle)>,
}

impl Sender {
    /// Muss bei Plugin-Zielen innerhalb einer Tokio-Laufzeit aufgerufen werden.
    pub fn new(target: &Target, plugins: &Plugins) -> Sender {
        let plugin = match target {
            Target::Plugin { plugin, .. } => plugins.get(plugin).map(|p| (p, tokio::runtime::Handle::current())),
            _ => None,
        };
        Sender { plugin }
    }

    pub fn send(&self, n: &NotifierConfig, title: &str, body: &str, priority: Priority, t: Option<&Telegram>) -> Result<()> {
        match (&n.target, &self.plugin) {
            (Target::Plugin { settings, .. }, Some((plugin, runtime))) => plugin.notify_blocking(runtime, &n.name, settings, (title, body, priority), t),
            (Target::Plugin { plugin, .. }, None) => bail!("Plugin '{plugin}' läuft nicht"),
            (target, _) => send(target, title, body, priority),
        }
    }
}

/// Prüft die Alarmregeln, ohne Threads zu starten.
pub fn check(alarms: &[AlarmConfig], notifiers: &[NotifierConfig]) -> Result<()> {
    alarms.iter().try_for_each(|a| compile(a, notifiers).map(|_| ()))
}

fn compile(a: &AlarmConfig, notifiers: &[NotifierConfig]) -> Result<Alarm> {
    let filter = Filter::new(&FilterConfig {
        name: a.name.clone(),
        kind: a.kind,
        source: a.source.clone(),
        address: a.address.clone(),
        sub: a.sub.clone(),
        message: a.message.clone(),
        ort: a.ort.clone(),
        kurz: a.kurz.clone(),
        lang: a.lang.clone(),
        exclude: false,
    })?;
    let targets = a
        .notify
        .iter()
        .map(|name| {
            notifiers
                .iter()
                .position(|n| &n.name == name)
                .with_context(|| format!("Alarm '{}': unbekannter notifier '{name}'", a.name))
        })
        .collect::<Result<Vec<_>>>()?;
    if targets.is_empty() {
        bail!("Alarm '{}': notify ist leer", a.name);
    }
    Ok(Alarm { name: a.name.clone(), filter, targets, priority: a.priority, title: a.title.clone(), text: a.text.clone() })
}

/// Welche Ziele mit welchem Titel und Text benachrichtigt werden. Passen mehrere Regeln auf
/// dasselbe Ziel, gibt es trotzdem nur eine Nachricht (mit der höchsten Priorität und den
/// Vorlagen der ersten passenden Regel).
fn targets(alarms: &[Alarm], t: &Telegram) -> Vec<(usize, String, String, Priority)> {
    let mut out: Vec<(usize, Vec<&Alarm>, Priority)> = Vec::new();
    for a in alarms.iter().filter(|a| a.filter.matches(t)) {
        for &target in &a.targets {
            match out.iter_mut().find(|(i, _, _)| *i == target) {
                Some((_, matched, priority)) => {
                    matched.push(a);
                    *priority = (*priority).max(a.priority);
                }
                None => out.push((target, vec![a], a.priority)),
            }
        }
    }
    out.into_iter()
        .map(|(i, matched, priority)| {
            let names = matched.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
            let first = matched[0];
            let title = fill(first.title.as_deref().unwrap_or("{alarm}"), t, &names);
            let body = first.text.as_deref().map_or_else(|| text(t), |template| fill(template, t, &names));
            (i, title, body, priority)
        })
        .collect()
}

/// Platzhalter für Vorlagen (Benachrichtigungen, Desktop-Ereignisse), für Hilfetexte
pub const PLACEHOLDERS: &str = "{kind} {address} {sub} {message} {ort} {kurz} {lang} {source} {time} {alarm}";

/// Ersetzt die Platzhalter aus [`PLACEHOLDERS`]; `alarm` sind die Namen der passenden Regeln.
pub fn fill(template: &str, t: &Telegram, alarm: &str) -> String {
    let mut out = String::with_capacity(template.len() + 64);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find('}').map_or(0, |e| e + 1);
        let value = match &rest[..end] {
            "{kind}" => t.kind.as_str().to_uppercase(),
            "{address}" => t.address.clone(),
            "{sub}" => t.sub.clone(),
            "{message}" => t.message.clone(),
            "{ort}" => t.ort.clone(),
            "{kurz}" => t.kurz.clone(),
            "{lang}" => t.lang.clone(),
            "{source}" => t.source.clone(),
            "{time}" => t.time.format("%d.%m.%Y %H:%M:%S").to_string(),
            "{alarm}" => alarm.to_string(),
            _ => {
                out.push('{');
                rest = &rest[1..];
                continue;
            }
        };
        out.push_str(&value);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Standardtext ohne Vorlage
pub fn text(t: &Telegram) -> String {
    let mut head = format!("{} {}", t.kind.as_str().to_uppercase(), t.address);
    if !t.sub.is_empty() {
        head = format!("{head} {}", t.sub);
    }
    let mut text = head;
    for line in [t.message.clone(), [t.ort.as_str(), t.kurz.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ")] {
        if !line.is_empty() {
            text = format!("{text}\n{line}");
        }
    }
    format!("{text}\n{} · {}", t.source, t.time.format("%d.%m.%Y %H:%M:%S"))
}

pub fn send(target: &Target, title: &str, body: &str, priority: Priority) -> Result<()> {
    match target {
        Target::Telegram { bot_token, chat_id } => post_form(
            &format!("https://api.telegram.org/bot{bot_token}/sendMessage"),
            &[("chat_id", chat_id), ("text", &format!("{title}\n{body}"))],
        ),
        Target::Pushover { token, user } => {
            let mut form = vec![("token", token.as_str()), ("user", user), ("title", title), ("message", body)];
            match priority {
                Priority::Normal => {}
                Priority::High => form.push(("priority", "1")),
                // Notfall: alle 60 s wiederholen, höchstens eine Stunde lang
                Priority::Emergency => form.extend([("priority", "2"), ("retry", "60"), ("expire", "3600")]),
            }
            post_form("https://api.pushover.net/1/messages.json", &form)
        }
        Target::Email { server, port, security, username, password, from, to } => {
            use lettre::Transport;
            let mut email = lettre::Message::builder().from(from.parse().context("from")?).subject(title);
            for address in to {
                email = email.to(address.parse().with_context(|| format!("to '{address}'"))?);
            }
            let email = email.body(body.to_string())?;
            let mut mailer = match security {
                Security::Starttls => lettre::SmtpTransport::starttls_relay(server)?.port(port.unwrap_or(587)),
                Security::Tls => lettre::SmtpTransport::relay(server)?.port(port.unwrap_or(465)),
                Security::None => lettre::SmtpTransport::builder_dangerous(server).port(port.unwrap_or(25)),
            };
            if let (Some(user), Some(password)) = (username, password) {
                mailer = mailer.credentials((user.clone(), password.clone()).into());
            }
            mailer.build().send(&email)?;
            Ok(())
        }
        Target::Plugin { plugin, .. } => bail!("Dienst des Plugins '{plugin}' nur über Sender"),
    }
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<()> {
    match ureq::post(url).send_form(form.iter().copied()) {
        Ok(_) => Ok(()),
        // Bot-Token nicht in Fehlermeldungen ausgeben
        Err(ureq::Error::StatusCode(code)) => bail!("Dienst antwortet mit HTTP {code}"),
        Err(e) => bail!("{}", e.to_string().replace(url, "<url>")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    fn telegram(kind: Kind, address: &str, message: &str) -> Telegram {
        let d = crate::telegram::Decoded { kind, address: address.into(), sub: "a".into(), message: message.into() };
        Telegram::new(d, "Scanner", chrono::Local::now())
    }

    #[test]
    fn alarms_select_targets_once() {
        #[derive(Deserialize)]
        struct C {
            notifier: Vec<NotifierConfig>,
            alarm: Vec<AlarmConfig>,
        }
        let c: C = toml::from_str(
            r#"
            [[notifier]]
            name = "Handy"
            type = "pushover"
            token = "t"
            user = "u"
            [[notifier]]
            name = "Gruppe"
            type = "telegram"
            bot_token = "b"
            chat_id = "1"
            [[alarm]]
            name = "Schleife"
            kind = "pocsag"
            address = "^18599"
            notify = ["Handy", "Gruppe"]
            [[alarm]]
            name = "Brand"
            message = "(?i)brand"
            notify = ["Handy"]
            priority = "high"
            title = "🔥 {kurz}"
            text = "{ort}: {message} ({alarm})"
            "#,
        )
        .unwrap();
        let alarms: Vec<Alarm> = c.alarm.iter().map(|a| compile(a, &c.notifier).unwrap()).collect();
        let got = |t: Telegram| targets(&alarms, &t).into_iter().map(|(i, title, _, p)| (i, title, p)).collect::<Vec<_>>();
        assert_eq!(
            got(telegram(Kind::Pocsag, "1859992", "Brand 3")),
            [(0, "Schleife, Brand".into(), Priority::High), (1, "Schleife".into(), Priority::Normal)]
        );
        let mut brand = telegram(Kind::Pocsag, "1234567", "BRAND");
        (brand.ort, brand.kurz) = ("Musterstadt".into(), "FW".into());
        assert_eq!(targets(&alarms, &brand), [(0, "🔥 FW".into(), "Musterstadt: BRAND (Brand)".into(), Priority::High)]);
        assert!(got(telegram(Kind::Fms, "1859992", "")).is_empty());

        let unknown: AlarmConfig = toml::from_str("name = 'X'\nnotify = ['Fax']").unwrap();
        assert!(compile(&unknown, &c.notifier).is_err());
    }

    #[test]
    fn repeats_are_suppressed_within_window() {
        let mut n = Repeats::new(Duration::from_secs(60));
        let start = Instant::now();
        let alarm = telegram(Kind::Pocsag, "1859992", "Brand 3");
        assert!(!n.is_repeat(&alarm, start));
        assert!(n.is_repeat(&alarm, start + Duration::from_secs(30)));
        assert!(!n.is_repeat(&telegram(Kind::Pocsag, "1859992", "Brand 4"), start + Duration::from_secs(31)));
        // Das Fenster zählt ab der letzten Wiederholung
        assert!(n.is_repeat(&alarm, start + Duration::from_secs(80)));
        assert!(!n.is_repeat(&alarm, start + Duration::from_secs(141)));
    }

    #[test]
    fn text_contains_all_fields() {
        let mut t = telegram(Kind::Pocsag, "1859992", "Brand 3");
        let s = text(&t);
        assert!(s.starts_with("POCSAG 1859992 a\nBrand 3\nScanner · "), "{s}");
        t.ort = "Musterstadt".into();
        t.kurz = "FW".into();
        assert!(text(&t).starts_with("POCSAG 1859992 a\nBrand 3\nMusterstadt · FW\nScanner · "));
    }

    #[test]
    fn fill_placeholders() {
        let mut t = telegram(Kind::Pocsag, "1859992", "Brand 3");
        t.lang = "Löschzug Nord".into();
        let s = fill("{kind} {address}{sub}: {message} {unbekannt} {ort}| {lang} {alarm} {", &t, "Regel");
        assert_eq!(s, "POCSAG 1859992a: Brand 3 {unbekannt} | Löschzug Nord Regel {");
        assert!(fill("{time}", &t, "").contains('.'));
    }

    #[test]
    fn post_form_sends_urlencoded_body() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/bot123/sendMessage", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let (mut head, mut len) = (String::new(), 0);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
                head += &line;
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
            (head, String::from_utf8(body).unwrap())
        });
        post_form(&url, &[("chat_id", "42"), ("text", "Brand 3 Göppingen\nB&W")]).unwrap();
        let (head, body) = server.join().unwrap();
        assert!(head.starts_with("POST /bot123/sendMessage "));
        assert_eq!(body, "chat_id=42&text=Brand+3+G%C3%B6ppingen%0AB%26W");
    }
}
