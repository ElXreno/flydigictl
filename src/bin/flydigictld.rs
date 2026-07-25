//! Fan curve daemon for Flydigi BS series coolers.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{debug, error, info, warn};

use flydigictl::config::{Aggregate, Config, ConfigError, Sensor};
use flydigictl::device::Device;
use flydigictl::error::Error;
use flydigictl::ipc::{self, Reply, Request, Status, Warning, WarningCode};
use flydigictl::protocol::{self, MAX_RPM, MIN_RPM, STOP_RPM};
use flydigictl::{sensor, watch};

const READ_TIMEOUT: Duration = Duration::from_secs(3);
const ACK_TIMEOUT: Duration = Duration::from_millis(1500);

/// Run a fan curve against a Flydigi BS series cooler
#[derive(argh::FromArgs)]
struct Args {
    /// config file (default: /etc/flydigictl/config.toml)
    #[argh(option, short = 'c')]
    config: Option<PathBuf>,

    /// socket to listen on (default: /run/flydigictl/flydigictl.sock)
    #[argh(option, short = 's')]
    socket: Option<PathBuf>,

    /// hidraw device path (auto-detected if omitted)
    #[argh(option, short = 'd')]
    device: Option<PathBuf>,
}

/// What the control loop reacts to.
enum Event {
    Tick,
    ConfigChanged,
    Command(Request, Sender<Reply>),
}

/// Everything a client can read without disturbing the control loop.
#[derive(Default)]
struct Shared {
    status: Option<Status>,
}

/// Configured sensors, resolved to sysfs paths.
///
/// Resolution is retried while anything is still missing: a hwmon can show up
/// after the daemon starts, and giving up once would mean never running the
/// curve on a machine that was merely slow to load a module.
struct Sensors {
    resolved: Vec<(Sensor, Option<PathBuf>)>,
    aggregate: Aggregate,
    complained: bool,
}

impl Sensors {
    fn resolve(config: &Config) -> Self {
        let mut sensors = Self {
            resolved: config
                .sensors
                .iter()
                .map(|sensor| (sensor.clone(), sensor::resolve(sensor)))
                .collect(),
            aggregate: config.aggregate,
            complained: false,
        };
        sensors.complain_about_missing();
        sensors
    }

    fn describe(sensor: &Sensor) -> String {
        if sensor.label.is_empty() {
            format!("{} (first input)", sensor.hwmon)
        } else {
            format!("{}/{}", sensor.hwmon, sensor.label)
        }
    }

    fn complain_about_missing(&mut self) {
        let missing: Vec<String> = self
            .resolved
            .iter()
            .filter(|(_, path)| path.is_none())
            .map(|(sensor, _)| Self::describe(sensor))
            .collect();

        if missing.is_empty() || self.complained {
            return;
        }
        self.complained = true;

        let available: Vec<String> = sensor::list()
            .iter()
            .map(|entry| {
                if entry.label.is_empty() {
                    entry.hwmon.clone()
                } else {
                    format!("{}/{}", entry.hwmon, entry.label)
                }
            })
            .collect();

        warn!(
            "no sensor for: {}, retrying. available: {}",
            missing.join(", "),
            available.join(", ")
        );
    }

    /// Current temperature for the curve, retrying anything unresolved.
    fn read(&mut self) -> Option<u8> {
        let mut readings = Vec::new();

        for (sensor, path) in &mut self.resolved {
            if path.is_none() {
                *path = sensor::resolve(sensor);
                if path.is_some() {
                    info!("sensor {} found", Sensors::describe(sensor));
                }
            }

            if let Some(reading) = path.as_deref().and_then(sensor::read) {
                readings.push(reading);
            }
        }

        self.aggregate.apply(&readings)
    }
}

fn main() -> ExitCode {
    let args: Args = argh::from_env();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    let config_path = args
        .config
        .unwrap_or_else(|| PathBuf::from(flydigictl::config::DEFAULT_PATH));
    let socket_path = args
        .socket
        .unwrap_or_else(|| PathBuf::from(ipc::DEFAULT_SOCKET));

    match run(&config_path, &socket_path, args.device.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run(config_path: &Path, socket_path: &Path, device: Option<&Path>) -> Result<(), Error> {
    let mut config = match Config::load(config_path) {
        Ok(config) => config,
        Err(ConfigError::Read { .. }) => {
            info!("no config at {}, using defaults", config_path.display());
            Config::default()
        }
        Err(err) => return Err(Error::Config(err.to_string())),
    };

    let writable = Config::is_writable(config_path);
    if !writable {
        warn!(
            "{} is read-only, runtime changes are lost on restart",
            config_path.display()
        );
    }

    let (tx, rx) = mpsc::channel();

    // The watcher only says "something happened"; adapt that to an event.
    let (watch_tx, watch_rx) = mpsc::channel();
    watch::spawn(config_path, watch_tx);
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            while watch_rx.recv().is_ok() {
                if tx.send(Event::ConfigChanged).is_err() {
                    return;
                }
            }
        });
    }

    spawn_ticker(tx.clone(), config.interval_secs);

    let shared = Arc::new(Mutex::new(Shared::default()));
    serve(socket_path, tx, Arc::clone(&shared))?;

    control_loop(rx, &mut config, config_path, writable, device, &shared)
}

fn spawn_ticker(tx: Sender<Event>, mut interval_secs: u64) {
    if interval_secs == 0 {
        interval_secs = 1;
    }
    std::thread::spawn(move || loop {
        if tx.send(Event::Tick).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_secs(interval_secs));
    });
}

fn serve(path: &Path, tx: Sender<Event>, shared: Arc<Mutex<Shared>>) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(path);

    let listener = UnixListener::bind(path).map_err(|source| Error::Open {
        path: path.to_path_buf(),
        source,
    })?;
    info!("listening on {}", path.display());

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let tx = tx.clone();
                    let shared = Arc::clone(&shared);
                    std::thread::spawn(move || handle_client(stream, tx, shared));
                }
                Err(err) => warn!("accept failed: {err}"),
            }
        }
    });

    Ok(())
}

fn handle_client(stream: UnixStream, tx: Sender<Event>, shared: Arc<Mutex<Shared>>) {
    let reader = BufReader::new(match stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            warn!("client setup failed: {err}");
            return;
        }
    });
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => return,
        };
        if line.trim().is_empty() {
            continue;
        }

        let reply = match serde_json::from_str::<Request>(&line) {
            Err(err) => Reply::Error {
                message: format!("bad request: {err}"),
            },

            // Status is served straight from the shared snapshot so a stuck
            // control loop cannot block a GUI that is only polling.
            Ok(Request::Status) => match shared.lock().unwrap().status.clone() {
                Some(status) => Reply::Status(status),
                None => Reply::Error {
                    message: "no device yet".to_string(),
                },
            },

            Ok(request) => {
                let (reply_tx, reply_rx) = mpsc::channel();
                if tx.send(Event::Command(request, reply_tx)).is_err() {
                    return;
                }
                reply_rx.recv().unwrap_or(Reply::Error {
                    message: "daemon shutting down".to_string(),
                })
            }
        };

        let mut text = match serde_json::to_string(&reply) {
            Ok(text) => text,
            Err(err) => format!(r#"{{"reply":"error","message":"{err}"}}"#),
        };
        text.push('\n');

        if writer.write_all(text.as_bytes()).is_err() {
            return;
        }
    }
}

fn control_loop(
    rx: Receiver<Event>,
    config: &mut Config,
    config_path: &Path,
    writable: bool,
    device_path: Option<&Path>,
    shared: &Arc<Mutex<Shared>>,
) -> Result<(), Error> {
    let mut device: Option<Device> = None;
    let mut sensors = Sensors::resolve(config);

    // Tracked so the target is only rewritten when it actually moves, and so a
    // reconnect can restore it: realtime mode does not survive one.
    let mut applied: Option<u16> = None;

    for event in rx {
        match event {
            Event::ConfigChanged => match Config::load(config_path) {
                Ok(fresh) => {
                    if fresh != *config {
                        info!("config reloaded from {}", config_path.display());
                        *config = fresh;
                        sensors = Sensors::resolve(config);
                        applied = None;
                    }
                }
                Err(err) => warn!("ignoring config change: {err}"),
            },

            Event::Command(request, reply_tx) => {
                let reply = match request {
                    Request::Status => unreachable!("served from the snapshot"),

                    Request::GetConfig => Reply::Config {
                        config: config.clone(),
                        writable,
                    },

                    Request::SetConfig { config: fresh } => {
                        *config = fresh;
                        config.curve.sort_by_key(|point| point.temp_c);
                        sensors = Sensors::resolve(config);
                        applied = None;
                        persist(config, config_path, writable)
                    }

                    Request::SetManual { rpm } => match rpm {
                        Some(rpm) if rpm != STOP_RPM && !(MIN_RPM..=MAX_RPM).contains(&rpm) => {
                            Reply::Error {
                                message: format!("rpm {rpm} out of range ({MIN_RPM}-{MAX_RPM})"),
                            }
                        }
                        rpm => {
                            config.manual_rpm = rpm;
                            applied = None;
                            persist(config, config_path, writable)
                        }
                    },
                };

                let _ = reply_tx.send(reply);
            }

            Event::Tick => {
                if device.is_none() {
                    device = Device::open(device_path).ok();
                    if let Some(dev) = device.as_mut() {
                        info!("{} on {}", dev.model.name(), dev.path.display());
                        // A fresh connection is back in gear mode.
                        applied = None;

                        // Re-assert standby on every connection: it is stored in
                        // the cooler, but the cooler is what we just met.
                        if let Some(standby) = config.standby {
                            match dev.send_acked(protocol::set_standby(standby), ACK_TIMEOUT) {
                                Ok(_) => info!("standby set to {standby}"),
                                Err(err) => warn!("cannot set standby: {err}"),
                            }
                        }
                    }
                }

                let Some(dev) = device.as_mut() else {
                    shared.lock().unwrap().status = None;
                    continue;
                };

                let temp = sensors.read();
                let mut lost = false;

                match dev.read_status(READ_TIMEOUT) {
                    Err(Error::Disconnected | Error::Timeout) => {
                        warn!("cooler stopped responding, reopening");
                        lost = true;
                    }

                    Err(err) => warn!("read failed: {err}"),

                    Ok(status) => {
                        if temp.is_none() {
                            debug!("no temperature yet");
                        }

                        if let Some(wanted) = temp.and_then(|t| config.target_for(t)) {
                            // Re-apply when the curve moves enough to matter, and
                            // whenever the cooler has fallen back to gear mode -
                            // a reconnect or the physical button does that.
                            let moved = applied
                                .is_none_or(|last| last.abs_diff(wanted) >= config.hysteresis_rpm);
                            let drifted = status.mode != protocol::Mode::Realtime;

                            if moved || drifted {
                                match apply(dev, wanted) {
                                    Ok(()) => {
                                        debug!("target {wanted} rpm at {temp:?} C");
                                        applied = Some(wanted);
                                    }
                                    Err(err) => {
                                        warn!("cannot set {wanted} rpm: {err}");
                                        lost = true;
                                    }
                                }
                            }
                        }

                        if !lost {
                            shared.lock().unwrap().status = Some(Status {
                                model: dev.model.name().to_string(),
                                connected: true,
                                temp_c: temp,
                                current_rpm: Some(status.current_rpm),
                                target_rpm: applied.or(Some(status.target_rpm)),
                                manual: config.manual_rpm.is_some(),
                            });
                        }
                    }
                }

                if lost {
                    device = None;
                    applied = None;
                    shared.lock().unwrap().status = None;
                }
            }
        }
    }

    Ok(())
}

fn apply(dev: &mut Device, rpm: u16) -> Result<(), Error> {
    dev.send_acked(protocol::enter_realtime(), ACK_TIMEOUT)?;
    std::thread::sleep(Duration::from_millis(200));
    dev.send_acked(protocol::set_realtime_rpm(rpm), ACK_TIMEOUT)?;
    Ok(())
}

fn persist(config: &Config, path: &Path, writable: bool) -> Reply {
    if !writable {
        return Reply::Ok {
            warning: Some(Warning {
                code: WarningCode::ConfigReadOnly,
                message: format!(
                    "{} is read-only, change not saved",
                    path.display()
                ),
            }),
        };
    }

    match config.save(path) {
        Ok(()) => Reply::Ok { warning: None },
        Err(err) => Reply::Ok {
            warning: Some(Warning {
                code: WarningCode::ConfigSaveFailed,
                message: format!("change applied but not saved: {err}"),
            }),
        },
    }
}
