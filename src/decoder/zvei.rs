//! ZVEI-1 5-Tonfolgen-Dekoder (Töne à 70 ms, Wiederholton 2600 Hz).

use super::Decoder;
use crate::telegram::{Decoded, Kind};

/// Ziffern 0-9, Index 10 = Wiederholton
const FREQ: [f32; 11] = [
    2400.0, 1060.0, 1160.0, 1270.0, 1400.0, 1530.0, 1670.0, 1830.0, 2000.0, 2200.0, 2600.0,
];
const REPEAT: usize = 10;
/// Ein Ton muss mindestens so viele 10-ms-Blöcke stabil anliegen.
const MIN_BLOCKS: u32 = 4;
/// So viele Blöcke ohne Ton beenden eine Folge.
const END_BLOCKS: u32 = 6;

pub struct Zvei {
    block_len: usize,
    coeff: [f32; 11],
    s1: [f32; 11],
    s2: [f32; 11],
    energy: f32,
    n: usize,
    current: Option<usize>,
    run: u32,
    gap: u32,
    tones: Vec<usize>,
}

impl Zvei {
    pub fn new(sample_rate: u32) -> Zvei {
        let sr = sample_rate as f32;
        Zvei {
            block_len: (sample_rate / 100) as usize,
            coeff: FREQ.map(|f| 2.0 * (std::f32::consts::TAU * f / sr).cos()),
            s1: [0.0; 11],
            s2: [0.0; 11],
            energy: 0.0,
            n: 0,
            current: None,
            run: 0,
            gap: 0,
            tones: Vec::new(),
        }
    }

    /// Goertzel-Auswertung eines 10-ms-Blocks: Index des eindeutig dominanten Tons.
    fn block_tone(&mut self) -> Option<usize> {
        let mut power = [0f32; 11];
        for k in 0..11 {
            power[k] = self.s1[k] * self.s1[k] + self.s2[k] * self.s2[k]
                - self.coeff[k] * self.s1[k] * self.s2[k];
        }
        self.s1 = [0.0; 11];
        self.s2 = [0.0; 11];
        let energy = std::mem::take(&mut self.energy);
        if energy < 1e-6 {
            return None;
        }
        let norm = energy * self.block_len as f32 / 2.0;
        let (best, &p) = power.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
        let dominant = power.iter().enumerate().all(|(k, &q)| k == best || q < 0.1 * p);
        (p / norm > 0.5 && dominant).then_some(best)
    }

    fn on_block(&mut self, tone: Option<usize>, out: &mut Vec<Decoded>) {
        if tone.is_some() && tone == self.current {
            self.run += 1;
        } else {
            self.current = tone;
            self.run = 1;
        }
        match tone {
            Some(t) => {
                self.gap = 0;
                if self.run == MIN_BLOCKS {
                    self.tones.push(t);
                }
            }
            None => {
                self.gap += 1;
                if self.gap == END_BLOCKS {
                    self.end_sequence(out);
                }
            }
        }
    }

    fn end_sequence(&mut self, out: &mut Vec<Decoded>) {
        let tones = std::mem::take(&mut self.tones);
        if tones.len() != 5 {
            return;
        }
        let mut digits = String::new();
        let mut last = None;
        for t in tones {
            let d = if t == REPEAT { last } else { Some(t) };
            let Some(d) = d else { return };
            digits.push(char::from_digit(d as u32, 10).unwrap());
            last = Some(d);
        }
        out.push(Decoded { kind: Kind::Zvei, address: digits, sub: String::new(), message: String::new() });
    }
}

impl Decoder for Zvei {
    fn process(&mut self, samples: &[f32], out: &mut Vec<Decoded>) {
        for &x in samples {
            for k in 0..11 {
                let s = x + self.coeff[k] * self.s1[k] - self.s2[k];
                self.s2[k] = self.s1[k];
                self.s1[k] = s;
            }
            self.energy += x * x;
            self.n += 1;
            if self.n == self.block_len {
                self.n = 0;
                let tone = self.block_tone();
                self.on_block(tone, out);
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<Decoded>) {
        self.end_sequence(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::testsignal;

    fn decode(samples: &[f32], sr: u32) -> Vec<String> {
        let mut d = Zvei::new(sr);
        let mut out = Vec::new();
        d.process(samples, &mut out);
        d.finish(&mut out);
        out.into_iter().map(|d| d.address).collect()
    }

    #[test]
    fn decodes_sequences_with_repeat_tone() {
        for sr in [22050, 44100, 48000] {
            let mut s = testsignal::zvei("12345", sr);
            s.extend(testsignal::zvei("22333", sr));
            s.extend(testsignal::zvei("90807", sr));
            let got = decode(&testsignal::with_noise(&s, 0.1), sr);
            assert_eq!(got, ["12345", "22333", "90807"], "sr={sr}");
        }
    }

    #[test]
    fn ignores_incomplete_sequences_and_noise() {
        let mut s = testsignal::zvei("123", 44100);
        s.extend(testsignal::with_noise(&vec![0.0; 44100 * 10], 0.3));
        assert!(decode(&s, 44100).is_empty());
    }
}
