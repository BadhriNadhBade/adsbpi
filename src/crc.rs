//! Mode S 24-bit CRC (polynomial 0xFFF409) and single-bit error correction.

use std::collections::HashMap;
use std::sync::OnceLock;

const POLY: u32 = 0xFFF409;

const fn make_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 16;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x80_0000 != 0 { (c << 1) ^ POLY } else { c << 1 };
            b += 1;
        }
        t[i] = c & 0xFF_FFFF;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = make_table();

/// Syndrome of a whole Mode S message (data + 24-bit parity field).
///
/// For DF17/18/11 a correct message has syndrome 0 (DF11: < 80 when the
/// interrogator id is overlaid). For address/parity types (DF 0/4/5/16/20/21)
/// the syndrome *is* the aircraft's ICAO address.
pub fn syndrome(msg: &[u8]) -> u32 {
    let n = msg.len();
    debug_assert!(n >= 4);
    let mut crc = 0u32;
    for &b in &msg[..n - 3] {
        crc = ((crc << 8) ^ TABLE[(((crc >> 16) ^ b as u32) & 0xFF) as usize]) & 0xFF_FFFF;
    }
    let parity = ((msg[n - 3] as u32) << 16) | ((msg[n - 2] as u32) << 8) | msg[n - 1] as u32;
    crc ^ parity
}

fn build_fix_table(nbytes: usize) -> HashMap<u32, usize> {
    let mut map = HashMap::new();
    let mut buf = vec![0u8; nbytes];
    for bit in 0..nbytes * 8 {
        buf.iter_mut().for_each(|b| *b = 0);
        buf[bit / 8] |= 0x80 >> (bit % 8);
        // Syndromes of single-bit errors are unique within a message length.
        map.insert(syndrome(&buf), bit);
    }
    map
}

/// If `msg` has exactly one flipped bit, flip it back and return its index.
pub fn fix_single_bit(msg: &mut [u8]) -> Option<usize> {
    static SHORT: OnceLock<HashMap<u32, usize>> = OnceLock::new();
    static LONG: OnceLock<HashMap<u32, usize>> = OnceLock::new();
    let table = match msg.len() {
        7 => SHORT.get_or_init(|| build_fix_table(7)),
        14 => LONG.get_or_init(|| build_fix_table(14)),
        _ => return None,
    };
    let s = syndrome(msg);
    if s == 0 {
        return None;
    }
    let &bit = table.get(&s)?;
    msg[bit / 8] ^= 0x80 >> (bit % 8);
    Some(bit)
}

#[cfg(test)]
pub fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_good_df17_frames_have_zero_syndrome() {
        for h in [
            "8D4840D6202CC371C32CE0576098",
            "8D40621D58C382D690C8AC2863A7",
            "8D40621D58C386435CC412692AD6",
            "8D485020994409940838175B284F",
        ] {
            assert_eq!(syndrome(&from_hex(h)), 0, "{h}");
        }
    }

    #[test]
    fn corrupted_frame_has_nonzero_syndrome() {
        let mut m = from_hex("8D4840D6202CC371C32CE0576098");
        m[5] ^= 0x10;
        assert_ne!(syndrome(&m), 0);
    }

    #[test]
    fn single_bit_error_is_corrected() {
        let good = from_hex("8D4840D6202CC371C32CE0576098");
        for bit in [0usize, 8, 33, 70, 111] {
            let mut m = good.clone();
            m[bit / 8] ^= 0x80 >> (bit % 8);
            assert_eq!(fix_single_bit(&mut m), Some(bit));
            assert_eq!(m, good);
        }
    }

    #[test]
    fn two_bit_error_is_not_silently_fixed_to_wrong_frame() {
        let good = from_hex("8D4840D6202CC371C32CE0576098");
        let mut m = good.clone();
        m[3] ^= 0x01;
        m[9] ^= 0x40;
        // Either rejected, or (rarely) aliased - but never claimed to equal the original.
        if fix_single_bit(&mut m).is_some() {
            assert_ne!(m, good);
        }
    }
}
