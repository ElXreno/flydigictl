//! Wire format of the Flydigi BS series HID protocol.
//!
//! Frames are `5A A5 <cmd> <len> <payload...> <checksum>`, wrapped in a HID
//! report padded to 25 bytes. Reports the device sends use report ID `0x01`,
//! reports we send use `0x02`.

pub const VID: u16 = 0x37D7;

pub const REPORT_ID_IN: u8 = 0x01;
pub const REPORT_ID_OUT: u8 = 0x02;

/// Every report is 25 bytes, lighting included. THRM pads lighting reports to
/// 65 bytes, which a BS3 Pro ignores without so much as an error.
pub const REPORT_LEN: usize = 25;

const MAGIC: [u8; 2] = [0x5A, 0xA5];

pub const CMD_SET_REALTIME_RPM: u8 = 0x21;
pub const CMD_ENTER_REALTIME: u8 = 0x23;
pub const CMD_EXIT_REALTIME: u8 = 0x24;
pub const CMD_STATUS_NOTIFY: u8 = 0xEF;

pub const CMD_LIGHT_APPLY: u8 = 0x43;
pub const CMD_LIGHT_EFFECT: u8 = 0x44;
pub const CMD_LIGHT_SELECT: u8 = 0x45;
pub const CMD_LIGHT_POWER: u8 = 0x46;
pub const CMD_LIGHT_FRAME: u8 = 0x47;
pub const CMD_GEAR_LIGHT: u8 = 0x48;

/// The firmware accepts frame indices `0x00`-`0x12` only: index 0 is the
/// header, `0x01`-`0x11` carry 10 bytes each and `0x12` carries 6. Higher
/// indices are acknowledged and then discarded, so there is no point sending
/// the 30 frames the vendor app uploads.
const LAST_STEP: usize = 0x12;
const SHORT_STEP: usize = 6;
const STEP_LEN: usize = 10;

/// Steps that actually drive an LED.
const LIT_STEPS: [usize; 5] = [2, 5, 8, 11, 14];

/// Firmware clamps neither end, so the app has to.
///
/// 4000 RPM is the rated ceiling of a BS3 Pro (and a BS2 Pro). Reaching it also
/// needs a 9V/3A PD adapter in the side USB-C port - powered from a laptop USB
/// port the cooler stays at its level 2 gear, 2700 RPM.
///
/// The low end was measured: 500 RPM holds steady, while 100-400 make the fan
/// stall and the tachometer flip between 0 and 400. A target of exactly 0 is
/// honoured and stops the fan.
pub const MIN_RPM: u16 = 500;
pub const MAX_RPM: u16 = 4000;

/// Stopping the fan is the one target below [`MIN_RPM`] the device handles
/// cleanly.
pub const STOP_RPM: u16 = 0;

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

/// Turn the RGB strip off.
pub fn light_off() -> [u8; REPORT_LEN] {
    build_report(CMD_LIGHT_POWER, &[0x00])
}

/// Highest built-in effect the firmware implements.
pub const MAX_EFFECT: u8 = 5;

/// Select a lighting effect.
///
/// Mode 0 plays the uploaded buffer; 1 to [`MAX_EFFECT`] are presets baked into
/// the firmware. The presets are gated: the handler bails out unless the fan is
/// in realtime mode, and it acknowledges the command either way, so a silent
/// no-op is indistinguishable from success on the wire.
pub fn light_effect(mode: u8) -> Vec<[u8; REPORT_LEN]> {
    vec![
        build_report(CMD_LIGHT_POWER, &[0x01]),
        build_report(CMD_LIGHT_SELECT, &[]),
        build_report(CMD_LIGHT_SELECT, &[0x01]),
        build_report(CMD_LIGHT_EFFECT, &[mode]),
    ]
}

/// Toggle the gear indicator LEDs. Unlike the strip, this is a normal
/// 25-byte control report.
pub fn gear_light(on: bool) -> [u8; REPORT_LEN] {
    build_report(CMD_GEAR_LIGHT, &[u8::from(on)])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    fn scaled(self, brightness: u8) -> Self {
        let factor = f32::from(brightness.min(100)) / 100.0;
        Self {
            r: (f32::from(self.r) * factor) as u8,
            g: (f32::from(self.g) * factor) as u8,
            b: (f32::from(self.b) * factor) as u8,
        }
    }
}

/// Paint the strip a single colour.
///
/// The strip renders an animation held in device memory, so a static colour is
/// an animation whose lit steps all carry the same colour. The upload is a
/// header step followed by 30 animation steps, and nothing shows until the
/// final apply switches the strip over to the buffer.
pub fn light_static(color: Rgb, brightness: u8) -> Vec<[u8; REPORT_LEN]> {
    let mut reports = vec![
        build_report(CMD_LIGHT_POWER, &[0x01]),
        build_report(CMD_LIGHT_SELECT, &[]),
        build_report(CMD_LIGHT_SELECT, &[0x01]),
    ];

    let header = [
        0x00,
        0x02,
        0x00,
        0x00,
        LIGHT_SPEED_MEDIUM,
        brightness.min(100),
        color.r,
        color.g,
        color.b,
        0x00,
    ];
    reports.push(build_report(
        CMD_LIGHT_FRAME,
        &[&[0x00][..], &header[..]].concat(),
    ));

    let lit = color.scaled(brightness);
    for index in 1..=LAST_STEP {
        let mut data = [0u8; STEP_LEN];
        if LIT_STEPS.contains(&(index - 1)) {
            data[6] = lit.r;
            data[7] = lit.g;
            data[8] = lit.b;
        }
        let len = if index == LAST_STEP {
            SHORT_STEP
        } else {
            STEP_LEN
        };
        reports.push(build_report(
            CMD_LIGHT_FRAME,
            &[&[index as u8][..], &data[..len]].concat(),
        ));
    }

    reports.push(build_report(CMD_LIGHT_APPLY, &[0x01]));
    reports
}

const LIGHT_SPEED_MEDIUM: u8 = 0x0A;

/// A frame the device sent us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub cmd: u8,
    pub payload: &'a [u8],
}

/// Validate an incoming report and split out its command and payload.
/// Returns `None` for malformed frames and checksum mismatches.
pub fn parse_frame(buf: &[u8]) -> Option<Frame<'_>> {
    if buf.len() < 6 || buf[0] != REPORT_ID_IN || buf[1..3] != MAGIC {
        return None;
    }

    let cmd = buf[3];
    let payload_len = (buf[4] as usize).checked_sub(2)?;
    let payload = buf.get(5..5 + payload_len)?;
    if *buf.get(5 + payload_len)? != checksum(cmd, payload) {
        return None;
    }

    Some(Frame { cmd, payload })
}

/// Commands are acknowledged by echoing the command back with a status byte.
/// Every acknowledgement observed so far carries `0x01`.
pub fn parse_ack(buf: &[u8]) -> Option<Frame<'_>> {
    let frame = parse_frame(buf)?;
    (frame.cmd != CMD_STATUS_NOTIFY).then_some(frame)
}

/// Parse a status notification. Returns `None` for any other frame.
pub fn parse_status(buf: &[u8]) -> Option<Status> {
    let Frame { cmd, payload } = parse_frame(buf)?;
    if cmd != CMD_STATUS_NOTIFY || payload.len() < 11 {
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
    fn parses_captured_ack() {
        // Reply to `light off`, captured from a BS3 Pro.
        let raw = [
            0x01, 0x5A, 0xA5, 0x46, 0x03, 0x01, 0x4A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let ack = parse_ack(&raw).expect("ack should parse");
        assert_eq!(ack.cmd, CMD_LIGHT_POWER);
        assert_eq!(ack.payload, &[0x01]);
    }

    #[test]
    fn telemetry_is_not_an_ack() {
        assert!(parse_ack(&SAMPLE).is_none());
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
