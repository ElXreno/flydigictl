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
    /// The configuration in force.
    GetConfig,
    /// Replace the configuration.
    SetConfig { config: Config },
    /// Hold a fixed speed; `null` returns to the curve.
    SetManual { rpm: Option<u16> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Status(Status),
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
