//! Newline-delimited JSON over a unix socket.
//!
//! Deliberately boring: a GUI in any language can speak it, and it debugs with
//! `socat - UNIX-CONNECT:/run/flydigictl/flydigictl.sock`.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::curve::Demand;

pub const DEFAULT_SOCKET: &str = "/run/flydigictl/flydigictl.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Current fan state as the daemon sees it.
    Status,
    /// Turn this connection into a stream of [`Reply::Status`].
    ///
    /// The daemon writes one every time its picture of the cooler changes and
    /// nothing in between, so a client sees new speeds as fast as the cooler
    /// reports them without asking for them. No further requests are read on a
    /// subscribed connection: open a second one for those.
    Subscribe,
    /// The configuration in force.
    GetConfig,
    /// Replace the configuration.
    SetConfig { config: Config },
    /// Hold a fixed speed; `null` returns to the curve.
    SetManual { rpm: Option<u16> },
    /// The four speeds stored in the cooler.
    Gears,
    /// Rewrite one of them. Named rather than numbered so a client does not
    /// have to know the order the firmware keeps them in.
    SetGear { gear: String, rpm: u16 },
    /// Drive the lighting.
    Light { light: Light },
    /// What the cooler should do on its own once the host goes away.
    SetStandby { standby: crate::protocol::Standby },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "light", rename_all = "snake_case")]
pub enum Light {
    /// Turn the side strip off.
    Off,
    /// Paint it a single colour, `rrggbb` with or without a leading hash.
    Static { color: String, brightness: u8 },
    /// Play one of the firmware's own animations, 1 to 5.
    Effect { mode: u8 },
    /// The gear indicator LEDs, which are a separate light entirely.
    Indicators { on: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Status(Status),
    Gears {
        gears: Vec<Gear>,
    },
    Config {
        config: Config,
        /// False when the daemon cannot persist changes, e.g. a NixOS store path.
        writable: bool,
    },
    Ok {
        /// Set when a change was applied but could not be written to disk.
        warning: Option<Warning>,
    },
    Error {
        message: String,
    },
}

/// One stored gear speed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gear {
    pub name: String,
    pub rpm: u16,
    /// False when the supply cannot carry this gear, so the cooler stores the
    /// speed but refuses to run it.
    pub allowed: bool,
}

/// A warning carries a stable code alongside its text.
///
/// The text names the config path, and on NixOS that path changes on every
/// rebuild - so a client that wants to show each warning once must dedupe on
/// `code`, never on `message`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    pub code: WarningCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    /// The config cannot be written: changes are live but not persisted.
    ConfigReadOnly,
    /// The config is writable in principle, but saving failed.
    ConfigSaveFailed,
    /// The cooler's power supply caps the speed below what was asked for.
    SupplyLimited,
}

impl Status {
    /// What subscribers are told while no cooler is attached.
    ///
    /// Sent rather than nothing so a client can tell "the cooler is unplugged"
    /// from "the daemon died", which look identical if silence is the only
    /// signal.
    pub fn disconnected() -> Self {
        Self {
            model: String::new(),
            connected: false,
            temp_c: None,
            current_rpm: None,
            target_rpm: None,
            manual: false,
            supply: None,
            supply_max_rpm: None,
            leading: None,
            demands: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub model: String,
    pub connected: bool,
    pub temp_c: Option<u8>,
    pub current_rpm: Option<u16>,
    pub target_rpm: Option<u16>,
    pub manual: bool,

    /// What the cooler is powered by: `low`, `medium`, `full` or unknown.
    pub supply: Option<String>,

    /// The speed the supply allows. A target above this is clamped by the
    /// firmware, so a client showing the curve should show this line too.
    pub supply_max_rpm: Option<u16>,

    /// Curve currently setting the speed, so a client can say *why* it is loud.
    pub leading: Option<String>,

    /// Every curve's reading and demand, for graphs and debugging.
    pub demands: Vec<Demand>,
}
