/// println! that ignores a closed stdout (e.g. `adsbpi probe | head`) instead of panicking.
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

mod app;
mod config;
mod crc;
mod demod;
mod modes;
mod net;
mod sdr;
mod tracker;
mod web;

use app::App;
use clap::{Parser, Subcommand};
use config::{Config, Home};
use modes::{decode, validate, IcaoCache};
use std::path::PathBuf;
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tracker::Tracker;

#[derive(Parser)]
#[command(version, about = "ADS-B receiver, decoder, live map and Beast/SBS server for RTL-SDR dongles")]
struct Cli {
    /// Config file (default: /etc/adsbpi.toml; missing file = defaults)
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Decode a recorded 2 MS/s unsigned 8-bit I/Q file instead of using a dongle
    #[arg(long, value_name = "FILE")]
    file: Option<PathBuf>,
    /// With --file: keep the servers running after the file has been read
    #[arg(long, requires = "file")]
    serve: bool,
    /// Receiver latitude (overrides the config for this run)
    #[arg(long, requires = "lon", allow_negative_numbers = true)]
    lat: Option<f64>,
    /// Receiver longitude (overrides the config for this run)
    #[arg(long, requires = "lat", allow_negative_numbers = true)]
    lon: Option<f64>,
    /// Override the web listen address, e.g. 127.0.0.1:8080
    #[arg(long, value_name = "ADDR")]
    web_bind: Option<String>,
    /// Override the Beast listen address
    #[arg(long, value_name = "ADDR")]
    beast_bind: Option<String>,
    /// Override the SBS listen address
    #[arg(long, value_name = "ADDR")]
    sbs_bind: Option<String>,
    /// (testing) wait this long before reading --file, so clients can connect first
    #[arg(long, hide = true, default_value_t = 0, requires = "file")]
    start_delay_ms: u64,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List RTL-SDR dongles and check that one can be opened
    Probe,
}

fn main() {
    let cli = Cli::parse();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    if let Some(Command::Probe) = cli.command {
        std::process::exit(sdr::probe());
    }

    let cfg_path = cli.config.clone().unwrap_or_else(config::default_config_path);
    let mut cfg = match Config::load(&cfg_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: bad config: {e}");
            std::process::exit(2);
        }
    };
    if let Some(a) = cli.web_bind.clone() {
        cfg.web_bind = a;
    }
    if let Some(a) = cli.beast_bind.clone() {
        cfg.beast_bind = a;
    }
    if let Some(a) = cli.sbs_bind.clone() {
        cfg.sbs_bind = a;
    }
    let home = match (cli.lat, cli.lon) {
        (Some(lat), Some(lon)) => {
            let h = Home { lat, lon };
            if !h.valid() {
                eprintln!("error: --lat/--lon out of range");
                std::process::exit(2);
            }
            Some(h)
        }
        _ => cfg.effective_home(),
    };

    match home {
        Some(h) => log::info!("home position {:.5}, {:.5}", h.lat, h.lon),
        None => log::warn!("home position not set: range check disabled; set it on the map or in the config"),
    }

    let tracker = Tracker::new(home.map(|h| h.position()), cfg.max_range_km, Duration::from_secs(cfg.ttl_secs));
    let app = Arc::new(App::new(cfg.clone(), tracker));

    match web::start(app.clone(), &cfg.web_bind) {
        Ok(a) => log::info!("web map: http://{}/", display_addr(a)),
        Err(e) => fatal(&e),
    }
    match app.beast.listen(&cfg.beast_bind, "beast") {
        Ok(a) => log::info!("Beast output on {a}"),
        Err(e) => fatal(&format!("Beast server on {}: {e}", cfg.beast_bind)),
    }
    match app.sbs.listen(&cfg.sbs_bind, "sbs") {
        Ok(a) => log::info!("SBS output on {a}"),
        Err(e) => fatal(&format!("SBS server on {}: {e}", cfg.sbs_bind)),
    }

    let (tx, rx) = sync_channel::<Vec<u8>>(64);
    let file_mode = cli.file.is_some();
    let source = match cli.file.clone() {
        Some(p) => sdr::Source::File(p, Duration::from_millis(cli.start_delay_ms)),
        None => sdr::Source::Device,
    };
    sdr::spawn(source, cfg, app.clone(), tx);

    process(&app, rx);

    if file_mode {
        print_summary(&app);
        if cli.serve {
            log::info!("file finished; serving results (Ctrl-C to stop)");
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
    }
}

fn fatal(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn display_addr(a: std::net::SocketAddr) -> String {
    if a.ip().is_unspecified() { format!("localhost:{}", a.port()) } else { a.to_string() }
}

/// The decode loop: samples in, frames validated, tracker + network outputs updated.
fn process(app: &Arc<App>, rx: std::sync::mpsc::Receiver<Vec<u8>>) {
    let mut demod = demod::Demodulator::new();
    let mut cache = IcaoCache::new(Duration::from_secs(60));
    let mut last_expire = Instant::now();
    for chunk in rx {
        let now = Instant::now();
        demod.process(&chunk, |cand| {
            let Some(frame) = validate(cand.bytes, &mut cache, now) else {
                return false;
            };
            if frame.corrected {
                app.corrected_frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let d = decode(&frame);
            let update = app.tracker.lock().unwrap().update(frame.icao, &d, cand.signal, now);
            app.beast.send(net::beast_encode(&frame, cand.sample_index, cand.signal));
            if app.sbs.client_count() > 0 {
                let lines = net::sbs_lines(&frame, &d, update.new_position, update.altitude_ft, SystemTime::now());
                if !lines.is_empty() {
                    app.sbs.send(lines.concat().into_bytes());
                }
            }
            true
        });
        if now.duration_since(last_expire) >= Duration::from_secs(1) {
            app.tracker.lock().unwrap().expire(now);
            last_expire = now;
        }
    }
}

fn print_summary(app: &App) {
    let t = app.tracker.lock().unwrap();
    outln!("{} messages, {} aircraft, {} positions ({} rejected)", t.total_messages, t.aircraft.len(), t.total_positions, t.rejected_positions);
    let mut v: Vec<_> = t.aircraft.values().collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.messages));
    outln!("{:<7} {:<9} {:>7} {:>5} {:>6} {:>10} {:>10} {:>5}", "ICAO", "FLIGHT", "ALT ft", "kt", "TRACK", "LAT", "LON", "MSGS");
    for a in v {
        outln!(
            "{:<7} {:<9} {:>7} {:>5} {:>6} {:>10} {:>10} {:>5}",
            format!("{:06X}", a.icao),
            a.flight.as_deref().unwrap_or(""),
            a.altitude_ft.map(|x| x.to_string()).unwrap_or_default(),
            a.speed_kt.map(|x| format!("{x:.0}")).unwrap_or_default(),
            a.track_deg.map(|x| format!("{x:.0}")).unwrap_or_default(),
            a.pos.map(|p| format!("{:.4}", p.lat)).unwrap_or_default(),
            a.pos.map(|p| format!("{:.4}", p.lon)).unwrap_or_default(),
            a.messages
        );
    }
}
