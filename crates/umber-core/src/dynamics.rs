//! Inputs other than pressure, and the small table that routes them.
//!
//! MyPaint evaluates every brush setting as
//!
//! ```text
//! value(inputs) = base_value + Σ mapping_i(input_i)
//! ```
//!
//! Umber's [`Brush`](crate::Brush) has always read the `pressure` term of that
//! sum, and only onto four settings. This module is the rest of it: a
//! fixed-capacity list of `(target, input, curve)` triples that the stroke
//! builder evaluates once per dab.
//!
//! # Why a table rather than a curve per input per target
//!
//! `Brush` is `Copy` and [`ResponseCurve`] is a fixed array, both deliberately
//! (see `docs/brushes.md`). A curve for every input on every target would be
//! six inputs × ten targets = 60 curves, 1.2 kB, on a struct that is copied
//! into every preset and every stroke — to carry a median of **two** live
//! entries. A table of [`Modulations::MAX`] entries costs a tenth of that,
//! stays `Copy`, and serialises to nothing at all when it is empty.
//!
//! The cap is not free: a brush with more live mappings than fit loses the
//! narrowest ones. [`Modulations::push`] keeps the widest, which is the
//! ordering that discards least. Measured against the shipped MyPaint pack the
//! busiest brush uses 6 slots, so nothing in the library is truncated.
//!
//! # The units are MyPaint's, not Umber's
//!
//! A modulation's output is stated in the units of the MyPaint setting it came
//! from, because that is what makes the sum above reproducible: a size
//! modulation is an offset in **log** radius, an angle one is in degrees, and
//! an opacity one is the odd man out — a *factor*, because MyPaint reaches
//! opacity by multiplying two settings rather than adding to one.
//! [`Modulated`] documents each.

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::curve::ResponseCurve;

/// What drives a modulation.
///
/// A subset of MyPaint's inputs: the ones a desktop machine can actually
/// produce. `tilt_*`, `attack_angle` and `barrel_rotation` are omitted because
/// winit reports no tilt on desktop (see the pressure section of `CLAUDE.md`),
/// so they would sit at their neutral for every user — and `custom`,
/// `gridmap_*` and `viewzoom` because they drive features Umber has no
/// equivalent for. `brush_radius` and `viewzoom` are *constants* during a
/// stroke and are folded into the base value at import instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DabInput {
    /// Pen pressure, `0..1`.
    Pressure,
    /// MyPaint's `speed1`: a log of how fast the pointer is moving, smoothed
    /// over about 40 ms so it reacts within a flick.
    Speed,
    /// MyPaint's `speed2`: the same number smoothed over 800 ms, so it
    /// describes the pace of the whole gesture rather than the moment.
    SlowSpeed,
    /// How far into the stroke we are, `0..1`, measured in dab radii travelled
    /// rather than in seconds — see [`crate::Brush::stroke_span`].
    Stroke,
    /// Heading of the stroke in degrees, `0..180`. Undirected, like MyPaint's:
    /// a line pulled left and the same line pulled right read the same.
    Direction,
    /// A fresh uniform draw per dab, `0..1`.
    Random,
}

impl DabInput {
    pub const ALL: [Self; 6] = [
        Self::Pressure,
        Self::Speed,
        Self::SlowSpeed,
        Self::Stroke,
        Self::Direction,
        Self::Random,
    ];

    /// MyPaint's own name for the input, which is what a `.myb` file spells.
    pub fn myb_name(self) -> &'static str {
        match self {
            Self::Pressure => "pressure",
            Self::Speed => "speed1",
            Self::SlowSpeed => "speed2",
            Self::Stroke => "stroke",
            Self::Direction => "direction",
            Self::Random => "random",
        }
    }

    /// Label for the brush editor.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pressure => "Pressure",
            Self::Speed => "Speed",
            Self::SlowSpeed => "Speed (slow)",
            Self::Stroke => "Stroke position",
            Self::Direction => "Direction",
            Self::Random => "Random",
        }
    }

    /// The span of input values a mapping is written across, in MyPaint's
    /// units — its `soft_min` and `soft_max` from `brushsettings.json`.
    ///
    /// Values outside it are held rather than extrapolated. MyPaint
    /// extrapolates the end segment instead; within the range the two agree
    /// exactly, and outside it holding is the conservative direction. A speed
    /// mapping extrapolated off the end of its curve is how a single fast flick
    /// turns into a dab the size of the canvas.
    pub fn domain(self) -> (f32, f32) {
        match self {
            Self::Pressure | Self::Stroke | Self::Random => (0.0, 1.0),
            Self::Speed | Self::SlowSpeed => (0.0, 4.0),
            Self::Direction => (0.0, 180.0),
        }
    }

    /// Where the input sits when nothing is driving it.
    ///
    /// Used at import to decide what a mapping *contributes*: MyPaint adds
    /// `mapping(x)` to the base, so the part of it already accounted for in the
    /// base value is `mapping(neutral)`.
    ///
    /// Zero for everything with a natural floor — a stroke starts at its
    /// beginning, a pointer starts at rest — and 0.5 for `random`, whose
    /// average is the honest answer.
    pub fn neutral(self) -> f32 {
        match self {
            Self::Random => 0.5,
            _ => 0.0,
        }
    }

    /// Normalise a live value onto `0..=1` for [`ResponseCurve::sample`].
    pub fn normalise(self, value: f32) -> f32 {
        let (lo, hi) = self.domain();
        ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
    }
}

/// What a modulation drives.
///
/// Every one of these is applied **per dab**, so a brush may vary it along a
/// single stroke. The unit each carries is documented on [`Modulated`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DabTarget {
    /// MyPaint's `radius_logarithmic`.
    Size,
    /// MyPaint's `opaque` × `opaque_multiply`.
    Opacity,
    /// MyPaint's `hardness`.
    Hardness,
    /// MyPaint's `offset_by_random`.
    Scatter,
    /// MyPaint's `elliptical_dab_ratio`.
    Ratio,
    /// MyPaint's `elliptical_dab_angle`.
    Angle,
    /// MyPaint's `smudge`.
    Smudge,
    /// MyPaint's `change_color_h`.
    Hue,
    /// MyPaint's `change_color_hsv_s` and `change_color_hsl_s`.
    Saturation,
    /// MyPaint's `change_color_v` and `change_color_l`.
    Value,
}

impl DabTarget {
    pub const ALL: [Self; 10] = [
        Self::Size,
        Self::Opacity,
        Self::Hardness,
        Self::Scatter,
        Self::Ratio,
        Self::Angle,
        Self::Smudge,
        Self::Hue,
        Self::Saturation,
        Self::Value,
    ];

    /// Label for the brush editor.
    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "Size",
            Self::Opacity => "Opacity",
            Self::Hardness => "Hardness",
            Self::Scatter => "Scatter",
            Self::Ratio => "Roundness",
            Self::Angle => "Angle",
            Self::Smudge => "Colour pickup",
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Value => "Brightness",
        }
    }

    /// Whether driving this target forces the per-dab colour path on, which
    /// costs a second scratch target — see [`crate::Brush::smudge`].
    pub fn is_colour(self) -> bool {
        matches!(self, Self::Hue | Self::Saturation | Self::Value)
    }

    /// The smallest output span worth a slot, in this target's own unit.
    ///
    /// Two jobs, and it has to be a property of the target rather than of the
    /// importer for the second one. The first is a floor: below this the
    /// modulation is invisible, and MyPaint's editor writes near-flat mappings
    /// by the dozen. The second is a *scale*, so that [`Modulations::push`] can
    /// compare a 360-degree angle sweep against a 0.02-turn hue wobble and
    /// answer which matters more. Comparing the raw spans would let any angle
    /// entry evict every colour entry, every time.
    pub fn significant_span(self) -> f32 {
        match self {
            Self::Size | Self::Opacity | Self::Hardness | Self::Smudge => 0.02,
            Self::Scatter | Self::Saturation | Self::Value => 0.01,
            Self::Ratio => 0.05,
            Self::Angle => 1.0,
            // A turn, so 0.004 is about a degree and a half of hue.
            Self::Hue => 0.004,
        }
    }
}

/// One `(target, input)` mapping.
///
/// `low` and `high` bracket the output; `curve` carries only the shape between
/// them, exactly as [`crate::Brush::size_curve`] and its siblings do. That is
/// what lets a five-sample curve reproduce an arbitrary MyPaint mapping's range
/// exactly and its interior to within the sampling.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Modulation {
    pub target: DabTarget,
    pub input: DabInput,
    /// Output where `curve` reads 0.
    pub low: f32,
    /// Output where `curve` reads 1.
    pub high: f32,
    pub curve: ResponseCurve,
}

impl Modulation {
    /// A slot that carries nothing. Only for filling the unused tail of the
    /// array — [`Modulations::len`] is what says which entries are real.
    const EMPTY: Self = Self {
        target: DabTarget::Size,
        input: DabInput::Pressure,
        low: 0.0,
        high: 0.0,
        curve: ResponseCurve::LINEAR,
    };

    /// Output for an input already normalised onto `0..=1`.
    pub fn at(&self, t: f32) -> f32 {
        self.low + (self.high - self.low) * self.curve.sample(t)
    }

    /// Output for a live input value in MyPaint's units.
    pub fn at_raw(&self, value: f32) -> f32 {
        self.at(self.input.normalise(value))
    }

    /// How far the output travels. Zero means the entry does nothing, which is
    /// how a mapping that MyPaint's editor wrote but nobody ever moved is told
    /// apart from one that matters.
    pub fn span(&self) -> f32 {
        (self.high - self.low).abs()
    }

    /// The span in units of "how much this target has to move to be seen",
    /// which is the only form in which two different targets can be compared.
    pub fn weight(&self) -> f32 {
        self.span() / self.target.significant_span()
    }
}

/// A brush's modulation table.
///
/// Fixed capacity so [`crate::Brush`] stays `Copy`, and serialised as a plain
/// sequence of the live entries so an empty one costs `[]` in the library file
/// and an old library with no such field loads unchanged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Modulations {
    entries: [Modulation; Self::MAX],
    len: u8,
}

impl Default for Modulations {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Modulations {
    /// Slots per brush. Six is the most any brush in the shipped MyPaint pack
    /// needs; eight leaves room for a user to add a couple by hand.
    pub const MAX: usize = 8;

    pub const EMPTY: Self = Self {
        entries: [Modulation::EMPTY; Self::MAX],
        len: 0,
    };

    pub fn as_slice(&self) -> &[Modulation] {
        &self.entries[..self.len as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut Modulation> {
        self.entries.get_mut(..self.len as usize)?.get_mut(i)
    }

    /// Add a modulation, or replace the narrowest one when the table is full.
    ///
    /// Returns `false` when the entry was dropped, which is how the importer
    /// knows to report a loss rather than silently painting something else.
    /// A mapping too narrow to see is refused outright: MyPaint's editor writes
    /// a two-point mapping for every input a brush has ever been shown, and
    /// letting those in would fill the table with entries that do nothing.
    pub fn push(&mut self, m: Modulation) -> bool {
        if m.weight() < 1.0 {
            return false;
        }
        if (self.len as usize) < Self::MAX {
            self.entries[self.len as usize] = m;
            self.len += 1;
            return true;
        }
        // Full: keep the ones that move the stroke most. Discarding the
        // faintest changes the mark least, and it is at least a stated rule
        // rather than "whichever the file happened to list first".
        let (i, faintest) = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.weight()))
            .fold((0, f32::MAX), |acc, e| if e.1 < acc.1 { e } else { acc });
        if m.weight() > faintest {
            self.entries[i] = m;
            true
        } else {
            false
        }
    }

    pub fn remove(&mut self, i: usize) {
        if i >= self.len as usize {
            return;
        }
        for j in i..self.len as usize - 1 {
            self.entries[j] = self.entries[j + 1];
        }
        self.len -= 1;
    }

    /// Whether any entry reads this input. The stroke builder uses it to keep
    /// the random draw guarded — an unconditional draw would reshuffle the
    /// numbers every *other* random feature gets.
    pub fn uses(&self, input: DabInput) -> bool {
        self.as_slice().iter().any(|m| m.input == input)
    }

    pub fn drives(&self, target: DabTarget) -> bool {
        self.as_slice().iter().any(|m| m.target == target)
    }

    /// Whether the table changes the colour of individual dabs, which forces
    /// the stroke onto the per-dab colour path.
    pub fn tints(&self) -> bool {
        self.as_slice().iter().any(|m| m.target.is_colour())
    }

    /// Sum the table for one dab.
    pub fn evaluate(&self, inputs: &DabInputs) -> Modulated {
        let mut out = Modulated::NONE;
        for m in self.as_slice() {
            let v = m.at_raw(inputs.get(m.input));
            match m.target {
                DabTarget::Size => out.size_log += v,
                // Multiplied, not summed: MyPaint reaches opacity by
                // multiplying `opaque` by `opaque_multiply`, so a factor is the
                // form that composes.
                DabTarget::Opacity => out.opacity *= v,
                DabTarget::Hardness => out.hardness += v,
                DabTarget::Scatter => out.scatter += v,
                DabTarget::Ratio => out.ratio += v,
                DabTarget::Angle => out.angle += v,
                DabTarget::Smudge => out.smudge += v,
                DabTarget::Hue => out.hue += v,
                DabTarget::Saturation => out.saturation += v,
                DabTarget::Value => out.value += v,
            }
        }
        out
    }
}

impl FromIterator<Modulation> for Modulations {
    fn from_iter<T: IntoIterator<Item = Modulation>>(iter: T) -> Self {
        let mut out = Self::EMPTY;
        for m in iter {
            out.push(m);
        }
        out
    }
}

impl Serialize for Modulations {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.as_slice().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Modulations {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Modulations;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a list of brush modulations")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Modulations, A::Error> {
                let mut out = Modulations::EMPTY;
                while let Some(m) = seq.next_element::<Modulation>()? {
                    // Silently dropped past the cap rather than refused: a
                    // library that will not load is worse than one brush that
                    // paints slightly plainer than it was saved.
                    out.push(m);
                }
                Ok(out)
            }
        }
        d.deserialize_seq(V)
    }
}

/// Every input, in MyPaint's units, for one dab.
#[derive(Clone, Copy, Debug, Default)]
pub struct DabInputs {
    pub pressure: f32,
    pub speed: f32,
    pub slow_speed: f32,
    pub stroke: f32,
    pub direction: f32,
    pub random: f32,
}

impl DabInputs {
    pub fn get(&self, input: DabInput) -> f32 {
        match input {
            DabInput::Pressure => self.pressure,
            DabInput::Speed => self.speed,
            DabInput::SlowSpeed => self.slow_speed,
            DabInput::Stroke => self.stroke,
            DabInput::Direction => self.direction,
            DabInput::Random => self.random,
        }
    }
}

/// The table's summed contribution to one dab.
///
/// The identity is *not* all zeroes — `opacity` composes multiplicatively — so
/// [`Modulated::NONE`] rather than `Default` is what an unmodulated dab uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Modulated {
    /// Offset in **log** radius: `radius *= size_log.exp()`. MyPaint's
    /// `radius_logarithmic` is a log, so a mapping on it composes by addition
    /// in that space and by multiplication in pixels.
    pub size_log: f32,
    /// Factor on per-dab coverage, `0..=1`.
    pub opacity: f32,
    /// Added to hardness, in hardness' own `0..1`.
    pub hardness: f32,
    /// Added to scatter, in dab radii.
    pub scatter: f32,
    /// Added to the dab's long:short ratio.
    pub ratio: f32,
    /// Added to the dab's angle, in degrees.
    pub angle: f32,
    /// Added to the colour-pickup fraction.
    pub smudge: f32,
    /// Added to the dab colour's hue, in turns (MyPaint's unit — `0.5` is the
    /// opposite side of the wheel).
    pub hue: f32,
    /// Scales the dab colour's saturation, as MyPaint does: the shift is
    /// proportional to how saturated the colour already is, so a grey stays
    /// grey.
    pub saturation: f32,
    /// Added to the dab colour's value.
    pub value: f32,
}

impl Modulated {
    pub const NONE: Self = Self {
        size_log: 0.0,
        opacity: 1.0,
        hardness: 0.0,
        scatter: 0.0,
        ratio: 0.0,
        angle: 0.0,
        smudge: 0.0,
        hue: 0.0,
        saturation: 0.0,
        value: 0.0,
    };

    /// Whether the colour needs touching at all — the fast path check.
    pub fn tints(&self) -> bool {
        self.hue != 0.0 || self.saturation != 0.0 || self.value != 0.0
    }
}

/// MyPaint's mapping from physical speed to its `speed1` / `speed2` inputs.
///
/// `y = log(gamma + x) * m + q`, with `m` and `q` fixed by two constraints
/// libmypaint hard-codes: 45 px/s reads 0.5, and the slope there is 0.015 per
/// px/s. `gamma` is `exp(speed_gamma)`, and every brush in the pack leaves
/// `speed1_gamma` and `speed2_gamma` at MyPaint's default of 4.
///
/// Without this a `speed1` mapping is meaningless — its control points are
/// written on this scale, not in pixels per second.
pub fn speed_input(pixels_per_second: f32, gamma_log: f32) -> f32 {
    let gamma = gamma_log.exp();
    let m = 0.015 * (45.0 + gamma);
    let q = 0.5 - m * (45.0 + gamma).ln();
    (gamma + pixels_per_second.max(0.0)).ln() * m + q
}

/// The time constants libmypaint smooths speed with, in seconds: `speed1` is
/// the twitchy one and `speed2` describes the pace of a gesture. Every brush in
/// the pack leaves both at these defaults.
pub const SPEED_SLOWNESS: f32 = 0.04;
pub const SLOW_SPEED_SLOWNESS: f32 = 0.8;

/// `speed1_gamma`'s default, and every brush in the pack's value for it.
pub const SPEED_GAMMA_LOG: f32 = 4.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn m(target: DabTarget, input: DabInput, low: f32, high: f32) -> Modulation {
        Modulation {
            target,
            input,
            low,
            high,
            curve: ResponseCurve::LINEAR,
        }
    }

    #[test]
    fn an_empty_table_is_the_identity() {
        let out = Modulations::EMPTY.evaluate(&DabInputs::default());
        assert_eq!(out, Modulated::NONE);
        assert!(!out.tints());
    }

    #[test]
    fn a_flat_mapping_is_refused_rather_than_stored() {
        // MyPaint's editor writes a two-point mapping for every input a brush
        // has ever been shown. Letting those in would fill eight slots with
        // entries that do nothing and evict the ones that matter.
        let mut t = Modulations::EMPTY;
        assert!(!t.push(m(DabTarget::Size, DabInput::Speed, 0.3, 0.3)));
        assert!(t.is_empty());
    }

    #[test]
    fn a_full_table_keeps_the_widest_entries() {
        let mut t = Modulations::EMPTY;
        for i in 0..Modulations::MAX {
            assert!(t.push(m(
                DabTarget::Hardness,
                DabInput::Speed,
                0.0,
                (i + 1) as f32
            )));
        }
        assert_eq!(t.len(), Modulations::MAX);
        // Narrower than the narrowest present: refused.
        assert!(!t.push(m(DabTarget::Size, DabInput::Stroke, 0.0, 0.5)));
        // Wider: evicts the span-1 entry.
        assert!(t.push(m(DabTarget::Size, DabInput::Stroke, 0.0, 99.0)));
        assert!(t.as_slice().iter().all(|e| e.span() > 1.0));
    }

    #[test]
    fn opacity_composes_by_multiplication_and_the_rest_by_addition() {
        // Every input reads zero here, so each entry contributes its `low`.
        let t: Modulations = [
            m(DabTarget::Opacity, DabInput::Speed, 0.5, 1.0),
            m(DabTarget::Opacity, DabInput::Stroke, 0.5, 1.0),
            m(DabTarget::Hardness, DabInput::Speed, 0.2, 0.9),
            m(DabTarget::Hardness, DabInput::Stroke, 0.2, 0.9),
        ]
        .into_iter()
        .collect();
        let out = t.evaluate(&DabInputs::default());
        assert!((out.opacity - 0.25).abs() < 1e-3, "{}", out.opacity);
        assert!((out.hardness - 0.4).abs() < 1e-3, "{}", out.hardness);
    }

    #[test]
    fn an_input_outside_its_domain_holds_rather_than_extrapolating() {
        let e = m(DabTarget::Size, DabInput::Speed, 0.0, 2.0);
        assert!((e.at_raw(4.0) - 2.0).abs() < 1e-5);
        assert!((e.at_raw(40.0) - 2.0).abs() < 1e-5, "a flick must not blow up");
        assert!(e.at_raw(-5.0).abs() < 1e-5);
    }

    #[test]
    fn the_speed_curve_matches_mypaints_two_fixed_points() {
        // 45 px/s reads 0.5 by construction, and the slope there is 0.015.
        assert!((speed_input(45.0, SPEED_GAMMA_LOG) - 0.5).abs() < 1e-4);
        let slope = (speed_input(45.5, SPEED_GAMMA_LOG) - speed_input(44.5, SPEED_GAMMA_LOG)) / 1.0;
        assert!((slope - 0.015).abs() < 1e-4, "slope {slope}");
        // At rest the input sits just below its stated floor, which is why the
        // domain clamps rather than the formula.
        assert!(speed_input(0.0, SPEED_GAMMA_LOG) < 0.0);
    }

    #[test]
    fn an_empty_table_round_trips_as_an_empty_list() {
        let text = ron::to_string(&Modulations::EMPTY).expect("serialise");
        assert_eq!(text, "[]");
        let back: Modulations = ron::from_str(&text).expect("parse");
        assert!(back.is_empty());
    }

    #[test]
    fn a_table_round_trips_through_ron() {
        let t: Modulations = [
            m(DabTarget::Ratio, DabInput::Random, 1.0, 4.0),
            m(DabTarget::Size, DabInput::Speed, -0.5, 0.5),
        ]
        .into_iter()
        .collect();
        let back: Modulations = ron::from_str(&ron::to_string(&t).expect("serialise")).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn removing_an_entry_closes_the_gap() {
        let mut t: Modulations = [
            m(DabTarget::Size, DabInput::Speed, 0.0, 1.0),
            m(DabTarget::Ratio, DabInput::Random, 0.0, 2.0),
            m(DabTarget::Angle, DabInput::Stroke, 0.0, 3.0),
        ]
        .into_iter()
        .collect();
        t.remove(0);
        assert_eq!(t.len(), 2);
        assert_eq!(t.as_slice()[0].target, DabTarget::Ratio);
        assert_eq!(t.as_slice()[1].target, DabTarget::Angle);
    }
}
