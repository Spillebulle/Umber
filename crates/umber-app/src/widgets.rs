//! Widgets drawn to match the design.
//!
//! egui's stock slider, checkbox and radio group have a look of their own that
//! the Graphite design does not use — thin rails with a round knob, pill
//! toggles, segmented pickers. These are painted directly rather than fought
//! with via styling.

use crate::icons::{self, Icon};
use crate::theme::{Palette, metrics, text};
use egui::{Align2, Color32, FontId, Rect, Response, Sense, Stroke, Ui, Vec2, pos2, vec2};
use std::ops::RangeInclusive;
use std::sync::Arc;
use umber_core::{
    Brush, Dab, InputPoint, ResponseCurve, ScrollSpan, StrokeBuilder, TipMask, preview,
};

/// Narrowest anything here will draw itself.
///
/// A docked panel can be dragged down to a width that leaves a slider or a
/// picker no room at all, and `available_width` then comes back at or below
/// zero. An `egui::Rect` built from a negative size has its max to the left of
/// its min, which does not panic — it paints somewhere unrelated, or fills the
/// whole panel. Clamping here means a squeezed control is merely useless rather
/// than wrong.
const MIN_TRACK: f32 = 8.0;

/// Label on the left, monospace readout on the right, thin rail beneath.
///
/// Returns true when the value changed. `log` maps the rail logarithmically,
/// which is what makes a 1–400 px brush size usable — half the travel covers
/// 1–20 px, where the useful sizes actually live.
///
/// The rail is immediate: the value is handed back as the knob moves. The one
/// case that cannot be — a rail drawn *inside* the thing it scales, which moves
/// out from under the pointer if it is applied per frame — is
/// [`NumberRow::deferred`], on the row that also lets the figure be typed.
pub fn slider_row(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    log: bool,
    display: impl Fn(f32) -> String,
) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let log = log && lo > 0.0 && hi > lo;
    let mut changed = false;

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;

        // A panel squeezed to its minimum can leave nothing here at all, and a
        // negative width makes a `Rect` whose max is left of its min — which
        // paints as a stray mark somewhere it was never asked to.
        let width = ui.available_width().max(MIN_TRACK);

        // Header: name and current value on one baseline.
        let (header, _) = ui.allocate_exact_size(vec2(width, text::SMALL + 2.0), Sense::hover());
        let painter = ui.painter();
        painter.text(
            header.left_center(),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(text::SMALL),
            p.text_dim,
        );
        painter.text(
            header.right_center(),
            Align2::RIGHT_CENTER,
            display(*value),
            FontId::monospace(text::TINY),
            p.text,
        );

        // Rail: a tall invisible hit area around a thin visible track, so the
        // 3 px rail is still comfortable to grab.
        let (row, response) =
            ui.allocate_exact_size(vec2(width, metrics::SLIDER_ROW), Sense::click_and_drag());
        let track = Rect::from_center_size(
            row.center(),
            vec2(
                (row.width() - metrics::SLIDER_KNOB).max(MIN_TRACK),
                metrics::SLIDER_RAIL,
            ),
        );

        changed = drag_track(&response, track, value, lo, hi, log, 0.0);
        paint_track(
            ui.painter(),
            p,
            track,
            to_t(*value, lo, hi, log),
            metrics::SLIDER_KNOB,
        );
    });

    changed
}

/// A square icon that is either on or off, and can be unavailable.
///
/// The Layers panel's flags — clip, lock, link — are all of this shape:
/// something the selected layer either is or is not, small enough to sit four
/// across in a docked panel. It is not [`toggle`], which is a 28-px pill and
/// belongs on a labelled row; and it is not [`crate::ui::icon_button`], which
/// has no on state at all, so a lock drawn with it would look identical whether
/// the layer was locked or not.
///
/// **Off is not dim.** Dim means "unavailable" everywhere in this interface, so
/// an off toggle keeps the ordinary text colour and it is the *chip* behind it
/// that appears when it is on. A disabled one is dim and does not respond,
/// which is the state the tooltip has to explain.
pub fn icon_toggle(
    ui: &mut Ui,
    p: &Palette,
    icon: Icon,
    on: bool,
    enabled: bool,
    tip: &str,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::splat(20.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hovered = enabled && response.hovered();
    if on {
        ui.painter()
            .rect_filled(rect, metrics::RADIUS, p.control_active);
        ui.painter().rect_stroke(
            rect,
            metrics::RADIUS,
            Stroke::new(1.0, p.accent_dim),
            egui::StrokeKind::Inside,
        );
    } else if hovered {
        ui.painter().rect_filled(rect, metrics::RADIUS, p.control);
    }
    icons::draw(
        ui.painter(),
        rect.shrink(2.0),
        icon,
        if !enabled {
            p.text_dim.gamma_multiply(0.4)
        } else if on || hovered {
            p.text_strong
        } else {
            p.text
        },
    );
    response.on_hover_text(tip).clicked()
}

/// A progress bar: a track the full width, filled to `fraction`.
///
/// `None` means there is no honest figure to draw, and it produces an **empty
/// track** rather than an animation. A bar that moves without knowing anything
/// is the control that lies, and the one place in Umber where progress cannot
/// be reported — Windows' installer, once it has been handed the package —
/// says so in words beside the bar instead.
///
/// The splash paints its own copy of this shape and has to: it runs before wgpu
/// exists, on a `softbuffer` framebuffer with no egui in the process at all.
/// This one is the interface's. They share the palette tokens and the
/// proportions, which is as much as two renderers with no common drawing API
/// can share.
pub fn progress_bar(ui: &mut Ui, p: &Palette, fraction: Option<f32>) {
    let width = ui.available_width().max(MIN_TRACK);
    let height = metrics::PROGRESS_BAR;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let radius = height * 0.5;
    let painter = ui.painter();
    // Track first, then the fill over it, so the fill's rounded cap sits on the
    // track rather than knocking a notch out of it. `rail` is the slider-track
    // token, which is what this is.
    painter.rect_filled(rect, radius, p.rail);
    let Some(fraction) = fraction else {
        return;
    };
    let filled = width * fraction.clamp(0.0, 1.0);
    // Below one radius the rounded fill draws as a lens narrower than it should
    // be; a bar that reads as empty at 1% is better than one that reads as
    // started at 0%.
    if filled >= radius {
        let end = Rect::from_min_size(rect.min, vec2(filled, height));
        painter.rect_filled(end, radius, p.accent);
    }
}

/// Pill toggle, 28×16 with a sliding knob.
pub fn toggle(ui: &mut Ui, p: &Palette, on: &mut bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(28.0, 16.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }

    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, if *on { p.accent } else { p.rail });
    let knob_x = if *on {
        rect.right() - 2.0 - 6.0
    } else {
        rect.left() + 2.0 + 6.0
    };
    painter.circle_filled(pos2(knob_x, rect.center().y), 6.0, p.knob);
    response
}

/// Dim label on the left, [`toggle`] pushed to the right edge.
///
/// Here rather than beside its callers because two modules now draw one, and a
/// second copy is how the pill ends up a different size in the Colour panel
/// than in the brush editor.
pub fn toggle_row(ui: &mut Ui, p: &Palette, label: &str, value: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .size(text::SMALL)
                .color(p.text_dim),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            toggle(ui, p, value);
        });
    });
}

/// A row of mutually exclusive choices inside an inset well.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    p: &Palette,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    if options.is_empty() {
        return false;
    }
    let mut changed = false;

    let width = ui.available_width().max(MIN_TRACK);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, metrics::RADIUS, p.window);

    // `shrink` on a rect narrower than its own inset inverts it, so the inner
    // well is built from the clamped width rather than taken off the outside.
    let inner = Rect::from_center_size(
        rect.center(),
        vec2((rect.width() - 4.0).max(MIN_TRACK), rect.height() - 4.0),
    );
    let cell_w = inner.width() / options.len() as f32;

    for (i, (value, label)) in options.iter().enumerate() {
        let cell = Rect::from_min_size(
            pos2(inner.left() + cell_w * i as f32, inner.top()),
            vec2(cell_w, inner.height()),
        );
        let response = ui.interact(cell, ui.id().with((label, i)), Sense::click());
        if response.clicked() {
            *current = *value;
            changed = true;
        }

        let selected = *current == *value;
        let painter = ui.painter();
        if selected {
            painter.rect_filled(cell, metrics::RADIUS - 1.0, p.control_hover);
        } else if response.hovered() {
            painter.rect_filled(cell, metrics::RADIUS - 1.0, p.control);
        }
        painter.text(
            cell.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(text::TINY),
            if selected { p.text_strong } else { p.text_dim },
        );
    }

    changed
}

/// The leading mark's box on a [`dropdown`] that has one.
const DROPDOWN_ICON: f32 = 12.0;
/// The chevron at a [`dropdown`]'s right-hand end.
const DROPDOWN_CHEVRON: f32 = 11.0;
/// Between any two of a [`dropdown`]'s parts.
const DROPDOWN_GAP: f32 = 4.0;

/// How wide a [`dropdown`] draws itself.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DropdownWidth {
    /// Exactly what its own contents need. The trigger sits in a row of other
    /// things — a panel header, the tool options strip — and a fixed width
    /// there would leave a gap that means nothing.
    Content,
    /// The whole width the layout is offering. What a trigger on its own line
    /// in a panel body or one of the brush editor's columns wants: it is the
    /// only thing on that line, so anything narrower reads as a mistake.
    Fill,
    /// Exactly this. For the pickers whose width is decided by something beside
    /// them rather than by the layout — the curve presets, as wide as the curve
    /// panel they sit under, and the layer blend, which shares its row with the
    /// opacity slider and must not take the room that wants.
    Exact(f32),
}

/// A dropdown trigger: an optional mark, the current choice, an optional figure
/// at the right, and a chevron.
///
/// A struct rather than five positional arguments, and built up rather than
/// written out, because most call sites want only the label — see
/// [`Dropdown::new`].
pub struct Dropdown<'a> {
    /// The choice currently in force. Elided if the width it is given cannot
    /// hold it.
    label: &'a str,
    /// A mark for *what* is being chosen, drawn before the label.
    ///
    /// Optional because most of these have no natural one: "Drives", "Driven
    /// by" and the curve presets name an abstraction, and a glyph invented to
    /// fill the slot would be worse than the gap — it would have to be learnt,
    /// and it would say something the interface does not mean.
    icon: Option<Icon>,
    /// A figure between the label and the chevron: the brush library's count of
    /// what is in the collection. Monospace and dim, because it is a reading
    /// rather than part of the choice.
    trailing: Option<&'a str>,
    width: DropdownWidth,
}

impl<'a> Dropdown<'a> {
    /// A trigger showing `label`, sized to its own contents, with no mark and
    /// no figure.
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            icon: None,
            trailing: None,
            width: DropdownWidth::Content,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn trailing(mut self, trailing: &'a str) -> Self {
        self.trailing = Some(trailing);
        self
    }

    pub fn width(mut self, width: DropdownWidth) -> Self {
        self.width = width;
        self
    }
}

/// Where a trigger's label starts, relative to its left edge.
fn dropdown_lead(icon: bool) -> f32 {
    if icon {
        DROPDOWN_ICON + DROPDOWN_GAP
    } else {
        0.0
    }
}

/// What a trigger's fixed parts take: the mark, the figure, the chevron and the
/// gaps between them. Everything left over is the label's.
///
/// One function, used both to size a trigger to its content — furniture plus
/// the measured label — and to decide how much of a trigger of a *given* width
/// the label may have. Stating it twice is how a picker ends up a few pixels
/// short of its own longest option, eliding a name that fits, which nobody sees
/// until they pick that one option.
fn dropdown_furniture(icon: bool, trailing: Option<f32>) -> f32 {
    dropdown_lead(icon)
        + trailing.map_or(0.0, |w| w + DROPDOWN_GAP)
        + DROPDOWN_GAP
        + DROPDOWN_CHEVRON
}

/// The dropdown. One trigger, one menu, everywhere in the interface.
///
/// The look is the Colour panel's picker-type switch: dim text that comes up to
/// [`Palette::text_strong`] under the pointer, with a chevron after it and no
/// fill at all. There used to be four looks for the one gesture — that switch,
/// a filled pill on the tool options strip, a full-width row in the brush
/// library, and five stock `egui::ComboBox`es — so the same act of choosing
/// read as four different controls depending where you met it. A stock
/// ComboBox is also the thing this module exists not to do: egui's own widgets
/// have a look the design does not use, and restyling them fights the framework
/// rather than settling it.
///
/// **The menu is `egui::Popup::menu`, and every caller opens it that way.** The
/// alternative in use was a `bool` on `Editor::ui` toggled by the trigger and
/// cleared when the popup came back `None`. It works, but it puts a field on the
/// editor for every dropdown anybody adds — state that is not per-document,
/// that nothing else reads, and that has to be cleared in two places or the menu
/// reopens itself. `Popup::menu` keeps that in egui's memory against the
/// trigger's own id, which is exactly the scope the flag was standing in for:
/// the popup toggles on a click, closes on a click anywhere, and closes on
/// Escape without anybody writing that down. A painted trigger is a plain
/// `Response`, which is all `Popup::menu` ever wanted.
///
/// The body is laid out the way `ComboBox` lays its own out — justified, wrap
/// off so a long entry widens the menu rather than folding, and inside a scroll
/// area, because a menu taller than the window has entries that cannot be
/// reached. Returns whatever the body returned, or `None` when the menu is shut.
pub fn dropdown<R>(
    ui: &mut Ui,
    p: &Palette,
    trigger: Dropdown<'_>,
    menu: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let font = FontId::proportional(text::TINY);
    let figure = FontId::monospace(text::TINY);
    let measure = |s: &str, font: &FontId| {
        ui.painter()
            .layout_no_wrap(s.to_owned(), font.clone(), p.text)
            .size()
            .x
    };
    let label_w = measure(trigger.label, &font);
    let trailing_w = trigger.trailing.map(|t| measure(t, &figure));

    let furniture = dropdown_furniture(trigger.icon.is_some(), trailing_w);
    let width = match trigger.width {
        DropdownWidth::Content => furniture + label_w,
        DropdownWidth::Fill => ui.available_width(),
        DropdownWidth::Exact(w) => w,
    };
    // A panel dragged to its minimum can offer nothing at all, and a `Rect`
    // built from a negative width has its max left of its min — which paints
    // somewhere unrelated rather than panicking.
    let width = width.max(MIN_TRACK);

    let (rect, response) = ui.allocate_exact_size(vec2(width, metrics::DROPDOWN), Sense::click());
    let ink = if response.hovered() {
        p.text_strong
    } else {
        p.text_dim
    };

    let painter = ui.painter();
    if let Some(icon) = trigger.icon {
        icons::draw(
            painter,
            Rect::from_min_size(rect.left_top(), vec2(DROPDOWN_ICON, rect.height())),
            icon,
            ink,
        );
    }
    // The chevron is last on every trigger, whatever else is on one: it is the
    // mark that says this opens, so it has to be in the same place each time.
    // The figure goes inside it rather than beyond it, which is the one thing
    // the brush library's own switch used to do the other way round.
    let chevron = Rect::from_min_size(
        pos2(rect.right() - DROPDOWN_CHEVRON, rect.top()),
        vec2(DROPDOWN_CHEVRON, rect.height()),
    );
    icons::draw(painter, chevron, Icon::ChevronDown, ink);
    if let Some(text) = trigger.trailing {
        painter.text(
            pos2(chevron.left() - DROPDOWN_GAP, rect.center().y),
            Align2::RIGHT_CENTER,
            text,
            figure,
            p.text_dim,
        );
    }
    painter.text(
        pos2(
            rect.left() + dropdown_lead(trigger.icon.is_some()),
            rect.center().y,
        ),
        Align2::LEFT_CENTER,
        elide(painter, trigger.label, text::TINY, width - furniture),
        font,
        ink,
    );

    egui::Popup::menu(&response)
        .width(rect.width())
        .show(|ui| {
            ui.set_min_width(ui.available_width());
            egui::ScrollArea::vertical()
                .max_height(metrics::DROPDOWN_MENU)
                .show(ui, |ui| {
                    // A trigger is often narrower than its own longest entry,
                    // and wrapping there folds every label in the list rather
                    // than widening the menu once.
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    menu(ui)
                })
                .inner
        })
        .map(|popup| popup.inner)
}

/// Map a value onto `0..=1` along a slider, linearly or logarithmically.
///
/// A logarithmic map is what makes a 1–400 px brush size usable: half the
/// travel covers 1–20 px, where the useful sizes actually live.
fn to_t(v: f32, lo: f32, hi: f32, log: bool) -> f32 {
    let v = v.clamp(lo, hi);
    if log {
        (v.ln() - lo.ln()) / (hi.ln() - lo.ln())
    } else {
        (v - lo) / (hi - lo)
    }
}

fn from_t(t: f32, lo: f32, hi: f32, log: bool) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if log {
        (lo.ln() + t * (hi.ln() - lo.ln())).exp()
    } else {
        lo + t * (hi - lo)
    }
}

/// Drag a track and report whether the value moved.
///
/// `snap` is the multiple a drag is pulled onto, in the value's own units, and
/// zero — what every rail but [`number_row`]'s passes — is the exact identity:
/// [`snapped`] returns its argument untouched, so nothing that did not ask for
/// a snap pays a rounding step for one.
fn drag_track(
    response: &Response,
    track: Rect,
    value: &mut f32,
    lo: f32,
    hi: f32,
    log: bool,
    snap: f32,
) -> bool {
    if !(response.dragged() || response.clicked()) {
        return false;
    }
    let Some(pos) = response.interact_pointer_pos() else {
        return false;
    };
    let t = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
    let next = snapped(from_t(t, lo, hi, log), snap, lo, hi);
    if next == *value {
        return false;
    }
    *value = next;
    true
}

fn paint_track(painter: &egui::Painter, p: &Palette, track: Rect, t: f32, knob: f32) {
    let radius = track.height() * 0.5;
    painter.rect_filled(track, radius, p.rail);
    if t > 0.0 {
        let filled = Rect::from_min_size(track.min, vec2(track.width() * t, track.height()));
        painter.rect_filled(filled, radius, p.accent);
    }
    if knob > 0.0 {
        painter.circle_filled(
            pos2(track.left() + track.width() * t, track.center().y),
            knob * 0.5,
            p.knob,
        );
    }
}

/// A canvas scrollbar: a thumb on a track, along one edge of the document
/// region.
///
/// Returns how far the camera should move along this axis, in document units,
/// or `None` if the bar was not dragged. The geometry is
/// [`umber_core::ScrollSpan`]'s — this only paints it and turns a drag into a
/// fraction of the bar.
///
/// Unlike the sliders above there is no track fill and no round knob: a
/// scrollbar's thumb *is* the value, and an accent-coloured bar down the side of
/// the canvas would read as something selected rather than as somewhere to be.
pub fn canvas_scrollbar(
    ui: &mut Ui,
    p: &Palette,
    rect: Rect,
    span: ScrollSpan,
    vertical: bool,
) -> Option<f32> {
    let response = ui.interact(
        rect,
        ui.id().with(("canvas-scroll", vertical)),
        Sense::click_and_drag(),
    );

    let length = if vertical {
        rect.height()
    } else {
        rect.width()
    };
    if length <= MIN_TRACK {
        return None;
    }

    let (start, thumb) = span.thumb();
    let thumb_rect = if vertical {
        Rect::from_min_size(
            pos2(rect.left(), rect.top() + length * start),
            vec2(rect.width(), length * thumb),
        )
    } else {
        Rect::from_min_size(
            pos2(rect.left() + length * start, rect.top()),
            vec2(length * thumb, rect.height()),
        )
    };

    // The track is left unpainted. The canvas is behind it and the document may
    // be too, and a filled strip along two edges of the picture is exactly the
    // furniture a paint application is trying not to put there. Only the thumb
    // is drawn, and only while it is needed.
    let ink = if response.dragged() {
        p.text_muted
    } else if response.hovered() {
        p.knob
    } else {
        p.rail
    };
    let inset = thumb_rect.shrink(2.0);
    ui.painter()
        .rect_filled(inset, inset.width().min(inset.height()) * 0.5, ink);

    if response.dragged() {
        let moved = response.drag_delta();
        let along = if vertical { moved.y } else { moved.x };
        // A fraction of the bar, so the thumb keeps up with the pointer exactly.
        return Some(span.pan_by(along / length));
    }
    None
}

/// Compact label + rail + readout, laid out horizontally for the options strip.
pub fn inline_slider(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    log: bool,
    display: impl Fn(f32) -> String,
) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let log = log && lo > 0.0 && hi > lo;

    ui.label(
        egui::RichText::new(label)
            .size(text::SMALL)
            .color(p.text_dim),
    );

    let (row, response) = ui.allocate_exact_size(vec2(90.0, 16.0), Sense::click_and_drag());
    let track = Rect::from_center_size(row.center(), vec2(row.width() - 10.0, 3.0));
    let changed = drag_track(&response, track, value, lo, hi, log, 0.0);
    paint_track(ui.painter(), p, track, to_t(*value, lo, hi, log), 10.0);

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(display(*value))
            .monospace()
            .size(text::TINY)
            .color(p.text),
    );

    changed
}

/// A rail with no label or readout, for rows that supply their own.
pub fn bare_slider(ui: &mut Ui, p: &Palette, value: &mut f32, range: RangeInclusive<f32>) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let width = (ui.available_width() - 30.0).max(MIN_TRACK * 3.0);
    let (row, response) = ui.allocate_exact_size(vec2(width, 14.0), Sense::click_and_drag());
    let track = Rect::from_center_size(row.center(), vec2(row.width(), 3.0));
    let changed = drag_track(&response, track, value, lo, hi, false, 0.0);
    paint_track(ui.painter(), p, track, to_t(*value, lo, hi, false), 0.0);
    changed
}

// ---------------------------------------------------------------------------
// A figure you can drag onto, or type
// ---------------------------------------------------------------------------

/// Fraction of a snap step within which a drag is pulled onto it.
///
/// An eighth of the step either side, so a quarter of the rail's travel lands
/// on a multiple and three quarters of it is free. A half would leave no free
/// travel at all — every value on the rail would be a snapped one, and the
/// control would read as a segmented picker wearing a slider's clothes. A
/// twentieth is too small to land on with a hand on a trackpad, which is the
/// case the snap exists for.
const SNAP_PULL: f32 = 0.125;

/// The nearest multiple of `step`, where the value came close enough to it.
///
/// A pure function of four numbers so the feel of the thing can be pinned
/// without a window: how wide the pull is, and that a multiple outside the
/// control's own range is not one to land on.
///
/// `step` of zero — every rail but [`number_row`]'s — returns the value bit for
/// bit, so nothing that did not ask for a snap is rounded by one.
fn snapped(value: f32, step: f32, lo: f32, hi: f32) -> f32 {
    // Spelled out rather than as `!(step > 0.0)`: a NaN step has to fall out
    // here too, and a negated comparison on a partially ordered type is the
    // one place that is easy to write and hard to read.
    if !step.is_finite() || step <= 0.0 || !value.is_finite() {
        return value;
    }
    let nearest = (value / step).round() * step;
    // A multiple past the end of the range is not somewhere the control can
    // go, and clamping it back would put the value at the end rather than
    // leaving the drag where the hand put it.
    if (nearest - value).abs() <= step * SNAP_PULL && nearest >= lo && nearest <= hi {
        nearest
    } else {
        value
    }
}

/// How a [`number_row`] reads its figure, writes it back, and lands on a
/// multiple.
///
/// A struct of plain fields rather than seven positional arguments or a
/// builder, for the reason [`BrushRow`] is one — and for one more here: every
/// field is read by `number_row` itself, so the shipped set cannot drift out of
/// use as call sites come and go. A builder's unused method is dead code the
/// day before the call site that wanted it lands.
///
/// Every number is in the **value's own units** and none of them is in the
/// readout's: `snap: 45.0` for an angle in degrees, `snap: 0.25` for a scale
/// shown as 25%. One set of units through the whole struct is what stops a call
/// site being right about its range and wrong about its step.
pub struct NumberRow<'a> {
    pub label: &'a str,
    pub range: RangeInclusive<f32>,
    /// The multiple a drag lands on. Zero for a rail that snaps to nothing.
    ///
    /// A *typed* figure is never snapped — that is the whole reason the field
    /// is there — and Alt held during a drag gives the free travel back.
    pub snap: f32,
    /// How many of the readout's units one of the value's is: 1.0 where the
    /// readout is in the value's own units, 100.0 where a fraction around 1 is
    /// shown and typed as a percentage.
    pub per_unit: f32,
    /// What follows the figure in the readout — and only there. A field being
    /// typed into starts from the bare number; see [`NumberRow::bare`].
    pub suffix: &'a str,
    /// Places after the point, in the readout and in what a field starts from.
    pub decimals: usize,
    /// Hand the value back only when the drag ends, for the one case where the
    /// rail is drawn *inside* the thing it changes: the interface scale.
    /// Applying that per frame rescales the dialog under the pointer, which
    /// moves the track, which changes the value the pointer is now over — so
    /// the knob runs away from the cursor and the setting is impossible to land
    /// on. Every other rail in Umber changes something it is not part of, and
    /// those want the immediate answer; this is not a better default, it is a
    /// different situation.
    ///
    /// The in-progress figure lives in egui's temporary store rather than in
    /// the caller. It has to: the caller reads its copy back out of the thing
    /// being set, which by construction has not been set yet, so a caller-held
    /// value would snap back to the old scale on the very next frame.
    ///
    /// A typed figure is applied at once either way — the pointer is nowhere
    /// near the track, so there is nothing to run away from.
    pub deferred: bool,
}

impl NumberRow<'_> {
    /// The readout: the figure in the units it is shown in, and its suffix.
    pub fn format(&self, value: f32) -> String {
        format!("{}{}", self.bare(value), self.suffix)
    }

    /// The same figure with nothing after it.
    ///
    /// What a field starts from, so the suffix is never something to delete
    /// before typing and never something to retype after.
    pub fn bare(&self, value: f32) -> String {
        let shown = value * self.per_unit;
        let decimals = self.decimals;
        format!("{shown:.decimals$}")
    }

    /// What a typed line means, or `None` where it means nothing — in which
    /// case the value is left exactly as it was.
    ///
    /// The exact inverse of [`NumberRow::bare`] by construction rather than by
    /// agreement: one scale and one suffix serve both directions, so a call
    /// site cannot hand this a parser that disagrees with its own formatter.
    /// That is the same argument `docimport::srgb`'s pair is held to, on a
    /// much smaller thing.
    ///
    /// The suffix is accepted and not required. Somebody who selects the whole
    /// field and types "90" means ninety degrees, and somebody who pastes
    /// "90°" back in means the same.
    pub fn parse(&self, text: &str) -> Option<f32> {
        let text = text.trim();
        let text = text.strip_suffix(self.suffix).unwrap_or(text).trim();
        let typed: f32 = text.parse().ok()?;
        if !typed.is_finite() {
            return None;
        }
        Some(typed / self.per_unit)
    }
}

/// [`slider_row`] with a figure that can be typed, and a rail that lands on
/// multiples.
///
/// Returns true when the value changed. The rail is exactly the one every other
/// row draws — same track, same knob, same drag — and the two things added sit
/// either side of it:
///
/// - **The readout is a field.** Dragging a rail to exactly 90° is a matter of
///   luck at any panel width; typing it is not. A typed figure is taken
///   verbatim, clamped to the range and snapped to nothing.
/// - **A drag lands on each multiple of [`NumberRow::snap`]**, within
///   [`SNAP_PULL`] of one. Sweeping through still feels continuous because
///   three quarters of the travel is free, and **Alt** held gives back the
///   last quarter for the case where the exact figure wanted is 43°.
///
/// Called from the colour picker's wheel (an angle, 45° apart) and from
/// Settings' Interface scale (a percentage, 25% apart, and
/// [`NumberRow::deferred`] because that one is drawn inside the thing it
/// scales). It is deliberately linear: a logarithmic rail with a snap would
/// have a pull that changed width as it travelled, which is a control that
/// feels broken rather than one that feels helpful.
///
/// **Key dispatch is suspended while the field has focus, and not from here.**
/// `ui::draw` asks `Context::text_edit_focused` once for the whole interface
/// and pulls `shortcuts::set_typing`, which is what stops the digits of a typed
/// angle also selecting the brush and then the eraser. A widget reaching for
/// that lever itself would be exactly the per-module version that rule exists
/// to replace — and it must not reach for `shortcuts::set_capturing`, the other
/// flag, which belongs to the shortcut recorder: a second writer would hand
/// dispatch back to the canvas while a chord was still being listened for.
pub fn number_row(ui: &mut Ui, p: &Palette, value: &mut f32, row: NumberRow<'_>) -> bool {
    let id = ui.id().with(("number-row", row.label));
    let held_id = id.with("held");
    let (lo, hi) = (*row.range.start(), *row.range.end());

    // What the rail is showing. That is the value itself, except part-way
    // through a deferred drag, when it is the figure the pointer is over and
    // the caller has not been told about yet.
    let mut shown = ui
        .ctx()
        .data(|d| d.get_temp::<f32>(held_id))
        .filter(|_| row.deferred)
        .unwrap_or(*value);

    let mut typed = None;
    let rail = ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;

        // A panel squeezed to its minimum can leave nothing here at all, and a
        // negative width makes a `Rect` whose max is left of its min.
        let width = ui.available_width().max(MIN_TRACK);

        // The header is a rail's height rather than [`slider_row`]'s line of
        // text: a caret needs somewhere to stand, and a field clipped to the
        // cap height of its own glyphs looks like a mistake.
        let (header, _) = ui.allocate_exact_size(vec2(width, metrics::SLIDER_ROW), Sense::hover());
        ui.painter().text(
            header.left_center(),
            Align2::LEFT_CENTER,
            row.label,
            FontId::proportional(text::SMALL),
            p.text_dim,
        );
        typed = number_field(ui, p, header, id, shown, &row);

        let (track_row, response) =
            ui.allocate_exact_size(vec2(width, metrics::SLIDER_ROW), Sense::click_and_drag());
        let track = Rect::from_center_size(
            track_row.center(),
            vec2(
                (track_row.width() - metrics::SLIDER_KNOB).max(MIN_TRACK),
                metrics::SLIDER_RAIL,
            ),
        );

        // Alt gives the snap back. The modifier is read here rather than off
        // the drag's start because a hand that finds itself two degrees off can
        // reach for it mid-sweep, which is when it is actually wanted.
        let free = ui.input(|i| i.modifiers.alt);
        drag_track(
            &response,
            track,
            &mut shown,
            lo,
            hi,
            false,
            if free { 0.0 } else { row.snap },
        );
        paint_track(
            ui.painter(),
            p,
            track,
            to_t(shown, lo, hi, false),
            metrics::SLIDER_KNOB,
        );

        // Built only while the pointer is actually over the rail: this is a
        // panel body, drawn every frame, and a `format!` for a tooltip nobody
        // is looking at is an allocation per frame for nothing.
        if row.snap > 0.0 && response.hovered() {
            return response.on_hover_text(format!(
                "Lands on each {}. Hold Alt for anything in between, or type the figure above.",
                row.format(row.snap)
            ));
        }
        response
    });

    // A typed figure ends any deferral with it: whatever the rail was holding
    // was abandoned the moment somebody said what they actually wanted.
    if let Some(figure) = typed {
        ui.ctx().data_mut(|d| d.remove::<f32>(held_id));
        let figure = figure.clamp(lo, hi);
        if figure != *value {
            *value = figure;
            return true;
        }
        return false;
    }

    // Still held, and the caller asked not to be told until it is let go.
    if row.deferred && rail.inner.is_pointer_button_down_on() {
        ui.ctx().data_mut(|d| d.insert_temp(held_id, shown));
        return false;
    }
    ui.ctx().data_mut(|d| d.remove::<f32>(held_id));
    if shown != *value {
        *value = shown;
        true
    } else {
        false
    }
}

/// The figure at the right of a [`number_row`]'s header, as a field.
///
/// Returns what was typed, on the one frame it is committed — Enter, or the
/// focus going elsewhere. Escape abandons it, which is what egui's own
/// `DragValue` does and therefore what a keyboard already expects here.
///
/// The text entry itself is egui's, for the reason `controls::search_field`
/// gives: caret, selection, IME and clipboard are not worth reimplementing to
/// change a border. Only the frame is ours — none at all, the readout's
/// monospace face and the palette's own ink — so a field nobody is typing in is
/// indistinguishable from the readout [`slider_row`] paints.
fn number_field(
    ui: &mut Ui,
    p: &Palette,
    header: Rect,
    id: egui::Id,
    value: f32,
    row: &NumberRow<'_>,
) -> Option<f32> {
    let edit_id = id.with("field");
    let buffer_id = id.with("typed");
    let font = FontId::monospace(text::TINY);

    // Sized from the widest figure the range can produce, never from the one
    // showing. A field that grew as a drag took the number from one digit to
    // three would creep leftwards under the very pointer aiming at it.
    let width = {
        let painter = ui.painter();
        let measure = |v: f32| {
            painter
                // A digit's worth of room past the readout, so a caret at the
                // end of the text has somewhere to be.
                .layout_no_wrap(format!("{}0", row.format(v)), font.clone(), p.text)
                .size()
                .x
        };
        measure(*row.range.start())
            .max(measure(*row.range.end()))
            .clamp(MIN_TRACK, header.width().max(MIN_TRACK))
    };
    let rect = Rect::from_min_max(pos2(header.right() - width, header.top()), header.max);

    // A figure that can be typed into has to look like one. Painted before the
    // field rather than from its response — a fill added afterwards would be
    // over the glyphs — and from `contains_pointer`, which is geometry alone
    // and cannot oscillate with what it reveals.
    let focused = ui.memory(|m| m.has_focus(edit_id));
    if focused || ui.rect_contains_pointer(rect) {
        ui.painter()
            .rect_filled(rect.expand2(vec2(4.0, 1.0)), metrics::RADIUS, p.control);
    }

    let held: Option<String> = ui.ctx().data(|d| d.get_temp(buffer_id));
    let editing = held.is_some();
    let mut text = held.unwrap_or_else(|| row.format(value));

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let edit = child.add(
        egui::TextEdit::singleline(&mut text)
            .id(edit_id)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .desired_width(rect.width())
            .horizontal_align(egui::Align::RIGHT)
            .clip_text(true)
            .font(font)
            .text_color(p.text),
    );

    if edit.gained_focus() {
        // Start from the bare figure, whole and selected: the first keystroke
        // then replaces it, which is what somebody who clicked a number and
        // typed "90" meant.
        text = row.bare(value);
        let mut state = egui::TextEdit::load_state(child.ctx(), edit_id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::default(),
                egui::text::CCursor::new(text.chars().count()),
            )));
        state.store(child.ctx(), edit_id);
    }

    if edit.has_focus() {
        child.ctx().data_mut(|d| d.insert_temp(buffer_id, text));
        return None;
    }
    if !editing {
        return None;
    }
    child.ctx().data_mut(|d| d.remove::<String>(buffer_id));
    if child.input(|i| i.key_pressed(egui::Key::Escape)) {
        return None;
    }
    row.parse(&text)
}

/// A read-only bordered pill showing a name and its value.
///
/// `tooltip` is not optional on purpose: the design draws these with a chevron,
/// as menus, and a pill that looks like a control has to say why it is not one
/// and where the real one is.
pub fn chip(ui: &mut Ui, p: &Palette, label: &str, value: &str, tooltip: &str) {
    let padding = 9.0;
    let font = FontId::proportional(text::SMALL);
    let text_w = ui
        .painter()
        .layout_no_wrap(format!("{label}  {value}"), font.clone(), p.text)
        .size()
        .x;
    let (rect, response) =
        ui.allocate_exact_size(vec2(text_w + padding * 2.0, 22.0), Sense::hover());

    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, p.window);
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.left_center() + vec2(padding, 0.0),
        Align2::LEFT_CENTER,
        label,
        font.clone(),
        p.text_dim,
    );
    painter.text(
        rect.right_center() - vec2(padding, 0.0),
        Align2::RIGHT_CENTER,
        value,
        font,
        p.text,
    );

    response.on_hover_text(tooltip);
}

/// What one brush row shows.
///
/// A struct rather than eight positional arguments: the library draws the same
/// row in two shapes — a compact one in the 264 px panel and a taller one in
/// the browser, where a second line carries the attribution — and at that width
/// a call site of bare booleans stops being readable.
pub struct BrushRow<'a> {
    pub name: &'a str,
    /// The line under the name: author and licence. Empty in the panel, which
    /// has no room for it and puts the credit in a tooltip instead.
    pub detail: &'a str,
    /// The brush itself, rather than the two numbers the sample used to be
    /// drawn from. The library is 239 presets deep and the sample is how you
    /// choose between them, so it has to show what actually separates them.
    pub brush: &'a Brush,
    /// The stamp this brush lays down, if it has one.
    ///
    /// Without it a stamp brush's row is a row of circles, which is the same
    /// flattery that made two hundred rows look like one row repeated when the
    /// sample was drawn from opacity and hardness alone. A stamp is
    /// unrecognisable from its numbers and unmistakable the moment it makes a
    /// mark.
    pub tip: Option<&'a Arc<TipMask>>,
    pub selected: bool,
    /// One the user saved, as opposed to one Umber ships. Marked with a dot
    /// rather than a word, because the panel is 264 px wide.
    pub user: bool,
    pub height: f32,
    /// Width kept clear at the right for controls the caller draws over the
    /// row — rename and delete in the browser. Reserved always, so a name does
    /// not reflow the moment the pointer arrives.
    pub trailing: f32,
    /// Whether this row can be picked up and carried to a collection.
    ///
    /// Only the browser's rows can: it is the one place the collections are on
    /// screen to be dropped on. Asked for rather than assumed, because the
    /// panel's rows are a shortlist with nowhere to drag to, and a row that
    /// senses a drag it can do nothing with is a row that swallows one.
    pub draggable: bool,
}

/// A brush preset: a stroke sample, then the name.
///
/// The sample is a real stroke, laid down by the real dab engine along
/// [`preview::stroke`]'s path — a hand that presses and lifts, a line that
/// curves, and a loop that turns the brush through every heading. So a chisel
/// reads as a chisel, a rake turns, a spray sprays and a blender carries what
/// it picked up. Drawing it from opacity and hardness alone made two hundred
/// rows look like one row repeated, which in a list this long is the difference
/// between choosing a brush and scrolling past it.
pub fn brush_row(ui: &mut Ui, p: &Palette, row: BrushRow<'_>) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), row.height),
        if row.draggable {
            Sense::click_and_drag()
        } else {
            Sense::click()
        },
    );

    // The library is 239 presets deep and both lists are scrolled, so most rows
    // on most frames are off screen. A sample is a few hundred stamps
    // rasterised the first time it is asked for — cached afterwards, but a row
    // nobody can see should not build one at all, and this early return is the
    // whole of how it does not.
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter();
    if row.selected {
        painter.rect_filled(rect, metrics::RADIUS, p.control_active);
    } else if response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control);
    }

    // The browser's rows are half as tall again as the panel's, and the sample
    // is now a stroke with a loop in it rather than a bar — so where there is
    // room, it takes it. The panel's 26 px row lands on 14 exactly as before.
    let sample_h = (row.height - 12.0).clamp(10.0, 24.0);
    let sample = Rect::from_center_size(
        pos2(rect.left() + 7.0 + 32.0, rect.center().y),
        vec2(64.0, sample_h),
    );
    brush_sample(painter, p, sample, row.brush, row.tip);

    // A dot marks the rows that are yours — the ones the browser will let you
    // rename and delete.
    let mut right = rect.right() - 7.0 - row.trailing;
    if row.user {
        painter.circle_filled(pos2(right - 3.0, rect.center().y), 3.0, p.accent);
        right -= 12.0;
    }

    let ink = if row.selected { p.text_strong } else { p.text };
    let left = sample.right() + 9.0;
    let width = (right - left).max(0.0);
    if row.detail.is_empty() {
        painter.text(
            pos2(left, rect.center().y),
            Align2::LEFT_CENTER,
            elide(painter, row.name, text::TINY, width),
            FontId::proportional(text::TINY),
            ink,
        );
    } else {
        painter.text(
            pos2(left, rect.center().y - 7.0),
            Align2::LEFT_CENTER,
            elide(painter, row.name, text::CONTROL, width),
            FontId::proportional(text::CONTROL),
            ink,
        );
        painter.text(
            pos2(left, rect.center().y + 8.0),
            Align2::LEFT_CENTER,
            elide(painter, row.detail, 9.5, width),
            FontId::proportional(9.5),
            p.text_dim,
        );
    }

    response
}

/// How many points of [`preview::stroke`] are fed to the dab engine.
///
/// A pointer reports a few hundred samples over a mark this long, and the
/// figure matters for more than smoothness: `Brush::stabilization` is an
/// exponential filter *per sample*, so a path fed too coarsely would round the
/// loop off a brush that would actually draw it.
const PATH_SAMPLES: usize = 96;

/// How long the preview stroke is taken to have taken.
///
/// Only a timed brush and the speed inputs read the clock, and both need a
/// plausible figure rather than none: an airbrush given no time at all deposits
/// a single dab, and a brush whose size follows speed would see a stroke at
/// rest.
const STROKE_SECONDS: f64 = 0.6;

/// What a preview's dab draws at, at full size and full pressure, as a fraction
/// of the sample's height.
///
/// The straight-line sample used a half, which filled the row — and that is
/// exactly the room the loop needs. Measured against a solid round brush at
/// full pressure, which is the worst case: at a half the turn is a solid blot
/// and the whole row is ink, at a fifth the loop is still closing up at the
/// panel's 14 px sample, and below about an eighth the mark stops reading as
/// paint at all. The mark is therefore thinner than it was, deliberately: a
/// narrow stroke that shows a shape says more than a bar that shows none.
const MARK_RADIUS: f32 = 0.18;

/// Ceiling on how many stamps one preview rasterises.
///
/// A finely spaced brush over the six-odd diameters the path spans is a few
/// hundred. The cap is for the pathological preset rather than the ordinary
/// one, because this runs while a slider in the brush editor is being dragged.
const MAX_PREVIEW_DABS: usize = 4096;

/// Where a preview's stroke sits inside its own buffer, in buffer pixels.
#[derive(Clone, Copy, Debug)]
struct MarkBox {
    width: usize,
    height: usize,
    /// Top-left of the box the unit path is fitted into.
    origin: Vec2,
    /// And its size.
    span: Vec2,
    /// What a dab draws at, at full size and full pressure.
    radius: f32,
    /// The sample's own height, in points, which is what tells the panel's list
    /// from the browser's — and is therefore what [`preview_texture`] keys on
    /// beside the brush. Held in points rather than buffer pixels so the set of
    /// values is the handful of row shapes there are, rather than one per
    /// interface scale anybody has ever dragged through.
    shape: u32,
}

/// A little more than the sample's own rectangle: scatter and jitter throw
/// stamps off the line, and a spray squashed back onto its axis is a spray that
/// looks like a line. This used to be a clip on the painter and is now the
/// buffer's own extent — a stamp outside it is simply not written.
fn sample_field(sample: Rect) -> Rect {
    sample.expand2(vec2(2.0, 3.0))
}

impl MarkBox {
    /// The geometry of one row's preview: a buffer covering [`sample_field`],
    /// with the path fitted inside `sample` itself.
    ///
    /// One statement of it, so the tests below drive the same arithmetic the
    /// rows do.
    fn of(sample: Rect, ppp: f32) -> Self {
        let field = sample_field(sample);
        let radius = (sample.height() * MARK_RADIUS * ppp).max(0.75);
        // Room for a full-width mark at either end of the path. Less of it
        // vertically than horizontally: the loop wants the height, and the
        // pixel or two the widest dab then overhangs by is inside the buffer's
        // own margin rather than outside the picture.
        let inset = vec2(radius, radius * 0.65);
        Self {
            width: ((field.width() * ppp).round() as usize).clamp(8, 512),
            height: ((field.height() * ppp).round() as usize).clamp(8, 256),
            origin: vec2(sample.left() - field.left(), sample.top() - field.top()) * ppp + inset,
            span: (vec2(sample.width(), sample.height()) * ppp - inset * 2.0).max(Vec2::splat(1.0)),
            radius,
            shape: sample.height().round().clamp(0.0, 255.0) as u32,
        }
    }

    /// Document pixels to buffer pixels.
    ///
    /// The brush is drawn at whatever scale puts a full-pressure dab at
    /// [`MarkBox::radius`], so a row shows the brush's *response* to pressure
    /// rather than its absolute size — a 400 px wash and a 4 px pen both fill
    /// their row, which is the only way a list of 201 of them is readable.
    fn scale(&self, brush: &Brush) -> f32 {
        self.radius / (brush.size * 0.5).max(0.5)
    }
}

/// The stamps one preview lays down, from the engine that lays down real ones.
///
/// [`StrokeBuilder`] already turns a path and a pressure into dabs with every
/// dynamic applied — size, opacity, hardness, spacing, scatter, angle, the
/// modulation table, the speed filters and the direction the stroke is
/// travelling in. None of that is restated here, which is the whole point: a
/// second dab loop beside it would be a second thing to keep in step, and the
/// row would slowly stop showing what the brush does.
fn preview_dabs(brush: &Brush, at: &MarkBox) -> Vec<Dab> {
    let scale = at.scale(brush);
    // The path in **document** pixels: the box the preview will occupy, taken
    // back through the scale. Everything the engine then decides — spacing,
    // scatter, the speed filters — is in the brush's own units, and the map
    // back to the buffer is one uniform factor that distorts no dab.
    let span = glam::Vec2::new(at.span.x / scale, at.span.y / scale);

    // A fresh builder per row is what makes the seeding identical: it seeds its
    // RNG from the number of strokes it has begun, so the first stroke of a new
    // builder always scatters the same way. Two rows differ because their
    // settings differ, and the list does not shimmer as it scrolls.
    let mut builder = StrokeBuilder::new();
    for (i, point) in preview::stroke(PATH_SAMPLES).iter().enumerate() {
        let sample = InputPoint::new(
            glam::Vec2::new(point.pos.x * span.x, point.pos.y * span.y),
            point.pressure,
            i as f64 / (PATH_SAMPLES - 1) as f64 * STROKE_SECONDS,
        );
        if i == 0 {
            // White, and the deposited colour is then ignored: the row's ink is
            // a palette token, so a hue jitter shown here would be a second
            // colour in the interface rather than a brush setting.
            builder.begin(*brush, [1.0; 3], sample);
        } else {
            builder.extend(sample);
        }
    }
    builder.drain_pending().take(MAX_PREVIEW_DABS).collect()
}

/// A tip's coverage, box-averaged down to something a preview can sample.
struct TipThumb {
    width: usize,
    height: usize,
    coverage: Vec<f32>,
    /// The mask's proportions with its longer side at 1 — [`TipMask::aspect`],
    /// which is exactly what the dab pass hands its vertex shader.
    aspect: (f32, f32),
}

impl TipThumb {
    /// Through [`tip_image`] rather than beside it: a second box-average would
    /// be a second thing to get right, and that one is already what the
    /// library's stamp thumbnails are drawn from. Point-sampling the full mask
    /// instead is what makes a sparse spatter tip an empty square half the
    /// time.
    fn new(mask: &TipMask) -> Self {
        let image = tip_image(mask, Color32::WHITE, ROW_TIP_TEXELS);
        Self {
            width: image.width(),
            height: image.height(),
            coverage: image
                .pixels
                .iter()
                .map(|texel| f32::from(texel.a()) / 255.0)
                .collect(),
            aspect: mask.aspect(),
        }
    }

    /// Bilinear coverage at `(u, v)`, and nothing at all outside the mask's own
    /// square: the dab pass's sampler clamps, but its quad never reaches past
    /// the mask either.
    fn at(&self, u: f32, v: f32) -> f32 {
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return 0.0;
        }
        let x = (u * self.width as f32 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let y = (v * self.height as f32 - 0.5).clamp(0.0, (self.height - 1) as f32);
        let (fx, fy) = (x.fract(), y.fract());
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let at = |x: usize, y: usize| self.coverage[y * self.width + x];
        let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
        let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
        top + (bottom - top) * fy
    }
}

/// One preview's rasterised stroke.
struct Mark {
    /// Coverage in `0..=1` per pixel, **before** the stroke's opacity.
    coverage: Vec<f32>,
    /// How much of the picked-up colour the paint at that pixel is carrying.
    carried: Vec<f32>,
}

/// `dab.wgsl`'s falloff, on the CPU.
fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Stamp a brush's whole preview stroke into a coverage buffer.
///
/// **This is a deliberate second implementation of the coverage rules, for a
/// thumbnail, and it holds to all three of them.** Dabs saturate under a `max`
/// — or accumulate, where [`Brush::build_up`] asks them to, which is a choice
/// of accumulation and not a different shape. `Brush::opacity` is applied
/// exactly once, afterwards, in [`preview_image`], and never folded into a
/// dab's coverage. And the falloff, the antialiasing margin sized from the
/// *short* axis, and the tip's proportions are `dab.wgsl`'s.
///
/// The alternative — one translucent egui shape per dab, which is what the row
/// used to draw — is the compounding bug the wet-layer scheme exists to
/// prevent: overlaps darken, so a 30% brush previews at 90% and a stroke that
/// crosses itself previews darker where it crosses. A loop crosses itself by
/// construction, so the old approximation stopped being survivable the moment
/// the path gained one. A preview that lies about opacity is worse than a plain
/// line, because opacity is one of the numbers people choose a brush by.
///
/// The other alternative — a real GPU pass per row — is not the answer either.
/// The library is 201 presets deep and both lists scroll, so it would be a
/// pass, a target and a readback per visible row per change, from inside a
/// scroll area, to draw a picture 70 pixels wide. This is a few hundred stamps
/// of arithmetic, once per brush, and the result is cached.
fn preview_mark(brush: &Brush, tip: Option<&TipThumb>, at: &MarkBox) -> Mark {
    let mut mark = Mark {
        coverage: vec![0.0; at.width * at.height],
        carried: vec![0.0; at.width * at.height],
    };
    let dabs = preview_dabs(brush, at);
    let scale = at.scale(brush);
    let (tsx, tsy) = tip.map_or((1.0, 1.0), |thumb| thumb.aspect);
    // A blender's mark carries the colour it found rather than the palette's,
    // and the row has to show that or every Blenders entry looks like a paint.
    // There is no canvas here to pick anything up from, so the decay is stated
    // rather than sampled: a short smear loses what it carries within a stamp
    // or two, a long one keeps it the whole way.
    let smudge = brush.smudge.clamp(0.0, 1.0);
    let keep = brush.smudge_length.clamp(0.0, 0.99);
    let last = dabs.len().saturating_sub(1).max(1) as f32;

    for (i, dab) in dabs.iter().enumerate() {
        let centre = at.origin + vec2(dab.pos[0], dab.pos[1]) * scale;
        let radius = (dab.radius * scale).max(0.4);
        let short = radius / dab.aspect.max(1.0);
        // The quad the dab pass builds: semi-axes carrying the tip's own
        // proportions, so a portrait stamp stays portrait, then rotated.
        let (long_axis, short_axis) = (radius * tsx, short * tsy);
        let (sin, cos) = dab.angle.sin_cos();
        // Its axis-aligned box, plus a pixel for the antialiased edge — the
        // same bound `StrokeBuilder::bounds` takes, and too tight for the same
        // reason: a rotated quad reaches out to its corners.
        let reach = vec2(
            (long_axis * cos).abs() + (short_axis * sin).abs() + 1.0,
            (long_axis * sin).abs() + (short_axis * cos).abs() + 1.0,
        );
        let x0 = (centre.x - reach.x).floor().max(0.0) as usize;
        let y0 = (centre.y - reach.y).floor().max(0.0) as usize;
        let x1 = ((centre.x + reach.x).ceil().max(0.0) as usize).min(at.width);
        let y1 = ((centre.y + reach.y).ceil().max(0.0) as usize).min(at.height);

        // At least one pixel of falloff whatever the hardness, sized from the
        // *short* axis because that is the demanding one: a chisel two pixels
        // across needs the softening a two-pixel round brush does.
        let aa = (1.0 / long_axis.min(short_axis).max(1.0)).clamp(0.001, 0.5);
        let inner = dab.hardness.clamp(0.0, 1.0 - aa);
        // `0f32.powf(0.0)` is 1, so the head of the stroke is always fully
        // loaded — which is what a blender does.
        let held = smudge * keep.powf(i as f32 / last * 6.0);

        for y in y0..y1 {
            for x in x0..x1 {
                let d = vec2(x as f32 + 0.5, y as f32 + 0.5) - centre;
                // Into the dab's own frame, where the ellipse is a unit circle
                // and the mask fills the square — which is what keeps this
                // identical to the shader for every shape.
                let local = vec2(
                    (d.x * cos + d.y * sin) / long_axis,
                    (d.y * cos - d.x * sin) / short_axis,
                );
                let shape = match tip {
                    Some(thumb) => thumb.at(local.x * 0.5 + 0.5, local.y * 0.5 + 0.5),
                    None => 1.0 - smoothstep(inner, 1.0, local.length()),
                };
                let cov = shape * dab.coverage;
                if cov <= 0.0 {
                    continue;
                }
                let at = y * at.width + x;
                if brush.build_up {
                    mark.coverage[at] += cov * (1.0 - mark.coverage[at]);
                } else {
                    mark.coverage[at] = mark.coverage[at].max(cov);
                }
                // Colour blends `over` while coverage does not, exactly as the
                // second scratch target does along a real smudging stroke: the
                // smear trails along the stroke instead of averaging everything
                // the brush has been over. Guarded, so a brush that carries
                // nothing pays nothing.
                if held > 0.0 {
                    mark.carried[at] = held * cov + mark.carried[at] * (1.0 - cov);
                }
            }
        }
    }
    mark
}

/// The mark, inked and with the stroke's opacity applied — once, here.
fn preview_image(mark: &Mark, brush: &Brush, at: &MarkBox, inks: [Color32; 2]) -> egui::ColorImage {
    let pixels = mark
        .coverage
        .iter()
        .zip(&mark.carried)
        .map(|(coverage, carried)| {
            let alpha = (coverage * brush.opacity).clamp(0.0, 1.0);
            let ink = mix(inks[0], inks[1], *carried);
            Color32::from_rgba_unmultiplied(
                ink.r(),
                ink.g(),
                ink.b(),
                (alpha * 255.0).round() as u8,
            )
        })
        .collect();
    egui::ColorImage::new([at.width, at.height], pixels)
}

/// One row's stamped stroke, cached against the brush it was stamped from.
///
/// A few hundred stamps is cheap once and not cheap 201 times a frame, and the
/// picture only changes when the brush does. Keyed by the preset's own address
/// **and the shape of the row it is drawn in** — and what is *compared* is
/// everything else the picture depends on: the brush by value, so a slider
/// moved in the editor redraws on the next frame; the tip by `Arc` identity, so
/// two brushes cut from one stamp share the downsample rather than comparing a
/// megabyte of coverage; both inks, so switching theme does not leave every row
/// the old colour; and the buffer's size, which every other figure in
/// [`MarkBox`] is a fixed fraction of.
///
/// The row shape has to be in the *key* rather than only in the comparison, and
/// that was a crash rather than a nicety. The Brushes panel and the library
/// browser draw the same presets at 14 and 24 points, and the browser is a modal
/// over the panel — so both are in the same pass. Sharing one entry, the second
/// row to draw replaced the first's, dropping the last `TextureHandle` to a
/// texture the first had already queued a `Shape` against; egui then reported
/// the free in that very pass's delta, and destroying a texture a recorded draw
/// still names fails validation at submit. `app::submit_frame` is what makes
/// such a free survivable at all — this is what stops the library producing one
/// every frame, along with the two rasterisations and two uploads per visible
/// preset per frame that came with it.
fn preview_texture(
    ctx: &egui::Context,
    brush: &Brush,
    tip: Option<&Arc<TipMask>>,
    inks: [Color32; 2],
    at: &MarkBox,
) -> egui::TextureHandle {
    type Held = (
        Brush,
        Option<Arc<TipMask>>,
        [Color32; 2],
        [usize; 2],
        egui::TextureHandle,
    );
    let same_tip = |held: &Option<Arc<TipMask>>| match (held, tip) {
        (None, None) => true,
        (Some(held), Some(mask)) => Arc::ptr_eq(held, mask),
        _ => false,
    };

    let id = egui::Id::new((
        "brush-preview",
        std::ptr::from_ref(brush) as usize,
        at.shape,
    ));
    let cached: Option<Held> = ctx.data(|d| d.get_temp(id));
    if let Some((held, held_tip, held_inks, size, texture)) = cached
        && held == *brush
        && same_tip(&held_tip)
        && held_inks == inks
        && size == [at.width, at.height]
    {
        return texture;
    }

    let thumb = tip.map(|mask| TipThumb::new(mask));
    let mark = preview_mark(brush, thumb.as_ref(), at);
    let texture = ctx.load_texture(
        "brush-preview",
        preview_image(&mark, brush, at, inks),
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|d| {
        d.insert_temp(
            id,
            (
                *brush,
                tip.cloned(),
                inks,
                [at.width, at.height],
                texture.clone(),
            ),
        );
    });
    texture
}

/// Stamp one brush's mark into `sample`.
///
/// Not a miniature of the real dab loop any more — it *is* the real dab loop,
/// over [`preview::stroke`]'s path. The hand presses and lifts, the line curves
/// and crosses itself, and every dab is the one [`StrokeBuilder`] would emit,
/// so a rake turns through the loop, a chisel goes thick and thin round it, a
/// spray sprays and a pressure-tapered brush tapers.
fn brush_sample(
    painter: &egui::Painter,
    p: &Palette,
    sample: Rect,
    brush: &Brush,
    tip: Option<&Arc<TipMask>>,
) {
    let at = MarkBox::of(sample, painter.ctx().pixels_per_point().clamp(0.5, 4.0));
    // The "found" colour is the dim ink: it reads as canvas rather than as a
    // second palette colour, and it needs no token of its own.
    let texture = preview_texture(painter.ctx(), brush, tip, [p.text_strong, p.text_dim], &at);
    painter.image(
        texture.id(),
        sample_field(sample),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// Widest a mask is downsampled to for a library row.
///
/// The row's sample is 14 to 24 points tall and a stamp in it is a few pixels
/// across, so 32 texels is already more than the screen can show. It is also
/// what keeps the work small enough to do the first time a stamp row scrolls
/// into view.
const ROW_TIP_TEXELS: u32 = 32;

/// Box-average a coverage mask down to a thumbnail, tinted with an ink.
///
/// Coverage becomes *alpha* rather than luminance, so a stamp reads the same way
/// on either theme and an empty texel is the surface behind it rather than
/// black. Box-averaged rather than point-sampled: a sparse spatter tip shown by
/// nearest neighbour is an empty square about half the time.
pub fn tip_image(mask: &TipMask, ink: Color32, texels: u32) -> egui::ColorImage {
    let scale = (mask.width().max(mask.height()).div_ceil(texels)).max(1);
    let (w, h) = (
        mask.width().div_ceil(scale).max(1),
        mask.height().div_ceil(scale).max(1),
    );
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0u32;
            let mut n = 0u32;
            for sy in y * scale..((y + 1) * scale).min(mask.height()) {
                for sx in x * scale..((x + 1) * scale).min(mask.width()) {
                    sum += u32::from(mask.at(sx, sy));
                    n += 1;
                }
            }
            // `n` is zero only for a cell entirely past the mask's edge, which
            // the ceiling division above cannot produce — the guard is there so
            // an off-by-one here can never be a division by zero.
            let coverage = sum.checked_div(n).unwrap_or(0) as u8;
            pixels.push(Color32::from_rgba_unmultiplied(
                ink.r(),
                ink.g(),
                ink.b(),
                coverage,
            ));
        }
    }
    egui::ColorImage::new([w as usize, h as usize], pixels)
}

/// Linear blend of two opaque palette colours.
fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    Color32::from_rgb(
        lerp(from.r(), to.r()),
        lerp(from.g(), to.g()),
        lerp(from.b(), to.b()),
    )
}

/// Cut a run of text down to what fits, ending in an ellipsis.
///
/// An `egui::Label` would truncate for us, but these rows are painted rather
/// than laid out — that is what lets the whole row be one click target — so the
/// measuring has to happen here. Binary search rather than a linear walk keeps
/// it at a handful of layout calls for a name that does not fit, and none at
/// all for one that does.
pub fn elide(painter: &egui::Painter, s: &str, size: f32, width: f32) -> String {
    let font = FontId::proportional(size);
    let measure = |t: &str| {
        painter
            .layout_no_wrap(t.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width <= 0.0 {
        return String::new();
    }
    if measure(s) <= width {
        return s.to_owned();
    }
    // Character boundaries, so a multi-byte name is never cut mid-glyph.
    let cuts: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    let (mut lo, mut hi) = (0usize, cuts.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&format!("{}…", &s[..cuts[mid.min(cuts.len() - 1)]])) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    format!("{}…", &s[..cuts[lo.min(cuts.len() - 1)]])
}

pub struct LayerRowResponse {
    pub clicked: bool,
    pub eye_clicked: bool,
    /// The mask chip was clicked, which is how the edit target is switched from
    /// the list rather than from the toggle row.
    pub mask_clicked: bool,
    /// The tick box was clicked. Its own target, like the eye's: ticking a row
    /// for a bulk operation must not also select it, or every tick would move
    /// the brush.
    pub pick_clicked: bool,
}

/// What one row has to draw besides its name.
///
/// A struct rather than seven positional arguments, because a row of `bool`s at
/// a call site is exactly where "visible" and "locked" get transposed and
/// nothing complains.
#[derive(Clone, Copy)]
pub struct LayerRow<'a> {
    pub name: &'a str,
    /// Identifies the layer for the row's inner hit targets. The layer's
    /// *name* looks like the obvious key and is not one: names Umber generates
    /// are unique, but an imported ORA or PSD routinely carries two layers
    /// called the same thing, and two widgets sharing an id is an egui id clash
    /// — one of the two eyes then stops answering. A slot is unique by
    /// construction and never changes hands while a layer exists.
    pub slot: u32,
    pub visible: bool,
    pub active: bool,
    pub blend: &'a str,
    pub has_mask: bool,
    /// The mask, rather than the layer, is what a stroke would land in. Only
    /// ever true on the selected row — the edit target is per document.
    pub editing_mask: bool,
    pub clipped: bool,
    pub locked: bool,
    /// Which link group the layer is in, if any. The chain mark is drawn in
    /// that group's colour, which is the whole of how a row says *which* set
    /// of layers it travels with rather than only that it travels with some.
    pub link: Option<u8>,
    /// What is actually on the layer, scaled to fill the chip — or `None`,
    /// which draws the checker alone and means "nothing to show". The two
    /// reasons for `None` are deliberately drawn the same: a layer that is
    /// genuinely empty, and one whose picture has not come back from the GPU
    /// yet. Distinguishing them would mean a spinner on every row of a freshly
    /// opened document for the two frames it takes.
    pub thumb: Option<&'a egui::TextureHandle>,
    /// Ticked for a bulk operation. Drawn on every row whether or not anything
    /// is ticked: a box that only appeared once you had found the first one is
    /// a feature nobody finds.
    pub picked: bool,
}

/// One row of the layer stack: visibility, a thumbnail chip, name and blend.
///
/// The flags are **shown** here and changed from the panel's toggle row. Four
/// more hit targets per row would either grow the row — the list has to hold
/// eight layers on screen — or leave the name nothing to be drawn in. The one
/// exception is the mask chip, which is a target because clicking the thing you
/// want to paint is the whole gesture and there is nowhere else it could go.
pub fn layer_row(ui: &mut Ui, p: &Palette, row: LayerRow<'_>) -> LayerRowResponse {
    let LayerRow {
        name,
        slot,
        visible,
        active,
        blend,
        ..
    } = row;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());

    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, metrics::RADIUS, p.control_active);
        painter.rect_stroke(
            rect,
            metrics::RADIUS,
            Stroke::new(1.0, p.accent_dim),
            egui::StrokeKind::Inside,
        );
    } else if response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control);
    }

    // The tick box, and then the eye. Both are their own hit targets inside the
    // row, so neither also changes the selection: ticking four rows to hide
    // them would otherwise move the brush four times on the way.
    let pick = Rect::from_min_size(rect.left_top() + vec2(4.0, 6.0), vec2(18.0, 18.0));
    let pick_response = ui.interact(pick, ui.id().with(("pick", slot)), Sense::click());
    let box_rect = Rect::from_center_size(pick.center(), Vec2::splat(12.0));
    if row.picked {
        ui.painter().rect_filled(box_rect, 2.0, p.accent);
        icons::draw(ui.painter(), box_rect.shrink(1.0), Icon::Check, p.window);
    } else {
        ui.painter().rect_stroke(
            box_rect,
            2.0,
            Stroke::new(
                1.0,
                if pick_response.hovered() {
                    p.text_dim
                } else {
                    p.border
                },
            ),
            egui::StrokeKind::Inside,
        );
    }

    let eye = Rect::from_min_size(rect.left_top() + vec2(23.0, 6.0), vec2(18.0, 18.0));
    let eye_response = ui.interact(eye, ui.id().with(("eye", slot)), Sense::click());

    icons::draw(
        ui.painter(),
        eye,
        if visible { Icon::Eye } else { Icon::EyeOff },
        if visible { p.text } else { p.text_dim },
    );

    // The layer's own content, scaled to fill the chip, over a checker.
    //
    // The checker is drawn whether or not there is a picture, because the
    // picture carries alpha and a thumbnail of a sketch is mostly transparent —
    // laying it on a flat fill would say the layer was opaque. Where there is
    // no picture at all the checker is the whole chip, and that is the stated
    // "nothing on this layer" state; see [`LayerRow::thumb`].
    let thumb = Rect::from_min_size(rect.left_top() + vec2(45.0, 3.0), vec2(24.0, 24.0));
    painter.rect_filled(thumb, 3.0, p.window);
    for i in 0..4 {
        for j in 0..4 {
            if (i + j) % 2 == 0 {
                continue;
            }
            let cell = Rect::from_min_size(
                thumb.left_top() + vec2(i as f32 * 6.0, j as f32 * 6.0),
                vec2(6.0, 6.0),
            );
            painter.rect_filled(cell.intersect(thumb), 0.0, p.control_hover);
        }
    }
    if let Some(picture) = row.thumb {
        painter.image(
            picture.id(),
            thumb,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    // The mask chip, beside the layer's own, and only where there is a mask.
    // It is the switch as well as the indicator: clicking the thing you mean to
    // paint is the gesture every application uses for this, and the ring is
    // what says which of the two the brush is currently pointed at.
    let mut mask_clicked = false;
    let mut text_left = thumb.right() + 8.0;
    if row.has_mask {
        let chip = Rect::from_min_size(thumb.right_top() + vec2(4.0, 3.0), vec2(18.0, 18.0));
        let hit = ui.interact(chip, ui.id().with(("mask", slot)), Sense::click());
        mask_clicked = hit.clicked();
        ui.painter().rect_filled(chip, 3.0, p.window);
        icons::draw(
            ui.painter(),
            chip.shrink(2.0),
            Icon::Mask,
            if row.editing_mask {
                p.text_strong
            } else {
                p.text_dim
            },
        );
        // Drawn on whichever of the two the strokes are going into, so the pair
        // always says which is which rather than only saying "there is a mask".
        let aimed = if row.editing_mask { chip } else { thumb };
        ui.painter().rect_stroke(
            aimed.expand(1.0),
            3.0,
            Stroke::new(1.0, if active { p.accent } else { p.border }),
            egui::StrokeKind::Outside,
        );
        text_left = chip.right() + 6.0;
    }

    // The flags, as small marks between the name and the blend label. Painted
    // only when set: a row of four grey icons on every layer is noise, and the
    // list's job is to let a stack be read at a glance.
    let painter = ui.painter();
    let mut marks_left = rect.right() - 7.0;
    let blend_width = painter
        .layout_no_wrap(blend.to_owned(), FontId::proportional(9.0), p.text_dim)
        .size()
        .x;
    marks_left -= blend_width;
    for (on, icon, tint) in [
        (row.clipped, Icon::Clip, p.text_dim),
        (row.locked, Icon::Lock, p.text_dim),
        // The one mark here drawn in anything but the dim text colour, because
        // it is the one that has to be told apart from the identical mark on
        // another row. Never hard-coded: `Palette::link_colour` is the table.
        (
            row.link.is_some(),
            Icon::Chain,
            row.link.map_or(p.text_dim, |g| p.link_colour(g)),
        ),
    ] {
        if !on {
            continue;
        }
        marks_left -= 14.0;
        icons::draw(
            painter,
            Rect::from_min_size(pos2(marks_left, rect.center().y - 6.0), Vec2::splat(12.0)),
            icon,
            tint,
        );
    }

    painter.text(
        pos2(text_left, rect.center().y),
        Align2::LEFT_CENTER,
        // Cut to what is left after the marks and the blend label, so a long
        // name cannot run underneath either of them.
        elide(
            painter,
            name,
            text::SMALL,
            (marks_left - text_left - 6.0).max(0.0),
        ),
        FontId::proportional(text::SMALL),
        match (active, visible) {
            (true, _) => p.text_strong,
            (false, true) => p.text,
            (false, false) => p.text_dim,
        },
    );
    painter.text(
        rect.right_center() - vec2(7.0, 0.0),
        Align2::RIGHT_CENTER,
        blend,
        FontId::proportional(9.0),
        p.text_dim.gamma_multiply(0.8),
    );

    LayerRowResponse {
        clicked: response.clicked(),
        eye_clicked: eye_response.clicked(),
        mask_clicked,
        pick_clicked: pick_response.clicked(),
    }
}

/// A draggable response curve.
///
/// Handles move vertically only — their inputs are fixed and evenly spaced —
/// so the curve cannot be dragged into a shape that maps one pressure to two
/// values.
pub fn curve_editor(ui: &mut Ui, p: &Palette, curve: &mut ResponseCurve, size: f32) -> bool {
    let mut changed = false;
    let size = size.max(MIN_TRACK * 2.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());

    let at = |i: usize, v: f32| {
        pos2(
            rect.left() + rect.width() * ResponseCurve::x_of(i),
            rect.bottom() - rect.height() * v,
        )
    };

    // Drag whichever handle is nearest horizontally — with five fixed columns
    // that is unambiguous, and it means you never have to hit the dot exactly.
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let t = ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        let i = (t * (ResponseCurve::N - 1) as f32).round() as usize;
        let v = 1.0 - ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
        let before = curve.points[i.min(ResponseCurve::N - 1)];
        curve.set(i, v);
        changed = (curve.points[i.min(ResponseCurve::N - 1)] - before).abs() > 1e-5;
    }

    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, p.window);
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );

    // Quarter grid, plus the diagonal as a reference for "no change".
    for k in 1..4 {
        let f = k as f32 / 4.0;
        let x = rect.left() + rect.width() * f;
        let y = rect.top() + rect.height() * f;
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, p.border.gamma_multiply(0.6)),
        );
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, p.border.gamma_multiply(0.6)),
        );
    }
    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        Stroke::new(1.0, p.border),
    );

    let points: Vec<_> = (0..ResponseCurve::N)
        .map(|i| at(i, curve.points[i]))
        .collect();
    painter.add(egui::Shape::line(
        points.clone(),
        Stroke::new(2.0, p.accent),
    ));
    for point in points {
        painter.circle_filled(point, 4.0, p.knob);
    }

    changed
}

/// The design's tool-rail button: [`metrics::TOOL_BUTTON`] square.
///
/// Icons are painted rather than loaded: the design specifies them as a handful
/// of SVG primitives, and drawing those directly avoids shipping an image
/// atlas or a font just for four glyphs.
pub fn tool_button(ui: &mut Ui, p: &Palette, icon: Icon, active: bool, tooltip: &str) -> Response {
    let size = metrics::TOOL_BUTTON;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());

    let fill = if active {
        p.control_active
    } else if response.hovered() {
        p.control_hover
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter();
    if fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, metrics::RADIUS_LARGE, fill);
    }

    let colour = if active { p.accent } else { p.text_muted };
    icons::draw(painter, rect.shrink(7.0), icon, colour);

    response.on_hover_text(tooltip)
}

// ---------------------------------------------------------------------------
// Input diagnostics
// ---------------------------------------------------------------------------

/// A `0..=1` bar with the figure beside it.
///
/// `value` is an `Option` deliberately. A device that reported nothing has to
/// read as nothing: printing an absent reading as `0.00` is exactly the
/// ambiguity the pen fix exists to resolve — winit answers `None` both for a
/// mouse with no sensor and for a pen a hair off the glass — and a meter that
/// quietly bottomed out would put that ambiguity straight back on the page.
/// `absent` is what to say instead.
pub fn value_meter(ui: &mut Ui, p: &Palette, label: &str, value: Option<f32>, absent: &str) {
    ui.horizontal(|ui| {
        ui.scope(|ui| {
            ui.set_width(132.0);
            ui.label(
                egui::RichText::new(label)
                    .size(text::SMALL)
                    .color(p.text_muted),
            );
        });

        let width = (ui.available_width() - 62.0).max(MIN_TRACK * 4.0);
        let (row, _) = ui.allocate_exact_size(vec2(width, 14.0), Sense::hover());
        let track = Rect::from_center_size(row.center(), vec2(row.width(), 5.0));
        let painter = ui.painter();
        painter.rect_filled(track, 2.5, p.rail);
        if let Some(v) = value {
            let t = v.clamp(0.0, 1.0);
            if t > 0.0 {
                painter.rect_filled(
                    Rect::from_min_size(track.min, vec2(track.width() * t, track.height())),
                    2.5,
                    p.accent,
                );
            }
            // A genuine zero still gets a tick, or it is indistinguishable from
            // the empty rail an absent reading draws.
            painter.rect_filled(
                Rect::from_min_size(track.min, vec2(1.5, track.height())),
                0.0,
                p.text_dim,
            );
        }

        ui.add_space(6.0);
        let (body, colour) = match value {
            Some(v) => (format!("{v:.3}"), p.text),
            None => (absent.to_string(), p.text_dim),
        };
        ui.label(
            egui::RichText::new(body)
                .monospace()
                .size(text::TINY)
                .color(colour),
        );
    });
}

/// One column of [`pressure_graph`]: the pair of figures for a single event.
#[derive(Clone, Copy, Default)]
pub struct TracePoint {
    /// What the device reported, `None` where it reported nothing.
    pub reported: Option<f32>,
    /// What the pressure model made of it, `None` where nothing resolved it.
    pub resolved: Option<f32>,
}

/// A trace of the recent pointer samples: reported against resolved.
///
/// The most useful picture on the Input & pen page, because it answers what a
/// still readout cannot — does pressure actually fall to zero as the pen is
/// lifted, or does it jump back to full for the last few samples?
///
/// One column per sample rather than per unit of time. Even spacing is the
/// right choice here: the shape of the fall is what is being read, and a
/// time-scaled x would squash a fast lift — the very part worth seeing — into
/// nothing. It also means the trace stands still once the hand stops, so it can
/// be studied after the fact, and costs no repaint timer to keep true.
///
/// A missing figure is a **gap**, never a zero. The line is broken and picked
/// up on the far side, so an absent reading is visibly absent.
pub fn pressure_graph(
    ui: &mut Ui,
    p: &Palette,
    height: f32,
    columns: usize,
    points: impl Iterator<Item = TracePoint>,
) {
    let width = ui.available_width().max(MIN_TRACK * 8.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS_LARGE, p.window);
    painter.rect_stroke(
        rect,
        metrics::RADIUS_LARGE,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );

    let plot = rect.shrink(8.0);
    let hair = Stroke::new(1.0, p.border.gamma_multiply(0.7));
    for (t, caption) in [(0.0, "1.0"), (0.5, "0.5"), (1.0, "0")] {
        let y = plot.top() + plot.height() * t;
        painter.line_segment([pos2(plot.left() + 22.0, y), pos2(plot.right(), y)], hair);
        painter.text(
            pos2(plot.left() + 18.0, y),
            Align2::RIGHT_CENTER,
            caption,
            FontId::monospace(9.0),
            p.text_dim.gamma_multiply(0.8),
        );
    }

    let field = Rect::from_min_max(pos2(plot.left() + 24.0, plot.top()), plot.max);
    let span = (columns.max(2) - 1) as f32;
    let at = |i: usize, v: f32| {
        pos2(
            field.left() + field.width() * (i as f32 / span),
            field.bottom() - field.height() * v.clamp(0.0, 1.0),
        )
    };

    // Each figure becomes a list of runs, broken wherever it was absent.
    let mut reported: Vec<Vec<egui::Pos2>> = vec![Vec::new()];
    let mut resolved: Vec<Vec<egui::Pos2>> = vec![Vec::new()];
    let mut seen = 0usize;
    for (i, point) in points.enumerate() {
        seen = i + 1;
        for (value, runs) in [
            (point.reported, &mut reported),
            (point.resolved, &mut resolved),
        ] {
            match value {
                Some(v) => runs
                    .last_mut()
                    .expect("a run is always open")
                    .push(at(i, v)),
                // A new run rather than a join across the gap, so an absent
                // reading reads as absent instead of as a dive to zero.
                None => {
                    if !runs.last().is_some_and(Vec::is_empty) {
                        runs.push(Vec::new());
                    }
                }
            }
        }
    }

    if seen == 0 {
        painter.text(
            field.center(),
            Align2::CENTER_CENTER,
            "Nothing yet — move the pointer over the window.",
            FontId::proportional(text::TINY),
            p.text_dim,
        );
        return;
    }

    // Resolved underneath and thicker, so where the two agree — the ordinary
    // case on Device — the reported line rides on it and the trace reads as one
    // band rather than as two lines that keep crossing.
    for (runs, stroke) in [
        (&resolved, Stroke::new(3.0, p.text_muted)),
        (&reported, Stroke::new(1.5, p.accent)),
    ] {
        for run in runs {
            match run.len() {
                0 => {}
                // A lone sample between two gaps still has to appear.
                1 => {
                    painter.circle_filled(run[0], stroke.width * 0.6, stroke.color);
                }
                _ => {
                    painter.add(egui::Shape::line(run.clone(), stroke));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings' Interface scale, as the dialog itself states it — not a copy
    /// of its numbers. A range typed out again here would go on passing while
    /// the control it stands for was changed underneath it, which is the whole
    /// reason `scale_row` is a function at its call site rather than seven
    /// arguments at a call.
    use crate::settings::scale_row as scale;

    /// The colour wheel's Angle control. The one figure that is still stated
    /// twice: `colorpicker::angle_row` is private to its module, and widening
    /// it to be read here would be widening an interface for a test.
    fn angle() -> NumberRow<'static> {
        NumberRow {
            label: "Angle",
            range: 0.0..=359.0,
            snap: 45.0,
            per_unit: 1.0,
            suffix: "°",
            decimals: 0,
            deferred: false,
        }
    }

    /// A row must be sized by the space it is given, never the other way round.
    ///
    /// The settings dialog is one fixed size and everything in it is clipped to
    /// that, so a control reporting itself wider than its column is the trap
    /// the module docs there describe — a label in a horizontal layout
    /// extending rather than wrapping, and with it the pane and the window.
    /// This row puts a `TextEdit` where the readout used to be painted, which
    /// is the widget most likely to ask for room it was not offered, so the
    /// Interface scale's own column width is what it is driven at here.
    #[test]
    fn a_number_row_is_no_wider_than_the_column_it_is_drawn_in() {
        // The width `settings::general_pane` gives the control.
        const COLUMN: f32 = 320.0;
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 600.0))),
            ..Default::default()
        };

        // Twice: the first pass builds the font atlas and the field's stored
        // state, and a control that only misbehaves once it has been laid out
        // before would otherwise go unseen.
        for _ in 0..2 {
            let _ = ctx.run_ui(input.clone(), |ui| {
                let p = Palette::of(crate::theme::ThemeKind::Graphite);
                let scope = ui.scope(|ui| {
                    ui.set_max_width(COLUMN);
                    let mut value = 1.25;
                    number_row(ui, &p, &mut value, scale());
                });
                let width = scope.response.rect.width();
                assert!(
                    width <= COLUMN + 0.5,
                    "the row claimed {width} px of a {COLUMN} px column"
                );
            });
        }
    }

    /// A trigger that sized itself to its own label has room for that label.
    ///
    /// The two halves of the arithmetic are the same function, which is what
    /// makes this hold rather than nearly hold; the test is what stops somebody
    /// splitting them again. The failure it guards against is quiet: a picker a
    /// few pixels short elides the one long option and reads as a name somebody
    /// mistyped, and every other option looks fine.
    #[test]
    fn a_dropdown_sized_to_its_content_has_room_for_all_of_it() {
        // Plausible measured widths: a name, and a two-figure member count.
        for label in [8.0, 42.0, 137.5] {
            for icon in [false, true] {
                for trailing in [None, Some(13.0)] {
                    let width = dropdown_furniture(icon, trailing) + label;
                    let room = width - dropdown_furniture(icon, trailing);
                    assert_eq!(room, label, "{label} px label, icon {icon}, {trailing:?}");
                }
            }
        }
    }

    /// And every part of one is paid for exactly once.
    ///
    /// Written as differences rather than totals so it says what each part
    /// costs instead of restating the sum the widget already computes — a test
    /// that copies the formula passes whatever the formula becomes.
    #[test]
    fn each_part_of_a_dropdown_costs_itself_and_one_gap() {
        let bare = dropdown_furniture(false, None);
        // A bare trigger is its chevron and the gap before it, and nothing else:
        // no fill, no padding, no leading inset.
        assert_eq!(bare, DROPDOWN_GAP + DROPDOWN_CHEVRON);
        assert_eq!(
            dropdown_furniture(true, None) - bare,
            DROPDOWN_ICON + DROPDOWN_GAP
        );
        assert_eq!(
            dropdown_furniture(false, Some(13.0)) - bare,
            13.0 + DROPDOWN_GAP
        );
        assert_eq!(
            dropdown_furniture(true, Some(13.0)) - bare,
            DROPDOWN_ICON + DROPDOWN_GAP + 13.0 + DROPDOWN_GAP
        );
    }

    /// A trigger told to fill its column takes the column and no more.
    ///
    /// The same rule `a_number_row_is_no_wider_than_the_column_it_is_drawn_in`
    /// exists for, and the reason [`DropdownWidth::Fill`] is a variant rather
    /// than every call site passing `ui.available_width()`: two of these sit in
    /// one of the brush editor's `ui.columns` pair, inside a modal of fixed
    /// width, so a picker that reported itself wider than its column would put
    /// the dialog wider than the screen.
    #[test]
    fn a_filled_dropdown_is_no_wider_than_the_column_it_is_drawn_in() {
        // A brush editor column: half the dialog, less the gap between them.
        const COLUMN: f32 = 270.0;
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 600.0))),
            ..Default::default()
        };

        // Twice, for the reason the number row's test runs twice: the first
        // pass builds the font atlas, and a control that only misbehaves once
        // it has been laid out before would otherwise go unseen.
        // A label far longer than the column. Both widths are measured, and the
        // content-sized one is asserted *over* the column — otherwise a label
        // that happened to fit would make this pass without testing anything.
        const LABEL: &str = "The direction the stroke is travelling in, \
                             in degrees measured from due east";

        for _ in 0..2 {
            let _ = ctx.run_ui(input.clone(), |ui| {
                let p = Palette::of(crate::theme::ThemeKind::Graphite);
                let sized = |ui: &mut Ui, width: DropdownWidth| {
                    ui.scope(|ui| {
                        ui.set_max_width(COLUMN);
                        dropdown(
                            ui,
                            &p,
                            Dropdown::new(LABEL).trailing("128").width(width),
                            |_| {},
                        );
                    })
                    .response
                    .rect
                    .width()
                };
                let content = sized(ui, DropdownWidth::Content);
                assert!(
                    content > COLUMN,
                    "the label is too short for this to be a test: {content} px in a \
                     {COLUMN} px column"
                );
                let filled = sized(ui, DropdownWidth::Fill);
                assert!(
                    filled <= COLUMN + 0.5,
                    "the trigger claimed {filled} px of a {COLUMN} px column"
                );
            });
        }
    }

    /// The point of the snap: a drag that lands near a multiple is taken to it,
    /// so 90° is reachable with a hand rather than with luck.
    #[test]
    fn a_drag_that_lands_near_a_multiple_is_taken_to_it() {
        // Within an eighth of 45° — 5.625° — either side.
        for degrees in [45.0, 44.0, 46.0, 41.0, 49.0, 90.5, 314.0] {
            let landed = snapped(degrees, 45.0, 0.0, 359.0);
            assert_eq!(
                landed % 45.0,
                0.0,
                "{degrees} should have been pulled onto a multiple, got {landed}"
            );
        }
    }

    /// And three quarters of the travel is free, or the rail would be a
    /// segmented picker in a slider's clothes.
    #[test]
    fn a_drag_between_two_multiples_is_left_where_the_hand_put_it() {
        // Each of these is more than 5.625° from every multiple of 45.
        for degrees in [20.0, 22.5, 30.0, 60.0, 100.0, 200.5] {
            assert_eq!(snapped(degrees, 45.0, 0.0, 359.0), degrees);
        }
    }

    /// A multiple past the end of the range is not somewhere the control can
    /// go. Clamping to it would put the value at the end of the rail rather
    /// than leaving the drag alone, which is a knob that jumps at the extreme.
    #[test]
    fn a_snap_never_reaches_outside_the_range() {
        // 360° is the nearest multiple to 359° and is not on this rail.
        assert_eq!(snapped(359.0, 45.0, 0.0, 359.0), 359.0);
        // Nor is 0.5 on a rail that starts just above it: close enough to be
        // pulled, and nowhere the control can actually go.
        assert_eq!(snapped(0.52, 0.25, 0.51, 2.0), 0.52);
        // Where the rung *is* on the rail — the scale's own bottom — it lands.
        assert_eq!(snapped(0.76, 0.25, 0.75, 2.0), 0.75);
    }

    /// Every rail but this one passes no step, and must be untouched by the
    /// arithmetic that exists for the one that does.
    #[test]
    fn no_snap_step_is_the_exact_identity() {
        for value in [0.0, 1.0, 0.37, 44.999, -12.5, 1e6] {
            assert_eq!(snapped(value, 0.0, -1e9, 1e9), value);
        }
        // Nothing non-finite is rounded into a number either: a NaN that came
        // out of a drag has to stay a NaN and be dealt with where it was made.
        assert!(snapped(f32::NAN, 45.0, 0.0, 359.0).is_nan());
    }

    /// The readout and the field are one scale and one suffix in two
    /// directions, so a figure typed back exactly as it was shown is the figure
    /// that was shown. This is what "type exactly 90°" rests on.
    #[test]
    fn a_typed_figure_is_the_readout_it_was_taken_from() {
        for degrees in [0.0, 45.0, 90.0, 137.0, 359.0] {
            let row = angle();
            assert_eq!(row.parse(&row.format(degrees)), Some(degrees));
            assert_eq!(row.parse(&row.bare(degrees)), Some(degrees));
        }
        for factor in [0.75, 1.0, 1.25, 1.75, 2.0] {
            let row = scale();
            assert_eq!(row.parse(&row.format(factor)), Some(factor));
            assert_eq!(row.parse(&row.bare(factor)), Some(factor));
        }
    }

    /// The Interface scale's own half of that, on the rungs somebody actually
    /// asks for: 100% to get back to where it started, and 125% or 150% on a
    /// high-density screen. Exact in both directions, so typing the figure the
    /// readout is showing is not a change — a scale that came back as 1.2499999
    /// would rescale the whole interface for a round trip that meant nothing.
    #[test]
    fn the_scales_own_ladder_is_exact_in_both_directions() {
        let row = scale();
        for (factor, shown) in [(1.0, "100%"), (1.25, "125%"), (1.5, "150%")] {
            assert_eq!(row.format(factor), shown);
            assert_eq!(row.bare(factor), shown.trim_end_matches('%'));
            assert_eq!(row.parse(shown), Some(factor));
            assert_eq!(row.parse(&row.bare(factor)), Some(factor));
        }
        // And both ends of the rail, which a percentage has to reach whole:
        // a readout of "75.0%" on a control set in quarters is a decimal place
        // that can never say anything.
        assert_eq!(row.format(*row.range.start()), "75%");
        assert_eq!(row.format(*row.range.end()), "200%");
    }

    /// The scale's rail lands on each 25%, so 125% is reachable with a hand as
    /// well as with the keyboard. In the value's own units — a quarter of a
    /// factor, not 25 of anything — which is the units the whole struct is in.
    #[test]
    fn the_scale_rail_lands_on_each_quarter() {
        let row = scale();
        let (lo, hi) = (*row.range.start(), *row.range.end());
        // Within an eighth of a quarter — 0.03125 — either side of a rung.
        for (near, rung) in [
            (1.0, 1.0),
            (0.98, 1.0),
            (1.02, 1.0),
            (1.24, 1.25),
            (1.27, 1.25),
            (1.48, 1.5),
            (1.76, 1.75),
            (1.99, 2.0),
        ] {
            assert_eq!(
                snapped(near, row.snap, lo, hi),
                rung,
                "{near} should have been pulled onto {rung}"
            );
        }
        // And three quarters of the travel is still free, or the rail would be
        // a segmented picker in a slider's clothes.
        for free in [0.9, 1.1, 1.375, 1.6, 1.9] {
            assert_eq!(snapped(free, row.snap, lo, hi), free);
        }
        // Both ends of the rail are themselves rungs, so a drag pinned at
        // either end lands on one rather than a hair off it.
        assert_eq!(snapped(lo, row.snap, lo, hi), lo);
        assert_eq!(snapped(hi, row.snap, lo, hi), hi);
    }

    /// The suffix is the readout's and never the field's: "125" is what a scale
    /// of 1.25 offers to be typed over, and it means 1.25 back.
    #[test]
    fn the_suffix_is_offered_and_not_demanded() {
        let row = scale();
        assert_eq!(row.format(1.25), "125%");
        assert_eq!(row.bare(1.25), "125");
        assert_eq!(row.parse("125"), Some(1.25));
        assert_eq!(row.parse("125%"), Some(1.25));
        assert_eq!(row.parse("  125 % "), Some(1.25));
        let row = angle();
        assert_eq!(row.format(90.0), "90°");
        assert_eq!(row.parse("90"), Some(90.0));
        assert_eq!(row.parse("90°"), Some(90.0));
    }

    /// A line that means nothing leaves the value exactly as it was, rather
    /// than resolving to zero — which for an angle would be a shape that
    /// silently snapped back to its neutral because somebody mistyped.
    #[test]
    fn a_line_that_means_nothing_is_refused() {
        let row = angle();
        for text in ["", " ", "°", "ninety", "9 0", "1/2", "--3", "nan", "inf"] {
            assert_eq!(row.parse(text), None, "{text:?} parsed");
        }
    }

    /// The brush library panel's own sample, at the size it is actually drawn
    /// at, so the assertions below are about the picture the rows show.
    fn sample_box() -> MarkBox {
        MarkBox::of(Rect::from_min_size(pos2(0.0, 0.0), vec2(64.0, 14.0)), 1.0)
    }

    fn preview_of(brush: &Brush) -> Mark {
        preview_mark(brush, None, &sample_box())
    }

    /// The row as it is actually inked — which is the coverage *and* the
    /// opacity applied to it, and the two palette colours a blender's mark is
    /// drawn between.
    fn pixels_of(brush: &Brush) -> Vec<Color32> {
        let at = sample_box();
        preview_image(
            &preview_mark(brush, None, &at),
            brush,
            &at,
            [Color32::WHITE, Color32::GRAY],
        )
        .pixels
    }

    /// The reason the sample is stamped from the brush at all: two presets that
    /// differ in one setting have to make two different marks. Drawing the row
    /// from a couple of numbers is what made two hundred rows look like one row
    /// repeated, and every setting here is one somebody would expect to see.
    #[test]
    fn two_brushes_that_differ_draw_different_marks() {
        let base = Brush::default();
        let plain = pixels_of(&base);
        for (what, brush) in [
            (
                "roundness",
                Brush {
                    dab_ratio: 6.0,
                    ..base
                },
            ),
            (
                "angle",
                Brush {
                    dab_ratio: 6.0,
                    dab_angle: 60.0,
                    ..base
                },
            ),
            (
                "spacing",
                Brush {
                    spacing: 0.9,
                    ..base
                },
            ),
            (
                "hardness",
                Brush {
                    hardness: 0.05,
                    ..base
                },
            ),
            (
                "opacity",
                Brush {
                    opacity: 0.3,
                    ..base
                },
            ),
            (
                "scatter",
                Brush {
                    scatter: 1.5,
                    ..base
                },
            ),
            (
                "size response",
                Brush {
                    min_size_ratio: 1.0,
                    ..base
                },
            ),
            (
                "pickup",
                Brush {
                    smudge: 0.9,
                    ..base
                },
            ),
            (
                "build-up",
                Brush {
                    build_up: true,
                    opacity: 0.4,
                    ..base
                },
            ),
        ] {
            // Not `assert_ne!`: these are a few thousand pixels each, and a
            // failure that prints both of them says less than one that names
            // the setting.
            assert!(
                pixels_of(&brush) != plain,
                "{what} made no difference to the row"
            );
        }
    }

    /// And one brush draws the same mark every time, because the engine's RNG
    /// is seeded from a builder that has begun exactly one stroke.
    ///
    /// Byte for byte, on the brush with every random feature turned on. A seed
    /// off the clock — or one carried between rows — would make two presets
    /// differ because one drew luckier numbers, and would make the list shimmer
    /// as it scrolled.
    #[test]
    fn one_brush_draws_the_same_mark_twice() {
        let brush = Brush {
            scatter: 1.4,
            radius_jitter: 0.5,
            dab_angle_jitter: 180.0,
            dab_ratio: 3.0,
            ..Default::default()
        };
        assert!(
            preview_of(&brush).coverage == preview_of(&brush).coverage,
            "the same brush drew two different rows"
        );
    }

    /// The wet-layer guarantee, in a thumbnail: a stroke that crosses itself
    /// does not darken where it crosses, and the stroke's opacity is what the
    /// heaviest pixel comes out at.
    ///
    /// The path has a loop in it, so every preview overlaps itself by
    /// construction — which is what makes this the one rule the row could not
    /// go on approximating. One translucent shape per dab would put the middle
    /// of this stroke at nearly full opacity whatever the brush asked for.
    #[test]
    fn overlapping_dabs_in_a_preview_do_not_compound() {
        for build_up in [false, true] {
            let brush = Brush {
                spacing: 0.02,
                opacity: 0.4,
                hardness: 1.0,
                pressure_size: false,
                build_up,
                ..Default::default()
            };
            let mark = preview_of(&brush);
            let peak = mark.coverage.iter().copied().fold(0.0f32, f32::max);
            assert!(
                peak <= 1.0 + 1e-5,
                "coverage compounded to {peak} with build_up {build_up}"
            );
            // And it does reach the top, or the bound above would hold for a
            // row that drew nothing at all.
            assert!(peak > 0.99, "the stroke never covered anything: {peak}");
            // Which is the opacity asked for, once, and not once per overlap.
            assert!((peak * brush.opacity - 0.4).abs() < 1e-4);
        }
    }

    /// Nothing may be freed by the pass that still draws it.
    ///
    /// The Brushes panel and the library browser show the same presets at two
    /// different row heights, and both are on screen together — the browser is
    /// a modal drawn over the panel. So one preset is one `&Brush`, drawn twice
    /// in one pass, at two sizes. Keyed on the address alone the two shared a
    /// single cache entry: the browser's row replaced the panel's, dropping the
    /// last `TextureHandle` to a texture the panel had already queued a `Shape`
    /// against. egui reports that as a free in the *same* pass's
    /// `textures_delta`, and `egui_wgpu::Renderer::free_texture` destroys a
    /// texture outright — so the frame failed validation at `Queue::submit` and
    /// the application went down with it. Opening the library was enough.
    ///
    /// This is the CPU half of the guard, and it is the half worth having:
    /// `app::submit_frame` makes a same-pass free survivable, and this makes
    /// the library stop asking it to. It also pins the cost — two lists fighting
    /// over one entry re-rasterised and re-uploaded every visible row of both,
    /// every frame the browser was open.
    #[test]
    fn a_preset_drawn_in_two_lists_at_once_frees_no_texture_either_still_draws() {
        use egui::epaint::Primitive;
        use std::collections::HashSet;

        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 600.0))),
            ..Default::default()
        };
        let p = Palette::of(crate::theme::ThemeKind::Graphite);
        // One brush at one address, exactly as `Editor::presets` hands the same
        // element to both lists.
        let brush = Brush::default();
        let row = |ui: &mut Ui, height: f32| {
            brush_row(
                ui,
                &p,
                BrushRow {
                    name: "Pencil",
                    detail: "",
                    brush: &brush,
                    tip: None,
                    selected: false,
                    user: false,
                    height,
                    trailing: 0.0,
                    draggable: false,
                },
            );
        };

        // Three passes: the first builds the font atlas and both entries, and
        // the failure is a *replacement*, so it needs a pass with something
        // already there to replace.
        for pass in 0..3 {
            let output = ctx.run_ui(input.clone(), |ui| {
                row(ui, metrics::BRUSH_ROW);
                row(ui, metrics::BRUSH_ROW_DETAIL);
            });
            let drawn: HashSet<egui::TextureId> = ctx
                .tessellate(output.shapes, output.pixels_per_point)
                .iter()
                .filter_map(|job| match &job.primitive {
                    Primitive::Mesh(mesh) => Some(mesh.texture_id),
                    Primitive::Callback(_) => None,
                })
                .collect();
            for id in &output.textures_delta.free {
                assert!(
                    !drawn.contains(id),
                    "pass {pass}: {id:?} was freed by the pass that drew it"
                );
            }
        }
    }
}
