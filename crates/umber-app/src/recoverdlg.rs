//! Offering back what a session that stopped left behind.
//!
//! The autosave has kept copies for a long time and Settings has been able to
//! open the folder they are in for just as long. Neither is a recovery: an
//! artist whose machine went down does not know the folder exists, and if they
//! did they would be looking at file names with a hash in them. This is the
//! part that says, on the next start, *here is your painting, shall I open it*.
//!
//! # The division
//!
//! Everything that is a **rule** is in [`crate::autosave`], where the copies
//! and their containment already live: whether the last session ended cleanly
//! ([`crate::autosave::SessionMark`]), which copies are worth offering
//! ([`crate::autosave::offer_from`]), and what each row's sentence says
//! ([`crate::autosave::Recoverable::note`]). This file paints them and nothing
//! else — the same division `dock.rs` keeps against `panels.rs`, and it is what
//! lets the interesting cases be tested without a window.
//!
//! # What the dialog may claim
//!
//! Nothing about how complete a copy is. A crash box can compare the copy's
//! revision against the document's, because a panic hook reads a session that
//! is still in memory; nothing here can, because the session is gone. So every
//! row says what it *can* — when the copy was written, where a Save would put
//! it, and that anything painted afterwards is not in it — and
//! `no_row_claims_a_copy_is_complete` fails the build if that ever softens.
//!
//! # Dismissing it is safe, and says so
//!
//! "Not now" takes the offer down and **removes nothing**. The copies stay
//! where they are until [`crate::autosave::Reaper`] expires them on its own
//! schedule, which is the one thing in Umber that deletes a document. The
//! dialog says that above the buttons rather than leaving somebody to guess,
//! because the answer to "will this throw my painting away?" has to be visible
//! before the click.
//!
//! What dismissing *does* remove is the marker, so the same offer is not made
//! again on every start for ever. That is bookkeeping, not a document — see
//! [`crate::autosave::Marks`].

use egui::{Sense, Vec2, vec2};

use crate::autosave::{Offer, Recoverable};
use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::tabs;
use crate::theme::{Palette, text};
use crate::ui::UiActions;

/// The most of the dialog the list may take before it starts scrolling.
///
/// A **cap**, not a claim: the list shrinks to what is in it, because it is
/// usually one row and a hand's breadth of empty box under a single line reads
/// as something failing to load. What the cap is for is the other end — a
/// session with eight documents open would otherwise size the modal to the
/// height of the window and put its buttons off the bottom of the screen.
const LIST_HEIGHT: f32 = 260.0;

/// Width of the dialog. Wider than the close prompt's 420 because a row carries
/// a file path, which is the longest string in it.
const WIDTH: f32 = 520.0;

/// The offer, and what has been done about it.
///
/// Kept out of [`crate::editor::UiState`] so that stays `Copy`, exactly as
/// `canvas_form` and `updates` are. It belongs above the `--- documents ---`
/// line: it describes a session that is over, not any document that is open.
#[derive(Debug, Default)]
pub struct Recovery {
    /// What the last session left, or `None` once it has been answered.
    offer: Option<Offer>,
    /// Which rows have already been opened, by position in `offer.found`.
    ///
    /// Positions rather than a copy of the entry, and the list is never
    /// reordered while the dialog is up — the same reasoning
    /// `UiState::modulation` gives for indexing the modulation list.
    opened: Vec<bool>,
    /// Rows the caller should open this frame. Read and cleared by `app.rs` in
    /// the frame the request was made in, which is the arrangement
    /// [`UiActions::delete_picked`] keeps and for the same reason: `UiActions`
    /// is `Copy` and this is a list.
    wanted: Vec<usize>,
    /// An offer has arrived and has not been drawn yet.
    ///
    /// It is raised *after* the frame has been presented — the autosave
    /// collects there — so nothing on screen has it yet, and under
    /// `ControlFlow::Wait` a value appearing in a field is not an event. The
    /// same gap `Autosave::set_waker` and `app::Wake` exist for, one frame
    /// wide: without this the offer would sit unseen until the painter happened
    /// to move the mouse.
    arrived: bool,
}

impl Recovery {
    /// Take an offer. Ignored if one is already up, which cannot happen —
    /// `begin_run` answers once per run — and would otherwise silently discard
    /// whichever arrived first.
    pub fn offer(&mut self, offer: Offer) {
        if self.offer.is_some() {
            return;
        }
        self.opened = vec![false; offer.found.len()];
        self.offer = Some(offer);
        self.arrived = true;
    }

    /// Whether a frame has to be asked for so the offer is seen.
    pub fn take_arrived(&mut self) -> bool {
        std::mem::take(&mut self.arrived)
    }

    /// The copies the artist asked for this frame, with the row each came from.
    ///
    /// The row is **not** marked here. A copy that turns out not to open — a
    /// truncated archive, a canvas this GPU cannot hold — must leave its button
    /// where it was rather than a row saying "Opened" beside a document that is
    /// not there. See [`Recovery::note_opened`].
    pub fn take_wanted(&mut self) -> Vec<(usize, Recoverable)> {
        let Some(offer) = self.offer.as_ref() else {
            self.wanted.clear();
            return Vec::new();
        };
        std::mem::take(&mut self.wanted)
            .into_iter()
            .filter_map(|index| offer.found.get(index).map(|e| (index, e.clone())))
            .collect()
    }

    /// A row's copy actually opened.
    pub fn note_opened(&mut self, index: usize) {
        if let Some(done) = self.opened.get_mut(index) {
            *done = true;
        }
    }

    /// The offer has been answered. Returns the markers to forget.
    ///
    /// Returning them rather than removing them here keeps every deletion in
    /// `autosave`, behind [`crate::autosave::Marks`] — this module paints and
    /// decides nothing about the file system.
    pub fn dismiss(&mut self) -> Vec<std::path::PathBuf> {
        self.opened.clear();
        self.offer.take().map(|o| o.marks).unwrap_or_default()
    }
}

/// What one pass over the dialog was asked for.
struct Outcome {
    /// Rows to open, by position in the offer.
    wanted: Vec<usize>,
    dismiss: bool,
    reveal: bool,
}

/// Draw the offer, if there is one.
///
/// Drawn from `ui::draw` rather than from a panel body, for the reason the
/// brush library's modals and the canvas dialogs are: the layout can hide a
/// panel, and a dialog owned by something that is not on screen can neither be
/// shut nor reopened.
pub fn show(root: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    let Some(offer) = ed.recovery.offer.as_ref() else {
        return;
    };
    // Cloned out so the modal's closure can hand requests back to
    // `ed.recovery` while it draws. The offer is a handful of strings, built
    // once per run.
    let found = offer.found.clone();
    let at_risk = offer.at_risk.clone();
    let opened = ed.recovery.opened.clone();
    let mut out = Outcome {
        wanted: Vec::new(),
        dismiss: false,
        reveal: false,
    };

    egui::Modal::new(egui::Id::new("recover-documents"))
        .frame(tabs::dialog_frame(p))
        .show(root.ctx(), |ui| {
            body(ui, p, &found, &at_risk, &opened, &mut out)
        });

    // Escape and a click outside are deliberately **not** wired to
    // `should_close`. Both destroy nothing — the copies stay — but this offer is
    // made once, and a stray keypress should not be the thing that takes the
    // only signpost to somebody's afternoon off the screen. "Not now" is one
    // click away and says what it does.

    ed.recovery.wanted = out.wanted;
    actions.recover |= !ed.recovery.wanted.is_empty();
    actions.reveal_autosaves |= out.reveal;
    if out.dismiss {
        actions.dismiss_recovery = true;
    }
}

/// The dialog itself, apart from the modal that holds it.
///
/// Split out so its layout can be measured without a window — see
/// `the_offer_is_sized_by_its_list_and_not_by_the_screen`. What that guards is
/// invisible until somebody with several documents open has a crash.
fn body(
    ui: &mut egui::Ui,
    p: &Palette,
    found: &[Recoverable],
    at_risk: &[String],
    opened: &[bool],
    out: &mut Outcome,
) {
    let remaining = opened.iter().filter(|done| !**done).count();
    ui.set_width(WIDTH);
    heading(ui, p, found.len());

    ui.add_space(12.0);
    // Shrinks vertically and not horizontally. The settings dialog's own scroll
    // area claims its height whatever is in it, and deliberately — its pages
    // swap under one frame, so a box that resized would make the dialog jump.
    // Nothing swaps here: the list is settled before the dialog is drawn and
    // is usually one row, so claiming 260 points would be a hand's breadth of
    // empty box under a single line. The cap is what the height is *for*.
    egui::ScrollArea::vertical()
        .max_height(LIST_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (index, entry) in found.iter().enumerate() {
                row(
                    ui,
                    p,
                    entry,
                    opened.get(index).copied().unwrap_or(false),
                    &mut out.wanted,
                    index,
                );
            }
            if !at_risk.is_empty() {
                ui.add_space(10.0);
                missing(ui, p, at_risk);
            }
        });

    ui.add_space(12.0);
    // Before the buttons, because it is the answer to "will saying no throw my
    // painting away?" and that has to be readable before the click rather than
    // discovered after it.
    ui.label(
        egui::RichText::new(
            "Nothing here is deleted either way — Umber keeps its copies in the \
             autosave folder until they expire. This offer is only made once.",
        )
        .size(text::TINY)
        .color(p.text_dim),
    );

    ui.add_space(14.0);
    // Inside a `horizontal`. A bare right-to-left layout takes the whole
    // remaining height of the `Ui` it is in — the align is the cross axis —
    // and would stretch this modal to the height of the window.
    ui.horizontal(|ui| {
        if tabs::button(
            ui,
            p,
            if remaining == 0 { "Done" } else { "Not now" },
            false,
        ) {
            out.dismiss = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Only while there is something left to open, so the strong button
            // never promises an action that would do nothing.
            if remaining > 0
                && tabs::button(
                    ui,
                    p,
                    if remaining == 1 {
                        "Recover it"
                    } else {
                        "Recover them all"
                    },
                    true,
                )
            {
                out.wanted.extend(
                    opened
                        .iter()
                        .enumerate()
                        .filter(|(_, done)| !**done)
                        .map(|(i, _)| i),
                );
            }
            // The words Settings uses for the same control. Two spellings of
            // one command is how the two come to be read as two things.
            if tabs::button(ui, p, "Open the folder", false) {
                out.reveal = true;
            }
        });
    });
}

/// The mark, the heading, and the one line of fact under it.
fn heading(ui: &mut egui::Ui, p: &Palette, count: usize) {
    ui.horizontal(|ui| {
        let (mark, _) = ui.allocate_exact_size(egui::Vec2::splat(26.0), Sense::hover());
        icons::draw(ui.painter(), mark, Icon::Alert, p.warning);
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new("Umber did not close properly last time")
                    .size(text::HEADING)
                    .color(p.text_strong)
                    .strong(),
            );
            ui.label(
                egui::RichText::new(match count {
                    0 => "It kept no copy of what was open.".to_string(),
                    1 => "It had kept a copy of one document.".to_string(),
                    n => format!("It had kept copies of {n} documents."),
                })
                .size(text::SMALL)
                .color(p.text_muted),
            );
        });
    });
}

/// One recoverable document: what it is, what the copy is, and one button.
///
/// A button per row rather than a tick column and one Open: there is usually
/// one row, the answer per document is genuinely independent, and a tick box
/// here would be a fourth spelling of a control the layers panel already owns.
fn row(
    ui: &mut egui::Ui,
    p: &Palette,
    entry: &Recoverable,
    opened: bool,
    wanted: &mut Vec<usize>,
    index: usize,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            // Bounded, because the button beside it must keep its place: an
            // egui label defaults to `TextWrapMode::Extend`, which is what put
            // the brush browser wider than the screen.
            ui.set_max_width(WIDTH - 130.0);
            ui.label(
                egui::RichText::new(&entry.title)
                    .size(text::SMALL)
                    .color(p.text_strong),
            );
            // Every sentence comes from `autosave`, where the rule about what
            // may be claimed is decided and tested. Nothing is phrased here.
            note(ui, p, &entry.note());
            note(ui, p, &entry.destination());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if opened {
                // Not a disabled button. The row has already done its job, and
                // a control that is drawn and refuses is one that lies about
                // being available.
                ui.label(
                    egui::RichText::new("Recovered")
                        .size(text::SMALL)
                        .color(p.text_dim),
                );
            } else if tabs::button(ui, p, "Recover", false) {
                wanted.push(index);
            }
        });
    });
    ui.add_space(6.0);
    let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rule, 0.0, p.border);
}

/// Documents that held work and have no copy anywhere.
///
/// Named rather than passed over, exactly as the crash box's `at_risk` is: a
/// dialog that offers two documents back and says nothing about the third reads
/// as a promise about the third.
fn missing(ui: &mut egui::Ui, p: &Palette, titles: &[String]) {
    ui.label(
        egui::RichText::new(if titles.len() == 1 {
            "Umber had no copy of this one:"
        } else {
            "Umber had no copy of these:"
        })
        .size(text::SMALL)
        .color(p.text),
    );
    for title in titles {
        ui.horizontal(|ui| {
            // The quit prompt's own dot column, taken from where it is stated
            // rather than re-typed, so two lists of documents in one
            // application line up and go on lining up.
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(tabs::MARK), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.0, p.warning);
            ui.label(
                egui::RichText::new(title)
                    .size(text::SMALL)
                    .color(p.text_strong),
            );
        });
    }
}

/// A dim line under a title, wrapped rather than extending.
fn note(ui: &mut egui::Ui, p: &Palette, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .size(crate::theme::text::TINY)
                .color(p.text_dim),
        )
        .wrap(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autosave::Recoverable;
    use std::path::PathBuf;

    fn entry(title: &str, original: Option<&str>, seconds_ago: u64) -> Recoverable {
        Recoverable {
            title: title.to_string(),
            original: original.map(PathBuf::from),
            copy: PathBuf::from("/data/autosave/hands-0123456789abcdef.ora"),
            seconds_ago,
        }
    }

    fn offer_of(found: Vec<Recoverable>) -> Offer {
        Offer {
            marks: vec![PathBuf::from(
                "/data/autosave/sessions/00000000deadbeef.json",
            )],
            found,
            at_risk: Vec::new(),
        }
    }

    /// The one sentence this dialog must never produce. A crash box can say a
    /// copy holds everything, because it compares two revisions of a session
    /// still in memory; this cannot, and must not borrow the phrasing.
    #[test]
    fn no_row_claims_a_copy_is_complete() {
        for seconds in [0, 45, 240, 3600, 90_000] {
            for original in [None, Some("/work/hands.ora")] {
                let entry = entry("hands.ora", original, seconds);
                let said = format!("{} {}", entry.note(), entry.destination());
                for forbidden in ["everything", "complete", "all of", "safe"] {
                    assert!(
                        !said.to_ascii_lowercase().contains(forbidden),
                        "“{forbidden}” in: {said}",
                    );
                }
                assert!(
                    said.contains("not in it"),
                    "a row has to say what it cannot know: {said}",
                );
            }
        }
    }

    /// A never-saved document has to say that a Save will ask, and one with a
    /// file has to name it — the whole point of recovering into the *original*
    /// path rather than into the copy's.
    #[test]
    fn a_row_says_where_a_save_would_go() {
        assert!(
            entry("Untitled 3", None, 60)
                .destination()
                .contains("never been saved"),
        );
        let saved = entry("hands.ora", Some("/work/hands.ora"), 60).destination();
        assert!(saved.contains("hands.ora"), "{saved}");
        assert!(saved.contains("Save writes back"), "{saved}");
        // And the half that decides whether somebody dares click: opening a
        // copy to look at it must not be what replaces the file they have.
        // `Candidate::write_own_file` is what makes this true.
        assert!(saved.contains("until you do"), "{saved}");
    }

    /// A row says "Opened" only once its copy actually opened.
    ///
    /// The tempting shape is to mark it as the request is taken, which is one
    /// line shorter and puts "Opened" beside a truncated archive that raised a
    /// notice and produced no document — with the button that would have let
    /// somebody try again now gone.
    #[test]
    fn a_row_is_marked_opened_only_when_its_copy_opened() {
        let mut recovery = Recovery::default();
        recovery.offer(offer_of(vec![
            entry("a.ora", None, 10),
            entry("b.ora", None, 20),
        ]));

        recovery.wanted = vec![0];
        let taken = recovery.take_wanted();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].1.title, "a.ora");
        assert_eq!(
            recovery.opened,
            vec![false, false],
            "taking the request must not be what marks the row",
        );

        recovery.note_opened(taken[0].0);
        assert_eq!(recovery.opened, vec![true, false]);
        assert!(recovery.take_wanted().is_empty(), "the list is drained");

        // Out of range is ignored rather than panicking: the list is the
        // caller's own, but a drawing path must not be the thing that dies.
        recovery.note_opened(99);
        assert_eq!(recovery.opened, vec![true, false]);
    }

    /// Dismissing hands back the markers to forget and nothing else. It must
    /// never name a copy: the copies are documents, and only `Reaper` may
    /// delete one of those.
    #[test]
    fn dismissing_forgets_the_marker_and_names_no_document() {
        let mut recovery = Recovery::default();
        recovery.offer(offer_of(vec![entry("a.ora", None, 10)]));
        assert!(recovery.offer.is_some());
        assert!(
            recovery.take_arrived(),
            "an offer nothing asks a frame for is one nobody sees",
        );
        assert!(!recovery.take_arrived(), "asked for twice");

        let marks = recovery.dismiss();
        assert_eq!(marks.len(), 1);
        assert!(
            marks
                .iter()
                .all(|p| p.extension().is_some_and(|e| e == "json")),
            "dismissing named something that is not a marker: {marks:?}",
        );
        assert!(recovery.offer.is_none());
        assert!(recovery.dismiss().is_empty(), "answered twice");
    }

    /// The dialog is sized by its own list, not by the window it is in.
    ///
    /// Two ways this goes wrong and both have shipped elsewhere in Umber: a
    /// list that sizes the modal, so a session with several documents in it
    /// puts the buttons off the bottom of the screen; and a bare
    /// `Layout::right_to_left(Align::Center)` for the button strip, which takes
    /// the **whole** remaining height of the `Ui` it is in because the align is
    /// the cross axis. Either produces a dialog as tall as the window, so one
    /// measurement catches both.
    ///
    /// A CPU test, like `ticking_a_layer_does_not_move_the_layer_list`: this is
    /// geometry and needs no device.
    #[test]
    fn the_offer_is_sized_by_its_list_and_not_by_the_screen() {
        use crate::theme::ThemeKind;
        use egui::{Rect, pos2, vec2};

        let ctx = egui::Context::default();
        let screen = vec2(1440.0, 900.0);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), screen)),
            ..Default::default()
        };
        let p = Palette::of(ThemeKind::Graphite);

        let height = |rows: usize| {
            let found: Vec<Recoverable> = (0..rows)
                .map(|i| {
                    entry(
                        &format!("study-{i}.ora"),
                        Some("/work/a-rather-long-path.ora"),
                        90,
                    )
                })
                .collect();
            let opened = vec![false; rows];
            let mut measured = 0.0;
            // Twice, and the second is the one read: the first pass through a
            // fresh context builds the font atlas, and text laid out against a
            // half-built one is not the height it will settle at.
            for _ in 0..2 {
                let _ = ctx.run_ui(input.clone(), |ui| {
                    let mut out = Outcome {
                        wanted: Vec::new(),
                        dismiss: false,
                        reveal: false,
                    };
                    body(ui, &p, &found, &[], &opened, &mut out);
                    measured = ui.min_rect().height();
                });
            }
            measured
        };

        let one = height(1);
        assert!(one < screen.y, "one row already filled the window: {one}");
        let many = height(12);
        assert!(
            many < screen.y * 0.75,
            "twelve documents made a dialog {many} tall in a {} window",
            screen.y,
        );
        assert!(
            many - one < LIST_HEIGHT + 1.0,
            "the list grew past its own cap: {one} to {many}",
        );
    }

    /// An offer that arrives while one is up is refused rather than replacing
    /// it, or the ticks and the list would describe different sessions.
    #[test]
    fn a_second_offer_does_not_displace_the_one_on_screen() {
        let mut recovery = Recovery::default();
        recovery.offer(offer_of(vec![entry("first.ora", None, 10)]));
        recovery.offer(offer_of(vec![
            entry("second.ora", None, 10),
            entry("third.ora", None, 10),
        ]));
        assert_eq!(recovery.opened.len(), 1);
    }
}
