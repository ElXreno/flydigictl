//! Desktop front end for the fan curve daemon.

mod client;
mod editor;
mod palette;
mod picker;
mod spinner;

use std::path::PathBuf;
use std::time::Duration;

use iced::futures::{SinkExt, Stream, StreamExt};
use iced::widget::{
    button, canvas, checkbox, column, container, pick_list, progress_bar, row, scrollable, slider,
    text, text_input,
};
use iced::{Element, Length, Subscription, Task, Theme};

use flydigictl::config::{Config, Curve, Point, Sensor};
use flydigictl::curve;
use flydigictl::ipc::{self, Status, Warning, WarningCode};
use flydigictl::protocol::{LightMode, Lighting, Rgb, Standby, EFFECT_COUNT, MAX_RPM, MIN_RPM};

use picker::Hsv;

use client::Client;

/// How long to wait before dialling a daemon that is not there yet.
const RECONNECT: Duration = Duration::from_secs(1);

/// How long a note stays up before it takes itself away.
const NOTE_LIFE: Duration = Duration::from_secs(6);

/// How far back undo reaches.
const HISTORY: usize = 64;

/// Control a Flydigi BS series cooler
#[derive(argh::FromArgs)]
struct Args {
    /// daemon socket (default: /run/flydigictl/flydigictl.sock)
    #[argh(option, short = 's')]
    socket: Option<PathBuf>,
}

fn main() -> iced::Result {
    let args: Args = argh::from_env();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    let socket = args
        .socket
        .unwrap_or_else(|| PathBuf::from(ipc::DEFAULT_SOCKET));

    iced::application(move || State::new(socket.clone()), update, view)
        .title(title)
        .subscription(subscription)
        .theme(theme)
        .window_size((940.0, 580.0))
        .antialiasing(true)
        .run()
}

#[derive(Debug, Clone)]
enum Message {
    /// A push from the daemon: this is the only thing that drives the display.
    Live(Box<Status>),
    /// The stream broke, so the daemon or the cooler is gone.
    Offline,
    CurveSelected(usize),
    PointAdded(Point),
    PointMoved {
        index: usize,
        point: Point,
    },
    PointRemoved(usize),
    /// The pointer was released: the drag is over and the change can go out.
    PointsSettled,
    ManualToggled(bool),
    /// Dragging: local only, because the socket call is blocking and doing one
    /// per pixel of travel makes the slider crawl.
    ManualChanged(u16),
    ManualCommitted,
    Reload,

    TabSelected(Tab),

    /// Dragging the slider, which is not worth a write to the cooler yet.
    GearMoved {
        index: usize,
        rpm: u16,
    },
    /// The slider was let go, so the value is meant.
    GearCommitted(usize),

    CurveAdded,
    CurveRemoved,
    CurveRenamed(String),
    CurveNameCommitted,
    CurveSensorPicked(SensorChoice),
    CurvePanicChanged(u8),
    CurvePanicCommitted,

    /// Dragging inside the picker, which only moves the swatch.
    ColorPicked(Hsv),
    ColorCommitted,
    ModePicked(LightMode),
    BrightnessChanged(u8),
    BrightnessCommitted,
    IndicatorsToggled(bool),
    FollowScreensToggled(bool),

    StandbySelected(Standby),
    ExportConfig,
    Undo,
    Redo,
    /// A frame went by, which is how the note knows its time is up.
    Ticked,

    /// A request the interface sent has come back.
    Done(Box<Answer>),
}

/// What a background request produced.
///
/// Every one of these is a blocking socket round trip, and lighting is the
/// worst of them: the daemon walks the cooler through a couple of dozen
/// acknowledged reports before it answers. None of that belongs on the thread
/// that draws the window.
#[derive(Debug, Clone)]
enum Answer {
    Acked(Result<Option<Warning>, String>),
    /// Kept apart so the next lighting change knows the cooler is free again.
    Light(Result<Option<Warning>, String>),
    Config(Result<(Config, bool), String>),
    Gears(Result<Vec<ipc::Gear>, String>),
    Sensors(Result<Vec<ipc::SensorInfo>, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Curve,
    Gears,
    Light,
}

/// A sensor as offered in the picker: the label a person reads, plus what the
/// config needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SensorChoice {
    label: String,
    sensor: Sensor,
}

impl std::fmt::Display for SensorChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

/// Every hwmon the daemon can read, plus a whole-hwmon entry for each: an empty
/// label means "the hottest input of this chip", which is what covers a pair
/// of DIMMs or two drives with one curve.
///
/// The list comes from the daemon and not from this process. They can disagree:
/// a sandboxed daemon may be blind to sensors that are plainly there here, and
/// offering one of those would build a curve that never reads anything.
fn sensor_choices(sensors: &[ipc::SensorInfo]) -> Vec<SensorChoice> {
    use std::collections::BTreeMap;

    // No readings: the list is built once per connection, so a number in it
    // would sit stale in the closed picker.
    let mut chips: BTreeMap<&str, BTreeMap<&str, Vec<&ipc::SensorInfo>>> = BTreeMap::new();
    for entry in sensors {
        chips
            .entry(&entry.hwmon)
            .or_default()
            .entry(&entry.device)
            .or_default()
            .push(entry);
    }

    let mut choices: Vec<SensorChoice> = Vec::new();

    for (hwmon, devices) in &chips {
        let several = devices.len() > 1;

        let mut labels: Vec<&str> = devices
            .values()
            .flatten()
            .map(|entry| entry.label.as_str())
            .filter(|label| !label.is_empty())
            .collect();
        labels.sort_unstable();
        labels.dedup();

        choices.push(SensorChoice {
            label: if several {
                format!("{hwmon} (all, hottest)")
            } else {
                format!("{hwmon} (hottest)")
            },
            sensor: Sensor {
                hwmon: hwmon.to_string(),
                device: String::new(),
                label: String::new(),
            },
        });

        for label in &labels {
            choices.push(SensorChoice {
                label: format!("{hwmon}{}/{label}", if several { " (all)" } else { "" }),
                sensor: Sensor {
                    hwmon: hwmon.to_string(),
                    device: String::new(),
                    label: label.to_string(),
                },
            });
        }

        if !several {
            continue;
        }

        for (device, entries) in devices {
            let named = format!("{hwmon} {}", flydigictl::sensor::short_address(device));

            choices.push(SensorChoice {
                label: format!("{named} (hottest)"),
                sensor: Sensor {
                    hwmon: hwmon.to_string(),
                    device: device.to_string(),
                    label: String::new(),
                },
            });

            for entry in entries {
                if entry.label.is_empty() {
                    continue;
                }

                choices.push(SensorChoice {
                    label: format!("{named}/{}", entry.label),
                    sensor: Sensor {
                        hwmon: hwmon.to_string(),
                        device: device.to_string(),
                        label: entry.label.clone(),
                    },
                });
            }
        }
    }

    choices
}

/// Does the daemon have anything this curve would read?
///
/// The same matching the daemon does: an empty field accepts anything.
fn sensor_exists(sensors: &[ipc::SensorInfo], sensor: &Sensor) -> bool {
    sensors.iter().any(|entry| {
        entry.hwmon == sensor.hwmon
            && (sensor.device.is_empty()
                || entry.device == sensor.device
                || entry.kernel == sensor.device)
            && (sensor.label.is_empty() || entry.label == sensor.label)
    })
}

struct State {
    client: Client,
    status: Option<Status>,
    config: Option<Config>,
    writable: bool,
    selected: usize,

    /// One-off news: a command that failed, or one that worked with a caveat.
    ///
    /// Standing conditions do not belong here. A read-only config is true
    /// whether or not the last click succeeded, so it is drawn from `writable`
    /// instead - keeping it here meant the next successful command wiped a
    /// warning that was still true.
    note: Option<Note>,

    /// Read once at startup: the file behind it is written by a scheme
    /// generator, and those do not change it while a window is open.
    theme: Option<Theme>,
    tab: Tab,

    /// Read from the cooler rather than from the config: the gear table lives
    /// in the device and the physical button changes it too.
    gears: Vec<ipc::Gear>,

    /// What the controls are set to, which is not always what the cooler is
    /// showing: the truth is in the status, this is the draft being edited.
    light: Lighting,
    picked: Hsv,

    /// What was last asked for, until the daemon says the same thing back.
    ///
    /// Without it the control draws the daemon's answer only, so a click sits
    /// there doing nothing for as long as the round trip takes and the window
    /// looks stuck when it is merely waiting.
    manual_intent: Option<Option<u16>>,

    /// As the daemon reports them, and the choices built from that.
    available: Vec<ipc::SensorInfo>,
    sensors: Vec<SensorChoice>,

    /// How many requests are out, which is only worth knowing where there is
    /// nothing yet to draw.
    pending: usize,

    /// Lighting takes the cooler the best part of a second, and a slider let go
    /// twice would otherwise queue two of them. Only the latest state matters,
    /// so a change made while one is out replaces it instead of following it.
    lighting_out: bool,
    lighting_again: bool,

    /// When the queue last went from empty to busy, for the turning arc.
    working_since: Option<std::time::Instant>,

    /// The config as the daemon last had it, and the way back and forward.
    ///
    /// Snapshots are taken when a change is sent rather than as it is made:
    /// dragging a point produces a message per pixel, and undo should return
    /// to where the point was before the drag, not to halfway through it.
    committed: Option<Config>,
    history: Vec<Config>,
    future: Vec<Config>,
    /// Held while it is being typed, because sending on every keystroke means
    /// a socket round trip per letter.
    name_draft: Option<String>,
}

impl State {
    fn new(socket: PathBuf) -> (Self, Task<Message>) {
        let mut state = Self {
            client: Client::new(socket),
            status: None,
            config: None,
            writable: false,
            selected: 0,
            note: None,
            theme: palette::load(),
            tab: Tab::Curve,
            gears: Vec::new(),
            light: Lighting::default(),
            picked: Hsv::from_rgb(Rgb {
                r: 0x7A,
                g: 0xA2,
                b: 0xF7,
            }),
            manual_intent: None,
            available: Vec::new(),
            sensors: Vec::new(),
            name_draft: None,
            pending: 0,
            lighting_out: false,
            lighting_again: false,
            working_since: None,
            committed: None,
            history: Vec::new(),
            future: Vec::new(),
        };

        let opening = Task::batch([state.reload(), state.load_sensors()]);
        (state, opening)
    }

    /// True while a request is out, which is only worth knowing where there is
    /// nothing yet to draw.
    fn busy(&self) -> bool {
        self.pending > 0
    }

    fn ask<T: Send + 'static>(
        &mut self,
        work: impl FnOnce() -> T + Send + 'static,
        answer: impl FnOnce(T) -> Answer + Send + 'static,
    ) -> Task<Message> {
        if self.pending == 0 {
            self.working_since = Some(std::time::Instant::now());
        }

        self.pending += 1;
        offload(work, answer)
    }

    fn load_sensors(&mut self) -> Task<Message> {
        let client = self.client.clone();
        self.ask(move || client.sensors(), Answer::Sensors)
    }

    fn load_gears(&mut self) -> Task<Message> {
        let client = self.client.clone();
        self.ask(move || client.gears(), Answer::Gears)
    }

    fn reload(&mut self) -> Task<Message> {
        let client = self.client.clone();
        self.ask(move || client.config(), Answer::Config)
    }

    /// Push the edited config back. The daemon sorts the points and applies the
    /// result immediately, so there is nothing to apply separately.
    fn push(&mut self) -> Task<Message> {
        let Some(config) = self.config.clone() else {
            return Task::none();
        };

        if let Some(previous) = self.committed.replace(config.clone()) {
            if previous != config {
                self.history.push(previous);

                // Far more than anyone reaches for, and bounded so a long
                // session of dragging does not grow without end.
                if self.history.len() > HISTORY {
                    self.history.remove(0);
                }

                self.future.clear();
            }
        }

        self.send(config)
    }

    /// Send a config without touching the history, which undo needs.
    fn send(&mut self, config: Config) -> Task<Message> {
        let client = self.client.clone();
        self.ask(move || client.set_config(config), Answer::Acked)
    }

    fn step(&mut self, back: bool) -> Task<Message> {
        let (from, to) = if back {
            (&mut self.history, &mut self.future)
        } else {
            (&mut self.future, &mut self.history)
        };

        let Some(config) = from.pop() else {
            return Task::none();
        };

        if let Some(current) = self.config.take() {
            to.push(current);
        }

        self.selected = self.selected.min(config.curves.len().saturating_sub(1));
        self.name_draft = None;
        self.committed = Some(config.clone());
        self.config = Some(config.clone());

        self.send(config)
    }

    /// Send the draft as it stands. The daemon works out which reports that
    /// actually needs, so a brightness nudge does not restart an animation it
    /// did not have to.
    fn apply_light(&mut self) -> Task<Message> {
        if self.lighting_out {
            self.lighting_again = true;
            return Task::none();
        }

        self.lighting_out = true;

        let client = self.client.clone();
        let light = self.light;
        self.ask(move || client.set_lighting(light), Answer::Light)
    }

    fn report(&mut self, outcome: Result<Option<Warning>, String>) {
        self.note = match outcome {
            Ok(Some(Warning {
                code: WarningCode::ConfigReadOnly,
                ..
            })) => None,
            Ok(Some(Warning { message, .. })) => Some(Note::new(message)),
            Ok(None) => None,
            Err(err) => Some(Note::new(err)),
        };
    }

    fn ceiling(&self) -> u16 {
        self.status
            .as_ref()
            .and_then(|status| status.supply_max_rpm)
            .unwrap_or(MAX_RPM)
    }

    fn points_mut(&mut self) -> Option<&mut Vec<Point>> {
        let selected = self.selected;
        self.config
            .as_mut()
            .and_then(|config| config.curves.get_mut(selected))
            .map(|curve| &mut curve.points)
    }
}

/// Do the work on a thread of its own and deliver the result as a message.
fn offload<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    answer: impl FnOnce(T) -> Answer + Send + 'static,
) -> Task<Message> {
    let started = std::time::Instant::now();

    Task::perform(
        async move {
            let (tx, rx) = iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(work());
            });
            rx.await
        },
        move |result| {
            log::debug!("request answered in {} ms", started.elapsed().as_millis());

            Message::Done(Box::new(match result {
                Ok(value) => answer(value),
                Err(_) => Answer::Acked(Err("the request never finished".to_string())),
            }))
        },
    )
}

fn title(state: &State) -> String {
    match state.status.as_ref() {
        Some(status) => format!("{} - flydigictl", status.model),
        None => "flydigictl".to_string(),
    }
}

/// The machine's own palette when it has one, and otherwise iced's judgement.
///
/// `None` is how iced is asked to follow the desktop's light or dark
/// preference, which is all there is to follow when nothing generated a
/// scheme.
fn theme(state: &State) -> Option<Theme> {
    state.theme.clone()
}

/// Updates arrive when the cooler has something to say, not on a timer.
///
/// The reading itself is blocking, so it lives on its own thread and reaches
/// the interface through a channel. Identifying the subscription by the socket
/// path means it restarts by itself if that ever changes.
fn subscription(state: &State) -> Subscription<Message> {
    let updates = Subscription::run_with(Socket(state.client.socket().to_path_buf()), updates);
    let keys = iced::keyboard::listen().filter_map(shortcut);
    let updates = Subscription::batch([updates, keys]);

    // Frames are only worth asking for while something is moving.
    if state.note.is_some() || state.busy() {
        Subscription::batch([updates, iced::window::frames().map(|_| Message::Ticked)])
    } else {
        updates
    }
}

/// Ctrl+Z and its two usual spellings of redo.
fn shortcut(event: iced::keyboard::Event) -> Option<Message> {
    use iced::keyboard::{key::Named, Event, Key};

    let Event::KeyPressed { key, modifiers, .. } = event else {
        return None;
    };

    if !modifiers.command() {
        return None;
    }

    match key.as_ref() {
        Key::Character("z") if modifiers.shift() => Some(Message::Redo),
        Key::Character("z") => Some(Message::Undo),
        Key::Character("y") => Some(Message::Redo),
        Key::Named(Named::Undo) => Some(Message::Undo),
        Key::Named(Named::Redo) => Some(Message::Redo),
        _ => None,
    }
}

/// Identity of the subscription, which iced compares to decide whether the
/// running one still matches what the application asked for.
#[derive(Hash)]
struct Socket(PathBuf);

/// Something worth saying once, and not worth a click to get rid of.
#[derive(Debug, Clone)]
struct Note {
    text: String,
    said: std::time::Instant,
}

impl Note {
    fn new(text: String) -> Self {
        Self {
            text,
            said: std::time::Instant::now(),
        }
    }

    /// How much of its life is left, for the strip that counts it down.
    fn left(&self) -> f32 {
        let spent = self.said.elapsed().as_secs_f32() / NOTE_LIFE.as_secs_f32();
        (1.0 - spent).clamp(0.0, 1.0)
    }
}

fn updates(socket: &Socket) -> impl Stream<Item = Message> {
    let socket = socket.0.clone();

    iced::stream::channel(
        64,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();

            std::thread::spawn(move || {
                let client = Client::new(socket);

                loop {
                    if let Ok(stream) = client.subscribe() {
                        for status in stream {
                            if sender
                                .unbounded_send(Message::Live(Box::new(status)))
                                .is_err()
                            {
                                return;
                            }
                        }
                    }

                    if sender.unbounded_send(Message::Offline).is_err() {
                        return;
                    }
                    std::thread::sleep(RECONNECT);
                }
            });

            while let Some(message) = receiver.next().await {
                if output.send(message).await.is_err() {
                    return;
                }
            }
        },
    )
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Done(answer) => {
            state.pending = state.pending.saturating_sub(1);

            if state.pending == 0 {
                state.working_since = None;
            }

            match *answer {
                Answer::Acked(outcome) => state.report(outcome),

                Answer::Light(outcome) => {
                    state.report(outcome);
                    state.lighting_out = false;

                    if state.lighting_again {
                        state.lighting_again = false;
                        return state.apply_light();
                    }
                }

                Answer::Config(Ok((config, writable))) => {
                    state.selected = state.selected.min(config.curves.len().saturating_sub(1));
                    state.committed = Some(config.clone());
                    state.config = Some(config);
                    state.writable = writable;
                }

                Answer::Gears(Ok(gears)) => state.gears = gears,

                Answer::Sensors(Ok(sensors)) => {
                    state.sensors = sensor_choices(&sensors);
                    state.available = sensors;
                }

                Answer::Config(Err(err)) | Answer::Gears(Err(err)) | Answer::Sensors(Err(err)) => {
                    state.note = Some(Note::new(err));
                }
            }

            Task::none()
        }

        Message::Live(status) => {
            let first = state.status.is_none();

            if first {
                if let Some(lighting) = status.lighting {
                    state.light = lighting;
                    if let LightMode::Static { color } = lighting.mode {
                        state.picked = Hsv::from_rgb(color);
                    }
                }
            }

            // Drop the intent once the daemon reports the same thing, so a
            // change made elsewhere is not held off the screen by it.
            if state.manual_intent == Some(status.manual_rpm) {
                state.manual_intent = None;
            }

            state.status = Some(*status);

            // A daemon that just came back may be running a different config,
            // and its sensors are its own to report.
            if first {
                Task::batch([state.reload(), state.load_sensors()])
            } else {
                Task::none()
            }
        }

        Message::Offline => {
            state.status = None;
            state.config = None;
            Task::none()
        }

        Message::Reload => state.reload(),

        Message::CurveSelected(index) => {
            state.selected = index;
            state.name_draft = None;
            Task::none()
        }

        Message::PointAdded(point) => {
            if let Some(points) = state.points_mut() {
                points.push(point);
                points.sort_by_key(|point| point.temp_c);
            }
            state.push()
        }

        // Sorting mid-drag would renumber points under the hand dragging one.
        Message::PointMoved { index, point } => {
            if let Some(points) = state.points_mut() {
                if let Some(slot) = points.get_mut(index) {
                    *slot = point;
                }
            }
            Task::none()
        }

        Message::PointRemoved(index) => {
            if let Some(points) = state.points_mut() {
                if points.len() > 1 && index < points.len() {
                    points.remove(index);
                }
            }
            state.push()
        }

        Message::PointsSettled => {
            if let Some(points) = state.points_mut() {
                points.sort_by_key(|point| point.temp_c);
            }
            state.push()
        }

        Message::ManualToggled(on) => {
            let rpm = on.then(|| {
                state
                    .status
                    .as_ref()
                    .and_then(|status| status.target_rpm)
                    .unwrap_or(MIN_RPM)
                    .clamp(MIN_RPM, state.ceiling())
            });

            state.manual_intent = Some(rpm);

            let client = state.client.clone();
            state.ask(move || client.set_manual(rpm), Answer::Acked)
        }

        Message::ManualChanged(rpm) => {
            state.manual_intent = Some(Some(rpm));
            Task::none()
        }

        Message::ManualCommitted => {
            let Some(Some(rpm)) = state.manual_intent else {
                return Task::none();
            };

            let client = state.client.clone();
            state.ask(move || client.set_manual(Some(rpm)), Answer::Acked)
        }

        Message::TabSelected(tab) => {
            state.tab = tab;

            if tab == Tab::Gears {
                state.load_gears()
            } else {
                Task::none()
            }
        }

        Message::GearMoved { index, rpm } => {
            if let Some(gear) = state.gears.get_mut(index) {
                gear.rpm = rpm;
            }
            Task::none()
        }

        Message::GearCommitted(index) => {
            let Some(gear) = state.gears.get(index).cloned() else {
                return Task::none();
            };

            let client = state.client.clone();
            let write = state.ask(move || client.set_gear(&gear.name, gear.rpm), Answer::Acked);

            // The cooler rounds what it stores, so read the table back.
            Task::batch([write, state.load_gears()])
        }

        Message::CurveAdded => {
            let sensor = state
                .sensors
                .first()
                .map(|choice| choice.sensor.clone())
                .unwrap_or_default();

            if let Some(config) = state.config.as_mut() {
                config.curves.push(Curve {
                    name: String::new(),
                    sensor,
                    points: vec![
                        Point {
                            temp_c: 50,
                            rpm: 500,
                        },
                        Point {
                            temp_c: 80,
                            rpm: 2600,
                        },
                    ],
                    panic_c: None,
                });
                state.selected = config.curves.len() - 1;
            }

            state.name_draft = None;
            state.push()
        }

        Message::CurveRemoved => {
            if let Some(config) = state.config.as_mut() {
                // Nothing left to follow is a way to cook a machine.
                if config.curves.len() > 1 && state.selected < config.curves.len() {
                    config.curves.remove(state.selected);
                    state.selected = state.selected.min(config.curves.len() - 1);
                }
            }

            state.name_draft = None;
            state.push()
        }

        Message::CurveRenamed(name) => {
            state.name_draft = Some(name);
            Task::none()
        }

        Message::CurveNameCommitted => {
            let Some(name) = state.name_draft.take() else {
                return Task::none();
            };

            let selected = state.selected;
            if let Some(curve) = state
                .config
                .as_mut()
                .and_then(|config| config.curves.get_mut(selected))
            {
                curve.name = name.trim().to_string();
            }

            state.push()
        }

        Message::CurveSensorPicked(choice) => {
            let selected = state.selected;
            if let Some(curve) = state
                .config
                .as_mut()
                .and_then(|config| config.curves.get_mut(selected))
            {
                curve.sensor = choice.sensor;
            }

            state.push()
        }

        Message::CurvePanicChanged(panic_c) => {
            let selected = state.selected;
            if let Some(curve) = state
                .config
                .as_mut()
                .and_then(|config| config.curves.get_mut(selected))
            {
                curve.panic_c = Some(panic_c);
            }
            Task::none()
        }

        Message::CurvePanicCommitted => state.push(),

        Message::ColorPicked(hsv) => {
            state.picked = hsv;
            state.light.mode = LightMode::Static {
                color: hsv.to_rgb(),
            };
            Task::none()
        }

        Message::ColorCommitted => state.apply_light(),

        Message::ModePicked(mode) => {
            state.light.mode = mode;
            if let LightMode::Static { color } = mode {
                state.picked = Hsv::from_rgb(color);
            }
            state.apply_light()
        }

        Message::BrightnessChanged(brightness) => {
            state.light.brightness = brightness;
            Task::none()
        }

        Message::BrightnessCommitted => state.apply_light(),

        Message::IndicatorsToggled(on) => {
            state.light.indicators = on;
            state.apply_light()
        }

        Message::FollowScreensToggled(on) => {
            let Some(config) = state.config.as_mut() else {
                return Task::none();
            };

            config.lights_follow_screens = on;
            state.push()
        }

        Message::StandbySelected(standby) => {
            if let Some(config) = state.config.as_mut() {
                config.standby = Some(standby);
            }

            let client = state.client.clone();
            state.ask(move || client.set_standby(standby), Answer::Acked)
        }

        // The daemon holds the config, and on NixOS the file it reads is a
        // store path nobody can edit. Handing it over as text is what makes a
        // curve dragged into shape here reusable anywhere else.
        Message::ExportConfig => {
            let Some(config) = state.config.as_ref() else {
                return Task::none();
            };

            match toml::to_string_pretty(config) {
                Ok(text) => {
                    state.note = Some(Note::new("Configuration copied as TOML".to_string()));
                    iced::clipboard::write(text)
                }
                Err(err) => {
                    state.note = Some(Note::new(err.to_string()));
                    Task::none()
                }
            }
        }

        Message::Undo => state.step(true),

        Message::Redo => state.step(false),

        Message::Ticked => {
            if state.note.as_ref().is_some_and(|note| note.left() <= 0.0) {
                state.note = None;
            }
            Task::none()
        }
    }
}

fn view(state: &State) -> Element<'_, Message> {
    let side = column![
        speed_card(state),
        manual_card(state),
        standby_card(state),
        curve_list(state)
    ]
    .spacing(12)
    .width(Length::Fixed(320.0));

    let pane = match state.tab {
        Tab::Curve => editor_pane(state),
        Tab::Gears => gears_pane(state),
        Tab::Light => light_pane(state),
    };

    let right = column![tabs(state), pane].spacing(10).width(Length::Fill);

    let mut screen = column![row![side, right].spacing(12)]
        .spacing(10)
        .padding(12);

    if state.config.is_some() && !state.writable {
        screen = screen.push(
            text("Changes apply now; the daemon cannot save them, so a restart forgets them")
                .size(11),
        );
    }

    if let Some(note) = &state.note {
        screen = screen.push(
            container(
                column![
                    text(note.text.clone()).size(13),
                    progress_bar(0.0..=1.0, note.left())
                        .girth(2)
                        .length(Length::Fill),
                ]
                .spacing(6),
            )
            .padding(10)
            .width(Length::Fill)
            .style(container::bordered_box),
        );
    }

    screen.into()
}

fn speed_card(state: &State) -> Element<'_, Message> {
    let Some(status) = state.status.as_ref() else {
        return card(
            column![
                text("No daemon").size(18),
                text(format!(
                    "Nothing answering on {}",
                    state.client.socket().display()
                ))
                .size(12),
                action("Retry", button::primary, Message::Reload),
            ]
            .spacing(8)
            .into(),
        );
    };

    if !status.connected {
        return card(
            column![
                text("No cooler").size(18),
                text("The daemon is running but nothing is paired").size(12),
            ]
            .spacing(6)
            .into(),
        );
    }

    let current = status
        .current_rpm
        .map_or("-".to_string(), |rpm| rpm.to_string());
    let target = status
        .target_rpm
        .map_or("-".to_string(), |rpm| format!("{rpm} rpm"));

    let supply = match (&status.supply, status.supply_max_rpm) {
        (Some(supply), Some(max)) => format!("supply {supply}, up to {max} rpm"),
        _ => "supply unknown".to_string(),
    };

    let leading = match (&status.leading, status.manual) {
        (_, true) => "held by hand".to_string(),
        (Some(name), _) => format!("led by {name}"),
        (None, _) => "no reading yet".to_string(),
    };

    card(
        column![
            text(status.model.clone()).size(18),
            text(format!("{current} rpm")).size(34),
            text(format!("target {target}")).size(13),
            text(leading).size(13),
            text(supply).size(12),
        ]
        .spacing(4)
        .into(),
    )
}

fn manual_card(state: &State) -> Element<'_, Message> {
    let manual = match state.manual_intent {
        Some(intent) => intent,
        None => state.status.as_ref().and_then(|status| status.manual_rpm),
    };

    let mut inner = column![checkbox(manual.is_some())
        .label("Hold a fixed speed")
        .on_toggle(Message::ManualToggled)]
    .spacing(8);

    if let Some(rpm) = manual {
        let ceiling = state.ceiling();
        inner = inner.push(text(format!("{rpm} rpm")).size(13));
        inner = inner.push(
            slider(MIN_RPM..=ceiling, rpm.min(ceiling), Message::ManualChanged)
                .step(50u16)
                .on_release(Message::ManualCommitted),
        );
    }

    card(inner.into())
}

fn curve_list(state: &State) -> Element<'_, Message> {
    let Some(config) = state.config.as_ref() else {
        return card(text("No config").size(14).into());
    };

    let mut heading = row![text("Curves").size(15).width(Length::Fill)]
        .spacing(6)
        .align_y(iced::Alignment::Center);

    if !state.history.is_empty() {
        heading = heading.push(action("Undo", button::secondary, Message::Undo));
    }

    if !state.future.is_empty() {
        heading = heading.push(action("Redo", button::secondary, Message::Redo));
    }

    let mut list = column![heading
        .push(action("Export", button::secondary, Message::ExportConfig))
        .push(action("Add", button::secondary, Message::CurveAdded))]
    .spacing(6);

    for (index, curve) in config.curves.iter().enumerate() {
        let name = curve::describe(curve, index);
        let demand = state
            .status
            .as_ref()
            .and_then(|status| status.demands.iter().find(|demand| demand.name == name));

        let detail = match demand {
            Some(demand) => format!("{} C  ->  {} rpm", demand.temp_c, demand.rpm),
            None => "no reading".to_string(),
        };

        let leading = state
            .status
            .as_ref()
            .and_then(|status| status.leading.as_deref())
            == Some(name.as_str());

        let label = column![text(name).size(14), text(detail).size(12)].spacing(2);

        let style = if index == state.selected {
            button::primary
        } else if leading {
            button::success
        } else {
            button::secondary
        };

        list = list.push(
            button(label)
                .width(Length::Fill)
                .style(style)
                .on_press(Message::CurveSelected(index)),
        );
    }

    card(scrollable(list).height(Length::Fill).into())
}

fn editor_pane(state: &State) -> Element<'_, Message> {
    let Some(config) = state.config.as_ref() else {
        return card(text("Waiting for the daemon").into());
    };

    let Some(curve) = config.curves.get(state.selected) else {
        return card(text("No curve selected").into());
    };

    let name = curve::describe(curve, state.selected);
    let demand = state
        .status
        .as_ref()
        .and_then(|status| status.demands.iter().find(|demand| demand.name == name));

    let graph = canvas(editor::Editor {
        points: &curve.points,
        max_rpm: state.ceiling(),
        reading_c: demand.map(|demand| demand.temp_c),
        demand_rpm: demand.map(|demand| demand.rpm),
    })
    .width(Length::Fill)
    .height(Length::Fill);

    let sensor = if curve.sensor.label.is_empty() {
        format!("{}, hottest input", curve.sensor.hwmon)
    } else {
        format!("{}/{}", curve.sensor.hwmon, curve.sensor.label)
    };

    let chosen = state
        .sensors
        .iter()
        .find(|choice| choice.sensor == curve.sensor)
        .cloned()
        .or_else(|| {
            // A curve can address something the picker has no entry for: a
            // sensor this machine does not have, or one written by hand in a
            // form the list does not offer. Only the first deserves a warning.
            let known = sensor_exists(&state.available, &curve.sensor);

            Some(SensorChoice {
                label: if known {
                    sensor.clone()
                } else {
                    format!("{sensor} (missing)")
                },
                sensor: curve.sensor.clone(),
            })
        });

    let panic_c = curve.panic_c.unwrap_or(config.smoothing.panic_c);

    card(
        column![
            row![
                text_input(
                    "name this curve",
                    state.name_draft.as_ref().unwrap_or(&curve.name)
                )
                .on_input(Message::CurveRenamed)
                .on_submit(Message::CurveNameCommitted)
                .width(Length::Fixed(200.0)),
                pick_list(state.sensors.clone(), chosen, Message::CurveSensorPicked)
                    .width(Length::Fill),
                action("Remove", button::danger, Message::CurveRemoved),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            row![
                text(format!("Panic at {panic_c} C")).width(Length::Fixed(120.0)),
                slider(40u8..=110, panic_c, Message::CurvePanicChanged)
                    .on_release(Message::CurvePanicCommitted)
                    .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            graph,
            row![
                text("Drag a point to move it, click the graph to add one, right click to remove")
                    .size(12)
                    .width(Length::Fill),
                text(format!("above {panic_c} C the reading skips smoothing")).size(12),
            ],
        ]
        .spacing(8)
        .into(),
    )
}

fn tabs(state: &State) -> Element<'_, Message> {
    let tab = |label: &'static str, which: Tab| {
        action(
            label,
            if state.tab == which {
                button::primary
            } else {
                button::secondary
            },
            Message::TabSelected(which),
        )
    };

    let mut bar = row![
        tab("Curve", Tab::Curve),
        tab("Gears", Tab::Gears),
        tab("Light", Tab::Light),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    if let Some(since) = state.working_since {
        bar = bar.push(
            container(
                canvas(spinner::Spinner {
                    elapsed: since.elapsed().as_secs_f32(),
                })
                .width(Length::Fixed(16.0))
                .height(Length::Fixed(16.0)),
            )
            .width(Length::Fill)
            .align_right(Length::Fill),
        );
    }

    bar.into()
}

fn standby_card(state: &State) -> Element<'_, Message> {
    let current = state.config.as_ref().and_then(|config| config.standby);

    card(
        column![
            text("When the host goes away").size(14),
            pick_list(
                [Standby::Off, Standby::Instant, Standby::Delayed],
                current,
                Message::StandbySelected
            )
            .width(Length::Fill),
            text("Stored in the cooler, so it still applies once this machine is off").size(11),
        ]
        .spacing(6)
        .into(),
    )
}

fn gears_pane(state: &State) -> Element<'_, Message> {
    if state.gears.is_empty() && state.busy() {
        return card(text("Reading the gear table").size(15).into());
    }

    if state.gears.is_empty() {
        return card(
            column![
                text("No gear table").size(15),
                text("The cooler answers this one itself, so it has to be connected").size(12),
                action("Retry", button::primary, Message::TabSelected(Tab::Gears)),
            ]
            .spacing(8)
            .into(),
        );
    }

    let ceiling = state.ceiling();
    let mut list =
        column![
        text("Speeds stored in the cooler").size(16),
        text("These are what the button on the cooler cycles through, and they survive a reconnect")
            .size(12),
    ]
        .spacing(6);

    for (index, gear) in state.gears.iter().enumerate() {
        let note = if gear.allowed {
            String::new()
        } else {
            "  needs more power".to_string()
        };

        list = list.push(
            column![
                row![
                    text(format!("{}{note}", gear.name)).width(Length::Fill),
                    text(format!("{} rpm", gear.rpm)),
                ],
                slider(MIN_RPM..=ceiling, gear.rpm.min(ceiling), move |rpm| {
                    Message::GearMoved { index, rpm }
                })
                .step(50u16)
                .on_release(Message::GearCommitted(index)),
            ]
            .spacing(4),
        );
    }

    card(scrollable(list.spacing(14)).height(Length::Fill).into())
}

fn light_pane(state: &State) -> Element<'_, Message> {
    let showing = state.status.as_ref().and_then(|status| status.lighting);
    let strip_on = state.status.as_ref().and_then(|status| status.strip_on);

    // Marked from the draft rather than from the status: lighting takes the
    // cooler the best part of a second, and a button that only lights up once
    // that is over reads as a click that did nothing.
    let mode = |label: String, which: LightMode| {
        action(
            label,
            if state.light.mode == which {
                button::primary
            } else {
                button::secondary
            },
            Message::ModePicked(which),
        )
    };

    // Nothing can be asked what pattern the strip is playing, so when the
    // daemon has not set one this session, say that instead of inventing it.
    let state_line = match (showing, strip_on) {
        (Some(showing), _) => format!("showing {showing}"),
        (None, Some(true)) => "lit, pattern set before this session".to_string(),
        (None, Some(false)) => "strip is off".to_string(),
        (None, None) => "unknown".to_string(),
    };

    let mut effects = row![text("Animations").width(Length::Fixed(90.0))]
        .spacing(6)
        .align_y(iced::Alignment::Center);

    for effect in 1..=EFFECT_COUNT {
        effects = effects.push(mode(effect.to_string(), LightMode::Effect { effect }));
    }

    let swatch = match state.light.mode {
        LightMode::Static { color } => picker::color(color),
        _ => iced::Color::TRANSPARENT,
    };

    let drafted = state.picked.to_rgb();

    card(
        column![
            row![
                text("Side strip").size(16).width(Length::Fill),
                text(state_line).size(13),
            ],
            row![
                canvas(picker::Shades { hsv: state.picked })
                    .width(Length::Fixed(260.0))
                    .height(Length::Fixed(150.0)),
                column![
                    container(text(""))
                        .width(Length::Fixed(60.0))
                        .height(Length::Fixed(60.0))
                        .style(move |_: &Theme| container::Style {
                            background: Some(swatch.into()),
                            border: iced::border::rounded(4),
                            ..container::Style::default()
                        }),
                    text(format!(
                        "#{:02x}{:02x}{:02x}",
                        drafted.r, drafted.g, drafted.b
                    ))
                    .size(12),
                ]
                .spacing(6),
            ]
            .spacing(12),
            canvas(picker::Hues { hsv: state.picked })
                .width(Length::Fixed(260.0))
                .height(Length::Fixed(20.0)),
            row![
                text("Brightness").width(Length::Fixed(90.0)),
                slider(
                    0u8..=100,
                    state.light.brightness,
                    Message::BrightnessChanged
                )
                .on_release(Message::BrightnessCommitted)
                .width(Length::Fill),
                text(format!("{:3}%", state.light.brightness)).width(Length::Fixed(40.0)),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            effects,
            row![
                text("").width(Length::Fixed(90.0)),
                mode(
                    "Colour".to_string(),
                    LightMode::Static {
                        color: state.picked.to_rgb()
                    }
                ),
                mode("Off".to_string(), LightMode::Off),
            ]
            .spacing(6),
            text(
                "Brightness applies to whichever of these is running, so dimming an animation \
                 keeps it animated"
            )
            .size(11),
            checkbox(state.light.indicators)
                .label("Gear indicator LEDs")
                .on_toggle(Message::IndicatorsToggled),
            checkbox(
                state
                    .config
                    .as_ref()
                    .is_some_and(|config| config.lights_follow_screens)
            )
            .label("Go dark with the screens")
            .on_toggle(Message::FollowScreensToggled),
            text("Both lights go out while every display is off, and come back with the first one that lights up")
                .size(11),
        ]
        .spacing(10)
        .into(),
    )
}

fn action<'a>(
    label: impl text::IntoFragment<'a>,
    style: fn(&Theme, button::Status) -> button::Style,
    message: Message,
) -> iced::widget::Button<'a, Message> {
    button(text(label)).style(style).on_press(message)
}

fn card(content: Element<'_, Message>) -> Element<'_, Message> {
    container(content)
        .padding(12)
        .width(Length::Fill)
        .style(container::bordered_box)
        .into()
}
