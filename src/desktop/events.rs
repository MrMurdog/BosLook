//! Lokale Ereignisse: Ton, Benachrichtigung bzw. Popup, Programm starten.

use crate::config::EventConfig;
use anyhow::Result;
use boslook::filter::{Filter, FilterConfig};
use boslook::notify::{Priority, Repeats, fill, text};
use boslook::telegram::Telegram;
use std::time::{Duration, Instant};

pub struct Events {
    rules: Vec<(EventConfig, Filter)>,
    repeats: Repeats,
}

impl Events {
    pub fn new(configs: &[EventConfig], repeat: u64) -> Result<Events> {
        let rules = configs
            .iter()
            .map(|e| {
                let filter = Filter::new(&FilterConfig {
                    name: e.name.clone(),
                    kind: e.kind,
                    address: e.address.clone(),
                    sub: e.sub.clone(),
                    message: e.message.clone(),
                    ort: e.ort.clone(),
                    kurz: e.kurz.clone(),
                    lang: e.lang.clone(),
                    exclude: false,
                })?;
                Ok((e.clone(), filter))
            })
            .collect::<Result<_>>()?;
        Ok(Events { rules, repeats: Repeats::new(Duration::from_secs(repeat)) })
    }

    pub fn handle(&mut self, t: &Telegram) {
        let matching: Vec<&EventConfig> = self.rules.iter().filter(|(_, f)| f.matches(t)).map(|(e, _)| e).collect();
        if matching.is_empty() || self.repeats.is_repeat(t, Instant::now()) {
            return;
        }
        let names = matching.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(", ");
        let priority = matching.iter().map(|e| e.priority).max().unwrap_or_default();
        // Vorlagen der ersten passenden Regel
        let title = fill(matching[0].title.as_deref().unwrap_or("{alarm}"), t, &names);
        let body = matching[0].text.as_deref().map_or_else(|| text(t), |template| fill(template, t, &names));
        for e in &matching {
            if let Some(path) = &e.sound {
                play(path.clone());
            }
            if let Some(program) = &e.program {
                let args = e.args.iter().map(|a| fill(a, t, &e.name));
                if let Err(err) = std::process::Command::new(program).args(args).spawn() {
                    eprintln!("Ereignis '{}': {program}: {err}", e.name);
                }
            }
        }
        if priority == Priority::Normal {
            std::thread::spawn(move || {
                let shown = notify_rust::Notification::new().appname("BosLook").summary(&title).body(&body).show();
                if let Err(e) = shown {
                    eprintln!("Benachrichtigung: {e}");
                }
            });
        } else {
            popup(&title, &body, priority == Priority::Emergency);
        }
    }
}

fn play(path: String) {
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let mut sink = rodio::DeviceSinkBuilder::open_default_sink()?;
            sink.log_on_drop(false);
            let file = std::io::BufReader::new(std::fs::File::open(&path)?);
            rodio::play(sink.mixer(), file)?.sleep_until_end();
            Ok(())
        })();
        if let Err(e) = result {
            eprintln!("Ton {path}: {e:#}");
        }
    });
}

/// Das Popup läuft als eigener Prozess, damit es unabhängig vom (evtl. verborgenen) Hauptfenster erscheint.
fn popup(title: &str, body: &str, emergency: bool) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return eprintln!("Popup: {e}"),
    };
    let mut command = std::process::Command::new(exe);
    command.args(["popup", "--title", title, "--body", body]);
    if emergency {
        command.arg("--emergency");
    }
    if let Err(e) = command.spawn() {
        eprintln!("Popup: {e}");
    }
}
