//! Mehrere Kanäle aus einem RTL-SDR-Stick: rohe IQ-Daten von `rtl_sdr` werden je Kanal auf die
//! Kanalfrequenz verschoben, gefiltert, auf [`AUDIO_RATE`] heruntergerechnet und FM-demoduliert.

use anyhow::{Result, bail};
use std::f64::consts::TAU;

/// Abtastrate des demodulierten Audiosignals
pub const AUDIO_RATE: u32 = 24_000;
/// Größter Abstand zwischen der niedrigsten und der höchsten Frequenz eines Sticks
pub const MAX_SPAN: f64 = 1_000_000.0;
/// Abtastraten des Sticks, die kleinste passende wird genommen (ganzzahlige Vielfache von [`AUDIO_RATE`])
const RATES: [u32; 3] = [240_000, 1_200_000, 2_400_000];
/// Halbe Bandbreite eines Kanals (12,5-kHz-Raster) plus Reserve für die Filterflanke
const CHANNEL_HALF: f64 = 12_500.0;
/// Abstand der Kanäle zur Mittenfrequenz, damit der Gleichspannungsanteil des Sticks nicht stört
const DC_GAP: f64 = 25_000.0;
/// Durchlass (bis) und Sperre (ab) des Kanalfilters in Hz: FM mit ±4,5 kHz Hub und 1200 Baud
const PASS: f64 = 6_500.0;
const STOP: f64 = 11_500.0;

/// Frequenzangabe wie `173.255M`, `173255k` oder `173255000` in Hz
pub fn parse_frequency(text: &str) -> Result<f64> {
    let t = text.trim().trim_end_matches(['H', 'h', 'z', 'Z']).trim();
    let (number, factor) = match t.chars().last() {
        Some('M' | 'm') => (&t[..t.len() - 1], 1e6),
        Some('k' | 'K') => (&t[..t.len() - 1], 1e3),
        Some('G' | 'g') => (&t[..t.len() - 1], 1e9),
        _ => (t, 1.0),
    };
    match number.trim().replace(',', ".").parse::<f64>() {
        Ok(f) if f > 0.0 => Ok(f * factor),
        _ => bail!("Frequenz '{text}' nicht lesbar, z. B. 173.255M"),
    }
}

/// Mittenfrequenz und Abtastrate für die Kanäle
#[derive(Debug, PartialEq)]
pub struct Plan {
    pub center: f64,
    pub rate: u32,
}

pub fn plan(frequencies: &[f64]) -> Result<Plan> {
    let (Some(min), Some(max)) = (frequencies.iter().copied().reduce(f64::min), frequencies.iter().copied().reduce(f64::max)) else {
        bail!("Mindestens ein Kanal mit Frequenz nötig");
    };
    if max - min > MAX_SPAN {
        bail!("Die Kanäle liegen {:.0} kHz auseinander, höchstens {:.0} kHz gehen mit einem Stick", (max - min) / 1e3, MAX_SPAN / 1e3);
    }
    // Möglichst die Mitte zwischen den Kanälen; liegt dort ein Kanal, knapp daneben
    let mid = (min + max) / 2.0;
    let reach = |c: f64| frequencies.iter().map(|f| (f - c).abs()).fold(0.0, f64::max) + CHANNEL_HALF;
    let center = [mid, mid - DC_GAP, mid + DC_GAP]
        .into_iter()
        .chain(frequencies.iter().flat_map(|f| [f - DC_GAP, f + DC_GAP]))
        .filter(|c| frequencies.iter().all(|f| (f - c).abs() >= DC_GAP))
        .min_by(|a, b| reach(*a).total_cmp(&reach(*b)))
        .unwrap_or(max + DC_GAP);
    let reach = reach(center);
    // Die Filter brauchen etwas Platz bis zum Rand des Bands
    let Some(rate) = RATES.into_iter().find(|r| reach <= 0.45 * *r as f64) else {
        bail!("Kanäle passen nicht in die Bandbreite des Sticks");
    };
    Ok(Plan { center, rate })
}

#[derive(Clone, Copy, Debug, Default)]
struct C32 {
    re: f32,
    im: f32,
}

impl C32 {
    fn mul(self, o: C32) -> C32 {
        C32 { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
}

/// FIR-Tiefpass mit Dezimierung: berechnet nur jedes `factor`-te Ausgangssample
struct Decimator {
    taps: Vec<f32>,
    factor: usize,
    /// Die letzten Eingangssamples (Länge der Filter minus eins) plus neue
    history: Vec<C32>,
    /// Position des nächsten Ausgangssamples in `history`
    next: usize,
}

impl Decimator {
    /// Windowed-Sinc (Blackman) mit Grenzfrequenz `cutoff` und Übergangsbreite `transition` (Hz)
    fn new(rate: f64, factor: usize, cutoff: f64, transition: f64) -> Decimator {
        let n = ((5.5 * rate / transition).ceil() as usize) | 1;
        let fc = cutoff / rate;
        let mid = (n / 2) as f64;
        let mut taps: Vec<f64> = (0..n)
            .map(|i| {
                let x = i as f64 - mid;
                let sinc = if x == 0.0 { 2.0 * fc } else { (TAU * fc * x).sin() / (std::f64::consts::PI * x) };
                let w = 0.42 - 0.5 * (TAU * i as f64 / (n - 1) as f64).cos() + 0.08 * (2.0 * TAU * i as f64 / (n - 1) as f64).cos();
                sinc * w
            })
            .collect();
        let sum: f64 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Decimator { taps: taps.into_iter().map(|t| t as f32).collect(), factor, history: vec![C32::default(); n - 1], next: n - 1 }
    }

    fn process(&mut self, input: &[C32], out: &mut Vec<C32>) {
        self.history.extend_from_slice(input);
        let n = self.taps.len();
        while self.next < self.history.len() {
            let window = &self.history[self.next + 1 - n..=self.next];
            let mut acc = C32::default();
            for (t, x) in self.taps.iter().zip(window) {
                acc.re += t * x.re;
                acc.im += t * x.im;
            }
            out.push(acc);
            self.next += self.factor;
        }
        // Nur so viel behalten, wie das Filter für das nächste Ausgangssample braucht
        let keep_from = self.history.len() - (n - 1);
        self.history.drain(..keep_from);
        self.next -= keep_from;
    }
}

/// Ein Kanal: Frequenz verschieben, filtern, dezimieren, FM-demodulieren
pub struct Channel {
    rotation: C32,
    step: C32,
    stages: Vec<Decimator>,
    last: C32,
    since_normalize: usize,
    mixed: Vec<C32>,
    stage_out: Vec<C32>,
}

impl Channel {
    /// `offset`: Abstand der Kanalfrequenz zur Mittenfrequenz in Hz
    pub fn new(offset: f64, rate: u32) -> Channel {
        let phase = -TAU * offset / rate as f64;
        let mut stages = Vec::new();
        let mut current = rate as f64;
        let mut total = (rate / AUDIO_RATE) as usize;
        // Zuerst grob um 10 (breite Flanke, wenige Koeffizienten), dann auf die Audiorate
        while total > 10 {
            let out = current / 10.0;
            stages.push(Decimator::new(current, 10, out / 2.0, out - 2.0 * PASS));
            current = out;
            total /= 10;
        }
        stages.push(Decimator::new(current, total, (PASS + STOP) / 2.0, STOP - PASS));
        Channel {
            rotation: C32 { re: 1.0, im: 0.0 },
            step: C32 { re: phase.cos() as f32, im: phase.sin() as f32 },
            stages,
            last: C32::default(),
            since_normalize: 0,
            mixed: Vec::new(),
            stage_out: Vec::new(),
        }
    }

    /// IQ-Samples des Sticks rein, Audio mit [`AUDIO_RATE`] raus (angehängt an `audio`)
    pub fn process(&mut self, iq: &[Iq], audio: &mut Vec<f32>) {
        self.mixed.clear();
        for s in iq {
            self.mixed.push(C32 { re: s.0, im: s.1 }.mul(self.rotation));
            self.rotation = self.rotation.mul(self.step);
            self.since_normalize += 1;
            if self.since_normalize == 1024 {
                // Rundungsfehler würden den Betrag sonst langsam verändern
                let len = self.rotation.re.hypot(self.rotation.im);
                self.rotation = C32 { re: self.rotation.re / len, im: self.rotation.im / len };
                self.since_normalize = 0;
            }
        }
        let mut input = std::mem::take(&mut self.mixed);
        for stage in &mut self.stages {
            self.stage_out.clear();
            stage.process(&input, &mut self.stage_out);
            std::mem::swap(&mut input, &mut self.stage_out);
        }
        // Phasendifferenz aufeinanderfolgender Samples = Frequenzabweichung;
        // ±1,0 entspricht ±6 kHz Hub
        let scale = AUDIO_RATE as f32 / (std::f32::consts::TAU * 6_000.0);
        for s in &input {
            let d = s.mul(C32 { re: self.last.re, im: -self.last.im });
            audio.push(d.im.atan2(d.re) * scale);
            self.last = *s;
        }
        self.mixed = input;
    }
}

/// Ein IQ-Sample (I, Q) im Bereich -1.0..1.0
pub type Iq = (f32, f32);

/// Wandelt die Ausgabe von `rtl_sdr` (abwechselnd I und Q als vorzeichenlose Bytes) um
pub fn iq_from_u8(bytes: &[u8], out: &mut Vec<Iq>) {
    out.extend(bytes.as_chunks::<2>().0.iter().map(|[i, q]| ((*i as f32 - 127.5) / 127.5, (*q as f32 - 127.5) / 127.5)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::testsignal::{self, PocsagMsg};
    use crate::decoder::{Bank, pocsag::Format};
    use crate::telegram::Kind;

    #[test]
    fn frequencies() {
        assert_eq!(parse_frequency("173.255M").unwrap(), 173_255_000.0);
        assert_eq!(parse_frequency("173255k").unwrap(), 173_255_000.0);
        assert_eq!(parse_frequency(" 173,255 MHz ").unwrap(), 173_255_000.0);
        assert_eq!(parse_frequency("173255000").unwrap(), 173_255_000.0);
        assert!(parse_frequency("abc").is_err());
        assert!(parse_frequency("").is_err());
    }

    #[test]
    fn plans() {
        // Zwei Kanäle eng beieinander: kleine Abtastrate, Mitte dazwischen
        assert_eq!(plan(&[173.255e6, 173.305e6]).unwrap(), Plan { center: 173.28e6, rate: 240_000 });
        // Ein Kanal: Mitte daneben, nicht auf dem Kanal
        let p = plan(&[173.255e6]).unwrap();
        assert!((p.center - 173.255e6).abs() >= DC_GAP && p.rate == 240_000);
        // Drei Kanäle, der mittlere läge auf der Mitte
        let p = plan(&[172.0e6, 172.4e6, 172.8e6]).unwrap();
        assert!([172.0e6, 172.4e6, 172.8e6].iter().all(|f| (f - p.center).abs() >= DC_GAP));
        assert_eq!(p.rate, 1_200_000);
        assert!(plan(&[172.0e6, 173.1e6]).is_err());
        assert!(plan(&[]).is_err());
    }

    /// FM-moduliert `audio` (mit AUDIO_RATE) auf `offset` und addiert es zu `iq`
    fn modulate(iq: &mut [Iq], audio: &[f32], offset: f64, rate: u32, deviation: f64) {
        let up = (rate / AUDIO_RATE) as usize;
        let mut phase = 0.0f64;
        for (n, s) in iq.iter_mut().enumerate() {
            let a = audio.get(n / up).copied().unwrap_or(0.0) as f64;
            phase += TAU * (offset + deviation * a) / rate as f64;
            s.0 += 0.3 * phase.cos() as f32;
            s.1 += 0.3 * phase.sin() as f32;
        }
    }

    fn decode(iq: &[Iq], offset: f64, rate: u32) -> Vec<crate::telegram::Decoded> {
        let mut channel = Channel::new(offset, rate);
        let names: Vec<String> = crate::decoder::ALL.iter().map(|s| s.to_string()).collect();
        let mut bank = Bank::new(&names, AUDIO_RATE, Format::Alpha).unwrap();
        let mut out = Vec::new();
        // In Blöcken wie vom Stick
        for block in iq.chunks(16_384) {
            let mut audio = Vec::new();
            channel.process(block, &mut audio);
            out.extend(bank.process(&audio));
        }
        out.extend(bank.finish());
        out
    }

    fn three_channels(rate: u32, offsets: [f64; 3]) {
        let mut pocsag = testsignal::pocsag(&[PocsagMsg { ric: 1_859_992, function: 3, text: "Brand 3 Musterstr", numeric: false }], 1200, AUDIO_RATE, false);
        pocsag.extend(vec![0.0; AUDIO_RATE as usize / 2]);
        let mut fms = testsignal::fms([1, 2, 3, 4, 5, 6, 7, 8], 3, 0, 1, AUDIO_RATE);
        fms.extend(vec![0.0; AUDIO_RATE as usize / 2]);
        let mut zvei = testsignal::zvei("25123", AUDIO_RATE);
        zvei.extend(vec![0.0; AUDIO_RATE as usize / 2]);
        let len = pocsag.len().max(fms.len()).max(zvei.len()) * (rate / AUDIO_RATE) as usize;
        let mut iq = vec![(0.0, 0.0); len];
        modulate(&mut iq, &pocsag, offsets[0], rate, 4_000.0);
        modulate(&mut iq, &fms, offsets[1], rate, 2_500.0);
        modulate(&mut iq, &zvei, offsets[2], rate, 2_500.0);
        let iq: Vec<Iq> = testsignal::with_noise(&iq.iter().flat_map(|s| [s.0, s.1]).collect::<Vec<_>>(), 0.2).chunks(2).map(|c| (c[0], c[1])).collect();

        let a = decode(&iq, offsets[0], rate);
        assert!(a.iter().any(|d| d.kind == Kind::Pocsag && d.address == "1859992" && d.message == "Brand 3 Musterstr"), "{a:?}");
        assert!(a.iter().all(|d| d.kind != Kind::Fms), "FMS darf nicht im POCSAG-Kanal landen: {a:?}");
        let b = decode(&iq, offsets[1], rate);
        assert!(b.iter().any(|d| d.kind == Kind::Fms && d.address == "12345678"), "{b:?}");
        assert!(b.iter().all(|d| d.kind != Kind::Pocsag), "POCSAG darf nicht im FMS-Kanal landen: {b:?}");
        let c = decode(&iq, offsets[2], rate);
        assert!(c.iter().any(|d| d.kind == Kind::Zvei && d.address == "25123"), "{c:?}");
        assert!(c.iter().all(|d| d.kind == Kind::Zvei), "Nur ZVEI im ZVEI-Kanal: {c:?}");
    }

    #[test]
    fn channels_close_together() {
        three_channels(240_000, [-25_000.0, 25_000.0, 75_000.0]);
    }

    #[test]
    fn channels_far_apart() {
        three_channels(1_200_000, [-480_000.0, 300_000.0, 512_500.0]);
    }
}
