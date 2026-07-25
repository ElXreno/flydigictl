//! hidraw discovery and I/O.
//!
//! Coolers paired over Bluetooth hang off `uhid` and have no USB parent, so
//! matching on `idVendor` in sysfs does not work. `HID_ID` in the hid parent's
//! uevent carries `bus:vendor:product` for both transports, so match on that.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use log::debug;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

use crate::error::{Error, Result};
use crate::protocol::{self, Model, Status, VID};

pub struct Found {
    pub path: PathBuf,
    pub model: Model,
}

/// Scan `/sys/class/hidraw` for Flydigi BS series coolers.
pub fn find_all() -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir("/sys/class/hidraw") else {
        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();

    names
        .iter()
        .filter_map(|name| {
            let uevent = Path::new("/sys/class/hidraw")
                .join(name)
                .join("device/uevent");
            let text = std::fs::read_to_string(&uevent).ok()?;
            let model = parse_hid_id(&text)?;
            debug!("{name}: matched {}", model.name());
            Some(Found {
                path: PathBuf::from("/dev").join(name),
                model,
            })
        })
        .collect()
}

/// Extract the model from a `HID_ID=bus:vendor:product` line.
fn parse_hid_id(uevent: &str) -> Option<Model> {
    let hid_id = uevent
        .lines()
        .find_map(|line| line.strip_prefix("HID_ID="))?;

    let mut parts = hid_id.split(':');
    let _bus = parts.next()?;
    let vendor = u32::from_str_radix(parts.next()?, 16).ok()?;
    let product = u32::from_str_radix(parts.next()?, 16).ok()?;

    if vendor != VID as u32 {
        return None;
    }
    Model::from_pid(product as u16)
}

pub struct Device {
    file: File,
    pub model: Model,
    pub path: PathBuf,
}

impl Device {
    /// Open an explicit hidraw path, or the first cooler found.
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let found = match path {
            Some(path) => Found {
                path: path.to_path_buf(),
                model: probe_model(path).unwrap_or(Model::Bs3Pro),
            },
            None => find_all().into_iter().next().ok_or(Error::NotFound)?,
        };

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&found.path)
            .map_err(|source| Error::Open {
                path: found.path.clone(),
                source,
            })?;

        Ok(Self {
            file,
            model: found.model,
            path: found.path,
        })
    }

    pub fn send(&mut self, report: impl AsRef<[u8]>) -> Result<()> {
        self.file.write_all(report.as_ref()).map_err(Error::Write)
    }

    /// Send a command and wait for the device to acknowledge it, returning the
    /// status byte it echoes back.
    ///
    /// A report the firmware does not understand is dropped silently, so a
    /// missing acknowledgement is the only signal that a command was rejected.
    pub fn send_acked(
        &mut self,
        report: [u8; protocol::REPORT_LEN],
        timeout: Duration,
    ) -> Result<u8> {
        let cmd = report[3];
        self.send(report)?;

        self.read_matching(timeout, |buf| {
            protocol::parse_ack(buf)
                .filter(|ack| ack.cmd == cmd)
                .map(|ack| ack.payload.first().copied().unwrap_or_default())
        })
        .map_err(|err| match err {
            Error::Timeout => Error::NoAck { cmd },
            other => other,
        })
    }

    /// Send a query and return the payload it answers with.
    pub fn query(
        &mut self,
        report: [u8; protocol::REPORT_LEN],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let cmd = report[3];
        self.send(report)?;

        self.read_matching(timeout, |buf| {
            protocol::parse_ack(buf)
                .filter(|ack| ack.cmd == cmd)
                .map(|ack| ack.payload.to_vec())
        })
        .map_err(|err| match err {
            Error::Timeout => Error::NoAck { cmd },
            other => other,
        })
    }

    /// Wait for the next status notification, ignoring unrelated frames.
    pub fn read_status(&mut self, timeout: Duration) -> Result<Status> {
        self.read_matching(timeout, protocol::parse_status)
    }

    /// Read frames until `want` accepts one, or the deadline passes.
    fn read_matching<T>(
        &mut self,
        timeout: Duration,
        mut want: impl FnMut(&[u8]) -> Option<T>,
    ) -> Result<T> {
        let deadline = std::time::Instant::now() + timeout;
        let mut buf = [0u8; 64];

        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }

            let mut fds = [PollFd::new(self.file.as_fd(), PollFlags::POLLIN)];
            let timeout =
                PollTimeout::try_from(remaining.as_millis() as u64).unwrap_or(PollTimeout::MAX);

            match poll(&mut fds, timeout) {
                Ok(0) => return Err(Error::Timeout),
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => continue,
                Err(errno) => return Err(Error::Poll(errno)),
            }

            // A cooler that loses power mid-session (a PD renegotiation is
            // enough) drops its Bluetooth link and takes the hidraw node with
            // it. Say so instead of blaming the timeout.
            let revents = fds[0].revents().unwrap_or(PollFlags::empty());
            if revents.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
                return Err(Error::Disconnected);
            }

            let n = match self.file.read(&mut buf) {
                Ok(0) => return Err(Error::Disconnected),
                Ok(n) => n,
                Err(err) if err.raw_os_error() == Some(nix::libc::ENODEV) => {
                    return Err(Error::Disconnected)
                }
                Err(err) => return Err(Error::Read(err)),
            };

            if let Some(matched) = want(&buf[..n]) {
                return Ok(matched);
            }
        }
    }
}

fn probe_model(path: &Path) -> Option<Model> {
    let name = path.file_name()?.to_str()?;
    let uevent = Path::new("/sys/class/hidraw")
        .join(name)
        .join("device/uevent");
    parse_hid_id(&std::fs::read_to_string(uevent).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_bluetooth_attached_cooler() {
        let uevent = "DRIVER=hid-generic\nHID_ID=0005:000037D7:00001004\nHID_NAME=FlyDigi BS3PRO\n";
        assert_eq!(parse_hid_id(uevent), Some(Model::Bs3Pro));
    }

    #[test]
    fn matches_usb_attached_cooler() {
        let uevent = "HID_ID=0003:000037D7:00001002\n";
        assert_eq!(parse_hid_id(uevent), Some(Model::Bs2Pro));
    }

    #[test]
    fn ignores_other_vendors() {
        assert_eq!(parse_hid_id("HID_ID=0003:0000258A:000001A2\n"), None);
        assert_eq!(parse_hid_id("DRIVER=hid-generic\n"), None);
    }
}
