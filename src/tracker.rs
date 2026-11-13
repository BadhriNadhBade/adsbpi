//! Aircraft table: merges decoded messages, resolves CPR positions, expires stale aircraft.

use crate::modes::{Cpr, Decoded};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone)]
pub struct Aircraft {
    pub icao: u32,
    pub flight: Option<String>,
    pub altitude_ft: Option<i32>,
    pub squawk: Option<u16>,
    pub speed_kt: Option<f64>,
    pub track_deg: Option<f64>,
    pub vrate_fpm: Option<i32>,
    pub pos: Option<Position>,
    pub pos_time: Option<Instant>,
    pub messages: u64,
    pub rssi: u8,
    pub last_seen: Instant,
    even: Option<(Cpr, Instant)>,
    odd: Option<(Cpr, Instant)>,
}

impl Aircraft {
    fn new(icao: u32, now: Instant) -> Self {
        Self {
            icao,
            flight: None,
            altitude_ft: None,
            squawk: None,
            speed_kt: None,
            track_deg: None,
            vrate_fpm: None,
            pos: None,
            pos_time: None,
            messages: 0,
            rssi: 0,
            last_seen: now,
            even: None,
            odd: None,
        }
    }
}

/// What changed in the aircraft table because of one frame (used for SBS output).
#[derive(Debug, Default, Clone)]
pub struct Update {
    pub new_position: Option<Position>,
    /// Best known altitude of the aircraft right after this update.
    pub altitude_ft: Option<i32>,
}

pub struct Tracker {
    pub aircraft: HashMap<u32, Aircraft>,
    pub home: Option<Position>,
    pub max_range_km: f64,
    pub ttl: Duration,
    pub total_messages: u64,
    pub total_positions: u64,
    pub rejected_positions: u64,
}

const CPR_PAIR_MAX_AGE: Duration = Duration::from_secs(10);
const LOCAL_DECODE_MAX_AGE: Duration = Duration::from_secs(60);

impl Tracker {
    pub fn new(home: Option<Position>, max_range_km: f64, ttl: Duration) -> Self {
        Self {
            aircraft: HashMap::new(),
            home,
            max_range_km,
            ttl,
            total_messages: 0,
            total_positions: 0,
            rejected_positions: 0,
        }
    }

    pub fn update(&mut self, icao: u32, d: &Decoded, rssi: u8, now: Instant) -> Update {
        self.total_messages += 1;
        let home = self.home;
        let max_range = self.max_range_km;
        let ac = self.aircraft.entry(icao).or_insert_with(|| Aircraft::new(icao, now));
        ac.messages += 1;
        ac.last_seen = now;
        ac.rssi = rssi;
        if let Some(v) = &d.ident {
            ac.flight = Some(v.clone());
        }
        if let Some(v) = d.altitude_ft {
            ac.altitude_ft = Some(v);
        }
        if let Some(v) = d.squawk {
            ac.squawk = Some(v);
        }
        if let Some(v) = d.speed_kt {
            ac.speed_kt = Some(v);
        }
        if let Some(v) = d.track_deg {
            ac.track_deg = Some(v);
        }
        if let Some(v) = d.vrate_fpm {
            ac.vrate_fpm = Some(v);
        }

        let mut out = Update::default();
        if let Some(cpr) = d.cpr {
            if cpr.odd {
                ac.odd = Some((cpr, now));
            } else {
                ac.even = Some((cpr, now));
            }
            let resolved = resolve(ac, cpr, now);
            match resolved {
                Some(p) if plausible(ac, p, home, max_range, now) => {
                    ac.pos = Some(p);
                    ac.pos_time = Some(now);
                    out.new_position = Some(p);
                    out.altitude_ft = ac.altitude_ft;
                    self.total_positions += 1;
                }
                Some(_) => self.rejected_positions += 1,
                None => {}
            }
        }
        out
    }

    pub fn expire(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.aircraft.retain(|_, a| now.duration_since(a.last_seen) <= ttl);
    }
}

fn plausible(ac: &Aircraft, p: Position, home: Option<Position>, max_range: f64, now: Instant) -> bool {
    if let Some(h) = home {
        if distance_km(h, p) > max_range {
            return false;
        }
    }
    if let (Some(prev), Some(t)) = (ac.pos, ac.pos_time) {
        let dt = now.duration_since(t).as_secs_f64();
        // 1,300 km/h upper bound plus slack for CPR resolution.
        if distance_km(prev, p) > 5.0 + 0.36 * dt {
            return false;
        }
    }
    true
}

fn resolve(ac: &Aircraft, newest: Cpr, now: Instant) -> Option<Position> {
    if let (Some((e, te)), Some((o, to))) = (ac.even, ac.odd) {
        let gap = if te > to { te - to } else { to - te };
        if gap <= CPR_PAIR_MAX_AGE && now.duration_since(te.min(to)) <= CPR_PAIR_MAX_AGE * 2 {
            if let Some(p) = global_decode(e, o, newest.odd) {
                return Some(p);
            }
        }
    }
    // A single message can only be resolved relative to a nearby reference. Use the aircraft's
    // own recent position. The receiver's home is deliberately NOT used: for an aircraft beyond
    // ~330 km, decoding against home always lands on a plausible-looking but wrong position.
    let (prev, t) = (ac.pos?, ac.pos_time?);
    if now.duration_since(t) > LOCAL_DECODE_MAX_AGE {
        return None;
    }
    Some(local_decode(newest, prev))
}

const NZ: f64 = 15.0;

fn cpr_nl(lat: f64) -> f64 {
    let lat = lat.abs();
    if lat < 1e-9 {
        return 59.0;
    }
    if (lat - 87.0).abs() < 1e-9 {
        return 2.0;
    }
    if lat > 87.0 {
        return 1.0;
    }
    let a = 1.0 - (std::f64::consts::PI / (2.0 * NZ)).cos();
    let b = (std::f64::consts::PI / 180.0 * lat).cos().powi(2);
    (2.0 * std::f64::consts::PI / (1.0 - a / b).acos()).floor()
}

fn modulo(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r < 0.0 { r + b } else { r }
}

pub fn global_decode(even: Cpr, odd: Cpr, newest_is_odd: bool) -> Option<Position> {
    const SCALE: f64 = 131072.0;
    let (lat0, lon0) = (even.lat as f64, even.lon as f64);
    let (lat1, lon1) = (odd.lat as f64, odd.lon as f64);
    let dlat0 = 360.0 / 60.0;
    let dlat1 = 360.0 / 59.0;
    let j = ((59.0 * lat0 - 60.0 * lat1) / SCALE + 0.5).floor();
    let mut rlat0 = dlat0 * (modulo(j, 60.0) + lat0 / SCALE);
    let mut rlat1 = dlat1 * (modulo(j, 59.0) + lat1 / SCALE);
    if rlat0 >= 270.0 {
        rlat0 -= 360.0;
    }
    if rlat1 >= 270.0 {
        rlat1 -= 360.0;
    }
    if !(-90.0..=90.0).contains(&rlat0) || !(-90.0..=90.0).contains(&rlat1) {
        return None;
    }
    let nl0 = cpr_nl(rlat0);
    if nl0 != cpr_nl(rlat1) {
        return None;
    }
    let (lat, mut lon) = if !newest_is_odd {
        let ni = nl0.max(1.0);
        let m = ((lon0 * (nl0 - 1.0) - lon1 * nl0) / SCALE + 0.5).floor();
        (rlat0, (360.0 / ni) * (modulo(m, ni) + lon0 / SCALE))
    } else {
        let ni = (nl0 - 1.0).max(1.0);
        let m = ((lon0 * (nl0 - 1.0) - lon1 * nl0) / SCALE + 0.5).floor();
        (rlat1, (360.0 / ni) * (modulo(m, ni) + lon1 / SCALE))
    };
    if lon > 180.0 {
        lon -= 360.0;
    }
    Some(Position { lat, lon })
}

pub fn local_decode(c: Cpr, reference: Position) -> Position {
    const SCALE: f64 = 131072.0;
    let odd = if c.odd { 1.0 } else { 0.0 };
    let dlat = 360.0 / (60.0 - odd);
    let lat_c = c.lat as f64 / SCALE;
    let j = (reference.lat / dlat).floor() + (0.5 + modulo(reference.lat, dlat) / dlat - lat_c).floor();
    let lat = dlat * (j + lat_c);
    let ni = (cpr_nl(lat) - odd).max(1.0);
    let dlon = 360.0 / ni;
    let lon_c = c.lon as f64 / SCALE;
    let m = (reference.lon / dlon).floor() + (0.5 + modulo(reference.lon, dlon) / dlon - lon_c).floor();
    Position { lat, lon: dlon * (m + lon_c) }
}

/// Great-circle distance in kilometres (haversine).
pub fn distance_km(a: Position, b: Position) -> f64 {
    let (la1, la2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = la2 - la1;
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVEN: Cpr = Cpr { odd: false, lat: 93000, lon: 51372 };
    const ODD: Cpr = Cpr { odd: true, lat: 74158, lon: 50194 };

    #[test]
    fn global_decode_matches_published_example() {
        // Even/odd pair from "The 1090 MHz Riddle": 52.25720, 3.91937 (odd newest -> 52.2657, 3.9389).
        let p = global_decode(EVEN, ODD, false).unwrap();
        assert!((p.lat - 52.25720).abs() < 1e-4, "{p:?}");
        assert!((p.lon - 3.91937).abs() < 1e-4, "{p:?}");
        let q = global_decode(EVEN, ODD, true).unwrap();
        assert!((q.lat - 52.26578).abs() < 1e-4, "{q:?}");
        assert!((q.lon - 3.93891).abs() < 1e-4, "{q:?}");
    }

    #[test]
    fn local_decode_agrees_with_global() {
        let reference = Position { lat: 52.0, lon: 4.0 };
        let p = local_decode(EVEN, reference);
        assert!((p.lat - 52.25720).abs() < 1e-4, "{p:?}");
        assert!((p.lon - 3.91937).abs() < 1e-4, "{p:?}");
        let q = local_decode(ODD, reference);
        assert!((q.lat - 52.26578).abs() < 1e-4, "{q:?}");
    }

    #[test]
    fn nl_values() {
        assert_eq!(cpr_nl(0.0), 59.0);
        assert_eq!(cpr_nl(52.2572), 36.0);
        assert_eq!(cpr_nl(88.0), 1.0);
    }

    #[test]
    fn unset_home_does_not_reject_positions() {
        // Regression for the Windows dump1090 bug: home = (-1,-1) rejected every position.
        let mut t = Tracker::new(None, 500.0, Duration::from_secs(60));
        let now = Instant::now();
        let mut d = Decoded::default();
        d.cpr = Some(EVEN);
        t.update(0x40621D, &d, 100, now);
        d.cpr = Some(ODD);
        let u = t.update(0x40621D, &d, 100, now + Duration::from_secs(1));
        assert!(u.new_position.is_some());
    }

    #[test]
    fn far_from_home_positions_are_rejected() {
        let home = Position { lat: 32.9618, lon: -96.8292 }; // Addison, TX
        let mut t = Tracker::new(Some(home), 500.0, Duration::from_secs(60));
        let now = Instant::now();
        let mut d = Decoded::default();
        d.cpr = Some(EVEN);
        t.update(1, &d, 100, now);
        d.cpr = Some(ODD);
        let u = t.update(1, &d, 100, now + Duration::from_secs(1));
        assert!(u.new_position.is_none()); // Netherlands is not within 500 km of Texas
        assert_eq!(t.rejected_positions, 1);
    }

    #[test]
    fn single_message_from_far_aircraft_is_not_placed_near_home() {
        // Regression: decoding one CPR frame against the receiver position puts any aircraft
        // beyond ~330 km on a bogus spot near the receiver.
        let home = Position { lat: 32.9618, lon: -96.8292 };
        let mut t = Tracker::new(Some(home), 500.0, Duration::from_secs(60));
        let mut d = Decoded::default();
        d.cpr = Some(EVEN);
        let u = t.update(1, &d, 100, Instant::now());
        assert!(u.new_position.is_none());
        assert_eq!(t.total_positions, 0);
    }

    #[test]
    fn follow_up_message_uses_the_aircrafts_own_last_position() {
        let mut t = Tracker::new(None, 500.0, Duration::from_secs(60));
        let now = Instant::now();
        let mut d = Decoded::default();
        d.cpr = Some(EVEN);
        t.update(1, &d, 100, now);
        d.cpr = Some(ODD);
        t.update(1, &d, 100, now + Duration::from_secs(1));
        d.cpr = Some(EVEN);
        let u = t.update(1, &d, 100, now + Duration::from_secs(2));
        let p = u.new_position.expect("single even frame resolved locally");
        assert!((p.lat - 52.25720).abs() < 1e-3, "{p:?}");
    }

    #[test]
    fn aircraft_expire() {
        let mut t = Tracker::new(None, 500.0, Duration::from_secs(60));
        let now = Instant::now();
        t.update(7, &Decoded::default(), 1, now);
        t.expire(now + Duration::from_secs(30));
        assert_eq!(t.aircraft.len(), 1);
        t.expire(now + Duration::from_secs(61));
        assert!(t.aircraft.is_empty());
    }

    #[test]
    fn haversine() {
        let a = Position { lat: 32.9618, lon: -96.8292 };
        let b = Position { lat: 32.8998, lon: -97.0403 }; // DFW area
        let d = distance_km(a, b);
        assert!((15.0..25.0).contains(&d), "{d}");
    }
}
