//! Newline-delimited JSON over a unix socket.
//!
//! Deliberately boring: a GUI in any language can speak it, and it debugs with
//! `socat - UNIX-CONNECT:/run/flydigictl/flydigictl.sock`.

use serde::{Deserialize, Serialize};

use crate::config::Config;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub model: String,
    pub connected: bool,
    pub temp_c: Option<u8>,
    pub current_rpm: Option<u16>,
    pub target_rpm: Option<u16>,
    pub manual: bool,
}
