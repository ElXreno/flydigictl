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

use flydigictl::config::{Config, ConfigError, Kind, Point, Sensor, Smoothing};
use flydigictl::curve::{self, Demand, Smoothed};
use flydigictl::device::Device;
use flydigictl::error::Error;
use flydigictl::ipc::{self, Reply, Request, Status, Warning, WarningCode};
use flydigictl::protocol::{self, MAX_RPM, MIN_RPM, STOP_RPM};
use flydigictl::{nvidia, screens, sensor, watch};

/// The cooler reports itself every 500 ms; the loop wakes far more often than
/// that because a command waits for it to come back from reading. Frames are
/// queued by the kernel meanwhile, so nothing is missed by looking sooner, and
/// a client asking for a speed gets an answer in tens of milliseconds instead
/// of a couple of hundred. Curves are evaluated on their own, slower schedule.
const FRAME_POLL: Duration = Duration::from_millis(50);
const FRAME_TIMEOUT: Duration = Duration::from_millis(40);

/// Screens do not change often, and asking the kernel costs a handful of file
/// reads, so this is as often as it is worth asking.
const SCREEN_POLL: Duration = Duration::from_secs(1);

/// Silence this long means the cooler is gone rather than merely quiet.
const SILENCE: Duration = Duration::from_secs(3);

const ACK_TIMEOUT: Duration = Duration::from_millis(1500);

/// How long every curve has to agree the fan can stop before it is stopped.
const STOP_AFTER: Duration = Duration::from_secs(60);

/// How long to keep asking a freshly connected cooler what supply it has.
const SUPPLY_SETTLE: Duration = Duration::from_secs(5);

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
    /// Resolved lazily and retried: a chip can appear after we start, and
    /// giving up once would mean never running that curve again.
    source: Source,
    smoothed: Option<Smoothed>,
}

/// Where a curve's readings come from, once resolved.
enum Source {
    /// Every input the curve matches; the hottest of them is what it follows.
    Hwmon(Vec<PathBuf>),
    /// A card, holding its mapped registers open: mapping is what costs, and
    /// the mapping outlives the card suspending and resuming.
    Nvidia(Box<nvidia::Sensor>),
}

impl Source {
    fn find(sensor: &Sensor) -> Self {
        match sensor.kind {
            Kind::Hwmon => Self::Hwmon(sensor::resolve_all(sensor)),
            Kind::Nvidia => {
                let card = if sensor.device.is_empty() {
                    nvidia::cards().first().cloned().unwrap_or_default()
                } else {
                    sensor.device.clone()
                };

                Self::Nvidia(Box::new(nvidia::Sensor::open(
                    &card,
                    nvidia::Part::named(&sensor.label),
                )))
            }
        }
    }

    fn missing(&self) -> bool {
        match self {
            Self::Hwmon(paths) => paths.is_empty(),
            Self::Nvidia(card) => card.card().is_empty() || card.missing(),
        }
    }

    /// Is the thing this watches asleep? Only a card can be.
    fn sleeping(&self) -> bool {
        match self {
            Self::Hwmon(_) => false,
            Self::Nvidia(card) => card.sleeping(),
        }
    }

    /// The reading to follow, or nothing when there is none to be had - which
    /// for a sleeping GPU is the honest answer rather than a failure.
    fn read(&mut self) -> Option<u8> {
        match self {
            Self::Hwmon(paths) => paths.iter().filter_map(|path| sensor::read(path)).max(),
            Self::Nvidia(card) => card.read(),
        }
    }
}

struct Curves {
    runners: Vec<Runner>,
    smoothing: Smoothing,
    complained: bool,

    /// Curves that read nothing because the thing they watch is asleep, which
    /// is not the same as a sensor that is missing or broken.
    asleep: Vec<String>,

    /// Curves whose sensor is present and awake and still gave nothing, which
    /// on this machine means the daemon cannot get at it.
    unreadable: Vec<String>,
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
                source: Source::find(&curve.sensor),
                smoothed: None,
            })
            .collect();

        let mut curves = Self {
            runners,
            smoothing: config.smoothing,
            complained: false,
            asleep: Vec::new(),
            unreadable: Vec::new(),
        };
        curves.complain_about_missing();
        curves
    }

    fn complain_about_missing(&mut self) {
        let missing: Vec<&str> = self
            .runners
            .iter()
            .filter(|runner| runner.source.missing())
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
        self.asleep.clear();
        self.unreadable.clear();

        for runner in &mut self.runners {
            if runner.source.missing() {
                runner.source = Source::find(&runner.sensor);
                if !runner.source.missing() {
                    info!("curve {}: sensor found", runner.name);
                }
            }

            let Some(raw) = runner.source.read() else {
                // Three different silences, and a client cannot act on them the
                // same way: asleep is fine and expected, unreadable is a sensor
                // that is there and will not answer, missing is already said.
                if runner.source.sleeping() {
                    self.asleep.push(runner.name.clone());
                } else if !runner.source.missing() {
                    self.unreadable.push(runner.name.clone());
                }
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

    let mut demands: Vec<Demand> = Vec::new();
    let mut leader: Option<Demand> = None;
    let mut evaluated: Option<Instant> = None;
    let mut missed = Duration::ZERO;
    let mut searched: Option<Instant> = None;

    // Tracked so the target is only rewritten when it actually moves, and so a
    // reconnect can restore it: realtime mode does not survive one.
    let mut applied: Option<u16> = None;

    // Since when every curve has been asking for a stopped fan.
    let mut idle_since: Option<Instant> = None;

    let mut supply: Option<protocol::Supply> = None;

    // The firmware measures its supply once per power-up and answers zero until
    // it has, which takes up to three and a half seconds, so the first answer
    // after a connection is often no answer at all and has to be asked again.
    let mut asked_supply_since: Option<Instant> = None;
    let mut strip_on: Option<bool> = None;

    // Not left in the config struct: `set_config` replaces that wholesale, so
    // a client's stale copy would undo changes made since it last read one.
    let mut manual_rpm = config.manual_rpm;
    let mut lighting = config.lighting;

    // Bumped whenever the configuration changes under a client's feet, which
    // a rebuild does: the daemon rereads the file and says nothing, so an
    // interface holding a copy from before would go on drawing yesterday's
    // curves. Status carries it; a client that sees it move refetches.
    let mut revision: u64 = 0;

    // What the cooler is believed to be holding, as opposed to what is wanted.
    //
    // Uploading an animation erases and rewrites a fixed page of the cooler's
    // flash, and it keeps that page across a reconnect and a power cut - so
    // replaying the upload every time the link comes back spends an erase cycle
    // to write bytes that are already there. This machine reconnects some fifty
    // times a day, which is the difference between a decade and a year.
    let mut uploaded: Option<protocol::Lighting> = None;

    // Blanked is not a lighting state of its own: the wanted one stays put and
    // comes back untouched, so a screen going off does not lose the colour.
    let mut blanked = false;
    let mut screens_checked: Option<Instant> = None;
    let mut lit_before_blanking = true;
    let mut changed_while_blank = false;
    let mut warned_about_supply = false;

    for event in rx {
        match event {
            Event::ConfigChanged => match Config::load(config_path) {
                Ok(fresh) => {
                    if fresh != *config {
                        info!("config reloaded from {}", config_path.display());
                        revision += 1;

                        if fresh.manual_rpm != config.manual_rpm {
                            manual_rpm = fresh.manual_rpm;
                        }
                        if fresh.lighting != config.lighting {
                            lighting = fresh.lighting;
                            if let (Some(dev), Some(lighting)) = (device.as_mut(), lighting) {
                                if let Reply::Ok { .. } =
                                    send_all(dev, lighting.reports(uploaded.as_ref()))
                                {
                                    uploaded = Some(lighting);
                                }
                            }
                        }

                        *config = fresh;
                        curves = Curves::build(config);
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
                        revision += 1;
                        config.manual_rpm = manual_rpm;
                        config.lighting = lighting;
                        for curve in &mut config.curves {
                            curve.points.sort_by_key(|point| point.temp_c);
                        }
                        curves = Curves::build(config);

                        // Not `applied = None`: the cooler is already holding
                        // what it was told, and a curve edit that leaves the
                        // demand where it was needs no word to the device. The
                        // next evaluation compares and writes only if it moved,
                        // which is what keeps a burst of edits from costing a
                        // third of a second of device traffic each.
                        persist(config, config_path, writable)
                    }

                    Request::SetManual { rpm } => match rpm {
                        Some(rpm) if rpm != STOP_RPM && !(MIN_RPM..=MAX_RPM).contains(&rpm) => {
                            Reply::Error {
                                message: format!("rpm {rpm} out of range ({MIN_RPM}-{MAX_RPM})"),
                            }
                        }
                        rpm => {
                            manual_rpm = rpm;
                            config.manual_rpm = rpm;
                            let reply = Reply::Ok { warning: None };

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

                    Request::Sensors => {
                        let mut sensors: Vec<ipc::SensorInfo> = sensor::list()
                            .into_iter()
                            .map(|entry| ipc::SensorInfo {
                                kind: Kind::Hwmon,
                                hwmon: entry.hwmon,
                                device: entry.device,
                                kernel: entry.kernel,
                                label: entry.label,
                                temp_c: sensor::read(&entry.path),
                            })
                            .collect();

                        // A card has two readings and a power state, and a
                        // sleeping one has neither reading; the state is
                        // carried so a client can say so rather than guess.
                        sensors.extend(nvidia::cards().into_iter().flat_map(|card| {
                            let state = nvidia::power_state(&card).unwrap_or_default();

                            [nvidia::Part::Core, nvidia::Part::Memory].map(move |part| {
                                ipc::SensorInfo {
                                    kind: Kind::Nvidia,
                                    hwmon: "nvidia".to_string(),
                                    kernel: state.clone(),
                                    temp_c: nvidia::read_temperature(&card, part),
                                    device: card.clone(),
                                    label: part.label().to_string(),
                                }
                            })
                        }));

                        Reply::Sensors { sensors }
                    }

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

                    Request::SetLighting { lighting: wanted } => match device.as_mut() {
                        None => Reply::Error {
                            message: "no cooler".to_string(),
                        },
                        Some(dev) => {
                            // While the screens are off the cooler stays dark;
                            // the choice is remembered and shown when they are
                            // back rather than lighting an empty room.
                            let reports = if blanked {
                                // Remembered and shown when the screens come
                                // back, rather than lighting an empty room.
                                changed_while_blank = true;
                                Vec::new()
                            } else {
                                wanted.reports(uploaded.as_ref())
                            };

                            match send_all(dev, reports) {
                                Reply::Ok { .. } => {
                                    info!("lighting {wanted}");
                                    lighting = Some(wanted);
                                    if !blanked {
                                        uploaded = Some(wanted);
                                    }
                                    config.lighting = lighting;
                                    strip_on = Some(wanted.mode != protocol::LightMode::Off);
                                    Reply::Ok { warning: None }
                                }
                                failure => failure,
                            }
                        }
                    },

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
                        asked_supply_since = Some(Instant::now());
                        if let Some(supply) = supply.filter(|supply| supply.decided()) {
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

                        // Whether the strip is lit is the one thing the cooler
                        // will report; what it is showing, it will not.
                        strip_on = dev
                            .query(protocol::query_strip(), ACK_TIMEOUT)
                            .ok()
                            .and_then(|payload| payload.first().map(|byte| *byte != 0));

                        if let Some(lighting) = lighting {
                            // Against what the cooler already holds, not from
                            // scratch: a reconnect does not empty its flash.
                            match send_all(dev, lighting.reports(uploaded.as_ref())) {
                                Reply::Ok { .. } => {
                                    if uploaded != Some(lighting) {
                                        info!("lighting {lighting}");
                                        uploaded = Some(lighting);
                                    }
                                }
                                other => warn!("cannot set the lighting: {other:?}"),
                            }
                        }
                    }
                }

                let Some(dev) = device.as_mut() else {
                    shared.lock().unwrap().publish(None);
                    continue;
                };

                // Ask again while the cooler is still making up its mind, and
                // stop asking once it has or once it has had long enough.
                if supply.is_none_or(|supply| !supply.decided()) {
                    if let Some(since) = asked_supply_since {
                        if since.elapsed() < SUPPLY_SETTLE {
                            supply = read_supply(dev);
                            if let Some(supply) = supply.filter(|supply| supply.decided()) {
                                info!("supply {supply}, up to {} rpm", supply.max_rpm());
                                asked_supply_since = None;
                            }
                        } else {
                            warn!("the cooler never said what supply it has");
                            asked_supply_since = None;
                        }
                    }
                }

                if config.lights_follow_screens
                    && screens_checked.is_none_or(|last| last.elapsed() >= SCREEN_POLL)
                {
                    screens_checked = Some(Instant::now());

                    if let Some(dark) = screens::all_dark() {
                        if dark != blanked {
                            blanked = dark;

                            // Power and indicators only: the pattern lives in
                            // the cooler's own flash, so putting the strip out
                            // and lighting it again brings back exactly what
                            // was showing - nothing uploaded, and nothing
                            // assumed about a state that was never recorded.
                            let reports = if dark {
                                lit_before_blanking = strip_on.unwrap_or(true);
                                changed_while_blank = false;

                                vec![protocol::light_off(), protocol::gear_light(false)]
                            } else if changed_while_blank {
                                let wanted = lighting.unwrap_or_default();
                                let reports = wanted.reports(uploaded.as_ref());
                                uploaded = Some(wanted);
                                reports
                            } else {
                                let indicators =
                                    lighting.is_none_or(|lighting| lighting.indicators);
                                let mut reports = vec![protocol::gear_light(indicators)];

                                if lit_before_blanking {
                                    reports.insert(0, protocol::light_on());
                                }

                                reports
                            };

                            match send_all(dev, reports) {
                                Reply::Ok { .. } => {
                                    info!("screens {}", if dark { "off" } else { "on" });
                                    strip_on = Some(if dark { false } else { lit_before_blanking });
                                }
                                other => warn!("cannot follow the screens: {other:?}"),
                            }
                        }
                    }
                }

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

                        if due && demands.is_empty() {
                            debug!("no temperature yet");
                        }

                        // A manual speed overrides every curve, by design. With
                        // no leader but readings in hand, every curve is content
                        // and the answer is a stopped fan; with no readings at
                        // all there is nothing to say, so the cooler is left as
                        // it is.
                        let asked = manual_rpm.or_else(|| match leader.as_ref() {
                            Some(demand) => Some(demand.rpm),
                            None => (!demands.is_empty()).then_some(STOP_RPM),
                        });

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

                        // Stopping is not symmetric with starting. Speeding up
                        // happens the moment a curve asks for it, but a stopped
                        // fan needs some 20 s of stall-and-retry before it turns
                        // again, so it stops only once the curves have held it
                        // there for a while - otherwise a reading wobbling
                        // across the first point switches the cooler on and off
                        // all evening. Asking for a stop by hand is honoured at
                        // once: that is a person, not a wobbling reading.
                        let wanted = match wanted {
                            Some(STOP_RPM) if manual_rpm.is_none() => {
                                let since = *idle_since.get_or_insert_with(Instant::now);
                                if applied == Some(STOP_RPM) || since.elapsed() >= STOP_AFTER {
                                    Some(STOP_RPM)
                                } else {
                                    Some(MIN_RPM)
                                }
                            }
                            other => {
                                idle_since = None;
                                other
                            }
                        };

                        if let Some(wanted) = wanted {
                            // Re-apply when the curve moves enough to matter, and
                            // whenever the cooler has fallen back to gear mode -
                            // a reconnect or the physical button does that.
                            let moved = applied
                                .is_none_or(|last| last.abs_diff(wanted) >= config.hysteresis_rpm);

                            // The cooler reports what it was told to hold, and
                            // that is the only account worth trusting: another
                            // client, a refused write or the button on the case
                            // can all leave it holding something else, and
                            // believing our own memory means never noticing.
                            let drifted = status.mode != protocol::Mode::Realtime
                                || applied.is_some_and(|last| status.target_rpm != last);

                            if moved || drifted {
                                match apply(dev, wanted, status.mode) {
                                    Ok(()) => {
                                        match (&leader, manual_rpm) {
                                            (Some(d), None) => debug!(
                                                "target {wanted} rpm, led by {} at {} C (smoothed {}){}",
                                                d.name,
                                                d.temp_c,
                                                d.smoothed_c,
                                                if d.panic { ", panic" } else { "" }
                                            ),
                                            (_, Some(_)) => debug!("target {wanted} rpm (manual)"),
                                            (None, None) => {
                                                debug!("target {wanted} rpm, every curve content")
                                            }
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
                                manual: manual_rpm.is_some(),
                                manual_rpm,
                                supply: supply.map(|supply| supply.to_string()),
                                supply_max_rpm: supply.and_then(|supply| supply.known_max_rpm()),
                                lighting,
                                strip_on,
                                leading: leader.as_ref().map(|d| d.name.clone()),
                                asleep: curves.asleep.clone(),
                                unreadable: curves.unreadable.clone(),
                                revision,
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

/// Hold a speed, entering realtime mode first if the cooler is not in it.
///
/// Entering costs an acknowledgement and a pause for the firmware to settle,
/// which is most of the time this takes - and is wasted when the cooler is
/// already there, which it is for every write after the first.
///
/// The speed itself is acknowledged with `01` when it was taken and `02` when
/// the cooler was not in realtime after all, in which case it is dropped. That
/// byte is the difference between a speed applied and a speed imagined.
fn apply(dev: &mut Device, rpm: u16, mode: protocol::Mode) -> Result<(), Error> {
    let began = Instant::now();

    let enter = |dev: &mut Device| -> Result<(), Error> {
        dev.send_acked(protocol::enter_realtime(), ACK_TIMEOUT)?;
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    };

    if mode != protocol::Mode::Realtime {
        enter(dev)?;
    }

    if dev.send_acked(protocol::set_realtime_rpm(rpm), ACK_TIMEOUT)? != 1 {
        warn!("cooler refused {rpm} rpm, entering realtime and trying again");
        enter(dev)?;

        if dev.send_acked(protocol::set_realtime_rpm(rpm), ACK_TIMEOUT)? != 1 {
            return Err(Error::Config(format!("the cooler will not hold {rpm} rpm")));
        }
    }

    debug!("apply {rpm} rpm took {} ms", began.elapsed().as_millis());
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
