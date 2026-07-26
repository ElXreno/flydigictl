//! Desktop front end for the fan curve daemon.

mod client;
mod editor;
mod picker;
mod theme;

use std::path::PathBuf;
use std::time::Duration;

use iced::futures::{SinkExt, Stream, StreamExt};
use iced::widget::{
    button, canvas, checkbox, column, container, pick_list, row, scrollable, slider, text,
    text_input,
};
use iced::{Element, Length, Subscription, Theme};

use flydigictl::config::{Config, Curve, Point, Sensor};
use flydigictl::curve;
use flydigictl::ipc::{self, Status, Warning, WarningCode};
use flydigictl::protocol::{LightMode, Lighting, Rgb, Standby, EFFECT_COUNT, MAX_RPM, MIN_RPM};
use flydigictl::sensor;

use picker::Hsv;

use client::Client;

/// How long to wait before dialling a daemon that is not there yet.
const RECONNECT: Duration = Duration::from_secs(1);

/// Control a Flydigi BS series cooler
#[derive(argh::FromArgs)]
struct Args {
    /// daemon socket (default: /run/flydigictl/flydigictl.sock)
    #[argh(option, short = 's')]
    socket: Option<PathBuf>,
}

fn main() -> iced::Result {
    let args: Args = argh::from_env();
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

    StandbySelected(Standby),
    DismissNote,
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

/// Every hwmon on the machine, plus a whole-hwmon entry for each: an empty
/// label means "the hottest input of this chip", which is what covers a pair
/// of DIMMs or two drives with one curve.
fn sensor_choices() -> Vec<SensorChoice> {
    let mut choices: Vec<SensorChoice> = Vec::new();

    for entry in sensor::list() {
        let whole = SensorChoice {
            label: format!("{} (hottest)", entry.hwmon),
            sensor: Sensor {
                hwmon: entry.hwmon.clone(),
                label: String::new(),
            },
        };
        if !choices.contains(&whole) {
            choices.push(whole);
        }

        if !entry.label.is_empty() {
            choices.push(SensorChoice {
                label: format!("{}/{}", entry.hwmon, entry.label),
                sensor: Sensor {
                    hwmon: entry.hwmon,
                    label: entry.label,
                },
            });
        }
    }

    choices
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
    note: Option<String>,

    theme: Theme,
    tab: Tab,

    /// Read from the cooler rather than from the config: the gear table lives
    /// in the device and the physical button changes it too.
    gears: Vec<ipc::Gear>,

    /// What the controls are set to, which is not always what the cooler is
    /// showing: the truth is in the status, this is the draft being edited.
    light: Lighting,
    picked: Hsv,

    sensors: Vec<SensorChoice>,
    /// Held while it is being typed, because sending on every keystroke means
    /// a socket round trip per letter.
    name_draft: Option<String>,
}

impl State {
    fn new(socket: PathBuf) -> Self {
        let mut state = Self {
            client: Client::new(socket),
            status: None,
            config: None,
            writable: false,
            selected: 0,
            note: None,
            theme: theme::load(),
            tab: Tab::Curve,
            gears: Vec::new(),
            light: Lighting::default(),
            picked: Hsv::from_rgb(Rgb {
                r: 0x7A,
                g: 0xA2,
                b: 0xF7,
            }),
            sensors: sensor_choices(),
            name_draft: None,
        };
        state.reload();
        state
    }

    fn load_gears(&mut self) {
        match self.client.gears() {
            Ok(gears) => self.gears = gears,
            Err(err) => self.note = Some(err),
        }
    }

    /// Everything but the config goes through here: the reply is either a
    /// warning worth showing or nothing worth saying.
    fn report(&mut self, outcome: Result<Option<Warning>, String>) {
        self.note = match outcome {
            // Already on screen for as long as it holds, so repeating it here
            // would only push the useful half of the reply out of view.
            Ok(Some(Warning {
                code: WarningCode::ConfigReadOnly,
                ..
            })) => None,
            Ok(Some(Warning { message, .. })) => Some(message),
            Ok(None) => None,
            Err(err) => Some(err),
        };
    }

    fn reload(&mut self) {
        match self.client.config() {
            Ok((config, writable)) => {
                self.selected = self.selected.min(config.curves.len().saturating_sub(1));
                self.config = Some(config);
                self.writable = writable;
            }
            Err(err) => self.note = Some(err),
        }
    }

    /// Push the edited config back. The daemon sorts the points and applies the
    /// result immediately, so there is nothing to apply separately.
    fn push(&mut self) {
        let Some(config) = self.config.clone() else {
            return;
        };

        let outcome = self.client.set_config(config);
        self.report(outcome);
    }

    /// Send the draft as it stands. The daemon works out which reports that
    /// actually needs, so a brightness nudge does not restart an animation it
    /// did not have to.
    fn apply_light(&mut self) {
        let outcome = self.client.set_lighting(self.light);
        self.report(outcome);
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

fn title(state: &State) -> String {
    match state.status.as_ref() {
        Some(status) => format!("{} - flydigictl", status.model),
        None => "flydigictl".to_string(),
    }
}

fn theme(state: &State) -> Theme {
    state.theme.clone()
}

/// Updates arrive when the cooler has something to say, not on a timer.
///
/// The reading itself is blocking, so it lives on its own thread and reaches
/// the interface through a channel. Identifying the subscription by the socket
/// path means it restarts by itself if that ever changes.
fn subscription(state: &State) -> Subscription<Message> {
    Subscription::run_with(Socket(state.client.socket().to_path_buf()), updates)
}

/// Identity of the subscription, which iced compares to decide whether the
/// running one still matches what the application asked for.
#[derive(Hash)]
struct Socket(PathBuf);

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

fn update(state: &mut State, message: Message) {
    match message {
        Message::Live(status) => {
            // The cooler cannot be asked what it is showing, so the daemon's
            // record is the only starting point the controls have.
            if state.status.is_none() {
                state.light = status.lighting;
                if let LightMode::Static { color } = status.lighting.mode {
                    state.picked = Hsv::from_rgb(color);
                }
            }
            state.status = Some(*status);
            // A daemon that came back up may be running a different config.
            if state.config.is_none() {
                state.reload();
            }
        }

        // The daemon itself is unreachable, so its config is no longer known
        // either; a cooler that merely went away arrives as a status instead.
        Message::Offline => {
            state.status = None;
            state.config = None;
        }

        Message::Reload => state.reload(),

        Message::CurveSelected(index) => state.selected = index,

        Message::PointAdded(point) => {
            if let Some(points) = state.points_mut() {
                points.push(point);
                points.sort_by_key(|point| point.temp_c);
            }
            state.push();
        }

        // Points are left in place while the pointer is down: sorting mid-drag
        // would renumber them under the hand doing the dragging.
        Message::PointMoved { index, point } => {
            if let Some(points) = state.points_mut() {
                if let Some(slot) = points.get_mut(index) {
                    *slot = point;
                }
            }
        }

        Message::PointRemoved(index) => {
            if let Some(points) = state.points_mut() {
                // A curve with nothing left in it silently stops asking for
                // air, so keep the last point.
                if points.len() > 1 && index < points.len() {
                    points.remove(index);
                }
            }
            state.push();
        }

        Message::PointsSettled => {
            if let Some(points) = state.points_mut() {
                points.sort_by_key(|point| point.temp_c);
            }
            state.push();
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

            if let Some(config) = state.config.as_mut() {
                config.manual_rpm = rpm;
            }
            let outcome = state.client.set_manual(rpm);
            state.report(outcome);
        }

        Message::ManualChanged(rpm) => {
            if let Some(config) = state.config.as_mut() {
                config.manual_rpm = Some(rpm);
            }
        }

        Message::ManualCommitted => {
            let rpm = state.config.as_ref().and_then(|config| config.manual_rpm);
            if let Some(rpm) = rpm {
                let outcome = state.client.set_manual(Some(rpm));
                state.report(outcome);
            }
        }

        Message::TabSelected(tab) => {
            state.tab = tab;
            if tab == Tab::Gears {
                state.load_gears();
            }
        }

        Message::GearMoved { index, rpm } => {
            if let Some(gear) = state.gears.get_mut(index) {
                gear.rpm = rpm;
            }
        }

        Message::GearCommitted(index) => {
            let Some(gear) = state.gears.get(index).cloned() else {
                return;
            };
            let outcome = state.client.set_gear(&gear.name, gear.rpm);
            state.report(outcome);

            // The cooler rounds and clamps what it stores, so take its word
            // for the table rather than our own.
            state.load_gears();
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
            state.push();
        }

        Message::CurveRemoved => {
            if let Some(config) = state.config.as_mut() {
                // Removing the last one would leave the cooler with nothing to
                // follow, which is a way to cook a machine by accident.
                if config.curves.len() > 1 && state.selected < config.curves.len() {
                    config.curves.remove(state.selected);
                    state.selected = state.selected.min(config.curves.len() - 1);
                }
            }
            state.name_draft = None;
            state.push();
        }

        Message::CurveRenamed(name) => state.name_draft = Some(name),

        Message::CurveNameCommitted => {
            let Some(name) = state.name_draft.take() else {
                return;
            };
            let selected = state.selected;
            if let Some(curve) = state
                .config
                .as_mut()
                .and_then(|config| config.curves.get_mut(selected))
            {
                curve.name = name.trim().to_string();
            }
            state.push();
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
            state.push();
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
        }

        Message::CurvePanicCommitted => state.push(),

        Message::ColorPicked(hsv) => {
            state.picked = hsv;
            state.light.mode = LightMode::Static {
                color: hsv.to_rgb(),
            };
        }

        Message::ColorCommitted => state.apply_light(),

        Message::ModePicked(mode) => {
            state.light.mode = mode;
            if let LightMode::Static { color } = mode {
                state.picked = Hsv::from_rgb(color);
            }
            state.apply_light();
        }

        Message::BrightnessChanged(brightness) => state.light.brightness = brightness,

        Message::BrightnessCommitted => state.apply_light(),

        Message::IndicatorsToggled(on) => {
            state.light.indicators = on;
            state.apply_light();
        }

        Message::DismissNote => state.note = None,

        Message::StandbySelected(standby) => {
            if let Some(config) = state.config.as_mut() {
                config.standby = Some(standby);
            }
            let outcome = state.client.set_standby(standby);
            state.report(outcome);
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
    .width(Length::Fixed(300.0));

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
        screen = screen.push(banner(
            "The daemon cannot save its config: changes apply now and are forgotten on restart",
            None,
        ));
    }

    if let Some(note) = &state.note {
        screen = screen.push(banner(note, Some(Message::DismissNote)));
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
                button(text("Retry")).on_press(Message::Reload),
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
    let manual = state.config.as_ref().and_then(|config| config.manual_rpm);

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

    let mut list = column![row![
        text("Curves").size(15).width(Length::Fill),
        button(text("Add"))
            .style(button::secondary)
            .on_press(Message::CurveAdded),
    ]
    .align_y(iced::Alignment::Center)]
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
            // A curve can name a sensor this machine does not have, and the
            // picker should say so rather than look empty.
            Some(SensorChoice {
                label: format!("{sensor} (missing)"),
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
                button(text("Remove"))
                    .style(button::danger)
                    .on_press(Message::CurveRemoved),
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
        button(text(label))
            .style(if state.tab == which {
                button::primary
            } else {
                button::secondary
            })
            .on_press(Message::TabSelected(which))
    };

    row![
        tab("Curve", Tab::Curve),
        tab("Gears", Tab::Gears),
        tab("Light", Tab::Light),
    ]
    .spacing(6)
    .into()
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
    if state.gears.is_empty() {
        return card(
            column![
                text("No gear table").size(15),
                text("The cooler answers this one itself, so it has to be connected").size(12),
                button(text("Retry")).on_press(Message::TabSelected(Tab::Gears)),
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
    let showing = state
        .status
        .as_ref()
        .map(|status| status.lighting)
        .unwrap_or_default();

    let mode = |label: String, which: LightMode| {
        button(text(label))
            .style(if showing.mode == which {
                button::primary
            } else {
                button::secondary
            })
            .on_press(Message::ModePicked(which))
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
                text(format!("showing {showing}")).size(13),
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
        ]
        .spacing(10)
        .into(),
    )
}

fn banner(message: &str, dismiss: Option<Message>) -> Element<'_, Message> {
    let mut line = row![text(message.to_string()).size(13).width(Length::Fill)].spacing(8);

    if let Some(dismiss) = dismiss {
        line = line.push(
            button(text("Dismiss"))
                .style(button::secondary)
                .on_press(dismiss),
        );
    }

    container(line)
        .padding(10)
        .width(Length::Fill)
        .style(container::bordered_box)
        .into()
}

fn card(content: Element<'_, Message>) -> Element<'_, Message> {
    container(content)
        .padding(12)
        .width(Length::Fill)
        .style(container::bordered_box)
        .into()
}
