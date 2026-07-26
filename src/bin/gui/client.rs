//! Blocking client for the daemon socket.
//!
//! Every call opens its own connection. Requests are rare and answered from a
//! snapshot, so a blocking round trip on the interface thread is cheaper than
//! carrying an async runtime around, and reconnecting per call means a
//! restarted daemon needs no recovery logic here. Updates are the exception:
//! they arrive on a subscription that a reader thread owns.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flydigictl::config::Config;
use flydigictl::ipc::{Gear, Reply, Request, SensorInfo, Status, Warning};
use flydigictl::protocol::{Lighting, Standby};

/// Long enough for a daemon busy talking to the cooler, short enough that a
/// wedged one cannot freeze the window.
const TIMEOUT: Duration = Duration::from_millis(500);

/// Lighting is a burst of twenty-odd reports the daemon sends one at a time,
/// so its answer is slow by construction rather than by fault.
const LIGHT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct Client {
    path: PathBuf,
}

impl Client {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn socket(&self) -> &Path {
        &self.path
    }

    /// Open a connection that the daemon pushes updates down.
    ///
    /// Read timeouts are off here on purpose: an idle cooler produces no
    /// updates, and a timeout would look exactly like a dead daemon.
    pub fn subscribe(&self) -> Result<Updates, String> {
        let stream = UnixStream::connect(&self.path).map_err(connect_fault)?;
        stream.set_write_timeout(Some(TIMEOUT)).map_err(fault)?;

        let mut writer = stream.try_clone().map_err(fault)?;
        let mut line = serde_json::to_string(&Request::Subscribe).map_err(fault)?;
        line.push('\n');
        writer.write_all(line.as_bytes()).map_err(fault)?;

        Ok(Updates {
            lines: BufReader::new(stream).lines(),
        })
    }

    /// The config plus whether the daemon can write it back.
    pub fn config(&self) -> Result<(Config, bool), String> {
        match self.request(&Request::GetConfig)? {
            Reply::Config { config, writable } => Ok((config, writable)),
            other => Err(unexpected(&other)),
        }
    }

    pub fn set_config(&self, config: Config) -> Result<Option<Warning>, String> {
        self.acknowledged(&Request::SetConfig { config })
    }

    pub fn set_manual(&self, rpm: Option<u16>) -> Result<Option<Warning>, String> {
        self.acknowledged(&Request::SetManual { rpm })
    }

    pub fn sensors(&self) -> Result<Vec<SensorInfo>, String> {
        match self.request(&Request::Sensors)? {
            Reply::Sensors { sensors } => Ok(sensors),
            other => Err(unexpected(&other)),
        }
    }

    pub fn gears(&self) -> Result<Vec<Gear>, String> {
        match self.request(&Request::Gears)? {
            Reply::Gears { gears } => Ok(gears),
            other => Err(unexpected(&other)),
        }
    }

    pub fn set_gear(&self, gear: &str, rpm: u16) -> Result<Option<Warning>, String> {
        self.acknowledged(&Request::SetGear {
            gear: gear.to_string(),
            rpm,
        })
    }

    pub fn set_lighting(&self, lighting: Lighting) -> Result<Option<Warning>, String> {
        match self.request_within(LIGHT_TIMEOUT, &Request::SetLighting { lighting })? {
            Reply::Ok { warning } => Ok(warning),
            other => Err(unexpected(&other)),
        }
    }

    pub fn set_standby(&self, standby: Standby) -> Result<Option<Warning>, String> {
        self.acknowledged(&Request::SetStandby { standby })
    }

    fn acknowledged(&self, request: &Request) -> Result<Option<Warning>, String> {
        match self.request(request)? {
            Reply::Ok { warning } => Ok(warning),
            other => Err(unexpected(&other)),
        }
    }

    fn request(&self, request: &Request) -> Result<Reply, String> {
        self.request_within(TIMEOUT, request)
    }

    fn request_within(&self, timeout: Duration, request: &Request) -> Result<Reply, String> {
        let stream = UnixStream::connect(&self.path).map_err(connect_fault)?;

        stream.set_read_timeout(Some(timeout)).map_err(fault)?;
        stream.set_write_timeout(Some(timeout)).map_err(fault)?;

        let mut writer = stream.try_clone().map_err(fault)?;
        let mut line = serde_json::to_string(request).map_err(fault)?;
        line.push('\n');
        writer.write_all(line.as_bytes()).map_err(fault)?;

        let mut answer = String::new();
        BufReader::new(stream)
            .read_line(&mut answer)
            .map_err(fault)?;
        if answer.trim().is_empty() {
            return Err("daemon closed the connection".to_string());
        }

        match serde_json::from_str::<Reply>(&answer).map_err(fault)? {
            Reply::Error { message } => Err(message),
            reply => Ok(reply),
        }
    }
}

/// A subscribed connection, one status per line until it breaks.
pub struct Updates {
    lines: std::io::Lines<BufReader<UnixStream>>,
}

impl Iterator for Updates {
    type Item = Status;

    fn next(&mut self) -> Option<Status> {
        loop {
            let line = self.lines.next()?.ok()?;
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<Reply>(&line) {
                Ok(Reply::Status(status)) => return Some(status),
                // The daemon sends nothing else down this connection, so
                // anything else is a version mismatch worth ignoring rather
                // than tearing the stream down for.
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }
}

fn connect_fault(err: std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => "daemon is not running".to_string(),
        std::io::ErrorKind::PermissionDenied => "no permission for the daemon socket".to_string(),
        _ => err.to_string(),
    }
}

fn unexpected(reply: &Reply) -> String {
    format!("unexpected reply: {reply:?}")
}

fn fault(err: impl std::fmt::Display) -> String {
    err.to_string()
}
