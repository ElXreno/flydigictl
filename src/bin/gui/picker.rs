//! A colour picker, drawn rather than typed.
//!
//! Two canvases: a square that picks saturation and value for one hue, and a
//! strip that picks the hue. Both are gradients the renderer already knows how
//! to draw, so this needs no widget library beyond what iced ships.

use iced::mouse;
use iced::widget::canvas::{self, gradient, Frame, Geometry, Path, Stroke};
use iced::{Color, Point, Rectangle, Renderer, Theme};

use flydigictl::protocol::Rgb;

use crate::Message;

/// The picker works in HSV because that is what a person adjusts: pick the
/// colour, then how pale and how bright it is.
#[derive(Debug, Clone, Copy)]
pub struct Hsv {
    pub hue: f32,
    pub saturation: f32,
    pub value: f32,
}

impl Hsv {
    pub fn from_rgb(rgb: Rgb) -> Self {
        let r = f32::from(rgb.r) / 255.0;
        let g = f32::from(rgb.g) / 255.0;
        let b = f32::from(rgb.b) / 255.0;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let span = max - min;

        let hue = if span == 0.0 {
            0.0
        } else if max == r {
            60.0 * (((g - b) / span) % 6.0)
        } else if max == g {
            60.0 * ((b - r) / span + 2.0)
        } else {
            60.0 * ((r - g) / span + 4.0)
        };

        Self {
            hue: if hue < 0.0 { hue + 360.0 } else { hue },
            saturation: if max == 0.0 { 0.0 } else { span / max },
            value: max,
        }
    }

    pub fn to_rgb(self) -> Rgb {
        let chroma = self.value * self.saturation;
        let sector = self.hue / 60.0;
        let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
        let base = self.value - chroma;

        let (r, g, b) = match sector as u32 {
            0 => (chroma, second, 0.0),
            1 => (second, chroma, 0.0),
            2 => (0.0, chroma, second),
            3 => (0.0, second, chroma),
            4 => (second, 0.0, chroma),
            _ => (chroma, 0.0, second),
        };

        Rgb {
            r: ((r + base) * 255.0).round() as u8,
            g: ((g + base) * 255.0).round() as u8,
            b: ((b + base) * 255.0).round() as u8,
        }
    }

    fn pure_hue(self) -> Color {
        let rgb = Self {
            hue: self.hue,
            saturation: 1.0,
            value: 1.0,
        }
        .to_rgb();

        Color::from_rgb8(rgb.r, rgb.g, rgb.b)
    }
}

pub fn color(rgb: Rgb) -> Color {
    Color::from_rgb8(rgb.r, rgb.g, rgb.b)
}

/// Saturation across, value down.
pub struct Shades {
    pub hsv: Hsv,
}

/// Hue from end to end.
pub struct Hues {
    pub hsv: Hsv,
}

#[derive(Default)]
pub struct Held {
    down: bool,
}

/// Both canvases behave the same way: press to set, drag to keep setting,
/// release to commit. Only the reading of the position differs.
fn track(
    state: &mut Held,
    event: &canvas::Event,
    bounds: Rectangle,
    cursor: mouse::Cursor,
    read: impl Fn(Point) -> Message,
) -> Option<canvas::Action<Message>> {
    let canvas::Event::Mouse(event) = event else {
        return None;
    };

    match event {
        mouse::Event::ButtonPressed(mouse::Button::Left) => {
            let position = cursor.position_in(bounds)?;
            state.down = true;
            Some(canvas::Action::publish(read(position)).and_capture())
        }

        mouse::Event::CursorMoved { .. } => {
            if !state.down {
                return None;
            }
            let position = cursor.position()?;
            Some(
                canvas::Action::publish(read(Point::new(
                    position.x - bounds.x,
                    position.y - bounds.y,
                )))
                .and_capture(),
            )
        }

        mouse::Event::ButtonReleased(mouse::Button::Left) => {
            if !state.down {
                return None;
            }
            state.down = false;
            Some(canvas::Action::publish(Message::ColorCommitted).and_capture())
        }

        _ => None,
    }
}

fn marker(frame: &mut Frame, at: Point, radius: f32) {
    frame.stroke(
        &Path::circle(at, radius),
        Stroke::default().with_color(Color::BLACK).with_width(3.0),
    );
    frame.stroke(
        &Path::circle(at, radius),
        Stroke::default().with_color(Color::WHITE).with_width(1.5),
    );
}

impl canvas::Program<Message> for Shades {
    type State = Held;

    fn update(
        &self,
        state: &mut Held,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let hsv = self.hsv;

        track(state, event, bounds, cursor, move |position| {
            Message::ColorPicked(Hsv {
                saturation: (position.x / bounds.width).clamp(0.0, 1.0),
                value: 1.0 - (position.y / bounds.height).clamp(0.0, 1.0),
                ..hsv
            })
        })
    }

    fn draw(
        &self,
        _state: &Held,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let area = Path::rectangle(Point::ORIGIN, bounds.size());

        // White to the hue across, then shaded to black going down: the same
        // two passes every colour square is made of.
        frame.fill(
            &area,
            gradient::Linear::new(Point::ORIGIN, Point::new(bounds.width, 0.0))
                .add_stop(0.0, Color::WHITE)
                .add_stop(1.0, self.hsv.pure_hue()),
        );

        frame.fill(
            &area,
            gradient::Linear::new(Point::ORIGIN, Point::new(0.0, bounds.height))
                .add_stop(0.0, Color::TRANSPARENT)
                .add_stop(1.0, Color::BLACK),
        );

        marker(
            &mut frame,
            Point::new(
                self.hsv.saturation * bounds.width,
                (1.0 - self.hsv.value) * bounds.height,
            ),
            6.0,
        );

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Held,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

impl canvas::Program<Message> for Hues {
    type State = Held;

    fn update(
        &self,
        state: &mut Held,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let hsv = self.hsv;

        track(state, event, bounds, cursor, move |position| {
            Message::ColorPicked(Hsv {
                hue: (position.x / bounds.width).clamp(0.0, 1.0) * 360.0,
                // A hue picked out of a black or colourless square would look
                // like nothing happened, so give it something to show.
                saturation: if hsv.saturation == 0.0 {
                    1.0
                } else {
                    hsv.saturation
                },
                value: if hsv.value == 0.0 { 1.0 } else { hsv.value },
            })
        })
    }

    fn draw(
        &self,
        _state: &Held,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        let mut spectrum = gradient::Linear::new(Point::ORIGIN, Point::new(bounds.width, 0.0));
        for step in 0..=6u8 {
            let hue = f32::from(step) * 60.0;
            spectrum = spectrum.add_stop(
                f32::from(step) / 6.0,
                Hsv {
                    hue,
                    saturation: 1.0,
                    value: 1.0,
                }
                .pure_hue(),
            );
        }

        frame.fill(&Path::rectangle(Point::ORIGIN, bounds.size()), spectrum);

        marker(
            &mut frame,
            Point::new(self.hsv.hue / 360.0 * bounds.width, bounds.height / 2.0),
            bounds.height / 2.0 - 2.0,
        );

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Held,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}
