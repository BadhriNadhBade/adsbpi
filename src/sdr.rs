//! Sample sources: an RTL-SDR dongle (pure-Rust driver over nusb) or a recorded I/Q file.

use crate::demod::SAMPLE_RATE;
use crate::{app::App, config::Config};
use rs_rtl::{DeviceDescriptors, DeviceId, RtlSdr};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const FREQ_HZ: u32 = 1_090_000_000;

pub enum Source {
    Device,
    File(PathBuf, Duration),
}

pub fn spawn(source: Source, cfg: Config, app: Arc<App>, tx: SyncSender<Vec<u8>>) {
    thread::Builder::new()
        .name("sdr".into())
        .spawn(move || match source {
            Source::File(path, delay) => {
                thread::sleep(delay);
                match read_file(&path, &tx) {
                    Ok(n) => {
                        log::info!("read {n} bytes from {}", path.display());
                        app.set_sdr_status(format!("file {} done", path.display()));
                    }
                    Err(e) => {
                        log::error!("{}: {e}", path.display());
                        app.set_sdr_status(format!("error: {e}"));
                    }
                }
                // tx dropped here -> the processing loop ends.
            }
            Source::Device => device_loop(&cfg, &app, &tx),
        })
        .expect("spawn sdr thread");
}

fn read_file(path: &PathBuf, tx: &SyncSender<Vec<u8>>) -> std::io::Result<u64> {
    let mut f = std::fs::File::open(path)?;
    let mut total = 0u64;
    loop {
        let mut buf = vec![0u8; 256 * 1024];
        let mut filled = 0;
        while filled < buf.len() {
            let n = f.read(&mut buf[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            return Ok(total);
        }
        buf.truncate(filled & !1); // keep whole I/Q pairs
        total += filled as u64;
        if tx.send(buf).is_err() {
            return Ok(total);
        }
    }
}

/// Extra advice when the USB bus is not visible at all, which is what a container without
/// passthrough looks like (a missing dongle on a normal machine still has /dev/bus/usb).
fn usb_hint() -> &'static str {
    if cfg!(target_os = "linux") && !std::path::Path::new("/dev/bus/usb").exists() {
        ". /dev/bus/usb does not exist here: in a container or VM, pass the USB device through from the host (README: Proxmox / LXC)"
    } else {
        ""
    }
}

/// Keep trying to (re)open the dongle forever: unplugging it must not kill the web map.
fn device_loop(cfg: &Config, app: &Arc<App>, tx: &SyncSender<Vec<u8>>) {
    let mut announced = String::new();
    loop {
        match stream_device(cfg, app, tx) {
            Ok(()) => return, // receiver side closed
            Err(e) => {
                if e != announced {
                    log::warn!("RTL-SDR: {e} (retrying every 5 s){}", usb_hint());
                    announced = e.clone();
                }
                app.set_sdr_status(format!("waiting for RTL-SDR: {e}"));
                thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

fn stream_device(cfg: &Config, app: &Arc<App>, tx: &SyncSender<Vec<u8>>) -> Result<(), String> {
    let mut sdr = RtlSdr::open(DeviceId::Index(cfg.device_index)).map_err(|e| e.to_string())?;
    sdr.set_sample_rate(SAMPLE_RATE).map_err(|e| format!("set sample rate: {e}"))?;
    sdr.set_center_freq(FREQ_HZ).map_err(|e| format!("set frequency: {e}"))?;
    match cfg.gain_db {
        Some(db) if db > 0.0 => {
            let want = (db * 10.0).round() as i32;
            let gains = sdr.gains().to_vec();
            let pick = gains.iter().copied().min_by_key(|g| (g - want).abs()).unwrap_or(want);
            sdr.set_gain_manual(pick).map_err(|e| format!("set gain: {e}"))?;
        }
        _ => sdr.set_gain_auto().map_err(|e| format!("set gain: {e}"))?,
    }
    if cfg.bias_tee {
        sdr.set_bias_t(true).map_err(|e| format!("bias tee: {e}"))?;
    }
    let tuner = format!("{:?}", sdr.tuner_type());
    let reader = sdr.start_streaming().map_err(|e| format!("start streaming: {e}"))?;
    log::info!("RTL-SDR streaming: tuner {tuner}, {} MS/s at 1090 MHz", SAMPLE_RATE as f64 / 1e6);
    app.set_sdr_status(format!("receiving ({tuner})"));
    let mut dropped_logged = 0;
    loop {
        match reader.recv() {
            Ok(Some(buf)) => {
                if tx.send(buf).is_err() {
                    return Ok(());
                }
                let dropped = reader.dropped_chunks();
                if dropped > dropped_logged && dropped % 100 == 1 {
                    log::warn!("USB sample chunks dropped so far: {dropped}");
                }
                dropped_logged = dropped;
            }
            Ok(None) => return Err("sample stream ended".into()),
            Err(e) => return Err(format!("stream error: {e}")),
        }
    }
}

/// `adsbpi probe`: list dongles and try to talk to the first one.
pub fn probe() -> i32 {
    let descs = match DeviceDescriptors::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("USB enumeration failed: {e}");
            return 1;
        }
    };
    if descs.is_empty() {
        eprintln!("No RTL-SDR found. Is it plugged in? On Linux, check `lsusb` for 0bda:2838 and that");
        eprintln!("the kernel's DVB driver is not holding it (the installer blacklists it).");
        return 1;
    }
    for d in descs.iter() {
        outln!(
            "#{}: {:04x}:{:04x} {} {} serial {}",
            d.index,
            d.vendor_id,
            d.product_id,
            d.manufacturer.as_deref().unwrap_or("?"),
            d.product.as_deref().unwrap_or("?"),
            d.serial.as_deref().unwrap_or("?")
        );
    }
    match RtlSdr::open(DeviceId::Index(0)) {
        Ok(sdr) => {
            outln!("opened #0: tuner {:?}, gains {:?}", sdr.tuner_type(), sdr.gains());
            0
        }
        Err(e) => {
            eprintln!("found a dongle but could not open it: {e}");
            eprintln!("(permissions? run as root or install the udev rule; another program using it?)");
            2
        }
    }
}
