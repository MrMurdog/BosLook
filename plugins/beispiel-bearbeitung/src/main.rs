//! Beispiel-Plugin: ersetzt Abkürzungen in der Meldung, z. B. „FF“ → „Freiwillige Feuerwehr“.
//! Vorlage für eigene Bearbeiten-Plugins (Fähigkeit `process`).

use boslook_plugin::{Init, Output, Plugin, Process, Telegram};
use std::collections::HashMap;

#[derive(Default)]
struct Ersetzen {
    /// Abkürzung → Langform
    words: HashMap<String, String>,
}

impl Plugin for Ersetzen {
    fn init(&mut self, init: &Init, _out: &Output) {
        // Feld „ersetzungen“ aus plugin.toml: eine Zeile je Ersetzung, „FF=Freiwillige Feuerwehr“
        let lines = init.config.get("ersetzungen").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for line in lines.iter().filter_map(|l| l.as_str()) {
            match line.split_once('=') {
                Some((short, long)) => {
                    self.words.insert(short.trim().to_string(), long.trim().to_string());
                }
                None => boslook_plugin::log(format!("Zeile ohne „=“ übersprungen: {line}")),
            }
        }
        boslook_plugin::log(format!("{} Ersetzungen geladen", self.words.len()));
    }

    fn process(&mut self, t: &Telegram) -> Process {
        let message: Vec<&str> = t.message.split(' ').map(|w| self.words.get(w).map_or(w, String::as_str)).collect();
        let message = message.join(" ");
        if message == t.message {
            return Process::Unchanged;
        }
        Process::Changed(Telegram { message, ..t.clone() })
    }
}

fn main() {
    boslook_plugin::run(Ersetzen::default());
}
