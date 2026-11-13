//! Mode S / ADS-B message validation and field decoding.

use crate::crc;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A validated Mode S frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub df: u8,
    pub len: usize,
    pub bytes: [u8; 14],
    pub icao: u32,
    pub corrected: bool,
}

impl Frame {
    pub fn raw(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Compact position report (CPR), airborne.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cpr {
    pub odd: bool,
    pub lat: u32,
    pub lon: u32,
}

/// What a frame told us about an aircraft.
#[derive(Debug, Clone, Default)]
pub struct Decoded {
    pub ident: Option<String>,
    pub altitude_ft: Option<i32>,
    pub squawk: Option<u16>,
    pub speed_kt: Option<f64>,
    pub track_deg: Option<f64>,
    pub vrate_fpm: Option<i32>,
    pub cpr: Option<Cpr>,
}

/// Remembers ICAO addresses recently seen in frames that carry the address in the clear
/// (DF11/17/18). Address/parity frames (DF0/4/5/16/20/21) are only accepted for these.
pub struct IcaoCache {
    seen: HashMap<u32, Instant>,
    ttl: Duration,
    last_gc: Instant,
}

impl IcaoCache {
    pub fn new(ttl: Duration) -> Self {
        Self { seen: HashMap::new(), ttl, last_gc: Instant::now() }
    }
    fn add(&mut self, icao: u32, now: Instant) {
        self.seen.insert(icao, now);
        if now.duration_since(self.last_gc) > self.ttl {
            let ttl = self.ttl;
            self.seen.retain(|_, t| now.duration_since(*t) <= ttl);
            self.last_gc = now;
        }
    }
    fn contains(&self, icao: u32, now: Instant) -> bool {
        self.seen.get(&icao).is_some_and(|t| now.duration_since(*t) <= self.ttl)
    }
}

pub fn df_len_bytes(df: u8) -> usize {
    if df >= 16 { 14 } else { 7 }
}

/// Validate a raw candidate (7 or 14 bytes, length implied by its DF) and recover its address.
pub fn validate(raw: &[u8], cache: &mut IcaoCache, now: Instant) -> Option<Frame> {
    let df = raw[0] >> 3;
    let len = df_len_bytes(df);
    if raw.len() < len {
        return None;
    }
    let mut bytes = [0u8; 14];
    bytes[..len].copy_from_slice(&raw[..len]);
    let mut corrected = false;

    match df {
        17 | 18 => {
            if crc::syndrome(&bytes[..len]) != 0 {
                crc::fix_single_bit(&mut bytes[..len])?;
                // A corrected bit must not have changed the format into something else.
                if bytes[0] >> 3 != df {
                    return None;
                }
                corrected = true;
            }
            let icao = addr(&bytes);
            cache.add(icao, now);
            Some(Frame { df, len, bytes, icao, corrected })
        }
        11 => {
            let s = crc::syndrome(&bytes[..len]);
            // Low 7 bits may carry the interrogator id: remainder < 80 is valid.
            if s != 0 && s >= 80 {
                return None;
            }
            let icao = addr(&bytes);
            cache.add(icao, now);
            Some(Frame { df, len, bytes, icao, corrected })
        }
        0 | 4 | 5 | 16 | 20 | 21 => {
            let icao = crc::syndrome(&bytes[..len]) & 0xFF_FFFF;
            if cache.contains(icao, now) {
                Some(Frame { df, len, bytes, icao, corrected })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn addr(b: &[u8; 14]) -> u32 {
    ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | b[3] as u32
}

const CHARSET: &[u8; 64] = b"?ABCDEFGHIJKLMNOPQRSTUVWXYZ????? ???????????????0123456789??????";

/// 13-bit altitude (DF0/4/16/20); None when metric or Gillham-coded.
fn decode_ac13(ac13: u32) -> Option<i32> {
    let m = ac13 & 0x0040;
    let q = ac13 & 0x0010;
    if m != 0 || q == 0 {
        return None;
    }
    let n = ((ac13 & 0x1F80) >> 2) | ((ac13 & 0x0020) >> 1) | (ac13 & 0x000F);
    Some(n as i32 * 25 - 1000)
}

/// 12-bit altitude in airborne position messages (Q-bit encoding only).
fn decode_ac12(ac12: u32) -> Option<i32> {
    if ac12 & 0x10 == 0 {
        return None;
    }
    let n = ((ac12 & 0x0FE0) >> 1) | (ac12 & 0x000F);
    Some(n as i32 * 25 - 1000)
}

/// 13-bit identity field to a squawk expressed as four octal digits read as decimal (e.g. 1200).
fn decode_squawk(id13: u32) -> u16 {
    let bit = |n: u32| (id13 >> n) & 1;
    let a = bit(11) | (bit(9) << 1) | (bit(7) << 2); // A1 A2 A4
    let b = bit(5) | (bit(3) << 1) | (bit(1) << 2); // B1 B2 B4
    let c = bit(12) | (bit(10) << 1) | (bit(8) << 2); // C1 C2 C4
    let d = bit(4) | (bit(2) << 1) | bit(0) << 2; // D1 D2 D4
    (a * 1000 + b * 100 + c * 10 + d) as u16
}

pub fn decode(f: &Frame) -> Decoded {
    let b = &f.bytes;
    let mut d = Decoded::default();
    match f.df {
        0 | 4 | 16 | 20 => {
            let ac13 = (((b[2] as u32) << 8) | b[3] as u32) & 0x1FFF;
            d.altitude_ft = decode_ac13(ac13);
        }
        5 | 21 => {
            let id13 = (((b[2] as u32) << 8) | b[3] as u32) & 0x1FFF;
            d.squawk = Some(decode_squawk(id13));
        }
        17 | 18 => decode_extended(&b[4..11], &mut d),
        _ => {}
    }
    if matches!(f.df, 20 | 21) {
        // Comm-B altitude/identity still fine; BDS payload is not decoded.
    }
    d
}

fn decode_extended(me: &[u8], d: &mut Decoded) {
    let tc = me[0] >> 3;
    match tc {
        1..=4 => {
            let c = [
                me[1] >> 2,
                ((me[1] & 3) << 4) | (me[2] >> 4),
                ((me[2] & 15) << 2) | (me[3] >> 6),
                me[3] & 63,
                me[4] >> 2,
                ((me[4] & 3) << 4) | (me[5] >> 4),
                ((me[5] & 15) << 2) | (me[6] >> 6),
                me[6] & 63,
            ];
            let s: String = c.iter().map(|&i| CHARSET[i as usize] as char).collect();
            let s = s.trim_end_matches([' ', '?']).to_string();
            if !s.is_empty() {
                d.ident = Some(s);
            }
        }
        9..=18 | 20..=22 => {
            if (9..=18).contains(&tc) {
                let ac12 = ((me[1] as u32) << 4) | (me[2] as u32 >> 4);
                d.altitude_ft = decode_ac12(ac12);
            }
            d.cpr = Some(Cpr {
                odd: me[2] & 0x04 != 0,
                lat: (((me[2] & 3) as u32) << 15) | ((me[3] as u32) << 7) | (me[4] as u32 >> 1),
                lon: (((me[4] & 1) as u32) << 16) | ((me[5] as u32) << 8) | me[6] as u32,
            });
        }
        19 => {
            let st = me[0] & 7;
            if st == 1 || st == 2 {
                let mult = if st == 2 { 4.0 } else { 1.0 };
                let ew_raw = (((me[1] & 3) as i32) << 8) | me[2] as i32;
                let ns_raw = (((me[3] & 0x7F) as i32) << 3) | (me[4] as i32 >> 5);
                if ew_raw != 0 && ns_raw != 0 {
                    let ew = (ew_raw - 1) as f64 * mult * if me[1] & 4 != 0 { -1.0 } else { 1.0 };
                    let ns = (ns_raw - 1) as f64 * mult * if me[3] & 0x80 != 0 { -1.0 } else { 1.0 };
                    d.speed_kt = Some(ew.hypot(ns));
                    let mut trk = ew.atan2(ns).to_degrees();
                    if trk < 0.0 {
                        trk += 360.0;
                    }
                    d.track_deg = Some(trk);
                }
                let vr_raw = (((me[4] & 7) as i32) << 6) | (me[5] as i32 >> 2);
                if vr_raw != 0 {
                    let vr = (vr_raw - 1) * 64;
                    d.vrate_fpm = Some(if me[4] & 0x08 != 0 { -vr } else { vr });
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::from_hex;

    fn frame(h: &str) -> Frame {
        let raw = from_hex(h);
        let mut cache = IcaoCache::new(Duration::from_secs(60));
        validate(&raw, &mut cache, Instant::now()).expect("valid")
    }

    #[test]
    fn identification() {
        let f = frame("8D4840D6202CC371C32CE0576098");
        assert_eq!(f.icao, 0x4840D6);
        assert_eq!(decode(&f).ident.as_deref(), Some("KLM1023"));
    }

    #[test]
    fn airborne_position_altitude_and_cpr() {
        let f = frame("8D40621D58C382D690C8AC2863A7");
        let d = decode(&f);
        assert_eq!(f.icao, 0x40621D);
        assert_eq!(d.altitude_ft, Some(38000));
        let c = d.cpr.unwrap();
        assert!(!c.odd);
        assert_eq!((c.lat, c.lon), (93000, 51372));
    }

    #[test]
    fn velocity() {
        let d = decode(&frame("8D485020994409940838175B284F"));
        assert_eq!(d.speed_kt.unwrap().round(), 159.0);
        assert!((d.track_deg.unwrap() - 182.88).abs() < 0.01);
        assert_eq!(d.vrate_fpm, Some(-832));
    }

    #[test]
    fn single_bit_error_in_df17_is_repaired() {
        let mut raw = from_hex("8D4840D6202CC371C32CE0576098");
        raw[6] ^= 0x04;
        let mut cache = IcaoCache::new(Duration::from_secs(60));
        let f = validate(&raw, &mut cache, Instant::now()).unwrap();
        assert!(f.corrected);
        assert_eq!(decode(&f).ident.as_deref(), Some("KLM1023"));
    }

    #[test]
    fn address_parity_frames_need_a_known_address() {
        // Build a DF5 (squawk) frame whose AP field encodes ICAO 4840D6.
        let mut raw = vec![0x28u8, 0x00, 0x1A, 0x38, 0, 0, 0];
        let s = crc::syndrome(&raw); // syndrome with zero parity field
        let ap = s ^ 0x4840D6;
        raw[4] = (ap >> 16) as u8;
        raw[5] = (ap >> 8) as u8;
        raw[6] = ap as u8;
        let mut cache = IcaoCache::new(Duration::from_secs(60));
        let now = Instant::now();
        assert!(validate(&raw, &mut cache, now).is_none());
        validate(&from_hex("8D4840D6202CC371C32CE0576098"), &mut cache, now).unwrap();
        let f = validate(&raw, &mut cache, now).expect("known address accepted");
        assert_eq!(f.icao, 0x4840D6);
        assert!(decode(&f).squawk.is_some());
    }

    #[test]
    fn squawk_decoding() {
        // 1200 = A1 B0? octal digits 1,2,0,0 -> A=1,B=2,C=0,D=0
        // A1 is id13 bit 11, B2 is bit 3.
        let id13 = (1 << 11) | (1 << 3);
        assert_eq!(decode_squawk(id13), 1200);
    }
}
