//! Fan curves and the smoothing in front of them.

use serde::{Deserialize, Serialize};

use crate::config::{Curve, Point, Smoothing};

/// Speed a curve asks for at a temperature, interpolated between its points.
pub fn target_for(points: &[Point], temp_c: u8) -> Option<u16> {
    let first = points.first()?;
    let last = points.last()?;

    if temp_c <= first.temp_c {
        return Some(first.rpm);
    }
    if temp_c >= last.temp_c {
        return Some(last.rpm);
    }

    let upper = points.iter().position(|p| p.temp_c >= temp_c)?;
    let (a, b) = (points[upper - 1], points[upper]);
    let span = u32::from(b.temp_c - a.temp_c);
    if span == 0 {
        return Some(b.rpm);
    }

    let into = u32::from(temp_c - a.temp_c);
    let low = u32::from(a.rpm);
    let high = u32::from(b.rpm);
    let rpm = if high >= low {
        low + (high - low) * into / span
    } else {
        low - (low - high) * into / span
    };

    Some(rpm as u16)
}

/// Exponential smoothing with separate time constants for heating and cooling.
///
/// A CPU can jump 30 degrees and fall back inside ten seconds; feeding that
/// straight into a curve makes the fan chase every hiccup. Smoothing the input
/// rather than delaying the output keeps a genuine climb fully responsive while
/// a spike barely registers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Smoothed {
    value: f32,
}

impl Smoothed {
    pub fn new(initial: u8) -> Self {
        Self {
            value: f32::from(initial),
        }
    }

    /// Feed a reading taken `dt_secs` after the previous one.
    pub fn update(&mut self, reading: u8, dt_secs: f32, smoothing: &Smoothing) -> u8 {
        let reading = f32::from(reading);
        let rising = reading > self.value;
        let tau = if rising {
            smoothing.rise_secs
        } else {
            smoothing.fall_secs
        };

        // tau = 0 means "no smoothing at all", which is a legitimate choice.
        let alpha = if tau <= 0.0 {
            1.0
        } else {
            1.0 - (-dt_secs / tau).exp()
        };

        self.value += (reading - self.value) * alpha;
        self.value.round().clamp(0.0, 255.0) as u8
    }

    pub fn get(&self) -> u8 {
        self.value.round().clamp(0.0, 255.0) as u8
    }
}

/// What one curve wants right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Demand {
    pub name: String,
    /// Reading straight off the sensor.
    pub temp_c: u8,
    /// What the curve actually sees after smoothing.
    pub smoothed_c: u8,
    pub rpm: u16,
    /// True when the raw reading crossed the panic threshold and smoothing was
    /// bypassed.
    pub panic: bool,
}

/// Combine per-curve demands: the most demanding one wins.
///
/// Temperatures from different subsystems cannot be averaged - 60 °C is idle
/// for a CPU and hot for RAM - so each curve converts its own reading into a
/// speed first, and only speeds are compared.
pub fn winner(demands: &[Demand]) -> Option<&Demand> {
    demands.iter().max_by_key(|demand| demand.rpm)
}

/// Name a curve for logs and status when the config left it blank.
pub fn describe(curve: &Curve, index: usize) -> String {
    if !curve.name.is_empty() {
        return curve.name.clone();
    }
    if curve.sensor.kind == crate::config::Kind::Nvidia {
        return if curve.sensor.device.is_empty() {
            format!("nvidia#{index}")
        } else {
            format!("nvidia {}", curve.sensor.device)
        };
    }

    let chip = if curve.sensor.device.is_empty() {
        curve.sensor.hwmon.clone()
    } else {
        format!(
            "{} {}",
            curve.sensor.hwmon,
            crate::sensor::short_address(&curve.sensor.device)
        )
    };

    if curve.sensor.label.is_empty() {
        format!("{chip}#{index}")
    } else {
        format!("{chip}/{}", curve.sensor.label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points() -> Vec<Point> {
        vec![
            Point { temp_c: 40, rpm: 0 },
            Point {
                temp_c: 60,
                rpm: 1000,
            },
            Point {
                temp_c: 80,
                rpm: 3000,
            },
        ]
    }

    #[test]
    fn holds_the_ends_and_interpolates_between() {
        let points = points();
        assert_eq!(target_for(&points, 20), Some(0));
        assert_eq!(target_for(&points, 50), Some(500));
        assert_eq!(target_for(&points, 70), Some(2000));
        assert_eq!(target_for(&points, 95), Some(3000));
    }

    #[test]
    fn a_spike_barely_moves_the_smoothed_value() {
        let smoothing = Smoothing {
            rise_secs: 10.0,
            fall_secs: 60.0,
            panic_c: 85,
        };
        let mut smoothed = Smoothed::new(40);

        // 40 -> 70 for two ticks, then back down: what the curve sees stays far
        // below the peak.
        assert!(smoothed.update(70, 3.0, &smoothing) < 50);
        assert!(smoothed.update(70, 3.0, &smoothing) < 56);
        let after = smoothed.update(45, 3.0, &smoothing);
        assert!(
            after < 56,
            "cooling should not overshoot downwards: {after}"
        );
    }

    #[test]
    fn sustained_load_reaches_the_real_temperature() {
        let smoothing = Smoothing {
            rise_secs: 10.0,
            fall_secs: 60.0,
            panic_c: 85,
        };
        let mut smoothed = Smoothed::new(40);

        for _ in 0..15 {
            smoothed.update(70, 3.0, &smoothing);
        }
        assert!(
            smoothed.get() >= 69,
            "should converge on the real value: {}",
            smoothed.get()
        );
    }

    #[test]
    fn cooling_is_slower_than_heating() {
        let smoothing = Smoothing {
            rise_secs: 10.0,
            fall_secs: 60.0,
            panic_c: 85,
        };

        let mut up = Smoothed::new(40);
        up.update(70, 3.0, &smoothing);
        let climbed = up.get() - 40;

        let mut down = Smoothed::new(70);
        down.update(40, 3.0, &smoothing);
        let dropped = 70 - down.get();

        assert!(climbed > dropped, "{climbed} should exceed {dropped}");
    }

    #[test]
    fn the_hungriest_curve_wins() {
        let demands = vec![
            Demand {
                name: "cpu".into(),
                temp_c: 60,
                smoothed_c: 60,
                rpm: 700,
                panic: false,
            },
            Demand {
                name: "ram".into(),
                temp_c: 60,
                smoothed_c: 60,
                rpm: 2400,
                panic: false,
            },
        ];
        assert_eq!(winner(&demands).map(|d| d.name.as_str()), Some("ram"));
    }
}
