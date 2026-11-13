//! State shared between the decoder thread and the servers.

use crate::config::Config;
use crate::net::Hub;
use crate::tracker::Tracker;
use std::sync::atomic::AtomicU64;
use std::sync::Mutex;
use std::time::Instant;

pub struct App {
    pub tracker: Mutex<Tracker>,
    pub config: Config,
    pub started: Instant,
    pub beast: Hub,
    pub sbs: Hub,
    /// Frames accepted only after a single-bit error was repaired.
    pub corrected_frames: AtomicU64,
    sdr_status: Mutex<String>,
}

impl App {
    pub fn new(config: Config, tracker: Tracker) -> Self {
        Self {
            tracker: Mutex::new(tracker),
            config,
            started: Instant::now(),
            beast: Hub::new(),
            sbs: Hub::new(),
            corrected_frames: AtomicU64::new(0),
            sdr_status: Mutex::new("starting".into()),
        }
    }
    pub fn set_sdr_status(&self, s: String) {
        *self.sdr_status.lock().unwrap() = s;
    }
    pub fn sdr_status(&self) -> String {
        self.sdr_status.lock().unwrap().clone()
    }
}
