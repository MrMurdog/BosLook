//! FMS-Dekoder (FSK 1200 Baud, "1" = 1200 Hz, "0" = 1800 Hz) nach TR-BOS FMS.

use super::Decoder;
use crate::telegram::{Decoded, Kind};

const SYNC: u32 = 0x7FF1A; // 11 Bit Vorlauf + Blocksync "0001" + "1010"
const SYNC_MASK: u32 = 0x7FFFF;
/// Mindestanteil der stärkeren FSK-Frequenz an der Signalenergie, damit ein Bit als sauber gilt.
const MIN_QUALITY: f64 = 0.3;

pub struct Fms {
    len: usize,
    pos: usize,
    /// je Frequenz: x[n]·e^(jωn) der letzten `len` Samples, dazu x²
    ring: Vec<[(f64, f64); 2]>,
    ring_energy: Vec<f64>,
    sums: [(f64, f64); 2],
    energy: f64,
    osc: [(f64, f64); 2],
    rot: [(f64, f64); 2],
    samples_since_renorm: u32,
    phase: f64,
    phase_inc: f64,
    last_bit: bool,
    rxstate: u32,
    telegram: Option<Vec<u32>>,
    bad_bits: u32,
}

impl Fms {
    pub fn new(sample_rate: u32) -> Fms {
        let sr = sample_rate as f64;
        let len = (sr / 1200.0).round() as usize;
        let rot = |f: f64| {
            let w = std::f64::consts::TAU * f / sr;
            (w.cos(), w.sin())
        };
        Fms {
            len,
            pos: 0,
            ring: vec![[(0.0, 0.0); 2]; len],
            ring_energy: vec![0.0; len],
            sums: [(0.0, 0.0); 2],
            energy: 0.0,
            osc: [(1.0, 0.0); 2],
            rot: [rot(1200.0), rot(1800.0)],
            samples_since_renorm: 0,
            phase: 0.0,
            phase_inc: 1200.0 / sr,
            last_bit: false,
            rxstate: 0,
            telegram: None,
            bad_bits: 0,
        }
    }

    /// Gleitende DFT über eine Bitdauer: liefert (Bit, Qualität 0..1).
    fn correlate(&mut self, x: f64) -> (bool, f64) {
        for k in 0..2 {
            let (c, s) = self.osc[k];
            let v = (x * c, x * s);
            let old = self.ring[self.pos][k];
            self.sums[k].0 += v.0 - old.0;
            self.sums[k].1 += v.1 - old.1;
            self.ring[self.pos][k] = v;
            let (rc, rs) = self.rot[k];
            self.osc[k] = (c * rc - s * rs, c * rs + s * rc);
        }
        self.energy += x * x - self.ring_energy[self.pos];
        self.ring_energy[self.pos] = x * x;
        self.pos = (self.pos + 1) % self.len;

        self.samples_since_renorm += 1;
        if self.samples_since_renorm >= 1 << 16 {
            self.renormalize();
        }

        let e1 = self.sums[0].0.powi(2) + self.sums[0].1.powi(2);
        let e0 = self.sums[1].0.powi(2) + self.sums[1].1.powi(2);
        let quality = if self.energy > 1e-9 {
            e1.max(e0) / (self.energy * self.len as f64 / 2.0)
        } else {
            0.0
        };
        (e1 > e0, quality)
    }

    /// Gegen Rundungsfehler: Oszillatoren normieren und Summen neu berechnen.
    fn renormalize(&mut self) {
        self.samples_since_renorm = 0;
        for k in 0..2 {
            let (c, s) = self.osc[k];
            let m = (c * c + s * s).sqrt();
            self.osc[k] = (c / m, s / m);
            self.sums[k] = self.ring.iter().fold((0.0, 0.0), |a, v| (a.0 + v[k].0, a.1 + v[k].1));
        }
        self.energy = self.ring_energy.iter().sum();
    }

    fn on_bit(&mut self, bit: bool, quality: f64, out: &mut Vec<Decoded>) {
        let bit = bit as u32;
        self.rxstate = ((self.rxstate << 1) | bit) & SYNC_MASK;
        match &mut self.telegram {
            None => {
                if self.rxstate == SYNC {
                    self.telegram = Some(Vec::with_capacity(48));
                    self.bad_bits = 0;
                }
            }
            Some(bits) => {
                bits.push(bit);
                if quality < MIN_QUALITY {
                    self.bad_bits += 1;
                }
                if bits.len() == 48 {
                    let bits = self.telegram.take().unwrap();
                    self.rxstate = 0;
                    if self.bad_bits <= 2 {
                        out.extend(decode(bits));
                    }
                }
            }
        }
    }
}

impl Decoder for Fms {
    fn process(&mut self, samples: &[f32], out: &mut Vec<Decoded>) {
        for &x in samples {
            let (bit, quality) = self.correlate(x as f64);
            if bit != self.last_bit {
                // Bitwechsel liegen bei Phase 0.5, abgetastet wird am Bitende (Phase 1.0)
                self.phase -= 0.3 * (self.phase - 0.5);
                self.last_bit = bit;
            }
            self.phase += self.phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                self.on_bit(bit, quality, out);
            }
        }
    }
}

fn crc_ok(bits: &[u32]) -> bool {
    let mut crc = [0u32; 7];
    for &b in bits {
        let d = b ^ crc[6];
        crc = [d, crc[0], crc[1] ^ d, crc[2], crc[3], crc[4], crc[5] ^ d];
    }
    crc.iter().all(|&c| c == 0)
}

fn decode(mut bits: Vec<u32>) -> Option<Decoded> {
    if !crc_ok(&bits) {
        let i = (0..48).find(|&i| {
            bits[i] ^= 1;
            let ok = crc_ok(&bits);
            bits[i] ^= 1;
            ok
        })?;
        bits[i] ^= 1;
    }
    let nibble = |i: usize| (0..4).fold(0, |a, b| a | (bits[i * 4 + b] << b));
    let kennung: String = (0..8).map(|i| format!("{:X}", nibble(i))).collect();
    let status = nibble(8) as usize;
    let to_vehicle = bits[37] == 1;
    let tki = bits[38] | (bits[39] << 1);
    let text = if to_vehicle { STATUS_LST_FZG[status] } else { STATUS_FZG_LST[status] };
    let dir = if to_vehicle { "LST→FZG" } else { "FZG→LST" };
    Some(Decoded {
        kind: Kind::Fms,
        address: kennung,
        sub: format!("{status:X}"),
        message: format!("{text} ({dir}, TKI {})", tki + 1),
    })
}

const STATUS_FZG_LST: [&str; 16] = [
    "Notruf / priorisierter Sprechwunsch",
    "Einsatzbereit über Funk",
    "Einsatzbereit auf Wache",
    "Einsatz übernommen",
    "Am Einsatzort",
    "Sprechwunsch",
    "Nicht einsatzbereit",
    "Patient aufgenommen",
    "Am Transportziel",
    "Fremdanmeldung / Handquittung",
    "Vorbereitung Folgetelegramm",
    "Beendigung Folgetelegramm",
    "Sonderstatus 1",
    "Sonderstatus 2",
    "Automatische Quittung",
    "Sprechtaste",
];

const STATUS_LST_FZG: [&str; 16] = [
    "Statusabfrage",
    "Sammelruf",
    "Einrücken / Einsatz abgebrochen",
    "Melden für Einsatzübernahme",
    "Über Draht melden",
    "Wache anfahren",
    "Sprechaufforderung",
    "Lagemeldung",
    "Fernwirken 1",
    "Fernwirken 2",
    "Vorbereitung Textübertragung",
    "Beendigung Textübertragung",
    "Kurztext C",
    "Kurztext D",
    "Kurztext E",
    "Automatische Quittung",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::testsignal;

    fn decode_samples(samples: &[f32], sr: u32) -> Vec<Decoded> {
        let mut d = Fms::new(sr);
        let mut out = Vec::new();
        d.process(samples, &mut out);
        out
    }

    #[test]
    fn decodes_status_telegram() {
        for sr in [22050, 44100, 48000] {
            let mut s = testsignal::fms([6, 2, 6, 3, 1, 2, 3, 4], 3, 0, 1, sr);
            s.extend(testsignal::fms([9, 7, 1, 2, 5, 6, 7, 8], 2, 1, 0, sr));
            let got = decode_samples(&testsignal::with_noise(&s, 0.1), sr);
            assert_eq!(got.len(), 2, "sr={sr}: {got:?}");
            assert_eq!(got[0].address, "62631234");
            assert_eq!(got[0].sub, "3");
            assert_eq!(got[0].message, "Einsatz übernommen (FZG→LST, TKI 2)");
            assert_eq!(got[1].address, "97125678");
            assert_eq!(got[1].message, "Einrücken / Einsatz abgebrochen (LST→FZG, TKI 1)");
        }
    }

    #[test]
    fn noise_gives_nothing() {
        let samples = testsignal::with_noise(&vec![0.0; 44100 * 30], 0.3);
        assert!(decode_samples(&samples, 44100).is_empty());
    }
}
