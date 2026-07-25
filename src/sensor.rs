//! Temperature sources: plain sysfs hwmon, no external tools.

use std::path::{Path, PathBuf};

use crate::config::Sensor;

/// One temperature input the system offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub hwmon: String,
    pub label: String,
    pub path: PathBuf,
}

/// Everything readable under `/sys/class/hwmon`, for `flydigictl sensors` and
/// for telling the user what they could have written instead.
pub fn list() -> Vec<Available> {
    let Ok(entries) = std::fs::read_dir("/sys/class/hwmon") else {
        return Vec::new();
    };

    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    dirs.sort();

    let mut found = Vec::new();
    for dir in dirs {
        // A hwmon without a name is not something a config can refer to, but it
        // is no reason to stop looking at the rest.
        let Some(hwmon) = read_trimmed(&dir.join("name")) else {
            continue;
        };

        for input in inputs_of(&dir) {
            let label = label_of(&input).unwrap_or_default();
            found.push(Available {
                hwmon: hwmon.clone(),
                label,
                path: input,
            });
        }
    }

    found
}

/// Find the input a config entry asks for.
pub fn resolve(sensor: &Sensor) -> Option<PathBuf> {
    let available = list();

    let matching = available
        .iter()
        .filter(|entry| entry.hwmon == sensor.hwmon)
        .collect::<Vec<_>>();

    if sensor.label.is_empty() {
        return matching.first().map(|entry| entry.path.clone());
    }

    matching
        .iter()
        .find(|entry| entry.label == sensor.label)
        .map(|entry| entry.path.clone())
}

/// Read a resolved input, in whole degrees.
pub fn read(path: &Path) -> Option<u8> {
    let millidegrees: i64 = read_trimmed(path)?.parse().ok()?;
    let degrees = millidegrees / 1000;
    (0..=255).contains(&degrees).then_some(degrees as u8)
}

fn inputs_of(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut inputs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("temp") && name.ends_with("_input"))
        })
        .collect();

    inputs.sort();
    inputs
}

fn label_of(input: &Path) -> Option<String> {
    let name = input.file_name()?.to_str()?;
    read_trimmed(&input.with_file_name(name.replace("_input", "_label")))
}

fn read_trimmed(path: &Path) -> Option<String> {
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}

/// All inputs a config entry matches.
///
/// An empty label matches every input of that hwmon - one curve then covers
/// both DIMMs or both drives without spelling out indices.
pub fn resolve_all(sensor: &Sensor) -> Vec<PathBuf> {
    list()
        .into_iter()
        .filter(|entry| entry.hwmon == sensor.hwmon)
        .filter(|entry| sensor.label.is_empty() || entry.label == sensor.label)
        .map(|entry| entry.path)
        .collect()
}
