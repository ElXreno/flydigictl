//! Daemon configuration.
//!
//! On NixOS the file is a symlink into the store, so it cannot be written and
//! it is replaced wholesale on every switch. Both cases are handled: writes are
//! attempted and reported honestly, and the watcher looks at the directory
//! rather than the file.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_PATH: &str = "/etc/flydigictl/config.toml";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// One curve per subsystem. Each converts its own sensor into a speed, and
    /// the highest of those speeds wins - temperatures from a CPU and a stick
    /// of RAM mean entirely different things and must not be averaged.
    pub curves: Vec<Curve>,

    /// Input smoothing, shared by every curve.
    pub smoothing: Smoothing,

    /// How often to sample the sensors.
    pub interval_secs: u64,

    /// Only change the target once it moves by at least this much, to stop the
    /// fan chasing every degree.
    pub hysteresis_rpm: u16,

    /// Hold this speed instead of following the curve.
    pub manual_rpm: Option<u16>,

    /// What the cooler should do on its own when the host goes away.
    ///
    /// Applied to the device at startup and stored there, so it keeps working
    /// after the daemon stops or the machine shuts down. `None` leaves whatever
    /// the cooler was already set to.
    pub standby: Option<crate::protocol::Standby>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Curve {
    /// Shown in logs and status; defaults to the sensor it follows.
    pub name: String,

    /// Sensor this curve reacts to.
    pub sensor: Sensor,

    /// Points, sorted by temperature on load.
    pub points: Vec<Point>,

    /// Raw temperature at which this curve stops being smoothed.
    ///
    /// Per-curve because the number means nothing on its own: 85 °C is a normal
    /// load for a CPU and long past trouble for an SSD. Falls back to
    /// [`Smoothing::panic_c`].
    pub panic_c: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sensor {
    /// hwmon driver name, e.g. "k10temp", "nvme" or "spd5118".
    pub hwmon: String,

    /// Label of the input, e.g. "Tctl". Empty matches every input of that
    /// hwmon, and the hottest of them is used - which is what you want for two
    /// sticks of RAM or a pair of drives.
    pub label: String,
}

/// Smoothing applied to every sensor before its curve sees it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Smoothing {
    /// Time constant while the temperature climbs.
    pub rise_secs: f32,

    /// Time constant while it falls; larger than `rise_secs` keeps the fan from
    /// dropping back the moment a burst of load ends.
    pub fall_secs: f32,

    /// A raw reading at or above this bypasses smoothing entirely.
    pub panic_c: u8,
}

impl Default for Smoothing {
    fn default() -> Self {
        Self {
            rise_secs: 10.0,
            fall_secs: 60.0,
            panic_c: 85,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub temp_c: u8,
    pub rpm: u16,
}

impl Default for Sensor {
    fn default() -> Self {
        Self {
            hwmon: "k10temp".to_string(),
            label: String::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            curves: vec![Curve {
                name: "cpu".to_string(),
                sensor: Sensor::default(),
                panic_c: Some(95),
                points: vec![
                    Point {
                        temp_c: 55,
                        rpm: 500,
                    },
                    Point {
                        temp_c: 70,
                        rpm: 1500,
                    },
                    Point {
                        temp_c: 85,
                        rpm: 2800,
                    },
                    Point {
                        temp_c: 95,
                        rpm: 4000,
                    },
                ],
            }],
            smoothing: Smoothing::default(),
            interval_secs: 3,
            hysteresis_rpm: 100,
            manual_rpm: None,
            standby: Some(crate::protocol::Standby::Delayed),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;

        let mut config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;

        for curve in &mut config.curves {
            curve.points.sort_by_key(|point| point.temp_c);
        }
        Ok(config)
    }

    /// Write the config back, atomically via a neighbouring temp file.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        let parent = path.parent().unwrap_or(Path::new("."));
        let temp = parent.join(".config.toml.new");

        let write = || -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temp, path)
        };

        write().map_err(|source| {
            let _ = std::fs::remove_file(&temp);
            ConfigError::Write {
                path: path.to_path_buf(),
                source,
            }
        })
    }

    /// Can this config be written back?
    ///
    /// Checked by actually creating a file next to it: permissions alone do not
    /// tell the truth on a read-only store path, and the file itself is a
    /// symlink whose own mode says nothing.
    pub fn is_writable(path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };

        let probe = parent.join(".flydigictl-write-probe");
        match std::fs::File::create(&probe) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(_) => false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("cannot parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("cannot serialise config: {0}")]
    Serialize(toml::ser::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn sorts_curve_points_on_load() {
        let text = r#"
            [[curves]]
            name = "ram"
            sensor = { hwmon = "spd5118" }
            points = [
              { temp_c = 70, rpm = 4000 },
              { temp_c = 45, rpm = 500 },
              { temp_c = 60, rpm = 2400 },
            ]
        "#;

        let dir = std::env::temp_dir().join("flydigictl-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, text).unwrap();

        let config = Config::load(&path).unwrap();
        let temps: Vec<u8> = config.curves[0].points.iter().map(|p| p.temp_c).collect();
        assert_eq!(temps, vec![45, 60, 70]);

        std::fs::remove_file(&path).unwrap();
    }
}
