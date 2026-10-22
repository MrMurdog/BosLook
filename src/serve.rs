//! `boslook serve`: Eingänge dekodieren, filtern, speichern und per Weboberfläche anzeigen.

use crate::config::{Config, InputConfig, Source, Tls};
use crate::db::Db;
use crate::decoder::Bank;
use crate::filter::Filters;
use crate::notify::Notify;
use crate::telegram::{Decoded, Telegram};
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
pub fn validate(config: &Config) -> Result<()> {
    if config.web.is_some() && config.users.is_empty() {
        bail!("Kein Benutzer konfiguriert. Passwort-Hash erzeugen mit: boslook hash-password");
    }
    if config.web.is_none() && !(config.users.is_empty() && config.clients.is_empty()) {
        bail!("[[user]] und [[client]] funktionieren nur zusammen mit [web]");
    }
    let hashes = config.users.iter().map(|u| ("Benutzer", &u.name, &u.password_hash));
    for (what, name, hash) in hashes.chain(config.clients.iter().map(|c| ("Client", &c.name, &c.token_hash))) {
        PasswordHash::new(hash).map_err(|e| anyhow::anyhow!("{what} '{name}': ungültiger Hash ({e})"))?;
    }
    if config.inputs.iter().filter(|i| matches!(i.source, Source::Stdin { .. })).count() > 1 {
        bail!("Es kann nur einen Eingang vom Typ 'stdin' geben");
    }
    for (n, i) in config.inputs.iter().enumerate() {
        if config.inputs[..n].iter().any(|j| j.name == i.name) {
            bail!("Eingang '{}' ist doppelt vorhanden, die Namen müssen eindeutig sein", i.name);
        }
        Bank::new(&i.decoders, 22050, i.pocsag).with_context(|| format!("Eingang '{}'", i.name))?;
    }
    if let Some(w) = &config.weather
        && (w.warncells.is_empty() || w.interval < 60)
    {
        bail!("[weather]: mindestens eine Warncell-ID und interval von mindestens 60 Sekunden angeben");
    }
    config.web.as_ref().map(|w| w.listen.parse::<SocketAddr>()).transpose().context("web.listen")?;
    Filters::new(&config.filters)?;
    crate::notify::check(&config.alarms, &config.notifiers)?;
    Ok(())
}

pub async fn run(config_path: &str) -> Result<()> {
    if std::env::var_os(RESTART_WAIT).is_some() {
        // Windows: warten, bis der alte Prozess den Port freigegeben hat (siehe restart())
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let config = Config::load(config_path)?;
    validate(&config)?;
    let addr: Option<SocketAddr> = config.web.as_ref().map(|w| w.listen.parse()).transpose().context("web.listen")?;
    let filters = Arc::new(Filters::new(&config.filters)?);
    let mut notify = Notify::new(&config.alarms, config.notifiers, Duration::from_secs(config.notify_repeat))?;
    let db = Db::open(&config.database).with_context(|| format!("Datenbank {}", config.database))?;

    let (tx, mut rx) = mpsc::channel::<Telegram>(1024);
    let app = Arc::new(App::new(
        db,
        Settings {
            users: config.users,
            inputs: config
                .inputs
                .iter()
                .map(|i| i.name.clone())
                .chain(config.weather.as_ref().map(|_| crate::weather::SOURCE.to_string()))
                .collect(),
            secure_cookie: config.web.as_ref().is_some_and(|w| w.tls != Tls::Off),
            clients: config.clients,
            config_path: config_path.to_string(),
            ingest: tx.clone(),
            filters: filters.clone(),
        },
    ));
    for i in config.inputs {
        let (filters, tx) = (filters.clone(), tx.clone());
        std::thread::spawn(move || {
            if let Err(e) = run_input(&i, &filters, &tx) {
                eprintln!("Eingang '{}': {e:#}", i.name);
            }
        });
    }
    if let Some(weather) = config.weather {
        let (filters, tx, app) = (filters.clone(), tx.clone(), app.clone());
        std::thread::spawn(move || crate::weather::run(&weather, &filters, &app.db, &tx));
    }
    drop(tx);

    let forward = config.forward.map(|f| {
        let (ftx, frx) = mpsc::channel::<Telegram>(1024);
        tokio::spawn(forward::run(f, frx));
        ftx
    });

    let store = app.clone();
    tokio::spawn(async move {
        while let Some(mut t) = rx.recv().await {
            match store.db.lock().unwrap().insert(&t) {
                Ok(id) => t.id = id,
                Err(e) => eprintln!("Datenbank: {e:#}"),
            }
            println!("{}\t{}\t{}\t{}\t{}\t{}", t.time.format("%F %T"), t.source, t.kind.as_str(), t.address, t.sub, t.message);
            notify.handle(&t);
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
        tls => {
            if tls == Tls::SelfSigned {
                ensure_self_signed(&web.cert, &web.key)?;
            }
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

fn run_input(input: &InputConfig, filters: &Filters, tx: &mpsc::Sender<Telegram>) -> Result<()> {
    let send = |decoded: Vec<Decoded>| {
        for d in decoded.into_iter().filter(|d| filters.allows(d)) {
            let _ = tx.blocking_send(Telegram::new(d, &input.name, chrono::Local::now()));
        }
    };
    match &input.source {
        Source::Wav { path } => {
            let rate = hound::WavReader::open(path).with_context(|| format!("WAV-Datei {path}"))?.spec().sample_rate;
            let mut bank = Bank::new(&input.decoders, rate, input.pocsag)?;
            input::read_wav(path, |samples, _| send(bank.process(samples)))?;
            send(bank.finish());
            eprintln!("Eingang '{}': Datei {path} fertig dekodiert", input.name);
        }
        Source::Stdin { sample_rate } => {
            let mut bank = Bank::new(&input.decoders, *sample_rate, input.pocsag)?;
            input::read_stdin_raw(*sample_rate, |samples, _| send(bank.process(samples)))?;
            send(bank.finish());
            eprintln!("Eingang '{}': stdin beendet", input.name);
        }
        Source::Soundcard { device } => loop {
            // Bei Fehlern (z. B. USB-Soundkarte abgezogen) nach kurzer Pause neu öffnen
            let result = input::Soundcard::open(device.as_deref()).and_then(|card| {
                eprintln!("Eingang '{}': Soundkarte mit {} Hz geöffnet", input.name, card.sample_rate);
                let mut bank = Bank::new(&input.decoders, card.sample_rate, input.pocsag)?;
                card.read(|samples| send(bank.process(samples)))
            });
            if let Err(e) = result {
                eprintln!("Eingang '{}': {e:#}, neuer Versuch in 5 s", input.name);
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        },
    }
    Ok(())
}

const RESTART_WAIT: &str = "BOSLOOK_RESTART_WAIT";

/// Startet BosLook mit denselben Argumenten neu (nach Änderungen im Config-Editor).
pub fn restart() -> ! {
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
