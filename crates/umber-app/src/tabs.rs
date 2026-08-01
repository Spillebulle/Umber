//! The document tab strip, and the two dialogs that belong to it.
//!
//! The design puts a 30 px strip of tabs between the menu bar and the tool
//! options: one tab per open document, a dot on the ones with unsaved work, and
//! a `+` at the end. That is what [`strip`] paints, with [`tab_layout`] deciding
//! how the room is divided — the design draws two documents in a 1440 px window
//! and says nothing about what a dozen in a narrow one should do.
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

/// Diameter of the modified dot, which the close mark replaces on hover.
const MARK: f32 = 14.0;

/// Padding either side of a tab's label.
const TAB_PAD: f32 = 12.0;

/// Gap between tabs.
const TAB_GAP: f32 = 2.0;

/// Narrowest a tab is allowed to get before the strip stops shrinking them.
///
/// Below this there is no room for even an ellipsis beside the dot, and a row of
/// identical stubs is no more useful than a row that runs off the edge.
const TAB_MIN: f32 = 64.0;

/// Which tabs the strip can show, and how wide to draw each of them.
///
/// Returns the index of the first tab drawn and a width per tab from there on.
///
/// Tabs are placed by hand rather than by a layout, so nothing stops them
/// running past the right edge — and when they did, the `+` went with them and
/// there was no way to open a document from the strip at all. So the `+` and the
/// outer padding come off the top, and this divides what is left.
///
/// Three rules, in order:
///
/// * While they all fit, tabs keep their natural width. A strip whose tabs
///   resize every time a document is opened is unsettling to read.
/// * Past that they share equally, down to [`TAB_MIN`] — but never wider than a
///   tab actually wants, so shrinking the long names does not stretch the short
///   ones to meet them.
/// * Past *that* some cannot be drawn at all, and the run shown is slid along to
///   keep the active document in it. Dropping the tail unconditionally would put
///   the document you are looking at off the end of its own tab strip, with no
///   other way to reach it.
fn tab_layout(natural: &[f32], room: f32, active: usize) -> (usize, Vec<f32>) {
    if natural.is_empty() {
        return (0, Vec::new());
    }
    let gaps = TAB_GAP * natural.len() as f32;
    if natural.iter().sum::<f32>() + gaps <= room {
        return (0, natural.to_vec());
    }

    let share = ((room - gaps) / natural.len() as f32).max(TAB_MIN);
    let squeezed: Vec<f32> = natural.iter().map(|w| w.min(share)).collect();

    // How many fit, starting from `first`.
    let run = |first: usize| {
        let mut used = 0.0;
        let mut n = 0usize;
        for w in &squeezed[first..] {
            if used + w + TAB_GAP > room {
                break;
            }
            used += w + TAB_GAP;
            n += 1;
        }
        n
    };

    // Slide the window along so the active tab is the last one in it, then
    // re-measure — the run from there may hold a different number, since the
    // tabs are not all the same width.
    let fits = run(0);
    let first = active.saturating_sub(fits.saturating_sub(1));
    let count = run(first).min(natural.len() - first);
    (first, squeezed[first..first + count].to_vec())
}

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

/// One tab: its body, its name, and the dot or close mark at its end.
///
/// Split out of [`strip`] because the tabs are painted in two passes with the
/// strip's rule between them, and a second copy of this is how the selected tab
/// would end up a different height from the rest.
#[allow(clippy::too_many_arguments)]
fn paint_tab(
    ui: &egui::Ui,
    p: &Palette,
    ed: &Editor,
    rect: Rect,
    index: usize,
    response: &egui::Response,
    closable: bool,
    actions: &mut TabActions,
) {
    let Some(tab) = ed.session.tabs().get(index) else {
        return;
    };
    let is_active = index == ed.session.active_index();
    let font = FontId::proportional(text::SMALL);
    let painter = ui.painter();

    // A folder leaf: rounded where it meets the top of the strip, square where
    // it meets the surface below, so the two read as one continuous face.
    let radius = egui::CornerRadius {
        nw: metrics::RADIUS_LARGE as u8,
        ne: metrics::RADIUS_LARGE as u8,
        sw: 0,
        se: 0,
    };

    if is_active {
        // The selected tab wears the tool options strip's own colour and runs a
        // pixel past the bottom of its panel, so the rule and the tab's own
        // bottom edge are both outside the clip. What is left is a tab open at
        // the foot into the strip beneath it.
        let body = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y + 1.0));
        painter.rect_filled(body, radius, p.chrome);
        painter.rect_stroke(body, radius, Stroke::new(1.0, p.border), StrokeKind::Inside);
    } else {
        // The others are darker than the selected one and sit behind the rule,
        // which the caller paints after this pass.
        let fill = if response.hovered() {
            p.control_hover
        } else {
            p.control
        };
        painter.rect_filled(rect, radius, fill);
    }

    let ink = if is_active {
        p.text_strong
    } else if response.hovered() {
        p.text
    } else {
        p.text_dim
    };
    // The mark's place is reserved whether or not it is drawn, so a name
    // that has been squeezed ends in an ellipsis rather than under the dot.
    let room = rect.width() - TAB_PAD * 2.0 - MARK;
    painter.text(
        pos2(rect.left() + TAB_PAD, rect.center().y),
        Align2::LEFT_CENTER,
        crate::widgets::elide(painter, &tab.title, text::SMALL, room),
        font,
        ink,
    );

    // The design puts a dot after the name on a document with unsaved work.
    // Hovering turns it into the close mark, so the tab needs no extra
    // width for a control that is only wanted momentarily.
    let mark = Rect::from_center_size(
        pos2(rect.right() - TAB_PAD - MARK * 0.5, rect.center().y),
        vec2(MARK, MARK),
    );
    let over_mark = response
        .hover_pos()
        .is_some_and(|pos| mark.expand(2.0).contains(pos));

    if response.hovered() && closable {
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
        painter.circle_filled(mark.center(), 3.0, p.accent);
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
    response.clone().on_hover_text(tip);
}

/// Draw the strip. Panels are laid out by the caller; this fills one.
pub fn strip(ui: &mut egui::Ui, p: &Palette, ed: &Editor) -> TabActions {
    let mut actions = TabActions::default();

    let full = ui.max_rect();
    // The strip's own bottom rule. Painted between the two passes below rather
    // than here — see them for why.
    let border = Rect::from_min_size(
        pos2(full.left(), full.bottom() - 1.0),
        vec2(full.width(), 1.0),
    );

    let active = ed.session.active_index();
    let closable = ed.session.len() > 1;
    let font = FontId::proportional(text::SMALL);

    // The `+` is placed first, at a fixed distance from the right edge, so it
    // stays reachable however many documents are open. The tabs then divide
    // what is left of the strip.
    let plus = Rect::from_min_size(
        pos2(
            full.right() - 8.0 - metrics::TAB,
            full.bottom() - metrics::TAB,
        ),
        vec2(metrics::TAB, metrics::TAB),
    );
    let natural: Vec<f32> = ed
        .session
        .tabs()
        .iter()
        .map(|tab| {
            let label_w = ui
                .painter()
                .layout_no_wrap(tab.title.clone(), font.clone(), p.text)
                .size()
                .x;
            label_w + MARK + TAB_PAD * 2.0 + 8.0
        })
        .collect();
    let (first, widths) = tab_layout(&natural, plus.left() - 4.0 - (full.left() + 8.0), active);
    let hidden = natural.len() - widths.len();

    // Where every visible tab sits, and what the pointer is doing to it.
    //
    // Collected before anything is painted because the paint order is not the
    // tab order: the strip's rule has to run *over* the tabs that are not
    // selected and *under* the one that is. That is what makes the selected tab
    // read as the front leaf of a folder — it breaks the line and joins the
    // strip below — while the rest are tucked behind it. A single pass could
    // only put the rule under all of them or over all of them, and the selected
    // tab is not reliably last.
    let mut placed: Vec<(usize, Rect, egui::Response)> = Vec::with_capacity(widths.len());
    let mut x = full.left() + 8.0;
    for (offset, width) in widths.iter().copied().enumerate() {
        let index = first + offset;
        let rect = Rect::from_min_size(
            pos2(x, full.bottom() - metrics::TAB),
            vec2(width, metrics::TAB),
        );
        x += width + TAB_GAP;
        let response = ui.interact(rect, ui.id().with(("doc-tab", index)), Sense::click());
        placed.push((index, rect, response));
    }

    for (index, rect, response) in &placed {
        if *index != active {
            paint_tab(ui, p, ed, *rect, *index, response, closable, &mut actions);
        }
    }
    ui.painter().rect_filled(border, 0.0, p.border);
    for (index, rect, response) in &placed {
        if *index == active {
            paint_tab(ui, p, ed, *rect, *index, response, closable, &mut actions);
        }
    }

    // The design's `+`, drawn rather than typed: Archivo has no such glyph. It
    // keeps its place at the right whatever the tabs did.
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

    // Documents the strip could not fit. Saying how many beats letting them
    // disappear, since there is no other sign that a tab is open — and the
    // File menu can still reach the one you want.
    if hidden > 0 {
        let label = format!("+{hidden}");
        let overflow = Rect::from_min_max(
            pos2(x, full.bottom() - metrics::TAB),
            pos2(plus.left() - 4.0, full.bottom()),
        );
        if overflow.width() > 4.0 {
            ui.painter().text(
                overflow.right_center(),
                Align2::RIGHT_CENTER,
                &label,
                FontId::proportional(text::TINY),
                p.text_dim,
            );
            ui.interact(overflow, ui.id().with("doc-tab-overflow"), Sense::hover())
                .on_hover_text(format!(
                    "{hidden} more document{} open — the window is too narrow to \
                     show {}. File, Close document makes room.",
                    if hidden == 1 { "" } else { "s" },
                    if hidden == 1 { "it" } else { "them" },
                ));
        }
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

/// What the user chose in the quit prompt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum QuitChoice {
    /// Stay. The window does not close.
    Cancel,
    /// Write every document that holds work — asking for a file for any that
    /// has never had one — and quit only if all of them succeed.
    SaveAll,
    /// Quit, losing what has not been written.
    Discard,
}

/// Ask before closing the window on work that is not written down.
///
/// The close prompt asks about one document; this asks about all of them at
/// once, because closing the window discards every open document and naming
/// one of three would be worse than naming none. It lists them, so the answer
/// is given with the facts in view rather than to the word "documents".
///
/// Cancel is the emphasised answer here, unlike the close prompt where Save is.
/// Closing a window is very often a mis-click on the wrong title bar, and the
/// safest reading of that is "stay".
pub fn quit_prompt(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) -> Option<QuitChoice> {
    if !ed.ui.quit_prompt {
        return None;
    }
    // Recomputed rather than remembered: a document saved while this is up is
    // no longer at risk, and the prompt should stop claiming it is.
    let at_risk = ed.unsaved_documents();
    if at_risk.is_empty() {
        ed.ui.quit_prompt = false;
        return Some(QuitChoice::Discard);
    }

    let names: Vec<(String, bool)> = at_risk
        .iter()
        .filter_map(|i| ed.session.tabs().get(*i))
        .map(|tab| (tab.title.clone(), tab.path.is_some()))
        .collect();
    let any_untitled = names.iter().any(|(_, has_file)| !has_file);
    let mut choice = None;

    let modal = egui::Modal::new(egui::Id::new("quit-umber"))
        .frame(dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(440.0);
            ui.label(
                egui::RichText::new(if names.len() == 1 {
                    "One document has unsaved work.".to_string()
                } else {
                    format!("{} documents have unsaved work.", names.len())
                })
                .size(text::CONTROL)
                .color(p.text_strong)
                .strong(),
            );
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("Closing Umber now discards those changes for good.")
                    .size(text::SMALL)
                    .color(p.text),
            );

            ui.add_space(10.0);
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    for (title, has_file) in &names {
                        ui.horizontal(|ui| {
                            let (dot, _) = ui.allocate_exact_size(vec2(MARK, MARK), Sense::hover());
                            ui.painter().circle_filled(dot.center(), 3.0, p.accent);
                            ui.label(egui::RichText::new(title).size(text::SMALL).color(p.text));
                            ui.label(
                                egui::RichText::new(if *has_file {
                                    "unsaved changes"
                                } else {
                                    "never saved"
                                })
                                .size(text::TINY)
                                .color(p.text_dim),
                            );
                        });
                    }
                });

            if any_untitled {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Save all will ask where to put anything that has never \
                         been saved.",
                    )
                    .size(text::SMALL)
                    .color(p.text_dim),
                );
            }

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if button(ui, p, "Discard and quit", false) {
                    choice = Some(QuitChoice::Discard);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Emphasis on staying: closing a window is very often a
                    // mis-click, and this is the answer that loses nothing.
                    if button(ui, p, "Keep painting", true) {
                        choice = Some(QuitChoice::Cancel);
                    }
                    if button(
                        ui,
                        p,
                        if any_untitled {
                            "Save all…"
                        } else {
                            "Save all"
                        },
                        false,
                    ) {
                        choice = Some(QuitChoice::SaveAll);
                    }
                });
            });
        });

    // Escape and a click outside are "not now", as everywhere else that a
    // dialog's other answer destroys work.
    if modal.should_close() {
        choice = Some(QuitChoice::Cancel);
    }
    if matches!(choice, Some(QuitChoice::Cancel)) {
        ed.ui.quit_prompt = false;
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

/// The frame every modal in this application uses.
///
/// Shared out of here rather than copied: the canvas dialogs, the About dialog
/// and the update prompts are the same kind of thing as the close prompt and
/// the import notice, and two spellings of one border is how a set of dialogs
/// starts drifting apart.
pub(crate) fn dialog_frame(p: &Palette) -> Frame {
    Frame::NONE
        .fill(p.popover)
        .stroke(Stroke::new(1.0, p.popover_border))
        .corner_radius(8)
        .inner_margin(Margin::same(18))
}

/// A dialog button. `strong` marks the one that carries out the action.
///
/// Shared with the canvas dialogs, About and the update prompts rather than
/// copied: two dialog buttons of slightly different sizes is exactly what a
/// second copy produces.
pub(crate) fn button(ui: &mut egui::Ui, p: &Palette, label: &str, strong: bool) -> bool {
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

    /// Total width the strip would need to draw `widths`.
    fn spans(widths: &[f32]) -> f32 {
        widths.iter().sum::<f32>() + TAB_GAP * widths.len() as f32
    }

    #[test]
    fn tabs_that_fit_keep_the_width_their_names_want() {
        let natural = [120.0, 90.0, 150.0];
        let (first, widths) = tab_layout(&natural, 1000.0, 0);
        assert_eq!(first, 0);
        assert_eq!(widths, natural);
    }

    #[test]
    fn a_crowded_strip_shrinks_the_long_names_and_leaves_the_short_ones() {
        // Six tabs into 500 points: the share is ~81, so the 70 wide one is
        // already narrower than its share and must not be stretched up to it.
        let natural = [200.0, 200.0, 200.0, 200.0, 200.0, 70.0];
        let (_, widths) = tab_layout(&natural, 500.0, 0);
        assert_eq!(*widths.last().expect("a tab"), 70.0, "{widths:?}");
        assert!(widths[0] < 200.0, "{widths:?}");
        assert!(spans(&widths) <= 500.0, "{widths:?}");
    }

    #[test]
    fn nothing_is_ever_drawn_past_the_room_it_was_given() {
        // The failure this replaces: tabs ran off the right edge and took the
        // `+` with them, so a document could not be opened from the strip.
        for count in 1..24usize {
            for room in [0.0, 40.0, 130.0, 400.0, 900.0] {
                let natural = vec![180.0; count];
                let (first, widths) = tab_layout(&natural, room, count - 1);
                assert!(first + widths.len() <= count);
                assert!(
                    spans(&widths) <= room.max(0.0) + 1e-3,
                    "{count} tabs in {room}: {widths:?}",
                );
            }
        }
    }

    #[test]
    fn the_active_document_is_always_one_of_the_tabs_drawn() {
        // Otherwise the tab strip hides the very document it is describing, and
        // there is nothing else in the interface that switches documents.
        let natural = vec![180.0; 12];
        for active in 0..12 {
            let (first, widths) = tab_layout(&natural, 300.0, active);
            assert!(!widths.is_empty(), "room for at least one");
            assert!(
                (first..first + widths.len()).contains(&active),
                "active {active} fell outside {first}..{}",
                first + widths.len(),
            );
        }
    }

    #[test]
    fn a_strip_with_no_room_at_all_draws_nothing_rather_than_a_negative_tab() {
        let (first, widths) = tab_layout(&[180.0, 180.0], 10.0, 1);
        assert!(widths.is_empty(), "{widths:?}");
        assert!(first <= 2);
        assert_eq!(tab_layout(&[], 400.0, 0), (0, Vec::new()));
    }

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
