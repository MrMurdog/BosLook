//! POCSAG-Dekoder (512/1200 Baud) für das Diskriminator-Signal eines Funkempfängers.
//! Die Polarität des Signals wird anhand des Sync-Codeworts automatisch erkannt.

use super::Decoder;
use crate::telegram::{Decoded, Kind};
use serde::Deserialize;

/// Wie der Inhalt von POCSAG-Meldungen dargestellt wird.
#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Immer als Text
    #[default]
    Alpha,
    /// Immer als Ziffern
    Numeric,
    /// Sub a als Ziffern, sonst als Text
    Auto,
}

pub const SYNC: u32 = 0x7CD2_15D8;
pub const IDLE: u32 = 0x7A89_C197;
/// BCH(31,21)-Generatorpolynom x^10+x^9+x^8+x^6+x^5+x^3+1
const BCH_POLY: u32 = 0x769;

pub fn bch_remainder(cw31: u32) -> u32 {
    let mut v = cw31;
    for i in (10..31).rev() {
        if v & (1 << i) != 0 {
            v ^= BCH_POLY << (i - 10);
        }
    }
    v
}

fn is_valid(cw: u32) -> bool {
    bch_remainder(cw >> 1) == 0 && cw.count_ones() % 2 == 0
}

/// Korrigiert bis zu 2 Bitfehler, `None` wenn das Codewort nicht zu retten ist.
fn correct(cw: u32) -> Option<u32> {
    if is_valid(cw) {
        return Some(cw);
    }
    for i in 0..32 {
        let c = cw ^ (1 << i);
        if is_valid(c) {
            return Some(c);
        }
    }
    for i in 0..32 {
        for j in i + 1..32 {
            let c = cw ^ (1 << i) ^ (1 << j);
            if is_valid(c) {
                return Some(c);
            }
        }
    }
    None
}

/// Wandelt das Audiosignal in Bits um (Integrieren über eine Bitdauer, Takt per PLL).
struct BitSlicer {
    phase_inc: f32,
    phase: f32,
    acc: f32,
    lp: f32,
    lp_alpha: f32,
    dc: f32,
    dc_alpha: f32,
    last_positive: bool,
}

impl BitSlicer {
    fn new(baud: u32, sample_rate: u32) -> BitSlicer {
        let sr = sample_rate as f32;
        BitSlicer {
            phase_inc: baud as f32 / sr,
            phase: 0.0,
            acc: 0.0,
            lp: 0.0,
            lp_alpha: 1.0 - (-2.0 * std::f32::consts::PI * baud as f32 / sr).exp(),
            dc: 0.0,
            dc_alpha: 1.0 - (-2.0 * std::f32::consts::PI * 1.0 / sr).exp(),
            last_positive: false,
        }
    }

    fn push(&mut self, x: f32) -> Option<bool> {
        self.dc += self.dc_alpha * (x - self.dc);
        self.lp += self.lp_alpha * (x - self.dc - self.lp);
        let v = self.lp;

        let positive = v > 0.0;
        if positive != self.last_positive {
            // Flanken sollen auf der Bitgrenze (Phase 0) liegen
            let err = if self.phase < 0.5 { self.phase } else { self.phase - 1.0 };
            self.phase -= 0.05 * err;
            self.last_positive = positive;
        }

        self.acc += v;
        self.phase += self.phase_inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            let bit = self.acc > 0.0;
            self.acc = 0.0;
            return Some(bit);
        }
        None
    }
}

#[derive(Clone, Copy)]
enum State {
    Hunt,
    /// `index` 0..16 = Codewort im Batch, 16 = Sync-Codewort erwartet
    Synced { inverted: bool, index: u32, bits: u32 },
}

struct Message {
    address: u32,
    function: u32,
    data: Vec<u32>,
}

pub struct Pocsag {
    slicer: BitSlicer,
    shreg: u32,
    state: State,
    message: Option<Message>,
    format: Format,
}

impl Pocsag {
    pub fn new(baud: u32, sample_rate: u32, format: Format) -> Pocsag {
        Pocsag {
            slicer: BitSlicer::new(baud, sample_rate),
            shreg: 0,
            state: State::Hunt,
            message: None,
            format,
        }
    }

    fn on_bit(&mut self, bit: bool, out: &mut Vec<Decoded>) {
        self.shreg = (self.shreg << 1) | bit as u32;
        match self.state {
            State::Hunt => {
                if (self.shreg ^ SYNC).count_ones() <= 1 {
                    self.state = State::Synced { inverted: false, index: 0, bits: 0 };
                } else if (!self.shreg ^ SYNC).count_ones() <= 1 {
                    self.state = State::Synced { inverted: true, index: 0, bits: 0 };
                }
            }
            State::Synced { inverted, index, bits } => {
                if bits + 1 < 32 {
                    self.state = State::Synced { inverted, index, bits: bits + 1 };
                    return;
                }
                let cw = if inverted { !self.shreg } else { self.shreg };
                if index == 16 {
                    if (cw ^ SYNC).count_ones() <= 2 {
                        self.state = State::Synced { inverted, index: 0, bits: 0 };
                    } else {
                        self.flush(out);
                        self.state = State::Hunt;
                    }
                    return;
                }
                self.state = State::Synced { inverted, index: index + 1, bits: 0 };
                self.codeword(cw, index / 2, out);
            }
        }
    }

    fn codeword(&mut self, cw: u32, frame: u32, out: &mut Vec<Decoded>) {
        let Some(cw) = correct(cw) else {
            self.flush(out);
            return;
        };
        if cw == IDLE {
            self.flush(out);
        } else if cw & 0x8000_0000 == 0 {
            self.flush(out);
            self.message = Some(Message {
                address: (((cw >> 13) & 0x3FFFF) << 3) | frame,
                function: (cw >> 11) & 3,
                data: Vec::new(),
            });
        } else if let Some(m) = &mut self.message {
            m.data.push((cw >> 11) & 0xFFFFF);
        }
    }

    fn flush(&mut self, out: &mut Vec<Decoded>) {
        let Some(m) = self.message.take() else { return };
        let message = if m.data.is_empty() {
            String::new()
        } else if self.format == Format::Numeric || (self.format == Format::Auto && m.function == 0) {
            numeric(&m.data)
        } else {
            alpha(&m.data)
        };
        out.push(Decoded {
            kind: Kind::Pocsag,
            address: format!("{:07}", m.address),
            sub: ["a", "b", "c", "d"][m.function as usize].to_string(),
            message,
        });
    }
}

impl Decoder for Pocsag {
    fn process(&mut self, samples: &[f32], out: &mut Vec<Decoded>) {
        for &x in samples {
            if let Some(bit) = self.slicer.push(x) {
                self.on_bit(bit, out);
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<Decoded>) {
        self.flush(out);
    }
}

fn numeric(data: &[u32]) -> String {
    const CHARS: &[u8; 16] = b"0123456789*U -)(";
    let mut s = String::new();
    for &d in data {
        for n in (0..5).rev() {
            let nibble = ((d >> (n * 4)) & 0xF) as u8;
            s.push(CHARS[(nibble.reverse_bits() >> 4) as usize] as char);
        }
    }
    s.trim_end().to_string()
}

const CONTROL_NAMES: [&str; 32] = [
    "NUL", "SOH", "STX", "ETX", "EOT", "ENQ", "ACK", "BEL", "BS", "HT", "LF", "VT", "FF", "CR",
    "SO", "SI", "DLE", "DC1", "DC2", "DC3", "DC4", "NAK", "SYN", "ETB", "CAN", "EM", "SUB", "ESC",
    "FS", "GS", "RS", "US",
];

fn alpha(data: &[u32]) -> String {
    let mut chars = Vec::new();
    let (mut acc, mut nbits) = (0u32, 0);
    for &d in data {
        for i in (0..20).rev() {
            acc = (acc << 1) | ((d >> i) & 1);
            nbits += 1;
            if nbits == 7 {
                // 7-Bit-Zeichen werden LSB zuerst übertragen
                chars.push((acc as u8).reverse_bits() >> 1);
                acc = 0;
                nbits = 0;
            }
        }
    }
    while matches!(chars.last(), Some(0) | Some(4)) {
        chars.pop();
    }
    let mut s = String::new();
    for c in chars {
        match c {
            0..=31 => {
                s.push('<');
                s.push_str(CONTROL_NAMES[c as usize]);
                s.push('>');
            }
            // Deutscher Zeichensatz (DIN 66003)
            b'[' => s.push('Ä'),
            b'\\' => s.push('Ö'),
            b']' => s.push('Ü'),
            b'{' => s.push('ä'),
            b'|' => s.push('ö'),
            b'}' => s.push('ü'),
            b'~' => s.push('ß'),
            127 => s.push_str("<DEL>"),
            _ => s.push(c as char),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::testsignal;

    fn decode(samples: &[f32], baud: u32, sr: u32, format: Format) -> Vec<Decoded> {
        let mut d = Pocsag::new(baud, sr, format);
        let mut out = Vec::new();
        d.process(samples, &mut out);
        d.finish(&mut out);
        out
    }

    #[test]
    fn corrects_two_bit_errors() {
        let cw = testsignal::pocsag_codeword(0x12345 << 1);
        assert!(is_valid(cw));
        assert_eq!(correct(cw ^ 0b101 << 7), Some(cw));
        assert_eq!(correct(cw ^ 1 << 31), Some(cw));
    }

    #[test]
    fn decodes_numeric_and_alpha_both_rates_and_polarities() {
        for (baud, sr, inverted) in [(1200, 44100, false), (1200, 22050, true), (512, 44100, true), (512, 48000, false)] {
            let msgs = [
                testsignal::PocsagMsg { ric: 1859992, function: 0, text: "80404", numeric: true },
                testsignal::PocsagMsg { ric: 1234567, function: 3, text: "Brand 3 Göppingen, Hauptstr. 12", numeric: false },
                testsignal::PocsagMsg { ric: 8, function: 1, text: "", numeric: false },
            ];
            let samples = testsignal::with_noise(&testsignal::pocsag(&msgs, baud, sr, inverted), 0.1);
            let got = decode(&samples, baud, sr, Format::Auto);
            let want: Vec<(String, String, String)> = vec![
                ("1859992".into(), "a".into(), "80404".into()),
                ("1234567".into(), "d".into(), "Brand 3 Göppingen, Hauptstr. 12".into()),
                ("0000008".into(), "b".into(), "".into()),
            ];
            let got: Vec<_> = got.into_iter().map(|d| (d.address, d.sub, d.message)).collect();
            assert_eq!(got, want, "baud={baud} sr={sr} inverted={inverted}");
        }
    }

    #[test]
    fn silence_and_noise_give_nothing() {
        let samples = testsignal::with_noise(&vec![0.0; 44100 * 5], 0.3);
        assert!(decode(&samples, 1200, 44100, Format::Alpha).is_empty());
    }

    #[test]
    fn format_selects_alpha_or_numeric() {
        let msgs = [
            testsignal::PocsagMsg { ric: 1859992, function: 0, text: "Brand", numeric: false },
            testsignal::PocsagMsg { ric: 1859993, function: 2, text: "12345", numeric: true },
        ];
        let samples = testsignal::pocsag(&msgs, 1200, 22050, false);
        let texts = |format| decode(&samples, 1200, 22050, format).into_iter().map(|d| d.message).collect::<Vec<_>>();
        assert_eq!(texts(Format::Alpha)[0], "Brand");
        assert_eq!(texts(Format::Numeric)[1], "12345");
        assert_eq!(texts(Format::Auto)[1], texts(Format::Alpha)[1]);
        assert_ne!(texts(Format::Auto)[0], "Brand");
    }
}
