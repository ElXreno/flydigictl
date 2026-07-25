use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "no cooler found (check bluetooth pairing and udev rules)"
    )]
    NotFound,

    #[error("cannot open {path}: {source}")]
    Open {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("read failed: {0}")]
    Read(std::io::Error),

    #[error("write failed: {0}")]
    Write(std::io::Error),

    #[error("poll failed: {0}")]
    Poll(nix::errno::Errno),

    #[error("timed out waiting for status")]
    Timeout,

    #[error("device went away")]
    Disconnected,

    #[error("rpm {rpm} out of range ({min}-{max}, or 0 to stop)")]
    RpmOutOfRange { rpm: u16, min: u16, max: u16 },

    #[error("bad colour: {0}")]
    BadColor(String),

    #[error("no effect {mode}, firmware has 0-{max}")]
    UnknownEffect { mode: u8, max: u8 },

    #[error(
        "built-in effects need realtime mode, set a speed first"
    )]
    NeedsRealtime,

    #[error("expected on or off, got {0}")]
    BadArgument(String),

    #[error("command 0x{cmd:02x} not acknowledged")]
    NoAck { cmd: u8 },
}

pub type Result<T> = std::result::Result<T, Error>;
