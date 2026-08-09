//! Canvas settings: the New document dialog, and the same controls again for
//! the document already open.
//!
//! One form, two dialogs. They ask the painter the same questions — what shape,
//! how many pixels, on what background, at what resolution, and (only when the
//! size changes) where the old picture goes — so they are one [`CanvasForm`] and
//! one body, with the differences named at the two call sites rather than
//! duplicated. Two dialogs drifting apart is how "New" ends up offering a size
//! that "Canvas settings" cannot express.
//!
//! **What a size *is* lives in [`umber_core::canvassize`], not here.** The
//! shapes, the sizes under each, the paper table, the arithmetic that turns A4
//! and 300 dpi into 2480 × 3508, which shape a given canvas reads as and what
//! the device will hold are all rules, and a rule belongs where it can be tested
//! without a window — the division `CanvasCopy::plan` and `Clip::place` already
//! keep. This file draws them and decides none of them.
//!
//! Nothing here reaches the GPU. The dialogs return a [`Document`] (and an
//! [`Anchor`], when there is a resize to place), and `app.rs` does the work,
//! because a resize means reallocating every texture the document owns and
//! clearing an undo history whose rectangles no longer mean anything.

use egui::{Rect, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};
use glam::UVec2;
use umber_core::canvassize::{self, Aspect, CanvasLimit, Chosen, LockedShape, Orientation, Sheet};
use umber_core::{Anchor, Background, Color, Document, Hsv, Unit, document};

use crate::colorpicker;
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
    /// The shape it holds. The arithmetic is [`LockedShape`]'s, in `umber-core`,
    /// because it is the same rounding [`Aspect::holds`] judges the result with
    /// and two copies of it is how a nudged edge falls off the shape the strip
    /// is claiming.
    lock_shape: LockedShape,
    /// Which set of sizes is on offer. Stored rather than derived every frame,
    /// because it is a choice — and kept honest by [`canvassize::settle`], which
    /// moves it the moment the size stops belonging to it.
    aspect: Aspect,
    /// Which sheet the size came from, while the shape is Paper.
    ///
    /// This is what lets a change of resolution re-derive the pixels. A4 at
    /// 600 dpi is a different pair of numbers from A4 at 300, and leaving the
    /// old pair behind would be a canvas calling itself A4 at half the size —
    /// which is the whole reason a paper preset carries a resolution at all.
    sheet: Option<Sheet>,
    /// Which way up that sheet is. Only paper has one; see `canvassize`.
    orientation: Orientation,
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
    /// What this machine's graphics will hold.
    ///
    /// Above the document, not part of it: it describes the computer, so it
    /// survives [`CanvasForm::open`] rather than being seeded per dialog. Set
    /// once, from `app.rs`, when the device exists.
    limit: CanvasLimit,
}

impl Default for CanvasForm {
    fn default() -> Self {
        Self::seeded(Document::default())
    }
}

impl CanvasForm {
    fn seeded(doc: Document) -> Self {
        let dpi = doc.dpi.round() as u32;
        let reading = canvassize::read(doc.size, dpi);
        Self {
            open: None,
            width: doc.size.x,
            height: doc.size.y,
            lock: false,
            lock_shape: LockedShape::of(doc.size.x, doc.size.y),
            aspect: reading.aspect,
            sheet: reading.sheet,
            orientation: reading.orientation.unwrap_or_default(),
            choice: match doc.background {
                Background::Transparent => Choice::Transparent,
                Background::Colour(c) if c == Color::WHITE => Choice::White,
                Background::Colour(c) if c == Color::BLACK => Choice::Black,
                Background::Colour(_) => Choice::Custom,
            },
            hsv: doc.background.colour().unwrap_or(Color::WHITE).to_hsv(),
            dpi,
            unit: Unit::Millimetres,
            anchor: Anchor::Centre,
            from: doc,
            limit: CanvasLimit::UNKNOWN,
        }
    }

    /// Open a dialog, seeded from `doc`.
    ///
    /// New inherits the live document wholesale — size, background and
    /// resolution — which is what makes it useful next to an imported one: the
    /// common reason to open a second tab is to try something at the same scale
    /// on the same paper.
    ///
    /// The device's bound is carried across rather than re-seeded, because it
    /// belongs to the machine and not to the document; `set_device_limit` is
    /// called once, long before any of this.
    pub fn open(&mut self, which: Dialog, doc: Document) {
        let limit = self.limit;
        *self = Self::seeded(doc);
        self.limit = limit;
        self.open = Some(which);
    }

    /// Record what the graphics device will actually hold.
    ///
    /// `max_texture_dimension_2d` is the number that decides whether a canvas
    /// can exist: past it, creating the layer array is a validation error, which
    /// is fatal. `Limits::downlevel_defaults` guarantees only 2048 and
    /// `using_resolution` raises exactly that limit from the adapter, so it has
    /// to be read from the device rather than assumed — and it is read once,
    /// here, rather than by the dialog every frame.
    pub fn set_device_limit(&mut self, max_texture_dimension_2d: u32) {
        self.limit = CanvasLimit::of_device(max_texture_dimension_2d);
    }

    fn size(&self) -> UVec2 {
        UVec2::new(self.width, self.height)
    }

    /// Take a size the model worked out, and the shape it implies for the lock.
    fn set_size(&mut self, size: UVec2) {
        let size = self.limit.clamp(size);
        self.width = size.x;
        self.height = size.y;
        // A size states a shape, so it also states what a lock would hold.
        // Leaving the old one behind would make the next nudge undo the choice.
        self.lock_shape = LockedShape::of(size.x, size.y);
    }

    /// Apply a shape the painter has just picked.
    ///
    /// The three answers are [`Chosen`]'s and every one of them is the model's;
    /// what is decided here is only that a fixed ratio also arms the lock, so
    /// nudging an edge afterwards keeps the shape that was asked for instead of
    /// dropping straight back to Custom.
    fn pick_aspect(&mut self, aspect: Aspect) {
        self.aspect = aspect;
        match canvassize::choose(aspect, self.size(), self.limit) {
            Chosen::Unchanged => self.sheet = None,
            Chosen::Size(size) => {
                self.sheet = None;
                self.set_size(size);
                // The *exact* ratio, not the one the produced size happens to
                // have. A 5000 square becomes 5000 × 2813, whose ratio is
                // 1.7775 rather than 16:9's 1.7778, and a lock holding that
                // would drive the next nudge a pixel off the shape it is
                // holding — which `canvassize::settle` would then correctly
                // report as Custom, taking the row of sizes off the screen
                // under the hand that was dragging.
                if let Some(exact) = LockedShape::of_aspect(aspect) {
                    self.lock_shape = exact;
                }
                self.lock = true;
            }
            Chosen::Sheet { sheet, dpi } => {
                self.sheet = Some(sheet);
                // Bounded by what this sheet can reach on this machine. On any
                // ordinary desktop that is `dpi` untouched; on one whose
                // textures stop at 2048 it is lower, and lower is visible in
                // the resolution field rather than silent in a canvas that is
                // not the size its button claims.
                self.dpi = dpi.min(sheet.max_dpi(self.limit));
                self.apply_sheet();
            }
        }
    }

    /// Re-derive the pixels from the sheet in hand.
    ///
    /// Called from the resolution controls and the orientation toggle, which are
    /// the two things that change what a sheet comes to.
    fn apply_sheet(&mut self) {
        if let Some(sheet) = self.sheet {
            self.set_size(sheet.pixels(self.dpi, self.orientation));
        }
    }

    /// Settle the strip after an edge has been typed.
    ///
    /// [`canvassize::settle`] is the rule: a shape that still holds the size
    /// stays, so Custom is sticky and a locked 16:9 does not flicker as an edge
    /// is nudged, and one that no longer holds it is read afresh rather than
    /// left claiming a shape the canvas has stopped being.
    fn note_typed_size(&mut self) {
        let reading = canvassize::settle(self.aspect, self.size(), self.dpi);
        self.aspect = reading.aspect;
        self.sheet = reading.sheet;
        if let Some(up) = reading.orientation {
            self.orientation = up;
        }
    }

    /// The highest resolution the size in hand can be stated at.
    ///
    /// Only a sheet bounds it: a fixed pixel size does not change when the
    /// resolution does, so there is nothing to overflow.
    fn max_dpi(&self) -> u32 {
        self.sheet
            .map(|sheet| sheet.max_dpi(self.limit))
            .unwrap_or(Document::MAX_DPI as u32)
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
    ///
    /// Bounded here as well as at every control that writes a size. The fields
    /// are ranged, the presets past the bound are not drawn and a sheet's
    /// resolution is capped, so this is the last line rather than the first —
    /// which is exactly the point: "no canvas dialog can ask for a texture the
    /// device refuses" is then a property of one function instead of the union
    /// of four call sites, and the failure it prevents is a validation error
    /// that takes the application down.
    fn document(&self) -> Document {
        let size = self.limit.clamp(self.size());
        Document::new(size.x, size.y)
            .with_background(self.background())
            .with_dpi(self.dpi as f32)
    }

    fn resizes(&self) -> bool {
        self.document().size != self.from.size
    }
}

/// Draw whichever canvas dialog is open.
pub fn show(root: &mut Ui, p: &Palette, ed: &mut Editor, out: &mut Outcome) {
    let Some(which) = ed.canvas_form.open else {
        return;
    };
    let form = &mut ed.canvas_form;
    let mut close = false;

    // How tall the body may grow before it scrolls. Taken off the window rather
    // than fixed, because the interface scale is the user's and a figure that
    // fit at 100% is off the bottom of a small screen at 200%. The reserve is
    // the modal's own margins, the heading and the button strip, all of which
    // sit outside the scrolling part — the settings dialog's shape, and for the
    // settings dialog's reason: a control that has scrolled out of reach is a
    // control that is not there, and the one that matters most here is Cancel.
    //
    // The floor is the chrome itself rather than a comfortable minimum. A
    // 200 point floor read as generous and was the one thing that could still
    // overrun the window: at 300 points it asked for 200 of body under 150 of
    // furniture. What is below the floor is a window too small to hold the
    // buttons at all, where there is nothing left to give.
    const CHROME: f32 = 150.0;
    let body_height = (root.ctx().content_rect().height() - CHROME).max(60.0);

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

            // One vertical scroll area, claiming the full width and shrinking to
            // its content vertically, so a short dialog is still a short dialog
            // and a tall one stops at the window instead of running off it.
            egui::ScrollArea::vertical()
                .max_height(body_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    // A sheet is a physical size, so its pixels follow the
                    // resolution. Reconciled once, here, rather than beside each
                    // control that can move that resolution — the quick-pick,
                    // the field, and whatever is added next to them — because
                    // that is the invariant that gets forgotten at the third
                    // call site. It is the exact identity whenever no sheet is
                    // in hand.
                    //
                    // Without it: A4 chosen at 300 and then set to 600 keeps
                    // 2480 × 3508 and is a 105 × 148 millimetre card wearing
                    // A4's name.
                    // `a_resolution_typed_into_the_dialog_moves_the_paper_it_is_
                    // holding` is the guard, and it drives the dialog rather
                    // than the form, because a guard on `apply_sheet` cannot see
                    // whether the panel calls it.
                    //
                    // **At the top and not the foot**, which is not tidiness.
                    // The resolution controls are drawn *below* the size fields
                    // and the physical readout, so reconciling after them left
                    // the frame that changed a resolution drawing the old pixels
                    // beside the new dpi — a millimetre figure wrong by the
                    // ratio rather than merely stale. Here the two always agree.
                    // What Apply reads is safe either way: it is a click, and a
                    // click lands on one widget per frame.
                    form.apply_sheet();

                    // Both dialogs, and that is the point of them sharing a
                    // form. The shape a canvas is has nothing to do with whether
                    // it exists yet, and offering the sizes to only one of the
                    // two is exactly how the pair starts to drift.
                    aspect_row(ui, p, form);
                    ui.add_space(12.0);
                    if size_choices(ui, p, form) {
                        ui.add_space(12.0);
                    }

                    size_fields(ui, p, form);
                    ui.add_space(12.0);
                    resolution(ui, p, form);
                    ui.add_space(12.0);
                    background_fields(ui, p, form);

                    // Only when there is something to place. On a New document
                    // there is nothing to anchor, and on an unchanged size the
                    // control would be a live knob that does nothing.
                    if which == Dialog::Settings && form.resizes() {
                        ui.add_space(12.0);
                        anchor_field(ui, p, form);
                    }
                });

            ui.add_space(16.0);
            // Inside a `horizontal`. A bare `right_to_left` takes the whole of
            // the remaining height of the `Ui` it is in, because the align is
            // the cross axis, so a short modal stretches to the height of the
            // window with its buttons floating in the middle.
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

/// A dim line under a control, for something the painter should know rather
/// than act on.
fn footnote(ui: &mut Ui, p: &Palette, line: &str) {
    ui.label(egui::RichText::new(line).size(text::TINY).color(p.text_dim));
}

/// The shape strip: the first control, because "what shape" is the question
/// somebody answers before "how many pixels".
fn aspect_row(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) {
    caption(ui, p, "Aspect ratio");
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        for aspect in Aspect::ALL {
            let selected = form.aspect == aspect;
            // A press on the shape already in front does nothing at all. Not
            // tidiness: picking Paper states a resolution, so re-picking it
            // would drag a document already set up at 600 dpi back to 300.
            if tabs::button(ui, p, aspect.label(), selected) && !selected {
                form.pick_aspect(aspect);
            }
        }
    });
}

/// The sizes under the shape in hand. Answers whether it drew anything, because
/// Custom deliberately offers none and a caption over an empty row would be a
/// control that is not there.
fn size_choices(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) -> bool {
    match form.aspect {
        Aspect::Custom => false,
        Aspect::Paper => {
            sheet_choices(ui, p, form);
            true
        }
        aspect => {
            caption(ui, p, "Size");
            ui.add_space(6.0);
            // Which one is lit is the model's answer, not a comparison written
            // out again here. `settle` keeps the shape while it holds the size,
            // so this is `read` restricted to the row actually on screen.
            let lit = canvassize::settle(aspect, form.size(), form.dpi).preset;
            ui.horizontal_wrapped(|ui| {
                for (i, preset) in aspect.presets().iter().enumerate() {
                    // A size this machine cannot hold is not drawn. A control
                    // that lights up promising a canvas the device will refuse
                    // is worse than one that is simply absent, and the sentence
                    // under the size fields says why the row is short.
                    if !form.limit.permits(preset.size()) {
                        continue;
                    }
                    if tabs::button(ui, p, preset.label, lit == Some(i)) {
                        form.sheet = None;
                        form.set_size(preset.size());
                    }
                }
            });
            true
        }
    }
}

/// Paper: the sheets, and which way up they go.
///
/// The resolution is not here. It is the ordinary Resolution control below,
/// which is the same figure a screen-sized canvas carries — a second dpi picker
/// beside the sheets would be two spellings of one question, and this one has to
/// stay in step with the field anyway.
fn sheet_choices(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) {
    caption(ui, p, "Paper size");
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        for sheet in Sheet::ALL {
            let selected = form.sheet == Some(sheet);
            if tabs::button(ui, p, sheet.label(), selected) {
                form.sheet = Some(sheet);
                // A sheet too large at the resolution in force is not refused:
                // it is offered at the highest resolution that does fit, which
                // the field below then shows. Refusing would take A3 off a
                // machine that can hold it perfectly well at 300.
                form.dpi = form.dpi.min(sheet.max_dpi(form.limit));
                form.apply_sheet();
            }
        }
    });

    ui.add_space(8.0);
    let mut orientation = form.orientation;
    if widgets::segmented(
        ui,
        p,
        &mut orientation,
        &Orientation::ALL.map(|o| (o, o.label())),
    ) {
        form.orientation = orientation;
        form.apply_sheet();
    }
}

fn size_fields(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) {
    caption(ui, p, "Canvas size");
    ui.add_space(6.0);

    let edges = form.limit.edges();
    let mut typed = false;
    if number_row(ui, p, "Width", &mut form.width, "px", edges.clone()) {
        if form.lock {
            form.height = form.lock_shape.height_for(form.width, form.limit);
        }
        typed = true;
    }
    if number_row(ui, p, "Height", &mut form.height, "px", edges) {
        if form.lock {
            form.width = form.lock_shape.width_for(form.height, form.limit);
        }
        typed = true;
    }
    if typed {
        form.note_typed_size();
    }

    ui.add_space(4.0);
    let was = form.lock;
    widgets::toggle_row(ui, p, "Lock aspect ratio", &mut form.lock);
    if form.lock && !was {
        // Captured on the way in, so the lock holds the shape the canvas has
        // right now rather than one it had earlier.
        form.lock_shape = LockedShape::of(form.width, form.height);
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

    // What this machine will not do, and what a very large canvas will cost.
    // Both are the model's sentences: one is a property of the graphics and the
    // other of the size, and neither is a number to re-derive here.
    if let Some(line) = form.limit.notice() {
        ui.add_space(6.0);
        footnote(ui, p, &line);
    }
    if let Some(line) = canvassize::memory_note(form.size()) {
        ui.add_space(6.0);
        footnote(ui, p, &line);
    }
}

fn resolution(ui: &mut Ui, p: &Palette, form: &mut CanvasForm) {
    caption(ui, p, "Resolution");
    ui.add_space(6.0);

    // The four figures people actually pick, above the field that takes any of
    // them. The same relation the sizes above have to the width and height: a
    // quick-pick of one value, not a second control for it.
    //
    // Filtered by what the sheet in hand can reach. On any ordinary machine
    // that is all four; on one whose textures stop at 8192, A3 cannot be stated
    // at 600, so the cell is gone rather than lit over a canvas that would come
    // back clamped.
    //
    // A prefix of the table rather than a filtered copy of it, which is what
    // makes this a borrow of a `const` and not an allocation on a frame that is
    // drawn continuously. The table is ascending, so what survives `<= top` is
    // always a prefix; `every_resolution_is_labelled_with_itself` pins the
    // order this rests on.
    let top = form.max_dpi();
    let kept = canvassize::DPI_CHOICES
        .iter()
        .position(|&(dpi, _)| dpi > top)
        .unwrap_or(canvassize::DPI_CHOICES.len());
    let offered = &canvassize::DPI_CHOICES[..kept];
    if !offered.is_empty() {
        let mut dpi = form.dpi;
        if widgets::segmented(ui, p, &mut dpi, offered) {
            form.dpi = dpi;
        }
        ui.add_space(6.0);
    }

    // Neither control re-derives the size itself: the one reconciliation at the
    // foot of `show` does, for both of them and for whatever is added beside
    // them.
    number_row(
        ui,
        p,
        "Pixels per inch",
        &mut form.dpi,
        "dpi",
        Document::MIN_DPI as u32..=top,
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
        // way — and nothing of the wheel's, which this mode does not draw and
        // which belongs to the panel behind this dialog.
        colorpicker::show_sliders(ui, p, &mut form.hsv);
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
            // rectangles a stroke damaged and a selection is another rectangle,
            // and a rectangle in the old geometry means different pixels in the
            // new one — so both go, exactly as the history does when a layer is
            // deleted. The text records go too, and `Editor::apply_canvas`
            // raises its own notice for that afterwards: this one is the
            // warning before, which is the half that lets somebody stop.
            ui.label(
                egui::RichText::new(
                    "Resizing clears the undo history and drops the selection: both \
                     are rectangles of this canvas, which mean nothing on a different \
                     one. Text on this document becomes ordinary paint.",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A form as the dialogs meet it: seeded, with a device that holds
    /// everything the format allows.
    fn form_of(doc: Document) -> CanvasForm {
        CanvasForm::seeded(doc)
    }

    // The lock's own arithmetic is `LockedShape`'s and is guarded there:
    // `a_locked_edge_stays_on_the_shape_the_strip_is_claiming` sweeps every
    // edge, and `a_lock_can_never_ask_for_a_canvas_that_cannot_exist` sweeps
    // the bound. What this file has left to prove is that the dialog *uses*
    // it, which is `a_typed_edge_under_a_lock_keeps_the_shape_the_strip_claims`
    // below.

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
            let form = form_of(doc);
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
        let mut form = form_of(Document::default().with_background(Background::opaque(mixed)));
        assert_eq!(form.choice, Choice::Custom);

        form.choice = Choice::White;
        assert_eq!(form.background(), Background::WHITE);
        form.choice = Choice::Custom;
        assert_eq!(
            form.background().colour().map(Color::to_srgb_u8),
            Some(mixed.to_srgb_u8()),
        );
    }

    // ---------------------------------------------------------------- shapes

    #[test]
    fn a_form_opens_on_the_shape_its_document_already_is() {
        let wide = form_of(Document::new(3840, 2160));
        assert_eq!(wide.aspect, Aspect::Wide);
        assert_eq!(wide.sheet, None);

        let a4 = form_of(Document::new(2480, 3508).with_dpi(300.0));
        assert_eq!(a4.aspect, Aspect::Paper);
        assert_eq!(a4.sheet, Some(Sheet::A4));
        assert_eq!(a4.orientation, Orientation::Portrait);

        let odd = form_of(Document::new(1234, 567));
        assert_eq!(odd.aspect, Aspect::Custom);
        assert_eq!(odd.sheet, None);
    }

    #[test]
    fn choosing_paper_states_a_resolution_and_the_pixels_follow_it() {
        // The one thing a paper preset must get right, and the one it is
        // easiest to get wrong: the resolution has to reach the document, and
        // changing it afterwards has to move the pixels.
        let mut form = form_of(Document::new(1920, 1080));
        form.pick_aspect(Aspect::Paper);
        assert_eq!(form.sheet, Some(Sheet::A4));
        assert_eq!(form.dpi, canvassize::PAPER_DPI);
        assert_eq!(form.size(), UVec2::new(2480, 3508));
        assert_eq!(form.document().dpi, 300.0);

        form.dpi = 600;
        form.apply_sheet();
        assert_eq!(form.size(), UVec2::new(4961, 7016));
        assert_eq!(form.document().dpi, 600.0);
        assert_eq!(form.document().size, UVec2::new(4961, 7016));

        // And at 72 it is the PostScript page, which is the same rule seen from
        // the other end.
        form.dpi = 72;
        form.apply_sheet();
        assert_eq!(form.size(), UVec2::new(595, 842));
    }

    #[test]
    fn a_sheet_turns_over_without_changing_which_sheet_it_is() {
        let mut form = form_of(Document::new(100, 100));
        form.pick_aspect(Aspect::Paper);
        form.orientation = Orientation::Landscape;
        form.apply_sheet();
        assert_eq!(form.sheet, Some(Sheet::A4));
        assert_eq!(form.size(), UVec2::new(3508, 2480));
        // And it still reads as itself, so the row stays lit after a reopen.
        let reopened = form_of(form.document());
        assert_eq!(reopened.aspect, Aspect::Paper);
        assert_eq!(reopened.sheet, Some(Sheet::A4));
        assert_eq!(reopened.orientation, Orientation::Landscape);
    }

    #[test]
    fn typing_a_size_that_leaves_the_shape_moves_the_strip_and_drops_the_sheet() {
        let mut form = form_of(Document::new(2480, 3508).with_dpi(300.0));
        assert_eq!(form.sheet, Some(Sheet::A4));
        form.width = 2481;
        form.note_typed_size();
        assert_eq!(form.aspect, Aspect::Custom);
        assert_eq!(
            form.sheet, None,
            "a sheet left standing would re-derive itself \
             over the size that was typed the next time the resolution moved"
        );
    }

    #[test]
    fn a_typed_edge_under_a_lock_keeps_the_shape_the_strip_claims() {
        // The synergy that makes the strip honest while an edge is dragged: the
        // shape arms the lock **at the exact ratio**, the lock holds it, and
        // `settle` therefore finds the shape still holding.
        //
        // Swept rather than tried at one width. 1600 was the whole of the first
        // version of this and it is the one number that proves least: 1600 x 9
        // is exactly 14400, so nothing rounds. The widths that decide it are
        // those landing on a half, and the ones that would have failed under the
        // old `f32` lock are those where a *rounded* stored ratio drifts — which
        // is why the shape is armed from `LockedShape::of_aspect` and not from
        // the 1000 x 563 the choice produced.
        let mut form = form_of(Document::new(1000, 1000));
        form.pick_aspect(Aspect::Wide);
        assert!(form.lock, "a shape arms the lock");
        assert_eq!(form.size(), UVec2::new(1000, 563));

        for width in 1000..1200 {
            form.width = width;
            form.height = form.lock_shape.height_for(form.width, form.limit);
            form.note_typed_size();
            assert_eq!(
                form.aspect,
                Aspect::Wide,
                "{width} x {} left 16:9",
                form.height
            );
        }
    }

    #[test]
    fn custom_moves_nothing_and_keeps_the_strip() {
        let mut form = form_of(Document::new(2480, 3508).with_dpi(300.0));
        form.pick_aspect(Aspect::Custom);
        assert_eq!(form.size(), UVec2::new(2480, 3508));
        assert_eq!(form.sheet, None);
        // Sticky: a size that would otherwise file itself does not move it.
        form.width = 1920;
        form.height = 1080;
        form.note_typed_size();
        assert_eq!(form.aspect, Aspect::Custom);
    }

    // ----------------------------------------------------------------- bounds

    #[test]
    fn no_route_into_the_size_can_ask_for_a_canvas_the_device_refuses() {
        // Every route the *form* offers, driven against a device that stops at
        // 4096: the shapes, the sheets at every offered resolution, and the
        // sizes that are still on offer.
        //
        // It is named for the form and not for the controls, because it
        // restates the panel's own "is this size permitted" filter inside the
        // loop below and therefore cannot see that filter removed. What can is
        // `the_dialog_does_not_draw_a_size_this_machine_cannot_hold`, which
        // reads the labels egui actually drew.
        let limit = CanvasLimit::of_device(4096);
        let mut form = form_of(Document::new(4096, 4096));
        form.limit = limit;

        for aspect in Aspect::ALL {
            form.pick_aspect(aspect);
            assert!(
                limit.permits(form.document().size),
                "{} produced {}",
                aspect.label(),
                form.document().size
            );
            for preset in aspect.presets() {
                if !limit.permits(preset.size()) {
                    continue;
                }
                form.set_size(preset.size());
                assert!(limit.permits(form.document().size), "{}", preset.label);
            }
        }

        for sheet in Sheet::ALL {
            form.pick_aspect(Aspect::Paper);
            form.sheet = Some(sheet);
            form.dpi = form.dpi.min(sheet.max_dpi(limit));
            form.apply_sheet();
            assert!(
                limit.permits(form.document().size),
                "{} at {} dpi produced {}",
                sheet.label(),
                form.dpi,
                form.document().size
            );
            // And it is still the sheet it says it is, rather than a clamped
            // rectangle wearing its name.
            assert_eq!(form.size(), sheet.pixels(form.dpi, form.orientation));
        }
    }

    #[test]
    fn a_sheet_bounds_the_resolution_and_a_pixel_size_does_not() {
        // The *reading* the resolution controls are built from. What the
        // controls then do with it is
        // `the_dialog_does_not_offer_a_resolution_the_sheet_in_hand_cannot_reach`
        // — this one deliberately claims nothing about the panel, because an
        // earlier version of it did and could not see the field's range widened
        // back to `MAX_DPI`.
        //
        // A3 at 2400 dpi is 28063 x 39685.
        let mut form = form_of(Document::default());
        form.limit = CanvasLimit::of_device(8192);
        form.pick_aspect(Aspect::Paper);
        form.sheet = Some(Sheet::A3);
        let top = form.max_dpi();
        assert!(top < Document::MAX_DPI as u32, "A3 should be bounded here");
        form.dpi = top;
        form.apply_sheet();
        assert!(form.limit.permits(form.size()), "{}", form.size());

        // With no sheet in hand there is nothing to overflow, so the field runs
        // the whole way: a pixel size does not change when the resolution does.
        form.pick_aspect(Aspect::Square);
        assert_eq!(form.max_dpi(), Document::MAX_DPI as u32);
    }

    // ------------------------------------------------------------- the drawing

    /// The modal's id, which is also its `Area`'s, so its rectangle can be read
    /// back out of egui's memory after a pass.
    fn modal_id() -> egui::Id {
        egui::Id::new("canvas-settings")
    }

    /// Draw whichever dialog is open into a window `height` points tall, and
    /// answer how tall the modal came out along with every word it drew.
    ///
    /// The labels are what make a *panel* guard possible at all. Which sizes
    /// the dialog offers is a filter inside `show`, and a test that restated
    /// the filter would only ever agree with itself — reading the text back out
    /// of egui's own output is the one way to ask what was actually put on the
    /// screen.
    fn drawn(ed: &mut Editor, height: f32) -> (f32, Vec<String>) {
        use crate::theme::{Palette, ThemeKind};
        use egui::{Rect, pos2, vec2};

        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, height))),
            ..Default::default()
        };
        // Twice: the first pass through a fresh context builds the font atlas,
        // and a modal laid out against a half-built one is not the height it
        // settles at. The same reason `panels.rs`'s measurements run twice.
        let mut words = Vec::new();
        for _ in 0..2 {
            let mut out = Outcome::default();
            let output = ctx.run_ui(input.clone(), |ui| {
                show(ui, &palette, ed, &mut out);
            });
            words.clear();
            collect_text(&output.shapes, &mut words);
        }
        let tall = ctx
            .memory(|m| m.area_rect(modal_id()))
            .map(|r| r.height())
            .unwrap_or_default();
        (tall, words)
    }

    fn collect_text(shapes: &[egui::epaint::ClippedShape], into: &mut Vec<String>) {
        fn walk(shape: &egui::Shape, into: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => into.push(text.galley.text().to_owned()),
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk(shape, into);
                    }
                }
                _ => {}
            }
        }
        for clipped in shapes {
            walk(&clipped.shape, into);
        }
    }

    fn drawn_height(ed: &mut Editor, height: f32) -> f32 {
        drawn(ed, height).0
    }

    #[test]
    fn every_shape_draws_in_both_dialogs() {
        // The cheapest thing this can be: a pass over each of the seven rows in
        // each of the two dialogs, which is enough to catch a `segmented` handed
        // an empty slice, a prefix slice off the end of the resolution table, or
        // a row that decided to divide by a size nobody has set yet. None of it
        // needs a window.
        for which in [Dialog::New, Dialog::Settings] {
            for aspect in Aspect::ALL {
                let mut ed = Editor::default();
                ed.canvas_form.open(which, Document::new(1000, 1000));
                ed.canvas_form.pick_aspect(aspect);
                let height = drawn_height(&mut ed, 900.0);
                assert!(
                    height > 0.0,
                    "{which:?} on {} drew nothing at all",
                    aspect.label()
                );
            }
        }
    }

    #[test]
    fn a_resolution_typed_into_the_dialog_moves_the_paper_it_is_holding() {
        // The panel half of the paper rule, and it has to be the panel: a guard
        // on `apply_sheet` cannot see whether the dialog calls it, which is
        // exactly the shape that let a reverted call site leave 1,485 tests
        // green elsewhere in this codebase.
        //
        // Setting the field is all a resolution control does. Everything after
        // that belongs to `show`.
        let mut ed = Editor::default();
        ed.canvas_form.open(Dialog::New, Document::new(1000, 1000));
        ed.canvas_form.pick_aspect(Aspect::Paper);
        assert_eq!(ed.canvas_form.size(), UVec2::new(2480, 3508));

        ed.canvas_form.dpi = 600;
        let _ = drawn_height(&mut ed, 900.0);
        assert_eq!(
            ed.canvas_form.size(),
            UVec2::new(4961, 7016),
            "the dialog kept A4's 300 dpi pixels at 600 dpi"
        );
        assert_eq!(ed.canvas_form.document().dpi, 600.0);

        // And a canvas with no sheet behind it is untouched by the same pass,
        // which is what makes the reconciliation safe to run unconditionally.
        ed.canvas_form.pick_aspect(Aspect::Wide);
        let before = ed.canvas_form.size();
        ed.canvas_form.dpi = 72;
        let _ = drawn_height(&mut ed, 900.0);
        assert_eq!(ed.canvas_form.size(), before);
    }

    #[test]
    fn the_dialog_does_not_draw_a_size_this_machine_cannot_hold() {
        // The panel half of the bound, and it has to read what was *drawn*. The
        // model-level version of this test restated the dialog's own filter
        // inside the test and could not see it removed at all: the button would
        // still be there, clicking it would still hand back 4096 square because
        // `set_size` clamps, and every assertion would still pass.
        let mut ed = Editor::default();
        ed.canvas_form.open(Dialog::New, Document::new(1000, 1000));
        ed.canvas_form.pick_aspect(Aspect::Square);

        let (_, roomy) = drawn(&mut ed, 900.0);
        assert!(roomy.iter().any(|w| w == "16384"), "{roomy:?}");
        assert!(roomy.iter().any(|w| w == "12000"), "{roomy:?}");
        // And the sentence is absent when nothing is missing, or it would be a
        // notice every machine gets about nothing.
        assert!(!roomy.iter().any(|w| w.contains("pixels on a side")));

        ed.canvas_form.set_device_limit(4096);
        let (_, bounded) = drawn(&mut ed, 900.0);
        assert!(!bounded.iter().any(|w| w == "16384"), "{bounded:?}");
        assert!(!bounded.iter().any(|w| w == "12000"), "{bounded:?}");
        assert!(!bounded.iter().any(|w| w == "5000"), "{bounded:?}");
        assert!(
            bounded.iter().any(|w| w == "2000"),
            "the sizes that do fit must still be there: {bounded:?}"
        );
        assert!(
            bounded.iter().any(|w| w.contains("4096 pixels on a side")),
            "nothing said why the row is short: {bounded:?}"
        );
    }

    #[test]
    fn the_dialog_does_not_offer_a_resolution_the_sheet_in_hand_cannot_reach() {
        // The other half, and the same trap: the previous version of this test
        // called `form.max_dpi()` and asserted about the number, which says
        // nothing about whether the strip is filtered by it or the field is
        // ranged by it. Widening the field back to `MAX_DPI` left it green.
        //
        // A3 at 600 dpi is 7016 x 9921, so on a machine that stops at 8192 the
        // 600 cell has to go. It has to stay everywhere else, including for A3
        // on an ordinary machine, or this would pass by simply never offering
        // 600 at all.
        let mut ed = Editor::default();
        ed.canvas_form.open(Dialog::New, Document::new(1000, 1000));
        ed.canvas_form.pick_aspect(Aspect::Paper);
        ed.canvas_form.sheet = Some(Sheet::A3);
        ed.canvas_form.apply_sheet();

        let (_, roomy) = drawn(&mut ed, 900.0);
        assert!(roomy.iter().any(|w| w == "600"), "{roomy:?}");

        ed.canvas_form.set_device_limit(8192);
        let (_, bounded) = drawn(&mut ed, 900.0);
        assert!(!bounded.iter().any(|w| w == "600"), "{bounded:?}");
        assert!(bounded.iter().any(|w| w == "300"), "{bounded:?}");
        assert!(
            ed.canvas_form.limit.permits(ed.canvas_form.size()),
            "{}",
            ed.canvas_form.size()
        );
    }

    #[test]
    fn the_dialog_stays_inside_a_short_window() {
        // What the scroll area is for. Paper is the tallest arrangement — a row
        // of sheets and a way-up toggle on top of everything else — and Canvas
        // settings adds the anchor block on top of that, so this is the worst
        // case the dialog has. Without the scroll area the modal simply grows
        // and its buttons go off the bottom of the screen, taking Cancel with
        // them.
        let mut ed = Editor::default();
        ed.canvas_form
            .open(Dialog::Settings, Document::new(1000, 1000));
        ed.canvas_form.pick_aspect(Aspect::Paper);
        assert!(ed.canvas_form.resizes(), "the anchor block has to be drawn");

        let tall = drawn_height(&mut ed, 1400.0);
        let short = drawn_height(&mut ed, 460.0);
        assert!(
            short <= 460.0,
            "the modal was {short} points tall in a 460 point window"
        );
        // And the two readings have to differ, or this is measuring a dialog
        // that was short enough all along and would pass under any rule.
        assert!(
            tall > short,
            "the dialog is {tall} points either way, so nothing here was tested"
        );
    }

    #[test]
    fn the_device_bound_survives_a_dialog_being_reopened() {
        // It describes the machine, not the document, and `open` replaces the
        // whole form. Losing it here would put every refused preset back on the
        // second time somebody opened the dialog.
        let mut form = CanvasForm::default();
        form.set_device_limit(4096);
        form.open(Dialog::New, Document::new(100, 100));
        assert_eq!(form.limit.max_edge(), 4096);
        form.open(Dialog::Settings, Document::new(200, 200));
        assert_eq!(form.limit.max_edge(), 4096);
    }
}
