//! `boslook serve`: Eingänge dekodieren, mit Plugins und Regeln bearbeiten, filtern, speichern und per Weboberfläche anzeigen.

use crate::config::{Config, InputConfig, Source, Tls};
use crate::db::Db;
use crate::decoder::Bank;
use crate::filter::Filters;
use crate::notify::Notify;
use crate::plugin::Plugins;
use crate::rewrite::Rewrites;
use crate::telegram::{Decoded, Telegram};
use crate::forward::{InputState, InputStatus};
use crate::web::{self, App, Settings};
use crate::{forward, input};
use anyhow::{Context, Result, bail};
use argon2::password_hash::phc::PasswordHash;
use axum_server::tls_rustls::RustlsConfig;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Prüft die Konfiguration, ohne etwas zu starten (auch für den Config-Editor).
/// `plugins` ist der Ordner der Plugins (siehe [`crate::plugin::dir`]).
pub fn validate(config: &Config, plugins: &Path) -> Result<()> {
    if config.web.is_some() && config.users.is_empty() {
        bail!("Kein Benutzer konfiguriert. Passwort-Hash erzeugen mit: boslook hash-password");
    }
    if config.web.is_none() && !(config.users.is_empty() && config.clients.is_empty()) {
        bail!("[[user]] und [[client]] funktionieren nur zusammen mit [web]");
    }
    let hashes = config.users.iter().map(|u| ("Benutzer", &u.name, &u.password_hash));
    // Clients ohne Token bekommen es später über den Einrichtungscode
    let clients = config.clients.iter().filter(|c| !c.token_hash.is_empty());
    for (what, name, hash) in hashes.chain(clients.map(|c| ("Client", &c.name, &c.token_hash))) {
        PasswordHash::new(hash).map_err(|e| anyhow::anyhow!("{what} '{name}': ungültiger Hash ({e})"))?;
    }
    for (n, c) in config.clients.iter().enumerate() {
        if config.clients[..n].iter().any(|d| d.name == c.name) {
            bail!("Client '{}' ist doppelt vorhanden, die Namen müssen eindeutig sein", c.name);
        }
        validate_inputs(&c.inputs).with_context(|| format!("Client '{}'", c.name))?;
    }
    validate_inputs(&config.inputs)?;
    if let Some(w) = &config.weather
        && (w.warncells.is_empty() || w.interval < 60)
    {
        bail!("[weather]: mindestens eine Warncell-ID und interval von mindestens 60 Sekunden angeben");
    }
    config.web.as_ref().map(|w| w.listen.parse::<SocketAddr>()).transpose().context("web.listen")?;
    Rewrites::new(&config.rewrites)?;
    Filters::new(&config.filters)?;
    crate::notify::check(&config.alarms, &config.notifiers)?;
    crate::plugin::check(config, &crate::plugin::manifests(plugins).0)?;
    Ok(())
}

/// Prüft die Eingänge eines Servers oder Clients
pub fn validate_inputs(inputs: &[InputConfig]) -> Result<()> {
    if inputs.iter().filter(|i| matches!(i.source, Source::Stdin { .. })).count() > 1 {
        bail!("Es kann nur einen Eingang vom Typ 'stdin' geben");
    }
    for (n, i) in inputs.iter().enumerate() {
        if inputs[..n].iter().any(|j| j.name == i.name) {
            bail!("Eingang '{}' ist doppelt vorhanden, die Namen müssen eindeutig sein", i.name);
        }
        // Kanäle erscheinen wie Eingänge als Reiter, darum auch untereinander eindeutig
        let sources: Vec<String> = inputs[..n].iter().flat_map(InputConfig::sources).collect();
        if let Some(s) = i.sources().into_iter().find(|s| sources.contains(s)) {
            bail!("Eingang '{}': der Name '{s}' ist schon vergeben", i.name);
        }
        Bank::new(&i.decoders, 22050, i.pocsag).with_context(|| format!("Eingang '{}'", i.name))?;
        if let Source::RtlFm { frequency, gain, .. } = &i.source {
            if frequency.trim().is_empty() {
                bail!("Eingang '{}': Frequenz fehlt", i.name);
            }
            if gain != "auto" && gain.parse::<f32>().is_err() {
                bail!("Eingang '{}': Verstärkung muss eine Zahl (dB) oder auto sein", i.name);
            }
        }
        if let Source::RtlSdr { gain, channels, .. } = &i.source {
            if gain != "auto" && gain.parse::<f32>().is_err() {
                bail!("Eingang '{}': Verstärkung muss eine Zahl (dB) oder auto sein", i.name);
            }
            for (m, c) in channels.iter().enumerate() {
                if channels[..m].iter().any(|d| d.name == c.name) {
                    bail!("Eingang '{}': Kanal '{}' ist doppelt vorhanden", i.name, c.name);
                }
            }
            let frequencies = channels.iter().map(|c| crate::sdr::parse_frequency(&c.frequency).with_context(|| format!("Kanal '{}'", c.name))).collect::<Result<Vec<_>>>();
            frequencies.and_then(|f| crate::sdr::plan(&f)).with_context(|| format!("Eingang '{}'", i.name))?;
        }
    }
    Ok(())
}

/// Eingänge vom Server, die ein Client beim letzten Mal bekommen hat (siehe `forward.rs`)
fn load_remote(path: &Path) -> Option<(String, crate::config::RemoteConfig)> {
    let text = std::fs::read_to_string(path).ok()?;
    match forward::parse_remote(&text) {
        Ok(remote) => Some((text, remote)),
        Err(e) => {
            eprintln!("Fernkonfiguration {}: {e:#}, es gelten die eigenen Eingänge", path.display());
            None
        }
    }
}

pub async fn run(config_path: &str) -> Result<()> {
    if std::env::var_os(RESTART_WAIT).is_some() {
        // Windows: warten, bis der alte Prozess den Port freigegeben hat (siehe restart())
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    crate::update::cleanup();
    let mut config = Config::load(config_path)?;
    let plugin_dir = crate::plugin::dir(config_path);
    validate(&config, &plugin_dir)?;
    // Ein Client nimmt die Eingänge vom Server, sobald er welche bekommen hat
    let remote_path = forward::remote_path(config_path);
    let mut running_remote = String::new();
    if config.forward.is_some()
        && let Some((text, remote)) = load_remote(&remote_path)
    {
        eprintln!("Eingänge vom Server (Fernkonfiguration, {})", remote_path.display());
        config.inputs = remote.inputs;
        running_remote = text;
    }
    // Den Fingerabdruck eines selbst signierten Zertifikats braucht der Einrichtungscode
    let fingerprint = match &config.web {
        Some(w) if w.tls == Tls::SelfSigned => {
            ensure_self_signed(&w.cert, &w.key)?;
            Some(forward::fingerprint_file(&w.cert)?)
        }
        _ => None,
    };
    let addr: Option<SocketAddr> = config.web.as_ref().map(|w| w.listen.parse()).transpose().context("web.listen")?;
    let rewrites = Rewrites::new(&config.rewrites)?;
    let filters = Filters::new(&config.filters)?;
    let db = Db::open(&config.database).with_context(|| format!("Datenbank {}", config.database))?;

    let (tx, mut rx) = mpsc::channel::<Telegram>(1024);
    let plugins = Arc::new(Plugins::new(&config, &plugin_dir)?);
    let mut notify = Notify::new(&config.alarms, config.notifiers, Duration::from_secs(config.notify_repeat), &plugins)?;
    let app = Arc::new(App::new(
        db,
        Settings {
            users: config.users,
            inputs: config
                .inputs
                .iter()
                .flat_map(InputConfig::sources)
                .chain(config.weather.as_ref().map(|_| crate::weather::SOURCE.to_string()))
                .chain(plugins.sources())
                .collect(),
            secure_cookie: config.web.as_ref().is_some_and(|w| w.tls != Tls::Off),
            clients: config.clients,
            fingerprint,
            update_beta: config.update_channel == crate::config::UpdateChannel::Beta,
            config_path: config_path.to_string(),
            ingest: tx.clone(),
            plugins: plugins.clone(),
        },
    ));
    // Erst jetzt starten: Plugins können ab dem ersten Moment Daten abfragen
    let weak = Arc::downgrade(&app);
    plugins.start(tx.clone(), Arc::new(move |q| weak.upgrade().context("BosLook wird beendet")?.plugin_query(q)));
    let status = InputStatus::default();
    for i in config.inputs {
        let (tx, status) = (tx.clone(), status.clone());
        std::thread::spawn(move || {
            if let Err(e) = run_input(&i, &tx, &status) {
                report(&status, &i.name, false, format!("{e:#}"));
            }
        });
    }
    if config.update_check {
        let app = app.clone();
        std::thread::spawn(move || {
            loop {
                app.check_update();
                std::thread::sleep(crate::update::INTERVAL);
            }
        });
    }
    if let Some(weather) = config.weather {
        let (tx, app) = (tx.clone(), app.clone());
        std::thread::spawn(move || crate::weather::run(&weather, &app.db, &tx));
    }
    drop(tx);

    let forward = config.forward.map(|f| {
        let (ftx, frx) = mpsc::channel::<Telegram>(1024);
        tokio::spawn(forward::run(f, frx, forward::Remote { path: remote_path, running: running_remote, status }));
        ftx
    });

    let store = app.clone();
    tokio::spawn(async move {
        while let Some(t) = rx.recv().await {
            // Bearbeiten-Plugins zuerst, damit Regeln und Filter z. B. schon entschlüsselten Text sehen
            let Some(mut t) = plugins.process(t).await else { continue };
            // Erst nachschlagen und bearbeiten, dann filtern: Filter können auch Ort und Beschreibungen prüfen
            {
                let db = store.db.lock().unwrap();
                if let Err(e) = rewrites.describe_and_apply(&db, &mut t) {
                    eprintln!("Datenbank: {e:#}");
                }
                if !filters.allows(&t) {
                    continue;
                }
                match db.insert(&t) {
                    Ok(id) => t.id = id,
                    Err(e) => eprintln!("Datenbank: {e:#}"),
                }
            }
            println!("{}\t{}\t{}\t{}\t{}\t{}", t.time.format("%F %T"), t.source, t.kind.as_str(), t.address, t.sub, t.message);
            notify.handle(&t);
            plugins.observe(&t);
            let _ = store.live.send(t.clone());
            if let Some(forward) = &forward {
                let _ = forward.send(t).await;
            }
        }
    });

    let (Some(web), Some(addr)) = (config.web, addr) else {
        std::future::pending::<()>().await;
        return Ok(());
    };
    let service = web::router(app).into_make_service();
    match web.tls {
        Tls::Off => {
            eprintln!("Weboberfläche: http://{addr}");
            axum_server::bind(addr).serve(service).await?;
        }
        _ => {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let rustls = RustlsConfig::from_pem_file(&web.cert, &web.key)
                .await
                .with_context(|| format!("Zertifikat {} / {}", web.cert, web.key))?;
            eprintln!("Weboberfläche: https://{addr}");
            axum_server::bind_rustls(addr, rustls).serve(service).await?;
        }
    }
    Ok(())
}

/// Zustand eines Eingangs ins Log schreiben und für die Meldung an den Server merken
fn report(status: &InputStatus, name: &str, ok: bool, message: String) {
    eprintln!("Eingang '{name}': {message}");
    status.lock().unwrap().insert(name.to_string(), InputState { ok, message });
}

/// Abtastrate, mit der BosLook `rtl_fm` startet
const RTL_FM_RATE: u32 = 22050;

/// Laufende `rtl_fm`/`rtl_sdr`; werden vor einem Neustart beendet, damit der Stick für den neuen Prozess frei ist
static RTL_FM: std::sync::Mutex<Vec<Arc<std::sync::Mutex<std::process::Child>>>> = std::sync::Mutex::new(Vec::new());

fn run_input(input: &InputConfig, tx: &mpsc::Sender<Telegram>, status: &InputStatus) -> Result<()> {
    let send = |decoded: Vec<Decoded>| {
        for d in decoded {
            let _ = tx.blocking_send(Telegram::new(d, &input.name, chrono::Local::now()));
        }
    };
    let report = |ok, message| report(status, &input.name, ok, message);
    match &input.source {
        Source::Wav { path } => {
            let rate = hound::WavReader::open(path).with_context(|| format!("WAV-Datei {path}"))?.spec().sample_rate;
            let mut bank = Bank::new(&input.decoders, rate, input.pocsag)?;
            input::read_wav(path, |samples, _| send(bank.process(samples)))?;
            send(bank.finish());
            report(true, format!("Datei {path} fertig dekodiert"));
        }
        Source::Stdin { sample_rate } => {
            let mut bank = Bank::new(&input.decoders, *sample_rate, input.pocsag)?;
            report(true, format!("liest von stdin mit {sample_rate} Hz"));
            input::read_stdin_raw(*sample_rate, |samples, _| send(bank.process(samples)))?;
            send(bank.finish());
            report(false, "stdin beendet".into());
        }
        Source::Soundcard { device } => loop {
            // Bei Fehlern (z. B. USB-Soundkarte abgezogen) nach kurzer Pause neu öffnen
            let result = input::Soundcard::open(device.as_deref()).and_then(|card| {
                report(true, format!("Soundkarte mit {} Hz geöffnet", card.sample_rate));
                let mut bank = Bank::new(&input.decoders, card.sample_rate, input.pocsag)?;
                card.read(|samples| send(bank.process(samples)))
            });
            if let Err(e) = result {
                report(false, format!("{e:#}, neuer Versuch in 5 s"));
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        },
        Source::RtlSdr { .. } => loop {
            let result = run_rtl_sdr(input, &report, tx);
            if let Err(e) = result {
                report(false, format!("{e:#}, neuer Versuch in 5 s"));
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        },
        Source::RtlFm { .. } => loop {
            // Beendet sich rtl_fm (z. B. Stick abgezogen), nach kurzer Pause neu starten
            let result = run_rtl_fm(input, &report, &send);
            if let Err(e) = result {
                report(false, format!("{e:#}, neuer Versuch in 5 s"));
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        },
    }
    Ok(())
}

/// Startet `rtl_fm` und dekodiert dessen Ausgabe, bis es sich beendet
fn run_rtl_fm(input: &InputConfig, report: &dyn Fn(bool, String), send: &dyn Fn(Vec<Decoded>)) -> Result<()> {
    let Source::RtlFm { frequency, ppm, gain, device, squelch, program } = &input.source else { unreachable!() };
    let mut args = vec!["-f".to_string(), frequency.clone(), "-M".into(), "fm".into(), "-s".into(), RTL_FM_RATE.to_string()];
    args.extend(["-p".into(), ppm.to_string(), "-d".into(), device.clone(), "-l".into(), squelch.to_string()]);
    if gain != "auto" {
        args.extend(["-g".into(), gain.clone()]);
    }
    let mut bank = Bank::new(&input.decoders, RTL_FM_RATE, input.pocsag)?;
    run_rtl(&input.name, program.as_deref().unwrap_or("rtl_fm"), &args, &format!("rtl_fm auf {frequency} gestartet"), report, |stdout| {
        let read = input::read_raw(stdout, RTL_FM_RATE, |samples, _| send(bank.process(samples)));
        send(bank.finish());
        read
    })
}

/// Startet `rtl_sdr` und demoduliert dessen IQ-Daten für jeden Kanal, bis es sich beendet
fn run_rtl_sdr(input: &InputConfig, report: &dyn Fn(bool, String), tx: &mpsc::Sender<Telegram>) -> Result<()> {
    use std::io::Read;
    let Source::RtlSdr { ppm, gain, device, program, channels } = &input.source else { unreachable!() };
    let frequencies = channels.iter().map(|c| crate::sdr::parse_frequency(&c.frequency)).collect::<Result<Vec<_>>>()?;
    let plan = crate::sdr::plan(&frequencies)?;
    let mut args = vec!["-f".to_string(), format!("{:.0}", plan.center), "-s".into(), plan.rate.to_string()];
    args.extend(["-p".into(), ppm.to_string(), "-d".into(), device.clone()]);
    if gain != "auto" {
        args.extend(["-g".into(), gain.clone()]);
    }
    let mut demod: Vec<_> = channels
        .iter()
        .zip(&frequencies)
        .map(|(c, f)| Ok((c.name.clone(), crate::sdr::Channel::new(f - plan.center, plan.rate), Bank::new(&input.decoders, crate::sdr::AUDIO_RATE, input.pocsag)?)))
        .collect::<Result<_>>()?;
    let send = |source: &str, decoded: Vec<Decoded>| {
        for d in decoded {
            let _ = tx.blocking_send(Telegram::new(d, source, chrono::Local::now()));
        }
    };
    let names: Vec<_> = channels.iter().map(|c| format!("{} ({})", c.name, c.frequency)).collect();
    let started = format!("rtl_sdr auf {:.3} MHz mit {} kS/s gestartet: {}", plan.center / 1e6, plan.rate / 1000, names.join(", "));
    run_rtl(&input.name, program.as_deref().unwrap_or("rtl_sdr"), &args, &started, report, |mut stdout| {
        // Etwa 30 ms je Block, damit Telegramme ohne spürbare Verzögerung erscheinen
        let mut bytes = vec![0u8; 65_536];
        let (mut iq, mut audio) = (Vec::new(), Vec::new());
        let mut pending: Option<u8> = None;
        loop {
            let n = stdout.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            // Ein angefangenes IQ-Paar aus dem letzten Block vorn anfügen
            let mut block: Vec<u8> = pending.take().into_iter().chain(bytes[..n].iter().copied()).collect();
            if block.len() % 2 == 1 {
                pending = block.pop();
            }
            iq.clear();
            crate::sdr::iq_from_u8(&block, &mut iq);
            for (name, channel, bank) in &mut demod {
                audio.clear();
                channel.process(&iq, &mut audio);
                send(name, bank.process(&audio));
            }
        }
        for (name, _, bank) in &mut demod {
            send(name, bank.finish());
        }
        Ok(())
    })
}

/// Startet ein Programm der rtl-sdr-Werkzeuge, übergibt dessen Ausgabe an `read` und meldet,
/// warum es sich beendet hat
fn run_rtl(
    name: &str,
    program: &str,
    args: &[String],
    started: &str,
    report: &dyn Fn(bool, String),
    read: impl FnOnce(std::process::ChildStdout) -> Result<()>,
) -> Result<()> {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let tool = Path::new(program).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| program.to_string());
    let mut child = Command::new(program).args(args).arg("-").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow::anyhow!(
            "{program} nicht gefunden (Linux: apt install rtl-sdr; Windows: {tool}.exe neben boslook.exe legen oder Programm angeben)"
        ),
        _ => anyhow::Error::new(e).context(format!("{program} starten")),
    })?;
    // Ausgaben ins Log; die letzte Zeile erklärt meist, warum es sich beendet hat
    let last = Arc::new(std::sync::Mutex::new(String::new()));
    let stderr = child.stderr.take().expect("stderr");
    let (prefix, last_line) = (format!("Eingang '{name}': {tool}"), last.clone());
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()) {
            eprintln!("{prefix}: {line}");
            *last_line.lock().unwrap() = line;
        }
    });
    let stdout = child.stdout.take().expect("stdout");
    let child = Arc::new(std::sync::Mutex::new(child));
    RTL_FM.lock().unwrap().push(child.clone());
    report(true, started.to_string());
    let result = read(stdout);
    RTL_FM.lock().unwrap().retain(|c| !Arc::ptr_eq(c, &child));
    let mut child = child.lock().unwrap();
    let _ = child.kill();
    let exit = child.wait()?;
    result.with_context(|| format!("Ausgabe von {tool} lesen"))?;
    let last = last.lock().unwrap().clone();
    bail!("{tool} beendet ({exit}){}", if last.is_empty() { String::new() } else { format!(": {last}") })
}

const RESTART_WAIT: &str = "BOSLOOK_RESTART_WAIT";

/// Startet BosLook mit denselben Argumenten neu (nach Änderungen im Config-Editor).
pub fn restart() -> ! {
    for child in RTL_FM.lock().unwrap().iter() {
        if let Ok(mut child) = child.try_lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    let exe = crate::update::exe().expect("Programmpfad");
    let mut command = std::process::Command::new(exe);
    command.args(std::env::args_os().skip(1));
    #[cfg(unix)]
    {
        // Ersetzt den laufenden Prozess; stdin (z. B. von rtl_fm) bleibt dabei erhalten
        use std::os::unix::process::CommandExt;
        let e = command.exec();
        eprintln!("Neustart fehlgeschlagen: {e}");
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        if let Err(e) = command.env(RESTART_WAIT, "1").spawn() {
            eprintln!("Neustart fehlgeschlagen: {e}");
        }
        std::process::exit(0);
    }
}

/// Erstellt ein selbst signiertes Zertifikat, falls noch keins vorhanden ist.
fn ensure_self_signed(cert: &str, key: &str) -> Result<()> {
    if Path::new(cert).exists() && Path::new(key).exists() {
        return Ok(());
    }
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "boslook".to_string()])?;
    std::fs::write(cert, generated.cert.pem())?;
    std::fs::write(key, generated.signing_key.serialize_pem())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(key, std::fs::Permissions::from_mode(0o600))?;
    }
    eprintln!("Selbst signiertes Zertifikat erstellt: {cert}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(text: &str) -> Result<()> {
        validate_inputs(&toml::from_str::<crate::config::RemoteConfig>(text)?.inputs)
    }

    #[test]
    fn rtl_sdr_inputs_are_checked() {
        let stick = |channels: &str| format!("[[input]]\nname = \"Stick\"\ntype = \"rtl_sdr\"\n{channels}");
        let channel = |name: &str, f: &str| format!("[[input.channel]]\nname = \"{name}\"\nfrequency = \"{f}\"\n");
        assert!(inputs(&stick(&(channel("A", "173.255M") + &channel("B", "172.9M")))).is_ok());
        let err = inputs(&stick(&(channel("A", "173.255M") + &channel("B", "171.9M")))).unwrap_err();
        assert!(format!("{err:#}").contains("höchstens 1000 kHz"), "{err:#}");
        assert!(inputs(&stick(&(channel("A", "173.255M") + &channel("A", "172.9M")))).is_err());
        assert!(inputs(&stick(&channel("A", "viel"))).is_err());
        assert!(inputs(&stick("")).is_err());
        // Kanalname gleich einem anderen Eingang
        let err = inputs(&(stick(&channel("Funk", "173.255M")) + "[[input]]\nname = \"Funk\"\ntype = \"soundcard\"\n")).unwrap_err();
        assert!(format!("{err:#}").contains("schon vergeben"), "{err:#}");
    }
}
