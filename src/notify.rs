//! Benachrichtigungen per Telegram, E-Mail und Pushover.
//!
//! `[[notifier]]` beschreibt ein Ziel, `[[alarm]]` eine Regel (Felder wie bei den Filtern),
//! die bei passenden Telegrammen die genannten Ziele benachrichtigt.

use crate::filter::{Filter, FilterConfig};
use crate::telegram::{Decoded, Kind, Telegram};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
pub struct NotifierConfig {
    pub name: String,
    #[serde(flatten)]
    pub target: Target,
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
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
    pub address: Option<String>,
    pub sub: Option<String>,
    pub message: Option<String>,
    /// Namen der `[[notifier]]`, die benachrichtigt werden
    pub notify: Vec<String>,
    #[serde(default)]
    pub priority: Priority,
}

struct Alarm {
    name: String,
    filter: Filter,
    targets: Vec<usize>,
    priority: Priority,
}

/// Prüft Telegramme gegen die Alarmregeln und gibt Nachrichten an die Ziele weiter.
/// Jedes Ziel sendet in einem eigenen Thread, damit ein langsamer Dienst die anderen nicht aufhält.
pub struct Notify {
    alarms: Vec<Alarm>,
    senders: Vec<mpsc::Sender<(String, String, Priority)>>,
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
    pub fn new(alarms: &[AlarmConfig], notifiers: Vec<NotifierConfig>, repeat: Duration) -> Result<Notify> {
        let alarms = alarms.iter().map(|a| compile(a, &notifiers)).collect::<Result<_>>()?;
        let senders = notifiers
            .into_iter()
            .map(|n| {
                let (tx, rx) = mpsc::channel::<(String, String, Priority)>();
                std::thread::spawn(move || {
                    for (title, body, priority) in rx {
                        if let Err(e) = send(&n.target, &title, &body, priority) {
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
        for (target, title, priority) in targets(&self.alarms, t) {
            let _ = self.senders[target].send((title, text(t), priority));
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
        address: a.address.clone(),
        sub: a.sub.clone(),
        message: a.message.clone(),
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
    Ok(Alarm { name: a.name.clone(), filter, targets, priority: a.priority })
}

/// Welche Ziele mit welchem Titel benachrichtigt werden. Passen mehrere Regeln auf dasselbe
/// Ziel, gibt es trotzdem nur eine Nachricht (mit der höchsten Priorität).
fn targets(alarms: &[Alarm], t: &Telegram) -> Vec<(usize, String, Priority)> {
    let d = Decoded { kind: t.kind, address: t.address.clone(), sub: t.sub.clone(), message: t.message.clone() };
    let mut out: Vec<(usize, Vec<&str>, Priority)> = Vec::new();
    for a in alarms.iter().filter(|a| a.filter.matches(&d)) {
        for &target in &a.targets {
            match out.iter_mut().find(|(i, _, _)| *i == target) {
                Some((_, names, priority)) => {
                    names.push(&a.name);
                    *priority = (*priority).max(a.priority);
                }
                None => out.push((target, vec![&a.name], a.priority)),
            }
        }
    }
    out.into_iter().map(|(i, names, priority)| (i, names.join(", "), priority)).collect()
}

pub fn text(t: &Telegram) -> String {
    let mut head = format!("{} {}", t.kind.as_str().to_uppercase(), t.address);
    if !t.sub.is_empty() {
        head = format!("{head} {}", t.sub);
    }
    let mut text = head;
    if !t.message.is_empty() {
        text = format!("{text}\n{}", t.message);
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
        let d = Decoded { kind, address: address.into(), sub: "a".into(), message: message.into() };
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
            "#,
        )
        .unwrap();
        let alarms: Vec<Alarm> = c.alarm.iter().map(|a| compile(a, &c.notifier).unwrap()).collect();
        let got = |t| targets(&alarms, &t);
        assert_eq!(
            got(telegram(Kind::Pocsag, "1859992", "Brand 3")),
            [(0, "Schleife, Brand".into(), Priority::High), (1, "Schleife".into(), Priority::Normal)]
        );
        assert_eq!(got(telegram(Kind::Pocsag, "1234567", "BRAND")), [(0, "Brand".into(), Priority::High)]);
        assert!(got(telegram(Kind::Fms, "1859992", "")).is_empty());

        let unknown = AlarmConfig {
            name: "X".into(),
            kind: None,
            address: None,
            sub: None,
            message: None,
            notify: vec!["Fax".into()],
            priority: Priority::Normal,
        };
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
        let s = text(&telegram(Kind::Pocsag, "1859992", "Brand 3"));
        assert!(s.starts_with("POCSAG 1859992 a\nBrand 3\nScanner · "), "{s}");
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
