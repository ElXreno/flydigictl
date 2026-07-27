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

pub const CMD_SET_STANDBY: u8 = 0x0D;
pub const CMD_QUERY_SUPPLY: u8 = 0x07;
pub const CMD_SET_GEAR_RPM: u8 = 0x26;
pub const CMD_QUERY_GEAR_TABLE: u8 = 0x27;

pub const CMD_LIGHT_UPLOAD_BEGIN: u8 = 0x41;
pub const CMD_LIGHT_UPLOAD_BLOCK: u8 = 0x42;
pub const CMD_LIGHT_APPLY: u8 = 0x43;
pub const CMD_QUERY_STRIP: u8 = 0x45;
pub const CMD_LIGHT_POWER: u8 = 0x46;
pub const CMD_LIGHT_FRAME: u8 = 0x47;
pub const CMD_GEAR_LIGHT: u8 = 0x48;

/// The firmware accepts frame indices `0x00`-`0x12` only: index 0 is the
/// header, `0x01`-`0x11` carry 10 bytes each and `0x12` carries 6. Higher
/// indices are acknowledged and then discarded, so there is no point sending
/// the 30 frames the vendor app uploads.
const STEP_LEN: usize = 10;

/// The whole animation, header included: 18 steps of 10 bytes and a final one
/// of 6. `0x47` addresses these as indices `0x00`-`0x12` and discards anything
/// above, so the 30 steps the vendor app uploads are mostly thrown away. The
/// firmware stores exactly this much and flushes it to flash on apply.
const FULL_BUFFER_LEN: usize = 186;

/// A frame longer than 20 bytes never reaches the firmware.
///
/// Measured the same way on `0x42`, `0x47` and `0x27`: a payload of 15 bytes is
/// answered and 16 is met with silence, so the limit is the frame rather than
/// any one command. Twenty bytes is what fits in a Bluetooth LE write at the
/// default ATT MTU, which is also why the 65-byte reports THRM sends are
/// dropped without a word.
const MAX_PAYLOAD: usize = 15;

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

/// Fan mode from byte 6 of a status frame.
///
/// The byte is a bitfield, not an enum: bit 0 is the realtime override and bits
/// 2 and 3 carry the standby setting. Measured across every combination:
///
/// | standby | gear   | realtime |
/// |---------|--------|----------|
/// | off     | `0x02` | `0x03`   |
/// | instant | `0x06` | `0x07`   |
/// | delayed | `0x0a` | `0x0b`   |
///
/// The `0x04`/`0x05` pair documented for a BS2 Pro is the same thing with
/// instant standby enabled, so there is nothing model-specific here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Gear,
    Realtime,
}

const MODE_REALTIME: u8 = 0x01;
const MODE_STANDBY_INSTANT: u8 = 0x04;
const MODE_STANDBY_DELAYED: u8 = 0x08;

impl Mode {
    fn from_byte(b: u8) -> Self {
        if b & MODE_REALTIME != 0 {
            Self::Realtime
        } else {
            Self::Gear
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gear => write!(f, "gear"),
            Self::Realtime => write!(f, "realtime"),
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

impl Gear {
    /// The four gears in the order the device stores and cycles them.
    pub const ALL: [Self; 4] = [Self::Quiet, Self::Standard, Self::Strong, Self::Overclock];

    /// Index into the stored speed table.
    fn slot(self) -> Option<u8> {
        match self {
            Self::Quiet => Some(0),
            Self::Standard => Some(1),
            Self::Strong => Some(2),
            Self::Overclock => Some(3),
            Self::Unknown(_) => None,
        }
    }
}

impl std::str::FromStr for Gear {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "quiet" => Ok(Self::Quiet),
            "standard" => Ok(Self::Standard),
            "strong" => Ok(Self::Strong),
            "overclock" => Ok(Self::Overclock),
            _ => Err(()),
        }
    }
}

/// How much power the cooler is getting, which is what decides how fast it is
/// allowed to spin.
///
/// The firmware keeps this as a level of 1 to 3, updated whenever the supply
/// changes, and enforces it in two places: the control loop clamps every
/// target, realtime ones included, and a gear too high for the level is stored
/// but not applied. Both ceilings come straight from the disassembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supply {
    /// Bus power, typically a laptop USB port.
    Low,
    /// Enough for everything but the top gear.
    Medium,
    /// A PD adapter in the side port: the full range.
    Full,
    Unknown(u8),
}

impl Supply {
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Low,
            2 => Self::Medium,
            3 => Self::Full,
            other => Self::Unknown(other),
        }
    }

    /// The speed the firmware clamps to. An unrecognised level is treated as
    /// unrestricted: the cooler enforces its own limit anyway, and guessing low
    /// would cap a device we simply do not know.
    pub fn max_rpm(self) -> u16 {
        match self {
            Self::Low => 2700,
            Self::Medium => 3300,
            Self::Full | Self::Unknown(_) => MAX_RPM,
        }
    }

    /// The highest gear that can be selected right now.
    pub fn max_gear(self) -> Gear {
        match self {
            Self::Low => Gear::Standard,
            Self::Medium => Gear::Strong,
            Self::Full | Self::Unknown(_) => Gear::Overclock,
        }
    }

    pub fn allows(self, gear: Gear) -> bool {
        match (gear.slot(), self.max_gear().slot()) {
            (Some(wanted), Some(ceiling)) => wanted <= ceiling,
            _ => true,
        }
    }
}

impl std::fmt::Display for Supply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::Full => write!(f, "full"),
            Self::Unknown(n) => write!(f, "unknown({n})"),
        }
    }
}

/// The lighting the cooler is showing.
///
/// Nothing can be read back: the firmware has no query for any of this, so
/// whoever set it last is the only one who knows. The daemon therefore keeps
/// this and hands it to clients, instead of every client guessing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LightMode {
    #[default]
    Off,
    Static {
        color: Rgb,
    },
    Effect {
        effect: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Lighting {
    pub mode: LightMode,
    /// Percentage, and it applies to an animation as much as to a colour: the
    /// firmware keeps brightness in the same header byte either way.
    pub brightness: u8,
    /// The gear indicator LEDs, a separate light from the strip.
    pub indicators: bool,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            mode: LightMode::Off,
            brightness: 100,
            indicators: true,
        }
    }
}

impl std::fmt::Display for Lighting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.mode {
            LightMode::Off => write!(f, "off"),
            LightMode::Static { color } => write!(
                f,
                "#{:02x}{:02x}{:02x} at {}%",
                color.r, color.g, color.b, self.brightness
            ),
            LightMode::Effect { effect } => write!(f, "effect {effect} at {}%", self.brightness),
        }
    }
}

impl Lighting {
    /// Reports that take the cooler from `previous` to this.
    ///
    /// Only the difference is sent: re-uploading an animation to toggle the
    /// indicator LEDs would restart it for no reason.
    pub fn reports(&self, previous: Option<&Self>) -> Vec<[u8; REPORT_LEN]> {
        let mut reports = Vec::new();

        let strip_changed = previous.is_none_or(|previous| {
            previous.mode != self.mode || previous.brightness != self.brightness
        });

        if strip_changed {
            match self.mode {
                LightMode::Off => reports.push(light_off()),
                LightMode::Static { color } => {
                    reports.extend(light_static(color, self.brightness));
                }
                LightMode::Effect { effect } => {
                    reports.extend(light_effect(effect, self.brightness));
                }
            }
        }

        if previous.is_none_or(|previous| previous.indicators != self.indicators) {
            reports.push(gear_light(self.indicators));
        }

        reports
    }
}

/// Ask whether the side strip is powered.
///
/// The one piece of lighting the cooler will admit to. THRM sends `0x45` as if
/// it selected something, but the handler ignores its payload entirely and only
/// ever answers with the stored flag, so those reports did nothing.
pub fn query_strip() -> [u8; REPORT_LEN] {
    build_report(CMD_QUERY_STRIP, &[])
}

/// Ask how much power the cooler thinks it has.
pub fn query_supply() -> [u8; REPORT_LEN] {
    build_report(CMD_QUERY_SUPPLY, &[])
}

/// Ask for the four stored gear speeds.
pub fn query_gear_table() -> [u8; REPORT_LEN] {
    build_report(CMD_QUERY_GEAR_TABLE, &[])
}

/// Four little-endian speeds, in [`Gear::ALL`] order.
pub fn parse_gear_table(payload: &[u8]) -> Option<[u16; 4]> {
    if payload.len() < 8 {
        return None;
    }
    Some([
        u16::from_le_bytes([payload[0], payload[1]]),
        u16::from_le_bytes([payload[2], payload[3]]),
        u16::from_le_bytes([payload[4], payload[5]]),
        u16::from_le_bytes([payload[6], payload[7]]),
    ])
}

/// Store a speed for one gear.
///
/// Unlike a realtime target this is written into the cooler and survives
/// reconnects, so it also changes what the physical button cycles through. The
/// firmware only applies the gear right away if the supply allows it: on weak
/// power it refuses the top gears, which is where the "2700 rpm on laptop USB"
/// limit actually comes from.
pub fn set_gear_rpm(gear: Gear, rpm: u16) -> Option<[u8; REPORT_LEN]> {
    let slot = gear.slot()?;
    let [lo, hi] = rpm.to_le_bytes();
    Some(build_report(CMD_SET_GEAR_RPM, &[slot, lo, hi]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub current_rpm: u16,
    pub target_rpm: u16,
    pub mode: Mode,
    /// Standby setting the cooler reports, which is where it is actually
    /// stored: the daemon can check its config took effect.
    pub standby: Standby,
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

/// What the cooler does when the host disappears.
///
/// The firmware handles this itself: on a Bluetooth drop it powers down the fan
/// and both light sources, and on reconnect it wakes up and restores the gear
/// it had. Nothing needs to be re-sent, and nothing is written to our config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standby {
    /// Keep running unattended.
    Off,
    /// Sleep as soon as the link drops.
    Instant,
    /// Sleep a minute after the link drops (600 ticks of 100 ms).
    Delayed,
}

impl Standby {
    /// Read the standby setting back out of a status frame.
    fn from_mode_byte(b: u8) -> Self {
        if b & MODE_STANDBY_DELAYED != 0 {
            Self::Delayed
        } else if b & MODE_STANDBY_INSTANT != 0 {
            Self::Instant
        } else {
            Self::Off
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Instant => 1,
            Self::Delayed => 2,
        }
    }
}

impl std::str::FromStr for Standby {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "off" | "never" => Ok(Self::Off),
            "instant" | "immediate" => Ok(Self::Instant),
            "delayed" | "delay" => Ok(Self::Delayed),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for Standby {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => write!(f, "off"),
            Self::Instant => write!(f, "instant"),
            Self::Delayed => write!(f, "delayed"),
        }
    }
}

/// Persisted in the cooler, so it survives reboots of the host.
pub fn set_standby(standby: Standby) -> [u8; REPORT_LEN] {
    build_report(CMD_SET_STANDBY, &[standby.code()])
}

/// Turn the RGB strip off.
pub fn light_off() -> [u8; REPORT_LEN] {
    build_report(CMD_LIGHT_POWER, &[0x00])
}

/// Light the strip again with whatever it was last given.
///
/// The animation lives in the cooler's own flash, so power is all this takes:
/// nothing has to be uploaded to get the same pattern back.
pub fn light_on() -> [u8; REPORT_LEN] {
    build_report(CMD_LIGHT_POWER, &[0x01])
}

/// Number of built-in effects the firmware carries.
pub const EFFECT_COUNT: u8 = 5;

/// Size of the effect buffer: 18 frames of 10 bytes.
const BUFFER_LEN: usize = 180;

/// Rebuild the buffer the firmware would install for a built-in effect.
///
/// Asking the device for a preset with `0x44` only works while it is in
/// realtime mode, and leaving that mode runs `set_effect(0)`, which drops the
/// strip straight back to this buffer. Uploading the preset's own palette is
/// therefore the only way to keep it playing in gear mode - the byte patterns
/// below are transcribed from `FUN_ram_00005bdc`.
fn preset_buffer(effect: u8, brightness: u8) -> [u8; BUFFER_LEN] {
    let mut buf = [0u8; BUFFER_LEN];

    // Header: 00 02 00 <mode> <speed> <brightness>
    let (mode, speed) = match effect {
        1 => (0x03, 0x0A),
        2 => (0x03, 0x06),
        3 => (0x03, 0x02),
        4 => (0x01, 0x02),
        _ => (0x02, 0x0A),
    };
    buf[1] = 0x02;
    buf[3] = mode;
    buf[4] = speed;
    buf[5] = brightness.min(100);

    // Presets 1-3 paint four colours at three points of each 90-byte half;
    // only the hue differs between them.
    let ramp: Option<[[u8; 3]; 4]> = match effect {
        1 => Some([
            [0x00, 0xFF, 0x00],
            [0x3F, 0xFF, 0x3F],
            [0xFF, 0xFF, 0xFF],
            [0x3F, 0xFF, 0x3F],
        ]),
        2 => Some([
            [0xFF, 0xFF, 0x00],
            [0xFF, 0xFF, 0x3F],
            [0xFF, 0xFF, 0xFF],
            [0xFF, 0xFF, 0x3F],
        ]),
        3 => Some([
            [0xFF, 0x00, 0x00],
            [0xFF, 0x3F, 0x3F],
            [0xFF, 0xFF, 0xFF],
            [0xFF, 0x3F, 0x3F],
        ]),
        _ => None,
    };

    if let Some(colors) = ramp {
        for half in [0usize, 90] {
            for (group, start) in [6usize, 36, 66].iter().enumerate() {
                for slot in 0..4 {
                    // Each group rotates the palette by one position.
                    let color = colors[(slot + group) % 4];
                    let at = half + start + slot * 3;
                    buf[at..at + 3].copy_from_slice(&color);
                }
            }
        }
        return buf;
    }

    if effect == 4 {
        // Solid red repeated every 30 bytes.
        for base in (0..BUFFER_LEN).step_by(30) {
            for slot in 0..2 {
                let at = base + 6 + slot * 3;
                buf[at..at + 3].copy_from_slice(&[0xFF, 0x00, 0x00]);
            }
        }
        return buf;
    }

    // Preset 5 writes individual words rather than a repeating pattern.
    for (offset, bytes) in [
        (6usize, &[0xFF, 0xFF][..]),
        (8, &[0xFF, 0x1E, 0x1E, 0xA0][..]),
        (14, &[0x50][..]),
        (36, &[0x00, 0x00, 0x50, 0xFF][..]),
        (40, &[0xFF, 0xFF, 0x1E, 0x1E][..]),
        (44, &[0xA0][..]),
        (68, &[0x50, 0x1E, 0x1E, 0xA0][..]),
        (72, &[0xFF, 0xFF][..]),
        (74, &[0xFF][..]),
        (96, &[0xFF, 0xFF, 0xFF, 0x1E][..]),
        (100, &[0x1E, 0xA0][..]),
        (104, &[0x50][..]),
        (128, &[0x50, 0xFF, 0xFF, 0xFF][..]),
        (132, &[0x1E, 0x1E][..]),
        (134, &[0xA0][..]),
        (156, &[0x00, 0x00, 0x50, 0x1E][..]),
        (160, &[0x1E, 0xA0, 0xFF, 0xFF][..]),
        (164, &[0xFF][..]),
    ] {
        buf[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    buf
}

/// Send a whole animation buffer and switch the strip over to it.
///
/// `0x47` addresses one 10-byte step per report; `0x41` and `0x42` stream the
/// same memory instead. The device keeps a write cursor: `0x41` rewinds it and
/// each `0x42` appends its payload, so the buffer arrives in 10 reports rather
/// than 19. A block that would run past the end of the buffer is dropped
/// without an acknowledgement, which surfaces as a missing reply.
///
/// `0x43` then copies the buffer to flash behind a `2BGR` magic and selects it,
/// so the animation survives a power cut.
fn light_upload(buf: &[u8]) -> Vec<[u8; REPORT_LEN]> {
    let mut reports = vec![
        build_report(CMD_LIGHT_POWER, &[0x01]),
        build_report(CMD_LIGHT_UPLOAD_BEGIN, &[]),
    ];

    reports.extend(
        buf.chunks(MAX_PAYLOAD)
            .map(|chunk| build_report(CMD_LIGHT_UPLOAD_BLOCK, chunk)),
    );

    reports.push(build_report(CMD_LIGHT_APPLY, &[0x01]));
    reports
}

/// Upload a built-in effect's palette and play it.
///
/// Brightness lives in the same header byte the firmware's own presets use, so
/// dimming an animation does not mean giving it up for a static colour.
pub fn light_effect(effect: u8, brightness: u8) -> Vec<[u8; REPORT_LEN]> {
    light_upload(&preset_buffer(effect, brightness))
}

/// Toggle the gear indicator LEDs. Unlike the strip, this is a normal
/// 25-byte control report.
pub fn gear_light(on: bool) -> [u8; REPORT_LEN] {
    build_report(CMD_GEAR_LIGHT, &[u8::from(on)])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    let mut buf = [0u8; FULL_BUFFER_LEN];

    buf[..STEP_LEN].copy_from_slice(&[
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
    ]);

    let lit = color.scaled(brightness);
    for step in LIT_STEPS {
        let at = (step + 1) * STEP_LEN;
        buf[at + 6] = lit.r;
        buf[at + 7] = lit.g;
        buf[at + 8] = lit.b;
    }

    light_upload(&buf)
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
        standby: Standby::from_mode_byte(payload[1]),
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
        assert_eq!(status.standby, Standby::Off);
        assert_eq!(status.max_gear, Gear::Overclock);
        assert_eq!(status.gear, Gear::Quiet);
        assert_eq!(status.seq, 0x23B4);
    }

    #[test]
    fn mode_byte_is_a_bitfield() {
        // Measured on hardware across every standby setting.
        for (byte, mode, standby) in [
            (0x02, Mode::Gear, Standby::Off),
            (0x03, Mode::Realtime, Standby::Off),
            (0x06, Mode::Gear, Standby::Instant),
            (0x07, Mode::Realtime, Standby::Instant),
            (0x0a, Mode::Gear, Standby::Delayed),
            (0x0b, Mode::Realtime, Standby::Delayed),
        ] {
            assert_eq!(Mode::from_byte(byte), mode, "mode of 0x{byte:02x}");
            assert_eq!(
                Standby::from_mode_byte(byte),
                standby,
                "standby of 0x{byte:02x}"
            );
        }
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

    /// Every report this crate can produce, for the checks that have to hold
    /// for all of them. Built once here so a new command has to be added to
    /// the list to compile, rather than quietly escaping the bounds below.
    fn every_report() -> Vec<(&'static str, [u8; REPORT_LEN])> {
        let lighting = Lighting {
            mode: LightMode::Static {
                color: Rgb { r: 9, g: 200, b: 7 },
            },
            brightness: 63,
            indicators: true,
        };

        let mut reports: Vec<(&'static str, [u8; REPORT_LEN])> = vec![
            ("enter_realtime", enter_realtime()),
            ("exit_realtime", exit_realtime()),
            ("set_realtime_rpm", set_realtime_rpm(2600)),
            ("stop", set_realtime_rpm(STOP_RPM)),
            ("query_supply", query_supply()),
            ("query_strip", query_strip()),
            ("query_gear_table", query_gear_table()),
            ("light_off", light_off()),
            ("light_on", light_on()),
            ("gear_light", gear_light(true)),
            ("set_standby", set_standby(Standby::Delayed)),
        ];

        reports.push((
            "set_gear_rpm",
            set_gear_rpm(Gear::Standard, 2000).expect("a real gear has a slot"),
        ));

        reports.extend(
            light_static(Rgb { r: 1, g: 2, b: 3 }, 100)
                .into_iter()
                .map(|report| ("light_static", report)),
        );
        reports.extend(
            light_effect(5, 50)
                .into_iter()
                .map(|report| ("light_effect", report)),
        );
        reports.extend(
            lighting
                .reports(None)
                .into_iter()
                .map(|report| ("lighting", report)),
        );

        reports
    }

    /// The firmware's own formula, written out again rather than reusing
    /// `checksum`: a test that calls the code it is checking proves nothing.
    #[test]
    fn checksum_matches_the_firmware_formula() {
        for (name, report) in every_report() {
            let len = report[4] as usize;
            let payload = &report[5..5 + (len - 2)];

            let expected = (u32::from(report[3])
                + u32::from(report[4])
                + payload.iter().map(|byte| u32::from(*byte)).sum::<u32>())
                & 0xFF;

            assert_eq!(
                u32::from(report[5 + (len - 2)]),
                expected,
                "checksum of {name}"
            );
        }
    }

    /// The HID path rejects a frame whose length byte is above 0x11 without a
    /// word of complaint, so a report that grows past it would simply stop
    /// working. Also the report id and magic, which gate it even earlier.
    #[test]
    fn every_report_stays_inside_the_firmware_bounds() {
        for (name, report) in every_report() {
            assert_eq!(report[0], REPORT_ID_OUT, "report id of {name}");
            assert_eq!(&report[1..3], &MAGIC, "magic of {name}");

            let len = report[4];
            assert!(len >= 2, "{name} declares a length below the header");
            assert!(len <= 0x11, "{name} declares {len}, past the 0x11 ceiling");
            assert!(
                usize::from(len) - 2 <= MAX_PAYLOAD,
                "{name} carries more than {MAX_PAYLOAD} payload bytes"
            );
        }
    }

    /// Three commands in the firmware are destructive or corrupting, and none
    /// of them has a use here: 0xDF erases the firmware and reboots into the
    /// ROM loader, 0x06 is a factory reset, and 0x08 with an out-of-range gear
    /// corrupts the stored table. Nothing this crate builds may be one.
    #[test]
    fn never_builds_a_dangerous_command() {
        for (name, report) in every_report() {
            assert!(
                !matches!(report[3], 0xDF | 0x06 | 0x08),
                "{name} builds command 0x{:02X}",
                report[3]
            );
        }
    }

    /// `0x26` takes a slot, and the firmware answers `00` for anything above
    /// three rather than clamping, so an unknown gear must never reach it.
    #[test]
    fn gear_writes_carry_a_slot_the_firmware_accepts() {
        for gear in Gear::ALL {
            let slot = gear.slot().expect("every listed gear has a slot");
            assert!(slot <= 3, "{gear} maps to slot {slot}");

            let report = set_gear_rpm(gear, 2000).expect("a real gear builds a report");
            assert_eq!(report[3], CMD_SET_GEAR_RPM);
            assert_eq!(report[5], slot);
        }

        assert!(
            set_gear_rpm(Gear::Unknown(9), 2000).is_none(),
            "an unknown gear must not be written"
        );
    }
}
