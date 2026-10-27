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
        Bank::new(&i.decoders, 22050, i.pocsag).with_context(|| format!("Eingang '{}'", i.name))?;
        if let Source::RtlFm { frequency, gain, .. } = &i.source {
            if frequency.trim().is_empty() {
                bail!("Eingang '{}': Frequenz fehlt", i.name);
            }
            if gain != "auto" && gain.parse::<f32>().is_err() {
                bail!("Eingang '{}': Verstärkung muss eine Zahl (dB) oder auto sein", i.name);
            }
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
                .map(|i| i.name.clone())
                .chain(config.weather.as_ref().map(|_| crate::weather::SOURCE.to_string()))
                .chain(plugins.sources())
                .collect(),
            secure_cookie: config.web.as_ref().is_some_and(|w| w.tls != Tls::Off),
            clients: config.clients,
            fingerprint,
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

/// Laufende `rtl_fm`; werden vor einem Neustart beendet, damit der Stick für den neuen Prozess frei ist
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
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let Source::RtlFm { frequency, ppm, gain, device, squelch, program } = &input.source else { unreachable!() };
    let program = program.as_deref().unwrap_or("rtl_fm");
    let mut command = Command::new(program);
    command.args(["-f", frequency, "-M", "fm", "-s", &RTL_FM_RATE.to_string(), "-p", &ppm.to_string()]);
    command.args(["-d", device, "-l", &squelch.to_string()]);
    if gain != "auto" {
        command.args(["-g", gain]);
    }
    command.arg("-").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow::anyhow!(
            "{program} nicht gefunden (Linux: apt install rtl-sdr; Windows: rtl_fm.exe neben boslook.exe legen oder Programm angeben)"
        ),
        _ => anyhow::Error::new(e).context(format!("{program} starten")),
    })?;
    // Ausgaben von rtl_fm ins Log; die letzte Zeile erklärt meist, warum es sich beendet hat
    let last = Arc::new(std::sync::Mutex::new(String::new()));
    let stderr = child.stderr.take().expect("stderr");
    let (name, last_line) = (input.name.clone(), last.clone());
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()) {
            eprintln!("Eingang '{name}': rtl_fm: {line}");
            *last_line.lock().unwrap() = line;
        }
    });
    let stdout = child.stdout.take().expect("stdout");
    let child = Arc::new(std::sync::Mutex::new(child));
    RTL_FM.lock().unwrap().push(child.clone());
    report(true, format!("rtl_fm auf {frequency} gestartet"));
    let mut bank = Bank::new(&input.decoders, RTL_FM_RATE, input.pocsag)?;
    let read = input::read_raw(stdout, RTL_FM_RATE, |samples, _| send(bank.process(samples)));
    send(bank.finish());
    RTL_FM.lock().unwrap().retain(|c| !Arc::ptr_eq(c, &child));
    let mut child = child.lock().unwrap();
    let _ = child.kill();
    let exit = child.wait()?;
    read.context("Ausgabe von rtl_fm lesen")?;
    let last = last.lock().unwrap().clone();
    bail!("rtl_fm beendet ({exit}){}", if last.is_empty() { String::new() } else { format!(": {last}") })
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
    let exe = std::env::current_exe().expect("Programmpfad");
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
