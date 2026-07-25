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
    /// Where to read temperatures from. Several may be listed - a laptop
    /// usually wants the CPU and the GPU, whichever is hotter.
    pub sensors: Vec<Sensor>,

    /// How to combine several sensors into the number the curve sees.
    pub aggregate: Aggregate,

    /// Fan curve, sorted by temperature on load.
    pub curve: Vec<Point>,

    /// How often to sample the sensor.
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sensor {
    /// hwmon driver name, e.g. "k10temp" or "coretemp".
    pub hwmon: String,

    /// Label of the input to use, e.g. "Tctl". Empty means the first input.
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub temp_c: u8,
    pub rpm: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregate {
    /// Follow the hottest sensor. Safe default: cooling is about the worst
    /// case, not the average one.
    #[default]
    Max,
    /// Average of the sensors that could be read.
    Mean,
}

impl Aggregate {
    pub fn apply(self, readings: &[u8]) -> Option<u8> {
        if readings.is_empty() {
            return None;
        }
        match self {
            Self::Max => readings.iter().copied().max(),
            Self::Mean => {
                let sum: u32 = readings.iter().map(|t| u32::from(*t)).sum();
                Some((sum / readings.len() as u32) as u8)
            }
        }
    }
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
            sensors: vec![Sensor::default()],
            aggregate: Aggregate::Max,
            curve: vec![
                Point { temp_c: 45, rpm: 0 },
                Point {
                    temp_c: 55,
                    rpm: 1300,
                },
                Point {
                    temp_c: 65,
                    rpm: 2100,
                },
                Point {
                    temp_c: 75,
                    rpm: 2800,
                },
                Point {
                    temp_c: 85,
                    rpm: 3500,
                },
            ],
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

        config.curve.sort_by_key(|point| point.temp_c);
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

    /// Target speed for a temperature, interpolated between curve points.
    pub fn target_for(&self, temp_c: u8) -> Option<u16> {
        if let Some(rpm) = self.manual_rpm {
            return Some(rpm);
        }

        let curve = &self.curve;
        let first = curve.first()?;
        let last = curve.last()?;

        if temp_c <= first.temp_c {
            return Some(first.rpm);
        }
        if temp_c >= last.temp_c {
            return Some(last.rpm);
        }

        let upper = curve.iter().position(|p| p.temp_c >= temp_c)?;
        let (a, b) = (curve[upper - 1], curve[upper]);
        let span = u32::from(b.temp_c - a.temp_c);
        if span == 0 {
            return Some(b.rpm);
        }

        let into = u32::from(temp_c - a.temp_c);
        let low = u32::from(a.rpm);
        let high = u32::from(b.rpm);
        let rpm = if high >= low {
            low + (high - low) * into / span
        } else {
            low - (low - high) * into / span
        };

        Some(rpm as u16)
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

    fn curve() -> Config {
        Config {
            curve: vec![
                Point { temp_c: 40, rpm: 0 },
                Point {
                    temp_c: 60,
                    rpm: 1000,
                },
                Point {
                    temp_c: 80,
                    rpm: 3000,
                },
            ],
            ..Config::default()
        }
    }

    #[test]
    fn holds_the_ends_of_the_curve() {
        let config = curve();
        assert_eq!(config.target_for(20), Some(0));
        assert_eq!(config.target_for(40), Some(0));
        assert_eq!(config.target_for(95), Some(3000));
    }

    #[test]
    fn interpolates_between_points() {
        let config = curve();
        assert_eq!(config.target_for(50), Some(500));
        assert_eq!(config.target_for(70), Some(2000));
    }

    #[test]
    fn manual_speed_wins_over_the_curve() {
        let config = Config {
            manual_rpm: Some(1234),
            ..curve()
        };
        assert_eq!(config.target_for(90), Some(1234));
    }

    #[test]
    fn round_trips_through_toml() {
        let config = curve();
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(config, back);
    }
}
