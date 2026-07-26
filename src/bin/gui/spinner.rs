//! A turning arc, shown while the daemon is being waited on.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{Point, Radians, Rectangle, Renderer, Theme};

use crate::Message;

/// One turn per second and a bit, which reads as working rather than stuck.
const TURN: f32 = 1.4;

pub struct Spinner {
    pub elapsed: f32,
}

impl canvas::Program<Message> for Spinner {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let palette = theme.extended_palette();

        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let radius = bounds.width.min(bounds.height) / 2.0 - 2.0;
        let start = self.elapsed / TURN * std::f32::consts::TAU;

        let track = Path::new(|path| {
            path.arc(canvas::path::Arc {
                center,
                radius,
                start_angle: Radians(0.0),
                end_angle: Radians(std::f32::consts::TAU),
            });
        });

        let sweep = Path::new(|path| {
            path.arc(canvas::path::Arc {
                center,
                radius,
                start_angle: Radians(start),
                end_angle: Radians(start + std::f32::consts::TAU * 0.3),
            });
        });

        frame.stroke(
            &track,
            Stroke::default()
                .with_color(palette.background.strong.color)
                .with_width(2.0),
        );

        frame.stroke(
            &sweep,
            Stroke::default()
                .with_color(palette.primary.base.color)
                .with_width(2.0),
        );

        vec![frame.into_geometry()]
    }
}
