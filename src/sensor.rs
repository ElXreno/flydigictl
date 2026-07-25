//! Temperature source: plain sysfs hwmon, no external tools.

use std::path::{Path, PathBuf};

use crate::config::Sensor;

/// Resolve the hwmon input a config asks for, so the path is looked up once
/// rather than on every sample.
pub fn resolve(sensor: &Sensor) -> Option<PathBuf> {
    let entries = std::fs::read_dir("/sys/class/hwmon").ok()?;

    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    dirs.sort();

    for dir in dirs {
        let name = read_trimmed(&dir.join("name"))?;
        if name != sensor.hwmon {
            continue;
        }

        let mut inputs: Vec<PathBuf> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("temp") && n.ends_with("_input"))
            })
            .collect();
        inputs.sort();

        if sensor.label.is_empty() {
            return inputs.into_iter().next();
        }

        for input in inputs {
            let label_path =
                input.with_file_name(input.file_name()?.to_str()?.replace("_input", "_label"));
            if read_trimmed(&label_path).is_some_and(|l| l == sensor.label) {
                return Some(input);
            }
        }
    }

    None
}

/// Read a resolved input, in whole degrees.
pub fn read(path: &Path) -> Option<u8> {
    let millidegrees: i64 = read_trimmed(path)?.parse().ok()?;
    let degrees = millidegrees / 1000;
    (0..=255).contains(&degrees).then_some(degrees as u8)
}

fn read_trimmed(path: &Path) -> Option<String> {
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}
