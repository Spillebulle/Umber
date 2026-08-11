//! Export: one flattened image, in one of the five formats
//! [`umber_core::export`] writes.
//!
//! The shape is [`crate::canvasdlg`]'s — a form, a modal drawn from
//! [`crate::ui::draw`] rather than from a panel body, and an answer the caller
//! carries out — for the same reasons. The layout can hide a panel, and a modal
//! that goes with one cannot then be shut or reopened; and nothing here may
//! reach the GPU, because the pixels come off it through
//! `CanvasRenderer::export_rgba` and the file dialog that follows this one
//! blocks the whole application.
//!
//! What the dialog is *for* is saying what will be lost before it happens.
//! Three of the five formats hold no transparency and one of them holds 256
//! colours, and an export that quietly flattened a drawing onto black — the
//! classic version of this bug — would be discovered by the artist and not by
//! Umber. So the losses are listed, in the document's own terms:
//! [`umber_core::export::losses`] takes whether *this* document has any
//! transparency to lose, and an opaque one is not warned about an alpha channel
//! it never had.
//!
//! Every rule about the formats themselves — what each can carry, what it is
//! called, which extension names it, what the file should be called — is in
//! `umber-core`, with tests. This file is the drawing.

use egui::Ui;
use umber_core::export::{self, ExportFormat, ExportOptions};
use umber_core::{Background, Color, Hsv};

use crate::colorpicker;
use crate::editor::Editor;
use crate::tabs;
use crate::theme::{Palette, text};
use crate::widgets;

/// The matte colours worth a button, plus one the painter mixes.
///
/// A choice beside the colour rather than a bare colour, for the reason
/// `canvasdlg`'s background has one: switching to White and back must not throw
/// away what was being mixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Matte {
    White,
    Black,
    Custom,
}

/// State of the export dialog.
///
/// Application state, above `editor.rs`'s `--- documents ---` line: it is a
/// dialog, and it is deliberately *not* reset per document. Somebody exporting
/// a set of frames as JPEG at 80 wants the next one to be JPEG at 80.
#[derive(Clone, Debug)]
pub struct ExportForm {
    pub open: bool,
    format: ExportFormat,
    /// 1–100. Kept as a float because the slider is one, and rounded on the way
    /// out — a quality that redisplayed as 89.6 would be a control that looks
    /// broken.
    quality: f32,
    matte: Matte,
    /// The custom matte's own state, HSV for the reason `Editor::hsv` is: hue
    /// is undefined for greys, so deriving it from the colour each frame means
    /// dragging value to black silently resets it to red.
    hsv: Hsv,
}

impl Default for ExportForm {
    fn default() -> Self {
        Self {
            open: false,
            format: ExportFormat::Png,
            quality: 90.0,
            matte: Matte::White,
            hsv: Color::WHITE.to_hsv(),
        }
    }
}

impl ExportForm {
    fn matte_colour(&self) -> [u8; 3] {
        let c = match self.matte {
            Matte::White => Color::WHITE,
            Matte::Black => Color::BLACK,
            Matte::Custom => self.hsv.to_color(1.0),
        };
        let [r, g, b, _] = c.to_srgb_u8();
        [r, g, b]
    }

    fn options(&self) -> ExportOptions {
        ExportOptions {
            format: self.format,
            quality: self.quality.round().clamp(1.0, 100.0) as u8,
            matte: self.matte_colour(),
        }
    }
}

/// What the dialog asked for this frame.
#[derive(Default, Clone, Copy)]
pub struct Outcome {
    /// Encode the flattened document like this. The caller asks for a file and
    /// does the work: the file dialog blocks, and the pixels are the GPU's.
    pub export: Option<ExportOptions>,
}

/// Draw the export dialog, if it is open.
pub fn show(root: &mut Ui, p: &Palette, ed: &mut Editor, out: &mut Outcome) {
    if !ed.export_form.open {
        return;
    }
    // Whether there is any transparency to lose. The background composites
    // *under* the stack inside the export pass, so an opaque background means
    // the exported image is opaque whatever the layers hold — which is the
    // whole reason this is one field and not a scan of the pixels.
    let transparent = ed.doc.background == Background::Transparent;
    let size = ed.doc.size;
    let form = &mut ed.export_form;
    let mut close = false;

    let modal = egui::Modal::new(egui::Id::new("export-image"))
        .frame(tabs::dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(360.0);
            heading(ui, p, "Export image");
            ui.add_space(4.0);
            caption(
                ui,
                p,
                &format!(
                    "{} × {} pixels, everything visible flattened into one image. \
                     Save keeps the layers.",
                    size.x, size.y
                ),
            );
            ui.add_space(12.0);

            format_field(ui, p, form);
            ui.add_space(12.0);

            // Only for the format that has one. A quality rail beside a PNG
            // would be a live control that changes no byte.
            if form.format.has_quality() {
                widgets::slider_row(
                    ui,
                    p,
                    "Quality",
                    &mut form.quality,
                    1.0..=100.0,
                    false,
                    |v| format!("{v:.0}"),
                );
                ui.add_space(12.0);
            }

            if export::needs_matte(form.format, transparent) {
                matte_field(ui, p, form);
                ui.add_space(12.0);
            }

            losses(ui, p, form.format, transparent);

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if tabs::button(ui, p, "Cancel", false) {
                    close = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if tabs::button(ui, p, "Export…", true) {
                        out.export = Some(form.options());
                        close = true;
                    }
                });
            });
        });

    // The trailing ellipsis on the button above is the promise this keeps:
    // nothing is written yet. Escape and a click outside are "not now".
    if close || modal.should_close() {
        ed.export_form.open = false;
    }
}

fn format_field(ui: &mut Ui, p: &Palette, form: &mut ExportForm) {
    caption(ui, p, "Format");
    ui.add_space(6.0);
    // Wrapped buttons rather than a segmented control: five labels do not fit
    // one 360-point row, and `segmented` divides its width equally, so "JPEG"
    // and "GIF" would get the same space and the strip would be as wide as the
    // longest label times five.
    ui.horizontal_wrapped(|ui| {
        for format in ExportFormat::ALL {
            if tabs::button(ui, p, format.label(), form.format == format) {
                form.format = format;
            }
        }
    });
    ui.add_space(6.0);
    caption(ui, p, form.format.note());
}

fn matte_field(ui: &mut Ui, p: &Palette, form: &mut ExportForm) {
    caption(ui, p, "Transparency becomes");
    ui.add_space(6.0);
    let mut matte = form.matte;
    if widgets::segmented(
        ui,
        p,
        &mut matte,
        &[
            (Matte::White, "White"),
            (Matte::Black, "Black"),
            (Matte::Custom, "Custom"),
        ],
    ) {
        form.matte = matte;
    }
    if form.matte == Matte::Custom {
        ui.add_space(8.0);
        // The Colour panel's own slider mode, so the two mix a colour the same
        // way — and nothing of the wheel's, which this mode does not draw and
        // which belongs to the panel behind this dialog.
        colorpicker::show_sliders(ui, p, &mut form.hsv);
    }
}

/// What this format costs this document, said before it happens.
fn losses(ui: &mut Ui, p: &Palette, format: ExportFormat, transparent: bool) {
    let losses = export::losses(format, transparent);
    if losses.is_empty() {
        // The other half of the warning, and worth saying: a format that keeps
        // everything should say so, or an artist reading only the loud cases
        // learns nothing about the quiet ones.
        ui.label(
            egui::RichText::new("Nothing in this document is lost by this format.")
                .size(text::TINY)
                .color(p.text_dim),
        );
        return;
    }
    for loss in losses {
        ui.label(
            egui::RichText::new(loss.to_string())
                .size(text::TINY)
                .color(p.warning),
        );
        ui.add_space(4.0);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_states_exactly_what_the_encoder_is_given() {
        let mut form = ExportForm::default();
        assert_eq!(form.options(), ExportOptions::default());

        form.format = ExportFormat::Jpeg;
        // A rail is a float and a quality is not; the encoder must never be
        // handed 89.6.
        form.quality = 89.6;
        assert_eq!(form.options().quality, 90);

        form.matte = Matte::Black;
        assert_eq!(form.options().matte, [0, 0, 0]);
        form.matte = Matte::Custom;
        form.hsv = Color::from_srgb_u8(200, 120, 40, 255).to_hsv();
        assert_eq!(form.options().matte, [200, 120, 40]);
        // Switching away and back keeps the mixed colour, which is the reason
        // the choice sits beside it rather than being read off it.
        form.matte = Matte::White;
        assert_eq!(form.options().matte, [255, 255, 255]);
        form.matte = Matte::Custom;
        assert_eq!(form.options().matte, [200, 120, 40]);
    }

    #[test]
    fn a_quality_that_left_the_rail_is_still_one_the_encoder_accepts() {
        let mut form = ExportForm::default();
        for (value, expected) in [(0.0, 1), (0.4, 1), (100.6, 100), (1e9, 100)] {
            form.quality = value;
            assert_eq!(form.options().quality, expected, "{value}");
        }
    }

    // ------------------------------------------------------------- the drawing
    //
    // Everything above drives `ExportForm::options()`, which is the value the
    // encoder is handed. **Nothing drove `show` at all**, and `show` is where
    // this module's entire reason for existing lives: the losses are named
    // *before* the write, which is the promise the file's own docs open with.
    // Three mutations, all of which left every one of the 808 tests in this
    // crate green:
    //
    //   deleting `losses(ui, p, form.format, transparent);`
    //   `if true` in place of the `needs_matte` gate
    //   `let transparent = true;`
    //
    // So the guards below read the words egui actually drew, the idiom
    // `canvasdlg` already keeps: a filter restated inside a test can only ever
    // agree with itself, and `FullOutput::shapes` carries every galley.

    /// Draw the dialog over a document with `background`, and answer every word
    /// it put on the screen.
    fn drawn(background: Background, format: ExportFormat) -> Vec<String> {
        use crate::editor::Editor;
        use crate::theme::ThemeKind;
        use egui::{Rect, pos2, vec2};

        let mut ed = Editor::default();
        ed.doc.background = background;
        ed.export_form.open = true;
        ed.export_form.format = format;

        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 900.0))),
            ..Default::default()
        };
        // Twice, for `canvasdlg::drawn`'s reason: the first pass through a
        // fresh context builds the font atlas, and a modal laid out against a
        // half-built one has not settled.
        let mut words = Vec::new();
        for _ in 0..2 {
            let mut out = Outcome::default();
            let output = ctx.run_ui(input.clone(), |ui| {
                show(ui, &palette, &mut ed, &mut out);
            });
            words = crate::paneltest::text_of(&output.shapes);
        }
        words
    }

    /// Whether `words` holds a line that is `loss`'s own sentence.
    ///
    /// Compared against `ExportLoss`'s `Display` rather than against a copy of
    /// its text, so a reworded warning moves this test with it instead of
    /// leaving a guard pinned to a sentence nobody says any more.
    ///
    /// **The whitespace normalisation is insurance, not a workaround**, and the
    /// distinction is worth drawing because the first version of this comment
    /// got it wrong. A wrapped `ui.label` produces *one* `Shape::Text` carrying
    /// one `Galley`, and `Galley::text()` hands back the layout job's source
    /// string with no wrap breaks in it — so a bare `contains` would work
    /// today. What the normalisation buys is that the comparison does not care
    /// how the sentence is broken up, whichever way egui later decides to
    /// break it. Stating the mechanism that is not the real one is the failure
    /// `Pen::at`'s note records; better to say what it actually buys.
    fn names(words: &[String], loss: export::ExportLoss) -> bool {
        let flat = words
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let wanted = loss.to_string();
        let wanted = wanted.split_whitespace().collect::<Vec<_>>().join(" ");
        flat.contains(&wanted)
    }

    /// **Every loss is named before the write**, which is what this dialog is
    /// for. Driven over the formats and both backgrounds rather than over one
    /// case, because the interesting half of the rule is what is *not* said.
    #[test]
    fn the_dialog_names_every_loss_the_format_costs_this_document() {
        let mut checked = 0;
        for format in ExportFormat::ALL {
            for background in [Background::Transparent, Background::opaque(Color::WHITE)] {
                let transparent = background == Background::Transparent;
                let words = drawn(background, format);
                for loss in export::losses(format, transparent) {
                    assert!(
                        names(&words, loss),
                        "{format:?} on a {} document does not say {loss:?} on \
                         screen, so the artist finds out by looking at the file",
                        if transparent { "transparent" } else { "white" },
                    );
                    checked += 1;
                }
            }
        }
        // **The loop above asserts nothing when `losses` is empty**, so without
        // this the whole test passes by never running its own body — which is
        // the vacuity it was written to fix, one level up. Seven is what the
        // ten pairs actually produce: JPEG loses alpha and detail on a
        // transparent document and detail alone on a white one, GIF the same
        // with its palette, and BMP loses alpha on a transparent document only.
        // PNG and TIFF lose nothing either way, which is the case the test
        // below is about.
        assert_eq!(
            checked, 7,
            "the formats stopped costing what this test is about, so it is no \
             longer driving the sentences it claims to"
        );
    }

    /// The other half, and the one a test built only out of the first would
    /// miss: a document with nothing to lose is told so, and is **not** warned
    /// about an alpha channel it never had.
    ///
    /// `losses` takes the document's own transparency for exactly this reason,
    /// so a guard that never varied the background would leave `let transparent
    /// = true` — one of the three mutations — passing.
    #[test]
    fn an_opaque_document_is_not_warned_about_transparency_it_does_not_have() {
        let flattened = export::ExportLoss::Flattened;

        // JPEG holds no alpha, so a transparent document loses it and a white
        // one does not. The same format, the same dialog, opposite sentences.
        let transparent = drawn(Background::Transparent, ExportFormat::Jpeg);
        assert!(names(&transparent, flattened), "{transparent:?}");

        let opaque = drawn(Background::opaque(Color::WHITE), ExportFormat::Jpeg);
        assert!(
            !names(&opaque, flattened),
            "a white-backed document was warned it would be flattened onto a \
             matte, which is the warning that trains people to ignore the rest: \
             {opaque:?}"
        );

        // And PNG, which loses nothing either way, says so out loud rather than
        // saying nothing — the quiet case the module docs argue for.
        let png = drawn(Background::Transparent, ExportFormat::Png);
        assert!(
            png.iter()
                .any(|w| w.contains("Nothing in this document is lost")),
            "{png:?}"
        );
    }

    /// The matte control is drawn only where it changes a pixel, and a knob
    /// that does nothing is worse than one that is not drawn.
    ///
    /// **What this can and cannot see.** The comparison is against
    /// `export::needs_matte`, which is the same call the panel's gate makes, so
    /// it catches the panel *forgetting to ask* — the `if true` and
    /// `let transparent = true` mutations, both of which it does fail on — and
    /// it cannot catch a mutation inside `needs_matte` itself, because that
    /// moves both sides together. That half is `umber-core`'s to guard and is
    /// guarded there, against literal expectations.
    ///
    /// The count is the anchor that keeps this from being purely relative: a
    /// `needs_matte` that answered `false` everywhere would agree with a panel
    /// that had stopped drawing the control at all, and the pair would pass.
    #[test]
    fn the_matte_control_appears_only_where_it_would_change_a_pixel() {
        const HEADING: &str = "Transparency becomes";
        let mut drew_count = 0;
        for format in ExportFormat::ALL {
            for background in [Background::Transparent, Background::opaque(Color::WHITE)] {
                let transparent = background == Background::Transparent;
                let words = drawn(background, format);
                let drew = words.iter().any(|w| w == HEADING);
                assert_eq!(
                    drew,
                    export::needs_matte(format, transparent),
                    "{format:?} on a {} document drew the matte control: {drew}",
                    if transparent { "transparent" } else { "white" },
                );
                drew_count += usize::from(drew);
            }
        }
        // Three of the ten pairs: JPEG, GIF and BMP over a transparent
        // document. PNG and TIFF carry alpha and an opaque document has none to
        // lose, so seven pairs draw nothing.
        assert_eq!(
            drew_count, 3,
            "the matte control is drawn on a different number of the ten \
             format/background pairs than this test is about"
        );
    }
}
