//! Audioquellen: WAV-Datei, rohe 16-Bit-Samples von stdin (z. B. von `rtl_fm` oder `arecord`) oder Soundkarte.

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

/// Aufnahme von einer Soundkarte. Ausgewertet wird nur der erste Kanal.
pub struct Soundcard {
    _stream: cpal::Stream,
    pub sample_rate: u32,
    rx: std::sync::mpsc::Receiver<Result<Vec<f32>, String>>,
}

impl Soundcard {
    /// `device`: ID oder Name aus `boslook devices`, sonst das Standardgerät.
    pub fn open(device: Option<&str>) -> Result<Soundcard> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let host = cpal::default_host();
        let dev = match device {
            None => host.default_input_device().context("Kein Aufnahmegerät gefunden")?,
            Some(wanted) => host
                .input_devices()?
                .find(|d| {
                    d.id().is_ok_and(|id| id.to_string() == wanted)
                        || d.description().is_ok_and(|desc| desc.name() == wanted)
                })
                .with_context(|| format!("Aufnahmegerät '{wanted}' nicht gefunden (siehe boslook devices)"))?,
        };
        let config = dev.default_input_config()?;
        let sample_rate = config.sample_rate();
        let channels = config.channels() as usize;
        let format = config.sample_format();
        let mut stream_config: cpal::StreamConfig = config.into();
        // Der Standardpuffer ist bei manchen Treibern so klein, dass ständig Samples verloren gehen
        let wanted = sample_rate / 10;
        if let cpal::SupportedBufferSize::Range { min, max } = config.buffer_size()
            && (*min..=*max).contains(&wanted)
        {
            stream_config.buffer_size = cpal::BufferSize::Fixed(wanted);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let stream = match format {
            cpal::SampleFormat::I16 => build::<i16>(&dev, &stream_config, channels, tx)?,
            cpal::SampleFormat::I32 => build::<i32>(&dev, &stream_config, channels, tx)?,
            cpal::SampleFormat::F32 => build::<f32>(&dev, &stream_config, channels, tx)?,
            format => anyhow::bail!("Sampleformat {format} der Soundkarte wird nicht unterstützt"),
        };
        stream.play()?;
        Ok(Soundcard { _stream: stream, sample_rate, rx })
    }

    /// Liefert die Samples an `f`, bis die Soundkarte einen Fehler meldet.
    pub fn read(&self, mut f: impl FnMut(&[f32])) -> Result<()> {
        loop {
            match self.rx.recv() {
                Ok(Ok(samples)) => f(&samples),
                Ok(Err(e)) => anyhow::bail!("Soundkarte: {e}"),
                Err(_) => anyhow::bail!("Soundkarte: Datenstrom beendet"),
            }
        }
    }
}

fn build<T>(
    dev: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    tx: std::sync::mpsc::Sender<Result<Vec<f32>, String>>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait;
    let errors = tx.clone();
    Ok(dev.build_input_stream(
        *config,
        move |data: &[T], _: &_| {
            let _ = tx.send(Ok(data.iter().step_by(channels).map(|s| s.to_sample::<f32>()).collect()));
        },
        move |e| match e.kind() {
            // Kurze Aussetzer sind kein Grund, die Aufnahme abzubrechen
            cpal::ErrorKind::Xrun => eprintln!("Soundkarte: {e}"),
            _ => {
                let _ = errors.send(Err(e.to_string()));
            }
        },
        None,
    )?)
}

/// Gibt die Aufnahmegeräte für die Konfiguration (`device = ...`) aus.
pub fn list_devices() -> Result<()> {
    for (id, description) in devices()? {
        println!("{id}\t{description}");
    }
    Ok(())
}

/// Eingabegeräte als (ID, Beschreibung mit Name, Abtastrate und Standard-Markierung)
pub fn devices() -> Result<Vec<(String, String)>> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.id().ok());
    let mut list = Vec::new();
    for dev in host.input_devices()? {
        let id = dev.id()?;
        let name = dev.description().map(|d| d.name().to_string()).unwrap_or_default();
        let rate = dev.default_input_config().map(|c| format!("{} Hz", c.sample_rate())).unwrap_or_default();
        let mark = if Some(&id) == default.as_ref() { " (Standard)" } else { "" };
        list.push((id.to_string(), format!("{name}\t{rate}{mark}")));
    }
    Ok(list)
}
