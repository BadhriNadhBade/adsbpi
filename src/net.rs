//! TCP fan-out servers (Beast binary on :30005, SBS/BaseStation text on :30003).

use crate::modes::{Decoded, Frame};
use crate::tracker::Position;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Broadcasts byte buffers to every connected TCP client. A slow or dead client is dropped
/// instead of stalling the decoder.
#[derive(Clone, Default)]
pub struct Hub {
    clients: Arc<Mutex<Vec<SyncSender<Arc<Vec<u8>>>>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    pub fn send(&self, data: Vec<u8>) {
        let mut clients = self.clients.lock().unwrap();
        if clients.is_empty() {
            return;
        }
        let data = Arc::new(data);
        clients.retain(|tx| match tx.try_send(data.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false, // client cannot keep up
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// Start accepting clients on `addr`. Returns the bound address.
    pub fn listen(&self, addr: &str, name: &'static str) -> std::io::Result<std::net::SocketAddr> {
        let listener = TcpListener::bind(addr)?;
        let local = listener.local_addr()?;
        let hub = self.clone();
        thread::Builder::new().name(format!("{name}-accept")).spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let peer = stream.peer_addr().ok();
                let (tx, rx) = sync_channel::<Arc<Vec<u8>>>(2048);
                hub.clients.lock().unwrap().push(tx);
                log::info!("{name}: client connected {peer:?}");
                thread::spawn(move || writer(stream, rx, name, peer));
            }
        })?;
        Ok(local)
    }
}

fn writer(
    mut s: TcpStream,
    rx: std::sync::mpsc::Receiver<Arc<Vec<u8>>>,
    name: &str,
    peer: Option<std::net::SocketAddr>,
) {
    let _ = s.set_nodelay(true);
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    while let Ok(buf) = rx.recv() {
        if s.write_all(&buf).is_err() {
            break;
        }
    }
    log::info!("{name}: client disconnected {peer:?}");
}

/// Beast binary format: 0x1A, type, 6-byte 12 MHz timestamp, signal, message; 0x1A is escaped by doubling.
pub fn beast_encode(frame: &Frame, sample_index: u64, signal: u8) -> Vec<u8> {
    let kind = if frame.len == 7 { b'2' } else { b'3' };
    let ts = sample_index.wrapping_mul(6) & 0xFFFF_FFFF_FFFF; // 2 MS/s -> 12 MHz
    let mut payload = Vec::with_capacity(7 + frame.len);
    payload.extend_from_slice(&ts.to_be_bytes()[2..]);
    payload.push(signal);
    payload.extend_from_slice(frame.raw());
    let mut out = Vec::with_capacity(2 + payload.len() + 4);
    out.push(0x1A);
    out.push(kind);
    for b in payload {
        out.push(b);
        if b == 0x1A {
            out.push(0x1A);
        }
    }
    out
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn sbs_stamp(now: std::time::SystemTime) -> String {
    let d = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (y, m, day) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    format!(
        "{y:04}/{m:02}/{day:02},{:02}:{:02}:{:02}.{:03}",
        sod / 3600,
        (sod / 60) % 60,
        sod % 60,
        d.subsec_millis()
    )
}

/// SBS-1 (BaseStation) lines for one decoded frame. Position (MSG,3) needs the resolved position.
pub fn sbs_lines(
    frame: &Frame,
    d: &Decoded,
    position: Option<Position>,
    altitude: Option<i32>,
    now: std::time::SystemTime,
) -> Vec<String> {
    let ts = sbs_stamp(now);
    let hex = format!("{:06X}", frame.icao);
    let head = |t: u8| format!("MSG,{t},1,1,{hex},1,{ts},{ts}");
    let mut out = Vec::new();
    if let Some(id) = &d.ident {
        out.push(format!("{},{id},,,,,,,,,,,", head(1)));
    }
    if let (Some(p), true) = (position, d.cpr.is_some()) {
        let alt = altitude.map(|a| a.to_string()).unwrap_or_default();
        out.push(format!("{},,{alt},,,{:.5},{:.5},,,0,0,0,0", head(3), p.lat, p.lon));
    }
    if let (Some(gs), Some(trk)) = (d.speed_kt, d.track_deg) {
        let vr = d.vrate_fpm.map(|v| v.to_string()).unwrap_or_default();
        out.push(format!("{},,,{gs:.0},{trk:.0},,,{vr},,,,,", head(4)));
    }
    if matches!(frame.df, 0 | 4 | 16 | 20) {
        if let Some(a) = d.altitude_ft {
            out.push(format!("{},,{a},,,,,,,0,0,0,0", head(5)));
        }
    }
    if let Some(sq) = d.squawk {
        out.push(format!("{},,,,,,,,{sq:04},0,0,0,0", head(6)));
    }
    out.into_iter().map(|l| l + "\r\n").collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::from_hex;
    use crate::modes::{decode, validate, IcaoCache};
    use std::time::{Instant, UNIX_EPOCH};

    fn frame(h: &str) -> Frame {
        let mut c = IcaoCache::new(Duration::from_secs(60));
        validate(&from_hex(h), &mut c, Instant::now()).unwrap()
    }

    #[test]
    fn beast_layout_and_escaping() {
        let f = frame("8D4840D6202CC371C32CE0576098");
        let b = beast_encode(&f, 1000, 0x1A); // signal byte 0x1A must be doubled
        assert_eq!(&b[..2], &[0x1A, b'3']);
        assert_eq!(&b[2..8], &[0, 0, 0, 0, 0x17, 0x70]); // 6000 ticks
        assert_eq!(&b[8..10], &[0x1A, 0x1A]);
        assert_eq!(&b[10..], f.raw());
    }

    #[test]
    fn sbs_ident_line() {
        let f = frame("8D4840D6202CC371C32CE0576098");
        let lines = sbs_lines(&f, &decode(&f), None, None, UNIX_EPOCH + Duration::from_secs(86_400));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("MSG,1,1,1,4840D6,1,1970/01/02,00:00:00.000,"), "{}", lines[0]);
        assert!(lines[0].contains("KLM1023"));
        assert!(lines[0].ends_with("\r\n"));
    }

    #[test]
    fn every_sbs_line_has_22_fields() {
        let f = frame("8D40621D58C382D690C8AC2863A7");
        let pos = Some(Position { lat: 52.2572, lon: 3.91937 });
        let mut all = sbs_lines(&f, &decode(&f), pos, Some(38000), UNIX_EPOCH);
        let g = frame("8D485020994409940838175B284F");
        all.extend(sbs_lines(&g, &decode(&g), None, None, UNIX_EPOCH));
        let h = frame("8D4840D6202CC371C32CE0576098");
        all.extend(sbs_lines(&h, &decode(&h), None, None, UNIX_EPOCH));
        let mut alt = f.clone();
        alt.df = 4;
        let d = Decoded { altitude_ft: Some(12000), squawk: Some(1200), ..Default::default() };
        all.extend(sbs_lines(&alt, &d, None, None, UNIX_EPOCH));
        assert!(all.len() >= 5, "{all:?}");
        for l in &all {
            assert_eq!(l.trim_end().split(',').count(), 22, "{l}");
        }
        assert!(all.iter().any(|l| l.contains(",52.25720,3.91937,")));
        assert!(all.iter().any(|l| l.contains(",1200,0,0,0,0")));
    }

    #[test]
    fn date_conversion() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }

    #[test]
    fn hub_delivers_to_connected_client() {
        use std::io::Read;
        let hub = Hub::new();
        let addr = hub.listen("127.0.0.1:0", "test").unwrap();
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        for _ in 0..50 {
            if hub.client_count() == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        hub.send(b"hello".to_vec());
        let mut buf = [0u8; 5];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }
}
