pub mod fms;
pub mod pocsag;
pub mod zvei;

#[cfg(test)]
pub mod testsignal;

use crate::telegram::Decoded;
use anyhow::{Result, bail};

pub const ALL: &[&str] = &["pocsag512", "pocsag1200", "fms", "zvei"];

/// Ein Dekoder bekommt Audio-Samples (mono, -1.0..1.0) und liefert Telegramme.
pub trait Decoder: Send {
    fn process(&mut self, samples: &[f32], out: &mut Vec<Decoded>);
    /// Am Ende des Datenstroms aufrufen, um angefangene Telegramme auszugeben.
    fn finish(&mut self, _out: &mut Vec<Decoded>) {}
}

/// Mehrere Dekoder, die parallel dasselbe Audiosignal auswerten.
pub struct Bank(Vec<Box<dyn Decoder>>);

impl Bank {
    pub fn new(names: &[String], sample_rate: u32) -> Result<Bank> {
        Ok(Bank(names.iter().map(|n| create(n, sample_rate)).collect::<Result<_>>()?))
    }

    pub fn process(&mut self, samples: &[f32]) -> Vec<Decoded> {
        let mut out = Vec::new();
        for d in &mut self.0 {
            d.process(samples, &mut out);
        }
        out
    }

    pub fn finish(&mut self) -> Vec<Decoded> {
        let mut out = Vec::new();
        for d in &mut self.0 {
            d.finish(&mut out);
        }
        out
    }
}

pub fn create(name: &str, sample_rate: u32) -> Result<Box<dyn Decoder>> {
    Ok(match name {
        "pocsag512" => Box::new(pocsag::Pocsag::new(512, sample_rate)),
        "pocsag1200" => Box::new(pocsag::Pocsag::new(1200, sample_rate)),
        "fms" => Box::new(fms::Fms::new(sample_rate)),
        "zvei" => Box::new(zvei::Zvei::new(sample_rate)),
        _ => bail!("Unbekannter Dekoder '{name}' (verfügbar: {})", ALL.join(", ")),
    })
}
