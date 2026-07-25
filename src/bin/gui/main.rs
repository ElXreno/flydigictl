//! Desktop front end for the fan curve daemon.

mod client;
mod editor;

use std::path::PathBuf;
use std::time::Duration;

use iced::futures::{SinkExt, Stream, StreamExt};
use iced::widget::{button, canvas, checkbox, column, container, row, scrollable, slider, text};
use iced::{Element, Length, Subscription, Theme};

use flydigictl::config::{Config, Point};
use flydigictl::curve;
use flydigictl::ipc::{self, Status, Warning};
use flydigictl::protocol::{MAX_RPM, MIN_RPM};

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
    ManualChanged(u16),
    Reload,
}

struct State {
    client: Client,
    status: Option<Status>,
    config: Option<Config>,
    writable: bool,
    selected: usize,
    /// Last thing worth telling the user, from either side of the socket.
    note: Option<String>,
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
        };
        state.reload();
        state
    }

    fn reload(&mut self) {
        match self.client.config() {
            Ok((config, writable)) => {
                self.selected = self.selected.min(config.curves.len().saturating_sub(1));
                self.config = Some(config);
                self.writable = writable;
                if !writable {
                    self.note = Some(
                        "The daemon cannot write its config, so changes are live but forgotten on restart"
                            .to_string(),
                    );
                }
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

        match self.client.set_config(config) {
            Ok(Some(Warning { message, .. })) => self.note = Some(message),
            Ok(None) => self.note = None,
            Err(err) => self.note = Some(err),
        }
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

fn theme(_state: &State) -> Theme {
    Theme::CatppuccinMacchiato
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
            match state.client.set_manual(rpm) {
                Ok(Some(Warning { message, .. })) => state.note = Some(message),
                Ok(None) => state.note = None,
                Err(err) => state.note = Some(err),
            }
        }

        Message::ManualChanged(rpm) => {
            if let Some(config) = state.config.as_mut() {
                config.manual_rpm = Some(rpm);
            }
            match state.client.set_manual(Some(rpm)) {
                Ok(Some(Warning { message, .. })) => state.note = Some(message),
                Ok(None) => state.note = None,
                Err(err) => state.note = Some(err),
            }
        }
    }
}

fn view(state: &State) -> Element<'_, Message> {
    let side = column![speed_card(state), manual_card(state), curve_list(state)]
        .spacing(12)
        .width(Length::Fixed(300.0));

    let mut screen = column![row![side, editor_pane(state)].spacing(12)]
        .spacing(10)
        .padding(12);

    if let Some(note) = &state.note {
        screen = screen.push(
            container(text(note.clone()).size(13))
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
        inner = inner
            .push(slider(MIN_RPM..=ceiling, rpm.min(ceiling), Message::ManualChanged).step(50u16));
    }

    card(inner.into())
}

fn curve_list(state: &State) -> Element<'_, Message> {
    let Some(config) = state.config.as_ref() else {
        return card(text("No config").size(14).into());
    };

    let mut list = column![text("Curves").size(15)].spacing(6);

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

    card(
        column![
            row![
                column![text(name).size(17), text(sensor).size(12)].spacing(2),
                container(text(format!(
                    "panic at {} C",
                    curve.panic_c.unwrap_or(config.smoothing.panic_c)
                )))
                .width(Length::Fill)
                .align_right(Length::Fill),
            ],
            graph,
            text("Drag a point to move it, click the graph to add one, right click to remove")
                .size(12),
        ]
        .spacing(8)
        .into(),
    )
}

fn card(content: Element<'_, Message>) -> Element<'_, Message> {
    container(content)
        .padding(12)
        .width(Length::Fill)
        .style(container::bordered_box)
        .into()
}
