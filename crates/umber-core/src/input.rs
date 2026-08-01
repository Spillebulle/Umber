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
/// Touch screens and — on Windows — pens report real pressure through winit's
/// `Force`. A device with no sensor at all reports nothing, so `Device` has to
/// answer for the mouse too; see [`PressureModel::resolve`] for how it tells
/// the two silences apart. `Constant` and `Simulated` are the deliberate
/// mouse-only fallbacks.
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
    /// Whether this stroke has carried a real reading yet. See [`Self::resolve`];
    /// cleared by [`Self::reset`], so it answers for one stroke only.
    sensed: bool,
}

impl Default for PressureModel {
    fn default() -> Self {
        Self {
            source: PressureSource::Device,
            max_speed: 3000.0,
            responsiveness: 0.25,
            current: 1.0,
            sensed: false,
        }
    }
}

impl PressureModel {
    pub fn reset(&mut self) {
        self.current = match self.source {
            PressureSource::Simulated => 0.35,
            _ => 1.0,
        };
        // Per stroke, not per session — `resolve` explains why.
        self.sensed = false;
    }

    /// Resolve the pressure for a sample, given how far and how long since the
    /// previous one.
    pub fn resolve(&mut self, reported: Option<f32>, distance: f32, dt: f64) -> f32 {
        match self.source {
            PressureSource::Constant => 1.0,
            PressureSource::Device => match reported {
                Some(p) => {
                    self.sensed = true;
                    p.clamp(0.0, 1.0)
                }
                // An absent reading is ambiguous, and the two readings of it
                // are opposites. A mouse has no sensor and must paint at full
                // pressure; a pen that has just left the glass is reporting
                // *zero* and must paint nothing. winit does not distinguish
                // them: its Windows `WM_POINTER` path runs the raw pressure
                // through `normalize_pointer_pressure`, which accepts `1..=1024`
                // and answers `None` for everything else — so a genuine zero
                // arrives looking exactly like a device that has never heard of
                // pressure.
                //
                // `unwrap_or(1.0)` therefore stamped a full-size, full-coverage
                // dab at the end of every pen stroke: pressure falls smoothly
                // towards zero as the pen is lifted, and the last samples before
                // the pointer-up all cross into the range winit reports as
                // absent. That is the blob. `unwrap_or(0.0)` is not the fix
                // either — it would make the mouse paint nothing at all.
                //
                // The device settles the question itself. Once a stroke has
                // carried one real reading, the device demonstrably has a
                // sensor, so a gap in it afterwards is a zero; until then
                // nothing has ever been reported and full pressure is the only
                // safe answer, which is exactly the mouse — and exactly a
                // touchscreen whose driver supplies no force, which must still
                // draw.
                //
                // Latched per stroke rather than per session deliberately. A
                // session-wide latch would let one pen stroke make every later
                // mouse stroke on the same machine paint nothing, which is a far
                // worse failure than the blob. Per stroke it cannot: a stroke
                // only opens on contact, and contact is what a pen reports
                // pressure for, so the reading that sets the latch arrives
                // before any absence does. A pen that reports no pressure at all
                // never sets it and keeps the old behaviour throughout.
                None if self.sensed => 0.0,
                None => 1.0,
            },
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

    /// A mouse reports nothing, ever. It must draw at full pressure for the
    /// whole stroke — this is the case `unwrap_or(1.0)` existed for.
    #[test]
    fn a_stroke_that_never_reports_stays_at_full_pressure() {
        let mut m = PressureModel::default();
        m.reset();
        for _ in 0..20 {
            assert_eq!(m.resolve(None, 4.0, 0.016), 1.0);
        }
    }

    /// A pen lifting off. winit maps a raw zero to `None`, so the tail of the
    /// stroke arrives as absent readings — and used to come back as 1.0, which
    /// is the blob at the end of every stroke.
    #[test]
    fn a_pen_that_reported_then_stopped_falls_to_zero() {
        let mut m = PressureModel::default();
        m.reset();
        for p in [0.6, 0.4, 0.2, 0.05] {
            assert_eq!(m.resolve(Some(p), 4.0, 0.016), p);
        }
        // Below winit's `1..=1024` window the reading is dropped entirely.
        for _ in 0..4 {
            assert_eq!(
                m.resolve(None, 1.0, 0.016),
                0.0,
                "a lifting pen must not end at full pressure"
            );
        }
    }

    /// The latch belongs to one stroke. A pen stroke followed by a mouse stroke
    /// must leave the mouse drawing, which a session-wide latch would not.
    #[test]
    fn a_mouse_stroke_after_a_pen_stroke_draws() {
        let mut m = PressureModel::default();
        m.reset();
        m.resolve(Some(0.5), 4.0, 0.016);
        assert_eq!(m.resolve(None, 1.0, 0.016), 0.0);

        m.reset();
        assert_eq!(m.resolve(None, 4.0, 0.016), 1.0);
    }

    /// A zero the device *does* state is the same as one it omits.
    #[test]
    fn an_explicit_zero_reading_is_honoured() {
        let mut m = PressureModel::default();
        m.reset();
        assert_eq!(m.resolve(Some(0.0), 4.0, 0.016), 0.0);
    }

    /// Out-of-range readings are still clamped, and a clamped reading is still
    /// a reading — it must set the latch.
    #[test]
    fn an_out_of_range_reading_is_clamped_and_counts() {
        let mut m = PressureModel::default();
        m.reset();
        assert_eq!(m.resolve(Some(2.5), 4.0, 0.016), 1.0);
        assert_eq!(m.resolve(None, 1.0, 0.016), 0.0);
    }

    /// The mouse-only fallbacks are unaffected by any of it.
    #[test]
    fn the_fallback_sources_ignore_the_latch() {
        let mut c = PressureModel {
            source: PressureSource::Constant,
            ..Default::default()
        };
        c.reset();
        c.resolve(Some(0.3), 4.0, 0.016);
        assert_eq!(c.resolve(None, 1.0, 0.016), 1.0);

        let mut s = PressureModel {
            source: PressureSource::Simulated,
            ..Default::default()
        };
        s.reset();
        s.resolve(Some(0.3), 4.0, 0.016);
        assert!(s.resolve(None, 1.0, 0.016) > 0.0);
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
