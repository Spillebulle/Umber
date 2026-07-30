//! Brush parameters.

/// What a stroke does to the layer underneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushMode {
    /// Composite the stroke colour over the layer.
    Paint,
    /// Subtract the stroke's coverage from the layer's alpha.
    Erase,
}

/// A round brush.
///
/// Sizes are in **document** pixels, so brush appearance is independent of
/// zoom — painting at 12% zoom lays down exactly the pixels you would get at
/// 100%.
#[derive(Clone, Copy, Debug)]
pub struct Brush {
    /// Diameter at full pressure.
    pub size: f32,
    /// Diameter at zero pressure, as a fraction of `size`.
    pub min_size_ratio: f32,
    /// `0.0` is a fully soft falloff, `1.0` a hard (still antialiased) edge.
    pub hardness: f32,
    /// Opacity of the finished stroke, applied once on commit rather than per
    /// dab — see [`crate::stroke`] for why that distinction matters.
    pub opacity: f32,
    /// Distance between dabs as a fraction of the current diameter. Smaller is
    /// smoother and more expensive; 0.1 is a good default.
    pub spacing: f32,
    pub pressure_size: bool,
    pub pressure_opacity: bool,
    /// Input smoothing, `0.0` (raw) to just under `1.0` (very heavy).
    pub stabilization: f32,
    pub mode: BrushMode,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            size: 24.0,
            min_size_ratio: 0.08,
            hardness: 0.55,
            opacity: 1.0,
            spacing: 0.1,
            pressure_size: true,
            pressure_opacity: false,
            stabilization: 0.35,
            mode: BrushMode::Paint,
        }
    }
}

impl Brush {
    pub const MIN_SIZE: f32 = 1.0;
    pub const MAX_SIZE: f32 = 2000.0;

    /// Dab radius for a given pressure, in document pixels.
    pub fn radius_at(&self, pressure: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let scale = if self.pressure_size {
            self.min_size_ratio + (1.0 - self.min_size_ratio) * p
        } else {
            1.0
        };
        (self.size * scale * 0.5).max(0.5)
    }

    /// Per-dab coverage for a given pressure.
    ///
    /// Note this deliberately excludes [`Brush::opacity`]: dabs accumulate with
    /// a `max` blend, so stroke opacity has to be applied once afterwards.
    pub fn coverage_at(&self, pressure: f32) -> f32 {
        if self.pressure_opacity {
            pressure.clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Distance to the next dab, in document pixels.
    pub fn step_at(&self, pressure: f32) -> f32 {
        (self.radius_at(pressure) * 2.0 * self.spacing).max(0.25)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_scales_radius_between_min_and_full() {
        let b = Brush {
            size: 100.0,
            min_size_ratio: 0.1,
            pressure_size: true,
            ..Default::default()
        };
        assert!((b.radius_at(1.0) - 50.0).abs() < 1e-4);
        assert!((b.radius_at(0.0) - 5.0).abs() < 1e-4);
    }

    #[test]
    fn step_never_reaches_zero() {
        // A zero step would spin the dab loop forever.
        let b = Brush {
            size: 1.0,
            spacing: 0.0,
            ..Default::default()
        };
        assert!(b.step_at(0.0) > 0.0);
    }
}
