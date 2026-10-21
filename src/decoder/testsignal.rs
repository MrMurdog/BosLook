//! Erzeugt Testsignale für die Dekoder-Tests.

use super::pocsag::{IDLE, SYNC, bch_remainder};
use std::f32::consts::TAU;

pub fn with_noise(samples: &[f32], amplitude: f32) -> Vec<f32> {
    let mut seed: u32 = 12345;
    samples
        .iter()
        .map(|&s| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            s + amplitude * ((seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0)
        })
        .collect()
}

fn silence(sr: u32, seconds: f32) -> Vec<f32> {
    vec![0.0; (sr as f32 * seconds) as usize]
}

/// Codewort aus den oberen 21 Bit (Flag + 20 Datenbits) inkl. BCH und Parität.
pub fn pocsag_codeword(top21: u32) -> u32 {
    let cw31 = (top21 << 10) | bch_remainder(top21 << 10);
    let cw = cw31 << 1;
    cw | (cw.count_ones() % 2)
}

pub struct PocsagMsg<'a> {
    pub ric: u32,
    pub function: u32,
    pub text: &'a str,
    pub numeric: bool,
}

fn message_words(m: &PocsagMsg) -> Vec<u32> {
    let mut bits: Vec<u32> = Vec::new();
    if m.numeric {
        for c in m.text.chars() {
            let v = "0123456789*U -)(".find(c).unwrap() as u8;
            let rev = v.reverse_bits() >> 4;
            bits.extend((0..4).rev().map(|i| ((rev >> i) & 1) as u32));
        }
        while bits.len() % 20 != 0 {
            bits.extend([0, 0, 1, 1]); // Leerzeichen (0xC, bitweise umgedreht)
        }
    } else {
        for c in m.text.chars() {
            let b = match c {
                'Ä' => b'[',
                'Ö' => b'\\',
                'Ü' => b']',
                'ä' => b'{',
                'ö' => b'|',
                'ü' => b'}',
                'ß' => b'~',
                c => c as u8,
            };
            bits.extend((0..7).map(|i| ((b >> i) & 1) as u32));
        }
        while bits.len() % 20 != 0 {
            bits.push(0);
        }
    }
    bits.chunks(20)
        .map(|c| pocsag_codeword((1 << 20) | c.iter().fold(0, |a, &b| (a << 1) | b)))
        .collect()
}

pub fn pocsag(msgs: &[PocsagMsg], baud: u32, sr: u32, inverted: bool) -> Vec<f32> {
    let mut words = Vec::new();
    for m in msgs {
        let frame = (m.ric & 7) as usize;
        while words.len() % 16 / 2 != frame {
            words.push(IDLE);
        }
        words.push(pocsag_codeword(((m.ric >> 3) << 2) | m.function));
        words.extend(message_words(m));
    }
    while words.len() % 16 != 0 || words.is_empty() {
        words.push(IDLE);
    }

    let mut bits: Vec<bool> = (0..576).map(|i| i % 2 == 0).collect();
    for batch in words.chunks(16) {
        for cw in std::iter::once(&SYNC).chain(batch) {
            bits.extend((0..32).rev().map(|i| (cw >> i) & 1 == 1));
        }
    }

    let mut out = silence(sr, 0.2);
    let spb = sr as f64 / baud as f64;
    for n in 0..(bits.len() as f64 * spb) as usize {
        let bit = bits[(n as f64 / spb) as usize];
        out.push(if bit != inverted { 0.5 } else { -0.5 });
    }
    out.extend(silence(sr, 0.2));
    out
}

fn fms_crc_ok(bits: &[u32]) -> bool {
    let mut crc = [0u32; 7];
    for &b in bits {
        let d = b ^ crc[6];
        crc = [d, crc[0], crc[1] ^ d, crc[2], crc[3], crc[4], crc[5] ^ d];
    }
    crc.iter().all(|&c| c == 0)
}

/// FMS-Telegramm: `nibbles` = BOS, Land, Ort(2), Fahrzeug(4)
pub fn fms(nibbles: [u8; 8], status: u8, direction: u8, tki: u8, sr: u32) -> Vec<f32> {
    let mut data: Vec<u32> = Vec::new();
    for n in nibbles.iter().chain([status].iter()) {
        data.extend((0..4).map(|i| ((n >> i) & 1) as u32));
    }
    data.push(0); // Baustufe
    data.push(direction as u32);
    data.extend((0..2).map(|i| ((tki >> i) & 1) as u32));
    let tail = (0..256u32)
        .map(|t| (0..8).map(|i| (t >> i) & 1).collect::<Vec<_>>())
        .find(|t| fms_crc_ok(&[data.clone(), t.clone()].concat()))
        .unwrap();
    data.extend(tail);

    let mut bits = vec![1u32; 12];
    bits.extend([0, 0, 0, 1, 1, 0, 1, 0]);
    bits.extend(data);
    bits.push(1);

    let mut out = silence(sr, 0.2);
    let spb = sr as f64 / 1200.0;
    let mut phase = 0f32;
    for n in 0..(bits.len() as f64 * spb) as usize {
        let f = if bits[(n as f64 / spb) as usize] == 1 { 1200.0 } else { 1800.0 };
        phase = (phase + TAU * f / sr as f32) % TAU;
        out.push(0.5 * phase.sin());
    }
    out.extend(silence(sr, 0.2));
    out
}

pub fn zvei(code: &str, sr: u32) -> Vec<f32> {
    const FREQ: [f32; 10] = [2400.0, 1060.0, 1160.0, 1270.0, 1400.0, 1530.0, 1670.0, 1830.0, 2000.0, 2200.0];
    let mut out = silence(sr, 0.3);
    let mut last_f = 0.0;
    let mut phase = 0f32;
    for c in code.chars() {
        let mut f = FREQ[c.to_digit(10).unwrap() as usize];
        if f == last_f {
            f = 2600.0;
        }
        last_f = f;
        for _ in 0..(sr as f32 * 0.070) as usize {
            phase = (phase + TAU * f / sr as f32) % TAU;
            out.push(0.5 * phase.sin());
        }
    }
    out.extend(silence(sr, 0.3));
    out
}
