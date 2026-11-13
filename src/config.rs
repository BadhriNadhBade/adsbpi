//! Configuration: /etc/adsbpi.toml plus a small state file holding a home position set from the map.

use crate::tracker::Position;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Home {
    pub lat: f64,
    pub lon: f64,
}

impl Home {
    pub fn valid(&self) -> bool {
        self.lat.is_finite()
            && self.lon.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
            // (0,0) and the old dump1090 "-1,-1" placeholder both mean "not configured"
            && !(self.lat.abs() < 1e-9 && self.lon.abs() < 1e-9)
            && !(self.lat == -1.0 && self.lon == -1.0)
    }
    pub fn position(&self) -> Position {
        Position { lat: self.lat, lon: self.lon }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Receiver location, used to reject impossible positions and to centre the map.
    pub home: Option<Home>,
    /// RTL-SDR tuner gain in dB. Omit or set to 0 for automatic gain.
    pub gain_db: Option<f32>,
    /// Which RTL-SDR to use when several are plugged in.
    pub device_index: usize,
    /// Enable the bias-tee (powers an LNA through the antenna cable). Off by default.
    pub bias_tee: bool,
    pub web_bind: String,
    pub beast_bind: String,
    pub sbs_bind: String,
    /// Seconds without a message before an aircraft is dropped.
    pub ttl_secs: u64,
    /// Positions further than this from `home` are rejected.
    pub max_range_km: f64,
    /// Where runtime state (home position chosen on the map) is stored.
    pub state_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            home: None,
            gain_db: Some(31.0),
            device_index: 0,
            bias_tee: false,
            web_bind: "0.0.0.0:8080".into(),
            beast_bind: "127.0.0.1:30005".into(),
            sbs_bind: "127.0.0.1:30003".into(),
            ttl_secs: 60,
            max_range_km: 500.0,
            state_dir: default_state_dir(),
        }
    }
}

fn default_state_dir() -> PathBuf {
    if cfg!(unix) { PathBuf::from("/var/lib/adsbpi") } else { PathBuf::from(".") }
}

pub fn default_config_path() -> PathBuf {
    if cfg!(unix) { PathBuf::from("/etc/adsbpi.toml") } else { PathBuf::from("adsbpi.toml") }
}

impl Config {
    /// Load `path`. A missing file yields defaults; a malformed one is an error.
    pub fn load(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn home_file(&self) -> PathBuf {
        self.state_dir.join("home.json")
    }

    /// The effective home: the map-chosen one (state file) wins over the config file.
    pub fn effective_home(&self) -> Option<Home> {
        if let Ok(text) = std::fs::read_to_string(self.home_file()) {
            if let Ok(h) = serde_json::from_str::<Home>(&text) {
                if h.valid() {
                    return Some(h);
                }
            }
        }
        self.home.filter(Home::valid)
    }

    pub fn save_home(&self, h: Home) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.state_dir)?;
        let tmp = self.state_dir.join("home.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&h).unwrap())?;
        std::fs::rename(tmp, self.home_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_file_missing() {
        let c = Config::load(Path::new("/definitely/not/here.toml")).unwrap();
        assert_eq!(c.web_bind, "0.0.0.0:8080");
        assert_eq!(c.beast_bind, "127.0.0.1:30005");
        assert!(c.home.is_none());
    }

    #[test]
    fn parses_partial_file() {
        let c: Config = toml::from_str("gain_db = 40.2\n[home]\nlat = 32.9618\nlon = -96.8292\n").unwrap();
        assert_eq!(c.gain_db, Some(40.2));
        assert_eq!(c.home.unwrap().lat, 32.9618);
        assert_eq!(c.ttl_secs, 60);
    }

    #[test]
    fn unknown_keys_are_an_error() {
        assert!(toml::from_str::<Config>("gian_db = 3").is_err());
    }

    #[test]
    fn placeholder_homes_are_invalid() {
        assert!(!Home { lat: -1.0, lon: -1.0 }.valid());
        assert!(!Home { lat: 0.0, lon: 0.0 }.valid());
        assert!(!Home { lat: 91.0, lon: 0.0 }.valid());
        assert!(!Home { lat: f64::NAN, lon: 0.0 }.valid());
        assert!(Home { lat: 32.9618, lon: -96.8292 }.valid());
    }

    #[test]
    fn state_file_overrides_config_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("adsbpi-test-{}", std::process::id()));
        let c = Config {
            home: Some(Home { lat: 1.5, lon: 2.5 }),
            state_dir: dir.clone(),
            ..Config::default()
        };
        assert_eq!(c.effective_home(), Some(Home { lat: 1.5, lon: 2.5 }));
        c.save_home(Home { lat: 32.9618, lon: -96.8292 }).unwrap();
        assert_eq!(c.effective_home(), Some(Home { lat: 32.9618, lon: -96.8292 }));
        let _ = std::fs::remove_dir_all(dir);
    }
}
