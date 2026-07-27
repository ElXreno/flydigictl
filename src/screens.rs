//! Whether anything is being displayed, straight from the kernel.
//!
//! A compositor turning monitors off leaves it in sysfs: the connector stops
//! being enabled, or its DPMS property goes off. That is readable by anything
//! with a filesystem, which a system service has and a session bus is not.

use std::path::Path;

/// True when every connected output is dark, `None` when there is nothing to
/// judge by - no DRM at all, or a kernel that reports none of this.
pub fn all_dark() -> Option<bool> {
    all_dark_in(Path::new("/sys/class/drm"))
}

fn all_dark_in(drm: &Path) -> Option<bool> {
    let entries = std::fs::read_dir(drm).ok()?;

    let mut connected = 0;
    let mut dark = 0;

    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();

        if read(&path, "status").as_deref() != Some("connected") {
            continue;
        }

        connected += 1;

        if is_dark(
            read(&path, "enabled").as_deref(),
            read(&path, "dpms").as_deref(),
        ) {
            dark += 1;
        }
    }

    // No outputs at all is a machine with nothing to look at, which counts.
    (connected == 0).then_some(true).or(Some(dark == connected))
}

/// An output is dark when it is switched off, and also when the compositor
/// simply stopped driving it: turning a monitor off does one or the other
/// depending on the driver.
fn is_dark(enabled: Option<&str>, dpms: Option<&str>) -> bool {
    matches!(dpms, Some("Off")) || matches!(enabled, Some("disabled"))
}

fn read(dir: &Path, name: &str) -> Option<String> {
    Some(
        std::fs::read_to_string(dir.join(name))
            .ok()?
            .trim()
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_output_as_dark_either_way() {
        assert!(is_dark(Some("disabled"), Some("On")));
        assert!(is_dark(Some("enabled"), Some("Off")));
        assert!(!is_dark(Some("enabled"), Some("On")));
    }

    #[test]
    fn missing_attributes_mean_lit() {
        assert!(!is_dark(None, None));
    }

    fn output(drm: &Path, name: &str, status: &str, enabled: &str, dpms: &str) {
        let dir = drm.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("status"), status).unwrap();
        std::fs::write(dir.join("enabled"), enabled).unwrap();
        std::fs::write(dir.join("dpms"), dpms).unwrap();
    }

    #[test]
    fn judges_a_machine_by_its_connected_outputs() {
        let drm = std::env::temp_dir().join("flydigictl-screens-test");
        let _ = std::fs::remove_dir_all(&drm);
        std::fs::create_dir_all(&drm).unwrap();

        // A laptop panel that is off, an external monitor that is not, and a
        // pile of connectors with nothing plugged in.
        output(&drm, "card1-eDP-1", "connected", "disabled", "Off");
        output(&drm, "card1-DP-2", "connected", "enabled", "On");
        output(&drm, "card1-DP-3", "disconnected", "disabled", "Off");
        assert_eq!(all_dark_in(&drm), Some(false));

        output(&drm, "card1-DP-2", "connected", "disabled", "Off");
        assert_eq!(all_dark_in(&drm), Some(true));

        // Nothing plugged in anywhere is nothing to light for either.
        output(&drm, "card1-eDP-1", "disconnected", "disabled", "Off");
        output(&drm, "card1-DP-2", "disconnected", "disabled", "Off");
        assert_eq!(all_dark_in(&drm), Some(true));

        std::fs::remove_dir_all(&drm).unwrap();
    }

    #[test]
    fn says_nothing_without_drm() {
        assert_eq!(all_dark_in(Path::new("/nonexistent/drm")), None);
    }
}
