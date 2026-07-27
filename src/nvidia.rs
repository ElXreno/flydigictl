//! Temperature of an NVIDIA GPU, asked for only when it is awake.
//!
//! The driver publishes no hwmon, so the only way in is the vendor tool - and
//! that wakes a sleeping card. On a laptop the card sleeps nearly all the time
//! and pulling it out of D3cold costs power, seconds of latency and, on this
//! chassis, a fan spin-up: the exact things a fan curve is meant to avoid.
//!
//! So the power state is checked first, in sysfs, where looking costs nothing.
//! A sleeping card reports no temperature at all, which is the truth: it is
//! making no heat worth answering.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PCI: &str = "/sys/bus/pci/devices";

/// NVIDIA's PCI vendor identifier.
const VENDOR: &str = "0x10de";

/// PCI class for a display controller, of which the first is the GPU.
const DISPLAY: &str = "0x030";

/// Every NVIDIA GPU on the machine, by PCI address.
pub fn cards() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(PCI) else {
        return Vec::new();
    };

    let mut found: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let path = entry.path();
            read(&path, "vendor").as_deref() == Some(VENDOR)
                && read(&path, "class").is_some_and(|class| class.starts_with(DISPLAY))
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();

    found.sort();
    found
}

/// Is the card awake? `None` when it has no runtime power management at all,
/// in which case it is always awake.
fn awake(card: &str) -> Option<bool> {
    let status = read(&Path::new(PCI).join(card), "power/runtime_status")?;
    Some(status == "active")
}

/// The card's temperature in whole degrees, or `None` when it is asleep, when
/// there is no tool to ask with, or when the answer makes no sense.
pub fn read_temperature(card: &str) -> Option<u8> {
    if awake(card) == Some(false) {
        return None;
    }

    let output = Command::new("nvidia-smi")
        .args([
            "--id",
            card,
            "--query-gpu=temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse(&String::from_utf8_lossy(&output.stdout))
}

fn parse(text: &str) -> Option<u8> {
    let degrees: u32 = text.trim().parse().ok()?;
    (degrees > 0 && degrees < 128).then_some(degrees as u8)
}

fn read(dir: &Path, name: &str) -> Option<String> {
    Some(
        std::fs::read_to_string(dir.join(name))
            .ok()?
            .trim()
            .to_string(),
    )
}

/// Where a card's power state lives, for anyone wanting to show it.
pub fn power_state(card: &str) -> Option<String> {
    read(&PathBuf::from(PCI).join(card), "power/runtime_status")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_plain_number() {
        assert_eq!(parse("47\n"), Some(47));
        assert_eq!(parse(" 61 "), Some(61));
    }

    #[test]
    fn refuses_an_answer_that_is_not_one() {
        assert_eq!(parse("N/A"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("0"), None);
        assert_eq!(parse("300"), None);
    }
}
