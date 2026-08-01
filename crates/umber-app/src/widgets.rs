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
use umber_core::{Brush, ResponseCurve};

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

        changed = drag_track(&response, track, value, lo, hi, log);
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
fn drag_track(
    response: &Response,
    track: Rect,
    value: &mut f32,
    lo: f32,
    hi: f32,
    log: bool,
) -> bool {
    if !(response.dragged() || response.clicked()) {
        return false;
    }
    let Some(pos) = response.interact_pointer_pos() else {
        return false;
    };
    let t = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
    let next = from_t(t, lo, hi, log);
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
    let changed = drag_track(&response, track, value, lo, hi, log);
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
    let changed = drag_track(&response, track, value, lo, hi, false);
    paint_track(ui.painter(), p, track, to_t(*value, lo, hi, false), 0.0);
    changed
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
    /// drawn from. The library is 201 presets deep and the sample is how you
    /// choose between them, so it has to show what actually separates them.
    pub brush: &'a Brush,
    pub selected: bool,
    /// One the user saved, as opposed to one Umber ships. Marked with a dot
    /// rather than a word, because the panel is 264 px wide.
    pub user: bool,
    pub height: f32,
    /// Width kept clear at the right for controls the caller draws over the
    /// row — rename and delete in the browser. Reserved always, so a name does
    /// not reflow the moment the pointer arrives.
    pub trailing: f32,
}

/// A brush preset: a stroke sample, then the name.
///
/// The sample is stamped from the preset's own settings — spacing, shape,
/// angle, scatter, jitter and colour pickup, all under a pressure ramp — so a
/// chisel reads as a chisel and a spray as a spray. Drawing it from opacity and
/// hardness alone made two hundred rows look like one row repeated, which in a
/// list this long is the difference between choosing a brush and scrolling past
/// it.
pub fn brush_row(ui: &mut Ui, p: &Palette, row: BrushRow<'_>) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), row.height), Sense::click());

    // The library is 201 presets deep and both lists are scrolled, so most
    // rows on most frames are off screen. Each sample is a few dozen stamps;
    // painting the invisible ones is the one part of this that would show up
    // in a frame time.
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter();
    if row.selected {
        painter.rect_filled(rect, metrics::RADIUS, p.control_active);
    } else if response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control);
    }

    let sample_h = (row.height - 12.0).clamp(10.0, 16.0);
    let sample = Rect::from_center_size(
        pos2(rect.left() + 7.0 + 32.0, rect.center().y),
        vec2(64.0, sample_h),
    );
    brush_sample(painter, p, sample, row.brush);

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

/// A deterministic scatter source for the previews.
///
/// The same xorshift the stroke builder uses, and started from the same seed on
/// every row: two brushes then differ because their *settings* differ, not
/// because one happened to draw a luckier set of numbers. A seed off the clock
/// would also make the list shimmer as it scrolled.
struct Scatter(u32);

impl Scatter {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Roughly normal, mean 0, sd 1 — three uniforms summed, as in
    /// `umber_core::stroke`, so a preview scatters in the same shape the
    /// engine does.
    fn gaussian(&mut self) -> f32 {
        self.next() + self.next() + self.next()
    }
}

/// Stamp one brush's mark into `sample`.
///
/// A miniature of the real dab loop: a pressure ramp along the stroke drives
/// size, coverage, hardness and scatter through the brush's own curves, and
/// each stamp is the ellipse the engine would lay down. It is not a simulation
/// — there is no wet layer, so overlaps here do compound where the canvas would
/// saturate — but every difference it *does* show is a real one.
fn brush_sample(painter: &egui::Painter, p: &Palette, sample: Rect, brush: &Brush) {
    // Scatter and jitter throw stamps off the line, and the sample sits inside
    // a row that also holds a name. Clip rather than clamp: a spray squashed
    // back onto its own axis is a spray that looks like a line.
    let painter = painter.with_clip_rect(sample.expand2(vec2(2.0, 3.0)));

    let radius = sample.height() * 0.5;
    // Spacing is a fraction of the diameter, so this is the real dab count for
    // a stroke this long — which is what makes a widely spaced spray read as
    // separate blobs rather than as a thinner line.
    let steps =
        (sample.width() / (radius * 2.0 * brush.spacing).max(0.5)).clamp(6.0, 40.0) as usize;

    let mut rng = Scatter(0x9E37_79B9);
    let aspect = brush.dab_ratio.max(1.0);
    // A blender's mark carries the colour it found rather than the palette's,
    // and the row has to show that or every Blenders entry looks like a paint.
    // The "found" colour is the dim ink: it reads as canvas rather than as a
    // second palette colour, and it needs no token of its own.
    let carried = brush.smudge.clamp(0.0, 1.0);
    let keep = brush.smudge_length.clamp(0.0, 0.99);

    for i in 0..steps {
        let t = i as f32 / (steps - 1).max(1) as f32;
        // The ramp stands in for pressure: nothing at the ends, full in the
        // middle. That is what gives a tapered stroke, and feeding it through
        // the brush's curves is what makes the pressure dynamics visible here.
        //
        // The `max(0.0)` is load-bearing: `sin(PI)` in f32 lands just *below*
        // zero, and a negative base with a fractional exponent is NaN, which
        // propagates into the alpha and trips ecolor's assert.
        let pressure = (t * std::f32::consts::PI).sin().max(0.0);
        // Width comes from the brush's own size response, not from the ramp: a
        // marker that ignores pressure should draw a bar of even width, and
        // tapering it anyway is exactly the flattery that made every row look
        // the same.
        let width = brush.radius_at(pressure) / (brush.size * 0.5).max(0.5);
        let mut r = radius * width.clamp(0.0, 1.0);
        if brush.radius_jitter > 0.0 {
            r *= (rng.gaussian() * brush.radius_jitter).exp();
            r = r.min(radius * 1.6);
        }
        if r <= 0.25 {
            continue;
        }

        let mut centre = pos2(sample.left() + sample.width() * t, sample.center().y);
        let scatter = brush.scatter_at(pressure);
        if scatter > 0.0 {
            let spread = r * scatter;
            centre += vec2(rng.gaussian(), rng.gaussian()) * spread;
        }

        // The sample stroke runs left to right, so a dab that follows the
        // stroke sits at zero and one that does not sits at its own angle.
        let mut angle = if brush.dab_angle_follows_stroke {
            0.0
        } else {
            brush.dab_angle.to_radians()
        };
        if brush.dab_angle_jitter > 0.0 {
            angle += rng.next() * brush.dab_angle_jitter.to_radians() * 0.5;
        }

        // Softer brushes read as a wider, fainter smear.
        let hardness = brush.hardness_at(pressure);
        let alpha = (brush.opacity * brush.coverage_at(pressure))
            * pressure.powf(1.0 + (1.0 - hardness) * 1.5);
        // How much of the picked-up colour a stamp is still carrying. A short
        // smear loses it within a stamp or two; a long one keeps it the whole
        // way. `0f32.powf(0.0)` is 1, so the head of the stroke is always fully
        // loaded — which is what a blender does.
        let held = carried * keep.powf(t * 6.0);
        let ink = mix(p.text_strong, p.text_dim, held).gamma_multiply(alpha.clamp(0.0, 1.0));

        if aspect > 1.05 {
            // A 10:1 dab whose long axis fits a 14 px row has a short axis of
            // less than a pixel. The floor is legibility, not flattery: below
            // it the mark stops being a thin shape and becomes an absent one,
            // and a row that shows nothing tells you less than one that
            // exaggerates slightly.
            let minor = (r / aspect).max(0.6);
            painter.add(egui::Shape::convex_polygon(
                ellipse(centre, r, minor, angle),
                ink,
                Stroke::NONE,
            ));
        } else {
            painter.circle_filled(centre, r, ink);
        }
    }
}

/// Points around an ellipse with semi-axes `a` (along `angle`) and `b`.
///
/// Ten segments: enough that a 10:1 chisel reads as a straight-edged sliver at
/// 14 px tall, and few enough that forty of them per row stay off the frame
/// budget.
fn ellipse(centre: egui::Pos2, a: f32, b: f32, angle: f32) -> Vec<egui::Pos2> {
    const SEGMENTS: usize = 10;
    let (sin, cos) = angle.sin_cos();
    (0..SEGMENTS)
        .map(|k| {
            let theta = k as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let (x, y) = (a * theta.cos(), b * theta.sin());
            centre + vec2(x * cos - y * sin, x * sin + y * cos)
        })
        .collect()
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
}

/// One row of the layer stack: visibility, a thumbnail chip, name and blend.
///
/// `slot` identifies the layer for the eye's own hit target. The layer's *name*
/// looks like the obvious key and is not one: names Umber generates are unique,
/// but an imported ORA or PSD routinely carries two layers called the same
/// thing, and two widgets sharing an id is an egui id clash — one of the two
/// eyes then stops answering. A slot is unique by construction and never
/// changes hands while a layer exists.
pub fn layer_row(
    ui: &mut Ui,
    p: &Palette,
    name: &str,
    slot: u32,
    visible: bool,
    active: bool,
    blend: &str,
) -> LayerRowResponse {
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

    // The eye is its own hit target inside the row, so toggling visibility
    // does not also change the selection.
    let eye = Rect::from_min_size(rect.left_top() + vec2(5.0, 6.0), vec2(18.0, 18.0));
    let eye_response = ui.interact(eye, ui.id().with(("eye", slot)), Sense::click());

    icons::draw(
        ui.painter(),
        eye,
        if visible { Icon::Eye } else { Icon::EyeOff },
        if visible { p.text } else { p.text_dim },
    );

    // Thumbnail placeholder: a checker chip. Rendering real layer thumbnails
    // needs a downscale pass that does not exist yet.
    let thumb = Rect::from_min_size(rect.left_top() + vec2(27.0, 3.0), vec2(24.0, 24.0));
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

    painter.text(
        pos2(thumb.right() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        name,
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
