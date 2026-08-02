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

use crate::colorpicker::{self, PickerMode, WheelAngles, WheelShape};
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
        // way. The wheel's shape, spin and angles belong to the wheel, which
        // this mode does not draw, so they are throwaways rather than the
        // editor's — a dialog must not be able to turn the picker behind it.
        let mut shape = WheelShape::Triangle;
        let mut rotate = false;
        let mut angles = WheelAngles::default();
        colorpicker::show(
            ui,
            p,
            PickerMode::Sliders,
            &mut shape,
            &mut rotate,
            &mut angles,
            &mut form.hsv,
        );
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
}
