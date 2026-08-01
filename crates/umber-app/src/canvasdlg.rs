//! Canvas settings: the New document dialog, and the same controls again for
//! the document already open.
//!
//! One form, two dialogs. They ask the painter the same four questions — how
//! many pixels, on what background, at what resolution, and (only when the size
//! changes) where the old picture goes — so they are one [`CanvasForm`] and one
//! body, with the differences named at the two call sites rather than
//! duplicated. Two dialogs drifting apart is how "New" ends up offering a preset
//! that "Canvas settings" cannot express.
//!
//! Nothing here reaches the GPU. The dialogs return a [`Document`] (and an
//! [`Anchor`], when there is a resize to place), and `app.rs` does the work,
//! because a resize means reallocating every texture the document owns and
//! clearing an undo history whose rectangles no longer mean anything.

use egui::{Rect, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};
use umber_core::{Anchor, Background, Color, Document, Hsv, Unit, document};

use crate::colorpicker::{self, PickerMode, WheelShape};
use crate::editor::Editor;
use crate::tabs;
use crate::theme::{Palette, metrics, text};
use crate::widgets;

/// Which dialog is open. They share [`CanvasForm`], so only one can be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    /// Describe a document that does not exist yet.
    New,
    /// Change the one in front.
    Settings,
}

/// A canvas change the caller has to carry out.
#[derive(Clone, Copy, Debug)]
pub struct CanvasChange {
    pub doc: Document,
    /// Where the old pixels sit in the new canvas. Meaningless when the size is
    /// unchanged, and ignored there.
    pub anchor: Anchor,
}

/// The four backgrounds the design offers.
///
/// A choice rather than a bare `Background`, because "white" and "a custom
/// colour that happens to be white" are different things to a picker: switching
/// to White and back must not throw away the colour that was being mixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Transparent,
    White,
    Black,
    Custom,
}

/// The document sizes worth one click.
///
/// Each carries its own resolution, because that is the only thing that makes
/// "A4" mean anything: 2480 × 3508 is A4 at 300 dpi and a meaningless pair of
/// numbers at 72.
const PRESETS: &[(&str, u32, u32, u32)] = &[
    ("Square 2048", 2048, 2048, 72),
    ("Square 1080", 1080, 1080, 72),
    ("HD", 1920, 1080, 72),
    ("4K", 3840, 2160, 72),
    ("A4", 2480, 3508, 300),
    ("A5", 1748, 2480, 300),
    ("US Letter", 2550, 3300, 300),
];

/// State of the canvas dialogs.
///
/// Application state rather than per-document state, and so it lives above
/// `editor.rs`'s `--- documents ---` line: it is a dialog, seeded from the live
/// document when it opens and dead the moment it closes. Not part of `UiState`
/// only because that is `Copy` and this holds a picker's HSV, which has the same
/// reason to be its own source of truth here as it does in the Colour panel.
#[derive(Clone, Debug)]
pub struct CanvasForm {
    pub open: Option<Dialog>,
    width: u32,
    height: u32,
    /// Changing one edge drives the other.
    lock: bool,
    /// Width over height, captured when the lock was switched on rather than
    /// recomputed from the fields. Recomputing would make the ratio drift a
    /// little with every rounded edge, so a locked 16:9 would stop being 16:9
    /// after a few nudges.
    ratio: f32,
    choice: Choice,
    /// The custom colour's own state. HSV rather than RGB for the same reason
    /// `Editor::hsv` is: hue is undefined for greys, so deriving it each frame
    /// means dragging value to black resets the hue to red.
    hsv: Hsv,
    dpi: u32,
    unit: Unit,
    anchor: Anchor,
    /// The size the dialog opened on, so it can tell a resize from an edit that
    /// leaves the geometry alone.
    from: Document,
}

impl Default for CanvasForm {
    fn default() -> Self {
        Self::seeded(Document::default())
    }
}

impl CanvasForm {
    fn seeded(doc: Document) -> Self {
        Self {
            open: None,
            width: doc.size.x,
            height: doc.size.y,
            lock: false,
            ratio: ratio_of(doc.size.x, doc.size.y),
            choice: match doc.background {
                Background::Transparent => Choice::Transparent,
                Background::Colour(c) if c == Color::WHITE => Choice::White,
                Background::Colour(c) if c == Color::BLACK => Choice::Black,
                Background::Colour(_) => Choice::Custom,
            },
            hsv: doc.background.colour().unwrap_or(Color::WHITE).to_hsv(),
            dpi: doc.dpi.round() as u32,
            unit: Unit::Millimetres,
            anchor: Anchor::Centre,
            from: doc,
        }
    }

    /// Open a dialog, seeded from `doc`.
    ///
    /// New inherits the live document wholesale — size, background and
    /// resolution — which is what makes it useful next to an imported one: the
    /// common reason to open a second tab is to try something at the same scale
    /// on the same paper.
    pub fn open(&mut self, which: Dialog, doc: Document) {
        *self = Self::seeded(doc);
        self.open = Some(which);
    }

    fn background(&self) -> Background {
        match self.choice {
            Choice::Transparent => Background::Transparent,
            Choice::White => Background::WHITE,
            Choice::Black => Background::BLACK,
            Choice::Custom => Background::opaque(self.hsv.to_color(1.0)),
        }
    }

    /// The document the form describes.
    fn document(&self) -> Document {
        Document::new(self.width, self.height)
            .with_background(self.background())
            .with_dpi(self.dpi as f32)
    }

    fn resizes(&self) -> bool {
        self.document().size != self.from.size
    }
}

/// Width over height, guarded so a lock can never divide by zero.
fn ratio_of(width: u32, height: u32) -> f32 {
    if height == 0 {
        1.0
    } else {
        width as f32 / height as f32
    }
}

/// The height that keeps `ratio` at `width`, and the width that keeps it at
/// `height`.
///
/// Rounded and clamped to a canvas that can exist. Deliberately driven from the
/// *stored* ratio rather than from the other field: computing it fresh each time
/// would let a rounded edge feed back into the ratio, so a locked 16:9 would
/// stop being 16:9 after a few nudges.
fn locked_height(width: u32, ratio: f32) -> u32 {
    edge(width as f32 / ratio.max(f32::MIN_POSITIVE))
}

fn locked_width(height: u32, ratio: f32) -> u32 {
    edge(height as f32 * ratio)
}

/// A float that came out of a ratio, as a canvas edge that can exist.
///
/// `clamp` rather than a cast: an absurd ratio overflows to infinity, and
/// `inf as u32` is a saturating cast in Rust but `NaN as u32` is zero — a canvas
/// with no pixels in it, which is a validation error rather than a small
/// picture. So infinity clamps to the largest canvas and NaN falls back to the
/// smallest.
fn edge(value: f32) -> u32 {
    if value.is_nan() {
        return 1;
    }
    value.round().clamp(1.0, Document::MAX_EDGE as f32) as u32
}

/// Draw whichever canvas dialog is open.
pub fn show(root: &mut Ui, p: &Palette, ed: &mut Editor, out: &mut Outcome) {
    let Some(which) = ed.canvas_form.open else {
        return;
    };
    let form = &mut ed.canvas_form;
    let mut close = false;

    let modal = egui::Modal::new(egui::Id::new("canvas-settings"))
        .frame(tabs::dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(360.0);
            heading(
                ui,
                p,
                match which {
                    Dialog::New => "New document",
                    Dialog::Settings => "Canvas settings",
                },
            );
            ui.add_space(10.0);

            if which == Dialog::New {
                presets(ui, p, form);
                ui.add_space(12.0);
            }

            size_fields(ui, p, form);
            ui.add_space(12.0);
            resolution(ui, p, form);
            ui.add_space(12.0);
            background_fields(ui, p, form);

            // Only when there is something to place. On a New document there is
            // nothing to anchor, and on an unchanged size the control would be
            // a live knob that does nothing.
            if which == Dialog::Settings && form.resizes() {
                ui.add_space(12.0);
                anchor_field(ui, p, form);
            }

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if tabs::button(ui, p, "Cancel", false) {
                    close = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = match which {
                        Dialog::New => "Create",
                        Dialog::Settings => "Apply",
                    };
                    if tabs::button(ui, p, label, true) {
                        match which {
                            Dialog::New => out.create = Some(form.document()),
                            Dialog::Settings => {
                                out.change = Some(CanvasChange {
                                    doc: form.document(),
                                    anchor: form.anchor,
                                })
                            }
                        }
                        close = true;
                    }
                });
            });
        });

    // Escape and a click outside both mean "not now", which is the safe answer
    // for a dialog that can throw an undo history away.
    if close || modal.should_close() {
        ed.canvas_form.open = None;
    }
}

/// What a dialog asked for this frame.
#[derive(Default, Clone, Copy)]
pub struct Outcome {
    pub create: Option<Document>,
    pub change: Option<CanvasChange>,
}

fn heading(ui: &mut Ui, p: &Palette, label: &str) {
    ui.label(
        egui::RichText::new(label)
            .size(text::CONTROL)
            .color(p.text_strong)
            .strong(),
    );
}

fn caption(ui: &mut Ui, p: &Palette, label: &str) {
    ui.label(
        egui::RichText::new(label)
            .size(text::SMALL)
            .color(p.text_dim),
    );
}

fn presets(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) {
    caption(ui, p, "Preset");
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        for (name, w, h, dpi) in PRESETS {
            let selected = form.width == *w && form.height == *h && form.dpi == *dpi;
            if tabs::button(ui, p, name, selected) {
                form.width = *w;
                form.height = *h;
                form.dpi = *dpi;
                // A preset states a shape, so it also states the ratio a lock
                // would hold. Leaving the old one behind would make the next
                // nudge undo the preset.
                form.ratio = ratio_of(*w, *h);
            }
        }
    });
}

fn size_fields(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) {
    caption(ui, p, "Canvas size");
    ui.add_space(6.0);

    if number_row(ui, p, "Width", &mut form.width, "px", edges()) && form.lock {
        form.height = locked_height(form.width, form.ratio);
    }
    if number_row(ui, p, "Height", &mut form.height, "px", edges()) && form.lock {
        form.width = locked_width(form.height, form.ratio);
    }

    ui.add_space(4.0);
    let was = form.lock;
    widgets::toggle_row(ui, p, "Lock aspect ratio", &mut form.lock);
    if form.lock && !was {
        // Captured on the way in, so the lock holds the shape the canvas has
        // right now rather than one it had earlier.
        form.ratio = ratio_of(form.width, form.height);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let (w, h) = (
            document::physical_size(form.width, form.dpi as f32, form.unit),
            document::physical_size(form.height, form.dpi as f32, form.unit),
        );
        let digits = match form.unit {
            Unit::Millimetres => 1,
            Unit::Inches => 2,
        };
        ui.label(
            egui::RichText::new(format!("{w:.digits$} × {h:.digits$}"))
                .size(text::SMALL)
                .color(p.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut unit = form.unit;
            if widgets::segmented(
                ui,
                p,
                &mut unit,
                &[
                    (Unit::Millimetres, Unit::Millimetres.label()),
                    (Unit::Inches, Unit::Inches.label()),
                ],
            ) {
                form.unit = unit;
            }
        });
    })
    .response
    .on_hover_text(
        "How big the canvas is on paper, at the resolution below. Nothing here \
         changes a pixel.",
    );
}

fn resolution(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) {
    caption(ui, p, "Resolution");
    ui.add_space(6.0);
    number_row(
        ui,
        p,
        "Pixels per inch",
        &mut form.dpi,
        "dpi",
        Document::MIN_DPI as u32..=Document::MAX_DPI as u32,
    );
}

fn background_fields(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) {
    caption(ui, p, "Background");
    ui.add_space(6.0);

    let mut choice = form.choice;
    if widgets::segmented(
        ui,
        p,
        &mut choice,
        &[
            (Choice::Transparent, "None"),
            (Choice::White, "White"),
            (Choice::Black, "Black"),
            (Choice::Custom, "Custom"),
        ],
    ) {
        form.choice = choice;
    }

    if form.choice == Choice::Custom {
        ui.add_space(8.0);
        // The Colour panel's own slider mode, so the two mix a colour the same
        // way. `shape` and `rotate` belong to the wheel and are untouched here.
        let mut shape = WheelShape::Triangle;
        let mut rotate = false;
        colorpicker::show(
            ui,
            p,
            PickerMode::Sliders,
            &mut shape,
            &mut rotate,
            &mut form.hsv,
        );
    }
}

fn anchor_field(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) {
    let from = form.from.size;
    let to = form.document().size;
    caption(ui, p, "Existing artwork");
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        anchor_grid(ui, p, &mut form.anchor);
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} × {} becomes {} × {}",
                    from.x, from.y, to.x, to.y
                ))
                .size(text::SMALL)
                .color(p.text),
            );
            ui.add_space(4.0);
            // Said out loud rather than discovered afterwards. Undo stores the
            // rectangles a stroke damaged, and a rectangle in the old geometry
            // means different pixels in the new one — so the history goes,
            // exactly as it does when a layer is deleted.
            ui.label(
                egui::RichText::new(
                    "Resizing clears the undo history: it stores rectangles of \
                     this canvas, which mean nothing on a different one.",
                )
                .size(text::TINY)
                .color(p.warning),
            );
        });
    });
}

/// The nine-square anchor picker.
fn anchor_grid(ui: &mut Ui, p: &Palette, anchor: &mut Anchor) -> bool {
    const CELL: f32 = 16.0;
    const GAP: f32 = 3.0;
    let side = CELL * 3.0 + GAP * 2.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());

    let mut changed = false;
    for (i, option) in Anchor::GRID.iter().enumerate() {
        let cell = Rect::from_min_size(
            rect.min + vec2((i % 3) as f32 * (CELL + GAP), (i / 3) as f32 * (CELL + GAP)),
            Vec2::splat(CELL),
        );
        let response = ui.interact(cell, ui.id().with(("anchor", i)), Sense::click());
        if response.clicked() {
            *anchor = *option;
            changed = true;
        }
        let selected = *anchor == *option;
        let painter = ui.painter();
        painter.rect_filled(
            cell,
            metrics::RADIUS,
            match (selected, response.hovered()) {
                (true, _) => p.accent,
                (false, true) => p.control_hover,
                (false, false) => p.control,
            },
        );
        if !selected {
            painter.rect_stroke(
                cell,
                metrics::RADIUS,
                Stroke::new(1.0, p.border),
                StrokeKind::Inside,
            );
        }
    }
    changed
}

/// A dim label with a number field pushed to the right edge.
///
/// `egui::DragValue` rather than something painted in `widgets.rs`: the design
/// has no numeric field, and a canvas size is one of the few values in this
/// application that people type exactly rather than feel for on a rail.
///
/// The range is the caller's, and it is what stops a field asking for a canvas
/// nothing could allocate or a resolution the physical readout would divide by.
fn number_row(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    value: &mut u32,
    suffix: &str,
    range: std::ops::RangeInclusive<u32>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .size(text::SMALL)
                .color(p.text_dim),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = ui
                .add(
                    egui::DragValue::new(value)
                        .range(range)
                        .speed(1.0)
                        .suffix(format!(" {suffix}")),
                )
                .changed();
        });
    });
    changed
}

/// Every canvas edge a field will accept.
fn edges() -> std::ops::RangeInclusive<u32> {
    1..=Document::MAX_EDGE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locked_edge_keeps_the_shape_it_was_locked_at() {
        let ratio = ratio_of(1920, 1080);
        assert_eq!(locked_height(960, ratio), 540);
        assert_eq!(locked_width(540, ratio), 960);
        // A4 at 300 dpi, halved.
        let a4 = ratio_of(2480, 3508);
        assert_eq!(locked_height(1240, a4), 1754);
    }

    #[test]
    fn a_locked_ratio_does_not_wander_as_an_edge_is_nudged() {
        // The reason the ratio is captured rather than recomputed: driving the
        // other edge from a freshly divided pair lets rounding feed back, and a
        // locked 16:9 stops being 16:9 after a few nudges.
        let ratio = ratio_of(1920, 1080);
        let mut width = 1920;
        for _ in 0..200 {
            width += 1;
            let height = locked_height(width, ratio);
            let drift = (width as f32 / height as f32) - ratio;
            assert!(drift.abs() < 0.01, "{width}x{height} drifted by {drift}");
        }
    }

    #[test]
    fn a_locked_edge_can_never_ask_for_a_canvas_that_cannot_exist() {
        // The fields clamp, but the lock computes — so a very wide ratio and a
        // large edge could otherwise drive the other one past the limit, or a
        // very tall one round it down to nothing.
        for ratio in [1e-6, 0.01, 1.0, 100.0, 1e6] {
            for edge in [1u32, 37, 4096, Document::MAX_EDGE] {
                let h = locked_height(edge, ratio);
                let w = locked_width(edge, ratio);
                assert!((1..=Document::MAX_EDGE).contains(&h), "{ratio} {edge} {h}");
                assert!((1..=Document::MAX_EDGE).contains(&w), "{ratio} {edge} {w}");
            }
        }
        // A document with no height cannot happen, but the ratio is arithmetic
        // and arithmetic gets handed whatever the fields hold. A ratio of zero
        // divides to infinity, which is the largest canvas rather than — as an
        // unguarded cast through NaN would give — one with no pixels in it.
        assert_eq!(ratio_of(100, 0), 1.0);
        assert_eq!(locked_height(100, 0.0), Document::MAX_EDGE);
        assert_eq!(locked_width(100, f32::NAN), 1);
    }

    #[test]
    fn a_form_seeded_from_a_document_describes_that_document() {
        for background in [
            Background::Transparent,
            Background::WHITE,
            Background::BLACK,
            Background::opaque(Color::from_srgb_u8(120, 60, 30, 255)),
        ] {
            let doc = Document::new(1234, 567)
                .with_background(background)
                .with_dpi(300.0);
            let form = CanvasForm::seeded(doc);
            assert_eq!(form.document(), doc, "{background:?} did not come back");
            assert!(!form.resizes(), "seeding is not a resize");
        }
    }

    #[test]
    fn switching_away_from_a_custom_colour_and_back_keeps_it() {
        // The reason the choice is an enum beside the colour rather than a bare
        // `Background`: White and "a custom colour that is white" are the same
        // background and different things to mix from.
        let mixed = Color::from_srgb_u8(200, 120, 40, 255);
        let mut form =
            CanvasForm::seeded(Document::default().with_background(Background::opaque(mixed)));
        assert_eq!(form.choice, Choice::Custom);

        form.choice = Choice::White;
        assert_eq!(form.background(), Background::WHITE);
        form.choice = Choice::Custom;
        assert_eq!(
            form.background().colour().map(Color::to_srgb_u8),
            Some(mixed.to_srgb_u8()),
        );
    }

    #[test]
    fn every_shipped_preset_is_the_size_its_name_claims() {
        for (name, w, h, dpi) in PRESETS {
            let doc = Document::new(*w, *h).with_dpi(*dpi as f32);
            let (mw, mh) = doc.physical(Unit::Millimetres);
            match *name {
                "A4" => assert!(
                    (mw - 210.0).abs() < 1.0 && (mh - 297.0).abs() < 1.0,
                    "{mw}x{mh}"
                ),
                "A5" => assert!(
                    (mw - 148.0).abs() < 1.0 && (mh - 210.0).abs() < 1.0,
                    "{mw}x{mh}"
                ),
                "US Letter" => {
                    let (iw, ih) = doc.physical(Unit::Inches);
                    assert!(
                        (iw - 8.5).abs() < 0.02 && (ih - 11.0).abs() < 0.02,
                        "{iw}x{ih}"
                    );
                }
                _ => {}
            }
        }
    }
}
