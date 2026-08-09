//! Temporary brush changes: the settings a painter reaches for while a picture
//! is under way, and what "a bit more" is worth however it is asked for.
//!
//! ## Where a temporary change lives, and what puts it back
//!
//! There is no second mechanism here, and looking for one was the first thing
//! this module had to not do. [`Editor::brush`] **is** the temporary copy.
//! `BrushPreset::brush`, in `Editor::presets` and in the user's `brushes.ron`,
//! is the saved one; the only thing that writes it is the Save/Update row at
//! the foot of the brush editor. Size and Opacity on the tool options strip
//! have always worked exactly this way, and so does every slider in the brush
//! editor itself — so a panel of six more rails needed no new state, no
//! override layer and no "is this modified?" flag. It writes the same fields.
//!
//! Three consequences, and each is a question somebody will ask:
//!
//! * **Picking a brush puts its own settings back.** `Editor::apply_preset`
//!   assigns `self.brush = preset.brush` and then restores only the paint or
//!   erase mode, so selecting *any* brush — including the one already in hand,
//!   which `brushlib`'s list re-applies on every click — discards every tweak.
//!   That is the reset, it is the only one, and it is the same one Size and
//!   Opacity have.
//! * **A tab switch keeps them.** `Editor::brush` sits above the
//!   `--- documents ---` line, so the brush in hand is the window's rather than
//!   the document's. The obvious reading of "just temporary in the project" is
//!   per-document, and it is deliberately *not* what this does: making six
//!   settings per-document while the two beside them on the options strip are
//!   not would be a split nobody could predict from the interface. If the brush
//!   is ever made per-document these six travel with it, because they are the
//!   same field.
//! * **The brush editor shows the tweaked value**, because it reads the same
//!   `Editor::brush`. It has always done so; a tweak is not hidden from it, and
//!   the Update button there is what makes one permanent.
//!
//! ## One table, two ways of asking
//!
//! A rail and an increase/decrease shortcut are two spellings of one question,
//! so they go through one [`Tweak`]. What makes it worth a table rather than
//! two call sites is the *step*: one press of a shortcut is worth [`STEP_PX`]
//! pixels of the drag. `Brush::RESIZE_DOUBLE_PX` is 100, so a size press is
//! `2^(20/100)` — 1.1487, the 1.15 the size shortcut has always used, now
//! falling out of the rule instead of restating it.
//! `a_size_press_is_still_the_115_it_always_was` pins that.
//!
//! ## There was a third, and what it cost
//!
//! Every row used to carry a hold-and-drag grip on its right: three dots in a
//! column, offering the same value over a longer travel than a 264-point panel
//! can give a rail. It was reported as a row of buttons that do nothing, and
//! that is exactly what it was. Three dots at the end of a row is a menu
//! everywhere else in this interface and everywhere else on the desktop, so it
//! invited a *click*, and it answered only to a hold — a control whose whole
//! affordance pointed at the one gesture it ignored.
//!
//! Nothing went with it. [`widgets::typed_row`] is a drag in its own right and
//! covers the same range; what the grip bought was travel, not reach, and the
//! rail is wider now that it has the row to itself — and its figure can be
//! typed, which is the exactness a longer travel was standing in for. The
//! lesson worth keeping is that a second control for a value the panel already
//! has must not look like a control for something else.

use crate::editor::Editor;
use crate::shortcuts::Action;
use crate::theme::Palette;
use crate::widgets;
use egui::{Ui, pos2};
use std::ops::RangeInclusive;
use umber_core::Brush;

/// How far a pointer would travel, in physical window pixels, for a *linear*
/// setting to cross its whole range.
///
/// The unit [`STEP_PX`] is stated in, and therefore what a shortcut press is
/// worth. It outlived the hold-grip it was written for — see the module docs —
/// because the shortcut still has to answer "how much is a bit more", and this
/// is the answer that keeps the size press at the 1.15 it has always been.
///
/// A logarithmic setting uses `Brush::RESIZE_DOUBLE_PX` instead, which is the
/// rate the canvas's Alt-drag has always used, so a size changed from here and
/// a size changed on the canvas move at exactly the same rate.
pub const DRAG_FULL_PX: f32 = 400.0;

/// What one press of an increase or decrease shortcut is worth, in pixels of
/// the drag above.
///
/// One number rather than a step per setting, so "a bit more" means the same
/// visible amount everywhere, and so the size shortcut's long-standing 1.15
/// falls out rather than being written down twice.
pub const STEP_PX: f32 = 20.0;

/// The narrowest a dab may be squashed, as the reciprocal that the Roundness
/// rail shows. A 20:1 chisel is already thinner than any real bristle.
///
/// The same floor `ui::brush_editor_tip`'s Roundness row uses, and the same
/// reciprocal: `Brush::dab_ratio` is long-over-short, so the two are inverses.
const MIN_ROUNDNESS: f32 = 0.05;

/// Where the **size rail** stops, which is not where a brush size stops.
///
/// `Brush::MAX_SIZE` is 2000 and this is 1000: the rail is a hand's instrument
/// and the value's bound is the engine's, which is the distinction
/// [`Tweak::range`] has always drawn and [`Tweak::span`] now names on the other
/// side. Typing 1500 into either size rail means 1500.
///
/// It was 400, and widening it costs granularity. A *logarithmic* rail loses
/// it uniformly: the whole travel is `ln(hi/lo)`, so one point of track is
/// worth a factor of `exp(ln(hi/lo) / track)` whatever the size, and
/// `ln 1000 / ln 400` is 1.153. One point of the strip's 80-point track is
/// therefore worth 9.02% of the size rather than 7.78%, and one point of the
/// brush editor's ~259-point track 2.70% rather than 2.34% — **13.3% fewer
/// distinguishable sizes**, at every size on both rails. On the strip that is
/// 1.80 px per point at a 20 px brush where it was 1.55, and 9.0 px per point
/// at 100 px where it was 7.8.
///
/// It is a deliberate trade and it is the *other* half of the figure being
/// typable. Widening a rail was measured and refused once before, when the
/// only way to state a figure was to land the knob on it; the exactness this
/// gives up is exactness the keyboard now hands back. What it buys is that
/// the sizes between 400 and 1000 px are on the rail at all rather than
/// reachable only by an Alt-drag on the canvas — 15 of the 252 shipped
/// presets carry a size past 400, and 14 of those 15 are inside 1000. The
/// fifteenth is the 1045 px brush the tap refusal in `widgets::track_value`
/// was written for, and it is still off the end, which is the case that has
/// to keep working rather than the case to widen the rail for.
///
/// One constant rather than two literals, because two rails for one setting
/// that stop in different places is a control that disagrees with itself.
pub const SIZE_RAIL_TOP: f32 = 1000.0;

/// A rail that reached the whole range would make [`Tweak::span`] and
/// [`Tweak::range`] the same answer and retire the distinction a typed figure
/// rests on — silently, since every test of "a size may be more than the rail
/// says" would go on passing by being vacuous.
///
/// A `const` assert rather than a test because the failure is **directional**:
/// only raising this needs saying, and nothing that runs would say it.
const _: () = assert!(SIZE_RAIL_TOP < Brush::MAX_SIZE);

/// A brush setting a painter changes without meaning to change the brush.
///
/// Every one of these writes a field of [`Editor::brush`] and nothing else —
/// see the module docs for what that means and what puts it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tweak {
    Size,
    Opacity,
    Hardness,
    Spacing,
    Roundness,
    AirbrushRate,
    Angle,
    ColourPickup,
}

impl Tweak {
    /// Every tweak a shortcut can reach.
    pub const ALL: [Tweak; 8] = [
        Self::Size,
        Self::Opacity,
        Self::Hardness,
        Self::Spacing,
        Self::Roundness,
        Self::AirbrushRate,
        Self::Angle,
        Self::ColourPickup,
    ];

    /// The six the module draws: the brush editor's Tip section in its own
    /// order, less Size and Opacity, with Colour pickup — which lives under
    /// Blending there — added at the end.
    ///
    /// Size and Opacity are deliberately not among them: both are on the tool
    /// options strip, which is above the canvas whatever the dock is doing, and
    /// a second rail for a number that already has one is a second place to
    /// look rather than a shorter reach. They are still in [`Tweak::ALL`],
    /// because a *shortcut* for them is a reach the strip cannot shorten.
    pub const PANEL: [Tweak; 6] = [
        Self::Hardness,
        Self::Spacing,
        Self::Roundness,
        Self::AirbrushRate,
        Self::Angle,
        Self::ColourPickup,
    ];

    /// The name on the rail and in the shortcut list.
    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "Size",
            Self::Opacity => "Opacity",
            Self::Hardness => "Hardness",
            Self::Spacing => "Spacing",
            Self::Roundness => "Roundness",
            Self::AirbrushRate => "Airbrush rate",
            Self::Angle => "Angle",
            Self::ColourPickup => "Colour pickup",
        }
    }

    /// What the setting may be set to — by a shortcut, by a drag on the canvas,
    /// or by typing the figure.
    ///
    /// Size's is the *whole* range rather than the [`SIZE_RAIL_TOP`] its two
    /// rails stop at: the size shortcut has always clamped at `Brush::MAX_SIZE`
    /// and a rail's span is not a bound on the value. [`Tweak::span`] is the
    /// other side of that sentence.
    pub fn range(self) -> RangeInclusive<f32> {
        match self {
            Self::Size => Brush::MIN_SIZE..=Brush::MAX_SIZE,
            Self::Opacity | Self::Hardness | Self::ColourPickup => 0.0..=1.0,
            Self::Spacing => 0.01..=0.5,
            Self::Roundness => MIN_ROUNDNESS..=1.0,
            Self::AirbrushRate => 0.0..=100.0,
            // Clamped rather than wrapped, and that is a decision. A rotation
            // that wrapped would be the friendlier gesture and would then
            // disagree with the rail beside it at both ends — two behaviours
            // for one number, which is the thing this interface refuses. The
            // rail is the authority; the grip and the shortcut follow it.
            Self::Angle => 0.0..=359.0,
        }
    }

    /// What the **rail** covers, which is not what the value may be.
    ///
    /// The same for every setting but Size, whose rail stops at
    /// [`SIZE_RAIL_TOP`] while `Brush::MAX_SIZE` is twice that. A rail cannot
    /// express a value past its own end — `widgets::track_value` pins the knob
    /// there and refuses a stationary tap — so the two figures have to be two
    /// figures, and the second is what a typed one is held to.
    pub fn span(self) -> RangeInclusive<f32> {
        match self {
            Self::Size => Brush::MIN_SIZE..=SIZE_RAIL_TOP,
            _ => self.range(),
        }
    }

    /// Whether the rail — and the drag — is logarithmic.
    ///
    /// The same two the brush editor draws logarithmically, for the same
    /// reason: the difference between a 3-pixel liner and a 6-pixel one is the
    /// whole character of the brush, and six pixels on a 300-pixel wash is
    /// nothing.
    pub fn log(self) -> bool {
        matches!(self, Self::Size | Self::Spacing)
    }

    /// How the setting reads out, and how a line typed into its field reads
    /// back.
    ///
    /// One statement of the pair rather than a formatter here and a parser
    /// somewhere else — see `widgets::Figure`. The airbrush is the one setting
    /// in Umber whose zero is a word rather than a number, and "off" is
    /// therefore a line the field accepts as well as one it shows.
    pub fn figure(self) -> widgets::Figure<'static> {
        match self {
            Self::Size => widgets::Figure::new(1.0, " px", 0),
            Self::Angle => widgets::Figure::new(1.0, "°", 0),
            Self::AirbrushRate => widgets::Figure {
                zero: "off",
                ..widgets::Figure::new(1.0, "/s", 0)
            },
            _ => widgets::Figure::new(100.0, "%", 0),
        }
    }

    /// The readout, in the setting's own units.
    pub fn format(self, value: f32) -> String {
        self.figure().format(value)
    }

    /// What the brush in hand currently says.
    pub fn value(self, brush: &Brush) -> f32 {
        match self {
            Self::Size => brush.size,
            Self::Opacity => brush.opacity,
            Self::Hardness => brush.hardness,
            Self::Spacing => brush.spacing,
            // The reciprocal of the engine's long-over-short ratio, which is
            // the word the design and every other paint application uses.
            //
            // **Not clamped to the rail**, and that is the difference between
            // a reading and an edit. `brushimport::kpp` bounds nothing, so a
            // Krita brush with a 40:1 tip is a legitimate `dab_ratio` of 40;
            // reporting it as the rail's 5% floor would make one press of a
            // shortcut — which reads, steps and writes — halve the dab's
            // aspect with nothing on screen to say why. Off the end of the
            // rail is where it honestly is, which is also exactly what
            // `ui::brush_editor_tip`'s own Roundness row shows. Only
            // [`Tweak::apply`] clamps, because only an edit should.
            Self::Roundness => 1.0 / brush.dab_ratio.max(1.0),
            Self::AirbrushRate => brush.dabs_per_second,
            Self::Angle => brush.dab_angle,
            Self::ColourPickup => brush.smudge,
        }
    }

    /// Write the setting, clamped to [`Tweak::range`].
    ///
    /// Clamping here rather than at the call sites is what lets the rail, the
    /// grip and the shortcut share one guarantee: no route into a brush can
    /// leave a value the engine has to defend itself against.
    pub fn apply(self, brush: &mut Brush, value: f32) {
        let range = self.range();
        let value = if value.is_finite() {
            value.clamp(*range.start(), *range.end())
        } else {
            *range.start()
        };
        match self {
            Self::Size => brush.size = value,
            Self::Opacity => brush.opacity = value,
            Self::Hardness => brush.hardness = value,
            Self::Spacing => brush.spacing = value,
            Self::Roundness => brush.dab_ratio = 1.0 / value,
            Self::AirbrushRate => brush.dabs_per_second = value,
            Self::Angle => brush.dab_angle = value,
            Self::ColourPickup => brush.smudge = value,
        }
    }

    /// The value a drag of `along` physical pixels "towards more" asks for,
    /// having started from `from`.
    ///
    /// Absolute against the value at the press, never stepped per event — the
    /// reason `Brush::size_after_drag` gives, and the property that makes
    /// dragging back to where you started give the setting back exactly.
    /// Resolve the two axes onto `along` with `umber_core::geom::
    /// drag_towards_more`, which is the same function the brush-size drag and
    /// the zoom drag both use.
    pub fn after_drag(self, from: f32, along: f32) -> f32 {
        let range = self.range();
        let (lo, hi) = (*range.start(), *range.end());
        let wanted = if self.log() {
            from * (along / Brush::RESIZE_DOUBLE_PX).exp2()
        } else {
            from + along / DRAG_FULL_PX * (hi - lo)
        };
        if wanted.is_finite() {
            wanted.clamp(lo, hi)
        } else {
            lo
        }
    }

    /// Move the setting by `steps` presses of its shortcut.
    pub fn nudge(self, brush: &mut Brush, steps: f32) {
        let from = self.value(brush);
        self.apply(brush, self.after_drag(from, steps * STEP_PX));
    }

    /// The pair of actions that decrease and increase it.
    pub fn actions(self) -> (Action, Action) {
        match self {
            Self::Size => (Action::SizeDown, Action::SizeUp),
            Self::Opacity => (Action::OpacityDown, Action::OpacityUp),
            Self::Hardness => (Action::HardnessDown, Action::HardnessUp),
            Self::Spacing => (Action::SpacingDown, Action::SpacingUp),
            Self::Roundness => (Action::RoundnessDown, Action::RoundnessUp),
            Self::AirbrushRate => (Action::AirbrushDown, Action::AirbrushUp),
            Self::Angle => (Action::AngleDown, Action::AngleUp),
            Self::ColourPickup => (Action::PickupDown, Action::PickupUp),
        }
    }

    /// Whether the setting means anything for the brush in hand.
    ///
    /// The same two readings the brush editor takes, so a rail cannot be live
    /// here and dead there. A control that does nothing is worse than one
    /// visibly switched off, and one that vanishes reads as a bug — hence a
    /// disabled rail with [`Tweak::why_off`] under it.
    pub fn enabled(self, ed: &Editor) -> bool {
        match self {
            // A stamp *replaces* the procedural falloff rather than being
            // multiplied into it, so hardness has nothing left to shape.
            Self::Hardness => ed.tip.is_none(),
            // A circle has no angle — but a bitmap does whatever its
            // roundness, because it is not rotationally symmetric. `Brush`
            // can only answer the first half; the tip is a name the editor
            // resolves. `ui::has_angle` is the one place the two are combined,
            // shared rather than copied so this rail and the brush editor's
            // cannot disagree.
            Self::Angle => crate::ui::has_angle(ed),
            // An eraser deposits no colour, so there is nothing for it to mix
            // with what is under it. `Brush::blend_applies` is the engine's own
            // statement of exactly that, already read by the Blending section
            // to not draw a blend mode for one — the same sentence, so the two
            // cannot answer differently.
            //
            // Live, this is not merely a control that does nothing: `smudge`
            // drives `Brush::colours_dabs`, which puts the whole stroke on the
            // two-attachment coloured dab pipeline, and `StrokeBuilder::probe`
            // is gated on `smudges()` with no mode test — so an erasing stroke
            // would record a canvas probe every frame for a colour that is
            // never deposited.
            Self::ColourPickup => ed.brush.blend_applies(),
            _ => true,
        }
    }

    /// Why the rail is switched off, for the caption under it.
    pub fn why_off(self) -> &'static str {
        match self {
            Self::Hardness => "The stamp decides this brush's edge.",
            Self::Angle => "A round dab has no angle to turn.",
            Self::ColourPickup => "An eraser lays down no colour to pick up.",
            _ => "",
        }
    }
}

/// Which tweak a shortcut asks for, and in which direction.
///
/// Here rather than in `shortcuts.rs` so that module stays a plain table of
/// bindings with nothing but winit behind it. `every_brush_action_names_a_
/// tweak` is what stops an action being added to the list and reaching
/// nothing.
pub fn of_action(action: Action) -> Option<(Tweak, f32)> {
    Tweak::ALL.into_iter().find_map(|tweak| {
        let (down, up) = tweak.actions();
        if action == down {
            Some((tweak, -1.0))
        } else if action == up {
            Some((tweak, 1.0))
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Painting
//
// Everything above is a model and is tested without a window. What follows
// draws it, and makes no decisions of its own.
// ---------------------------------------------------------------------------

/// The module's body: one rail and one hold-grip per setting.
pub fn panel(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    ui.add_space(4.0);
    ui.spacing_mut().item_spacing.y = 12.0;

    for tweak in Tweak::PANEL {
        let live = tweak.enabled(ed);
        ui.scope(|ui| {
            if !live {
                ui.disable();
            }
            row(ui, p, ed, tweak);
        });
        // The reason goes *under* the rail, which inserts a line and shifts
        // everything below it — the shape `ticking_a_layer_does_not_move_the_
        // layer_list` refuses. It is allowed here because it can never move a
        // row under the hand that caused it: Hardness flips only when the tip
        // changes, which is the Brushes panel's; Colour pickup flips only with
        // the tool, which is the rail's; and Angle flips when Roundness
        // reaches 100%, which is a rail *above* it. Reserving six blank lines
        // against a jump that cannot happen would be the worse trade.
        if !live {
            crate::ui::caption(ui, p, tweak.why_off());
        }
    }
    ui.add_space(6.0);
}

/// One setting: the rail, with its figure as a field.
///
/// The rail takes the whole line, and there is no second control beside it.
/// There used to be a hold-and-drag grip on the right of every row, three dots
/// in a column, offering the same value over a longer travel. It came off
/// because of what it *looked* like: three dots at the end of a row is a menu
/// everywhere else in this interface and everywhere else on the desktop, so it
/// was clicked rather than held — and a click did nothing at all, which reads
/// as a broken control rather than as a control being held wrong. What the grip
/// bought was travel, not reach; the rail is wider now that it has the row to
/// itself, and the figure beside it can be typed, which is the reach a longer
/// travel was standing in for.
///
/// `widgets::typed_row` rather than `widgets::slider_row`, and the same rail
/// the tool options strip draws — see `widgets::inline_slider`, which is that
/// one on a single line.
fn row(ui: &mut Ui, p: &Palette, ed: &mut Editor, tweak: Tweak) {
    let mut value = tweak.value(&ed.brush);
    if widgets::typed_row(
        ui,
        p,
        &mut value,
        &widgets::Rail {
            label: tweak.label(),
            span: tweak.span(),
            // What a typed figure is held to. The same as the span for all six
            // of `Tweak::PANEL`; Size is the one that differs and it is not
            // drawn here, because the options strip already has it.
            limit: tweak.range(),
            log: tweak.log(),
            snap: 0.0,
            deferred: false,
            figure: tweak.figure(),
        },
    ) {
        tweak.apply(&mut ed.brush, value);
    }
}

/// The module's picture in the module library, painted into `body`.
///
/// Its own function rather than an arm of `panels::module_preview`'s match,
/// because the schematic is this module's business and `panels.rs` is
/// everybody's. See that function for the rule it follows: a schematic in
/// palette tokens, never a bitmap.
pub fn preview(painter: &egui::Painter, p: &Palette, body: egui::Rect) {
    let ink = p.text_dim;
    // Three rails across the whole width, which is exactly what the panel is.
    for k in 0..3 {
        let y = body.top() + 6.0 + k as f32 * 11.0;
        let right = body.right();
        painter.rect_filled(
            egui::Rect::from_min_max(pos2(body.left(), y - 1.0), pos2(right, y + 1.0)),
            1.0,
            ink,
        );
        painter.circle_filled(
            pos2(
                body.left() + (right - body.left()) * (0.3 + k as f32 * 0.25),
                y,
            ),
            2.5,
            p.accent,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::metrics;
    use glam::vec2;
    use umber_core::geom::drag_towards_more;

    /// A stamp for the two tests that need one in the brush's hand. Its shape
    /// is irrelevant — what is being read is that there *is* one.
    fn solid_tip() -> umber_core::TipMask {
        umber_core::TipMask::new(4, 4, vec![255; 16]).expect("a 4x4 mask")
    }

    #[test]
    fn a_size_press_is_still_the_115_it_always_was() {
        // The size shortcut multiplied by exactly 1.15 before this module
        // existed, and CLAUDE.md states that figure as "this rate over 20
        // pixels". It now falls out of `STEP_PX` and `RESIZE_DOUBLE_PX`
        // instead of being written down beside them, so this is the guard that
        // the derivation still lands where the hand-written constant did.
        let mut brush = Brush {
            size: 100.0,
            ..Brush::default()
        };
        Tweak::Size.nudge(&mut brush, 1.0);
        assert!(
            (brush.size - 115.0).abs() < 0.2,
            "a press up gave {}",
            brush.size
        );
        Tweak::Size.nudge(&mut brush, -1.0);
        assert!(
            (brush.size - 100.0).abs() < 0.01,
            "and back again gave {}",
            brush.size
        );
    }

    #[test]
    fn the_grip_moves_the_size_exactly_as_the_alt_drag_does() {
        // The two must not drift: one is `Brush::size_after_drag` on the
        // canvas and the other is this table in a panel, and a painter who
        // uses both would feel any difference immediately.
        for delta in [
            vec2(50.0, 0.0),
            vec2(-30.0, 12.0),
            vec2(0.0, -80.0),
            vec2(17.0, -17.0),
        ] {
            let engine = Brush::size_after_drag(24.0, delta);
            let here = Tweak::Size.after_drag(24.0, drag_towards_more(delta));
            assert!(
                (engine - here).abs() < 1e-3,
                "{delta:?}: {engine} vs {here}"
            );
        }
    }

    #[test]
    fn a_drag_back_to_where_it_started_gives_the_setting_back_exactly() {
        // The absolute-against-the-press rule, for every tweak rather than
        // only for size. Stepping per event would fail this by a rounding
        // error at five hundred events a second.
        for tweak in Tweak::ALL {
            let mut brush = Brush::default();
            let from = tweak.value(&brush);
            tweak.apply(&mut brush, tweak.after_drag(from, 130.0));
            tweak.apply(&mut brush, tweak.after_drag(from, 0.0));
            assert!(
                (tweak.value(&brush) - from).abs() < 1e-4,
                "{tweak:?} came back to {} rather than {from}",
                tweak.value(&brush)
            );
        }
    }

    #[test]
    fn nothing_can_be_driven_outside_its_own_range() {
        // Every route in clamps, because they all go through `apply`. A
        // thousand pixels of drag is a hand thrown across two monitors.
        for tweak in Tweak::ALL {
            let range = tweak.range();
            for along in [-4000.0_f32, -300.0, 0.0, 300.0, 4000.0] {
                let mut brush = Brush::default();
                let from = tweak.value(&brush);
                tweak.apply(&mut brush, tweak.after_drag(from, along));
                let v = tweak.value(&brush);
                assert!(
                    v >= *range.start() - 1e-4 && v <= *range.end() + 1e-4,
                    "{tweak:?} reached {v} at {along}px"
                );
            }
            // And a value nobody could have produced by dragging.
            let mut brush = Brush::default();
            tweak.apply(&mut brush, f32::NAN);
            assert!(tweak.value(&brush).is_finite(), "{tweak:?} took a NaN");
        }
    }

    #[test]
    fn roundness_is_the_reciprocal_of_the_engines_ratio() {
        // The engine states the dab as long-over-short and the interface says
        // roundness; the two are inverses, and 5% is the floor both hold to.
        let mut brush = Brush::default();
        Tweak::Roundness.apply(&mut brush, 0.25);
        assert!((brush.dab_ratio - 4.0).abs() < 1e-4);
        assert!((Tweak::Roundness.value(&brush) - 0.25).abs() < 1e-4);

        Tweak::Roundness.apply(&mut brush, 0.0);
        assert!((brush.dab_ratio - 1.0 / MIN_ROUNDNESS).abs() < 1e-3);

        // A circle reads as fully round rather than as something the reciprocal
        // put off the end of the rail.
        brush.dab_ratio = 1.0;
        assert!((Tweak::Roundness.value(&brush) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_dab_narrower_than_the_rail_reads_as_it_is_and_is_not_quietly_rewritten() {
        // `brushimport::kpp` bounds nothing, so a Krita brush with a 40:1 tip
        // arrives as a `dab_ratio` of 40 — off the left end of a rail that
        // stops at 5%. Reading it as the floor would mean the *first* press of
        // a shortcut, which reads, steps and writes, halved the dab's aspect
        // with nothing on screen to explain it. So the reading is honest, and
        // it is the same one `ui::brush_editor_tip`'s Roundness row takes.
        let mut brush = Brush {
            dab_ratio: 40.0,
            ..Brush::default()
        };
        assert!((Tweak::Roundness.value(&brush) - 0.025).abs() < 1e-6);

        // And merely reading it changes nothing, which is the half that
        // matters: a panel drawn over this brush leaves it at 40:1.
        let drawn = Tweak::Roundness.value(&brush);
        Tweak::Roundness.apply(&mut brush, drawn.clamp(MIN_ROUNDNESS, 1.0));
        assert!(
            brush.dab_ratio >= 20.0 - 1e-3,
            "an edit still lands on the rail"
        );
    }

    #[test]
    fn an_eraser_is_not_offered_a_colour_to_pick_up() {
        // Not merely a control that does nothing: `smudge` drives
        // `colours_dabs`, which puts the whole stroke on the coloured dab
        // pipeline, and the canvas probe is gated on `smudges()` with no mode
        // test — so a live rail here would cost an erasing stroke a readback
        // every frame for a colour that is never deposited.
        let mut ed = Editor::default();
        ed.brush.mode = umber_core::BrushMode::Paint;
        assert!(Tweak::ColourPickup.enabled(&ed));
        ed.brush.mode = umber_core::BrushMode::Erase;
        assert!(!Tweak::ColourPickup.enabled(&ed));
        assert!(!Tweak::ColourPickup.why_off().is_empty());

        // The other four are about the shape of the mark, not about what
        // colour it is, so an eraser keeps every one of them.
        for tweak in Tweak::PANEL {
            if matches!(tweak, Tweak::ColourPickup | Tweak::Angle) {
                continue;
            }
            assert!(tweak.enabled(&ed), "{tweak:?} went off with the eraser");
        }
    }

    #[test]
    fn every_rail_that_can_be_switched_off_says_why() {
        // `why_off` is not exhaustive over `Tweak` — most rails are never off —
        // so this is what stops one being switched off with a blank line under
        // it. The states are the ones `enabled` actually reads.
        let mut ed = Editor::default();
        ed.brush.dab_ratio = 1.0;
        ed.brush.mode = umber_core::BrushMode::Erase;
        ed.tip = Some(std::sync::Arc::new(solid_tip()));
        for tweak in Tweak::ALL {
            if !tweak.enabled(&ed) {
                assert!(!tweak.why_off().is_empty(), "{tweak:?} is off and silent");
            }
        }
    }

    #[test]
    fn a_tweak_reads_back_what_was_written_to_it() {
        // Every arm of `value` matches the arm of `apply` beside it. Two long
        // matches over eight variants is exactly where a copy-paste lands on
        // the wrong field, and the failure is silent: a rail that moves a
        // setting nobody was looking at.
        for tweak in Tweak::ALL {
            let range = tweak.range();
            let mid = (*range.start() + *range.end()) * 0.5;
            let mut brush = Brush::default();
            tweak.apply(&mut brush, mid);
            assert!(
                (tweak.value(&brush) - mid).abs() < 1e-3,
                "{tweak:?} read back {} rather than {mid}",
                tweak.value(&brush)
            );
            // And it moved nothing else: every other tweak still says what a
            // default brush says.
            for other in Tweak::ALL {
                if other == tweak {
                    continue;
                }
                let untouched = other.value(&Brush::default());
                assert!(
                    (other.value(&brush) - untouched).abs() < 1e-3,
                    "setting {tweak:?} also moved {other:?}"
                );
            }
        }
    }

    #[test]
    fn every_brush_action_names_a_tweak_and_every_tweak_has_both_directions() {
        // The two halves of the shortcut table's contract with this module. An
        // action in the settings list that reaches nothing is a row that lets
        // somebody bind a key to silence.
        for tweak in Tweak::ALL {
            let (down, up) = tweak.actions();
            assert_ne!(down, up, "{tweak:?} bound one action twice");
            assert_eq!(of_action(down), Some((tweak, -1.0)));
            assert_eq!(of_action(up), Some((tweak, 1.0)));
        }
        for action in Action::ALL {
            if action.category() == "Brush" {
                assert!(
                    of_action(action).is_some(),
                    "{} is filed under Brush and reaches nothing",
                    action.label()
                );
            } else {
                assert!(
                    of_action(action).is_none(),
                    "{} reaches a tweak from outside the Brush group",
                    action.label()
                );
            }
        }
    }

    #[test]
    fn the_brush_actions_are_contiguous_in_the_settings_list() {
        // `settings::shortcuts_pane` draws a heading whenever the category
        // changes as it walks `Action::ALL`, so a group split in two would
        // draw "Brush" twice with something else between.
        let mut seen = Vec::new();
        for action in Action::ALL {
            match seen.last() {
                Some(last) if *last == action.category() => {}
                _ => seen.push(action.category()),
            }
        }
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            seen.len(),
            sorted.len(),
            "a category is drawn in two separate runs: {seen:?}"
        );
    }

    #[test]
    fn the_shortcuts_are_all_unbound_by_default_except_size() {
        // Fourteen new keys nobody asked for would collide with whatever the
        // painter's hands already know, and this list is long enough that
        // choosing well for everybody is not possible. They are all bindable
        // and none is bound — except the size pair, which shipped bound and
        // stays that way.
        let defaults = crate::shortcuts::defaults();
        for tweak in Tweak::ALL {
            let (down, up) = tweak.actions();
            let bound = defaults
                .iter()
                .filter(|b| b.action == down || b.action == up)
                .count();
            let wanted = if tweak == Tweak::Size { 2 } else { 0 };
            assert_eq!(bound, wanted, "{tweak:?} has {bound} default bindings");
        }
    }

    #[test]
    fn the_shortcuts_search_finds_every_rail_by_the_name_on_it() {
        // Fourteen rows is a long enough list that the search is how somebody
        // gets to one, and `settings::shortcuts_pane` filters on the label and
        // the category folded to lower case. So the name on the rail has to be
        // in the label of the pair that moves it — which is not automatic:
        // "Angle" is spelt "dab angle" there, and an action named for the
        // engine's field rather than for the control would be unfindable by
        // anybody looking at the panel.
        for tweak in Tweak::PANEL {
            let needle = tweak.label().to_lowercase();
            let hits = Action::ALL
                .iter()
                .filter(|a| {
                    a.label().to_lowercase().contains(&needle)
                        || a.category().to_lowercase().contains(&needle)
                })
                .count();
            assert_eq!(hits, 2, "searching for {needle:?} found {hits} rows");
        }
    }

    #[test]
    fn a_panel_tweak_is_one_a_shortcut_can_reach() {
        for tweak in Tweak::PANEL {
            assert!(Tweak::ALL.contains(&tweak));
        }
    }

    /// The size rail stops short of what a size may be, and it is the only one
    /// that does.
    ///
    /// Two figures for one setting is a thing to state rather than to notice:
    /// [`Tweak::span`] is what the rail lays out and [`Tweak::range`] is what a
    /// typed figure — or a shortcut, or the canvas's Alt-drag — is held to.
    /// Where the two agree there is nothing to get wrong, so this asserts both
    /// that they *disagree* for Size and that they agree for everything else.
    #[test]
    fn only_the_size_rail_stops_short_of_what_its_setting_may_be() {
        assert_eq!(*Tweak::Size.span().end(), SIZE_RAIL_TOP);
        assert_eq!(*Tweak::Size.range().end(), Brush::MAX_SIZE);
        // That the two differ at all is a `const` assert beside the constant,
        // for the reason written there.
        assert_eq!(*Tweak::Size.span().start(), *Tweak::Size.range().start());

        for tweak in Tweak::ALL {
            if tweak == Tweak::Size {
                continue;
            }
            assert_eq!(
                tweak.span(),
                tweak.range(),
                "{tweak:?} draws a rail that cannot reach its own setting"
            );
        }
    }

    /// The readout is exactly what it always was.
    ///
    /// `format` is `widgets::Figure`'s now rather than four match arms, which
    /// is what makes the field's parser the exact inverse of it — and is
    /// therefore also where a wrong scale or a lost suffix would be silent, on
    /// every rail at once. Golden strings rather than a round trip, because a
    /// round trip is self-consistent under any pair of mistakes that cancel.
    #[test]
    fn the_readout_of_every_setting_is_the_string_it_has_always_been() {
        assert_eq!(Tweak::Size.format(24.0), "24 px");
        assert_eq!(Tweak::Size.format(1045.08), "1045 px");
        assert_eq!(Tweak::Angle.format(90.0), "90°");
        assert_eq!(Tweak::AirbrushRate.format(0.0), "off");
        assert_eq!(Tweak::AirbrushRate.format(60.0), "60/s");
        assert_eq!(Tweak::Opacity.format(0.5), "50%");
        assert_eq!(Tweak::Hardness.format(1.0), "100%");
        assert_eq!(Tweak::Spacing.format(0.075), "8%");
        assert_eq!(Tweak::Roundness.format(0.25), "25%");
        assert_eq!(Tweak::ColourPickup.format(0.0), "0%");
    }

    #[test]
    fn a_stamp_switches_hardness_off_and_gives_a_round_dab_its_angle() {
        // The two readings the brush editor takes, so the rails cannot be live
        // in one place and dead in the other.
        let mut ed = Editor::default();
        ed.brush.dab_ratio = 1.0;
        assert!(Tweak::Hardness.enabled(&ed), "no stamp: hardness is live");
        assert!(!Tweak::Angle.enabled(&ed), "a circle has no angle");

        ed.brush.dab_ratio = 4.0;
        assert!(Tweak::Angle.enabled(&ed), "an ellipse does");

        ed.brush.dab_ratio = 1.0;
        ed.tip = Some(std::sync::Arc::new(solid_tip()));
        assert!(!Tweak::Hardness.enabled(&ed), "a stamp decides the edge");
        assert!(
            Tweak::Angle.enabled(&ed),
            "a bitmap is not rotationally symmetric"
        );

        for tweak in Tweak::PANEL {
            if matches!(tweak, Tweak::Hardness | Tweak::Angle) {
                assert!(!tweak.why_off().is_empty(), "{tweak:?} needs a reason");
            }
        }
    }

    #[test]
    fn picking_a_brush_puts_every_tweak_back() {
        // The reset, and the whole of it. This is the rule the module docs
        // rest on, and it is worth pinning because nothing in this module
        // implements it — `Editor::apply_preset` does, by assigning the
        // preset's brush wholesale.
        let mut ed = Editor::default();
        let Some(preset) = ed.presets.first().cloned() else {
            return;
        };
        ed.apply_preset(0);
        let before: Vec<f32> = Tweak::ALL.iter().map(|t| t.value(&ed.brush)).collect();
        for tweak in Tweak::ALL {
            tweak.nudge(&mut ed.brush, 3.0);
        }
        ed.apply_preset(0);
        for (tweak, was) in Tweak::ALL.into_iter().zip(before) {
            assert!(
                (tweak.value(&ed.brush) - was).abs() < 1e-3,
                "{tweak:?} survived picking {} again",
                preset.name
            );
        }
    }

    /// What the module looks like, in both themes.
    ///
    /// Written rather than asserted for the reason `layers_panel_preview` is:
    /// six rails and six grips laid out in 264 points is a thing to be looked
    /// at, and the one question worth asking of it — does a disabled rail read
    /// as switched off rather than as broken — has no numeric answer.
    ///
    /// ```sh
    /// cargo test -p umber-app tweaks_panel_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn tweaks_panel_preview() {
        use crate::dock::{Layout, PanelKind};
        use crate::docshot;
        use crate::theme::ThemeKind;
        use egui::{Pos2, Rect};

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tweaks");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        for (name, theme, stamped) in [
            ("1-graphite", ThemeKind::Graphite, false),
            ("2-paper", ThemeKind::Paper, false),
            ("3-stamped", ThemeKind::Graphite, true),
        ] {
            let mut ed = Editor::default();
            ed.layout = Layout::default();
            ed.ui.theme = theme;
            ed.brush.dab_ratio = 2.5;
            if stamped {
                ed.tip = Some(std::sync::Arc::new(solid_tip()));
                ed.brush.dab_ratio = 1.0;
            }
            let palette = Palette::with_accent(ed.ui.theme, ed.ui.accent);
            // `egui::vec2` by name: this test module imports `glam::vec2` for
            // the drag deltas, and the two types are distinct.
            let field = egui::vec2(metrics::PANEL, 320.0);
            let rect = Rect::from_min_size(Pos2::ZERO, field);
            let image = stage.shoot(field, 2.0, &palette, palette.dock, |root| {
                let mut actions = crate::ui::UiActions::default();
                crate::panels::panel(
                    root,
                    &palette,
                    &mut ed,
                    &mut actions,
                    PanelKind::Tweaks,
                    rect,
                );
            });
            docshot::write_png(&dir.join(format!("{name}.png")), &image).expect("write the png");
        }
        // And the card the module library draws, whole — frame, header and
        // all — through `panels::module_preview` rather than by calling
        // `preview` with a field worked out here. The body it hands over is
        // the card less a 9-point header, 5 points under it, a 6-point left
        // margin and 5 points off each end, and every one of those is a number
        // this test would have had to restate and could then have got wrong.
        // A schematic checked against a field a few points off the real one is
        // worth less than no picture, because it looks like evidence.
        let field = egui::Vec2::from(metrics::MODULE_PREVIEW);
        let palette = Palette::with_accent(ThemeKind::Graphite, crate::theme::Accent::Umber);
        let image = stage.shoot(field, 4.0, &palette, palette.window, |root| {
            crate::panels::module_preview(
                root.painter(),
                &palette,
                Rect::from_min_size(Pos2::ZERO, field),
                PanelKind::Tweaks,
            );
        });
        docshot::write_png(&dir.join("4-card.png"), &image).expect("write the png");

        println!("wrote 4 shots to {}", dir.display());
    }
}
