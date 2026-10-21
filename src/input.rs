//! Audioquellen: WAV-Datei oder rohe 16-Bit-Samples von stdin (z. B. von `rtl_fm` oder `arecord`).

use anyhow::{Context, Result};
use std::io::Read;

const CHUNK: usize = 4096;

/// Liest eine WAV-Datei blockweise als Mono-Signal (-1.0..1.0).
/// `f` bekommt die Samples und die Position (in Sekunden) am Blockende.
pub fn read_wav(path: &str, mut f: impl FnMut(&[f32], f64)) -> Result<u32> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("WAV-Datei {path}"))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let rate = spec.sample_rate;
    let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
    let samples: Box<dyn Iterator<Item = hound::Result<f32>>> = match spec.sample_format {
        hound::SampleFormat::Int => Box::new(reader.samples::<i32>().map(move |s| s.map(|v| v as f32 * scale))),
        hound::SampleFormat::Float => Box::new(reader.samples::<f32>()),
    };

    let mut block = Vec::with_capacity(CHUNK);
    let mut total = 0usize;
    // Bei Stereo wird nur der erste Kanal ausgewertet
    for s in samples.step_by(channels) {
        block.push(s?);
        if block.len() == CHUNK {
            total += block.len();
            f(&block, total as f64 / rate as f64);
            block.clear();
        }
    }
    total += block.len();
    f(&block, total as f64 / rate as f64);
    Ok(rate)
}

/// Liest rohe Samples (signed 16 Bit little endian, mono) von stdin, bis der Strom endet.
pub fn read_stdin_raw(sample_rate: u32, mut f: impl FnMut(&[f32], f64)) -> Result<()> {
    let mut stdin = std::io::stdin().lock();
    let mut buf = vec![0u8; CHUNK * 2];
    let mut block = Vec::with_capacity(CHUNK);
    let mut total = 0usize;
    let mut pending: Option<u8> = None;
    loop {
        let n = stdin.read(&mut buf).context("stdin lesen")?;
        if n == 0 {
            return Ok(());
        }
        block.clear();
        let mut bytes = pending.take().into_iter().chain(buf[..n].iter().copied());
        while let Some(lo) = bytes.next() {
            match bytes.next() {
                Some(hi) => block.push(i16::from_le_bytes([lo, hi]) as f32 / 32768.0),
                None => pending = Some(lo),
            }
        }
        total += block.len();
        f(&block, total as f64 / sample_rate as f64);
    }
}
