mod config;
mod db;
mod decoder;
mod filter;
mod forward;
mod input;
mod serve;
mod telegram;
mod web;

use anyhow::Result;
use argon2::password_hash::PasswordHasher;
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
    /// Aufnahmegeräte (Soundkarten) auflisten
    Devices,
    /// Passwort-Hash für die Konfiguration erzeugen (Passwort wird von stdin gelesen)
    HashPassword,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Decode { file, decoders, pocsag } => decode(&file, &decoders, pocsag),
        Command::Serve { config } => tokio::runtime::Runtime::new()?.block_on(serve::run(&config)),
        Command::Devices => input::list_devices(),
        Command::HashPassword => hash_password(),
    }
}

fn hash_password() -> Result<()> {
    eprint!("Passwort: ");
    let mut password = String::new();
    std::io::stdin().read_line(&mut password)?;
    let password = password.trim_end_matches(['\r', '\n']);
    if password.is_empty() {
        anyhow::bail!("Leeres Passwort");
    }
    let hash = argon2::Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{hash}");
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
