//! Plugins: eigenständige Programme in beliebiger Sprache. BosLook startet sie und tauscht mit
//! ihnen JSON-Nachrichten aus, eine pro Zeile (stdin: BosLook → Plugin, stdout: Plugin →
//! BosLook, stderr: Log). Protokoll und Beispiele: docs/PLUGINS.md
//!
//! Ein Plugin ist ein Ordner unter `plugins/` neben der Konfiguration mit einer `plugin.toml`
//! ([`Manifest`]). Eingeschaltet und eingestellt wird es in der Konfiguration unter
//! `[plugin.<name>]`.

use crate::config::Config;
use crate::form::{Field, Input, field};
use crate::notify::{Priority, Target};
use crate::telegram::Telegram;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Version der Plugin-Schnittstelle
pub const API: u32 = 1;
/// Pause vor dem Neustart eines beendeten Plugins
const RESTART: Duration = Duration::from_secs(5);
/// So lange darf ein Plugin ein Telegramm bearbeiten, danach läuft es unverändert weiter
const PROCESS_TIMEOUT: Duration = Duration::from_secs(2);
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(30);
/// Letzte Logzeilen je Plugin für die Seite „Plugins"
const LOG_LINES: usize = 100;
/// Diese Namen sind schon eingebaute Benachrichtigungsdienste
const RESERVED: [&str; 3] = ["telegram", "pushover", "email"];

/// Inhalt der `plugin.toml`
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    pub api: u32,
    /// Programm und Argumente; ein relativer Pfad gilt ab dem Plugin-Ordner
    pub command: Vec<String>,
    /// Abweichender Befehl unter Windows
    pub command_windows: Option<Vec<String>>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Einstellungen unter `[plugin.<name>]`
    #[serde(default, rename = "field")]
    pub fields: Vec<FieldDef>,
    /// Einstellungen je Benachrichtigungsziel (nur mit `notify`)
    #[serde(default, rename = "notifier_field")]
    pub notifier_fields: Vec<FieldDef>,
    /// Plugin-Ordner
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    /// Bekommt jedes gespeicherte Telegramm
    Observe,
    /// Bearbeitet oder verwirft Telegramme, bevor BosLook sie verarbeitet
    Process,
    /// Stellt einen Benachrichtigungsdienst bereit (`type = "<name>"` unter Benachrichtigungsziele)
    Notify,
    /// Liefert selbst Telegramme
    Input,
}

/// Ein Eingabefeld für das Konfigurationsformular
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDef {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: String,
    /// text, password, number, bool, select, lines, multiline
    #[serde(default = "default_input")]
    pub input: String,
    /// Auswahlwerte bei `select`
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub required: bool,
    pub default: Option<toml::Value>,
}

fn default_input() -> String {
    "text".into()
}

impl FieldDef {
    fn to_field(&self) -> Result<Field> {
        let input = match self.input.as_str() {
            "text" => Input::Text,
            "password" => Input::Password,
            "number" => Input::Number,
            "bool" => Input::Bool,
            "select" => Input::Select(self.options.iter().map(|o| intern(o)).collect()),
            "lines" => Input::Lines,
            "multiline" => Input::Multiline,
            other => bail!("Feld '{}': unbekannte Eingabe '{other}'", self.key),
        };
        let mut f = field(intern(&self.key), intern(&self.label), input).help(intern(&self.help));
        f.required = self.required;
        if let Some(d) = &self.default {
            f = f.default(serde_json::to_value(d)?);
        }
        Ok(f)
    }
}

/// Das Formular-Schema arbeitet mit `&'static str`. Texte aus `plugin.toml` werden darum
/// einmalig festgehalten; gleiche Texte teilen sich denselben Speicher, auch wenn die
/// Beschreibungen bei jedem Aufruf des Formulars neu gelesen werden.
pub fn intern(s: &str) -> &'static str {
    static TEXTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut texts = TEXTS.lock().unwrap();
    if let Some(t) = texts.iter().find(|t| **t == s) {
        return t;
    }
    let t: &'static str = Box::leak(s.to_string().into_boxed_str());
    texts.push(t);
    t
}

impl Manifest {
    pub fn has(&self, c: Capability) -> bool {
        self.capabilities.contains(&c)
    }

    /// Felder für den Abschnitt `[plugin.<name>]`. Im Formular schaltet „aktiv" das Plugin
    /// ein; `enabled = false` (nur als Text) schaltet es ab, ohne die Einstellungen zu löschen.
    pub fn section_fields(&self) -> Result<Vec<Field>> {
        self.fields.iter().map(FieldDef::to_field).collect()
    }

    pub fn notifier_fields(&self) -> Result<Vec<Field>> {
        self.notifier_fields.iter().map(|f| Ok(f.to_field()?.when("type", &[intern(&self.name)]))).collect()
    }
}

/// Ordner der Plugins: `plugins/` neben der Konfigurationsdatei
pub fn dir(config_path: &str) -> PathBuf {
    Path::new(config_path).parent().unwrap_or(Path::new("")).join("plugins")
}

/// Alle installierten Plugins, sortiert nach Namen, und Fehlermeldungen zu unlesbaren
pub fn manifests(dir: &Path) -> (Vec<Manifest>, Vec<String>) {
    let (mut found, mut errors) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(dir) else { return (found, errors) };
    for entry in entries.flatten() {
        let path = entry.path().join("plugin.toml");
        if !path.is_file() {
            continue;
        }
        match load(&path) {
            Ok(m) => found.push(m),
            Err(e) => errors.push(format!("{}: {e:#}", path.display())),
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    (found, errors)
}

fn load(path: &Path) -> Result<Manifest> {
    let mut m: Manifest = toml::from_str(&std::fs::read_to_string(path)?)?;
    m.dir = path.parent().context("Ordner")?.canonicalize()?;
    let folder = m.dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if m.name != folder {
        bail!("name '{}' muss wie der Ordner heißen ('{folder}')", m.name);
    }
    if m.name.is_empty() || !m.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        bail!("name darf nur aus a–z, 0–9, - und _ bestehen");
    }
    if RESERVED.contains(&m.name.as_str()) {
        bail!("name '{}' ist schon ein eingebauter Dienst", m.name);
    }
    if m.command.is_empty() {
        bail!("command ist leer");
    }
    m.section_fields()?;
    m.notifier_fields()?;
    Ok(m)
}

/// Prüft die Abschnitte `[plugin.…]` und Benachrichtigungsziele von Plugins.
pub fn check(config: &Config, manifests: &[Manifest]) -> Result<()> {
    let find = |name: &str| manifests.iter().find(|m| m.name == name);
    for (name, settings) in &config.plugins {
        let m = find(name).with_context(|| format!("Plugin '{name}' ist nicht installiert (Ordner plugins/{name} fehlt)"))?;
        if m.api != API {
            bail!("Plugin '{name}' braucht Schnittstelle {} (BosLook kann {API})", m.api);
        }
        if enabled(settings) {
            required(&m.fields, settings).with_context(|| format!("Plugin '{name}'"))?;
        }
    }
    for n in &config.notifiers {
        if let Target::Plugin { plugin, settings } = &n.target {
            let m = find(plugin).filter(|m| m.has(Capability::Notify));
            let m = m.with_context(|| format!("Benachrichtigungsziel '{}': unbekannter Dienst '{plugin}'", n.name))?;
            if !config.plugins.iter().any(|(name, s)| name == plugin && enabled(s)) {
                bail!("Benachrichtigungsziel '{}': Plugin '{plugin}' ist nicht eingeschaltet", n.name);
            }
            required(&m.notifier_fields, settings).with_context(|| format!("Benachrichtigungsziel '{}'", n.name))?;
        }
    }
    Ok(())
}

fn enabled(settings: &toml::Table) -> bool {
    settings.get("enabled").and_then(toml::Value::as_bool).unwrap_or(true)
}

fn required(fields: &[FieldDef], settings: &toml::Table) -> Result<()> {
    match fields.iter().find(|f| f.required && !settings.contains_key(&f.key)) {
        Some(f) => bail!("„{}“ fehlt ({})", f.label, f.key),
        None => Ok(()),
    }
}

/// Die laufenden Plugins
pub struct Plugins {
    running: Vec<Arc<Plugin>>,
    dir: PathBuf,
}

pub struct Plugin {
    pub manifest: Manifest,
    /// Zeilen an stdin des Plugins; während eines Neustarts warten sie hier
    tx: mpsc::UnboundedSender<String>,
    /// Offene Anfragen (process, notify) nach ID
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    next_id: AtomicU64,
    state: Mutex<String>,
    log: Mutex<VecDeque<String>>,
}

impl Plugins {
    /// Ohne Plugins (z. B. für Tests)
    pub fn none() -> Plugins {
        Plugins { running: Vec::new(), dir: PathBuf::new() }
    }

    /// Startet alle eingeschalteten Plugins. Muss innerhalb einer Tokio-Laufzeit aufgerufen werden.
    pub fn start(config: &Config, dir: &Path, ingest: mpsc::Sender<Telegram>) -> Result<Plugins> {
        let (manifests, _) = manifests(dir);
        check(config, &manifests)?;
        let mut running = Vec::new();
        for (name, settings) in config.plugins.iter().filter(|(_, s)| enabled(s)) {
            let manifest = manifests.iter().find(|m| &m.name == name).expect("geprüft").clone();
            let mut settings = settings.clone();
            settings.remove("enabled");
            let (tx, rx) = mpsc::unbounded_channel();
            let plugin = Arc::new(Plugin {
                manifest,
                tx,
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                state: Mutex::new("startet".into()),
                log: Mutex::new(VecDeque::new()),
            });
            tokio::spawn(supervise(plugin.clone(), settings, rx, ingest.clone()));
            running.push(plugin);
        }
        Ok(Plugins { running, dir: dir.to_path_buf() })
    }

    pub fn get(&self, name: &str) -> Option<Arc<Plugin>> {
        self.running.iter().find(|p| p.manifest.name == name).cloned()
    }

    fn with(&self, c: Capability) -> impl Iterator<Item = &Arc<Plugin>> {
        self.running.iter().filter(move |p| p.manifest.has(c))
    }

    /// Namen der Plugins, die Telegramme liefern (für die Reiter der Weboberfläche)
    pub fn sources(&self) -> Vec<String> {
        self.with(Capability::Input).map(|p| p.manifest.name.clone()).collect()
    }

    /// Lässt die Bearbeiten-Plugins der Reihe nach über das Telegramm laufen; `None` = verwerfen.
    pub async fn process(&self, mut t: Telegram) -> Option<Telegram> {
        for p in self.with(Capability::Process) {
            let reply = p.request(json!({ "type": "process", "telegram": t }), PROCESS_TIMEOUT).await;
            match reply.and_then(|r| apply_reply(&t, &r)) {
                Ok(Some(changed)) => t = changed,
                Ok(None) => return None,
                Err(e) => p.log(format!("Telegramm unverändert weitergegeben: {e:#}")),
            }
        }
        Some(t)
    }

    /// Gibt ein gespeichertes Telegramm an die Plugins zum Mitlesen.
    pub fn observe(&self, t: &Telegram) {
        for p in self.with(Capability::Observe) {
            p.send(&json!({ "type": "telegram", "telegram": t }));
        }
    }

    /// Alle installierten Plugins mit Zustand, für die Seite „Plugins"
    pub fn status(&self) -> Value {
        let (manifests, errors) = manifests(&self.dir);
        let list: Vec<Value> = manifests
            .iter()
            .map(|m| {
                let running = self.get(&m.name);
                json!({
                    "name": m.name,
                    "version": m.version,
                    "description": m.description,
                    "author": m.author,
                    "capabilities": m.capabilities,
                    "state": running.as_ref().map_or("aus".to_string(), |p| p.state.lock().unwrap().clone()),
                    "log": running.as_ref().map_or(Vec::new(), |p| p.log.lock().unwrap().iter().cloned().collect()),
                })
            })
            .collect();
        json!({ "plugins": list, "errors": errors })
    }
}

/// Antwort auf `process`: `drop` verwirft, `telegram` ersetzt die angegebenen Felder.
fn apply_reply(t: &Telegram, reply: &Value) -> Result<Option<Telegram>> {
    if reply["drop"] == true {
        return Ok(None);
    }
    let Some(changes) = reply.get("telegram").and_then(Value::as_object) else { return Ok(Some(t.clone())) };
    let mut value = serde_json::to_value(t)?;
    for (k, v) in changes {
        value[k] = v.clone();
    }
    Ok(Some(serde_json::from_value(value).context("ungültiges Telegramm in der Antwort")?))
}

impl Plugin {
    fn send(&self, msg: &Value) {
        let _ = self.tx.send(msg.to_string());
    }

    fn log(&self, line: String) {
        eprintln!("Plugin '{}': {line}", self.manifest.name);
        let mut log = self.log.lock().unwrap();
        log.push_back(format!("{} {line}", chrono::Local::now().format("%d.%m. %H:%M:%S")));
        if log.len() > LOG_LINES {
            log.pop_front();
        }
    }

    fn set_state(&self, state: impl Into<String>) {
        *self.state.lock().unwrap() = state.into();
    }

    /// Nachricht mit neuer `id` senden und auf die Antwort mit derselben `id` warten.
    async fn request(&self, mut msg: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        msg["id"] = id.into();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&msg);
        let reply = tokio::time::timeout(timeout, rx).await;
        self.pending.lock().unwrap().remove(&id);
        match reply {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => bail!("Plugin wurde beendet"),
            Err(_) => bail!("keine Antwort nach {} s", timeout.as_secs()),
        }
    }

    /// Benachrichtigung über dieses Plugin senden (aus einem Thread außerhalb der Laufzeit).
    pub fn notify_blocking(
        &self,
        runtime: &tokio::runtime::Handle,
        target: &str,
        settings: &toml::Table,
        message: (&str, &str, Priority),
        t: Option<&Telegram>,
    ) -> Result<()> {
        let (title, body, priority) = message;
        let mut target_json = serde_json::to_value(settings)?;
        target_json["name"] = target.into();
        let msg = json!({ "type": "notify", "target": target_json, "title": title, "body": body, "priority": priority, "telegram": t });
        let reply = runtime.block_on(self.request(msg, NOTIFY_TIMEOUT))?;
        match reply["error"].as_str() {
            Some(e) => bail!("{e}"),
            None => Ok(()),
        }
    }

    /// Eine Zeile von stdout des Plugins
    async fn handle(&self, line: &str, ingest: &mpsc::Sender<Telegram>) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return self.log(format!("keine JSON-Nachricht auf stdout (Ausgaben für das Log gehören nach stderr): {line}"));
        };
        match msg["type"].as_str().unwrap_or_default() {
            "processed" | "notified" => {
                let waiting = msg["id"].as_u64().and_then(|id| self.pending.lock().unwrap().remove(&id));
                if let Some(tx) = waiting {
                    let _ = tx.send(msg);
                }
            }
            "telegram" if self.manifest.has(Capability::Input) => match input(&msg["telegram"], &self.manifest.name) {
                Ok(t) => {
                    let _ = ingest.send(t).await;
                }
                Err(e) => self.log(format!("ungültiges Telegramm: {e:#}")),
            },
            "telegram" => self.log("liefert Telegramme, hat aber nicht die Fähigkeit 'input'".into()),
            other => self.log(format!("unbekannte Nachricht '{other}'")),
        }
    }
}

/// Telegramm von einem Eingangs-Plugin: `kind`, `address`, optional `sub`, `message`, `source`
fn input(value: &Value, plugin: &str) -> Result<Telegram> {
    #[derive(Deserialize)]
    struct Input {
        kind: crate::telegram::Kind,
        address: String,
        #[serde(default)]
        sub: String,
        #[serde(default)]
        message: String,
        source: Option<String>,
    }
    let i: Input = serde_json::from_value(value.clone())?;
    let decoded = crate::telegram::Decoded { kind: i.kind, address: i.address, sub: i.sub, message: i.message };
    Ok(Telegram::new(decoded, i.source.as_deref().unwrap_or(plugin), chrono::Local::now()))
}

/// Startet das Plugin und nach einem Ende nach kurzer Pause neu.
async fn supervise(plugin: Arc<Plugin>, settings: toml::Table, mut rx: mpsc::UnboundedReceiver<String>, ingest: mpsc::Sender<Telegram>) {
    let m = &plugin.manifest;
    let data_dir = m.dir.join("data");
    let init = json!({ "type": "init", "api": API, "boslook": env!("CARGO_PKG_VERSION"), "name": m.name, "config": settings, "data_dir": data_dir });
    loop {
        let result = async {
            std::fs::create_dir_all(&data_dir).with_context(|| data_dir.display().to_string())?;
            let mut child = command(m).spawn().with_context(|| format!("Start von {:?}", m.command))?;
            let mut stdin = child.stdin.take().context("stdin")?;
            let mut stdout = BufReader::new(child.stdout.take().context("stdout")?).lines();
            let mut stderr = BufReader::new(child.stderr.take().context("stderr")?).lines();
            plugin.set_state("läuft");
            plugin.log("gestartet".into());
            stdin.write_all(format!("{init}\n").as_bytes()).await?;
            let mut stderr_open = true;
            loop {
                tokio::select! {
                    line = stdout.next_line() => match line? {
                        Some(line) => plugin.handle(&line, &ingest).await,
                        None => break,
                    },
                    line = stderr.next_line(), if stderr_open => match line {
                        Ok(Some(line)) => plugin.log(line),
                        _ => stderr_open = false,
                    },
                    msg = rx.recv() => match msg {
                        Some(msg) => stdin.write_all(format!("{msg}\n").as_bytes()).await?,
                        None => return anyhow::Ok(None),
                    },
                }
            }
            // stdout geschlossen: Rest von stderr noch ins Log, dann auf das Ende warten
            while let Ok(Some(line)) = stderr.next_line().await {
                plugin.log(line);
            }
            drop(stdin);
            Ok(Some(child.wait().await?))
        }
        .await;
        plugin.pending.lock().unwrap().clear();
        let state = match result {
            Ok(None) => return,
            Ok(Some(status)) => format!("beendet ({status})"),
            Err(e) => format!("Fehler: {e:#}"),
        };
        plugin.log(format!("{state}, Neustart in {} s", RESTART.as_secs()));
        plugin.set_state(state);
        tokio::time::sleep(RESTART).await;
    }
}

fn command(m: &Manifest) -> tokio::process::Command {
    let args = match &m.command_windows {
        Some(c) if cfg!(windows) && !c.is_empty() => c,
        _ => &m.command,
    };
    // Relativer Programmpfad (z. B. target/release/plugin) gilt ab dem Plugin-Ordner
    let program = Path::new(&args[0]);
    let program = if program.components().count() > 1 && program.is_relative() { m.dir.join(program) } else { program.to_path_buf() };
    let mut c = tokio::process::Command::new(program);
    c.args(&args[1..])
        .current_dir(&m.dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ordner mit Plugins, die `plugin.toml` mit dem jeweiligen Text bekommen
    fn plugin_dir(test: &str, plugins: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("boslook-plugins-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, manifest) in plugins {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            std::fs::write(dir.join(name).join("plugin.toml"), manifest).unwrap();
        }
        dir
    }

    const HOOK: &str = r#"
        name = "hook"
        version = "1.0.0"
        api = 1
        command = ["python3", "hook.py"]
        capabilities = ["notify"]
        [[field]]
        key = "timeout"
        label = "Zeitlimit"
        input = "number"
        default = 10
        [[notifier_field]]
        key = "url"
        label = "Adresse"
        required = true
        [[notifier_field]]
        key = "token"
        label = "Token"
        input = "password"
    "#;

    #[test]
    fn manifests_are_checked() {
        let dir = plugin_dir(
            "manifests",
            &[
                ("hook", HOOK),
                ("falsch", "name = \"anders\"\nversion = \"1\"\napi = 1\ncommand = [\"x\"]\n"),
                ("email", "name = \"email\"\nversion = \"1\"\napi = 1\ncommand = [\"x\"]\n"),
                ("feld", "name = \"feld\"\nversion = \"1\"\napi = 1\ncommand = [\"x\"]\n[[field]]\nkey = \"a\"\nlabel = \"A\"\ninput = \"farbe\"\n"),
            ],
        );
        let (found, mut errors) = manifests(&dir);
        assert_eq!(found.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["hook"]);
        errors.sort();
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors[0].contains("email") && errors[0].contains("eingebauter Dienst"), "{errors:?}");
        assert!(errors[1].contains("falsch") && errors[1].contains("muss wie der Ordner heißen"), "{errors:?}");
        assert!(errors[2].contains("unbekannte Eingabe 'farbe'"), "{errors:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn config(text: &str) -> Config {
        Config::parse(text).unwrap()
    }

    #[test]
    fn config_is_checked_against_installed_plugins() {
        let dir = plugin_dir("check", &[("hook", HOOK)]);
        let (m, _) = manifests(&dir);
        let target = "[[notifier]]\nname = \"Ziel\"\ntype = \"hook\"\nurl = \"http://x\"\n";
        assert!(check(&config(&format!("{target}[plugin.hook]\n")), &m).is_ok());
        let err = |text: &str| format!("{:#}", check(&config(text), &m).unwrap_err());
        assert_eq!(err("[plugin.fehlt]\n"), "Plugin 'fehlt' ist nicht installiert (Ordner plugins/fehlt fehlt)");
        assert_eq!(err(target), "Benachrichtigungsziel 'Ziel': Plugin 'hook' ist nicht eingeschaltet");
        assert_eq!(err(&format!("{target}[plugin.hook]\nenabled = false\n")), "Benachrichtigungsziel 'Ziel': Plugin 'hook' ist nicht eingeschaltet");
        assert_eq!(err("[[notifier]]\nname = \"Ziel\"\ntype = \"hook\"\n[plugin.hook]\n"), "Benachrichtigungsziel 'Ziel': „Adresse“ fehlt (url)");
        assert_eq!(err("[[notifier]]\nname = \"Ziel\"\ntype = \"gibtsnicht\"\n"), "Benachrichtigungsziel 'Ziel': unbekannter Dienst 'gibtsnicht'");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_target_keeps_settings() {
        let c = config("[[notifier]]\nname = \"Ziel\"\ntype = \"hook\"\nurl = \"http://x\"\n[[notifier]]\nname = \"Bot\"\ntype = \"telegram\"\nbot_token = \"t\"\nchat_id = \"1\"\n");
        match &c.notifiers[0].target {
            Target::Plugin { plugin, settings } => assert_eq!((plugin.as_str(), settings.get("url").and_then(|v| v.as_str())), ("hook", Some("http://x"))),
            t => panic!("{t:?}"),
        }
        assert!(matches!(c.notifiers[1].target, Target::Telegram { .. }));
        assert!(Config::parse("[[notifier]]\nname = \"Bot\"\ntype = \"telegram\"\n").is_err());
    }

    #[test]
    fn plugins_keep_file_order() {
        let c = config("[plugin.zeta]\n[plugin.alpha]\n[plugin.mitte]\n");
        assert_eq!(c.plugins.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["zeta", "alpha", "mitte"]);
    }

    #[test]
    fn python_library_copy_is_current() {
        assert_eq!(include_str!("../plugins/webhook/boslook_plugin.py"), include_str!("../sdk/python/boslook_plugin.py"));
    }

    #[test]
    fn process_reply_changes_fields() {
        let d = crate::telegram::Decoded { kind: crate::telegram::Kind::Pocsag, address: "1234567".into(), sub: "a".into(), message: "alt".into() };
        let t = Telegram::new(d, "Test", chrono::Local::now());
        assert_eq!(apply_reply(&t, &json!({"telegram": {"message": "neu"}})).unwrap().unwrap().message, "neu");
        assert_eq!(apply_reply(&t, &json!({})).unwrap().unwrap().message, "alt");
        assert!(apply_reply(&t, &json!({"drop": true})).unwrap().is_none());
        assert!(apply_reply(&t, &json!({"telegram": {"kind": "quatsch"}})).is_err());
    }

    #[test]
    fn form_writes_plugin_sections_and_targets() {
        let dir = plugin_dir("form", &[("hook", HOOK)]);
        let (m, _) = manifests(&dir);
        let schema = crate::form::server_schema(&m);
        let text = "# Kommentar\n[web]\nlisten = \"127.0.0.1:1\"\n";
        let mut form = crate::form::to_json(text, &schema).unwrap();
        form["plugin.hook"] = json!({"timeout": "5"});
        form["notifier"] = json!([{"name": "Ziel", "type": "hook", "url": "http://x", "token": "geheim"}]);
        let text = crate::form::apply(text, &schema, &form).unwrap();
        assert!(text.contains("[[notifier]]\nname = \"Ziel\"\ntype = \"hook\"\nurl = \"http://x\"\ntoken = \"geheim\"\n"), "{text}");
        assert!(text.contains("\n[plugin.hook]\ntimeout = 5\n") && !text.contains("[plugin]"), "{text}");
        let c = Config::parse(&text).unwrap();
        check(&c, &m).unwrap();
        // Unverändert gespeichert bleibt der Text gleich, ohne Abschnitt verschwindet er
        let mut form = crate::form::to_json(&text, &schema).unwrap();
        assert_eq!(form["plugin.hook"]["timeout"], 5);
        assert_eq!(crate::form::apply(&text, &schema, &form).unwrap(), text);
        form["plugin.hook"] = Value::Null;
        assert!(!crate::form::apply(&text, &schema, &form).unwrap().contains("plugin"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
