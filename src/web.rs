//! Embedded web map + JSON API.

use crate::app::App;
use crate::config::Home;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Arc;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Method, Request, Response, Server};

struct Asset {
    path: &'static str,
    mime: &'static str,
    body: &'static [u8],
}

const ASSETS: &[Asset] = &[
    Asset { path: "/", mime: "text/html; charset=utf-8", body: include_bytes!("../assets/index.html") },
    Asset { path: "/leaflet/leaflet.js", mime: "application/javascript", body: include_bytes!("../assets/leaflet/leaflet.js") },
    Asset { path: "/leaflet/leaflet.css", mime: "text/css", body: include_bytes!("../assets/leaflet/leaflet.css") },
    Asset { path: "/leaflet/images/marker-icon.png", mime: "image/png", body: include_bytes!("../assets/leaflet/images/marker-icon.png") },
    Asset { path: "/leaflet/images/marker-icon-2x.png", mime: "image/png", body: include_bytes!("../assets/leaflet/images/marker-icon-2x.png") },
    Asset { path: "/leaflet/images/marker-shadow.png", mime: "image/png", body: include_bytes!("../assets/leaflet/images/marker-shadow.png") },
    Asset { path: "/leaflet/images/layers.png", mime: "image/png", body: include_bytes!("../assets/leaflet/images/layers.png") },
    Asset { path: "/leaflet/images/layers-2x.png", mime: "image/png", body: include_bytes!("../assets/leaflet/images/layers-2x.png") },
];

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

/// Bind the web server and start worker threads. Returns the bound address.
pub fn start(app: Arc<App>, addr: &str) -> Result<std::net::SocketAddr, String> {
    let server = Arc::new(Server::http(addr).map_err(|e| format!("web server on {addr}: {e}"))?);
    let local = server.server_addr().to_ip().ok_or("web server has no IP address")?;
    for n in 0..4 {
        let server = server.clone();
        let app = app.clone();
        thread::Builder::new()
            .name(format!("web-{n}"))
            .spawn(move || {
                for req in server.incoming_requests() {
                    handle(&app, req);
                }
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(local)
}

fn json_response(v: &Value, status: u16) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(serde_json::to_vec(v).unwrap())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
        .with_header(header("Access-Control-Allow-Origin", "*"))
}

fn handle(app: &Arc<App>, mut req: Request) {
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    let method = req.method().clone();
    let result = match (&method, path.as_str()) {
        (Method::Get, "/data/aircraft.json") => req.respond(json_response(&aircraft_json(app), 200)),
        (Method::Get, "/api/status") => req.respond(json_response(&status_json(app), 200)),
        (Method::Post, "/api/home") => {
            let (v, code) = set_home(app, &mut req);
            req.respond(json_response(&v, code))
        }
        (Method::Get, p) | (Method::Head, p) => match ASSETS.iter().find(|a| a.path == p || (p == "/index.html" && a.path == "/")) {
            Some(a) => {
                let cache = if a.path == "/" { "no-cache" } else { "public, max-age=86400" };
                req.respond(
                    Response::from_data(a.body)
                        .with_header(header("Content-Type", a.mime))
                        .with_header(header("Cache-Control", cache)),
                )
            }
            None => req.respond(Response::from_string("not found").with_status_code(404)),
        },
        _ => req.respond(Response::from_string("method not allowed").with_status_code(405)),
    };
    if let Err(e) = result {
        log::debug!("web response failed: {e}");
    }
}

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

fn round(v: f64, places: i32) -> f64 {
    let k = 10f64.powi(places);
    (v * k).round() / k
}

/// dump1090/tar1090-compatible aircraft list.
pub fn aircraft_json(app: &App) -> Value {
    let now = Instant::now();
    let t = app.tracker.lock().unwrap();
    let mut list: Vec<Value> = t
        .aircraft
        .values()
        .map(|a| {
            let mut o = serde_json::Map::new();
            o.insert("hex".into(), json!(format!("{:06x}", a.icao)));
            if let Some(f) = &a.flight {
                o.insert("flight".into(), json!(f));
            }
            if let Some(v) = a.altitude_ft {
                o.insert("alt_baro".into(), json!(v));
            }
            if let Some(v) = a.speed_kt {
                o.insert("gs".into(), json!(round(v, 1)));
            }
            if let Some(v) = a.track_deg {
                o.insert("track".into(), json!(round(v, 1)));
            }
            if let Some(v) = a.vrate_fpm {
                o.insert("baro_rate".into(), json!(v));
            }
            if let Some(v) = a.squawk {
                o.insert("squawk".into(), json!(format!("{v:04}")));
            }
            if let (Some(p), Some(pt)) = (a.pos, a.pos_time) {
                o.insert("lat".into(), json!(round(p.lat, 5)));
                o.insert("lon".into(), json!(round(p.lon, 5)));
                o.insert("seen_pos".into(), json!(round(now.duration_since(pt).as_secs_f64(), 1)));
            }
            o.insert("messages".into(), json!(a.messages));
            o.insert("seen".into(), json!(round(now.duration_since(a.last_seen).as_secs_f64(), 1)));
            // Approximate dBFS from the 0..255 preamble level.
            let rssi = 20.0 * (a.rssi.max(1) as f64 / 255.0).log10();
            o.insert("rssi".into(), json!(round(rssi, 1)));
            Value::Object(o)
        })
        .collect();
    list.sort_by(|a, b| a["hex"].as_str().cmp(&b["hex"].as_str()));
    json!({ "now": round(now_secs(), 1), "messages": t.total_messages, "aircraft": list })
}

pub fn status_json(app: &App) -> Value {
    let t = app.tracker.lock().unwrap();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_s": app.started.elapsed().as_secs(),
        "sdr": app.sdr_status(),
        "messages": t.total_messages,
        "aircraft": t.aircraft.len(),
        "with_position": t.aircraft.values().filter(|a| a.pos.is_some()).count(),
        "positions": t.total_positions,
        "rejected_positions": t.rejected_positions,
        "corrected_frames": app.corrected_frames.load(std::sync::atomic::Ordering::Relaxed),
        "home": t.home.map(|h| json!({"lat": h.lat, "lon": h.lon})),
        "max_range_km": t.max_range_km,
        "beast_clients": app.beast.client_count(),
        "sbs_clients": app.sbs.client_count(),
    })
}

fn set_home(app: &Arc<App>, req: &mut Request) -> (Value, u16) {
    let mut body = Vec::new();
    if req.as_reader().take(2048).read_to_end(&mut body).is_err() {
        return (json!({"error": "could not read body"}), 400);
    }
    let home: Home = match serde_json::from_slice(&body) {
        Ok(h) => h,
        Err(e) => return (json!({"error": format!("expected {{\"lat\":..,\"lon\":..}}: {e}")}), 400),
    };
    if !home.valid() {
        return (json!({"error": "latitude/longitude out of range or a placeholder"}), 400);
    }
    if let Err(e) = app.config.save_home(home) {
        // Still apply it for this run so the map works, but tell the user it will not persist.
        app.tracker.lock().unwrap().home = Some(home.position());
        log::warn!("could not persist home position: {e}");
        return (json!({"ok": true, "persisted": false, "warning": e.to_string()}), 200);
    }
    app.tracker.lock().unwrap().home = Some(home.position());
    log::info!("home position set to {:.5}, {:.5}", home.lat, home.lon);
    (json!({"ok": true, "persisted": true}), 200)
}
