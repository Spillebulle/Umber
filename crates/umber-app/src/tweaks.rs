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
//!   assigns `self.brush = preset.brush` wholesale, so selecting *any* brush —
//!   including the one already in hand — discards every tweak. That is the
//!   reset, it is the only one, and it is the same one Size and Opacity have.
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
//! ## One table, three ways of asking
//!
//! A rail, an increase/decrease shortcut and a hold-and-drag grip are three
//! spellings of one question, so they go through one [`Tweak`]. What makes it
//! worth a table rather than three call sites is the *step*: one press of a
//! shortcut is worth [`STEP_PX`] pixels of the drag. `Brush::RESIZE_DOUBLE_PX`
//! is 100, so a size press is `2^(20/100)` — 1.1487, the 1.15 the size
//! shortcut has always used, now falling out of the rule instead of restating
//! it. `a_size_press_is_still_the_115_it_always_was` pins that.
//!
//! ## The grip is not a canvas gesture, and must not become one
//!
//! `gesture.rs` answers "what does a press **on the canvas** mean", and the
//! grip's press lands on a panel — where `ui_owns` is true and the answer is
//! `Press::Ignored`, correctly and unchanged. So nothing here is in that model
//! and `a_pen_press_resolves_to_what_a_mouse_press_would` did not have to
//! move. It still reaches a pen: `egui-winit` turns `WindowEvent::Touch` into
//! ordinary pointer events, so a nib pressing the grip drags it exactly as a
//! mouse does, and `app.rs`'s touch arm ignores the press for the same reason
//! its mouse arm does.
//!
//! Putting six more settings into `gesture::press` would be the combinatorial
//! mess: Alt is already spoken for twice on the canvas — the eyedropper with a
//! button and the size drag without one — and there is no seventh modifier, so
//! it could only be a modal "which setting is Alt about?" that the pointer
//! could not see. A control that says which setting it adjusts by *being* that
//! setting's grip is the answer, and it is the one every other application with
//! this feature reaches for.

use crate::editor::Editor;
use crate::shortcuts::Action;
use crate::theme::{Palette, metrics};
use crate::widgets;
use egui::{Pos2, Sense, Stroke, Ui, pos2, vec2};
use std::ops::RangeInclusive;
use umber_core::Brush;

/// How far the pointer travels, in physical window pixels, for a *linear*
/// setting to cross its whole range under the hold-grip.
///
/// A logarithmic one uses `Brush::RESIZE_DOUBLE_PX` instead, which is the rate
/// the brush-size drag has always used — the point being that a size dragged
/// by this grip and a size dragged by Alt move at exactly the same rate.
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

    /// The six the module draws, in the order the brush editor's Tip section
    /// already puts them.
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

    /// What the setting may be set to.
    ///
    /// Size's is the *whole* range rather than the 400 the two rails stop at:
    /// the size shortcut has always clamped at `Brush::MAX_SIZE` and a rail's
    /// span is not a bound on the value.
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

    /// Whether the rail — and the drag — is logarithmic.
    ///
    /// The same two the brush editor draws logarithmically, for the same
    /// reason: the difference between a 3-pixel liner and a 6-pixel one is the
    /// whole character of the brush, and six pixels on a 300-pixel wash is
    /// nothing.
    pub fn log(self) -> bool {
        matches!(self, Self::Size | Self::Spacing)
    }

    /// The readout, in the setting's own units.
    pub fn format(self, value: f32) -> String {
        match self {
            Self::Size => format!("{value:.0} px"),
            Self::Angle => format!("{value:.0}°"),
            Self::AirbrushRate => {
                if value <= 0.0 {
                    "off".to_string()
                } else {
                    format!("{value:.0}/s")
                }
            }
            _ => format!("{:.0}%", value * 100.0),
        }
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
            Self::Roundness => (1.0 / brush.dab_ratio.max(1.0)).clamp(MIN_ROUNDNESS, 1.0),
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
            _ => true,
        }
    }

    /// Why the rail is switched off, for the caption under it.
    pub fn why_off(self) -> &'static str {
        match self {
            Self::Hardness => "The stamp decides this brush's edge.",
            Self::Angle => "A round dab has no angle to turn.",
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
    crate::controls::note(
        ui,
        p,
        "Changes here go to the brush in hand, like Size and Opacity above the \
         canvas. Picking a brush puts its own settings back; the brush editor's \
         Update is what makes one stick.",
    );
    ui.add_space(10.0);
    ui.spacing_mut().item_spacing.y = 12.0;

    for tweak in Tweak::PANEL {
        let live = tweak.enabled(ed);
        ui.scope(|ui| {
            if !live {
                ui.disable();
            }
            row(ui, p, ed, tweak);
        });
        if !live {
            ui.label(
                egui::RichText::new(tweak.why_off())
                    .size(10.0)
                    .color(p.text_dim),
            );
        }
    }
    ui.add_space(6.0);
}

/// One setting: the rail, and the grip beside it.
fn row(ui: &mut Ui, p: &Palette, ed: &mut Editor, tweak: Tweak) {
    let mut value = tweak.value(&ed.brush);
    // The grip's width comes off the line before the rail is drawn, because
    // `widgets::slider_row` sizes itself from `available_width` and would
    // otherwise take all of it and push the grip off the panel.
    let rail = (ui.available_width() - metrics::TWEAK_GRIP - 8.0).max(metrics::TWEAK_GRIP);
    ui.horizontal(|ui| {
        // **`vertical`, not `scope`.** A `slider_row` is two rows — the name
        // and readout on one baseline, the rail under them — and it allocates
        // them one after another in whatever layout it is handed. Given the
        // horizontal one this line is in, it laid the rail out *beside* its own
        // header and pushed both off the edge of the panel: labels with no
        // rails under them, which is what the first shot of
        // `tweaks_panel_preview` showed.
        ui.vertical(|ui| {
            ui.set_width(rail);
            if widgets::slider_row(
                ui,
                p,
                tweak.label(),
                &mut value,
                tweak.range(),
                tweak.log(),
                |v| tweak.format(v),
            ) {
                tweak.apply(&mut ed.brush, value);
            }
        });
        grip(ui, p, ed, tweak);
    });
}

/// The hold-and-drag handle: press it, drag anywhere, let go.
///
/// The rail beside it is the precise control and this is the wide one — a
/// panel is 264 points across, so a rail's whole range is a couple of hundred
/// pixels of travel, while this measures against the screen and keeps
/// measuring wherever the pointer goes. It is the brush-size drag's rate and
/// the brush-size drag's absolute-against-the-press rule, for every setting
/// that has no modifier left to spell that with.
///
/// The value at the press and where the press landed live in egui's own
/// per-widget memory rather than on [`Editor`]: they belong to one gesture on
/// one widget, which is exactly what that memory is for, and it keeps a
/// transient out of the state a tab switch has to reason about.
///
/// A panel body is a `ScrollArea`, which senses a drag of its own so a finger
/// can scroll it, and this widget is drawn over that. It wins for the same
/// reason every rail already in a panel does — the layer stack's opacity
/// slider, the colour picker's three — because egui resolves an overlap to the
/// topmost widget that senses the drag. A control here that lost to the scroll
/// would be a control every panel in Umber has already been proving works.
fn grip(ui: &mut Ui, p: &Palette, ed: &mut Editor, tweak: Tweak) {
    let size = metrics::TWEAK_GRIP;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());

    if response.drag_started()
        && let Some(origin) = response.interact_pointer_pos()
    {
        let from = tweak.value(&ed.brush);
        ui.data_mut(|d| d.insert_temp(response.id, (from, origin)));
    }
    if response.dragged()
        && let Some(now) = response.interact_pointer_pos()
        && let Some((from, origin)) = ui.data(|d| d.get_temp::<(f32, Pos2)>(response.id))
    {
        // egui works in points and the drag rate is stated in physical window
        // pixels, which is what makes this grip travel at the same rate as the
        // Alt-drag on the canvas whatever the interface scale is.
        let scale = ed.pixels_per_point.max(1e-3);
        let delta = glam::vec2((now.x - origin.x) * scale, (now.y - origin.y) * scale);
        let along = umber_core::geom::drag_towards_more(delta);
        tweak.apply(&mut ed.brush, tweak.after_drag(from, along));
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if response.drag_stopped() {
        ui.data_mut(|d| d.remove::<(f32, Pos2)>(response.id));
    }

    let active = response.dragged();
    let ink = if active || response.hovered() {
        p.accent
    } else {
        p.text_dim
    };
    let painter = ui.painter();
    if active || response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control);
    }
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(1.0, if active { p.accent } else { p.border }),
        egui::StrokeKind::Inside,
    );
    // Three dots in a column: the grab mark this interface already uses on a
    // panel header, rather than a glyph — Archivo carries none of the ones
    // that would say this.
    for k in 0..3 {
        painter.circle_filled(
            pos2(rect.center().x, rect.center().y + (k as f32 - 1.0) * 4.0),
            1.2,
            ink,
        );
    }

    response.on_hover_text(format!(
        "Hold and drag to set {}. Right and up for more, left and down for \
         less; come back to where you pressed and it is exactly what it was.",
        tweak.label().to_lowercase()
    ));
}

/// The module's picture in the module library, painted into `body`.
///
/// Its own function rather than an arm of `panels::module_preview`'s match,
/// because the schematic is this module's business and `panels.rs` is
/// everybody's. See that function for the rule it follows: a schematic in
/// palette tokens, never a bitmap.
pub fn preview(painter: &egui::Painter, p: &Palette, body: egui::Rect) {
    let ink = p.text_dim;
    // Three rails with a grip beside each, which is exactly what the panel is.
    for k in 0..3 {
        let y = body.top() + 6.0 + k as f32 * 11.0;
        let right = body.right() - 8.0;
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
        for dot in 0..3 {
            painter.circle_filled(pos2(right + 4.0, y + (dot as f32 - 1.0) * 2.2), 0.7, ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // And the schematic the module library draws on this module's card.
        // The field is what `panels::module_preview` hands `preview`: the
        // `metrics::MODULE_PREVIEW` card less its 9-point header, its 6-point
        // left margin and 5 points off each end. Restated here only so there
        // is something to look at — the caller is still the one that decides
        // it, and a picture drawn a few points larger than the card is exactly
        // the kind of thing that has to be seen rather than asserted.
        let field = egui::vec2(72.0, 34.0);
        let palette = Palette::with_accent(ThemeKind::Graphite, crate::theme::Accent::Umber);
        let image = stage.shoot(field, 4.0, &palette, palette.control, |root| {
            preview(
                root.painter(),
                &palette,
                Rect::from_min_size(Pos2::ZERO, field),
            );
        });
        docshot::write_png(&dir.join("4-card.png"), &image).expect("write the png");

        println!("wrote 4 shots to {}", dir.display());
    }
}
