//! Pointer input, normalised across mice, pens and touch.

use glam::Vec2;

/// A single sample from a pointing device, already converted to document space.
#[derive(Clone, Copy, Debug)]
pub struct InputPoint {
    pub pos: Vec2,
    /// `0.0..=1.0`. Devices without pressure report `1.0`.
    pub pressure: f32,
    /// Stylus tilt as a unit-ish vector, `(0, 0)` when unknown. Not consumed by
    /// the brush engine yet; carried so the input path doesn't need reworking
    /// when tilt-driven brushes land.
    pub tilt: Vec2,
    /// Seconds since app start. Used for velocity-derived effects.
    pub time: f64,
}

impl InputPoint {
    pub fn new(pos: Vec2, pressure: f32, time: f64) -> Self {
        Self {
            pos,
            pressure: pressure.clamp(0.0, 1.0),
            tilt: Vec2::ZERO,
            time,
        }
    }
}

/// Where pressure values come from.
///
/// Touch screens (Android, iPad) report real pressure through winit's
/// `Force`. Desktop pens do **not** currently surface pressure through winit's
/// mouse events, so on desktop the choice is a flat 1.0 or a speed-derived
/// approximation. Native tablet APIs can be slotted in behind this enum later
/// without touching the brush engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressureSource {
    /// Trust whatever the device reported.
    Device,
    /// Always full pressure.
    Constant,
    /// Derive pressure from stroke speed: fast strokes go thin, slow strokes
    /// go thick. A usable stand-in for a real pen on a mouse-only machine.
    Simulated,
}

/// Converts raw pointer motion into a pressure value.
#[derive(Clone, Copy, Debug)]
pub struct PressureModel {
    pub source: PressureSource,
    /// Speed (document px/s) that maps to minimum pressure.
    pub max_speed: f32,
    /// How quickly simulated pressure reacts, `0.0..=1.0`.
    pub responsiveness: f32,
    current: f32,
}

impl Default for PressureModel {
    fn default() -> Self {
        Self {
            source: PressureSource::Device,
            max_speed: 3000.0,
            responsiveness: 0.25,
            current: 1.0,
        }
    }
}

impl PressureModel {
    pub fn reset(&mut self) {
        self.current = match self.source {
            PressureSource::Simulated => 0.35,
            _ => 1.0,
        };
    }

    /// Resolve the pressure for a sample, given how far and how long since the
    /// previous one.
    pub fn resolve(&mut self, reported: Option<f32>, distance: f32, dt: f64) -> f32 {
        match self.source {
            PressureSource::Constant => 1.0,
            PressureSource::Device => reported.unwrap_or(1.0).clamp(0.0, 1.0),
            PressureSource::Simulated => {
                let speed = if dt > 1e-5 { distance / dt as f32 } else { 0.0 };
                let target = (1.0 - (speed / self.max_speed).clamp(0.0, 1.0)).clamp(0.05, 1.0);
                self.current += (target - self.current) * self.responsiveness.clamp(0.01, 1.0);
                self.current
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_source_ignores_device() {
        let mut m = PressureModel {
            source: PressureSource::Constant,
            ..Default::default()
        };
        assert_eq!(m.resolve(Some(0.1), 0.0, 0.01), 1.0);
    }

    #[test]
    fn simulated_pressure_drops_as_speed_rises() {
        let mut slow = PressureModel {
            source: PressureSource::Simulated,
            ..Default::default()
        };
        slow.reset();
        let mut fast = slow;
        for _ in 0..50 {
            slow.resolve(None, 1.0, 0.016);
            fast.resolve(None, 200.0, 0.016);
        }
        assert!(
            slow.current > fast.current,
            "slow {} should exceed fast {}",
            slow.current,
            fast.current
        );
    }

    #[test]
    fn zero_dt_does_not_divide_by_zero() {
        let mut m = PressureModel {
            source: PressureSource::Simulated,
            ..Default::default()
        };
        let p = m.resolve(None, 10.0, 0.0);
        assert!(p.is_finite());
    }
}
