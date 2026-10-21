//! `boslook serve`: Eingänge dekodieren, filtern, speichern und per Weboberfläche anzeigen.

use crate::config::{Config, InputConfig, Source, Tls};
use crate::db::Db;
use crate::decoder::Bank;
use crate::filter::Filters;
use crate::telegram::{Decoded, Telegram};
use crate::web::{self, App};
use crate::input;
use anyhow::{Context, Result, bail};
use argon2::password_hash::phc::PasswordHash;
use axum_server::tls_rustls::RustlsConfig;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;

pub async fn run(config_path: &str) -> Result<()> {
    let config = Config::load(config_path)?;
    if config.users.is_empty() {
        bail!("Kein Benutzer konfiguriert. Passwort-Hash erzeugen mit: boslook hash-password");
    }
    for u in &config.users {
        PasswordHash::new(&u.password_hash)
            .map_err(|e| anyhow::anyhow!("Benutzer '{}': ungültiger password_hash ({e})", u.name))?;
    }
    if config.inputs.iter().filter(|i| matches!(i.source, Source::Stdin { .. })).count() > 1 {
        bail!("Es kann nur einen Eingang vom Typ 'stdin' geben");
    }
    for i in &config.inputs {
        Bank::new(&i.decoders, 22050).with_context(|| format!("Eingang '{}'", i.name))?;
    }
    let addr: SocketAddr = config.web.listen.parse().context("web.listen")?;
    let filters = Arc::new(Filters::new(&config.filters)?);
    let db = Db::open(&config.database).with_context(|| format!("Datenbank {}", config.database))?;
    let app = Arc::new(App::new(db, config.users, config.web.tls != Tls::Off));

    let (tx, mut rx) = mpsc::channel::<Telegram>(1024);
    for i in config.inputs {
        let (filters, tx) = (filters.clone(), tx.clone());
        std::thread::spawn(move || {
            if let Err(e) = run_input(&i, &filters, &tx) {
                eprintln!("Eingang '{}': {e:#}", i.name);
            }
        });
    }
    drop(tx);

    let store = app.clone();
    tokio::spawn(async move {
        while let Some(mut t) = rx.recv().await {
            match store.db.lock().unwrap().insert(&t) {
                Ok(id) => t.id = id,
                Err(e) => eprintln!("Datenbank: {e:#}"),
            }
            println!("{}\t{}\t{}\t{}\t{}\t{}", t.time.format("%F %T"), t.source, t.kind.as_str(), t.address, t.sub, t.message);
            let _ = store.live.send(t);
        }
    });

    let service = web::router(app).into_make_service();
    match config.web.tls {
        Tls::Off => {
            eprintln!("Weboberfläche: http://{addr}");
            axum_server::bind(addr).serve(service).await?;
        }
        tls => {
            if tls == Tls::SelfSigned {
                ensure_self_signed(&config.web.cert, &config.web.key)?;
            }
            let _ = rustls::crypto::ring::default_provider().install_default();
            let rustls = RustlsConfig::from_pem_file(&config.web.cert, &config.web.key)
                .await
                .with_context(|| format!("Zertifikat {} / {}", config.web.cert, config.web.key))?;
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
            let mut bank = Bank::new(&input.decoders, rate)?;
            input::read_wav(path, |samples, _| send(bank.process(samples)))?;
            send(bank.finish());
            eprintln!("Eingang '{}': Datei {path} fertig dekodiert", input.name);
        }
        Source::Stdin { sample_rate } => {
            let mut bank = Bank::new(&input.decoders, *sample_rate)?;
            input::read_stdin_raw(*sample_rate, |samples, _| send(bank.process(samples)))?;
            send(bank.finish());
            eprintln!("Eingang '{}': stdin beendet", input.name);
        }
    }
    Ok(())
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
