//! Fan curve daemon for Flydigi BS series coolers.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};

use flydigictl::config::{Config, ConfigError, Point, Sensor, Smoothing};
use flydigictl::curve::{self, Demand, Smoothed};
use flydigictl::device::Device;
use flydigictl::error::Error;
use flydigictl::ipc::{self, Reply, Request, Status, Warning, WarningCode};
use flydigictl::protocol::{self, MAX_RPM, MIN_RPM, STOP_RPM};
use flydigictl::{sensor, watch};

/// The cooler reports itself every 500 ms, so the loop wakes twice as often to
/// forward what it says without waiting on the next wakeup. Curves are
/// evaluated on their own, much slower schedule: sensors do not change faster
/// than the fan can react, but subscribers still want the speed in real time.
const FRAME_POLL: Duration = Duration::from_millis(250);
const FRAME_TIMEOUT: Duration = Duration::from_millis(200);

/// Silence this long means the cooler is gone rather than merely quiet.
const SILENCE: Duration = Duration::from_secs(3);

const ACK_TIMEOUT: Duration = Duration::from_millis(1500);

/// Lighting reports sent back to back are dropped by the cooler.
const LIGHT_GAP: Duration = Duration::from_millis(5);

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
    /// One sender per subscribed client. Kept here rather than in the control
    /// loop so that a client can attach between two updates and still get the
    /// snapshot it missed.
    subscribers: Vec<Sender<Status>>,
}

impl Shared {
    /// Store a new picture of the cooler and hand it to every subscriber.
    ///
    /// Clients that went away are dropped here: a failed send is the only
    /// notice we get, since nothing tells the daemon a socket was closed.
    fn publish(&mut self, status: Option<Status>) {
        // Nothing changed while the cooler stays away, and subscribers do not
        // need to hear that on every wakeup.
        if status.is_none() && self.status.is_none() {
            return;
        }

        self.status = status.clone();

        let update = status.unwrap_or_else(Status::disconnected);
        self.subscribers
            .retain(|subscriber| subscriber.send(update.clone()).is_ok());
    }
}

/// One configured curve plus the state needed to run it.
struct Runner {
    name: String,
    sensor: Sensor,
    points: Vec<Point>,
    panic_c: u8,
    /// Resolved lazily and retried: a hwmon can appear after we start, and
    /// giving up once would mean never running that curve again.
    paths: Vec<PathBuf>,
    smoothed: Option<Smoothed>,
}

struct Curves {
    runners: Vec<Runner>,
    smoothing: Smoothing,
    complained: bool,
}

impl Curves {
    fn build(config: &Config) -> Self {
        let runners = config
            .curves
            .iter()
            .enumerate()
            .map(|(index, curve)| Runner {
                name: curve::describe(curve, index),
                sensor: curve.sensor.clone(),
                points: curve.points.clone(),
                panic_c: curve.panic_c.unwrap_or(config.smoothing.panic_c),
                paths: sensor::resolve_all(&curve.sensor),
                smoothed: None,
            })
            .collect();

        let mut curves = Self {
            runners,
            smoothing: config.smoothing,
            complained: false,
        };
        curves.complain_about_missing();
        curves
    }

    fn complain_about_missing(&mut self) {
        let missing: Vec<&str> = self
            .runners
            .iter()
            .filter(|runner| runner.paths.is_empty())
            .map(|runner| runner.name.as_str())
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
            "no sensor for curve(s): {}, retrying. available: {}",
            missing.join(", "),
            available.join(", ")
        );
    }

    /// Evaluate every curve. Curves whose sensor is missing are skipped, not
    /// fatal - the rest keep the cooler running.
    fn evaluate(&mut self, dt_secs: f32) -> Vec<Demand> {
        let mut demands = Vec::new();

        for runner in &mut self.runners {
            if runner.paths.is_empty() {
                runner.paths = sensor::resolve_all(&runner.sensor);
                if !runner.paths.is_empty() {
                    info!("curve {}: sensor found", runner.name);
                }
            }

            // Several inputs behind one curve (two DIMMs, two drives) collapse
            // to the hottest: that is the one needing air.
            let Some(raw) = runner
                .paths
                .iter()
                .filter_map(|path| sensor::read(path))
                .max()
            else {
                continue;
            };

            let smoothed = match &mut runner.smoothed {
                Some(state) => state.update(raw, dt_secs, &self.smoothing),
                None => {
                    // Start from the first reading rather than crawling up from
                    // zero, which would ignore a machine that is already warm.
                    runner.smoothed = Some(Smoothed::new(raw));
                    raw
                }
            };

            let panicking = raw >= runner.panic_c;
            let effective = if panicking { raw } else { smoothed };

            if let Some(rpm) = curve::target_for(&runner.points, effective) {
                demands.push(Demand {
                    name: runner.name.clone(),
                    temp_c: raw,
                    smoothed_c: smoothed,
                    rpm,
                    panic: panicking,
                });
            }
        }

        demands
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

    spawn_ticker(tx.clone());

    let shared = Arc::new(Mutex::new(Shared::default()));
    serve(socket_path, tx, Arc::clone(&shared))?;

    control_loop(rx, &mut config, config_path, writable, device, &shared)
}

fn spawn_ticker(tx: Sender<Event>) {
    std::thread::spawn(move || loop {
        if tx.send(Event::Tick).is_err() {
            return;
        }
        std::thread::sleep(FRAME_POLL);
    });
}

/// Take the listener systemd passed us, if it did.
///
/// Ownership and permissions of the socket are systemd's business: it creates
/// it before the daemon starts, which is also how the daemon can drop root and
/// still hand a usable socket to a desktop client.
fn inherited_listener() -> Option<UnixListener> {
    if std::env::var("LISTEN_PID").ok()? != std::process::id().to_string() {
        return None;
    }
    if std::env::var("LISTEN_FDS").ok()? != "1" {
        warn!("expected exactly one socket from systemd, binding our own");
        return None;
    }

    // SAFETY: systemd guarantees fd 3 is the listening socket it created, and
    // this runs once before anything else touches that descriptor.
    const SD_LISTEN_FDS_START: RawFd = 3;
    Some(unsafe { UnixListener::from_raw_fd(SD_LISTEN_FDS_START) })
}

fn serve(path: &Path, tx: Sender<Event>, shared: Arc<Mutex<Shared>>) -> Result<(), Error> {
    let listener = match inherited_listener() {
        Some(listener) => listener,
        None => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::remove_file(path);

            UnixListener::bind(path).map_err(|source| {
                // Under systemd the socket is created for us, in a directory an
                // unprivileged daemon has no business writing to. Getting here
                // means the service ran without that descriptor, which is a
                // wiring problem rather than something to retry into.
                if source.kind() == std::io::ErrorKind::PermissionDenied {
                    return Error::Config(format!(
                        "no socket was passed and {} cannot be created here: start \
                         flydigictld.socket, or point --socket somewhere writable",
                        path.display()
                    ));
                }

                Error::Open {
                    path: path.to_path_buf(),
                    source,
                }
            })?
        }
    };

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

        // A subscription takes over the connection, so it never comes back
        // here to read another request.
        if let Ok(Request::Subscribe) = serde_json::from_str::<Request>(&line) {
            stream_status(writer, Arc::clone(&shared));
            return;
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

/// Write updates to a subscribed client until it goes away.
///
/// The current snapshot goes out first so a client that connects between two
/// updates has something to draw immediately rather than an empty window.
fn stream_status(mut writer: UnixStream, shared: Arc<Mutex<Shared>>) {
    let (tx, rx) = mpsc::channel();

    let first = {
        let mut shared = shared.lock().unwrap();
        shared.subscribers.push(tx);
        shared.status.clone()
    };

    let mut write = |status: Status| -> bool {
        let Ok(mut text) = serde_json::to_string(&Reply::Status(status)) else {
            return true;
        };
        text.push('\n');
        writer.write_all(text.as_bytes()).is_ok()
    };

    if !write(first.unwrap_or_else(Status::disconnected)) {
        return;
    }

    for status in rx {
        if !write(status) {
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
    let mut curves = Curves::build(config);

    // Kept between frames: the curves are evaluated on their own schedule, but
    // every frame is published and needs something to say about them.
    let mut demands: Vec<Demand> = Vec::new();
    let mut leader: Option<Demand> = None;
    let mut evaluated: Option<Instant> = None;
    let mut missed = Duration::ZERO;
    let mut searched: Option<Instant> = None;

    // Tracked so the target is only rewritten when it actually moves, and so a
    // reconnect can restore it: realtime mode does not survive one.
    let mut applied: Option<u16> = None;

    // Read once per connection: the level only changes with the supply, and
    // that means a re-enumeration anyway.
    let mut supply: Option<protocol::Supply> = None;
    let mut warned_about_supply = false;

    for event in rx {
        match event {
            Event::ConfigChanged => match Config::load(config_path) {
                Ok(fresh) => {
                    if fresh != *config {
                        info!("config reloaded from {}", config_path.display());
                        *config = fresh;
                        curves = Curves::build(config);
                        applied = None;
                    }
                }
                Err(err) => warn!("ignoring config change: {err}"),
            },

            Event::Command(request, reply_tx) => {
                let reply = match request {
                    Request::Status => unreachable!("served from the snapshot"),

                    Request::Subscribe => unreachable!("kept on the client thread"),

                    Request::GetConfig => Reply::Config {
                        config: config.clone(),
                        writable,
                    },

                    Request::SetConfig { config: fresh } => {
                        *config = fresh;
                        for curve in &mut config.curves {
                            curve.points.sort_by_key(|point| point.temp_c);
                        }
                        curves = Curves::build(config);
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
                            let reply = persist(config, config_path, writable);

                            // A read-only config is a standing fact a client
                            // also learns from `get_config`, while this one is
                            // about the speed just requested, so it wins.
                            match (rpm, supply) {
                                (Some(rpm), Some(supply)) if rpm > supply.max_rpm() => Reply::Ok {
                                    warning: Some(Warning {
                                        code: WarningCode::SupplyLimited,
                                        message: format!(
                                            "supply is {supply}, holding {} rpm instead of {rpm}",
                                            supply.max_rpm()
                                        ),
                                    }),
                                },
                                _ => reply,
                            }
                        }
                    },

                    Request::Gears => match device.as_mut() {
                        None => Reply::Error {
                            message: "no cooler".to_string(),
                        },
                        Some(dev) => read_gears(dev, supply),
                    },

                    Request::SetGear { gear, rpm } => match device.as_mut() {
                        None => Reply::Error {
                            message: "no cooler".to_string(),
                        },
                        Some(dev) => set_gear(dev, supply, &gear, rpm),
                    },

                    Request::Light { light } => match device.as_mut() {
                        None => Reply::Error {
                            message: "no cooler".to_string(),
                        },
                        Some(dev) => match light_reports(&light) {
                            Err(message) => Reply::Error { message },
                            Ok(reports) => send_all(dev, reports),
                        },
                    },

                    // Stored in the cooler, but also in the config so that a
                    // cooler met for the first time is set up the same way.
                    Request::SetStandby { standby } => {
                        config.standby = Some(standby);

                        match device.as_mut() {
                            None => Reply::Error {
                                message: "no cooler".to_string(),
                            },
                            Some(dev) => {
                                match dev.send_acked(protocol::set_standby(standby), ACK_TIMEOUT) {
                                    Err(err) => Reply::Error {
                                        message: err.to_string(),
                                    },
                                    Ok(_) => {
                                        info!("standby set to {standby}");
                                        persist(config, config_path, writable)
                                    }
                                }
                            }
                        }
                    }
                };

                let _ = reply_tx.send(reply);
            }

            Event::Tick => {
                // Polling for frames is cheap, but scanning sysfs for a cooler
                // that is not there is not worth doing four times a second.
                if device.is_none() && searched.is_none_or(|last| last.elapsed() >= SILENCE) {
                    searched = Some(Instant::now());
                    device = Device::open(device_path).ok();

                    if let Some(dev) = device.as_mut() {
                        info!("{} on {}", dev.model.name(), dev.path.display());
                        // A fresh connection is back in gear mode.
                        applied = None;

                        supply = read_supply(dev);
                        warned_about_supply = false;
                        if let Some(supply) = supply {
                            info!("supply {supply}, up to {} rpm", supply.max_rpm());
                        }

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
                    shared.lock().unwrap().publish(None);
                    continue;
                };

                let interval = Duration::from_secs(config.interval_secs.max(1));
                let due = evaluated.is_none_or(|last| last.elapsed() >= interval);

                if due {
                    let dt = evaluated.map_or(interval, |last| last.elapsed());
                    demands = curves.evaluate(dt.as_secs_f32());
                    leader = curve::winner(&demands).cloned();
                    evaluated = Some(Instant::now());
                }

                let mut lost = false;

                match dev.read_status(FRAME_TIMEOUT) {
                    Err(Error::Disconnected) => {
                        warn!("cooler went away, reopening");
                        lost = true;
                    }

                    // A single quiet window means nothing: the cooler speaks
                    // when it feels like it, and a poll can simply fall between
                    // two of its frames.
                    Err(Error::Timeout) => {
                        missed += FRAME_TIMEOUT;
                        if missed >= SILENCE {
                            warn!("cooler stopped responding, reopening");
                            lost = true;
                        }
                    }

                    Err(err) => warn!("read failed: {err}"),

                    Ok(status) => {
                        missed = Duration::ZERO;

                        if due && leader.is_none() {
                            debug!("no temperature yet");
                        }

                        // A manual speed overrides every curve, by design.
                        let asked = config.manual_rpm.or(leader.as_ref().map(|d| d.rpm));

                        // Sending more than the supply allows is not an error,
                        // the firmware just holds its ceiling - but then every
                        // number the daemon reports is a speed the fan is not
                        // running at, so clamp here and say so once.
                        let wanted = match (asked, supply) {
                            (Some(rpm), Some(supply)) if rpm > supply.max_rpm() => {
                                if !warned_about_supply {
                                    warned_about_supply = true;
                                    warn!(
                                        "supply is {supply}: capping {rpm} rpm at {}",
                                        supply.max_rpm()
                                    );
                                }
                                Some(supply.max_rpm())
                            }
                            (rpm, _) => rpm,
                        };

                        if let Some(wanted) = wanted {
                            // Re-apply when the curve moves enough to matter, and
                            // whenever the cooler has fallen back to gear mode -
                            // a reconnect or the physical button does that.
                            let moved = applied
                                .is_none_or(|last| last.abs_diff(wanted) >= config.hysteresis_rpm);
                            let drifted = status.mode != protocol::Mode::Realtime;

                            if moved || drifted {
                                match apply(dev, wanted) {
                                    Ok(()) => {
                                        match &leader {
                                            Some(d) => debug!(
                                                "target {wanted} rpm, led by {} at {} C (smoothed {}){}",
                                                d.name,
                                                d.temp_c,
                                                d.smoothed_c,
                                                if d.panic { ", panic" } else { "" }
                                            ),
                                            None => debug!("target {wanted} rpm (manual)"),
                                        }
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
                            shared.lock().unwrap().publish(Some(Status {
                                model: dev.model.name().to_string(),
                                connected: true,
                                temp_c: leader.as_ref().map(|d| d.temp_c),
                                current_rpm: Some(status.current_rpm),
                                target_rpm: applied.or(Some(status.target_rpm)),
                                manual: config.manual_rpm.is_some(),
                                supply: supply.map(|supply| supply.to_string()),
                                supply_max_rpm: supply.map(|supply| supply.max_rpm()),
                                leading: leader.as_ref().map(|d| d.name.clone()),
                                demands: demands.clone(),
                            }));
                        }
                    }
                }

                if lost {
                    device = None;
                    applied = None;
                    missed = Duration::ZERO;
                    shared.lock().unwrap().publish(None);
                }
            }
        }
    }

    Ok(())
}

fn read_gears(dev: &mut Device, supply: Option<protocol::Supply>) -> Reply {
    let payload = match dev.query(protocol::query_gear_table(), ACK_TIMEOUT) {
        Ok(payload) => payload,
        Err(err) => {
            return Reply::Error {
                message: err.to_string(),
            }
        }
    };

    let Some(table) = protocol::parse_gear_table(&payload) else {
        return Reply::Error {
            message: "the cooler answered with a gear table it does not have".to_string(),
        };
    };

    Reply::Gears {
        gears: protocol::Gear::ALL
            .iter()
            .zip(table)
            .map(|(gear, rpm)| ipc::Gear {
                name: gear.to_string(),
                rpm,
                allowed: supply.is_none_or(|supply| supply.allows(*gear)),
            })
            .collect(),
    }
}

fn set_gear(dev: &mut Device, supply: Option<protocol::Supply>, name: &str, rpm: u16) -> Reply {
    let Ok(gear) = name.parse::<protocol::Gear>() else {
        return Reply::Error {
            message: format!("no gear called {name}"),
        };
    };

    if !(MIN_RPM..=MAX_RPM).contains(&rpm) {
        return Reply::Error {
            message: format!("rpm {rpm} out of range ({MIN_RPM}-{MAX_RPM})"),
        };
    }

    let Some(report) = protocol::set_gear_rpm(gear, rpm) else {
        return Reply::Error {
            message: format!("no gear called {name}"),
        };
    };

    match dev.send_acked(report, ACK_TIMEOUT) {
        Err(err) => Reply::Error {
            message: err.to_string(),
        },
        Ok(0) => Reply::Error {
            message: format!("the cooler refused gear {name}"),
        },
        Ok(_) => {
            info!("gear {gear} stored at {rpm} rpm");

            // Storing always works; running the gear is what a weak supply
            // stops, so say which of the two just happened.
            let warning = supply
                .filter(|supply| !supply.allows(gear))
                .map(|supply| Warning {
                    code: WarningCode::SupplyLimited,
                    message: format!(
                        "supply is {supply}, so this gear will not run until it improves"
                    ),
                });

            Reply::Ok { warning }
        }
    }
}

fn light_reports(light: &ipc::Light) -> Result<Vec<[u8; protocol::REPORT_LEN]>, String> {
    match light {
        ipc::Light::Off => Ok(vec![protocol::light_off()]),

        ipc::Light::Indicators { on } => Ok(vec![protocol::gear_light(*on)]),

        ipc::Light::Effect { mode } => {
            if *mode == 0 || *mode > protocol::EFFECT_COUNT {
                return Err(format!(
                    "no effect {mode}, the firmware has 1-{}",
                    protocol::EFFECT_COUNT
                ));
            }
            Ok(protocol::light_effect(*mode))
        }

        ipc::Light::Static { color, brightness } => {
            let hex = color.strip_prefix('#').unwrap_or(color);
            if hex.len() != 6 {
                return Err(format!("bad colour: {color}"));
            }

            let byte = |range: std::ops::Range<usize>| {
                u8::from_str_radix(&hex[range], 16).map_err(|_| format!("bad colour: {color}"))
            };

            let color = protocol::Rgb {
                r: byte(0..2)?,
                g: byte(2..4)?,
                b: byte(4..6)?,
            };
            Ok(protocol::light_static(color, *brightness))
        }
    }
}

/// Lighting arrives as a burst of reports, and the cooler drops the ones that
/// come too close together.
fn send_all(dev: &mut Device, reports: Vec<[u8; protocol::REPORT_LEN]>) -> Reply {
    for report in reports {
        if let Err(err) = dev.send_acked(report, ACK_TIMEOUT) {
            return Reply::Error {
                message: err.to_string(),
            };
        }
        std::thread::sleep(LIGHT_GAP);
    }

    Reply::Ok { warning: None }
}

/// Ask the cooler how much power it has, tolerating a device that does not know
/// the query.
fn read_supply(dev: &mut Device) -> Option<protocol::Supply> {
    match dev.query(protocol::query_supply(), ACK_TIMEOUT) {
        Ok(payload) => payload.first().copied().map(protocol::Supply::from_byte),
        Err(err) => {
            debug!("cannot read the supply level: {err}");
            None
        }
    }
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
                message: format!("{} is read-only, change not saved", path.display()),
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
