//! The document tab strip, and the two dialogs that belong to it.
//!
//! The design puts a 30 px strip of tabs between the menu bar and the tool
//! options: one tab per open document, a dot on the ones with unsaved work, and
//! a `+` at the end. That is what [`strip`] paints.
//!
//! The dialogs are here rather than in `ui.rs` because they are about
//! documents, not about the workspace: [`close_prompt`], which is the last
//! thing between unsaved work and nothing, and [`notice`], which shows what an
//! import or a save could not carry across.

use egui::{Align2, FontId, Frame, Margin, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use umber_core::docimport::ImportWarning;

use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::theme::{Palette, metrics, text};

/// Height of the tab strip.
///
/// A design metric, and it belongs in `theme::metrics` beside `MENU_BAR` and
/// `OPTIONS_STRIP`. It sits here only because that file is being changed by
/// another piece of work in flight.
pub const STRIP: f32 = 30.0;

/// Height of a tab within the strip. They sit on the bottom border, as the
/// design has them, so the active one runs into the strip below.
const TAB: f32 = 24.0;

/// Diameter of the modified dot, which the close mark replaces on hover.
const MARK: f32 = 14.0;

/// What the strip was asked to do this frame.
#[derive(Default, Clone, Copy)]
pub struct TabActions {
    /// Make this tab active.
    pub pick: Option<usize>,
    /// Close this tab, subject to confirmation if it holds work.
    pub close: Option<usize>,
    pub new_document: bool,
}

/// A message that has to reach the user rather than the log.
#[derive(Clone, Debug)]
pub struct Notice {
    pub title: String,
    /// Already phrased for the user — `ImportError` and `ImportWarning` both
    /// `Display` as finished sentences, and they are shown verbatim rather
    /// than being reworded here.
    pub lines: Vec<String>,
}

/// Draw the strip. Panels are laid out by the caller; this fills one.
pub fn strip(ui: &mut egui::Ui, p: &Palette, ed: &Editor) -> TabActions {
    let mut actions = TabActions::default();

    let full = ui.max_rect();
    // The strip's own bottom border, which the active tab breaks through.
    let border = Rect::from_min_size(
        pos2(full.left(), full.bottom() - 1.0),
        vec2(full.width(), 1.0),
    );
    ui.painter().rect_filled(border, 0.0, p.border);

    let active = ed.session.active_index();
    let closable = ed.session.len() > 1;
    let mut x = full.left() + 8.0;

    for (index, tab) in ed.session.tabs().iter().enumerate() {
        let font = FontId::proportional(text::SMALL);
        let label_w = ui
            .painter()
            .layout_no_wrap(tab.title.clone(), font.clone(), p.text)
            .size()
            .x;
        let width = label_w + MARK + 12.0 * 2.0 + 8.0;
        let rect = Rect::from_min_size(pos2(x, full.bottom() - TAB), vec2(width, TAB));
        x += width + 2.0;

        let response = ui.interact(rect, ui.id().with(("doc-tab", index)), Sense::click());
        let is_active = index == active;
        let painter = ui.painter();

        if is_active {
            // Rounded at the top only, and one pixel taller than the strip, so
            // it joins the surface below it exactly as the design draws it.
            let body = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y + 1.0));
            painter.rect_filled(body, metrics::RADIUS_LARGE, p.window);
            painter.rect_stroke(
                body,
                metrics::RADIUS_LARGE,
                Stroke::new(1.0, p.border),
                StrokeKind::Inside,
            );
        } else if response.hovered() {
            painter.rect_filled(rect, metrics::RADIUS_LARGE, p.control);
        }

        let ink = if is_active {
            p.text_strong
        } else if response.hovered() {
            p.text
        } else {
            p.text_dim
        };
        painter.text(
            pos2(rect.left() + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            &tab.title,
            font,
            ink,
        );

        // The design puts a dot after the name on a document with unsaved work.
        // Hovering turns it into the close mark, so the tab needs no extra
        // width for a control that is only wanted momentarily.
        let mark = Rect::from_center_size(
            pos2(rect.right() - 12.0 - MARK * 0.5, rect.center().y),
            vec2(MARK, MARK),
        );
        let over_mark = response
            .hover_pos()
            .is_some_and(|pos| mark.expand(2.0).contains(pos));

        if response.hovered() && closable {
            let painter = ui.painter();
            if over_mark {
                painter.circle_filled(mark.center(), MARK * 0.5, p.control_hover);
            }
            icons::draw(
                painter,
                mark.shrink(3.0),
                Icon::Close,
                if over_mark { p.text_strong } else { p.text_dim },
            );
        } else if tab.modified {
            ui.painter().circle_filled(mark.center(), 3.0, p.accent);
        }

        if response.clicked() {
            if over_mark && closable {
                actions.close = Some(index);
            } else {
                actions.pick = Some(index);
            }
        }

        // The active tab's document is live in the editor rather than parked.
        let size = tab.parked_size().unwrap_or(ed.doc.size);
        let mut tip = format!("{} — {} × {}", tab.title, size.x, size.y);
        if let Some(path) = &tab.path {
            tip.push('\n');
            tip.push_str(&path.display().to_string());
        }
        if tab.modified {
            tip.push_str(if tab.path.is_some() {
                "\nUnsaved changes"
            } else {
                "\nNever saved"
            });
        }
        response.on_hover_text(tip);
    }

    // The design's `+`, drawn rather than typed: Archivo has no such glyph.
    let plus = Rect::from_min_size(pos2(x + 4.0, full.bottom() - TAB), vec2(TAB, TAB));
    let response = ui.interact(plus, ui.id().with("doc-tab-new"), Sense::click());
    icons::draw(
        ui.painter(),
        plus.shrink(7.0),
        Icon::Plus,
        if response.hovered() {
            p.text_strong
        } else {
            p.text_dim
        },
    );
    if response.on_hover_text("New document").clicked() {
        actions.new_document = true;
    }

    actions
}

/// What the user chose in the close prompt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    Cancel,
    /// Write the document out, keeping its layers, and then close it.
    Save,
    /// Export a flat PNG — everything visible, but as one image.
    Export,
    Close,
}

/// Ask before closing a document that holds work.
///
/// Save is the affirmative answer and Discard is the destructive one, so Save
/// gets the emphasis: this dialog is the last thing between an afternoon's work
/// and nothing, and the button that keeps it should be the one the eye lands on.
pub fn close_prompt(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) -> Option<CloseChoice> {
    let index = ed.ui.close_prompt?;
    let Some(tab) = ed.session.tabs().get(index) else {
        ed.ui.close_prompt = None;
        return None;
    };

    let title = tab.title.clone();
    // Whether Save will need to ask for a file, which is worth saying before
    // the click rather than surprising the user with a dialog.
    let has_file = tab.path.is_some();
    let mut choice = None;

    let modal = egui::Modal::new(egui::Id::new("close-document"))
        .frame(dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(420.0);
            ui.label(
                egui::RichText::new(format!("Save “{title}” before closing?"))
                    .size(text::CONTROL)
                    .color(p.text_strong)
                    .strong(),
            );
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(if has_file {
                    "This document has been painted on since it was last saved. \
                     Closing without saving discards those changes for good."
                } else {
                    "This document has never been saved, so closing the tab \
                     discards the painting for good."
                })
                .size(text::SMALL)
                .color(p.text),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Saving keeps every layer. A flat PNG keeps only the picture.")
                    .size(text::SMALL)
                    .color(p.text_dim),
            );

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if button(ui, p, "Cancel", false) {
                    choice = Some(CloseChoice::Cancel);
                }
                if button(ui, p, "Discard and close", false) {
                    choice = Some(CloseChoice::Close);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if button(ui, p, if has_file { "Save" } else { "Save…" }, true) {
                        choice = Some(CloseChoice::Save);
                    }
                    if button(ui, p, "Export PNG…", false) {
                        choice = Some(CloseChoice::Export);
                    }
                });
            });
        });

    // Escape and a click outside both mean "not now", which is the safe answer
    // for a dialog whose other outcome destroys work.
    if modal.should_close() {
        choice = Some(CloseChoice::Cancel);
    }
    if matches!(choice, Some(CloseChoice::Cancel)) {
        ed.ui.close_prompt = None;
    }
    choice
}

/// Show whatever the last import could not do, or could not do at all.
pub fn notice(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    let Some(notice) = ed.notice.clone() else {
        return;
    };
    let mut dismiss = false;

    let modal = egui::Modal::new(egui::Id::new("import-notice"))
        .frame(dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(460.0);
            ui.label(
                egui::RichText::new(&notice.title)
                    .size(text::CONTROL)
                    .color(p.text_strong)
                    .strong(),
            );
            ui.add_space(10.0);

            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    for line in &notice.lines {
                        ui.label(egui::RichText::new(line).size(text::SMALL).color(p.text));
                        ui.add_space(6.0);
                    }
                });

            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if button(ui, p, "Close", true) {
                    dismiss = true;
                }
            });
        });

    if dismiss || modal.should_close() {
        ed.notice = None;
    }
}

/// Turn an import's warnings into lines for [`Notice`].
///
/// Warnings are shown verbatim — they are already written for the user — but a
/// forty-layer Photoshop file with a mask on every layer would otherwise be
/// forty lines saying the same thing. So each *kind* shows a few examples and
/// then a count of the rest, which is a tally rather than a rewording.
pub fn summarise(warnings: &[ImportWarning]) -> Vec<String> {
    /// Examples shown per kind before the rest are counted.
    const SHOWN: usize = 3;

    let mut kinds: Vec<(std::mem::Discriminant<ImportWarning>, Vec<String>)> = Vec::new();
    for warning in warnings {
        let kind = std::mem::discriminant(warning);
        match kinds.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, lines)) => lines.push(warning.to_string()),
            None => kinds.push((kind, vec![warning.to_string()])),
        }
    }

    let mut out = Vec::new();
    for (_, lines) in kinds {
        let hidden = lines.len().saturating_sub(SHOWN);
        out.extend(lines.into_iter().take(SHOWN));
        if hidden > 0 {
            out.push(format!("…and {hidden} more of the same kind."));
        }
    }
    out
}

fn dialog_frame(p: &Palette) -> Frame {
    Frame::NONE
        .fill(p.popover)
        .stroke(Stroke::new(1.0, p.popover_border))
        .corner_radius(8)
        .inner_margin(Margin::same(18))
}

/// A dialog button. `strong` marks the one that carries out the action.
fn button(ui: &mut egui::Ui, p: &Palette, label: &str, strong: bool) -> bool {
    let font = FontId::proportional(text::SMALL);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), p.text)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(vec2(text_w + 22.0, 26.0), Sense::click());

    let fill = match (strong, response.hovered()) {
        (true, false) => p.accent_dim,
        (true, true) => p.accent,
        (false, false) => p.control,
        (false, true) => p.control_hover,
    };
    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, fill);
    if !strong {
        painter.rect_stroke(
            rect,
            metrics::RADIUS,
            Stroke::new(1.0, p.border),
            StrokeKind::Inside,
        );
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font,
        if strong && response.hovered() {
            p.window
        } else {
            p.text_strong
        },
    );
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(layer: &str) -> ImportWarning {
        ImportWarning::MaskIgnored {
            layer: layer.to_string(),
        }
    }

    #[test]
    fn warnings_are_shown_verbatim() {
        let warnings = vec![mask("Ink")];
        assert_eq!(summarise(&warnings), vec![warnings[0].to_string()]);
    }

    #[test]
    fn many_of_one_kind_become_examples_and_a_count() {
        let warnings: Vec<_> = (0..40).map(|i| mask(&format!("L{i}"))).collect();
        let lines = summarise(&warnings);
        assert_eq!(lines.len(), 4, "three examples and one tally: {lines:?}");
        assert_eq!(lines[3], "…and 37 more of the same kind.");
    }

    #[test]
    fn different_kinds_are_counted_separately() {
        let mut warnings: Vec<_> = (0..5).map(|i| mask(&format!("L{i}"))).collect();
        warnings.push(ImportWarning::GroupFlattened {
            group: "Hair".into(),
        });
        let lines = summarise(&warnings);
        // Three masks, their tally, and the group on its own.
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(lines[4].contains("Hair"), "{lines:?}");
    }
}
