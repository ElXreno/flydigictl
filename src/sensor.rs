//! Temperature sources: plain sysfs hwmon, no external tools.

use std::path::{Path, PathBuf};

use crate::config::Sensor;

/// One temperature input the system offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub hwmon: String,
    /// Stable address of the chip: a PCI slot, plus an i2c address when
    /// several chips share one bus.
    ///
    /// Two of the same part answer to the same hwmon name - a pair of drives is
    /// `nvme` twice - so something has to tell them apart, and it cannot be the
    /// kernel's own numbering: `nvme0` and `nvme1` are handed out in probe
    /// order and swap between boots.
    pub device: String,

    /// What the kernel calls it right now: `nvme0`, `21-0050`, `phy0`. Fine to
    /// show, not to rely on.
    pub kernel: String,
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

        let kernel = kernel_name_of(&dir);
        let device = address_of(&dir).unwrap_or_else(|| kernel.clone());

        for input in inputs_of(&dir) {
            let label = label_of(&input).unwrap_or_default();
            found.push(Available {
                hwmon: hwmon.clone(),
                device: device.clone(),
                kernel: kernel.clone(),
                label,
                path: input,
            });
        }
    }

    found
}

/// Read a resolved input, in whole degrees.
pub fn read(path: &Path) -> Option<u8> {
    let millidegrees: i64 = read_trimmed(path)?.parse().ok()?;
    let degrees = millidegrees / 1000;
    (0..=255).contains(&degrees).then_some(degrees as u8)
}

fn kernel_name_of(dir: &Path) -> String {
    std::fs::read_link(dir.join("device"))
        .ok()
        .and_then(|target| {
            target
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Where the chip sits, rather than what the kernel called it this time.
///
/// The sysfs path of a hwmon runs from a PCI slot down to the thing itself, so
/// the last slot in it is a stable address. What hangs below is not: `nvme0`
/// and `nvme1` are probe order, `i2c-21` is adapter registration order. The one
/// exception is an i2c client's own address, which is set in hardware, and
/// without it a pair of memory sticks on one bus stay indistinguishable.
fn address_of(dir: &Path) -> Option<String> {
    let real = std::fs::canonicalize(dir).ok()?;
    let parts: Vec<&str> = real
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .collect();

    let slot = parts.iter().rposition(|part| is_pci_address(part))?;

    let mut address = parts[slot].to_string();
    for part in &parts[slot + 1..] {
        if let Some(client) = i2c_client_address(part) {
            address.push('/');
            address.push_str(client);
        }
    }

    Some(address)
}

/// `0000:05:00.0`
fn is_pci_address(part: &str) -> bool {
    let mut fields = part.split([':', '.']);
    let widths = [4, 2, 2, 1];

    for width in widths {
        match fields.next() {
            Some(field) if field.len() == width && field.chars().all(|c| c.is_ascii_hexdigit()) => {
            }
            _ => return false,
        }
    }

    fields.next().is_none()
}

/// `21-0050` names bus 21, chip `0050`. Only the second half is the chip's.
fn i2c_client_address(part: &str) -> Option<&str> {
    let (bus, chip) = part.split_once('-')?;
    let usable = !bus.is_empty()
        && bus.chars().all(|c| c.is_ascii_digit())
        && chip.len() == 4
        && chip.chars().all(|c| c.is_ascii_hexdigit());

    usable.then_some(chip)
}

/// The address as worth showing: a slot without its always-zero domain, or an
/// i2c address on its own.
pub fn short_address(address: &str) -> &str {
    match address.rsplit_once('/') {
        Some((_, client)) => client,
        None => address.strip_prefix("0000:").unwrap_or(address),
    }
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
/// Empty fields match anything: no label covers every input of that hwmon, and
/// no device covers every chip answering to that name. One curve then spans
/// both DIMMs or both drives without spelling out indices, while naming a
/// device picks out one of them.
pub fn resolve_all(sensor: &Sensor) -> Vec<PathBuf> {
    list()
        .into_iter()
        .filter(|entry| entry.hwmon == sensor.hwmon)
        // Accepting the kernel name too keeps a hand-written `device = "nvme0"`
        // working, even though the picker writes the stable address.
        .filter(|entry| {
            sensor.device.is_empty()
                || entry.device == sensor.device
                || entry.kernel == sensor.device
        })
        .filter(|entry| sensor.label.is_empty() || entry.label == sensor.label)
        .map(|entry| entry.path)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_pci_slots() {
        assert!(is_pci_address("0000:05:00.0"));
        assert!(is_pci_address("0000:00:18.3"));
        assert!(!is_pci_address("pci0000:00"));
        assert!(!is_pci_address("i2c-21"));
        assert!(!is_pci_address("nvme0"));
        assert!(!is_pci_address("0000:05:00.0.1"));
    }

    #[test]
    fn takes_the_chip_half_of_an_i2c_name() {
        assert_eq!(i2c_client_address("21-0050"), Some("0050"));
        assert_eq!(i2c_client_address("3-004f"), Some("004f"));
        assert_eq!(i2c_client_address("i2c-21"), None);
        assert_eq!(i2c_client_address("nvme0"), None);
    }

    #[test]
    fn shows_the_readable_tail() {
        assert_eq!(short_address("0000:05:00.0"), "05:00.0");
        assert_eq!(short_address("0000:00:14.0/0050"), "0050");
        assert_eq!(short_address("thermal_zone0"), "thermal_zone0");
    }
}
