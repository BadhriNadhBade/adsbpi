//! Mode S demodulator for 2.0 MS/s unsigned 8-bit I/Q (2 samples per 1 µs bit).
//!
//! The approach (preamble pattern match + per-bit energy comparison) follows the classic
//! dump1090 design: pulses sit at samples 0, 2, 7 and 9 of the 8 µs preamble, then each
//! data bit is "high-low" for a 1 and "low-high" for a 0.

use std::sync::OnceLock;

pub const SAMPLE_RATE: u32 = 2_000_000;
const PREAMBLE_SAMPLES: usize = 16;
const LONG_BITS: usize = 112;
const FULL_SAMPLES: usize = PREAMBLE_SAMPLES + LONG_BITS * 2;

/// magnitude table indexed by (i << 8 | q) where i,q are the raw u8 samples.
fn mag_table() -> &'static [u16] {
    static T: OnceLock<Vec<u16>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![0u16; 65536];
        for i in 0..256usize {
            for q in 0..256usize {
                let di = i as f32 - 127.4;
                let dq = q as f32 - 127.4;
                t[(i << 8) | q] = ((di * di + dq * dq).sqrt() * 360.0).round() as u16;
            }
        }
        t
    })
}

/// A raw candidate message found by the demodulator (not yet CRC-checked).
pub struct Candidate<'a> {
    pub bytes: &'a [u8; 14],
    /// Sample index (since start of stream) of the first preamble sample.
    pub sample_index: u64,
    /// Mean preamble pulse level, 0..=255.
    pub signal: u8,
}

pub struct Demodulator {
    carry: Vec<u16>,
    /// Absolute index of `carry[0]`.
    base: u64,
}

impl Default for Demodulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Demodulator {
    pub fn new() -> Self {
        mag_table();
        Self { carry: Vec::new(), base: 0 }
    }

    /// Process a chunk of raw I/Q bytes. `accept` is called for every candidate and returns
    /// true when the message was valid, in which case the scan skips past it.
    pub fn process(&mut self, iq: &[u8], mut accept: impl FnMut(Candidate<'_>) -> bool) {
        let t = mag_table();
        let mut buf = std::mem::take(&mut self.carry);
        buf.reserve(iq.len() / 2);
        buf.extend(iq.chunks_exact(2).map(|p| t[((p[0] as usize) << 8) | p[1] as usize]));

        let mut j = 0usize;
        if buf.len() > FULL_SAMPLES {
            let end = buf.len() - FULL_SAMPLES;
            while j < end {
                let m = &buf[j..j + FULL_SAMPLES];
                if !preamble_matches(m) {
                    j += 1;
                    continue;
                }
                let mut bytes = [0u8; 14];
                let mut prev = 0u8;
                for k in 0..LONG_BITS {
                    let low = m[PREAMBLE_SAMPLES + 2 * k] as i32;
                    let high = m[PREAMBLE_SAMPLES + 2 * k + 1] as i32;
                    let delta = (low - high).abs();
                    // Ambiguous bit: assume it repeats the previous one (CRC decides).
                    let bit = if k > 0 && delta < 256 { prev } else { (low > high) as u8 };
                    prev = bit;
                    bytes[k / 8] |= bit << (7 - (k % 8));
                }
                let signal = (((m[0] as u32 + m[2] as u32 + m[7] as u32 + m[9] as u32) / 4) >> 8)
                    .min(255) as u8;
                let ok = accept(Candidate {
                    bytes: &bytes,
                    sample_index: self.base + j as u64,
                    signal,
                });
                if ok {
                    let df = bytes[0] >> 3;
                    let bits = if df >= 16 { LONG_BITS } else { 56 };
                    j += PREAMBLE_SAMPLES + bits * 2;
                } else {
                    j += 1;
                }
            }
        }
        // Keep everything from `j` on (at most FULL_SAMPLES + chunk spill) for the next call.
        let keep_from = j.min(buf.len());
        self.base += keep_from as u64;
        self.carry = buf.split_off(keep_from);
    }
}

#[inline]
fn preamble_matches(m: &[u16]) -> bool {
    if !(m[0] > m[1]
        && m[1] < m[2]
        && m[2] > m[3]
        && m[3] < m[0]
        && m[4] < m[0]
        && m[5] < m[0]
        && m[6] < m[0]
        && m[7] > m[8]
        && m[8] < m[9]
        && m[9] > m[6])
    {
        return false;
    }
    let high = (m[0] as u32 + m[2] as u32 + m[7] as u32 + m[9] as u32) / 6;
    if m[4] as u32 >= high || m[5] as u32 >= high {
        return false;
    }
    if m[11] as u32 >= high || m[12] as u32 >= high || m[13] as u32 >= high || m[14] as u32 >= high {
        return false;
    }
    true
}

#[cfg(test)]
pub mod synth {
    /// Render a Mode S frame as 2 MS/s u8 I/Q: `lead` quiet samples, the frame, `tail` quiet samples.
    pub fn render(frame: &[u8], lead: usize, tail: usize, amp: i32) -> Vec<u8> {
        let mut mag = vec![0i32; lead];
        let mut pre = [0i32; 16];
        for &i in &[0usize, 2, 7, 9] {
            pre[i] = amp;
        }
        mag.extend_from_slice(&pre);
        for &byte in frame {
            for b in (0..8).rev() {
                if byte >> b & 1 == 1 {
                    mag.extend_from_slice(&[amp, 0]);
                } else {
                    mag.extend_from_slice(&[0, amp]);
                }
            }
        }
        mag.extend(std::iter::repeat(0).take(tail));
        let mut out = Vec::with_capacity(mag.len() * 2);
        for (n, m) in mag.iter().enumerate() {
            // Small deterministic "noise" so quiet samples are not exactly equal.
            let noise = ((n * 7919) % 5) as i32 - 2;
            out.push((127 + m + noise).clamp(0, 255) as u8);
            out.push(127);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::from_hex;

    fn run(demod: &mut Demodulator, iq: &[u8]) -> Vec<Vec<u8>> {
        let mut got = Vec::new();
        demod.process(iq, |c| {
            let df = c.bytes[0] >> 3;
            let len = if df >= 16 { 14 } else { 7 };
            if crate::crc::syndrome(&c.bytes[..len]) == 0 {
                got.push(c.bytes[..len].to_vec());
                true
            } else {
                false
            }
        });
        got
    }

    #[test]
    fn recovers_a_synthesised_long_frame() {
        let frame = from_hex("8D4840D6202CC371C32CE0576098");
        let iq = synth::render(&frame, 300, 300, 80);
        let got = run(&mut Demodulator::new(), &iq);
        assert_eq!(got, vec![frame]);
    }

    #[test]
    fn recovers_a_frame_split_across_chunks() {
        let frame = from_hex("8D40621D58C382D690C8AC2863A7");
        let iq = synth::render(&frame, 300, 600, 80);
        let mut d = Demodulator::new();
        let mut got = Vec::new();
        // Split mid-frame, on an even byte boundary (I/Q pairs stay intact).
        let cut = (300 + 100) * 2;
        got.extend(run(&mut d, &iq[..cut]));
        got.extend(run(&mut d, &iq[cut..]));
        assert_eq!(got, vec![frame]);
    }

    #[test]
    fn pure_noise_yields_nothing() {
        let iq: Vec<u8> = (0..20000usize).map(|n| 125 + ((n * 31 + n / 7) % 5) as u8).collect();
        assert!(run(&mut Demodulator::new(), &iq).is_empty());
    }

    #[test]
    fn two_back_to_back_frames() {
        let a = from_hex("8D4840D6202CC371C32CE0576098");
        let b = from_hex("8D40621D58C386435CC412692AD6");
        let mut iq = synth::render(&a, 200, 100, 90);
        iq.extend(synth::render(&b, 100, 300, 60));
        let got = run(&mut Demodulator::new(), &iq);
        assert_eq!(got, vec![a, b]);
    }
}
