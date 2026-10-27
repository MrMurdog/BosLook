use anyhow::Result;
use boslook::{config, decoder, input, notify, plugin, serve, telegram, weather};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "boslook", version, about = "Dekodiert ZVEI-, FMS- und POCSAG-Telegramme")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// WAV-Datei dekodieren und Telegramme ausgeben
    Decode {
        file: String,
        /// Dekoder, kommagetrennt
        #[arg(short, long, value_delimiter = ',', default_values_t = decoder::ALL.iter().map(|s| s.to_string()))]
        decoders: Vec<String>,
        /// Darstellung von POCSAG-Meldungen
        #[arg(short, long, value_enum, default_value_t)]
        pocsag: decoder::pocsag::Format,
    },
    /// Eingänge auswerten und Weboberfläche starten
    Serve {
        #[arg(short, long, default_value = "boslook.toml")]
        config: String,
    },
    /// Testnachricht an alle konfigurierten Benachrichtigungsziele senden
    TestNotify {
        #[arg(short, long, default_value = "boslook.toml")]
        config: String,
    },
    /// Warncell-ID für Wetterwarnungen suchen, z. B. `boslook warncell Leipzig`
    Warncell { name: String },
    /// Aufnahmegeräte (Soundkarten) auflisten
    Devices,
    /// Passwort-Hash für die Konfiguration erzeugen (Passwort wird von stdin gelesen)
    HashPassword,
    /// Client mit einem Einrichtungscode aus der Weboberfläche des Servers einrichten
    Setup {
        /// Einrichtungscode (beginnt mit boslook1:)
        code: String,
        #[arg(short, long, default_value = "boslook.toml")]
        config: String,
        /// Als Dienst einrichten und starten (Linux: systemd, als root; Windows: Aufgabe beim Systemstart)
        #[arg(long)]
        service: bool,
        /// Vorhandene Konfiguration überschreiben
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Decode { file, decoders, pocsag } => decode(&file, &decoders, pocsag),
        Command::Serve { config } => tokio::runtime::Runtime::new()?.block_on(serve::run(&config)),
        Command::TestNotify { config } => test_notify(&config),
        Command::Warncell { name } => weather::search(&name),
        Command::Devices => input::list_devices(),
        Command::HashPassword => hash_password(),
        Command::Setup { code, config, service, force } => boslook::setup::run(&code, &config, service, force),
    }
}

fn test_notify(path: &str) -> Result<()> {
    let config = config::Config::load(path)?;
    if config.notifiers.is_empty() {
        anyhow::bail!("Keine [[notifier]] konfiguriert");
    }
    // Dienste von Plugins brauchen die laufenden Plugins
    let runtime = tokio::runtime::Runtime::new()?;
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let plugins = plugin::Plugins::new(&config, &plugin::dir(path))?;
    runtime.block_on(async { plugins.start(tx, std::sync::Arc::new(|_| anyhow::bail!("bei test-notify gibt es keine Daten"))) });
    for n in &config.notifiers {
        let sender = {
            let _runtime = runtime.enter();
            notify::Sender::new(&n.target, &plugins)
        };
        match sender.send(n, "BosLook Test", "Testnachricht von BosLook", notify::Priority::Normal, None) {
            Ok(()) => println!("{}: gesendet", n.name),
            Err(e) => println!("{}: Fehler: {e:#}", n.name),
        }
    }
    Ok(())
}

fn hash_password() -> Result<()> {
    eprint!("Passwort: ");
    let mut password = String::new();
    std::io::stdin().read_line(&mut password)?;
    let password = password.trim_end_matches(['\r', '\n']);
    if password.is_empty() {
        anyhow::bail!("Leeres Passwort");
    }
    println!("{}", boslook::form::hash(password)?);
    Ok(())
}

fn decode(file: &str, decoders: &[String], pocsag: decoder::pocsag::Format) -> Result<()> {
    let spec = hound::WavReader::open(file)?.spec();
    let mut bank = decoder::Bank::new(decoders, spec.sample_rate, pocsag)?;
    let print = |pos: f64, d: telegram::Decoded| {
        let t = pos as u64;
        println!(
            "{:02}:{:02}:{:02}\t{}\t{}\t{}\t{}",
            t / 3600,
            t / 60 % 60,
            t % 60,
            d.kind.as_str(),
            d.address,
            d.sub,
            d.message
        );
    };
    let mut end = 0.0;
    input::read_wav(file, |samples, pos| {
        end = pos;
        for d in bank.process(samples) {
            print(pos, d);
        }
    })?;
    for d in bank.finish() {
        print(end, d);
    }
    Ok(())
}
