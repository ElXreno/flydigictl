//! The fan curve as a draggable graph.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::{Color, Point, Rectangle, Renderer, Theme};

use flydigictl::config::Point as CurvePoint;
use flydigictl::protocol::MIN_RPM;

use crate::Message;

/// Colder than this and no cooler matters; hotter and nothing is fine anyway.
const TEMP_MIN: f32 = 20.0;
const TEMP_MAX: f32 = 100.0;

const PAD_LEFT: f32 = 46.0;
const PAD_RIGHT: f32 = 14.0;
const PAD_TOP: f32 = 14.0;
const PAD_BOTTOM: f32 = 26.0;

/// How close the pointer has to be to grab a point.
const GRAB_RADIUS: f32 = 12.0;

pub struct Editor<'a> {
    pub points: &'a [CurvePoint],
    /// Ceiling of the vertical axis: the supply decides what the fan can do, so
    /// drawing a curve above it would promise a speed the cooler will not run.
    pub max_rpm: u16,
    pub reading_c: Option<u8>,
    pub demand_rpm: Option<u16>,
}

#[derive(Default)]
pub struct Grabbed {
    index: Option<usize>,
}

impl Editor<'_> {
    fn plot(&self, bounds: Rectangle) -> Rectangle {
        Rectangle {
            x: PAD_LEFT,
            y: PAD_TOP,
            width: (bounds.width - PAD_LEFT - PAD_RIGHT).max(1.0),
            height: (bounds.height - PAD_TOP - PAD_BOTTOM).max(1.0),
        }
    }

    fn to_screen(&self, bounds: Rectangle, temp: f32, rpm: f32) -> Point {
        let plot = self.plot(bounds);
        let x = (temp - TEMP_MIN) / (TEMP_MAX - TEMP_MIN);
        let y = rpm / f32::from(self.max_rpm).max(1.0);

        Point::new(
            plot.x + x.clamp(0.0, 1.0) * plot.width,
            plot.y + (1.0 - y.clamp(0.0, 1.0)) * plot.height,
        )
    }

    /// Screen position back to a point, clamped to what the cooler can do.
    ///
    /// Speeds between a stop and [`MIN_RPM`] are snapped away: the fan stalls
    /// there and takes twenty seconds of retrying to spin back up.
    fn to_point(&self, bounds: Rectangle, position: Point) -> CurvePoint {
        let plot = self.plot(bounds);
        let x = ((position.x - plot.x) / plot.width).clamp(0.0, 1.0);
        let y = 1.0 - ((position.y - plot.y) / plot.height).clamp(0.0, 1.0);

        let temp = TEMP_MIN + x * (TEMP_MAX - TEMP_MIN);
        let rpm = y * f32::from(self.max_rpm);
        let rpm = rpm.round() as u16;

        CurvePoint {
            temp_c: temp.round() as u8,
            rpm: if rpm < MIN_RPM / 2 {
                0
            } else {
                rpm.max(MIN_RPM)
            },
        }
    }

    fn nearest(&self, bounds: Rectangle, position: Point) -> Option<usize> {
        self.points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let at = self.to_screen(bounds, f32::from(point.temp_c), f32::from(point.rpm));
                (index, at.distance(position))
            })
            .filter(|(_, distance)| *distance <= GRAB_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    fn sorted(&self) -> Vec<CurvePoint> {
        let mut points = self.points.to_vec();
        points.sort_by_key(|point| point.temp_c);
        points
    }
}

impl canvas::Program<Message> for Editor<'_> {
    type State = Grabbed;

    fn update(
        &self,
        state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let canvas::Event::Mouse(event) = event else {
            return None;
        };

        // Dragging keeps working past the edge of the widget, which is what a
        // pointer flicked at the top of the graph expects.
        let inside = cursor.position_in(bounds);
        let anywhere = cursor.position();

        match event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let position = inside?;

                match self.nearest(bounds, position) {
                    Some(index) => {
                        state.index = Some(index);
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    None => Some(
                        canvas::Action::publish(Message::PointAdded(
                            self.to_point(bounds, position),
                        ))
                        .and_capture(),
                    ),
                }
            }

            mouse::Event::ButtonPressed(mouse::Button::Right) => {
                let index = self.nearest(bounds, inside?)?;
                Some(canvas::Action::publish(Message::PointRemoved(index)).and_capture())
            }

            mouse::Event::CursorMoved { .. } => {
                let index = state.index?;
                let position = anywhere?;
                let local = Point::new(position.x - bounds.x, position.y - bounds.y);

                Some(
                    canvas::Action::publish(Message::PointMoved {
                        index,
                        point: self.to_point(bounds, local),
                    })
                    .and_capture(),
                )
            }

            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                state.index.take()?;
                Some(canvas::Action::publish(Message::PointsSettled).and_capture())
            }

            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let palette = theme.extended_palette();
        let mut frame = Frame::new(renderer, bounds.size());
        let plot = self.plot(bounds);

        let grid = palette.background.weak.color;
        let ink = palette.background.base.text;
        let faded = Color { a: 0.55, ..ink };

        // Horizontal rules every 1000 rpm, vertical every 10 degrees.
        let step = 1000;
        let mut rpm = 0;
        while rpm <= self.max_rpm {
            let at = self.to_screen(bounds, TEMP_MIN, f32::from(rpm));
            frame.stroke(
                &Path::line(at, Point::new(plot.x + plot.width, at.y)),
                Stroke::default().with_color(grid).with_width(1.0),
            );
            frame.fill_text(Text {
                content: rpm.to_string(),
                position: Point::new(6.0, at.y - 7.0),
                color: faded,
                size: 11.0.into(),
                ..Text::default()
            });
            rpm += step;
        }

        let mut temp = TEMP_MIN;
        while temp <= TEMP_MAX {
            let at = self.to_screen(bounds, temp, f32::from(self.max_rpm));
            frame.stroke(
                &Path::line(at, Point::new(at.x, plot.y + plot.height)),
                Stroke::default().with_color(grid).with_width(1.0),
            );
            frame.fill_text(Text {
                content: format!("{temp:.0}"),
                position: Point::new(at.x - 8.0, plot.y + plot.height + 6.0),
                color: faded,
                size: 11.0.into(),
                ..Text::default()
            });
            temp += 10.0;
        }

        // Where the cooler is right now, so a curve can be aimed at reality
        // rather than at guesses.
        if let Some(reading) = self.reading_c {
            let x = self.to_screen(bounds, f32::from(reading), 0.0).x;
            frame.stroke(
                &Path::line(Point::new(x, plot.y), Point::new(x, plot.y + plot.height)),
                Stroke::default()
                    .with_color(palette.success.base.color)
                    .with_width(1.5),
            );

            if let Some(demand) = self.demand_rpm {
                let at = self.to_screen(bounds, f32::from(reading), f32::from(demand));
                frame.fill(
                    &Path::circle(at, 5.0),
                    Color {
                        a: 0.35,
                        ..palette.success.base.color
                    },
                );
            }
        }

        let points = self.sorted();

        if !points.is_empty() {
            let line = Path::new(|builder| {
                let first = &points[0];
                let start = self.to_screen(bounds, TEMP_MIN, f32::from(first.rpm));
                builder.move_to(start);

                for point in &points {
                    builder.line_to(self.to_screen(
                        bounds,
                        f32::from(point.temp_c),
                        f32::from(point.rpm),
                    ));
                }

                let last = points[points.len() - 1];
                builder.line_to(self.to_screen(bounds, TEMP_MAX, f32::from(last.rpm)));
            });

            frame.stroke(
                &line,
                Stroke::default()
                    .with_color(palette.primary.base.color)
                    .with_width(2.0),
            );
        }

        let hovered = cursor
            .position_in(bounds)
            .and_then(|position| self.nearest(bounds, position));

        for (index, point) in self.points.iter().enumerate() {
            let at = self.to_screen(bounds, f32::from(point.temp_c), f32::from(point.rpm));
            let active = state.index == Some(index) || hovered == Some(index);
            let radius = if active { 6.0 } else { 4.0 };

            frame.fill(&Path::circle(at, radius), palette.primary.base.color);

            if active {
                frame.fill_text(Text {
                    content: format!("{} C / {} rpm", point.temp_c, point.rpm),
                    position: Point::new(at.x + 10.0, at.y - 18.0),
                    color: ink,
                    size: 12.0.into(),
                    ..Text::default()
                });
            }
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.index.is_some() {
            return mouse::Interaction::Grabbing;
        }

        match cursor
            .position_in(bounds)
            .and_then(|position| self.nearest(bounds, position))
        {
            Some(_) => mouse::Interaction::Grab,
            None if cursor.is_over(bounds) => mouse::Interaction::Crosshair,
            None => mouse::Interaction::default(),
        }
    }
}
