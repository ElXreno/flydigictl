//! Wire format of the Flydigi BS series HID protocol.
//!
//! Frames are `5A A5 <cmd> <len> <payload...> <checksum>`, wrapped in a HID
//! report padded to 25 bytes. Reports the device sends use report ID `0x01`,
//! reports we send use `0x02`.

pub const VID: u16 = 0x37D7;

pub const REPORT_ID_IN: u8 = 0x01;
pub const REPORT_ID_OUT: u8 = 0x02;
pub const REPORT_LEN: usize = 25;

const MAGIC: [u8; 2] = [0x5A, 0xA5];

pub const CMD_SET_REALTIME_RPM: u8 = 0x21;
pub const CMD_ENTER_REALTIME: u8 = 0x23;
pub const CMD_EXIT_REALTIME: u8 = 0x24;
pub const CMD_STATUS_NOTIFY: u8 = 0xEF;

/// Firmware clamps neither end, so the app has to.
///
/// 4000 RPM is the rated ceiling of a BS3 Pro (and a BS2 Pro). Reaching it also
/// needs a 9V/3A PD adapter in the side USB-C port - powered from a laptop USB
/// port the cooler stays at its level 2 gear, 2700 RPM.
pub const MIN_RPM: u16 = 800;
pub const MAX_RPM: u16 = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    Bs1,
    Bs2Pro,
    Bs3,
    Bs3Pro,
}

impl Model {
    pub fn from_pid(pid: u16) -> Option<Self> {
        match pid {
            0x1001 => Some(Self::Bs1),
            0x1002 => Some(Self::Bs2Pro),
            0x1003 => Some(Self::Bs3),
            0x1004 => Some(Self::Bs3Pro),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bs1 => "BS1",
            Self::Bs2Pro => "BS2 Pro",
            Self::Bs3 => "BS3",
            Self::Bs3Pro => "BS3 Pro",
        }
    }
}

/// Fan mode reported in byte 6 of a status frame.
///
/// BS3 Pro reports `0x02`/`0x03` here; BS2 Pro is documented as `0x04`/`0x05`,
/// so treat unknown values as opaque rather than rejecting the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Gear,
    Realtime,
    Unknown(u8),
}

impl Mode {
    fn from_byte(b: u8) -> Self {
        match b {
            0x02 | 0x04 => Self::Gear,
            0x03 | 0x05 => Self::Realtime,
            other => Self::Unknown(other),
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gear => write!(f, "gear"),
            Self::Realtime => write!(f, "realtime"),
            Self::Unknown(b) => write!(f, "unknown(0x{b:02x})"),
        }
    }
}

/// Gear, encoded in the two nibbles of byte 5.
///
/// The selected gear and the ceiling use different encodings: `8/A/C/E` for the
/// former, `2/4/6` for the latter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gear {
    Quiet,
    Standard,
    Strong,
    Overclock,
    Unknown(u8),
}

impl Gear {
    fn selected(nibble: u8) -> Self {
        match nibble {
            0x8 => Self::Quiet,
            0xA => Self::Standard,
            0xC => Self::Strong,
            0xE => Self::Overclock,
            other => Self::Unknown(other),
        }
    }

    fn ceiling(nibble: u8) -> Self {
        match nibble {
            0x2 => Self::Standard,
            0x4 => Self::Strong,
            0x6 => Self::Overclock,
            other => Self::Unknown(other),
        }
    }
}

impl std::fmt::Display for Gear {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quiet => write!(f, "quiet"),
            Self::Standard => write!(f, "standard"),
            Self::Strong => write!(f, "strong"),
            Self::Overclock => write!(f, "overclock"),
            Self::Unknown(n) => write!(f, "unknown(0x{n:x})"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub current_rpm: u16,
    pub target_rpm: u16,
    pub mode: Mode,
    /// Highest gear the device will use.
    pub max_gear: Gear,
    /// Currently selected gear.
    pub gear: Gear,
    /// Monotonic counter, wraps; useful for spotting dropped frames.
    pub seq: u16,
}

pub fn checksum(cmd: u8, payload: &[u8]) -> u8 {
    let mut sum = cmd as u16 + (2 + payload.len()) as u16;
    for b in payload {
        sum += *b as u16;
    }
    (sum & 0xFF) as u8
}

/// Build a padded output report ready to be written to hidraw.
pub fn build_report(cmd: u8, payload: &[u8]) -> [u8; REPORT_LEN] {
    let mut report = [0u8; REPORT_LEN];
    report[0] = REPORT_ID_OUT;
    report[1..3].copy_from_slice(&MAGIC);
    report[3] = cmd;
    report[4] = (2 + payload.len()) as u8;
    report[5..5 + payload.len()].copy_from_slice(payload);
    report[5 + payload.len()] = checksum(cmd, payload);
    report
}

pub fn set_realtime_rpm(rpm: u16) -> [u8; REPORT_LEN] {
    build_report(CMD_SET_REALTIME_RPM, &rpm.to_le_bytes())
}

pub fn enter_realtime() -> [u8; REPORT_LEN] {
    build_report(CMD_ENTER_REALTIME, &[])
}

pub fn exit_realtime() -> [u8; REPORT_LEN] {
    build_report(CMD_EXIT_REALTIME, &[])
}

/// Parse a status notification. Returns `None` for other frames, malformed
/// frames, or a checksum mismatch.
pub fn parse_status(buf: &[u8]) -> Option<Status> {
    if buf.len() < 17 || buf[0] != REPORT_ID_IN || buf[1..3] != MAGIC {
        return None;
    }
    if buf[3] != CMD_STATUS_NOTIFY {
        return None;
    }

    let len = buf[4] as usize;
    let payload_len = len.checked_sub(2)?;
    let payload = buf.get(5..5 + payload_len)?;
    if *buf.get(5 + payload_len)? != checksum(CMD_STATUS_NOTIFY, payload) {
        return None;
    }
    if payload_len < 11 {
        return None;
    }

    Some(Status {
        max_gear: Gear::ceiling(payload[0] >> 4),
        gear: Gear::selected(payload[0] & 0x0F),
        mode: Mode::from_byte(payload[1]),
        current_rpm: u16::from_le_bytes([payload[3], payload[4]]),
        target_rpm: u16::from_le_bytes([payload[5], payload[6]]),
        seq: u16::from_le_bytes([payload[9], payload[10]]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame captured from a BS3 Pro over Bluetooth.
    const SAMPLE: [u8; 25] = [
        0x01, 0x5A, 0xA5, 0xEF, 0x0D, 0x68, 0x03, 0x05, 0x6C, 0x07, 0x6C, 0x07, 0x01, 0x01, 0xB4,
        0x23, 0x2B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn parses_captured_status() {
        let status = parse_status(&SAMPLE).expect("sample frame should parse");
        assert_eq!(status.current_rpm, 1900);
        assert_eq!(status.target_rpm, 1900);
        assert_eq!(status.mode, Mode::Realtime);
        assert_eq!(status.max_gear, Gear::Overclock);
        assert_eq!(status.gear, Gear::Quiet);
        assert_eq!(status.seq, 0x23B4);
    }

    #[test]
    fn rejects_corrupted_checksum() {
        let mut frame = SAMPLE;
        frame[16] ^= 0xFF;
        assert!(parse_status(&frame).is_none());
    }

    #[test]
    fn builds_realtime_commands_verified_on_hardware() {
        assert_eq!(&enter_realtime()[..5], &[0x02, 0x5A, 0xA5, 0x23, 0x02]);
        assert_eq!(enter_realtime()[5], 0x25);

        let set = set_realtime_rpm(2600);
        assert_eq!(&set[..7], &[0x02, 0x5A, 0xA5, 0x21, 0x04, 0x28, 0x0A]);
        assert_eq!(set[7], 0x57);

        assert_eq!(exit_realtime()[5], 0x26);
    }
}
