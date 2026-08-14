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

/// How the cooler is attached, which decides how a report is framed.
///
/// Over Bluetooth the report descriptor declares ids, so the kernel prepends
/// one and the reports are 25 bytes. Over USB it declares none: writes are 31
/// bytes starting at the magic, and reads are 32 starting at a constant 0x03
/// that belongs to the firmware rather than to HID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    Bluetooth,
}

impl Transport {
    fn from_bus(bus: u32) -> Self {
        match bus {
            0x03 => Self::Usb,
            _ => Self::Bluetooth,
        }
    }

    /// Bytes a written report must have, and where its magic starts.
    fn write_shape(self) -> (usize, usize) {
        match self {
            Self::Usb => (31, 1),
            Self::Bluetooth => (protocol::REPORT_LEN, 0),
        }
    }
}

pub struct Found {
    pub path: PathBuf,
    pub model: Model,
    pub transport: Transport,
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

    let mut found: Vec<Found> = names
        .iter()
        .filter_map(|name| {
            let uevent = Path::new("/sys/class/hidraw")
                .join(name)
                .join("device/uevent");
            let text = std::fs::read_to_string(&uevent).ok()?;
            let (model, transport) = parse_hid_id(&text)?;
            debug!("{name}: matched {} over {transport:?}", model.name());
            Some(Found {
                path: PathBuf::from("/dev").join(name),
                model,
                transport,
            })
        })
        .collect();

    // The wired path first: it survives the power renegotiation that drops the
    // Bluetooth link, and a cooler plugged into this machine is on both at
    // once. Names are no guide - which node is which changes across reboots.
    found.sort_by_key(|found| match found.transport {
        Transport::Usb => 0,
        Transport::Bluetooth => 1,
    });
    found
}

/// Extract the model from a `HID_ID=bus:vendor:product` line.
fn parse_hid_id(uevent: &str) -> Option<(Model, Transport)> {
    let hid_id = uevent
        .lines()
        .find_map(|line| line.strip_prefix("HID_ID="))?;

    let mut parts = hid_id.split(':');
    let bus = u32::from_str_radix(parts.next()?, 16).ok()?;
    let vendor = u32::from_str_radix(parts.next()?, 16).ok()?;
    let product = u32::from_str_radix(parts.next()?, 16).ok()?;

    if vendor != VID as u32 {
        return None;
    }
    Some((Model::from_pid(product as u16)?, Transport::from_bus(bus)))
}

pub struct Device {
    file: File,
    pub model: Model,
    pub path: PathBuf,
    pub transport: Transport,
}

impl Device {
    /// Open an explicit hidraw path, or the first cooler found.
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let found = match path {
            Some(path) => Found {
                path: path.to_path_buf(),
                model: probe_model(path).unwrap_or(Model::Bs3Pro),
                transport: transport_of(path).unwrap_or(Transport::Bluetooth),
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
            transport: found.transport,
        })
    }

    /// Write a report, reshaped for the transport this cooler is on.
    ///
    /// Reports are built in the Bluetooth shape everywhere else; the USB path
    /// wants the same bytes without the leading report id and padded to its
    /// own length, which is what the descriptor asks for.
    pub fn send(&mut self, report: impl AsRef<[u8]>) -> Result<()> {
        let report = report.as_ref();
        let (len, from) = self.transport.write_shape();

        let mut framed = vec![0u8; len];
        let body = &report[from..];
        let end = body.len().min(len);
        framed[..end].copy_from_slice(&body[..end]);

        self.file.write_all(&framed).map_err(Error::Write)
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
    Some(probe(path)?.0)
}

fn transport_of(path: &Path) -> Option<Transport> {
    Some(probe(path)?.1)
}

fn probe(path: &Path) -> Option<(Model, Transport)> {
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
        assert_eq!(
            parse_hid_id(uevent),
            Some((Model::Bs3Pro, Transport::Bluetooth))
        );
    }

    #[test]
    fn matches_usb_attached_cooler() {
        let uevent = "HID_ID=0003:000037D7:00001002\n";
        assert_eq!(parse_hid_id(uevent), Some((Model::Bs2Pro, Transport::Usb)));
    }

    /// A cooler plugged into this machine appears on both transports at once,
    /// and the wired one is the one to talk to. Node names say nothing: which
    /// number each lands on changes from boot to boot.
    #[test]
    fn a_wired_cooler_outranks_the_same_one_over_bluetooth() {
        let mut found = [
            Found {
                path: PathBuf::from("/dev/hidraw9"),
                model: Model::Bs3Pro,
                transport: Transport::Bluetooth,
            },
            Found {
                path: PathBuf::from("/dev/hidraw0"),
                model: Model::Bs3Pro,
                transport: Transport::Usb,
            },
        ];

        found.sort_by_key(|found| match found.transport {
            Transport::Usb => 0,
            Transport::Bluetooth => 1,
        });

        assert_eq!(found[0].transport, Transport::Usb);
    }

    /// Written reports are built in the Bluetooth shape; the USB path drops the
    /// report id and pads to its own length.
    #[test]
    fn usb_writes_start_at_the_magic() {
        let report = protocol::query_supply();
        assert_eq!(report[0], protocol::REPORT_ID_OUT);

        let (len, from) = Transport::Usb.write_shape();
        assert_eq!(len, 31);
        assert_eq!(&report[from..from + 2], &[0x5A, 0xA5]);

        let (len, from) = Transport::Bluetooth.write_shape();
        assert_eq!((len, from), (protocol::REPORT_LEN, 0));
    }

    #[test]
    fn ignores_other_vendors() {
        assert_eq!(parse_hid_id("HID_ID=0003:0000258A:000001A2\n"), None);
        assert_eq!(parse_hid_id("DRIVER=hid-generic\n"), None);
    }
}
